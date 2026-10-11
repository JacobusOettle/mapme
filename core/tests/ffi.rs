//! The C ABI used by the apps: argument checking, JSON errors, cancel,
//! DEM tile bookkeeping and manual control points. Everything goes through
//! the `extern "C"` functions exactly as Swift calls them.

mod common;
use common::*;
use mapme_core::ffi::*;
use mapme_core::geo::Homography;
use serde_json::{json, Value};
use std::ffi::{c_char, CStr, CString};
use std::ptr;

/// Take ownership of a returned string and parse it.
fn take_json(p: *mut c_char) -> Value {
    assert!(!p.is_null());
    let s = unsafe { CStr::from_ptr(p) }.to_str().expect("UTF-8").to_owned();
    unsafe { mm_string_free(p) };
    serde_json::from_str(&s).unwrap_or_else(|e| panic!("not JSON ({e}): {s}"))
}

struct Sess(*mut MMSession);

impl Sess {
    /// Blank white RGBA image.
    fn blank(w: u32, h: u32) -> Sess {
        let px = vec![255u8; (w * h * 4) as usize];
        let p = unsafe { mm_session_new(px.as_ptr(), w, h, w * 4) };
        assert!(!p.is_null());
        Sess(p)
    }

    fn georef_raw(&self, bytes: &[u8]) -> Value {
        let c = CString::new(bytes).unwrap();
        take_json(unsafe { mm_session_georeference(self.0, c.as_ptr()) })
    }

    fn georef(&self, req: &Value) -> Value {
        self.georef_raw(req.to_string().as_bytes())
    }

    fn mark_missing(&self, t: &Value) -> i32 {
        unsafe {
            mm_session_mark_dem_tile_missing(
                self.0,
                t["z"].as_u64().unwrap() as u8,
                t["x"].as_u64().unwrap() as u32,
                t["y"].as_u64().unwrap() as u32,
            )
        }
    }

    fn add_tile(&self, t: &Value, png: &[u8]) -> i32 {
        unsafe {
            mm_session_add_dem_tile(
                self.0,
                t["z"].as_u64().unwrap() as u8,
                t["x"].as_u64().unwrap() as u32,
                t["y"].as_u64().unwrap() as u32,
                png.as_ptr(),
                png.len(),
            )
        }
    }
}

impl Drop for Sess {
    fn drop(&mut self) {
        unsafe { mm_session_free(self.0) }
    }
}

fn centre_gps() -> Value {
    json!({"lat": CENTER.0, "lon": CENTER.1})
}

#[test]
fn version_is_json() {
    let v = take_json(mm_version());
    assert_eq!(v["version"], env!("CARGO_PKG_VERSION"));
    assert!(v["build"].as_str().unwrap().contains(std::env::consts::ARCH));
}

#[test]
fn null_pointers_and_bad_sizes_are_refused() {
    unsafe {
        mm_string_free(ptr::null_mut());
        mm_session_free(ptr::null_mut());
        mm_session_cancel(ptr::null_mut());
        assert_eq!(mm_session_progress(ptr::null()), 0.0);
        let px = vec![0u8; 64 * 64 * 4];
        assert!(mm_session_new(ptr::null(), 64, 64, 256).is_null());
        assert!(mm_session_new(px.as_ptr(), 0, 64, 256).is_null());
        assert!(mm_session_new(px.as_ptr(), 64, 0, 256).is_null());
        assert!(mm_session_new(px.as_ptr(), 64, 64, 255).is_null(), "bytes_per_row < 4 * width");
        assert!(mm_session_new(px.as_ptr(), 100_000, 64, 400_000).is_null(), "too large");

        let req = CString::new("{}").unwrap();
        let v = take_json(mm_session_georeference(ptr::null_mut(), req.as_ptr()));
        assert_eq!(v["status"], "failed");
        assert!(v["message"].as_str().unwrap().contains("NULL"));
        let s = Sess::blank(64, 64);
        let v = take_json(mm_session_georeference(s.0, ptr::null()));
        assert_eq!(v["status"], "failed");
        let v = take_json(mm_fit_control_points(ptr::null()));
        assert_eq!(v["status"], "failed");

        let png = [0x89u8, b'P', b'N', b'G'];
        assert_eq!(mm_session_add_dem_tile(ptr::null_mut(), 11, 1, 1, png.as_ptr(), png.len()), MM_ERR_ARGUMENT);
        assert_eq!(mm_session_add_dem_tile(s.0, 11, 1, 1, ptr::null(), 4), MM_ERR_ARGUMENT);
        assert_eq!(mm_session_add_dem_tile(s.0, 11, 1, 1, png.as_ptr(), 0), MM_ERR_ARGUMENT);
        assert_eq!(mm_session_add_dem_tile(s.0, 11, 4096, 1, png.as_ptr(), png.len()), MM_ERR_ARGUMENT);
        assert_eq!(mm_session_add_dem_tile(s.0, 30, 1, 1, png.as_ptr(), png.len()), MM_ERR_ARGUMENT);
        assert_eq!(mm_session_add_dem_tile(s.0, 11, 1, 1, png.as_ptr(), png.len()), MM_ERR_DECODE);
        assert_eq!(mm_session_mark_dem_tile_missing(ptr::null_mut(), 11, 1, 1), MM_ERR_ARGUMENT);
        assert_eq!(mm_session_mark_dem_tile_missing(s.0, 3, 8, 0), MM_ERR_ARGUMENT);
        assert_eq!(mm_session_mark_dem_tile_missing(s.0, 3, 7, 7), MM_OK);
    }
}

