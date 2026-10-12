//! End-to-end georeferencing of the real Grootkloof sheet through the C ABI
//! only, the way the iOS app drives it: create a session from RGBA pixels,
//! send request JSON, feed DEM tiles while the core answers `need_dem`.
//! Run with `cargo test --release`.

mod common;
use common::*;
use mapme_core::ffi::*;
use mapme_core::geo::Homography;
use serde_json::{json, Value};
use std::ffi::{CStr, CString};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

/// Heavy tests run one at a time so timings are meaningful.
fn serial() -> std::sync::MutexGuard<'static, ()> {
    static M: Mutex<()> = Mutex::new(());
    M.lock().unwrap_or_else(|e| e.into_inner())
}

/// The map as RGBA rows padded to a 64-byte multiple plus 32 garbage bytes
/// (as CoreGraphics bitmaps are), so `bytes_per_row` handling is exercised.
struct Raster {
    data: Vec<u8>,
    w: u32,
    h: u32,
    stride: u32,
}

fn padded(rgba: &[u8], w: usize, h: usize) -> Raster {
    let stride = (w * 4).div_ceil(64) * 64 + 32;
    let mut data = vec![0u8; stride * h];
    for y in 0..h {
        data[y * stride..y * stride + w * 4].copy_from_slice(&rgba[y * w * 4..(y + 1) * w * 4]);
        for (i, b) in data[y * stride + w * 4..(y + 1) * stride].iter_mut().enumerate() {
            *b = (i * 37 + y) as u8;
        }
    }
    Raster { data, w: w as u32, h: h as u32, stride: stride as u32 }
}

fn map_raster() -> &'static Raster {
    static R: OnceLock<Raster> = OnceLock::new();
    R.get_or_init(|| {
        let (rgba, w, h) = load_map_rgba();
        padded(&rgba, w, h)
    })
}

/// Owned session handle; `Sync` so another thread can poll progress / cancel.
struct Sess(*mut MMSession);
unsafe impl Send for Sess {}
unsafe impl Sync for Sess {}

impl Sess {
    fn new(r: &Raster) -> Sess {
        let p = unsafe { mm_session_new(r.data.as_ptr(), r.w, r.h, r.stride) };
        assert!(!p.is_null());
        Sess(p)
    }

    fn georef(&self, req: &Value) -> Value {
        let c = CString::new(req.to_string()).unwrap();
        let out = unsafe { mm_session_georeference(self.0, c.as_ptr()) };
        assert!(!out.is_null());
        let s = unsafe { CStr::from_ptr(out) }.to_str().unwrap().to_owned();
        unsafe { mm_string_free(out) };
        serde_json::from_str(&s).unwrap()
    }

    fn progress(&self) -> f32 {
        unsafe { mm_session_progress(self.0) }
    }

    fn cancel(&self) {
        unsafe { mm_session_cancel(self.0) }
    }

    /// Add the tile from the fixture if there, else mark it missing (offline).
    fn supply(&self, t: &Value) {
        let (z, x, y) = (t["z"].as_u64().unwrap(), t["x"].as_u64().unwrap(), t["y"].as_u64().unwrap());
        let path = fixture_dir().join("dem").join(format!("{z}_{x}_{y}.png"));
        let rc = match std::fs::read(&path) {
            Ok(png) => unsafe { mm_session_add_dem_tile(self.0, z as u8, x as u32, y as u32, png.as_ptr(), png.len()) },
            Err(_) => unsafe { mm_session_mark_dem_tile_missing(self.0, z as u8, x as u32, y as u32) },
        };
        assert_eq!(rc, MM_OK, "tile {z}/{x}/{y}");
    }

    /// Every fixture tile (no need_dem rounds).
    fn supply_all(&self) {
        for (t, png) in dem_tiles() {
            let rc = unsafe { mm_session_add_dem_tile(self.0, t.z, t.x, t.y, png.as_ptr(), png.len()) };
            assert_eq!(rc, MM_OK);
        }
    }
}

