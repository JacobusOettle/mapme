//! The photo-mode `warp` (docs/ARCHITECTURE.md, *Georeference response*): the
//! lattice contract (node layout, bilinear evaluation, clamping, JSON shape),
//! the fixed-point inverse, the fold-free bound, no warp for scans, and on
//! the real phone photo of the folded Grootkloof sheet
//! (`fixtures/grootkloof/photo`) the accuracy with and without it.
//!
//! Run with `cargo test --release --test warp -- --nocapture`.

mod common;
use common::{dem_tiles, fixture_dir, load_map, ocr_fixture, rectify, CENTER};
use mapme_core::geo::{haversine_m, latlon_to_merc, merc_to_latlon, Homography, LatLon};
use mapme_core::georef::{GeorefResponse, Session};
use mapme_core::image::Rgb8;
use mapme_core::labels::OcrObservation;
use mapme_core::warp::{Warp, MAX_DERIVATIVE_RATIO};
use serde_json::{json, Value};
use std::sync::{Mutex, OnceLock};

/// Heavy tests run one at a time.
fn serial() -> std::sync::MutexGuard<'static, ()> {
    static M: Mutex<()> = Mutex::new(());
    M.lock().unwrap_or_else(|e| e.into_inner())
}

type Pt = (f64, f64);

/// A projective pixel → Mercator transform like a flattened photo's
/// (≈ 4.3 m/px, north up, a little perspective).
fn photo_like_homography() -> Homography {
    let (x0, y0) = latlon_to_merc(-25.80, 27.36);
    let a = Homography { m: [4.3, 0.05, x0, 0.04, -4.3, y0, 0.0, 0.0, 1.0] };
    let p = Homography { m: [1.0, 0.0, 0.0, 0.0, 1.0, 0.0, 2e-6, -3e-6, 1.0] };
    a.mul(&p)
}

/// A smooth warp on the lattice of a `w` × `h` image: `amp` metres, folded
/// along x = w/2 (a tent), plus a gentle curl.
fn folded_warp(w: usize, h: usize, amp: f64) -> Warp {
    let mut warp = Warp::zero(w, h);
    for j in 0..warp.rows {
        for i in 0..warp.cols {
            let (x, y) = (i as f64 * warp.spacing[0], j as f64 * warp.spacing[1]);
            let (u, v) = (x / w as f64, y / h as f64);
            let k = 2 * (j * warp.cols + i);
            warp.offsets_m[k] = amp * (1.0 - 2.0 * (u - 0.5).abs()) + 0.3 * amp * v * v;
            warp.offsets_m[k + 1] = -0.6 * amp * (std::f64::consts::PI * v).sin() * u;
        }
    }
    warp
}

// ---------------------------------------------------------------------------
// Lattice contract
// ---------------------------------------------------------------------------

#[test]
fn lattice_evaluates_bilinearly_and_clamps() {
    // 3 × 2 nodes at (10, 20) + (i·100, j·50).
    let w: Warp = serde_json::from_value(json!({
        "cols": 3, "rows": 2, "origin": [10.0, 20.0], "spacing": [100.0, 50.0],
        "offsets_m": [0.0, 0.0,   10.0, -4.0,   20.0, 8.0,
                      2.0, 6.0,   12.0, 2.0,    22.0, 14.0]
    }))
    .unwrap();
    assert!(w.is_valid());
    let close = |a: Pt, b: Pt| (a.0 - b.0).abs() < 1e-12 && (a.1 - b.1).abs() < 1e-12;
    // Nodes: offsets_m[2·(j·cols + i)], [… + 1].
    assert!(close(w.offset(10.0, 20.0), (0.0, 0.0)));
    assert!(close(w.offset(110.0, 20.0), (10.0, -4.0)));
    assert!(close(w.offset(210.0, 70.0), (22.0, 14.0)));
    assert!(close(w.offset(10.0, 70.0), (2.0, 6.0)));
    // Bilinear inside a cell: the centre of the first cell is the mean of its corners.
    assert!(close(w.offset(60.0, 45.0), ((0.0 + 10.0 + 2.0 + 12.0) / 4.0, (0.0 - 4.0 + 6.0 + 2.0) / 4.0)));
    // A quarter along x on the top edge.
    assert!(close(w.offset(135.0, 20.0), (12.5, -1.0)));
    // Constant beyond the lattice (coordinates clamped).
    assert!(close(w.offset(-500.0, -500.0), (0.0, 0.0)));
    assert!(close(w.offset(1e6, 1e6), (22.0, 14.0)));
    assert!(close(w.offset(160.0, 1e6), w.offset(160.0, 70.0)));
    assert!(close(w.offset(-1.0, 45.0), w.offset(10.0, 45.0)));
    // The full model adds the offset to H.
    let h = photo_like_homography();
    let (hx, hy) = h.apply(135.0, 20.0);
    assert!(close(w.apply(&h, 135.0, 20.0), (hx + 12.5, hy - 1.0)));
    // Inconsistent sizes: invalid, and evaluates to no offset.
    let bad = Warp { offsets_m: vec![1.0; 5], ..w.clone() };
    assert!(!bad.is_valid());
    assert_eq!(bad.offset(50.0, 50.0), (0.0, 0.0));
}

