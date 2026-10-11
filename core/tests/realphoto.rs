//! Photo mode on the real phone photo of the printed Grootkloof sheet
//! (`fixtures/grootkloof/photo`): 1932×2576, the paper lying on a brown
//! wooden table, folded in four (creases near x = 938 and y = 1278, so it is
//! not flat), faint pinkish-brown contours, perspective, a white margin
//! around the map frame.
//!
//! The photo is flattened the way the app does it, by the page corners
//! (`truth.json` `page_quad`), exactly and with every corner moved by 0.5 %
//! and 1 % of the photo diagonal inwards and outwards. Requests go through
//! the JSON API with `"photo": true`, feeding the fixture DEM tiles on
//! `need_dem` like the app.
//!
//! Error metric: the result is composed back to photo pixels (flattened
//! pixel → photo pixel through the rectification homography) and evaluated
//! at every ground-truth control point (670 of them, inside the map frame);
//! the ground distance (haversine) to the truth's lat/lon gives the median,
//! 90th percentile and maximum. No single homography fits the folded sheet:
//! the best one over the control points is off by 37 m median, 66 m p90 and
//! 181 m max (`homography_floor_m`).
//!
//! Run with `cargo test --release --test realphoto -- --nocapture`;
//! `realphoto_report` (ignored) prints the whole table and
//! `dump_realphoto_debug` (ignored) writes debug images to `$PHOTO_DUMP`.

mod common;
use common::fixture_dir;
use mapme_core::contour::{normalize_illumination, photo_mask};
use mapme_core::geo::{haversine_m, latlon_to_merc, merc_to_latlon, Homography, LatLon};
use mapme_core::georef::Session;
use mapme_core::grid::{detect_lines_photo, GridLine, LineAxis};
use mapme_core::image::Rgb8;
use mapme_core::labels::OcrObservation;
use serde_json::{json, Value};
use std::path::PathBuf;
use std::sync::{Mutex, OnceLock};
use std::time::Instant;

/// Heavy tests run one at a time so timings are meaningful.
fn serial() -> std::sync::MutexGuard<'static, ()> {
    static M: Mutex<()> = Mutex::new(());
    M.lock().unwrap_or_else(|e| e.into_inner())
}

type Pt = (f64, f64);

fn photo_dir() -> PathBuf {
    fixture_dir().join("photo")
}

/// One ground-truth control point: photo pixel → lat/lon.
#[derive(Clone, Copy)]
struct Cp {
    x: f64,
    y: f64,
    lat: f64,
    lon: f64,
}

/// An OCR observation in photo pixels (its box as a quad TL, TR, BR, BL).
struct PhotoOcr {
    text: String,
    quad: [Pt; 4],
    confidence: f32,
}

struct RealPhoto {
    photo: Rgb8,
    page: [Pt; 4],
    frame: [Pt; 4],
    cps: Vec<Cp>,
    ocr: Vec<PhotoOcr>,
    floor: Value,
}

fn quad_of(v: &Value) -> [Pt; 4] {
    let a = v.as_array().expect("quad");
    [0, 1, 2, 3].map(|i| (a[i][0].as_f64().unwrap(), a[i][1].as_f64().unwrap()))
}

fn real() -> &'static RealPhoto {
    static R: OnceLock<RealPhoto> = OnceLock::new();
    R.get_or_init(|| {
        let bytes = std::fs::read(photo_dir().join("printed-map-photo.jpg")).expect("printed-map-photo.jpg");
        let mut dec = jpeg_decoder::Decoder::new(std::io::Cursor::new(bytes));
        let px = dec.decode().expect("decode jpeg");
        let info = dec.info().unwrap();
        assert_eq!(info.pixel_format, jpeg_decoder::PixelFormat::RGB24);
        let photo = Rgb8::from_rgb(info.width as usize, info.height as usize, px).unwrap();
        let truth: Value = serde_json::from_slice(&std::fs::read(photo_dir().join("truth.json")).unwrap()).unwrap();
        let cps = truth["control_points"]
            .as_array()
            .unwrap()
            .iter()
            .map(|c| Cp {
                x: c["x"].as_f64().unwrap(),
                y: c["y"].as_f64().unwrap(),
                lat: c["lat"].as_f64().unwrap(),
                lon: c["lon"].as_f64().unwrap(),
            })
            .collect();
        let ocr: Value = serde_json::from_slice(&std::fs::read(photo_dir().join("ocr.json")).unwrap()).unwrap();
        let ocr = ocr["observations"]
            .as_array()
            .unwrap()
            .iter()
            .map(|o| PhotoOcr {
                text: o["text"].as_str().unwrap().to_string(),
                quad: quad_of(&o["quad"]),
                confidence: o["confidence"].as_f64().unwrap_or(0.5) as f32,
            })
            .collect();
        RealPhoto {
            page: quad_of(&truth["page_quad"]),
            frame: quad_of(&truth["frame_quad"]),
            cps,
            ocr,
            floor: truth["homography_floor_m"].clone(),
            photo,
        }
    })
}

