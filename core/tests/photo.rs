//! Photo mode on synthetic photographs of the Grootkloof sheet: the paper is
//! photographed on a table by a tilted camera (perspective, vignetting, a
//! one-sided shadow, a warm colour cast, lens blur, sensor noise) and then
//! flattened the way the app does it, from page corners that are slightly
//! off (0.5 % and 2.5 % of the photo size), so the flattened image keeps a
//! residual perspective and a wrong aspect ratio. Requests go through the
//! JSON API with `"photo": true`, feeding DEM tiles on `need_dem` like the
//! app. Run with `cargo test --release`.

mod common;
use common::*;
use mapme_core::contour::{auto_mask, normalize_illumination, photo_mask};
use mapme_core::geo::{latlon_to_merc, Affine, Homography};
use mapme_core::georef::Session;
use mapme_core::grid::detect_lines;
use mapme_core::image::Rgb8;
use serde_json::{json, Value};
use std::sync::{Mutex, OnceLock};
use std::time::Instant;

/// Heavy tests run one at a time so timings are meaningful.
fn serial() -> std::sync::MutexGuard<'static, ()> {
    static M: Mutex<()> = Mutex::new(());
    M.lock().unwrap_or_else(|e| e.into_inner())
}

struct Fixture {
    map: Rgb8,
    cam: Homography,
    photo: Rgb8,
}

fn fixture() -> &'static Fixture {
    static F: OnceLock<Fixture> = OnceLock::new();
    F.get_or_init(|| {
        let map = load_map();
        let cam = photo_camera(&map);
        let t = Instant::now();
        let photo = synth_photo(&map, &cam, 7);
        eprintln!("[synth] {}x{} photo in {} ms", photo.w, photo.h, t.elapsed().as_millis());
        Fixture { map, cam, photo }
    })
}

/// Corner errors of the simulated page detection (fraction of the photo
/// size; negative: corners outside the page, so the table shows).
const SMALL: f64 = 0.005;
const LARGE: f64 = 0.025;
const OUTSIDE: f64 = -0.025;

fn case(corner_error: f64) -> &'static PhotoCase {
    static S: OnceLock<PhotoCase> = OnceLock::new();
    static L: OnceLock<PhotoCase> = OnceLock::new();
    static O: OnceLock<PhotoCase> = OnceLock::new();
    let cell = match corner_error {
        SMALL => &S,
        LARGE => &L,
        _ => &O,
    };
    cell.get_or_init(|| {
        let f = fixture();
        let c = photo_case(&f.map, &f.photo, &f.cam, corner_error);
        eprintln!("[synth] corner error {corner_error}: flattened to {}x{}", c.image.w, c.image.h);
        c
    })
}

/// The app's DEM loop: call, add the requested fixture tiles (or mark them
/// missing), call again. Returns the final response, rounds and core seconds.
fn run_loop(s: &Session, req: &Value) -> (Value, usize, f64) {
    let mut core_s = 0.0;
    for round in 1..=6 {
        let t = Instant::now();
        let r: Value = serde_json::from_str(&s.georeference_json(&req.to_string())).unwrap();
        core_s += t.elapsed().as_secs_f64();
        if r["status"] != "need_dem" {
            return (r, round, core_s);
        }
        for t in r["need_tiles"].as_array().unwrap() {
            let id = serde_json::from_value(t.clone()).unwrap();
            let (z, x, y) = (t["z"].as_u64().unwrap(), t["x"].as_u64().unwrap(), t["y"].as_u64().unwrap());
            match std::fs::read(fixture_dir().join("dem").join(format!("{z}_{x}_{y}.png"))) {
                Ok(png) => s.add_dem_tile(id, &png).unwrap(),
                Err(_) => s.mark_dem_tile_missing(id).unwrap(),
            }
        }
    }
    panic!("still need_dem after 6 rounds");
}

fn homography(r: &Value) -> Homography {
    let m: Vec<f64> = r["pixel_to_mercator"].as_array().unwrap().iter().map(|v| v.as_f64().unwrap()).collect();
    Homography { m: m.try_into().unwrap() }
}