#[test]
fn lattice_covers_the_image() {
    for (w, h) in [(1703, 2232), (3024, 4032), (4960, 3507), (800, 600)] {
        let warp = Warp::zero(w, h);
        assert_eq!(warp.origin, [0.0, 0.0]);
        assert_eq!(warp.spacing, [w as f64 / (warp.cols - 1) as f64, h as f64 / (warp.rows - 1) as f64]);
        assert_eq!(warp.cols.max(warp.rows), mapme_core::warp::LONG_SIDE_NODES);
        // About square cells.
        let r = warp.spacing[0] / warp.spacing[1];
        assert!((0.8..1.25).contains(&r), "{w}×{h}: {:?}", warp.spacing);
        assert_eq!(warp.offsets_m.len(), 2 * warp.cols * warp.rows);
    }
}

#[test]
fn warp_json_shape() {
    let w = folded_warp(1703, 2232, 120.0).rounded();
    let v = serde_json::to_value(&w).unwrap();
    let keys: Vec<&String> = v.as_object().unwrap().keys().collect();
    assert_eq!(keys, ["cols", "offsets_m", "origin", "rows", "spacing"]);
    assert_eq!(v["origin"], json!([0.0, 0.0]));
    assert_eq!(v["offsets_m"].as_array().unwrap().len(), 2 * w.cols * w.rows);
    // Centimetres in the JSON.
    assert!(w.offsets_m.iter().all(|o| (o * 100.0 - (o * 100.0).round()).abs() < 1e-6));
    let back: Warp = serde_json::from_value(v).unwrap();
    assert_eq!(back, w);
    // A response without a warp has no `warp` key; with one, it is the object above.
    let mut r = GeorefResponse::failed("x");
    assert!(serde_json::to_value(&r).unwrap().get("warp").is_none());
    r.warp = Some(w.clone());
    let v = serde_json::to_value(&r).unwrap();
    assert_eq!(serde_json::from_value::<Warp>(v["warp"].clone()).unwrap(), w);
    // Clients may send it back as null.
    let mut v = v;
    v["warp"] = Value::Null;
    assert!(serde_json::from_value::<GeorefResponse>(v).unwrap().warp.is_none());
}

/// p₀ = H⁻¹(m), pₖ₊₁ = H⁻¹(m − B(pₖ)) as the contract states it.
fn contract_inverse(w: &Warp, h: &Homography, m: Pt, iterations: usize) -> Pt {
    let hi = h.inverse().unwrap();
    let mut p = hi.apply(m.0, m.1);
    for _ in 0..iterations {
        let (dx, dy) = w.offset(p.0, p.1);
        p = hi.apply(m.0 - dx, m.1 - dy);
    }
    p
}

#[test]
fn inverse_by_fixed_point_iteration() {
    let h = photo_like_homography();
    let (iw, ih) = (1703usize, 2232usize);
    let warp = folded_warp(iw, ih, 150.0);
    let ratio = warp.derivative_ratio(&h);
    assert!(ratio < MAX_DERIVATIVE_RATIO, "{ratio}");
    let mut worst: f64 = 0.0;
    for j in -2..=24 {
        for i in -2..=18 {
            let p = (i as f64 * 100.0 + 7.3, j as f64 * 100.0 + 3.1);
            let m = warp.apply(&h, p.0, p.1);
            let q = warp.invert(&h, m.0, m.1).unwrap();
            worst = worst.max((q.0 - p.0).hypot(q.1 - p.1));
            // The contract: 6 iterations reach < 0.01 px.
            let q6 = contract_inverse(&warp, &h, m, 6);
            assert!((q6.0 - p.0).hypot(q6.1 - p.1) < 0.01, "{p:?}: {q6:?}");
        }
    }
    assert!(worst < 1e-3, "round trip off by {worst} px");
    // The zero warp inverts H exactly.
    let zero = Warp::zero(iw, ih);
    let m = h.apply(500.0, 600.0);
    let q = zero.invert(&h, m.0, m.1).unwrap();
    assert!((q.0 - 500.0).abs() < 1e-6 && (q.1 - 600.0).abs() < 1e-6);
}