impl Drop for Sess {
    fn drop(&mut self) {
        unsafe { mm_session_free(self.0) }
    }
}

/// What the app does: call, add the requested tiles, call again.
struct Loop {
    responses: Vec<Value>,
    /// Core time over all calls (excluding tile loading), seconds.
    core_s: f64,
}

impl Loop {
    fn last(&self) -> &Value {
        self.responses.last().unwrap()
    }
}

fn run_dem_loop(s: &Sess, req: &Value, max_rounds: usize) -> Loop {
    let mut responses = Vec::new();
    let mut core_s = 0.0;
    for _ in 0..max_rounds {
        let t = Instant::now();
        let r = s.georef(req);
        core_s += t.elapsed().as_secs_f64();
        let need = r["status"] == "need_dem";
        if need {
            let tiles = r["need_tiles"].as_array().unwrap();
            assert!(!tiles.is_empty());
            for t in tiles {
                s.supply(t);
            }
        }
        responses.push(r);
        if !need {
            return Loop { responses, core_s };
        }
    }
    panic!("still need_dem after {max_rounds} rounds: {}", responses.last().unwrap());
}

fn homography(r: &Value) -> Homography {
    let m: Vec<f64> = r["pixel_to_mercator"].as_array().unwrap().iter().map(|v| v.as_f64().unwrap()).collect();
    Homography { m: m.try_into().unwrap() }
}

fn ocr_json() -> Value {
    let v: Value = serde_json::from_slice(&std::fs::read(fixture_dir().join("ocr.json")).unwrap()).unwrap();
    v["observations"].clone()
}

fn gps(lat: f64, lon: f64) -> Value {
    json!({"lat": lat, "lon": lon, "accuracy_m": 8.0})
}

/// Mid-sheet GPS offsets used by the contour tests.
const NE_12KM: (f64, f64) = (CENTER.0 + 0.0764, CENTER.1 + 0.0846);
const WEST_55KM: (f64, f64) = (CENTER.0, CENTER.1 - 0.55);

fn summary(name: &str, r: &Value, err: Option<f64>, l: &Loop) {
    eprintln!(
        "[{name}] {} / {} conf {} rms {} scale {} rot {} err {} rounds {} core {:.0} ms\n    message: {}\n    timings: {}\n    \
         diagnostics: {}",
        r["status"],
        r["method"],
        r["confidence"],
        r["rms_m"],
        r["scale_m_per_px"],
        r["rotation_deg"],
        err.map_or("-".into(), |e| format!("{e:.1} m")),
        l.responses.len(),
        l.core_s * 1e3,
        r["message"].as_str().unwrap_or(""),
        r["timings_ms"],
        r["diagnostics"]
    );
}