/// How the page corners were found: exact, or every corner moved by `frac`
/// of the photo diagonal, inwards (`frac` > 0: the flattened image crops
/// the page edge) or outwards (< 0: a band of table shows around the page),
/// in fixed directions that do not cancel.
fn perturbed(q: &[Pt; 4], frac: f64, diag: f64) -> [Pt; 4] {
    let d = frac * diag;
    let dirs = [(1.0, 0.4), (-0.6, 1.0), (-1.0, -0.3), (0.5, -1.0)];
    let mut out = *q;
    for (o, (dx, dy)) in out.iter_mut().zip(dirs) {
        let n = f64::hypot(dx, dy);
        o.0 += d * dx / n;
        o.1 += d * dy / n;
    }
    out
}

/// The photo flattened by a page quad.
struct Flat {
    image: Rgb8,
    /// Flattened pixel → photo pixel.
    to_photo: Homography,
    /// Photo pixel → flattened pixel.
    from_photo: Homography,
}

/// Flatten like a document scanner (CIPerspectiveCorrection): output size
/// from the longer of each pair of opposite edges.
fn flatten(photo: &Rgb8, quad: &[Pt; 4]) -> Flat {
    let len = |a: Pt, b: Pt| (a.0 - b.0).hypot(a.1 - b.1);
    let w = len(quad[0], quad[1]).max(len(quad[3], quad[2])).round();
    let h = len(quad[0], quad[3]).max(len(quad[1], quad[2])).round();
    let src = [(0.0, 0.0), (w, 0.0), (w, h), (0.0, h)];
    let to_photo = Homography::fit(&src, quad).expect("quad homography");
    let image = mapme_core::image::warp_perspective(photo, &to_photo, w as usize, h as usize, [0, 0, 0]);
    Flat { image, from_photo: to_photo.inverse().expect("inverse"), to_photo }
}

impl Flat {
    /// The photo OCR moved into the flattened image (bounding box of each
    /// transformed quad).
    fn ocr(&self, r: &RealPhoto) -> Vec<OcrObservation> {
        r.ocr
            .iter()
            .map(|o| {
                let pts = o.quad.map(|(x, y)| self.from_photo.apply(x, y));
                let x0 = pts.iter().map(|p| p.0).fold(f64::MAX, f64::min);
                let x1 = pts.iter().map(|p| p.0).fold(f64::MIN, f64::max);
                let y0 = pts.iter().map(|p| p.1).fold(f64::MAX, f64::min);
                let y1 = pts.iter().map(|p| p.1).fold(f64::MIN, f64::max);
                OcrObservation { text: o.text.clone(), bbox: [x0, y0, x1 - x0, y1 - y0], confidence: o.confidence }
            })
            .collect()
    }

    /// Ground errors (metres) of a flattened-pixel → Mercator model at every
    /// control point.
    fn errors(&self, r: &RealPhoto, model: &dyn Fn(f64, f64) -> Pt) -> Vec<f64> {
        r.cps
            .iter()
            .map(|c| {
                let (u, v) = self.from_photo.apply(c.x, c.y);
                let (mx, my) = model(u, v);
                let (lat, lon) = merc_to_latlon(mx, my);
                haversine_m(LatLon::new(lat, lon), LatLon::new(c.lat, c.lon))
            })
            .collect()
    }
}

#[derive(Clone, Copy, Debug)]
struct Stats {
    median: f64,
    p90: f64,
    max: f64,
}