fn gps(lat: f64, lon: f64) -> Value {
    json!({"lat": lat, "lon": lon, "accuracy_m": 8.0})
}

fn summary(name: &str, r: &Value, err: f64, rounds: usize, core_s: f64) {
    eprintln!(
        "[{name}] {} / {} conf {} rms {} scale {} rot {} max err {err:.1} m, {rounds} calls, core {:.0} ms\n    \
         message: {}\n    timings: {}\n    diagnostics: {}",
        r["status"],
        r["method"],
        r["confidence"],
        r["rms_m"],
        r["scale_m_per_px"],
        r["rotation_deg"],
        core_s * 1e3,
        r["message"].as_str().unwrap_or(""),
        r["timings_ms"],
        r["diagnostics"]
    );
}

/// Response fields that must hold for any located photo.
fn check_geometry(c: &PhotoCase, r: &Value) {
    let h = homography(r);
    let (w, hh) = (c.image.w as f64, c.image.h as f64);
    let truth = c.truth();
    // Scale: ground metres per pixel at the image centre (local affine of the truth).
    let lat = (-25.85f64).to_radians().cos();
    let ts = truth.local_affine(w / 2.0, hh / 2.0).mean_scale() * lat;
    let scale = r["scale_m_per_px"].as_f64().unwrap();
    assert!((scale / ts - 1.0).abs() < 0.02, "scale {scale} vs {ts}");
    let rot = r["rotation_deg"].as_f64().unwrap();
    let tr = truth.local_affine(w / 2.0, hh / 2.0).map_up_bearing_deg();
    let d = (rot - tr).rem_euclid(360.0);
    assert!(d.min(360.0 - d) < 0.5, "rotation {rot} vs {tr}");
    // Corners TL, TR, BR, BL are the homography's image of the image corners.
    let corners = r["corners"].as_array().unwrap();
    for (k, &(x, y)) in [(0.0, 0.0), (w, 0.0), (w, hh), (0.0, hh)].iter().enumerate() {
        let (mx, my) = h.apply(x, y);
        let (la, lo) = mapme_core::geo::merc_to_latlon(mx, my);
        let ck = &corners[k];
        assert!((ck[0].as_f64().unwrap() - la).abs() < 1e-6 && (ck[1].as_f64().unwrap() - lo).abs() < 1e-6);
        let (tx, ty) = truth.apply(x, y);
        assert!((mx - tx).hypot(my - ty) * lat < 120.0, "corner {k} off by {:.0} m", (mx - tx).hypot(my - ty) * lat);
    }
}

// ---------------------------------------------------------------------------
// Grid labels
// ---------------------------------------------------------------------------

