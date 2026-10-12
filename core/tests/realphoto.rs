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
//! 181 m max (`homography_floor_m`). Both paths therefore end in a warp for
//! the folds (`warp` in the response): the full model `pixel_to_mercator +
//! warp` is checked against the truth, and `pixel_to_mercator` alone against
//! the bounds a single homography reaches.
//!
//! The truth itself (a SIFT field smoothed over ~260 px) is off the printed
//! graticule by up to ~7 px (~30 m) in places, near the creases and the top
//! left of the sheet (`realphoto_traced_lines_follow_the_graticule` shows
//! it), which is part of the error measured near them.
//!
//! Run with `cargo test --release --test realphoto -- --nocapture`;
//! `realphoto_report` (ignored) prints the table of every path and quad, and
//! `realphoto_dump` (ignored) writes debug images to `$PHOTO_DUMP`.

mod common;
use common::fixture_dir;
use mapme_core::contour::{normalize_illumination, photo_mask};
use mapme_core::geo::{haversine_m, latlon_to_merc, merc_to_latlon, Homography, LatLon};
use mapme_core::georef::Session;
use mapme_core::grid::{detect_lines_photo, georeference_from_grid_with, GridLine, GridOptions, GridResult, LineAxis};
use mapme_core::image::Rgb8;
use mapme_core::labels::OcrObservation;
use mapme_core::warp::Warp;
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

/// A printed graticule line traced in the photo.
struct TruthLine {
    axis: LineAxis,
    /// Latitude (horizontal lines) or longitude (vertical lines).
    value: f64,
    points: Vec<Pt>,
}

struct RealPhoto {
    photo: Rgb8,
    page: [Pt; 4],
    cps: Vec<Cp>,
    ocr: Vec<PhotoOcr>,
    graticule: Vec<TruthLine>,
    floor: Value,
}

fn quad_of(v: &Value) -> [Pt; 4] {
    let a = v.as_array().expect("quad");
    [0, 1, 2, 3].map(|i| (a[i][0].as_f64().unwrap(), a[i][1].as_f64().unwrap()))
}