fn stats(mut e: Vec<f64>) -> Stats {
    e.sort_by(|a, b| a.total_cmp(b));
    if e.is_empty() {
        return Stats { median: f64::NAN, p90: f64::NAN, max: f64::NAN };
    }
    let q = |p: f64| e[((e.len() - 1) as f64 * p).round() as usize];
    Stats { median: q(0.5), p90: q(0.9), max: *e.last().unwrap() }
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

fn homography(r: &Value) -> Option<Homography> {
    let m: Vec<f64> = r["pixel_to_mercator"].as_array()?.iter().map(|v| v.as_f64().unwrap()).collect();
    Some(Homography { m: m.try_into().ok()? })
}

/// A located response's ground errors at the control points.
fn response_stats(f: &Flat, r: &Value) -> Option<Stats> {
    let h = homography(r)?;
    Some(stats(f.errors(real(), &|x, y| h.apply(x, y))))
}

/// Quad perturbations exercised (fraction of the photo diagonal).
const PERTURBATIONS: [f64; 5] = [0.0, 0.005, -0.005, 0.01, -0.01];

fn label(frac: f64) -> String {
    if frac == 0.0 {
        "page quad".into()
    } else {
        format!("quad {} {:.1} %", if frac > 0.0 { "in" } else { "out" }, frac.abs() * 100.0)
    }
}

/// A point inside the map (the GPS of a user standing on the sheet).
const ON_MAP: (f64, f64) = (-25.8405, 27.3920);
/// Map centre.
const CENTER: (f64, f64) = (-25.8499, 27.4000);

fn gps(p: (f64, f64)) -> Value {
    json!({"lat": p.0, "lon": p.1, "accuracy_m": 10.0})
}

struct Outcome {
    r: Value,
    stats: Option<Stats>,
    core_s: f64,
    rounds: usize,
}

fn run(f: &Flat, req: &Value) -> Outcome {
    let t = Instant::now();
    let s = Session::new(f.image.clone());
    let t_new = t.elapsed().as_secs_f64();
    let (r, rounds, core_s) = run_loop(&s, req);
    let stats = if r["status"] == "ok" { response_stats(f, &r) } else { None };
    Outcome { stats, r, core_s: core_s + t_new, rounds }
}

fn grid_request(f: &Flat) -> Value {
    json!({"gps": gps(CENTER), "ocr": f.ocr(real()), "photo": true})
}

fn contour_request(at: (f64, f64)) -> Value {
    json!({"methods": ["contours"], "gps": gps(at), "photo": true})
}

fn report(path: &str, frac: f64, o: &Outcome) {
    let s = o.stats.unwrap_or(Stats { median: f64::NAN, p90: f64::NAN, max: f64::NAN });
    eprintln!(
        "[real photo] {path:8} {:16} {:6} {:9} conf {:5} median {:6.1} p90 {:6.1} max {:6.1} m  {:5.0} ms ({} calls) \
         model {} proj {}",
        label(frac),
        o.r["status"].as_str().unwrap_or("?"),
        o.r["method"].as_str().unwrap_or("-"),
        o.r["confidence"].as_f64().map_or("-".into(), |c| format!("{c:.2}")),
        s.median,
        s.p90,
        s.max,
        o.core_s * 1e3,
        o.rounds,
        o.r["diagnostics"]["grid"]["model"],
        o.r["diagnostics"]["refined"]["projective"],
    );
}

fn flat(frac: f64) -> Flat {
    let r = real();
    let diag = (r.photo.w as f64).hypot(r.photo.h as f64);
    flatten(&r.photo, &perturbed(&r.page, frac, diag))
}

// ---------------------------------------------------------------------------
// Ground truth sanity
// ---------------------------------------------------------------------------

/// The best homography through the control points (in flattened pixels of
/// the truth quad) reproduces the fixture's floor: the metric is right.
#[test]
fn realphoto_metric_matches_homography_floor() {
    let r = real();
    let f = flat(0.0);
    let src: Vec<Pt> = r.cps.iter().map(|c| f.from_photo.apply(c.x, c.y)).collect();
    let dst: Vec<Pt> = r.cps.iter().map(|c| latlon_to_merc(c.lat, c.lon)).collect();
    let h = Homography::fit(&src, &dst).unwrap();
    let s = stats(f.errors(r, &|x, y| h.apply(x, y)));
    eprintln!("[real photo] best homography: {s:?}; fixture floor {}", r.floor);
    assert!((s.median - r.floor["median"].as_f64().unwrap()).abs() < 8.0, "{s:?}");
    assert!(s.max < 1.3 * r.floor["max"].as_f64().unwrap(), "{s:?}");
    assert_eq!(r.cps.len(), 670);
    assert!(r.frame.iter().all(|&(x, y)| x > 0.0 && y > 0.0));
}

// ---------------------------------------------------------------------------
// Report (all paths × quad perturbations)
// ---------------------------------------------------------------------------

#[test]
#[ignore]
fn realphoto_report() {
    let _g = serial();
    let only: Option<f64> = std::env::var("REAL_FRAC").ok().and_then(|v| v.parse().ok());
    let paths: Vec<String> =
        std::env::var("REAL_PATHS").map(|v| v.split(',').map(str::to_string).collect()).unwrap_or_else(|_| {
            vec!["grid".into(), "contours".into()]
        });
    for frac in PERTURBATIONS {
        if only.is_some_and(|o| o != frac) {
            continue;
        }
        let f = flat(frac);
        for p in &paths {
            let req = match p.as_str() {
                "grid" => grid_request(&f),
                "contours" => contour_request(ON_MAP),
                "far" => contour_request((ON_MAP.0, ON_MAP.1 - 0.45)),
                _ => continue,
            };
            let o = run(&f, &req);
            report(p, frac, &o);
            if let Ok(dir) = std::env::var("PHOTO_DUMP") {
                let m = f.to_photo.m;
                let out = json!({"response": o.r, "rect_to_photo": m, "w": f.image.w, "h": f.image.h});
                std::fs::write(format!("{dir}/resp_{p}_{frac}.json"), out.to_string()).unwrap();
            }
            if std::env::var("REAL_VERBOSE").is_ok() {
                eprintln!("    message: {}\n    timings: {}\n    diagnostics: {}", o.r["message"], o.r["timings_ms"], o.r["diagnostics"]);
                if p == "grid" {
                    print_gcps(&f);
                }
            }
        }
    }
}

/// The grid fit's GCPs against the truth (photo pixels of the graticule
/// intersections).
fn print_gcps(f: &Flat) {
    let r = real();
    let norm = normalize_illumination(&f.image);
    let lines = detect_lines_photo(&norm);
    let g = mapme_core::grid::georeference_from_grid_with(
        &f.image,
        &lines,
        &f.ocr(r),
        Some(LatLon::new(CENTER.0, CENTER.1)),
        &mapme_core::grid::GridOptions { photo: true },
    );
    for o in f.ocr(r) {
        if mapme_core::labels::parse_coordinate(&o.text).is_none() {
            continue;
        }
        let (cx, cy) = o.center();
        let near: Vec<String> = lines
            .iter()
            .map(|l| match l.axis {
                LineAxis::Horizontal => (l.pos_at(cx) - cy, "h"),
                LineAxis::Vertical => (l.pos_at(cy) - cx, "v"),
            })
            .filter(|d| d.0.abs() < 120.0)
            .map(|d| format!("{}{:+.0}", d.1, d.0))
            .collect();
        eprintln!("    label {:14} centre ({cx:6.0}, {cy:6.0}) size {:.0}x{:.0} lines {near:?}", o.text, o.bbox[2], o.bbox[3]);
    }
    let Some(g) = g else {
        eprintln!("    no grid fit");
        return;
    };
    let truth: Value = serde_json::from_slice(&std::fs::read(photo_dir().join("truth.json")).unwrap()).unwrap();
    let inter = truth["graticule"]["intersections"].as_array().unwrap();
    for p in &g.gcps {
        let t = inter.iter().find(|i| {
            (i["lat"].as_f64().unwrap() - p.lat).abs() < 1e-6 && (i["lon"].as_f64().unwrap() - p.lon).abs() < 1e-6
        });
        let d = t.map(|t| {
            let (x, y) = f.from_photo.apply(t["x"].as_f64().unwrap(), t["y"].as_f64().unwrap());
            (x - p.x, y - p.y)
        });
        eprintln!("    gcp ({:7.1}, {:7.1}) {:.5} {:.5}  vs truth {:?}", p.x, p.y, p.lat, p.lon, d.map(|d| (d.0.round(), d.1.round())));
    }
}

// ---------------------------------------------------------------------------
// Debug images
// ---------------------------------------------------------------------------

fn write_png(path: &str, w: usize, h: usize, rgb: &[u8]) {
    let f = std::fs::File::create(path).unwrap();
    let mut enc = png::Encoder::new(std::io::BufWriter::new(f), w as u32, h as u32);
    enc.set_color(png::ColorType::Rgb);
    enc.set_depth(png::BitDepth::Eight);
    enc.write_header().unwrap().write_image_data(rgb).unwrap();
}

fn draw_line(img: &mut Rgb8, l: &GridLine, col: [u8; 3]) {
    let (w, h) = (img.w, img.h);
    match l.axis {
        LineAxis::Horizontal => {
            for x in 0..w {
                let y = l.pos_at(x as f64).round();
                if y >= 0.0 && (y as usize) < h {
                    let i = (y as usize * w + x) * 3;
                    img.data[i..i + 3].copy_from_slice(&col);
                }
            }
        }
        LineAxis::Vertical => {
            for y in 0..h {
                let x = l.pos_at(y as f64).round();
                if x >= 0.0 && (x as usize) < w {
                    let i = (y * w + x as usize) * 3;
                    img.data[i..i + 3].copy_from_slice(&col);
                }
            }
        }
    }
}

/// Contour diagnostics: the contour samples (as `real_samples.json`, for
/// checking against the truth field offline) and the refinement cost of the
/// best homography over the control points vs the pipeline's result.
#[test]
#[ignore]
fn diag_realphoto_contours() {
    use mapme_core::georef::ContourProducts;
    use mapme_core::refine::{evaluate_prepared_homography, refine_dem_box, PreparedDem};
    let frac: f64 = std::env::var("REAL_FRAC").ok().and_then(|v| v.parse().ok()).unwrap_or(0.0);
    let dir = std::env::var("PHOTO_DUMP").unwrap_or_else(|_| "/tmp".into());
    let r = real();
    let f = flat(frac);
    let norm = normalize_illumination(&f.image);
    let t = Instant::now();
    let prod = ContourProducts::compute_with(&norm, None, true);
    eprintln!(
        "products {} ms: {} segments {} samples, mask {:.4}",
        t.elapsed().as_millis(),
        prod.samples.n_segs,
        prod.samples.len(),
        prod.mask_fraction
    );
    let js: Vec<[f32; 3]> = prod.samples.pts.iter().zip(&prod.samples.seg).map(|(p, &s)| [p[0], p[1], s as f32]).collect();
    std::fs::write(format!("{dir}/real_samples.json"), serde_json::to_string(&js).unwrap()).unwrap();
    let src: Vec<Pt> = r.cps.iter().map(|c| f.from_photo.apply(c.x, c.y)).collect();
    let dst: Vec<Pt> = r.cps.iter().map(|c| latlon_to_merc(c.lat, c.lon)).collect();
    let floor = Homography::fit(&src, &dst).unwrap();
    let (w, h) = (f.image.w, f.image.h);
    let bbox = refine_dem_box(&floor.local_affine(w as f64 / 2.0, h as f64 / 2.0), w, h);
    let store = common::dem_store();
    let dem = store.mosaic(13, &bbox);
    let prep = PreparedDem::new_cropped(&dem, &bbox);
    let show = |name: &str, hm: &Homography| {
        let e = evaluate_prepared_homography(&prod.samples, &prep, hm);
        let s = stats(f.errors(r, &|x, y| hm.apply(x, y)));
        eprintln!(
            "{name:10} cost {:.2} m, seg std {:.2}, inliers {:.3}, expl {:.4}, snr {:.2}; error median {:.1} p90 {:.1} max {:.1}",
            e.cost_m, e.median_seg_std_m, e.inlier_fraction, e.explained_variance, e.interval_snr, s.median, s.p90, s.max
        );
    };
    show("floor", &floor);
    let o = run(&f, &contour_request(ON_MAP));
    if let Some(hm) = homography(&o.r) {
        show("pipeline", &hm);
    }
    let o = run(&f, &grid_request(&f));
    if let Some(hm) = homography(&o.r) {
        show("grid", &hm);
    }
}

/// Debug aid: grid lines and the grid fit on the synthetic photo of
/// tests/photo.rs (corner error `SYNTH_ERR`, default 0.005).
#[test]
#[ignore]
fn diag_synthetic_grid() {
    let err: f64 = std::env::var("SYNTH_ERR").ok().and_then(|v| v.parse().ok()).unwrap_or(0.005);
    let map = common::load_map();
    let cam = common::photo_camera(&map);
    let photo = common::synth_photo(&map, &cam, 7);
    let c = common::photo_case(&map, &photo, &cam, err);
    let norm = normalize_illumination(&c.image);
    let t = Instant::now();
    let lines = detect_lines_photo(&norm);
    eprintln!("{}x{}: {} lines in {} ms", c.image.w, c.image.h, lines.len(), t.elapsed().as_millis());
    for l in &lines {
        let ext = if l.axis == LineAxis::Horizontal { norm.w as f64 } else { norm.h as f64 };
        eprintln!(
            "  {:?} pos(mid) {:.1} tilt {:.2} cov {:.2} bend {:.1} seen {:?}",
            l.axis,
            l.pos_at(ext / 2.0),
            l.tilt_deg(),
            l.coverage,
            l.bend.map_or(0.0, |b| b.max_abs()),
            l.bend.map(|b| (b.lo.round(), b.hi.round()))
        );
    }
    let g = mapme_core::grid::georeference_from_grid_with(
        &c.image,
        &lines,
        &c.ocr(),
        Some(LatLon::new(CENTER.0, CENTER.1)),
        &mapme_core::grid::GridOptions { photo: true },
    );
    match g {
        Some(g) => eprintln!(
            "grid {:?} rms {:.1} lat {} lon {} gcps {} max err {:.1} m",
            g.method,
            g.rms_m,
            g.n_lat,
            g.n_lon,
            g.gcps.len(),
            c.max_error_m(&g.transform)
        ),
        None => eprintln!("no grid fit"),
    }
    let (mask, _) = photo_mask(&norm, None);
    let (min_len, step) = mapme_core::skeleton::default_sampling(norm.w, norm.h);
    let segs = mapme_core::skeleton::contour_samples(&mask, min_len, step).n_segs;
    eprintln!("mask {:.4}, {segs} segments (min_len {min_len})", mask.fraction());
    if let Ok(dir) = std::env::var("PHOTO_DUMP") {
        let m: Vec<u8> = mask.data.iter().flat_map(|&v| if v != 0 { [0u8; 3] } else { [255u8; 3] }).collect();
        write_png(&format!("{dir}/synth_mask.png"), mask.w, mask.h, &m);
        write_png(&format!("{dir}/synth_norm.png"), norm.w, norm.h, &norm.data);
        let segs = mapme_core::skeleton::contour_segments(&mask, min_len);
        let mut sk = Rgb8 { w: norm.w, h: norm.h, data: vec![255; norm.w * norm.h * 3] };
        for (si, s) in segs.iter().enumerate() {
            let col = [(si * 97 % 200) as u8, (si * 57 % 200) as u8, (si * 31 % 200) as u8];
            for &(x, y) in s {
                let i = (y as usize * sk.w + x as usize) * 3;
                sk.data[i..i + 3].copy_from_slice(&col);
            }
        }
        write_png(&format!("{dir}/synth_segments.png"), sk.w, sk.h, &sk.data);
        let cm = mapme_core::contour::colour_line_mask(&norm, &mapme_core::contour::default_model());
        let cs = mapme_core::skeleton::contour_segments(&cm, min_len);
        eprintln!("colour mask {:.4}: {} segments", cm.fraction(), cs.len());
        let mut sk = Rgb8 { w: norm.w, h: norm.h, data: vec![255; norm.w * norm.h * 3] };
        for (si, s) in cs.iter().enumerate() {
            let col = [(si * 97 % 200) as u8, (si * 57 % 200) as u8, (si * 31 % 200) as u8];
            for &(x, y) in s {
                let i = (y as usize * sk.w + x as usize) * 3;
                sk.data[i..i + 3].copy_from_slice(&col);
            }
        }
        write_png(&format!("{dir}/synth_segments_colour.png"), sk.w, sk.h, &sk.data);
    }
}

/// Normalised photo, contour mask and detected grid lines of the flattened
/// photo as PNGs in `$PHOTO_DUMP` (default /tmp). `REAL_FRAC` picks the quad.
#[test]
#[ignore]
fn dump_realphoto_debug() {
    let frac: f64 = std::env::var("REAL_FRAC").ok().and_then(|v| v.parse().ok()).unwrap_or(0.0);
    let dir = std::env::var("PHOTO_DUMP").unwrap_or_else(|_| "/tmp".into());
    let f = flat(frac);
    write_png(&format!("{dir}/real_flat.png"), f.image.w, f.image.h, &f.image.data);
    let t = Instant::now();
    let norm = normalize_illumination(&f.image);
    eprintln!("normalise {} ms", t.elapsed().as_millis());
    write_png(&format!("{dir}/real_norm.png"), norm.w, norm.h, &norm.data);
    let t = Instant::now();
    let (mask, model) = photo_mask(&norm, None);
    eprintln!("mask {} ms, model {model:?}, fraction {:.4}", t.elapsed().as_millis(), mask.fraction());
    let m: Vec<u8> = mask.data.iter().flat_map(|&v| if v != 0 { [0u8; 3] } else { [255u8; 3] }).collect();
    write_png(&format!("{dir}/real_mask.png"), mask.w, mask.h, &m);
    let (min_len, step) = mapme_core::skeleton::default_sampling(norm.w, norm.h);
    let segs = mapme_core::skeleton::contour_segments(&mask, min_len);
    let samples = mapme_core::skeleton::contour_samples(&mask, min_len, step);
    eprintln!("{} segments, {} samples (min_len {min_len}, step {step})", segs.len(), samples.len());
    let mut sk = Rgb8 { w: norm.w, h: norm.h, data: vec![255; norm.w * norm.h * 3] };
    for (si, s) in segs.iter().enumerate() {
        let col = [(si * 97 % 200) as u8, (si * 57 % 200) as u8, (si * 31 % 200) as u8];
        for &(x, y) in s {
            let i = (y as usize * sk.w + x as usize) * 3;
            sk.data[i..i + 3].copy_from_slice(&col);
        }
    }
    write_png(&format!("{dir}/real_segments.png"), sk.w, sk.h, &sk.data);
    let t = Instant::now();
    let lines = detect_lines_photo(&norm);
    eprintln!("lines {} ms: {}", t.elapsed().as_millis(), lines.len());
    let mut li = norm.clone();
    let mut js = Vec::new();
    for l in &lines {
        let ext = if l.axis == LineAxis::Horizontal { norm.w as f64 } else { norm.h as f64 };
        eprintln!(
            "  {:?} pos(mid) {:.1} tilt {:.2} cov {:.2} bend {:.1} seen {:?}",
            l.axis,
            l.pos_at(ext / 2.0),
            l.tilt_deg(),
            l.coverage,
            l.bend.map_or(0.0, |b| b.max_abs()),
            l.bend.map(|b| (b.lo.round(), b.hi.round()))
        );
        draw_line(&mut li, l, [255, 0, 0]);
        let pts: Vec<Pt> = (0..(ext as usize)).step_by(10).map(|s| l.point_at(s as f64)).collect();
        js.push(json!({"axis": format!("{:?}", l.axis), "coverage": l.coverage, "points": pts,
                       "seen": l.bend.map(|b| [b.lo, b.hi])}));
    }
    write_png(&format!("{dir}/real_lines.png"), li.w, li.h, &li.data);
    std::fs::write(format!("{dir}/real_lines.json"), serde_json::to_string(&js).unwrap()).unwrap();
    for (name, flattened) in [("sharp", false), ("flat", true)] {
        let pts = mapme_core::grid::photo_scan_points(&norm, flattened);
        let mut img = Rgb8 { w: norm.w, h: norm.h, data: vec![255; norm.w * norm.h * 3] };
        for (x, y, fam) in pts {
            let (x, y) = (x.round() as usize, y.round() as usize);
            if x < img.w && y < img.h {
                let i = (y * img.w + x) * 3;
                let c = if fam == 0 { [200, 0, 0] } else { [0, 0, 200] };
                for k in 0..3 {
                    img.data[i + k] = img.data[i + k].min(c[k]);
                }
            }
        }
        write_png(&format!("{dir}/real_points_{name}.png"), img.w, img.h, &img.data);
    }
}