fn grid_photo(corner_error: f64, max_err_m: f64, verify: bool) {
    let _g = serial();
    let c = case(corner_error);
    let t = Instant::now();
    let s = Session::new(c.image.clone());
    let t_new = t.elapsed().as_secs_f64();
    let req = json!({"gps": gps(CENTER.0, CENTER.1), "ocr": c.ocr(), "photo": true, "verify": verify});
    let (r, rounds, core_s) = run_loop(&s, &req);
    let err = c.max_error_m(&homography(&r));
    summary(&format!("photo grid, corners off {:.1} %", corner_error * 100.0), &r, err, rounds, core_s);
    eprintln!("[timing] photo grid path: session {:.0} ms + {:.0} ms", t_new * 1e3, core_s * 1e3);
    assert_eq!(r["status"], "ok", "{r}");
    assert_eq!(r["method"], "grid");
    assert_eq!(r["diagnostics"]["photo"], true);
    assert_eq!(r["diagnostics"]["grid"]["method"], "lines");
    assert_eq!(r["diagnostics"]["grid"]["model"], "homography");
    let m = homography(&r).m;
    assert!(m[6] != 0.0 || m[7] != 0.0, "expected a true homography: {m:?}");
    assert!(err < max_err_m, "max error {err:.1} m");
    assert!(r["confidence"].as_f64().unwrap() >= 0.9);
    check_geometry(c, &r);
    if verify {
        // The homography checked against the terrain through the photo's contours.
        let check = &r["diagnostics"]["contour_check"];
        assert_eq!(check["agrees"], true, "{check}");
        assert!(check["median_seg_std_m"].as_f64().unwrap() < 4.0, "{check}");
    }

    // The same GCPs fitted with an affine (what scan mode would report).
    let g = mapme_core::grid::georeference_from_grid_with(
        &normalize_illumination(&c.image),
        &mapme_core::grid::detect_lines_photo(&normalize_illumination(&c.image)),
        &c.ocr(),
        Some(mapme_core::geo::LatLon::new(CENTER.0, CENTER.1)),
        &mapme_core::grid::GridOptions { photo: true },
    )
    .unwrap();
    let src: Vec<(f64, f64)> = g.gcps.iter().map(|p| (p.x, p.y)).collect();
    let dst: Vec<(f64, f64)> = g.gcps.iter().map(|p| latlon_to_merc(p.lat, p.lon)).collect();
    let affine_err = c.max_error_m(&Affine::fit(&src, &dst).unwrap().to_homography());
    eprintln!(
        "[photo grid, corners off {:.1} %] homography from {} GCPs: max err {err:.1} m; affine from the same GCPs: \
         {affine_err:.1} m",
        corner_error * 100.0,
        src.len()
    );
    if corner_error >= LARGE {
        assert!(affine_err > 3.0 * err.max(5.0), "affine {affine_err:.1} m vs homography {err:.1} m");
    }
}

#[test]
fn photo_grid_small_corner_error() {
    grid_photo(SMALL, 10.0, false);
}

#[test]
fn photo_grid_large_corner_error() {
    grid_photo(LARGE, 20.0, true);
}

// ---------------------------------------------------------------------------
// Contours vs terrain
// ---------------------------------------------------------------------------

/// GPS 12 km north-east of the sheet centre (as in the scan tests).
const NE_12KM: (f64, f64) = (CENTER.0 + 0.0764, CENTER.1 + 0.0846);

fn contours_photo(corner_error: f64, max_err_m: f64) -> Value {
    let _g = serial();
    let c = case(corner_error);
    let t = Instant::now();
    let s = Session::new(c.image.clone());
    let t_new = t.elapsed().as_secs_f64();
    let req = json!({"gps": gps(NE_12KM.0, NE_12KM.1), "photo": true});
    let (r, rounds, core_s) = run_loop(&s, &req);
    let err = c.max_error_m(&homography(&r));
    summary(&format!("photo contours, corners off {:.1} %", corner_error * 100.0), &r, err, rounds, core_s);
    let total = t_new + core_s;
    eprintln!(
        "[timing] photo contour path: session {:.0} ms + {rounds} calls {:.0} ms = {:.0} ms (excluding DEM download)",
        t_new * 1e3,
        core_s * 1e3,
        total * 1e3
    );
    assert_eq!(r["status"], "ok", "{r}");
    assert_eq!(r["method"], "contours");
    assert!(err < max_err_m, "max error {err:.1} m");
    assert!(r["confidence"].as_f64().unwrap() >= 0.7, "{}", r["confidence"]);
    assert_eq!(r["diagnostics"]["refined"]["dem_zoom"], 13);
    // The coarse search (similarity templates) found the sheet despite the
    // perspective and the ~10 % aspect error of the flattening.
    let cand = &r["diagnostics"]["candidates"][0];
    let centre = latlon_to_merc(cand["center"][0].as_f64().unwrap(), cand["center"][1].as_f64().unwrap());
    let truth = c.truth().apply(c.image.w as f64 / 2.0, c.image.h as f64 / 2.0);
    let off = (centre.0 - truth.0).hypot(centre.1 - truth.1) * (-25.85f64).to_radians().cos();
    eprintln!("[coarse] best candidate {off:.0} m from the true centre, peak ratio {}", cand["peak_ratio"]);
    assert!(off < 400.0, "coarse candidate {off:.0} m off: {cand}");
    check_geometry(c, &r);
    assert!(total * 1e3 < time_budget_ms(6000.0), "photo contour path took {total:.1} s");
    r
}

