//! Contour georeferencing on the real Grootkloof sheet (and synthetic maps):
//! mask → skeleton samples → orientation field → coarse FFT search → DEM
//! refinement → confidence. Run with `cargo test --release`.

mod common;
use common::*;
use mapme_core::contour::auto_mask;
use mapme_core::dem::{tile_bounds, tiles_covering, DemMosaic, DemStore, MercBox, TileId, TILE_SIZE};
use mapme_core::geo::{haversine_m, latlon_to_merc, merc_to_latlon, Affine, LatLon};
use mapme_core::image::{Mask, Rgb8};
use mapme_core::refine::{confidence, refine_dem_box, refine_prepared, select_refined, PreparedDem, RefineResult};
use mapme_core::search::{coarse_search, map_field, required_dem_box, Candidate, MapField, SearchParams};
use mapme_core::skeleton::{contour_samples, default_sampling, ContourSamples};
use std::sync::{Mutex, OnceLock};
use std::time::Instant;

/// Heavy tests run one at a time so timings are meaningful.
fn serial() -> std::sync::MutexGuard<'static, ()> {
    static M: Mutex<()> = Mutex::new(());
    M.lock().unwrap_or_else(|e| e.into_inner())
}

/// Mask, samples and field of an image.
struct Prepared {
    w: usize,
    h: usize,
    mask: Mask,
    samples: ContourSamples,
    field: MapField,
}

fn prepare(img: &Rgb8) -> Prepared {
    let (mask, _) = auto_mask(img);
    let (min_len, step) = default_sampling(img.w, img.h);
    let samples = contour_samples(&mask, min_len, step);
    let field = map_field(&mask);
    Prepared { w: img.w, h: img.h, mask, samples, field }
}

fn grootkloof() -> &'static Prepared {
    static P: OnceLock<Prepared> = OnceLock::new();
    P.get_or_init(|| prepare(&load_map()))
}

fn store() -> &'static DemStore {
    static S: OnceLock<DemStore> = OnceLock::new();
    S.get_or_init(dem_store)
}

fn params_at(lat: f64, lon: f64) -> SearchParams {
    SearchParams {
        center_merc: latlon_to_merc(lat, lon),
        search_radius_m: 20_000.0,
        scale_min: 0.5,
        scale_max: 10.0,
        rotations_deg: vec![0.0],
        top_k: 3,
    }
}

/// Best available fine DEM around `t`: zoom 13, else 12, else 11 (the
/// fixture has z13 only around the sheet; the app would download z13).
fn fine_dem(dems: &DemStore, t: &Affine, w: usize, h: usize) -> DemMosaic {
    let b = refine_dem_box(t, w, h);
    for z in [13u8, 12] {
        let m = dems.mosaic(z, &b);
        if m.valid_fraction() > 0.98 {
            return m;
        }
    }
    dems.mosaic(11, &b)
}