#[test]
fn invalid_json_fails_softly() {
    let s = Sess::blank(64, 64);
    for bad in [&b"{not json"[..], b"", b"[1, 2]", b"{\"gps\": {\"lat\": 1}}", b"{\"ocr\": [{\"text\": 5}]}"] {
        let v = s.georef_raw(bad);
        assert_eq!(v["status"], "failed", "{}", String::from_utf8_lossy(bad));
        assert!(v["message"].as_str().unwrap().starts_with("Invalid request"), "{v}");
    }
    // Not UTF-8.
    let v = s.georef_raw(&[b'{', 0xff, 0xfe, b'}']);
    assert_eq!(v["status"], "failed");
    assert!(v["message"].as_str().unwrap().contains("UTF-8"));
    let v = s.georef(&json!({"contour_rgb": [1, 2]}));
    assert_eq!(v["status"], "failed");
    assert!(v["message"].as_str().unwrap().contains("contour_rgb"));
    let v = s.georef(&json!({"methods": ["magic"]}));
    assert_eq!(v["status"], "failed");
    assert!(v["message"].as_str().unwrap().contains("method"));
    let v = take_json(unsafe { mm_fit_control_points(CString::new("{oops").unwrap().as_ptr()) });
    assert_eq!(v["status"], "failed");
}

#[test]
fn unknown_fields_and_nulls_are_accepted() {
    let s = Sess::blank(64, 64);
    let v = s.georef(&json!({
        "gps": null, "ocr": null, "dpi": null, "contour_rgb": null, "search_radius_m": null,
        "allow_rotation": null, "methods": null, "verify": null, "something_new": {"x": 1}
    }));
    assert_eq!(v["status"], "failed");
    let msg = v["message"].as_str().unwrap();
    assert!(!msg.starts_with("Invalid request"), "{msg}");
    // No text and no location: says what is missing.
    assert!(msg.contains("location"), "{msg}");
    for key in ["method", "pixel_to_mercator", "confidence", "corners"] {
        assert!(v[key].is_null(), "{key}");
    }
    assert!(v["timings_ms"]["total"].is_number());
    let v = s.georef(&json!({"methods": ["grid"]}));
    assert!(v["message"].as_str().unwrap().contains("No text"), "{v}");
}

/// Cancel before a run, then the DEM bookkeeping: tiles requested once,
/// undecodable tiles count as missing, all-missing terrain fails clearly.
#[test]
fn cancel_and_dem_bookkeeping() {
    let s = Sess::blank(300, 400);
    let req = json!({"gps": centre_gps(), "methods": ["contours"]});
    unsafe { mm_session_cancel(s.0) };
    let v = s.georef(&req);
    assert_eq!(v["status"], "failed");
    assert_eq!(v["message"], "cancelled");
    assert_eq!(v["diagnostics"]["cancelled"], true);
    // The cancel was consumed.
    let v = s.georef(&req);
    assert_eq!(v["status"], "need_dem", "{v}");
    let tiles = v["need_tiles"].as_array().unwrap().clone();
    assert!(!tiles.is_empty() && tiles.iter().all(|t| t["z"] == 11));
    // An undecodable tile is refused but not requested again.
    assert_eq!(s.add_tile(&tiles[0], b"definitely not a png"), MM_ERR_DECODE);
    let v = s.georef(&req);
    assert_eq!(v["status"], "need_dem");
    let again = v["need_tiles"].as_array().unwrap();
    assert_eq!(again.len(), tiles.len() - 1);
    assert!(!again.contains(&tiles[0]));
    // Cancel between DEM rounds stops the next call.
    unsafe { mm_session_cancel(s.0) };
    assert_eq!(s.georef(&req)["message"], "cancelled");
    for t in again {
        assert_eq!(s.mark_missing(t), MM_OK);
    }
    let v = s.georef(&req);
    assert_eq!(v["status"], "failed");
    assert!(v["message"].as_str().unwrap().contains("Terrain data"), "{v}");
    assert_eq!(unsafe { mm_session_progress(s.0) }, 1.0);
}