#[test]
fn photo_contours_small_corner_error() {
    contours_photo(SMALL, 40.0);
}

#[test]
fn photo_contours_large_corner_error() {
    let r = contours_photo(LARGE, 60.0);
    // A 2.5 % corner error leaves real perspective: the refinement ends projective.
    assert_eq!(r["diagnostics"]["refined"]["projective"], true, "{}", r["diagnostics"]["refined"]);
    let m = homography(&r).m;
    assert!(m[6] != 0.0 || m[7] != 0.0);
}

/// Corners 2.5 % outside the page: a band of table around the page must not
/// be taken for contour lines (it is blanked by the normalisation).
#[test]
fn photo_contours_with_table_border() {
    contours_photo(OUTSIDE, 60.0);
    let c = case(OUTSIDE);
    let norm = normalize_illumination(&c.image);
    // The image corners show the table; after normalisation they are blank.
    let (mw, mh) = (fixture().map.w as f64, fixture().map.h as f64);
    for (x, y) in [(5, 5), (c.image.w - 6, 5), (c.image.w - 6, c.image.h - 6), (5, c.image.h - 6)] {
        let (u, v) = c.to_map.apply(x as f64, y as f64);
        assert!(u < 0.0 || v < 0.0 || u > mw || v > mh, "({x}, {y}) is on the page");
        assert_eq!(norm.rgb(x, y), [255, 255, 255], "({x}, {y})");
    }
}

/// GPS 55 km west: the sheet is outside the search area. The extra freedom
/// of photo mode (anisotropic affine, projective) must not turn a wrong
/// location into a confident answer.
#[test]
fn photo_contours_far_away_fail_honestly() {
    let _g = serial();
    let c = case(LARGE);
    let s = Session::new(c.image.clone());
    let req = json!({"gps": gps(CENTER.0, CENTER.1 - 0.55), "photo": true});
    let (r, rounds, core_s) = run_loop(&s, &req);
    summary("photo contours 55 km W", &r, f64::NAN, rounds, core_s);
    assert_eq!(r["status"], "failed", "{r}");
    assert!(r["pixel_to_mercator"].is_null());
    if let Some(conf) = r["diagnostics"]["best_confidence"].as_f64() {
        assert!(conf < 0.5, "{conf}");
    }
}

// ---------------------------------------------------------------------------
// Normalisation and extraction on the photo
// ---------------------------------------------------------------------------