#[test]
fn derivative_ratio_measures_folding() {
    let h = photo_like_homography();
    let flat = Warp::zero(1703, 2232);
    assert_eq!(flat.derivative_ratio(&h), 0.0);
    // 150 m over half the sheet width: a gentle tent.
    let gentle = folded_warp(1703, 2232, 150.0);
    let r = gentle.derivative_ratio(&h);
    assert!(r > 0.01 && r < 0.1, "{r}");
    // One node pushed 400 m against its neighbours ~110 px away (≈ 470 m):
    // nearly a fold.
    let mut steep = Warp::zero(1703, 2232);
    let k = 2 * (5 * steep.cols + 5);
    steep.offsets_m[k] = 400.0;
    let r = steep.derivative_ratio(&h);
    assert!(r > MAX_DERIVATIVE_RATIO, "{r}");
    // At the node the derivative is [[−400/sx, −400/sy], [0, 0]] (one cell
    // on): its norm over the smallest singular value of H's there.
    let la = h.local_affine(5.0 * steep.spacing[0], 5.0 * steep.spacing[1]);
    let s1 = la.a * la.a + la.b * la.b + la.d * la.d + la.e * la.e;
    let det = la.det().abs();
    let smin = ((s1 - (s1 * s1 - 4.0 * det * det).max(0.0).sqrt()) / 2.0).sqrt();
    let expect = 400.0 * (1.0 / steep.spacing[0]).hypot(1.0 / steep.spacing[1]) / smin;
    assert!((r / expect - 1.0).abs() < 0.02, "{r} vs {expect}");
}

// ---------------------------------------------------------------------------
// Scans never get a warp
// ---------------------------------------------------------------------------

/// The app's DEM loop: call, add the requested fixture tiles (or mark them
/// missing; all of them `offline`), call again.
fn run_loop(s: &Session, req: &Value, offline: bool) -> (Value, usize) {
    let tiles = dem_tiles();
    for round in 1..=6 {
        let r: Value = serde_json::from_str(&s.georeference_json(&req.to_string())).unwrap();
        if r["status"] != "need_dem" {
            return (r, round);
        }
        for t in r["need_tiles"].as_array().unwrap() {
            let id = serde_json::from_value(t.clone()).unwrap();
            match tiles.iter().find(|(k, _)| *k == id && !offline) {
                Some((_, png)) => s.add_dem_tile(id, png).unwrap(),
                None => s.mark_dem_tile_missing(id).unwrap(),
            }
        }
    }
    panic!("still need_dem after 6 rounds");
}

fn gps(p: Pt) -> Value {
    json!({"lat": p.0, "lon": p.1, "accuracy_m": 10.0})
}

#[test]
fn scans_never_get_a_warp() {
    let _g = serial();
    let s = Session::new(load_map());
    for req in [
        json!({"gps": gps(CENTER), "ocr": ocr_fixture()}),
        json!({"gps": gps(CENTER), "ocr": ocr_fixture(), "photo": false, "verify": true}),
    ] {
        let (r, _) = run_loop(&s, &req, false);
        assert_eq!(r["status"], "ok", "{r}");
        assert_eq!(r["method"], "grid");
        assert!(r.get("warp").is_none(), "{r}");
        assert!(r["diagnostics"].get("warp").is_none(), "{}", r["diagnostics"]);
    }
}

// ---------------------------------------------------------------------------
// The real photo of the folded sheet
// ---------------------------------------------------------------------------

struct Cp {
    x: f64,
    y: f64,
    lat: f64,
    lon: f64,
}

struct RealPhoto {
    /// The photo flattened by its page corners, and flattened → photo pixels.
    flat: Rgb8,
    to_photo: Homography,
    cps: Vec<Cp>,
    ocr: Vec<OcrObservation>,
}