/// A blank page with the terrain available: no contour lines to match.
#[test]
fn blank_map_has_no_contours() {
    let s = Sess::blank(300, 400);
    let req = json!({"gps": centre_gps()});
    let mut v = s.georef(&req);
    for _ in 0..3 {
        if v["status"] != "need_dem" {
            break;
        }
        for t in v["need_tiles"].as_array().unwrap() {
            let path = fixture_dir().join("dem").join(format!("{}_{}_{}.png", t["z"], t["x"], t["y"]));
            let rc = match std::fs::read(path) {
                Ok(png) => s.add_tile(t, &png),
                Err(_) => s.mark_missing(t),
            };
            assert_eq!(rc, MM_OK);
        }
        v = s.georef(&req);
    }
    assert_eq!(v["status"], "failed");
    assert!(v["message"].as_str().unwrap().contains("No contour lines"), "{v}");
}

fn fit(req: &Value) -> Value {
    let c = CString::new(req.to_string()).unwrap();
    take_json(unsafe { mm_fit_control_points(c.as_ptr()) })
}

/// Graticule intersections from truth.json as control points.
fn truth_points() -> Vec<Value> {
    let v: Value = serde_json::from_slice(&std::fs::read(fixture_dir().join("truth.json")).unwrap()).unwrap();
    let mut out = Vec::new();
    for la in v["lat_lines"].as_array().unwrap() {
        for lo in v["lon_lines"].as_array().unwrap() {
            out.push(json!({"x": lo["x"], "y": la["y"], "lat": la["lat"], "lon": lo["lon"]}));
        }
    }
    out
}

fn homography(r: &Value) -> Homography {
    let m: Vec<f64> = r["pixel_to_mercator"].as_array().unwrap().iter().map(|v| v.as_f64().unwrap()).collect();
    Homography { m: m.try_into().unwrap() }
}

#[test]
fn control_points() {
    let p = truth_points();
    // Three graticule intersections (NW, NE, SW): an exact affine.
    let r = fit(&json!({"points": [p[0], p[2], p[12]], "model": "auto"}));
    assert_eq!(r["status"], "ok", "{r}");
    assert_eq!(r["model"], "affine");
    let err = max_error_m(&homography(&r));
    eprintln!("[gcp] 3 points: {err:.2} m, {}", r["message"]);
    assert!(err < 2.0, "{err}");
    assert_eq!(r["residuals_m"].as_array().unwrap().len(), 3);
    assert!((r["scale_m_per_px"].as_f64().unwrap() - 1.906).abs() < 0.01);
    // Two points: a similarity (no model given).
    let r = fit(&json!({"points": [p[0], p[14]]}));
    assert_eq!(r["status"], "ok", "{r}");
    assert_eq!(r["model"], "similarity");
    let err = max_error_m(&homography(&r));
    eprintln!("[gcp] 2 points: {err:.2} m");
    assert!(err < 5.0, "{err}");
    // All 15, least squares.
    let r = fit(&json!({"points": p, "model": "affine"}));
    assert_eq!(r["status"], "ok");
    assert!(r["rms_m"].as_f64().unwrap() < 1.0);
    // Too few / degenerate.
    let r = fit(&json!({"points": [p[0]]}));
    assert_eq!(r["status"], "failed");
    assert!(r["pixel_to_mercator"].is_null());
    let r = fit(&json!({"points": [p[0], p[0]]}));
    assert_eq!(r["status"], "failed");
    let r = fit(&json!({"points": [p[0], p[1], p[2]], "model": "affine"}));
    assert_eq!(r["status"], "failed", "collinear: {r}");
    let r = fit(&json!({"points": [p[0], p[2], p[12]], "model": "warp"}));
    assert_eq!(r["status"], "failed");
}