struct Outcome {
    candidates: Vec<Candidate>,
    refined: Option<RefineResult>,
    confidence: f64,
    timings: Vec<(&'static str, f64)>,
}

/// What the app does: refine every coarse candidate on its own fine DEM and
/// keep the best (`select_refined`). Returns (index, confidence, result).
fn locate_all(p: &Prepared, dems: &DemStore, cands: &[Candidate]) -> Option<(usize, f64, RefineResult)> {
    let results: Vec<(Candidate, RefineResult)> = cands
        .iter()
        .map(|c| {
            let prep = PreparedDem::new(&fine_dem(dems, &c.transform, p.w, p.h));
            (c.clone(), refine_prepared(&p.samples, &prep, &c.transform))
        })
        .collect();
    for (i, (_, r)) in results.iter().enumerate() {
        eprintln!(
            "    refined cand {i}: fit conf {:.3} (R² {:.4}, snr {:.2}, interval {:.1} m)",
            mapme_core::refine::fit_confidence(r),
            r.explained_variance,
            r.interval_snr,
            r.contour_interval_m
        );
    }
    let (i, conf) = select_refined(&results)?;
    Some((i, conf, results[i].1.clone()))
}

/// Coarse search + refinement of the best candidate.
fn locate(p: &Prepared, dems: &DemStore, params: &SearchParams) -> Outcome {
    let mut timings = Vec::new();
    let t = Instant::now();
    let dem = dems.mosaic(11, &required_dem_box(params, p.w, p.h));
    timings.push(("coarse dem", t.elapsed().as_secs_f64()));
    let t = Instant::now();
    let candidates = coarse_search(&p.field, &dem, p.w, p.h, params, &|| false, &|_| {});
    timings.push(("coarse search", t.elapsed().as_secs_f64()));
    let Some(best) = candidates.first() else {
        return Outcome { candidates, refined: None, confidence: 0.0, timings };
    };
    let t = Instant::now();
    let fine = fine_dem(dems, &best.transform, p.w, p.h);
    let prep = PreparedDem::new(&fine);
    timings.push(("fine dem", t.elapsed().as_secs_f64()));
    let t = Instant::now();
    let r = refine_prepared(&p.samples, &prep, &best.transform);
    timings.push(("refine", t.elapsed().as_secs_f64()));
    let confidence = confidence(best, &r);
    Outcome { candidates, refined: Some(r), confidence, timings }
}

/// Ground distance (m) between the candidate's map centre and the true one.
fn centre_error_m(c: &Candidate, w: usize, h: usize) -> f64 {
    let a = c.center_merc(w, h);
    let b = truth().apply(w as f64 / 2.0, h as f64 / 2.0);
    let (la, lo) = merc_to_latlon(a.0, a.1);
    let (lb, lob) = merc_to_latlon(b.0, b.1);
    haversine_m(LatLon::new(la, lo), LatLon::new(lb, lob))
}

/// True ground scale of the sheet (m/px): 1:25 000 at 300 dpi = 2.117 m/px
/// Mercator × cos(25.85°).
const TRUE_SCALE: f64 = 1.906;

fn report(name: &str, o: &Outcome, p: &Prepared) {
    for (i, c) in o.candidates.iter().enumerate() {
        eprintln!(
            "[{name}] cand {i}: ncc {:.3} ratio {:.2} scale {:.3} centre err {:.0} m",
            c.score,
            c.peak_ratio,
            c.scale_m_per_px,
            if p.w == 3507 { centre_error_m(c, p.w, p.h) } else { f64::NAN }
        );
    }
    if let Some(r) = &o.refined {
        eprintln!(
            "[{name}] refined: cost {:.2} m, seg std {:.2} m, inliers {:.2}, R² {:.4}, relief {:.0} m, coverage {:.2}, \
             {} segs, interval {:.2} m, snr {:.2}, affine {}, iters {}, fit conf {:.3}",
            r.cost_m,
            r.median_seg_std_m,
            r.inlier_fraction,
            r.explained_variance,
            r.relief_m,
            r.dem_coverage,
            r.n_segments,
            r.contour_interval_m,
            r.interval_snr,
            r.affine,
            r.iterations,
            mapme_core::refine::fit_confidence(r)
        );
    }
    let t: Vec<String> = o.timings.iter().map(|(k, v)| format!("{k} {:.0} ms", v * 1e3)).collect();
    eprintln!("[{name}] confidence {:.3}; {}", o.confidence, t.join(", "));
}

fn check_positive(name: &str, lat: f64, lon: f64) {
    let _g = serial();
    let p = grootkloof();
    let o = locate(p, store(), &params_at(lat, lon));
    report(name, &o, p);
    let best = &o.candidates[0];
    let err = centre_error_m(best, p.w, p.h);
    assert!(err < 150.0, "{name}: coarse centre error {err:.0} m");
    let rel = (best.scale_m_per_px / TRUE_SCALE - 1.0).abs();
    assert!(rel < 0.06, "{name}: coarse scale {:.3} vs {TRUE_SCALE}", best.scale_m_per_px);
    let r = o.refined.as_ref().unwrap();
    let max_err = max_error_m(&r.transform.to_homography());
    eprintln!("[{name}] refined max error over the sheet: {max_err:.1} m");
    assert!(max_err < 40.0, "{name}: refined max error {max_err:.1} m");
    assert!(o.confidence >= 0.7, "{name}: confidence {:.3}", o.confidence);
    // Refining all candidates and keeping the best fit picks the same one.
    let (i, conf, r) = locate_all(p, store(), &o.candidates).unwrap();
    eprintln!("[{name}] select_refined: candidate {i}, confidence {conf:.3}");
    assert_eq!(i, 0);
    assert!(conf >= 0.7 && max_error_m(&r.transform.to_homography()) < 40.0);
}

/// (a) GPS at the map centre.
#[test]
fn grootkloof_gps_at_centre() {
    check_positive("centre", CENTER.0, CENTER.1);
}

/// (b) GPS 12 km north-east of the map centre.
#[test]
fn grootkloof_gps_12km_ne() {
    // 8.5 km north and 8.5 km east.
    check_positive("12km NE", CENTER.0 + 0.0764, CENTER.1 + 0.0846);
}

/// (b) GPS 16 km south-west of the map centre.
#[test]
fn grootkloof_gps_16km_sw() {
    check_positive("16km SW", CENTER.0 - 0.1018, CENTER.1 - 0.1128);
}

/// (c) GPS 55 km west: the sheet lies outside the 20 km search area.
#[test]
fn grootkloof_gps_55km_west_is_rejected() {
    let _g = serial();
    let p = grootkloof();
    let o = locate(p, store(), &params_at(CENTER.0, CENTER.1 - 0.55));
    report("55km W", &o, p);
    assert!(o.candidates.is_empty() || o.confidence <= 0.3, "confidence {:.3}", o.confidence);
    // The fit alone rejects it too.
    if let Some(r) = &o.refined {
        assert!(mapme_core::refine::fit_confidence(r) <= 0.3);
    }
    if let Some((_, conf, _)) = locate_all(p, store(), &o.candidates) {
        eprintln!("[55km W] select_refined confidence {conf:.3}");
        assert!(conf <= 0.3);
    }
}

/// Smooth random field on a coarse grid, bilinearly interpolated.
struct RandomSurface {
    gw: usize,
    gh: usize,
    cell: f64,
    v: Vec<f64>,
}

impl RandomSurface {
    fn new(gw: usize, gh: usize, cell: f64, seed: u64, octaves: &[(f64, f64)]) -> RandomSurface {
        let mut state = seed;
        let mut rnd = || {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            (state >> 11) as f64 / (1u64 << 53) as f64 - 0.5
        };
        let mut v = vec![0.0f64; gw * gh];
        for &(sigma, amp) in octaves {
            let noise: Vec<f32> = (0..gw * gh).map(|_| rnd() as f32).collect();
            let g = mapme_core::image::GrayF32 { w: gw, h: gh, data: noise }.gaussian_blur(sigma);
            let sd = (g.data.iter().map(|x| (*x as f64).powi(2)).sum::<f64>() / g.data.len() as f64).sqrt();
            for (o, x) in v.iter_mut().zip(&g.data) {
                *o += amp * *x as f64 / sd.max(1e-12);
            }
        }
        RandomSurface { gw, gh, cell, v }
    }