/// (a) Grid labels: one call, no DEM needed.
#[test]
fn grid_labels_locate_the_sheet() {
    let _g = serial();
    let t = Instant::now();
    let s = Sess::new(map_raster());
    let t_new = t.elapsed().as_secs_f64();
    let req = json!({"gps": gps(CENTER.0, CENTER.1), "ocr": ocr_json(), "dpi": 300.0});
    let l = run_dem_loop(&s, &req, 1);
    let r = l.last();
    let err = max_error_m(&homography(r));
    summary("grid", r, Some(err), &l);
    eprintln!(
        "[timing] grid path: session {:.0} ms + georeference {:.0} ms = {:.0} ms",
        t_new * 1e3,
        l.core_s * 1e3,
        (t_new + l.core_s) * 1e3
    );
    assert_eq!(r["status"], "ok");
    assert_eq!(r["method"], "grid");
    assert!(err < 8.0, "max error {err:.1} m");
    assert!(r["confidence"].as_f64().unwrap() >= 0.9);
    let msg = r["message"].as_str().unwrap();
    assert!(msg.contains("grid label"), "{msg}");
    assert!(msg.contains("latitude") && msg.contains("longitude"), "{msg}");
    let scale = r["scale_m_per_px"].as_f64().unwrap();
    assert!((scale - 1.906).abs() < 0.01, "scale {scale}");
    let rot = r["rotation_deg"].as_f64().unwrap();
    assert!(rot.min(360.0 - rot) < 0.1, "rotation {rot}");
    let rms = r["rms_m"].as_f64().unwrap();
    assert!((0.5..8.0).contains(&rms), "rms {rms}");
    // Corners: TL is north-west of BR, about 9.5 km apart north-south.
    let c = r["corners"].as_array().unwrap();
    let (tl, br) = (&c[0], &c[2]);
    assert!(tl[0].as_f64().unwrap() > br[0].as_f64().unwrap() && tl[1].as_f64().unwrap() < br[1].as_f64().unwrap());
    let pois = r["pois"].as_array().unwrap();
    assert!(pois.iter().any(|p| p["name"] == "KWAMI LODGE" && p["kind"] == "place"), "{pois:?}");
    assert!(pois.iter().any(|p| p["kind"] == "spot_height" && p["elevation_m"].as_f64() == Some(1523.0)), "{pois:?}");
    // Coordinate labels are not POIs.
    assert!(!pois.iter().any(|p| p["name"].as_str().unwrap().contains('°')));
    assert!(r["timings_ms"]["total"].as_f64().unwrap() < 3000.0);
}

/// (a') Grid + `verify`: the contour check needs zoom-13 tiles, then agrees.
#[test]
fn grid_result_verified_against_terrain() {
    let _g = serial();
    let s = Sess::new(map_raster());
    let req = json!({"gps": gps(CENTER.0, CENTER.1), "ocr": ocr_json(), "dpi": 300.0, "verify": true});
    let l = run_dem_loop(&s, &req, 3);
    let first = &l.responses[0];
    assert_eq!(first["status"], "need_dem");
    assert!(first["need_tiles"].as_array().unwrap().iter().all(|t| t["z"] == 13));
    let r = l.last();
    summary("grid+verify", r, Some(max_error_m(&homography(r))), &l);
    assert_eq!(r["status"], "ok");
    assert_eq!(r["method"], "grid");
    let check = &r["diagnostics"]["contour_check"];
    assert_eq!(check["agrees"], true, "{check}");
    assert!(check["median_seg_std_m"].as_f64().unwrap() < 4.0, "{check}");
    assert!(check["inlier_fraction"].as_f64().unwrap() > 0.6, "{check}");
}

/// (b) Contours only (no OCR), GPS 12 km north-east of the sheet centre.
#[test]
fn contours_locate_the_sheet_from_12km_away() {
    let _g = serial();
    let t = Instant::now();
    let s = Sess::new(map_raster());
    let t_new = t.elapsed().as_secs_f64();
    let req = json!({"gps": gps(NE_12KM.0, NE_12KM.1), "dpi": 300.0});
    let l = run_dem_loop(&s, &req, 5);
    let first = &l.responses[0];
    assert_eq!(first["status"], "need_dem");
    let tiles = first["need_tiles"].as_array().unwrap();
    assert!(tiles.iter().all(|t| t["z"] == 11), "{first}");
    let r = l.last();
    let err = max_error_m(&homography(r));
    summary("contours 12km NE", r, Some(err), &l);
    for (i, x) in l.responses.iter().enumerate() {
        eprintln!(
            "    round {i}: {} {} tiles, {}",
            x["status"],
            x["need_tiles"].as_array().map_or(0, |v| v.len()),
            x["timings_ms"]
        );
    }
    eprintln!(
        "[timing] contour path: session {:.0} ms + {} calls {:.0} ms = {:.0} ms (excluding DEM download)",
        t_new * 1e3,
        l.responses.len(),
        l.core_s * 1e3,
        (t_new + l.core_s) * 1e3
    );
    assert_eq!(r["status"], "ok", "{r}");
    assert_eq!(r["method"], "contours");
    assert!(err < 40.0, "max error {err:.1} m");
    assert!(r["confidence"].as_f64().unwrap() >= 0.7);
    let rms = r["rms_m"].as_f64().unwrap();
    assert!((10.0..60.0).contains(&rms), "rms {rms}");
    assert!(r["message"].as_str().unwrap().contains("contour"));
    assert!((r["scale_m_per_px"].as_f64().unwrap() - 1.906).abs() < 0.03);
    assert_eq!(r["diagnostics"]["refined"]["dem_zoom"], 13);
}