fn json_file(name: &str) -> Value {
    serde_json::from_slice(&std::fs::read(photo_dir().join(name)).unwrap()).unwrap()
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
        let truth = json_file("truth.json");
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
        let ocr = json_file("ocr.json")["observations"]
            .as_array()
            .unwrap()
            .iter()
            .map(|o| PhotoOcr {
                text: o["text"].as_str().unwrap().to_string(),
                quad: quad_of(&o["quad"]),
                confidence: o["confidence"].as_f64().unwrap_or(0.5) as f32,
            })
            .collect();
        let lines = |key: &str, value: &str, axis: LineAxis| -> Vec<TruthLine> {
            truth["graticule"][key]
                .as_array()
                .unwrap()
                .iter()
                .map(|l| TruthLine {
                    axis,
                    value: l[value].as_f64().unwrap(),
                    points: l["points"]
                        .as_array()
                        .unwrap()
                        .iter()
                        .map(|p| (p[0].as_f64().unwrap(), p[1].as_f64().unwrap()))
                        .collect(),
                })
                .collect()
        };
        let mut graticule = lines("lat_lines", "lat", LineAxis::Horizontal);
        graticule.extend(lines("lon_lines", "lon", LineAxis::Vertical));
        RealPhoto {
            page: quad_of(&truth["page_quad"]),
            cps,
            ocr,
            graticule,
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
    frac: f64,
    image: Rgb8,
    /// Photo pixel → flattened pixel.
    from_photo: Homography,
}

/// The photo flattened like a document scanner (CIPerspectiveCorrection)
/// by the page quad perturbed by `frac`: output size from the longer of
/// each pair of opposite edges.
fn flat(frac: f64) -> Flat {
    let r = real();
    let quad = perturbed(&r.page, frac, (r.photo.w as f64).hypot(r.photo.h as f64));
    let len = |a: Pt, b: Pt| (a.0 - b.0).hypot(a.1 - b.1);
    let w = len(quad[0], quad[1]).max(len(quad[3], quad[2])).round();
    let h = len(quad[0], quad[3]).max(len(quad[1], quad[2])).round();
    let src = [(0.0, 0.0), (w, 0.0), (w, h), (0.0, h)];
    let to_photo = Homography::fit(&src, &quad).expect("quad homography");
    let image = mapme_core::image::warp_perspective(&r.photo, &to_photo, w as usize, h as usize, [0, 0, 0]);
    Flat { frac, image, from_photo: to_photo.inverse().expect("inverse") }
}

impl Flat {
    /// The photo OCR moved into the flattened image (bounding box of each
    /// transformed quad).
    fn ocr(&self) -> Vec<OcrObservation> {
        real()
            .ocr
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
    fn errors(&self, model: &dyn Fn(f64, f64) -> Pt) -> Vec<f64> {
        real()
            .cps
            .iter()
            .map(|c| {
                let (u, v) = self.from_photo.apply(c.x, c.y);
                let (mx, my) = model(u, v);
                let (lat, lon) = merc_to_latlon(mx, my);
                haversine_m(LatLon::new(lat, lon), LatLon::new(c.lat, c.lon))
            })
            .collect()
    }

    fn label(&self) -> String {
        if self.frac == 0.0 {
            "page quad".into()
        } else {
            format!("quad {} {:.1} %", if self.frac > 0.0 { "in" } else { "out" }, self.frac.abs() * 100.0)
        }
    }
}

#[derive(Clone, Copy, Debug)]
struct Stats {
    median: f64,
    p90: f64,
    max: f64,
}

fn stats(mut e: Vec<f64>) -> Stats {
    assert!(!e.is_empty());
    e.sort_by(|a, b| a.total_cmp(b));
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

/// The response's warp, if any.
fn warp_of(r: &Value) -> Option<Warp> {
    let w: Warp = serde_json::from_value(r.get("warp")?.clone()).ok()?;
    assert!(w.is_valid(), "invalid warp {}", r["warp"]);
    Some(w)
}

fn homography(r: &Value) -> Option<Homography> {
    let m: Vec<f64> = r["pixel_to_mercator"].as_array()?.iter().map(|v| v.as_f64().unwrap()).collect();
    Some(Homography { m: m.try_into().ok()? })
}

/// Quad perturbations exercised (fraction of the photo diagonal).
const PERTURBATIONS: [f64; 5] = [0.0, 0.005, -0.005, 0.01, -0.01];

/// A point inside the map (the GPS of a user standing on the sheet).
const ON_MAP: (f64, f64) = (-25.8405, 27.3920);
/// About 45 km west of the map: outside a 20 km search radius (the `Far`
/// path asks for 20 km), inside the default 50 km.
const FAR: (f64, f64) = (-25.8405, 26.9420);
/// Map centre.
const CENTER: (f64, f64) = (-25.8499, 27.4000);

fn gps(p: (f64, f64)) -> Value {
    json!({"lat": p.0, "lon": p.1, "accuracy_m": 10.0})
}

#[derive(Clone, Copy, PartialEq, Debug)]
enum Path {
    /// Grid labels (OCR) and lines, GPS at the map centre.
    Grid,
    /// Contours against the DEM, GPS on the map.
    Contours,
    /// Contours with a GPS fix far from the map.
    Far,
}

impl Path {
    fn request(self, f: &Flat) -> Value {
        match self {
            Path::Grid => json!({"gps": gps(CENTER), "ocr": f.ocr(), "photo": true}),
            Path::Contours => json!({"methods": ["contours"], "gps": gps(ON_MAP), "photo": true}),
            Path::Far => json!({"methods": ["contours"], "gps": gps(FAR), "photo": true, "search_radius_m": 20000}),
        }
    }
}

/// One georeference of a flattened photo, scored against the truth.
struct Outcome {
    path: Path,
    quad: String,
    r: Value,
    /// Ground errors of a located response: `pixel_to_mercator` alone and
    /// the full model (with the warp, when there is one).
    flat: Option<Stats>,
    stats: Option<Stats>,
    core_s: f64,
    rounds: usize,
}

impl Outcome {
    fn status(&self) -> &str {
        self.r["status"].as_str().unwrap_or("?")
    }

    fn confidence(&self) -> f64 {
        self.r["confidence"].as_f64().unwrap_or(0.0)
    }

    fn row(&self) -> String {
        let s = self.stats.unwrap_or(Stats { median: f64::NAN, p90: f64::NAN, max: f64::NAN });
        let model = if self.r["diagnostics"]["refined"]["projective"] == true
            || self.r["diagnostics"]["grid"]["model"] == "homography"
        {
            "homography"
        } else if self.status() == "ok" {
            "affine"
        } else {
            "-"
        };
        let f = self.flat.unwrap_or(s);
        format!(
            "{:8} {:15} {:6} {:9} {:10} conf {:4} | H {:5.1} / {:5.1} / {:5.1} | {} {:5.1} / {:5.1} / {:5.1} m | \
             {:5.0} ms (warp {:4.0}), {} calls",
            format!("{:?}", self.path).to_lowercase(),
            self.quad,
            self.status(),
            self.r["method"].as_str().unwrap_or("-"),
            model,
            self.r["confidence"].as_f64().map_or("-".into(), |c| format!("{c:.2}")),
            f.median,
            f.p90,
            f.max,
            if self.r["warp"].is_object() { "H+warp" } else { "no warp" },
            s.median,
            s.p90,
            s.max,
            self.core_s * 1e3,
            self.r["timings_ms"]["warp"].as_f64().unwrap_or(0.0),
            self.rounds,
        )
    }
}

fn run(path: Path, f: &Flat) -> Outcome {
    let t = Instant::now();
    let s = Session::new(f.image.clone());
    let t_new = t.elapsed().as_secs_f64();
    let (r, rounds, core_s) = run_loop(&s, &path.request(f));
    let h = (r["status"] == "ok").then(|| homography(&r)).flatten();
    let flat = h.map(|h| stats(f.errors(&|x, y| h.apply(x, y))));
    let warp = warp_of(&r);
    let full = h.map(|h| match &warp {
        Some(w) => stats(f.errors(&|x, y| w.apply(&h, x, y))),
        None => flat.unwrap(),
    });
    Outcome { path, quad: f.label(), r, flat, stats: full, core_s: core_s + t_new, rounds }
}

/// Every quad perturbation through `path`; prints the table and returns it.
fn run_all(path: Path, fracs: &[f64]) -> Vec<Outcome> {
    let _g = serial();
    let out: Vec<Outcome> = fracs.iter().map(|&frac| run(path, &flat(frac))).collect();
    for o in &out {
        eprintln!("[real photo] {}", o.row());
    }
    out
}

/// Check every outcome; all failures are reported together.
fn check(out: &[Outcome], ok: impl Fn(&Outcome) -> Result<(), String>) {
    let bad: Vec<String> =
        out.iter().filter_map(|o| ok(o).err().map(|e| format!("{}: {e}\n  {}", o.quad, o.row()))).collect();
    assert!(bad.is_empty(), "{}", bad.join("\n"));
}

/// A located response within `median` / `p90` / `max` metres.
fn located(o: &Outcome, method: &str, min_conf: f64, bounds: Stats) -> Result<(), String> {
    if o.status() != "ok" || o.r["method"] != method {
        return Err(format!("expected {method}: {}", o.r["message"]));
    }
    let s = o.stats.ok_or("no transform")?;
    if s.median > bounds.median || s.p90 > bounds.p90 || s.max > bounds.max {
        return Err(format!("errors {s:?} exceed {bounds:?}"));
    }
    if o.confidence() < min_conf {
        return Err(format!("confidence {} < {min_conf}", o.confidence()));
    }
    Ok(())
}

/// A warp from `source` in the response; `pixel_to_mercator` alone within
/// `flat` metres; the warp fitted in at most `WARP_MS`.
fn warped(o: &Outcome, source: &str, flat: Stats) -> Result<(), String> {
    let d = &o.r["diagnostics"]["warp"];
    if !o.r["warp"].is_object() || d["accepted"] != true || d["source"] != source {
        return Err(format!("expected a warp from {source}: {d}"));
    }
    let f = o.flat.ok_or("no transform")?;
    if f.median > flat.median || f.p90 > flat.p90 || f.max > flat.max {
        return Err(format!("pixel_to_mercator alone {f:?} exceeds {flat:?}"));
    }
    let ms = o.r["timings_ms"]["warp"].as_f64().unwrap_or(f64::INFINITY);
    if ms > WARP_MS {
        return Err(format!("warp took {ms} ms"));
    }
    Ok(())
}

/// Time the warp fit may take (ms; the test profile is optimised like release).
const WARP_MS: f64 = 1500.0;

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
    let s = stats(f.errors(&|x, y| h.apply(x, y)));
    eprintln!("[real photo] best homography: {s:?}; fixture floor {}", r.floor);
    assert!((s.median - r.floor["median"].as_f64().unwrap()).abs() < 8.0, "{s:?}");
    assert!(s.max < 1.3 * r.floor["max"].as_f64().unwrap(), "{s:?}");
    assert_eq!(r.cps.len(), 670);
    assert_eq!(r.graticule.len(), 8);
}

// ---------------------------------------------------------------------------
// Grid path
// ---------------------------------------------------------------------------

/// Grid labels + lines: a homography through the 15 graticule intersections
/// (5 latitude × 3 longitude lines), at the single-homography floor
/// (~32 m median, ~90 m p90, ~295 m max), and a warp fitted to the traced
/// graticule, the map frame and the contours that brings the full model to
/// ~12 m median, ~50 m p90, ~145 m max.
#[test]
fn realphoto_grid() {
    let out = run_all(Path::Grid, &PERTURBATIONS);
    check(&out, |o| {
        located(o, "grid", 0.85, Stats { median: 15.0, p90: 60.0, max: 170.0 })?;
        warped(o, "grid", Stats { median: 35.0, p90: 95.0, max: 310.0 })?;
        let g = &o.r["diagnostics"]["grid"];
        if g["model"] != "homography" || g["n_gcps"].as_u64() < Some(12) {
            return Err(format!("grid fit {g}"));
        }
        let w = &o.r["diagnostics"]["warp"];
        if w["n_lines"] != 8 || w["n_straight_lines"].as_u64() < Some(2) || w["n_segments"].as_u64() < Some(500) {
            return Err(format!("warp data {w}"));
        }
        Ok(())
    });
}

/// The grid fit of the flattened photo, as the session computes it.
fn grid_fit(f: &Flat) -> (Vec<GridLine>, Option<GridResult>) {
    let lines = detect_lines_photo(&normalize_illumination(&f.image));
    let opts = GridOptions { photo: true };
    let g = georeference_from_grid_with(&f.image, &lines, &f.ocr(), Some(LatLon::new(CENTER.0, CENTER.1)), &opts);
    (lines, g)
}

/// (Position along the line, distance) from the printed graticule line to
/// an identified line at the truth polyline's points (flattened pixels).
fn line_offsets(f: &Flat, truth: &TruthLine, line: &GridLine) -> Vec<(f64, f64)> {
    truth
        .points
        .iter()
        .map(|&(x, y)| {
            let (u, v) = f.from_photo.apply(x, y);
            match truth.axis {
                LineAxis::Horizontal => (u, (line.pos_at(u) - v).abs()),
                LineAxis::Vertical => (v, (line.pos_at(v) - u).abs()),
            }
        })
        .collect()
}

/// The grid fit identifies all 8 printed graticule lines with their right
/// values and follows them along the folded paper: bent by up to ~14 px,
/// within ~1 px of the printed line (median) and a few pixels where a fold
/// creases it sharply.
#[test]
fn realphoto_grid_lines_follow_the_graticule() {
    let _g = serial();
    let f = flat(0.0);
    let (_, g) = grid_fit(&f);
    let g = g.expect("grid fit");
    assert_eq!(g.utm_zone, None);
    assert_eq!(g.lines.len(), 8, "{:?}", g.lines.iter().map(|l| l.value).collect::<Vec<_>>());
    let mut bad = Vec::new();
    for l in &g.lines {
        let t = real()
            .graticule
            .iter()
            .find(|t| t.axis == l.line.axis && (t.value - l.value).abs() < 1e-6)
            .unwrap_or_else(|| panic!("{:?} line at {} is not on the graticule", l.line.axis, l.value));
        let off = line_offsets(&f, t, &l.line);
        let s = stats(off.iter().map(|o| o.1).collect());
        let bend = l.line.bend.map_or(0.0, |b| b.max_abs());
        let worst = off.iter().fold((0.0, 0.0), |w, &o| if o.1 > w.1 { o } else { w });
        eprintln!(
            "[real photo] {:?} {:.4}: labelled {}, bend {bend:4.1} px, seen {:?}, off the printed line by median \
             {:.1} p90 {:.1} max {:.1} px (at {:.0})",
            l.line.axis,
            l.value,
            l.labelled,
            l.line.bend.map(|b| (b.lo.round(), b.hi.round())),
            s.median,
            s.p90,
            s.max,
            worst.0
        );
        if !(s.median < 2.0 && s.p90 < 10.0) {
            bad.push(format!("{:?} {}: {s:?}", l.line.axis, l.value));
        }
    }
    assert!(bad.is_empty(), "{bad:?}");
    let max_bend = g.lines.iter().filter_map(|l| l.line.bend).map(|b| b.max_abs()).fold(0.0, f64::max);
    assert!(max_bend > 5.0, "the folded sheet's grid lines should bend: {max_bend}");
}

/// Distance (pixels) of a point to a printed graticule line (the truth
/// polyline in flattened pixels, interpolated along the line).
fn truth_offset(f: &Flat, t: &TruthLine, (x, y): Pt) -> Option<f64> {
    let mut pts: Vec<Pt> = t.points.iter().map(|&(px, py)| f.from_photo.apply(px, py)).collect();
    let (s, p) = match t.axis {
        LineAxis::Horizontal => (x, y),
        LineAxis::Vertical => {
            pts.iter_mut().for_each(|q| *q = (q.1, q.0));
            (y, x)
        }
    };
    pts.sort_by(|a, b| a.0.total_cmp(&b.0));
    let k = pts.partition_point(|q| q.0 < s);
    if k == 0 || k == pts.len() {
        return None;
    }
    let (a, b) = (pts[k - 1], pts[k]);
    let f = (s - a.0) / (b.0 - a.0);
    Some((a.1 + f * (b.1 - a.1) - p).abs())
}

/// The warp's line tracer follows the 8 printed graticule lines along most
/// of their length, through the fold creases, within a pixel of the truth's
/// graticule (median).
#[test]
fn realphoto_traced_lines_follow_the_graticule() {
    let _g = serial();
    let f = flat(0.0);
    let (_, g) = grid_fit(&f);
    let g = g.expect("grid fit");
    let norm = normalize_illumination(&f.image);
    let mut bad = Vec::new();
    for l in &g.lines {
        let t = real().graticule.iter().find(|t| t.axis == l.line.axis && (t.value - l.value).abs() < 1e-6).unwrap();
        let pts = mapme_core::warp::trace_printed_line(&norm, &l.line);
        let off: Vec<f64> = pts.iter().filter_map(|&p| truth_offset(&f, t, p)).collect();
        let s = stats(off.clone());
        let extent = if l.line.axis == LineAxis::Horizontal { f.image.w } else { f.image.h } as f64;
        let cover = pts.len() as f64 * 8.0 / extent;
        eprintln!(
            "[real photo] traced {:?} {:.4}: {} points (cover {cover:.2}), off the printed line by median {:.2} p90 {:.2} \
             max {:.1} px",
            l.line.axis,
            l.value,
            pts.len(),
            s.median,
            s.p90,
            s.max
        );
        // The truth's graticule strays from the printed line by up to ~7 px
        // in places (checked by eye on the photo), hence the loose p90.
        if !(s.median < 1.0 && s.p90 < 8.0 && cover > 0.7) {
            bad.push(format!("{:?} {}: {s:?}, cover {cover:.2}", l.line.axis, l.value));
        }
    }
    assert!(bad.is_empty(), "{bad:?}");
}

// ---------------------------------------------------------------------------
// Contour path
// ---------------------------------------------------------------------------

/// Contours against the DEM from a GPS fix on the map: the coarse search
/// finds the sheet, the refinement ends in a homography at the floor
/// (~36–40 m median, ~87–98 m p90, ~290–350 m max), and a warp fitted to the
/// contours and the sheet's straight printed lines brings the full model to
/// ~15 m median, ~60–67 m p90, ~145–157 m max.
#[test]
fn realphoto_contours() {
    let out = run_all(Path::Contours, &PERTURBATIONS);
    check(&out, |o| {
        located(o, "contours", 0.7, Stats { median: 20.0, p90: 75.0, max: 180.0 })?;
        warped(o, "contours", Stats { median: 45.0, p90: 105.0, max: 360.0 })?;
        if o.r["diagnostics"]["refined"]["projective"] != true {
            return Err(format!("not projective: {}", o.r["diagnostics"]["refined"]));
        }
        Ok(())
    });
}

/// A GPS fix 45 km away with a 20 km search radius: the map is outside the
/// search area, so no confident answer.
#[test]
fn realphoto_far_gps_fails_honestly() {
    let out = run_all(Path::Far, &[0.0, -0.01]);
    check(&out, |o| match o.status() {
        "failed" => Ok(()),
        _ if o.confidence() < 0.5 => Ok(()),
        s => Err(format!("{s} with confidence {}", o.confidence())),
    });
}

// ---------------------------------------------------------------------------
// Report and debug images
// ---------------------------------------------------------------------------

/// Every path × quad perturbation as one table (`REAL_PATHS=grid,contours,far`
/// picks paths, `REAL_VERBOSE=1` adds each response's message and diagnostics).
#[test]
#[ignore]
fn realphoto_report() {
    let paths = std::env::var("REAL_PATHS").unwrap_or_else(|_| "grid,contours,far".into());
    let verbose = std::env::var("REAL_VERBOSE").is_ok();
    let fracs: Vec<f64> = std::env::var("REAL_FRACS")
        .map(|v| v.split(',').filter_map(|f| f.parse().ok()).collect())
        .unwrap_or_else(|_| PERTURBATIONS.to_vec());
    eprintln!("[real photo] flat-model floor (best single homography): {}", real().floor);
    for path in [Path::Grid, Path::Contours, Path::Far] {
        if !paths.contains(&format!("{path:?}").to_lowercase()) {
            continue;
        }
        for o in run_all(path, &fracs).iter().filter(|_| verbose) {
            eprintln!("{}\n    {}\n    {}\n    {}", o.row(), o.r["message"], o.r["timings_ms"], o.r["diagnostics"]);
        }
    }
}

fn write_png(path: &str, img: &Rgb8) {
    let f = std::fs::File::create(path).unwrap();
    let mut enc = png::Encoder::new(std::io::BufWriter::new(f), img.w as u32, img.h as u32);
    enc.set_color(png::ColorType::Rgb);
    enc.set_depth(png::BitDepth::Eight);
    enc.write_header().unwrap().write_image_data(&img.data).unwrap();
}

/// Debug images of the flattened photo (`REAL_FRAC`, default 0) in
/// `$PHOTO_DUMP` (default the temp dir): the normalised image, the contour
/// mask, its traced segments and the identified grid lines (red, others
/// blue).
#[test]
#[ignore]
fn realphoto_dump() {
    let frac: f64 = std::env::var("REAL_FRAC").ok().and_then(|v| v.parse().ok()).unwrap_or(0.0);
    let dir = std::env::var("PHOTO_DUMP").map(PathBuf::from).unwrap_or_else(|_| std::env::temp_dir());
    let out = |name: &str| dir.join(name).to_string_lossy().to_string();
    let f = flat(frac);
    let norm = normalize_illumination(&f.image);
    write_png(&out("real_norm.png"), &norm);
    let (mask, model) = photo_mask(&norm, None);
    let blank = || Rgb8 { w: norm.w, h: norm.h, data: vec![255; norm.w * norm.h * 3] };
    let mut m = blank();
    for (i, _) in mask.data.iter().enumerate().filter(|(_, &v)| v != 0) {
        m.data[i * 3..i * 3 + 3].fill(0);
    }
    write_png(&out("real_mask.png"), &m);
    let (min_len, _) = mapme_core::skeleton::default_sampling(norm.w, norm.h);
    let segs = mapme_core::skeleton::contour_segments(&mask, min_len);
    let mut sk = blank();
    for (si, s) in segs.iter().enumerate() {
        let col = [(si * 97 % 200) as u8, (si * 57 % 200) as u8, (si * 31 % 200) as u8];
        for &(x, y) in s {
            let i = (y as usize * sk.w + x as usize) * 3;
            sk.data[i..i + 3].copy_from_slice(&col);
        }
    }
    write_png(&out("real_segments.png"), &sk);
    eprintln!("model {model:?}, mask {:.4}, {} segments (min_len {min_len})", mask.fraction(), segs.len());
    let (lines, g) = grid_fit(&f);
    let valued: Vec<GridLine> = g.map(|g| g.lines.iter().map(|l| l.line).collect()).unwrap_or_default();
    let mut li = norm.clone();
    for l in &lines {
        let mid = if l.axis == LineAxis::Horizontal { li.w as f64 / 2.0 } else { li.h as f64 / 2.0 };
        eprintln!(
            "{:?} at {:.0}: coverage {:.2}, seen {:?}, bend {:.1} px{}",
            l.axis,
            l.pos_at(mid),
            l.coverage,
            l.bend.map(|b| (b.lo.round(), b.hi.round())),
            l.bend.map_or(0.0, |b| b.max_abs()),
            if valued.contains(l) { ", identified" } else { "" }
        );
        let col = if valued.contains(l) { [255, 0, 0] } else { [0, 0, 255] };
        let n = if l.axis == LineAxis::Horizontal { li.w } else { li.h };
        for s in 0..n {
            let (x, y) = l.point_at(s as f64);
            if x >= 0.0 && y >= 0.0 && (x as usize) < li.w && (y as usize) < li.h {
                let i = (y as usize * li.w + x as usize) * 3;
                li.data[i..i + 3].copy_from_slice(&col);
            }
        }
    }
    write_png(&out("real_lines.png"), &li);
    eprintln!("{} lines, {} identified; images in {}", lines.len(), valued.len(), dir.display());
}