    /// Value at continuous grid-metre coordinates (u, v) from the grid origin.
    fn at(&self, u: f64, v: f64) -> f64 {
        let x = (u / self.cell).clamp(0.0, (self.gw - 2) as f64);
        let y = (v / self.cell).clamp(0.0, (self.gh - 2) as f64);
        let (x0, y0) = (x.floor() as usize, y.floor() as usize);
        let (fx, fy) = (x - x0 as f64, y - y0 as f64);
        let g = |xx: usize, yy: usize| self.v[yy * self.gw + xx];
        g(x0, y0) * (1.0 - fx) * (1.0 - fy)
            + g(x0 + 1, y0) * fx * (1.0 - fy)
            + g(x0, y0 + 1) * (1.0 - fx) * fy
            + g(x0 + 1, y0 + 1) * fx * fy
    }
}

/// Render contour lines (every `interval`) of `elev(x, y)` as 2-px lines of
/// `colour` on white paper.
fn render_contours(
    w: usize,
    h: usize,
    interval: f64,
    colour: [u8; 3],
    elev: &(dyn Fn(f64, f64) -> f64 + Sync),
) -> Rgb8 {
    use rayon::prelude::*;
    let lvl: Vec<i64> = (0..w * h)
        .into_par_iter()
        .map(|i| (elev((i % w) as f64 + 0.5, (i / w) as f64 + 0.5) / interval).floor() as i64)
        .collect();
    let mut data = vec![255u8; w * h * 3];
    data.par_chunks_mut(w * 3).enumerate().for_each(|(y, row)| {
        for x in 0..w {
            let l = lvl[y * w + x];
            let edge = (x + 1 < w && lvl[y * w + x + 1] != l)
                || (y + 1 < h && lvl[(y + 1) * w + x] != l)
                || (x > 0 && lvl[y * w + x - 1] != l && lvl[y * w + x - 1] > l)
                || (y > 0 && lvl[(y - 1) * w + x] != l && lvl[(y - 1) * w + x] > l);
            if edge {
                row[x * 3..x * 3 + 3].copy_from_slice(&colour);
            }
        }
    });
    Rgb8::from_rgb(w, h, data).unwrap()
}

/// (d) A sheet of realistic-looking contours of random terrain, with GPS at
/// the real Grootkloof location: nothing should match.
#[test]
fn random_terrain_map_is_rejected() {
    let _g = serial();
    // ~3 m contour spacing on paper-ish terrain: 120 m relief over 6.7 × 9.5 km.
    let surf = RandomSurface::new(240, 340, 30.0, 0x5eed_1234_abcd, &[(12.0, 60.0), (4.0, 20.0)]);
    let img = render_contours(3507, 4960, 5.0, [168, 152, 136], &|x, y| surf.at(x * 1.906, y * 1.906));
    let p = prepare(&img);
    eprintln!(
        "[random] {} segments, {} samples, mask {:.1}%",
        p.samples.n_segs,
        p.samples.len(),
        100.0 * p.mask.fraction()
    );
    let o = locate(&p, store(), &params_at(CENTER.0, CENTER.1));
    report("random", &o, &p);
    assert!(o.candidates.is_empty() || o.confidence <= 0.3, "confidence {:.3}", o.confidence);
    if let Some(r) = &o.refined {
        assert!(mapme_core::refine::fit_confidence(r) <= 0.3);
    }
    if let Some((_, conf, _)) = locate_all(&p, store(), &o.candidates) {
        eprintln!("[random] select_refined confidence {conf:.3}");
        assert!(conf <= 0.3);
    }
}

/// (e) Whole contour pipeline from RGB pixels on the real sheet.
#[test]
fn full_pipeline_timing() {
    let _g = serial();
    let img = load_map();
    let dems = store();
    let t0 = Instant::now();
    let t = Instant::now();
    let (mask, _) = auto_mask(&img);
    let t_mask = t.elapsed().as_secs_f64();
    let t = Instant::now();
    let (min_len, step) = default_sampling(img.w, img.h);
    let samples = contour_samples(&mask, min_len, step);
    let t_skel = t.elapsed().as_secs_f64();
    let t = Instant::now();
    let field = map_field(&mask);
    let t_field = t.elapsed().as_secs_f64();
    let p = Prepared { w: img.w, h: img.h, mask, samples, field };
    let o = locate(&p, dems, &params_at(CENTER.0, CENTER.1));
    let total = t0.elapsed().as_secs_f64();
    eprintln!(
        "[timing] mask {:.0} ms, samples {:.0} ms ({} segs, {} pts), field {:.0} ms, {}; total {:.2} s",
        t_mask * 1e3,
        t_skel * 1e3,
        p.samples.n_segs,
        p.samples.len(),
        t_field * 1e3,
        o.timings.iter().map(|(k, v)| format!("{k} {:.0} ms", v * 1e3)).collect::<Vec<_>>().join(", "),
        total
    );
    assert!(o.confidence >= 0.7);
    assert!(t_skel < 1.0, "skeleton {t_skel:.2} s");
    assert!(total < 5.0, "pipeline {total:.2} s");
}

/// Refinement converges from coarse starts several hundred metres off.
#[test]
fn refine_converges_from_offset_starts() {
    let _g = serial();
    let p = grootkloof();
    let tr = truth();
    let prep = PreparedDem::new(&fine_dem(store(), &tr, p.w, p.h));
    let lc = (-25.85f64).to_radians().cos();
    let c = tr.apply(p.w as f64 / 2.0, p.h as f64 / 2.0);
    for (dx, dy, ds, dth) in [
        (120.0, -80.0, 1.02, 0.0),
        (-300.0, 200.0, 0.98, 0.01),
        (400.0, 250.0, 1.03, -0.01),
        (-450.0, -300.0, 1.0, 0.0),
    ] {
        let init = Affine::pixel_to_merc_similarity(
            tr.mean_scale() * ds,
            dth,
            p.w as f64 / 2.0,
            p.h as f64 / 2.0,
            c.0 + dx / lc,
            c.1 + dy / lc,
        );
        let e0 = max_error_m(&init.to_homography());
        let t = Instant::now();
        let r = refine_prepared(&p.samples, &prep, &init);
        let e1 = max_error_m(&r.transform.to_homography());
        eprintln!("[refine] start {e0:.0} m -> {e1:.1} m in {:.0} ms", t.elapsed().as_secs_f64() * 1e3);
        assert!(e1 < 25.0, "start {e0:.0} m -> {e1:.1} m");
    }
    // The ground truth itself fits the DEM to the noise floor.
    let ev = mapme_core::refine::evaluate_prepared(&p.samples, &prep, &tr);
    eprintln!(
        "[refine] truth: cost {:.2} m, seg std {:.2} m, R² {:.4}",
        ev.cost_m, ev.median_seg_std_m, ev.explained_variance
    );
    assert!(ev.explained_variance > 0.99 && ev.median_seg_std_m < 4.0);
}

/// Mask, samples, mask timings on the real sheet.
#[test]
fn mask_and_skeleton_on_real_map() {
    let _g = serial();
    let img = load_map();
    let t0 = Instant::now();
    let (mask, model) = auto_mask(&img);
    let t_mask = t0.elapsed().as_secs_f64();
    let t1 = Instant::now();
    let (min_len, step) = default_sampling(img.w, img.h);
    let s = contour_samples(&mask, min_len, step);
    let t_skel = t1.elapsed().as_secs_f64();
    eprintln!(
        "[mask] {:.0} ms ({:.1}% of pixels, {:?}), skeleton {:.0} ms: {} segments, {} samples",
        t_mask * 1e3,
        100.0 * mask.fraction(),
        model,
        t_skel * 1e3,
        s.n_segs,
        s.pts.len()
    );
    assert!(mask.fraction() > 0.05 && mask.fraction() < 0.2);
    assert!(s.n_segs > 300);
    assert!(t_skel < 1.0);
}

/// Synthetic DEM tiles at each (zoom, box) from an elevation function of
/// Mercator metres; tiles for which `missing(z, index)` holds are marked missing.
fn synthetic_store(
    zooms: &[(u8, MercBox)],
    elev: &(dyn Fn(f64, f64) -> f64 + Sync),
    missing: &dyn Fn(u8, usize) -> bool,
) -> DemStore {
    use rayon::prelude::*;
    let mut s = DemStore::new();
    for (z, b) in zooms {
        let res = mapme_core::dem::resolution(*z);
        let mut wanted = Vec::new();
        for (k, t) in tiles_covering(*z, b).into_iter().enumerate() {
            if missing(*z, k) {
                s.mark_missing(t);
            } else {
                wanted.push(t);
            }
        }
        let tiles: Vec<(TileId, Vec<f32>)> = wanted
            .into_par_iter()
            .map(|t| {
                let tb = tile_bounds(t);
                let mut e = vec![0.0f32; TILE_SIZE * TILE_SIZE];
                for r in 0..TILE_SIZE {
                    for c in 0..TILE_SIZE {
                        e[r * TILE_SIZE + c] =
                            elev(tb.min_x + (c as f64 + 0.5) * res, tb.max_y - (r as f64 + 0.5) * res) as f32;
                    }
                }
                (t, e)
            })
            .collect();
        for (t, e) in tiles {
            s.add_elevations(t, e);
        }
    }
    s
}

/// A synthetic map of synthetic terrain at a known transform.
struct Synth {
    name: &'static str,
    w: usize,
    h: usize,
    /// Ground metres per pixel.
    scale: f64,
    /// True map-up bearing (degrees clockwise from north).
    rot_deg: f64,
    /// Map centre offset from the GPS fix (east, north), ground metres.
    offset_m: (f64, f64),
    interval: f64,
    colour: [u8; 3],
    distractors: bool,
    rotations: Vec<f64>,
    radius_m: f64,
    /// Mark every n-th coarse tile missing (0 = none).
    missing_every: usize,
}

impl Default for Synth {
    fn default() -> Self {
        Synth {
            name: "synthetic",
            w: 1800,
            h: 2400,
            scale: 3.0,
            rot_deg: 0.0,
            offset_m: (4243.0, 4243.0),
            interval: 10.0,
            colour: [168, 152, 136],
            distractors: false,
            rotations: vec![0.0],
            radius_m: 15_000.0,
            missing_every: 0,
        }
    }
}

/// Typical non-contour map content: black grid lines and "text", blue
/// rivers, green vegetation tint, a yellow road.
fn add_distractors(img: &mut Rgb8) {
    let (w, h) = (img.w, img.h);
    let mut put = |x: usize, y: usize, c: [u8; 3], alpha: f32| {
        if x < w && y < h {
            let i = (y * w + x) * 3;
            for (p, &ck) in img.data[i..i + 3].iter_mut().zip(&c) {
                *p = (*p as f32 * (1.0 - alpha) + ck as f32 * alpha) as u8;
            }
        }
    };
    for y in 0..h {
        for x in 0..w {
            if x % 400 < 2 || y % 400 < 2 {
                put(x, y, [20, 20, 20], 1.0);
            }
            if (x / 150 + y / 210) % 7 == 0 && (x % 150) < 60 && (y % 210) < 90 {
                put(x, y, [110, 190, 80], 0.45);
            }
            if (x % 97 < 10) && (y % 131 < 14) && (x / 97 + y / 131) % 3 == 0 {
                put(x, y, [10, 10, 10], 1.0);
            }
        }
    }
    for x in 0..w {
        let yr = h as f64 * 0.35 + 120.0 * (x as f64 / 230.0).sin();
        let yy = h as f64 * 0.7 + 60.0 * (x as f64 / 170.0).cos() + x as f64 * 0.2;
        for d in 0..4 {
            put(x, yr as usize + d, [40, 110, 220], 1.0);
        }
        for d in 0..9 {
            put(x, yy as usize + d, [245, 215, 50], 1.0);
        }
    }
}

fn run_synthetic(cfg: &Synth) {
    let _g = serial();
    let (lat0, lon0) = (46.5, 8.0);
    let origin = latlon_to_merc(lat0, lon0);
    // Terrain: random surface over 120 × 120 km Mercator around the origin.
    let surf = RandomSurface::new(800, 800, 150.0, 42, &[(10.0, 400.0), (3.0, 60.0)]);
    let half = 60_000.0;
    let elev = move |mx: f64, my: f64| surf.at(mx - origin.0 + half, origin.1 + half - my) + 1500.0;
    let lc = lat0.to_radians().cos();
    let (w, h) = (cfg.w, cfg.h);
    let centre = (origin.0 + cfg.offset_m.0 / lc, origin.1 + cfg.offset_m.1 / lc);
    let tr = Affine::pixel_to_merc_similarity(
        cfg.scale / lc,
        cfg.rot_deg.to_radians(),
        w as f64 / 2.0,
        h as f64 / 2.0,
        centre.0,
        centre.1,
    );
    let mut img = render_contours(w, h, cfg.interval, cfg.colour, &|x, y| {
        let (mx, my) = tr.apply(x, y);
        elev(mx, my)
    });
    if cfg.distractors {
        add_distractors(&mut img);
    }
    let params = SearchParams {
        center_merc: origin,
        search_radius_m: cfg.radius_m,
        scale_min: 1.0,
        scale_max: 8.0,
        rotations_deg: cfg.rotations.clone(),
        top_k: 3,
    };
    let coarse_box = required_dem_box(&params, w, h);
    let fine_box = refine_dem_box(&tr, w, h);
    let every = cfg.missing_every;
    let dems =
        synthetic_store(&[(11, coarse_box), (13, fine_box)], &elev, &|z, k| z == 11 && every > 0 && k % every == 0);
    if every > 0 {
        let cov = dems.mosaic(11, &coarse_box).valid_fraction();
        eprintln!("[{}] coarse DEM coverage {:.0}%", cfg.name, 100.0 * cov);
    }
    let p = prepare(&img);
    let o = locate(&p, &dems, &params);
    report(cfg.name, &o, &p);
    let best = &o.candidates[0];
    let c = best.center_merc(w, h);
    let err = ((c.0 - centre.0).powi(2) + (c.1 - centre.1).powi(2)).sqrt() * lc;
    eprintln!("[{}] coarse centre error {err:.0} m, rotation {:.1}°", cfg.name, best.rotation_deg);
    assert!(err < 150.0, "{}: coarse centre error {err:.0} m", cfg.name);
    assert!((best.scale_m_per_px / cfg.scale - 1.0).abs() < 0.06, "{}: scale {}", cfg.name, best.scale_m_per_px);
    let dr = (best.rotation_deg - cfg.rot_deg + 540.0).rem_euclid(360.0) - 180.0;
    assert!(dr.abs() <= 6.0, "{}: rotation {} vs {}", cfg.name, best.rotation_deg, cfg.rot_deg);
    let r = o.refined.as_ref().unwrap();
    let mut worst = 0.0f64;
    for (x, y) in [(0.0, 0.0), (w as f64, 0.0), (0.0, h as f64), (w as f64, h as f64)] {
        let (a, b) = (r.transform.apply(x, y), tr.apply(x, y));
        worst = worst.max(((a.0 - b.0).powi(2) + (a.1 - b.1).powi(2)).sqrt() * lc);
    }
    eprintln!("[{}] refined corner error {worst:.1} m", cfg.name);
    assert!(worst < 15.0, "{}: refined corner error {worst:.1} m", cfg.name);
    assert!(o.confidence >= 0.7, "{}: confidence {:.3}", cfg.name, o.confidence);
}

/// (f) Synthetic terrain: render a map of its contours at a known transform,
/// then recover the transform with the full pipeline.
#[test]
fn synthetic_dem_map_is_located() {
    run_synthetic(&Synth::default());
}

/// Swiss/OS-style orange contours among grid lines, text, rivers, vegetation
/// tint and a yellow road.
#[test]
fn synthetic_orange_contours_with_clutter() {
    run_synthetic(&Synth {
        name: "orange+clutter",
        colour: [210, 140, 70],
        distractors: true,
        offset_m: (-6000.0, 2500.0),
        scale: 2.2,
        ..Default::default()
    });
}

/// USGS-style brown 40 ft contours on a smaller image (1:24 000 at 200 dpi).
#[test]
fn synthetic_usgs_brown_small_map() {
    run_synthetic(&Synth {
        name: "usgs-small",
        w: 1200,
        h: 1500,
        colour: [186, 140, 100],
        interval: 12.192,
        scale: 3.05,
        offset_m: (2000.0, -9000.0),
        ..Default::default()
    });
}

/// A map rotated 22° found by a rotation sweep (10° steps), then refined.
#[test]
fn synthetic_rotated_map_with_sweep() {
    run_synthetic(&Synth {
        name: "rotated",
        rot_deg: 22.0,
        rotations: (0..36).map(|i| i as f64 * 10.0).collect(),
        ..Default::default()
    });
}

/// A small rotation (UTM grid north, skewed scan) needs no sweep.
#[test]
fn synthetic_small_rotation_without_sweep() {
    run_synthetic(&Synth { name: "rot 4°", rot_deg: 4.0, ..Default::default() });
}

/// Missing coarse DEM tiles.
#[test]
fn synthetic_missing_tiles() {
    run_synthetic(&Synth { name: "missing", missing_every: 5, ..Default::default() });
}

/// A 40 km search radius (FFT tiling on the finest level).
#[test]
fn synthetic_large_radius() {
    run_synthetic(&Synth { name: "40km", radius_m: 40_000.0, offset_m: (-20000.0, 15000.0), ..Default::default() });
}

/// Degenerate inputs never panic and never produce confident answers.
#[test]
fn degenerate_inputs_are_handled() {
    let _g = serial();
    let params = params_at(CENTER.0, CENTER.1);
    // A blank sheet (no contours at all).
    let blank = Rgb8::from_rgb(1200, 1600, vec![250u8; 1200 * 1600 * 3]).unwrap();
    let p = prepare(&blank);
    assert_eq!(p.samples.len(), 0);
    let o = locate(&p, store(), &params);
    report("blank", &o, &p);
    assert!(o.candidates.is_empty() && o.confidence == 0.0);
    // A tiny image: no scale is searchable.
    let tiny = prepare(&Rgb8::from_rgb(40, 30, vec![200u8; 40 * 30 * 3]).unwrap());
    assert!(locate(&tiny, store(), &params).candidates.is_empty());
    // Zero-sized inputs.
    let empty = Mask::new(0, 0);
    assert_eq!(mapme_core::skeleton::thin(&empty).data.len(), 0);
    assert_eq!(map_field(&empty).w, 0);
    assert!(contour_samples(&empty, 10, 2).is_empty());
    // No DEM at all (every coarse tile missing).
    let g = grootkloof();
    let mut none = DemStore::new();
    for t in tiles_covering(11, &required_dem_box(&params, g.w, g.h)) {
        none.mark_missing(t);
    }
    let o = locate(g, &none, &params);
    assert!(o.candidates.is_empty() || o.confidence <= 0.3);
    // Cancellation returns nothing; progress ends near 1.
    let dem = store().mosaic(11, &required_dem_box(&params, g.w, g.h));
    let c = coarse_search(&g.field, &dem, g.w, g.h, &params, &|| true, &|_| {});
    assert!(c.is_empty());
    let last = std::sync::Mutex::new(0.0f32);
    let c = coarse_search(&g.field, &dem, g.w, g.h, &params, &|| false, &|f| {
        let mut l = last.lock().unwrap();
        *l = l.max(f);
    });
    assert!(!c.is_empty());
    let l = *last.lock().unwrap();
    assert!(l > 0.95 && l <= 1.0, "{l}");
    // Refinement and evaluation on an empty sample set.
    let r = mapme_core::refine::refine(&ContourSamples::default(), &dem, &c[0].transform);
    assert_eq!(mapme_core::refine::fit_confidence(&r), 0.0);
}

#[test]
#[ignore]
fn tune_search() {
    let p = grootkloof();
    let tc = truth().apply(p.w as f64 / 2.0, p.h as f64 / 2.0);
    let lc = (-25.85f64).to_radians().cos();
    let mut line = String::new();
    for (name, lat, lon) in [
        ("c", CENTER.0, CENTER.1),
        ("ne", CENTER.0 + 0.0764, CENTER.1 + 0.0846),
        ("sw", CENTER.0 - 0.1018, CENTER.1 - 0.1128),
        ("w55", CENTER.0, CENTER.1 - 0.55),
    ] {
        let params = params_at(lat, lon);
        let dem = store().mosaic(11, &required_dem_box(&params, p.w, p.h));
        let peaks = mapme_core::search::search_peaks(&p.field, &dem, p.w, p.h, &params, &|| false, &|_| {});
        let (mut tz, mut fz) = (f32::MIN, f32::MIN);
        let mut best: Option<&mapme_core::search::Peak> = None;
        for pk in &peaks {
            let d = ((pk.mx - tc.0).powi(2) + (pk.my - tc.1).powi(2)).sqrt() * lc;
            if d < 2000.0 {
                tz = tz.max(pk.z());
            } else {
                fz = fz.max(pk.z());
            }
            if best.is_none_or(|b| pk.z() > b.z()) {
                best = Some(pk);
            }
        }
        let b = best.unwrap();
        let second = peaks
            .iter()
            .filter(|q| ((q.mx - b.mx).powi(2) + (q.my - b.my).powi(2)).sqrt() * lc >= 2000.0)
            .map(|q| q.z())
            .fold(f32::MIN, f32::max);
        let err = ((b.mx - tc.0).powi(2) + (b.my - tc.1).powi(2)).sqrt() * lc;
        line.push_str(&format!("{name}: tz {tz:.2} fz {fz:.2} r {:.2} err {err:.0} | ", b.z() / second.max(2.0)));
    }
    eprintln!("TUNE {line}");
}