/// (b') OCR'd "1:25 000" + dpi narrows the scale search; grid not requested.
#[test]
fn contours_with_scale_prior() {
    let _g = serial();
    let s = Sess::new(map_raster());
    let req = json!({"gps": gps(CENTER.0, CENTER.1), "ocr": ocr_json(), "dpi": 300.0, "methods": ["contours"]});
    let l = run_dem_loop(&s, &req, 5);
    let r = l.last();
    let err = max_error_m(&homography(r));
    summary("contours+prior", r, Some(err), &l);
    assert_eq!(r["status"], "ok");
    assert_eq!(r["method"], "contours");
    assert!(err < 40.0, "max error {err:.1} m");
    let range = &r["diagnostics"]["scale_range"];
    assert_eq!(range["source"], "scale_label");
    assert!(range["min"].as_f64().unwrap() > 1.3 && range["max"].as_f64().unwrap() < 2.8, "{range}");
    // Coordinate labels were not used as coordinates, so they are not POIs either.
    assert!(r["pois"].as_array().unwrap().iter().any(|p| p["name"] == "KWAMI LODGE"));
}

/// (c) GPS 55 km west: the sheet is outside the search area. After the DEM
/// loop the answer must be "failed", never a confident wrong position.
#[test]
fn contours_far_away_fail_honestly() {
    let _g = serial();
    let s = Sess::new(map_raster());
    let req = json!({"gps": gps(WEST_55KM.0, WEST_55KM.1), "dpi": 300.0});
    let l = run_dem_loop(&s, &req, 8);
    let r = l.last();
    summary("contours 55km W", r, None, &l);
    assert_eq!(r["status"], "failed", "{r}");
    assert!(r["pixel_to_mercator"].is_null());
    // The default radius is 50 km; the message says what was searched.
    let m = r["message"].as_str().unwrap();
    assert!(m.starts_with("No match within 50 km of you (searched "), "{m}");
    assert_eq!(r["search"]["stage"], "done");
    assert_eq!(r["search"]["searched_radius_m"], 50_000.0);
    if let Some(c) = r["diagnostics"]["best_confidence"].as_f64() {
        assert!(c < 0.5);
    }
}