/// The flattened photo after normalisation: paper is white and neutral again,
/// in the shadow as in the light, and contours / grid lines are recovered.
#[test]
fn photo_normalisation_restores_paper_and_lines() {
    let _g = serial();
    let c = case(SMALL);
    let t = Instant::now();
    let norm = normalize_illumination(&c.image);
    let t_norm = t.elapsed().as_millis();
    // Paper: pixels whose source in map.jpg is white paper well away from ink.
    let map = &fixture().map;
    let mut raw = Vec::new();
    let mut out = Vec::new();
    for y in (40..c.image.h - 40).step_by(7) {
        for x in (40..c.image.w - 40).step_by(7) {
            let (u, v) = c.to_map.apply(x as f64 + 0.5, y as f64 + 0.5);
            let (u, v) = (u as isize, v as isize);
            let clean = (-6..=6).all(|dy| {
                (-6..=6).all(|dx| {
                    let (xx, yy) = (u + dx, v + dy);
                    xx >= 0
                        && yy >= 0
                        && (xx as usize) < map.w
                        && (yy as usize) < map.h
                        && map.rgb(xx as usize, yy as usize).iter().all(|&ch| ch >= 245)
                })
            });
            if clean {
                raw.push(c.image.rgb(x, y));
                out.push(norm.rgb(x, y));
            }
        }
    }
    assert!(out.len() > 5000, "{} paper samples", out.len());
    let stats = |v: &[[u8; 3]]| {
        let n = v.len() as f64;
        let mean = [0, 1, 2].map(|k| v.iter().map(|p| p[k] as f64).sum::<f64>() / n);
        let mut lum: Vec<f64> = v.iter().map(|p| p.iter().map(|&x| x as f64).sum::<f64>() / 3.0).collect();
        lum.sort_by(|a, b| a.total_cmp(b));
        (mean, lum[lum.len() / 20], lum[lum.len() * 19 / 20])
    };
    let (m0, lo0, hi0) = stats(&raw);
    let (m1, lo1, hi1) = stats(&out);
    eprintln!(
        "[normalise] {} ms; paper before: mean {m0:.0?} 5-95 % luminance {lo0:.0}..{hi0:.0}; after: mean {m1:.0?} \
         {lo1:.0}..{hi1:.0}",
        t_norm
    );
    assert!(hi0 - lo0 > 60.0, "the synthetic photo should be unevenly lit");
    assert!(m1.iter().all(|&v| v > 240.0), "paper not white: {m1:?}");
    assert!(m1.iter().fold(0.0f64, |a, &v| a.max(v)) - m1.iter().fold(255.0f64, |a, &v| a.min(v)) < 6.0, "cast left");
    assert!(lo1 > 225.0, "paper still shaded: 5 % luminance {lo1}");

    // Contours: hysteresis + default model on the normalised photo vs the raw photo.
    let (raw_mask, _) = auto_mask(&c.image);
    let (mask, model) = photo_mask(&norm, None);
    let (min_len, step) = mapme_core::skeleton::default_sampling(norm.w, norm.h);
    let segs = mapme_core::skeleton::contour_samples(&mask, min_len, step).n_segs;
    let raw_segs = mapme_core::skeleton::contour_samples(&raw_mask, min_len, step).n_segs;
    eprintln!("[normalise] contour segments: {segs} (raw photo {raw_segs}), model {model:?}");
    assert_eq!(model, mapme_core::contour::default_model());
    assert!(segs > 1500 && segs > 3 * raw_segs, "{segs} vs {raw_segs}");

    // Grid lines: the photo detector finds the graticule (5 + 3 lines) and frame.
    let lines = mapme_core::grid::detect_lines_photo(&norm);
    let plain = detect_lines(&c.image);
    eprintln!("[normalise] grid lines: {} (raw photo, scan detector: {})", lines.len(), plain.len());
    assert!(lines.len() >= 8, "{lines:?}");
}

/// Debug aid: crops of the normalised photo, its contour mask and traced
/// segments as PNGs in `$PHOTO_DUMP` (default /tmp).
#[test]
#[ignore]
fn dump_photo_masks() {
    let c = case(SMALL);
    let dir = std::env::var("PHOTO_DUMP").unwrap_or_else(|_| "/tmp".into());
    let norm = normalize_illumination(&c.image);
    let (mask, _) = photo_mask(&norm, None);
    let (min_len, _) = mapme_core::skeleton::default_sampling(norm.w, norm.h);
    let segs = mapme_core::skeleton::contour_segments(&mask, min_len);
    let (x0, y0, cw, ch) = (900usize, 1000usize, 800usize, 800usize);
    let ow = cw * 3;
    let mut out = vec![0u8; ow * ch * 3];
    for y in 0..ch {
        for x in 0..cw {
            out[(y * ow + x) * 3..(y * ow + x) * 3 + 3].copy_from_slice(&norm.rgb(x0 + x, y0 + y));
            let m = if mask.get(x0 + x, y0 + y) { 255 } else { 0 };
            out[(y * ow + cw + x) * 3..(y * ow + cw + x) * 3 + 3].copy_from_slice(&[m, m, m]);
        }
    }
    for (si, s) in segs.iter().enumerate() {
        let col = [(si * 97 % 200 + 55) as u8, (si * 57 % 200 + 55) as u8, (si * 31 % 200 + 55) as u8];
        for &(x, y) in s {
            let (x, y) = (x as usize, y as usize);
            if (x0..x0 + cw).contains(&x) && (y0..y0 + ch).contains(&y) {
                let i = ((y - y0) * ow + 2 * cw + (x - x0)) * 3;
                out[i..i + 3].copy_from_slice(&col);
            }
        }
    }
    let f = std::fs::File::create(format!("{dir}/photo_mask.png")).unwrap();
    let mut enc = png::Encoder::new(std::io::BufWriter::new(f), ow as u32, ch as u32);
    enc.set_color(png::ColorType::Rgb);
    enc.set_depth(png::BitDepth::Eight);
    enc.write_header().unwrap().write_image_data(&out).unwrap();
    eprintln!("{} segments; wrote {dir}/photo_mask.png", segs.len());
}