fn real() -> &'static RealPhoto {
    static R: OnceLock<RealPhoto> = OnceLock::new();
    R.get_or_init(|| {
        let dir = fixture_dir().join("photo");
        let bytes = std::fs::read(dir.join("printed-map-photo.jpg")).expect("printed-map-photo.jpg");
        let mut dec = jpeg_decoder::Decoder::new(std::io::Cursor::new(bytes));
        let px = dec.decode().expect("decode jpeg");
        let info = dec.info().unwrap();
        let photo = Rgb8::from_rgb(info.width as usize, info.height as usize, px).unwrap();
        let json = |name: &str| -> Value { serde_json::from_slice(&std::fs::read(dir.join(name)).unwrap()).unwrap() };
        let truth = json("truth.json");
        let q = truth["page_quad"].as_array().unwrap();
        let quad: [Pt; 4] = [0, 1, 2, 3].map(|i| (q[i][0].as_f64().unwrap(), q[i][1].as_f64().unwrap()));
        let (flat, to_photo) = rectify(&photo, &quad);
        let from_photo = to_photo.inverse().unwrap();
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
        // The photo OCR moved into the flattened image (bounding box of each quad).
        let ocr = json("ocr.json")["observations"]
            .as_array()
            .unwrap()
            .iter()
            .map(|o| {
                let q = o["quad"].as_array().unwrap();
                let pts: Vec<Pt> =
                    (0..4).map(|i| from_photo.apply(q[i][0].as_f64().unwrap(), q[i][1].as_f64().unwrap())).collect();
                let (x0, x1) = pts.iter().fold((f64::MAX, f64::MIN), |a, p| (a.0.min(p.0), a.1.max(p.0)));
                let (y0, y1) = pts.iter().fold((f64::MAX, f64::MIN), |a, p| (a.0.min(p.1), a.1.max(p.1)));
                OcrObservation {
                    text: o["text"].as_str().unwrap().to_string(),
                    bbox: [x0, y0, x1 - x0, y1 - y0],
                    confidence: o["confidence"].as_f64().unwrap_or(0.5) as f32,
                }
            })
            .collect();
        RealPhoto { flat, to_photo, cps, ocr }
    })
}

#[derive(Debug, Clone, Copy)]
struct Stats {
    median: f64,
    p90: f64,
    max: f64,
}

/// Ground errors (metres) of a flattened-pixel → Mercator model at the truth
/// control points (photo pixels, moved into the flattened image).
fn stats(model: &dyn Fn(f64, f64) -> Pt) -> Stats {
    let r = real();
    let from_photo = r.to_photo.inverse().unwrap();
    let mut e: Vec<f64> = r
        .cps
        .iter()
        .map(|c| {
            let (u, v) = from_photo.apply(c.x, c.y);
            let (mx, my) = model(u, v);
            let (lat, lon) = merc_to_latlon(mx, my);
            haversine_m(LatLon::new(lat, lon), LatLon::new(c.lat, c.lon))
        })
        .collect();
    e.sort_by(|a, b| a.total_cmp(b));
    let q = |p: f64| e[((e.len() - 1) as f64 * p).round() as usize];
    Stats { median: q(0.5), p90: q(0.9), max: *e.last().unwrap() }
}

/// Georeference the flattened real photo; check the warp's contract on the
/// response and return the errors without and with it.
fn real_photo(req: Value, source: &str, offline: bool) -> (Stats, Stats, Value) {
    let _g = serial();
    let r = real();
    let s = Session::new(r.flat.clone());
    let (resp, rounds) = run_loop(&s, &req, offline);
    assert_eq!(resp["status"], "ok", "{resp}");
    let m: Vec<f64> = resp["pixel_to_mercator"].as_array().unwrap().iter().map(|v| v.as_f64().unwrap()).collect();
    let h = Homography { m: m.try_into().unwrap() };
    let d = &resp["diagnostics"]["warp"];
    let warp: Warp = serde_json::from_value(resp["warp"].clone()).unwrap_or_else(|e| panic!("no warp ({e}): {d}"));
    let flat = stats(&|x, y| h.apply(x, y));
    let full = stats(&|x, y| warp.apply(&h, x, y));
    eprintln!(
        "[warp] {source}: H alone {flat:?}, H + warp {full:?}; {rounds} calls, warp {} ms\n    {d}",
        resp["timings_ms"]["warp"]
    );
    // The lattice of the flattened image, fold-free, plausible.
    let (iw, ih) = (r.flat.w, r.flat.h);
    let lattice = Warp::zero(iw, ih);
    assert_eq!(
        (warp.cols, warp.rows, warp.origin, warp.spacing),
        (lattice.cols, lattice.rows, [0.0; 2], lattice.spacing)
    );
    assert!(warp.derivative_ratio(&h) <= MAX_DERIVATIVE_RATIO);
    assert_eq!(d["accepted"], true);
    assert_eq!(d["source"], source);
    assert!(d["max_offset_m"].as_f64().unwrap() <= mapme_core::warp::MAX_OFFSET_M);
    // `corners`, `scale_m_per_px` and `rotation_deg` include the warp.
    let corners = resp["corners"].as_array().unwrap();
    for (k, &(x, y)) in [(0.0, 0.0), (iw as f64, 0.0), (iw as f64, ih as f64), (0.0, ih as f64)].iter().enumerate() {
        let (lat, lon) = {
            let (mx, my) = warp.apply(&h, x, y);
            merc_to_latlon(mx, my)
        };
        assert!(
            (corners[k][0].as_f64().unwrap() - lat).abs() < 1e-9
                && (corners[k][1].as_f64().unwrap() - lon).abs() < 1e-9
        );
    }
    let la = warp.local_affine(&h, iw as f64 / 2.0, ih as f64 / 2.0);
    let (_, my) = warp.apply(&h, iw as f64 / 2.0, ih as f64 / 2.0);
    let scale = la.mean_scale() * merc_to_latlon(0.0, my).0.to_radians().cos();
    assert!((resp["scale_m_per_px"].as_f64().unwrap() - scale).abs() < 1e-3);
    // The inverse at every control point.
    let from_photo = r.to_photo.inverse().unwrap();
    for c in &r.cps {
        let p = from_photo.apply(c.x, c.y);
        let m = warp.apply(&h, p.0, p.1);
        let q = contract_inverse(&warp, &h, m, 6);
        assert!((q.0 - p.0).hypot(q.1 - p.1) < 0.01, "inverse at {p:?}: {q:?}");
    }
    (flat, full, resp)
}