/// Labels without grid lines (graticule painted out): the labels-only fit is
/// refined against the terrain directly, with no coarse search.
#[test]
fn labels_without_lines_are_refined_with_contours() {
    let _g = serial();
    let (mut rgba, w, h) = load_map_rgba();
    let truth_json: Value = serde_json::from_slice(&std::fs::read(fixture_dir().join("truth.json")).unwrap()).unwrap();
    let (fx0, fy0, fx1, fy1) = (110, 110, 3396, 4838);
    for la in truth_json["lat_lines"].as_array().unwrap() {
        let y0 = la["y"].as_i64().unwrap() as usize;
        for y in y0 - 4..=y0 + 4 {
            for x in fx0..fx1 {
                rgba[(y * w + x) * 4..(y * w + x) * 4 + 3].copy_from_slice(&[255, 255, 255]);
            }
        }
    }
    for lo in truth_json["lon_lines"].as_array().unwrap() {
        let x0 = lo["x"].as_i64().unwrap() as usize;
        for y in fy0..fy1 {
            for x in x0 - 4..=x0 + 4 {
                rgba[(y * w + x) * 4..(y * w + x) * 4 + 3].copy_from_slice(&[255, 255, 255]);
            }
        }
    }
    let s = Sess::new(&padded(&rgba, w, h));
    let req = json!({"gps": gps(CENTER.0, CENTER.1), "ocr": ocr_json(), "dpi": 300.0});
    let l = run_dem_loop(&s, &req, 5);
    let r = l.last();
    let err = max_error_m(&homography(r));
    summary("labels->contours", r, Some(err), &l);
    assert_eq!(r["diagnostics"]["grid"]["method"], "labels_only", "{}", r["diagnostics"]["grid"]);
    assert_eq!(r["status"], "ok");
    assert_eq!(r["method"], "contours");
    assert_eq!(r["diagnostics"]["refined"]["source"], "labels");
    assert!(r["diagnostics"]["candidates"].is_null(), "no coarse search expected");
    assert!(err < 40.0, "max error {err:.1} m");
    assert!(r["confidence"].as_f64().unwrap() >= 0.7);

    // Without contour matching the labels-only fit is reported as such.
    let s = Sess::new(&padded(&rgba, w, h));
    let req = json!({"gps": gps(CENTER.0, CENTER.1), "ocr": ocr_json(), "methods": ["grid"]});
    let l = run_dem_loop(&s, &req, 1);
    let r = l.last();
    let err = max_error_m(&homography(r));
    summary("labels only", r, Some(err), &l);
    assert_eq!(r["status"], "ok");
    assert_eq!(r["method"], "labels");
    assert!(r["confidence"].as_f64().unwrap() <= 0.5);
    assert!(err < 150.0);
}

/// (e) Progress is readable from another thread while a contour run works,
/// and a cancel from another thread stops it.
#[test]
fn progress_and_cancel_from_another_thread() {
    let _g = serial();
    let s = Arc::new(Sess::new(map_raster()));
    s.supply_all();
    let req = json!({"gps": gps(CENTER.0, CENTER.1), "dpi": 300.0});

    // Progress poller.
    let done = Arc::new(AtomicBool::new(false));
    let poller = {
        let (s, done) = (s.clone(), done.clone());
        std::thread::spawn(move || {
            let mut seen = Vec::new();
            while !done.load(Ordering::Relaxed) {
                seen.push(s.progress());
                std::thread::sleep(Duration::from_micros(500));
            }
            seen
        })
    };
    let r = s.georef(&req);
    done.store(true, Ordering::Relaxed);
    let seen = poller.join().unwrap();
    assert_eq!(r["status"], "ok", "{r}");
    assert!(seen.iter().all(|p| (0.0..=1.0).contains(p)));
    let mid = seen.iter().filter(|p| **p > 0.0 && **p < 1.0).count();
    eprintln!(
        "[progress] {} samples, {} intermediate, max {:?}",
        seen.len(),
        mid,
        seen.iter().cloned().reduce(f32::max)
    );
    assert!(mid > 0, "no intermediate progress observed");
    assert_eq!(s.progress(), 1.0);

    // Cancel mid-run (a fresh session, so nothing is cached).
    let s = Arc::new(Sess::new(map_raster()));
    s.supply_all();
    let canceller = {
        let s = s.clone();
        std::thread::spawn(move || {
            let t = Instant::now();
            while s.progress() < 0.4 && t.elapsed() < Duration::from_secs(20) {
                std::thread::sleep(Duration::from_micros(200));
            }
            s.cancel();
        })
    };
    let t = Instant::now();
    let r = s.georef(&req);
    let dt = t.elapsed();
    canceller.join().unwrap();
    eprintln!("[cancel] {} after {:.0} ms: {}", r["status"], dt.as_secs_f64() * 1e3, r["message"]);
    assert_eq!(r["status"], "failed");
    assert_eq!(r["message"], "cancelled");
    // The cancel was consumed: the next call runs to the end.
    let r = s.georef(&req);
    assert_eq!(r["status"], "ok", "{r}");
    assert!(max_error_m(&homography(&r)) < 40.0);
}