/// Sweep over other cameras, seeds and corner-error patterns (prints the
/// accuracy of both paths; for tuning). `cargo test --release --test photo
/// photo_sweep -- --ignored --nocapture`
#[test]
#[ignore]
fn photo_sweep() {
    let map = &fixture().map;
    let cams = [(10.0, 3.0, -2.0), (20.0, -8.0, 5.0), (18.0, 10.0, 0.0), (12.0, -12.0, 8.0)];
    let only_cam: Option<usize> = std::env::var("SWEEP_CAM").ok().and_then(|v| v.parse().ok());
    let only_err: Option<f64> = std::env::var("SWEEP_ERR").ok().and_then(|v| v.parse().ok());
    for (k, &(tx, ty, roll)) in cams.iter().enumerate() {
        if only_cam.is_some_and(|c| c != k) {
            continue;
        }
        let cam = camera_homography(map.w, map.h, tx, ty, roll, 2900.0, 0.86);
        let photo = synth_photo(map, &cam, 100 + k as u64);
        for err in [SMALL, LARGE, OUTSIDE] {
            if only_err.is_some_and(|e| e != err) {
                continue;
            }
            let c = photo_case(map, &photo, &cam, err);
            let s = Session::new(c.image.clone());
            let (g, _, g_s) = run_loop(&s, &json!({"gps": gps(CENTER.0, CENTER.1), "ocr": c.ocr(), "photo": true}));
            let s = Session::new(c.image.clone());
            let (r, _, r_s) = run_loop(&s, &json!({"gps": gps(NE_12KM.0, NE_12KM.1), "photo": true}));
            let e = |r: &Value| if r["status"] == "ok" { c.max_error_m(&homography(r)) } else { f64::NAN };
            {
                // True perspective and anisotropy of the flattened image.
                let t = c.truth();
                let (cx, cy) = (c.image.w as f64 / 2.0, c.image.h as f64 / 2.0);
                let sn = 0.5 * (c.image.w as f64).hypot(c.image.h as f64);
                let q = t.m[6] * cx + t.m[7] * cy + t.m[8];
                let la = t.local_affine(cx, cy);
                let (a, b, cc, d) = (la.a, la.b, la.d, la.e);
                let s1 = a * a + b * b + cc * cc + d * d;
                let det = (a * d - b * cc).abs();
                let disc = (s1 * s1 - 4.0 * det * det).max(0.0).sqrt();
                let aniso = ((s1 + disc) / (s1 - disc)).sqrt();
                eprintln!(
                    "   truth: g {:.4} h {:.4} anisotropy {aniso:.3}; refined {}",
                    t.m[6] * sn / q,
                    t.m[7] * sn / q,
                    r["diagnostics"]["refined"]
                );
            }
            eprintln!(
                "camera {tx}/{ty}/{roll} corners {:+.1} % ({}x{}): grid {} {} {:.1} m ({:.0} ms); contours {} conf {} \
                 proj {} {:.1} m ({:.0} ms), peak {}",
                err * 100.0,
                c.image.w,
                c.image.h,
                g["method"],
                g["diagnostics"]["grid"]["model"],
                e(&g),
                g_s * 1e3,
                r["method"],
                r["confidence"],
                r["diagnostics"]["refined"]["projective"],
                e(&r),
                r_s * 1e3,
                r["diagnostics"]["candidates"][0]["peak_ratio"]
            );
        }
    }
}