/// Grid labels and lines (and the contours under them): the warp halves the
/// median error of the best homography the grid gives and cuts its tail.
#[test]
fn real_photo_grid_warp() {
    let (flat, full, resp) =
        real_photo(json!({"gps": gps(CENTER), "ocr": real().ocr.clone(), "photo": true}), "grid", false);
    assert_eq!(resp["method"], "grid");
    assert!(flat.median > 25.0 && flat.p90 > 70.0, "the folded sheet should defeat a single homography: {flat:?}");
    assert!(full.median <= 15.0 && full.p90 <= 60.0 && full.max <= 170.0, "{full:?}");
    assert!(full.median < 0.5 * flat.median && full.max < 0.6 * flat.max, "{flat:?} → {full:?}");
    let d = &resp["diagnostics"]["warp"];
    assert_eq!(d["n_lines"], 8);
    assert!(d["n_segments"].as_u64().unwrap() > 500, "{d}");
    assert!(d["holdout_rms_m"].as_f64().unwrap() < 0.6 * d["holdout_before_m"].as_f64().unwrap(), "{d}");
}

/// Contours only (GPS on the map): the warp from the contours and the
/// sheet's straight lines.
#[test]
fn real_photo_contour_warp() {
    let (flat, full, resp) =
        real_photo(json!({"methods": ["contours"], "gps": gps((-25.8405, 27.3920)), "photo": true}), "contours", false);
    assert_eq!(resp["method"], "contours");
    assert!(flat.median > 30.0, "{flat:?}");
    assert!(full.median <= 20.0 && full.p90 <= 75.0 && full.max <= 180.0, "{full:?}");
    assert!(full.median < 0.6 * flat.median && full.max < 0.6 * flat.max, "{flat:?} → {full:?}");
    assert!(resp["diagnostics"]["warp"]["contour_holdout_gain"].as_f64().unwrap() >= 0.06);
}

/// Offline (no DEM): the grid lines alone are too far apart on this sheet to
/// predict each other through the folds (each one left out ends up farther
/// from where the warp fitted without it puts it than from the homography's
/// prediction), so no warp is applied and `pixel_to_mercator` is the same.
#[test]
fn real_photo_grid_offline_gets_no_warp() {
    let req = json!({"gps": gps(CENTER), "ocr": real().ocr.clone(), "photo": true});
    let _g = serial();
    let online = run_loop(&Session::new(real().flat.clone()), &req, false).0;
    let (resp, _) = run_loop(&Session::new(real().flat.clone()), &req, true);
    assert_eq!(resp["status"], "ok");
    assert!(resp.get("warp").is_none());
    let d = &resp["diagnostics"]["warp"];
    eprintln!("[warp] grid offline: {d}");
    assert_eq!(d["accepted"], false);
    assert_eq!(d["n_segments"], 0);
    assert_eq!(d["rejected"], "grid lines left out do not agree with the warp");
    assert_eq!(resp["pixel_to_mercator"], online["pixel_to_mercator"]);
    assert!(online["warp"].is_object());
}
