//! Searching beyond the near area: request hints, hints from the map's own
//! coordinate labels, `hint_queries`, the chunked wide search and its
//! progress, all hermetic (fixture DEM tiles, or synthetic terrain served
//! tile by tile like the app's need_dem loop). Run with `cargo test --release`.

mod common;
use common::*;
use mapme_core::dem::{resolution, tile_bounds, TileId, TILE_SIZE};
use mapme_core::geo::{destination, latlon_to_merc, merc_to_latlon, Affine, Homography, LatLon};
use mapme_core::georef::{GeorefRequest, Session};
use mapme_core::hints::hint_queries;
use mapme_core::image::Rgb8;
use mapme_core::labels::OcrObservation;
use serde_json::{json, Value};
use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::{Mutex, OnceLock};
use std::time::Instant;

/// Heavy tests run one at a time so timings are meaningful.
fn serial() -> std::sync::MutexGuard<'static, ()> {
    static M: Mutex<()> = Mutex::new(());
    M.lock().unwrap_or_else(|e| e.into_inner())
}

fn map() -> &'static Rgb8 {
    static M: OnceLock<Rgb8> = OnceLock::new();
    M.get_or_init(load_map)
}

fn fixture_tiles() -> &'static Vec<(TileId, Vec<u8>)> {
    static T: OnceLock<Vec<(TileId, Vec<u8>)>> = OnceLock::new();
    T.get_or_init(dem_tiles)
}

/// One call through the JSON API.
fn call(s: &Session, req: &Value) -> Value {
    serde_json::from_str(&s.georeference_json(&req.to_string())).unwrap()
}

/// The app's loop on the fixture DEM: add requested tiles the fixture has,
/// mark the others missing. Returns the responses and the tiles requested.
fn fixture_loop(s: &Session, req: &Value, max_rounds: usize) -> (Vec<Value>, Vec<TileId>) {
    let have: std::collections::HashMap<TileId, &Vec<u8>> = fixture_tiles().iter().map(|(t, b)| (*t, b)).collect();
    let mut out = Vec::new();
    let mut asked = Vec::new();
    for _ in 0..max_rounds {
        let r = call(s, req);
        let need = r["status"] == "need_dem";
        if need {
            for t in r["need_tiles"].as_array().unwrap() {
                let t = TileId {
                    z: t["z"].as_u64().unwrap() as u8,
                    x: t["x"].as_u64().unwrap() as u32,
                    y: t["y"].as_u64().unwrap() as u32,
                };
                asked.push(t);
                match have.get(&t) {
                    Some(png) => s.add_dem_tile(t, png).unwrap(),
                    None => s.mark_dem_tile_missing(t).unwrap(),
                }
            }
        }
        out.push(r);
        if !need {
            return (out, asked);
        }
    }
    panic!("still need_dem after {max_rounds} rounds: {}", out.last().unwrap());
}

fn homography(r: &Value) -> Homography {
    let m: Vec<f64> = r["pixel_to_mercator"].as_array().unwrap().iter().map(|v| v.as_f64().unwrap()).collect();
    Homography { m: m.try_into().unwrap() }
}

fn gps(p: LatLon) -> Value {
    json!({"lat": p.lat, "lon": p.lon, "accuracy_m": 10.0})
}

fn summary(name: &str, rs: &[Value], err: Option<f64>) {
    let r = rs.last().unwrap();
    eprintln!(
        "[{name}] {} / {} conf {} err {} rounds {}\n    message: {}\n    search: {}\n    areas: {}\n    wide: {}\n    \
         timings: {}",
        r["status"],
        r["method"],
        r["confidence"],
        err.map_or("-".into(), |e| format!("{e:.1} m")),
        rs.len(),
        r["message"].as_str().unwrap_or(""),
        r["search"],
        r["diagnostics"]["search_areas"],
        r["diagnostics"]["wide"],
        r["timings_ms"]
    );
}

fn centre() -> LatLon {
    LatLon::new(CENTER.0, CENTER.1)
}

/// The search radius is clamped to 1…2 000 km (default 50 km), the tile
/// cap to 1…4 096 (default 64), and every need_dem answer respects the cap.
#[test]
fn radius_and_tile_cap_are_clamped_and_respected() {
    let _g = serial();
    let s = Session::new(map().clone());
    let at = |radius: Value, cap: Value| {
        let req = json!({"gps": gps(centre()), "dpi": 300.0, "methods": ["contours"],
                         "search_radius_m": radius, "max_tiles_per_round": cap});
        call(&s, &req)
    };
    let r = at(json!(null), json!(null));
    assert_eq!(r["status"], "need_dem");
    assert_eq!(r["search"]["radius_m"], 50_000.0, "{}", r["search"]);
    assert_eq!(r["search"]["stage"], "near");
    assert_eq!(r["search"]["coarse_zoom"], 11);
    assert!(r["need_tiles"].as_array().unwrap().len() <= 64);
    let r = at(json!(9e9), json!(5));
    assert_eq!(r["search"]["radius_m"], 2_000_000.0);
    assert_eq!(r["need_tiles"].as_array().unwrap().len(), 5);
    assert!(r["diagnostics"]["tiles_unknown"].as_u64().unwrap() > 5);
    let r = at(json!(10.0), json!(-3));
    assert_eq!(r["search"]["radius_m"], 1_000.0);
    assert_eq!(r["need_tiles"].as_array().unwrap().len(), 1);
    // Requests: hints need lat and lon; unknown sources are places.
    let q: GeorefRequest = serde_json::from_value(json!({
        "hints": [{"lat": -25.8, "lon": 27.4}, {"lat": -26.0, "lon": 28.0, "radius_m": 9000, "source": "USER",
                   "name": "Hartbeespoort"}, {"lat": 1, "lon": 2, "source": "geocoder", "radius_m": null}],
        "max_tiles_per_round": 100.4
    }))
    .unwrap();
    assert_eq!(q.search_radius_m, 50_000.0);
    assert_eq!(q.max_tiles_per_round, 100);
    assert_eq!(q.hints.len(), 3);
    assert_eq!(q.hints[0].radius_m, 15_000.0);
    assert_eq!(serde_json::to_value(q.hints[1].source).unwrap(), "user");
    assert_eq!(q.hints[1].name.as_deref(), Some("Hartbeespoort"));
    assert_eq!(serde_json::to_value(q.hints[2].source).unwrap(), "place");
    assert!(serde_json::from_value::<GeorefRequest>(json!({"hints": [{"lat": 1}]})).is_err());
    let back: GeorefRequest = serde_json::from_value(serde_json::to_value(&q).unwrap()).unwrap();
    assert_eq!(back, q);
}

/// GPS 600 km away (Durban), a correct place hint near the sheet: located
/// through the hint, on fixture tiles only (no tile around the fix is ever
/// requested). The same hint without a GPS fix defines the search alone.
#[test]
fn place_hint_locates_the_sheet_far_from_the_fix() {
    let _g = serial();
    let durban = LatLon::new(-29.86, 31.03);
    // "Hartbeespoort"-style geocode 6 km north-west of the sheet centre.
    let near_map = destination(centre(), 315.0, 6_000.0);
    let hint = json!({"lat": near_map.lat, "lon": near_map.lon, "radius_m": 15000, "source": "place",
                      "name": "Grootkloof"});
    let fixture: HashSet<TileId> = fixture_tiles().iter().map(|t| t.0).collect();
    for with_gps in [true, false] {
        let s = Session::new(map().clone());
        let mut req = json!({"dpi": 300.0, "methods": ["contours"], "search_radius_m": 700_000.0,
                             "hints": [hint.clone()], "max_tiles_per_round": 16});
        if with_gps {
            req["gps"] = gps(durban);
        }
        let t = Instant::now();
        let (rs, asked) = fixture_loop(&s, &req, 12);
        let r = rs.last().unwrap();
        let err = max_error_m(&homography(r));
        summary(if with_gps { "hint, gps 600 km" } else { "hint, no gps" }, &rs, Some(err));
        eprintln!("    {:.1} s, {} tiles asked", t.elapsed().as_secs_f64(), asked.len());
        assert_eq!(r["status"], "ok", "{r}");
        assert_eq!(r["method"], "contours");
        assert!(err < 40.0, "max error {err:.1} m");
        assert!(r["confidence"].as_f64().unwrap() >= 0.7);
        assert!(asked.iter().all(|t| fixture.contains(t)), "asked for tiles outside the fixture");
        for x in &rs {
            assert!(x["need_tiles"].as_array().map_or(0, |v| v.len()) <= 16);
        }
        let areas = r["diagnostics"]["search_areas"].as_array().unwrap();
        assert_eq!(areas[0]["area"], "hint");
        assert_eq!(areas[0]["outcome"], "located");
        assert_eq!(areas[0]["name"], "Grootkloof");
        assert_eq!(r["search"]["hints_tried"], 1);
        assert_eq!(r["search"]["stage"], "done");
        assert!(r["diagnostics"]["wide"].is_null(), "no wide search expected");
    }
}

/// A wrong hint (60 km east of the sheet, on fixture terrain) searched first
/// fails honestly; the correct one after it locates the sheet.
#[test]
fn wrong_hint_then_correct_hint() {
    let _g = serial();
    let wrong = destination(centre(), 90.0, 60_000.0);
    let right = destination(centre(), 200.0, 4_000.0);
    let s = Session::new(map().clone());
    let req = json!({
        "gps": gps(LatLon::new(-29.86, 31.03)), "dpi": 300.0, "methods": ["contours"], "search_radius_m": 800_000.0,
        "hints": [{"lat": wrong.lat, "lon": wrong.lon, "name": "Elsewhere"},
                  {"lat": right.lat, "lon": right.lon, "name": "Grootkloof"}]
    });
    let (rs, _) = fixture_loop(&s, &req, 12);
    let r = rs.last().unwrap();
    let err = max_error_m(&homography(r));
    summary("wrong hint, right hint", &rs, Some(err));
    assert_eq!(r["status"], "ok", "{r}");
    assert!(err < 40.0, "max error {err:.1} m");
    let areas = r["diagnostics"]["search_areas"].as_array().unwrap();
    assert_eq!(areas.len(), 2, "{areas:?}");
    assert_eq!(areas[0]["name"], "Elsewhere");
    assert_ne!(areas[0]["outcome"], "located");
    assert_eq!(areas[1]["name"], "Grootkloof");
    assert_eq!(areas[1]["outcome"], "located");
    assert_eq!(r["search"]["hints_tried"], 2);
}

/// The map's coordinate labels cut down to one latitude and one longitude
/// label (no grid fit possible) and the scale; GPS 300 km away: the core
/// turns the labels into a hint and finds the sheet there.
#[test]
fn coordinate_labels_become_a_hint() {
    let _g = serial();
    let obs: Vec<OcrObservation> = ocr_fixture()
        .into_iter()
        .filter(|o| {
            !(o.text.contains('°') || o.text.contains('\'')) || o.text == "25°49'00\"S" || o.text == "27°23'00\"E"
        })
        .collect();
    assert_eq!(obs.iter().filter(|o| o.text.contains('°') && !o.text.contains("Declination")).count(), 2);
    let polokwane = LatLon::new(-23.90, 29.45);
    let s = Session::new(map().clone());
    let req = json!({"gps": gps(polokwane), "dpi": 300.0, "ocr": obs, "search_radius_m": 400_000.0});
    let (rs, _) = fixture_loop(&s, &req, 12);
    let r = rs.last().unwrap();
    let err = max_error_m(&homography(r));
    summary("label hint", &rs, Some(err));
    assert_eq!(r["diagnostics"]["grid"]["found"], false, "{}", r["diagnostics"]["grid"]);
    assert_eq!(r["status"], "ok", "{r}");
    assert_eq!(r["method"], "contours");
    assert!(err < 40.0, "max error {err:.1} m");
    let hints = r["diagnostics"]["label_hints"].as_array().unwrap();
    assert_eq!(hints.len(), 1);
    assert_eq!(hints[0]["name"], "25°49'S 27°23'E");
    let areas = r["diagnostics"]["search_areas"].as_array().unwrap();
    assert_eq!(areas[0]["source"], "label");
    assert_eq!(areas[0]["outcome"], "located");
    // Every response to a request with OCR names places worth geocoding.
    for x in &rs {
        let q = x["hint_queries"].as_array().unwrap();
        assert!(q.iter().any(|n| n == "GROOTKLOOF"), "{q:?}");
    }
}

/// Neither a GPS fix nor hints (nor coordinate labels): an honest failure.
#[test]
fn no_fix_and_no_hints_fail_honestly() {
    let s = Session::new(map().clone());
    let r = call(&s, &json!({"dpi": 300.0, "methods": ["contours"], "search_radius_m": 1_000_000.0}));
    assert_eq!(r["status"], "failed", "{r}");
    assert!(r["need_tiles"].as_array().unwrap().is_empty());
    let m = r["message"].as_str().unwrap();
    assert!(m.contains("needs your location"), "{m}");
    // Hints beyond the radius of the fix are ignored; then the near area is
    // searched (fixture tiles) and the failure says what was searched.
    let s = Session::new(map().clone());
    let req = json!({"gps": gps(LatLon::new(-25.85, 26.85)), "dpi": 300.0, "methods": ["contours"],
                     "hints": [{"lat": -33.9, "lon": 18.4, "name": "Cape Town"}]});
    let (rs, _) = fixture_loop(&s, &req, 6);
    let r = rs.last().unwrap();
    summary("55 km west, far hint", &rs, None);
    assert_eq!(r["status"], "failed");
    assert_eq!(r["diagnostics"]["hints_ignored"], 1);
    let m = r["message"].as_str().unwrap();
    assert!(m.starts_with("No match within 50 km of you (searched "), "{m}");
    assert!(m.contains("terrain tiles"), "{m}");
    assert_eq!(r["search"]["searched_radius_m"], 50_000.0);
    assert_eq!(r["search"]["stage"], "done");
}

fn ocr_file(path: PathBuf) -> Vec<OcrObservation> {
    let v: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    serde_json::from_value(v.get("observations").cloned().unwrap_or(v)).unwrap()
}

/// Names worth geocoding on the three Magaliesberg sheets: titles and the
/// series name first, no legend entries, furniture or route notes.
#[test]
fn hint_queries_of_the_three_sheets() {
    let data = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/data");
    let fixtures = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../fixtures");
    let mut sheets: Vec<(&str, Vec<OcrObservation>, Vec<&str>)> = vec![
        ("grootkloof", ocr_fixture(), vec!["GROOTKLOOF", "HAMERKOP", "MAGALIESBERG", "KWAMI LODGE"]),
        ("castle-gorge (RapidOCR)", ocr_file(data.join("castle-gorge-ocr.json")), vec!["CASTLE GORGE", "MAGALIESBERG"]),
        ("mhlabatini (RapidOCR)", ocr_file(data.join("mhlabatini-ocr.json")), vec!["MHLABATINI", "MAGALIESBERG"]),
    ];
    // The sheets' own fixtures, once committed.
    for (dir, want) in [("castle-gorge", vec!["MAGALIESBERG"]), ("mhlabatini", vec!["MAGALIESBERG"])] {
        let p = fixtures.join(dir).join("ocr.json");
        if p.exists() {
            sheets.push((dir, ocr_file(p), want));
        }
    }
    let legend = [
        "trig",
        "beacon",
        "parking",
        "gate",
        "resort",
        "access",
        "camp",
        "cave",
        "pool",
        "waterfall",
        "spring",
        "river",
        "building",
        "ruin",
        "clinic",
        "police",
        "grave",
        "monument",
        "worship",
        "vegetation",
        "interval",
        "declination",
        "annual",
        "scale",
        "sheet",
        "mcsa",
        "gis",
        "sept",
        "entry",
        "keep",
        "turn",
        "toilet",
    ];
    for (name, obs, want) in sheets {
        let q = hint_queries(&obs);
        eprintln!("[hint_queries] {name}: {q:?}");
        assert!(!q.is_empty() && q.len() <= 12, "{name}: {q:?}");
        for w in &want {
            let pos = q.iter().position(|x| x.eq_ignore_ascii_case(w));
            assert!(pos.is_some(), "{name}: {w} missing from {q:?}");
        }
        // The title and the series name come first.
        for w in &want[..2] {
            let pos = q.iter().position(|x| x.eq_ignore_ascii_case(w)).unwrap();
            assert!(pos < 4, "{name}: {w} at {pos} in {q:?}");
        }
        for x in &q {
            let lower = x.to_lowercase();
            let words: Vec<&str> = lower.split(|c: char| !c.is_alphanumeric()).filter(|w| !w.is_empty()).collect();
            assert!(
                !words.iter().all(|w| legend.iter().any(|l| w.starts_with(l))),
                "{name}: legend word {x:?} in {q:?}"
            );
            assert!(!x.chars().any(|c| c == '°' || c == ':'), "{name}: {x:?}");
        }
    }
}

// ---------------------------------------------------------------------------
// Synthetic wide search
// ---------------------------------------------------------------------------

/// Smooth random terrain anywhere on Earth: hashed value noise (smoothstep
/// interpolation) in four octaves over ground-ish metres.
fn noise_terrain(mx: f64, my: f64) -> f64 {
    fn hash(i: i64, j: i64, o: u64) -> f64 {
        let mut h = (i as u64).wrapping_mul(0x9E37_79B9_7F4A_7C15) ^ (j as u64).wrapping_mul(0xC2B2_AE3D_27D4_EB4F) ^ o;
        h ^= h >> 29;
        h = h.wrapping_mul(0xBF58_476D_1CE4_E5B9);
        h ^= h >> 32;
        (h >> 11) as f64 / (1u64 << 53) as f64 * 2.0 - 1.0
    }
    let s = |t: f64| t * t * (3.0 - 2.0 * t);
    let mut z = 1200.0;
    for (o, &(lambda, amp)) in [(9_000.0, 320.0), (3_200.0, 130.0), (1_200.0, 45.0), (450.0, 12.0)].iter().enumerate() {
        let (u, v) = (mx / lambda, my / lambda);
        let (i, j) = (u.floor(), v.floor());
        let (fu, fv) = (s(u - i), s(v - j));
        let (i, j) = (i as i64, j as i64);
        let o = o as u64 * 7919;
        let a = hash(i, j, o) * (1.0 - fu) + hash(i + 1, j, o) * fu;
        let b = hash(i, j + 1, o) * (1.0 - fu) + hash(i + 1, j + 1, o) * fu;
        z += amp * (a * (1.0 - fv) + b * fv);
    }
    z
}

/// A Terrarium PNG of any tile of `noise_terrain`.
fn terrarium_png(t: TileId) -> Vec<u8> {
    let b = tile_bounds(t);
    let res = resolution(t.z);
    let mut rgb = vec![0u8; TILE_SIZE * TILE_SIZE * 3];
    for r in 0..TILE_SIZE {
        for c in 0..TILE_SIZE {
            let e = noise_terrain(b.min_x + (c as f64 + 0.5) * res, b.max_y - (r as f64 + 0.5) * res) + 32768.0;
            let i = (r * TILE_SIZE + c) * 3;
            rgb[i] = (e / 256.0).floor() as u8;
            rgb[i + 1] = (e.floor() as i64).rem_euclid(256) as u8;
            rgb[i + 2] = ((e - e.floor()) * 256.0).floor() as u8;
        }
    }
    let mut out = Vec::new();
    {
        let mut enc = png::Encoder::new(&mut out, TILE_SIZE as u32, TILE_SIZE as u32);
        enc.set_color(png::ColorType::Rgb);
        enc.set_depth(png::BitDepth::Eight);
        let mut w = enc.write_header().unwrap();
        w.write_image_data(&rgb).unwrap();
    }
    out
}

/// A 1:50 000 sheet of the synthetic terrain (20 m contours, brown on
/// white, 6.4 × 8.0 km) centred at `at`, and its "1:50 000" label.
fn synthetic_sheet(at: LatLon) -> (Rgb8, Value) {
    use rayon::prelude::*;
    let (w, h) = (1500usize, 1900usize);
    let scale = 50_000.0 * 0.0254 / 300.0; // ground m/px of 1:50 000 at 300 dpi
    let c = latlon_to_merc(at.lat, at.lon);
    let k = 1.0 / at.lat.to_radians().cos();
    let tr = Affine::pixel_to_merc_similarity(scale * k, 0.0, w as f64 / 2.0, h as f64 / 2.0, c.0, c.1);
    let lvl: Vec<i64> = (0..w * h)
        .into_par_iter()
        .map(|i| {
            let (mx, my) = tr.apply((i % w) as f64 + 0.5, (i / w) as f64 + 0.5);
            (noise_terrain(mx, my) / 20.0).floor() as i64
        })
        .collect();
    let mut data = vec![255u8; w * h * 3];
    for y in 0..h {
        for x in 0..w {
            let l = lvl[y * w + x];
            if (x + 1 < w && lvl[y * w + x + 1] != l) || (y + 1 < h && lvl[(y + 1) * w + x] != l) {
                for (dx, dy) in [(0usize, 0usize), (1, 0), (0, 1)] {
                    if x + dx < w && y + dy < h {
                        let i = ((y + dy) * w + x + dx) * 3;
                        data[i..i + 3].copy_from_slice(&[168, 120, 70]);
                    }
                }
            }
        }
    }
    let ocr = json!([{"text": "1:50 000", "bbox": [1200, 1850, 120, 25], "confidence": 0.9}]);
    (Rgb8::from_rgb(w, h, data).unwrap(), ocr)
}

/// The app's loop with synthetic terrain served for every tile asked for.
/// Returns the responses, the tiles served and a per-round log.
fn synthetic_loop(s: &Session, req: &Value, max_tiles: usize) -> (Vec<Value>, HashSet<TileId>, String) {
    use rayon::prelude::*;
    let t = Instant::now();
    let mut rs: Vec<Value> = Vec::new();
    let mut served: HashSet<TileId> = HashSet::new();
    let (mut core_s, mut tiles_s) = (0.0, 0.0);
    let mut rounds: Vec<String> = Vec::new();
    for _ in 0..80 {
        let tc = Instant::now();
        let r = call(s, req);
        core_s += tc.elapsed().as_secs_f64();
        let n = r["need_tiles"].as_array().map_or(0, |v| v.len());
        rounds.push(format!("{}:{n}", r["search"]["stage"].as_str().unwrap_or("-")));
        let need = r["status"] == "need_dem";
        if need {
            let tiles: Vec<TileId> = r["need_tiles"]
                .as_array()
                .unwrap()
                .iter()
                .map(|t| TileId {
                    z: t["z"].as_u64().unwrap() as u8,
                    x: t["x"].as_u64().unwrap() as u32,
                    y: t["y"].as_u64().unwrap() as u32,
                })
                .collect();
            assert!(!tiles.is_empty() && tiles.len() <= max_tiles, "{} tiles in one round", tiles.len());
            let tt = Instant::now();
            let pngs: Vec<(TileId, Vec<u8>)> = tiles.par_iter().map(|&t| (t, terrarium_png(t))).collect();
            tiles_s += tt.elapsed().as_secs_f64();
            for (t, png) in pngs {
                s.add_dem_tile(t, &png).unwrap();
                served.insert(t);
            }
        }
        rs.push(r);
        if !need {
            let log = format!(
                "{:.1} s total ({:.1} s core, {:.1} s making tiles), {} tiles served, {} rounds: {}",
                t.elapsed().as_secs_f64(),
                core_s,
                tiles_s,
                served.len(),
                rounds.len(),
                rounds.join(" ")
            );
            return (rs, served, log);
        }
    }
    panic!("still need_dem after 80 rounds");
}

/// A 1:50 000 sheet of synthetic terrain 120 km from the fix, R = 150 km:
/// the near area fails, the wide search (zoom 10, chunks nearest first,
/// tiles served on demand, at most 64 per round) finds it.
#[test]
fn synthetic_wide_search_finds_a_map_120_km_away() {
    let _g = serial();
    let fix = LatLon::new(46.5, 8.0);
    let at = destination(fix, 70.0, 120_000.0);
    let (img, ocr) = synthetic_sheet(at);
    let (w, h) = (img.w, img.h);
    let s = Session::new(img);
    let req = json!({"gps": gps(fix), "dpi": 300.0, "ocr": ocr, "methods": ["contours"],
                     "search_radius_m": 150_000.0});
    let (rs, served, log) = synthetic_loop(&s, &req, 64);
    let r = rs.last().unwrap();
    summary("synthetic wide 120 km", &rs, None);
    eprintln!("    {log}");
    assert_eq!(r["status"], "ok", "{}", r["message"]);
    let (gx, gy) = homography(r).apply(w as f64 / 2.0, h as f64 / 2.0);
    let (glat, glon) = merc_to_latlon(gx, gy);
    let off = mapme_core::geo::haversine_m(LatLon::new(glat, glon), at);
    assert!(off < 60.0, "map centre {off:.0} m off");
    assert!(r["confidence"].as_f64().unwrap() >= 0.7);
    assert_eq!(r["diagnostics"]["selected"]["source"], "wide", "{}", r["diagnostics"]["selected"]);
    let sp = &r["search"];
    assert_eq!(sp["coarse_zoom"], 10);
    assert_eq!(sp["radius_m"], 150_000.0);
    assert_eq!(sp["stage"], "done");
    let searched = sp["searched_radius_m"].as_f64().unwrap();
    assert!((50_000.0..=120_000.0).contains(&searched), "{sp}");
    assert!(sp["tiles_fetched"].as_u64().unwrap() <= sp["tiles_planned"].as_u64().unwrap());
    // The near area failed first; zoom-10 chunks were searched.
    let areas = r["diagnostics"]["search_areas"].as_array().unwrap();
    assert_eq!(areas.last().unwrap()["area"], "near");
    assert_ne!(areas.last().unwrap()["outcome"], "located");
    assert!(served.iter().any(|t| t.z == 10) && served.iter().any(|t| t.z == 13));
}

/// The same kind of sheet 260 km away while the radius is 200 km (zoom 9,
/// 6 × 6-tile chunks): every chunk is searched and the answer is an honest
/// failure saying so.
#[test]
fn synthetic_wide_search_outside_the_radius_fails_honestly() {
    let _g = serial();
    let fix = LatLon::new(46.5, 8.0);
    let (img, ocr) = synthetic_sheet(destination(fix, 250.0, 260_000.0));
    let s = Session::new(img);
    let req = json!({"gps": gps(fix), "dpi": 300.0, "ocr": ocr, "methods": ["contours"],
                     "search_radius_m": 200_000.0, "max_tiles_per_round": 36});
    let (rs, _, log) = synthetic_loop(&s, &req, 36);
    let r = rs.last().unwrap();
    summary("synthetic wide, map outside", &rs, None);
    eprintln!("    {log}");
    assert_eq!(r["status"], "failed", "{r}");
    assert!(r["pixel_to_mercator"].is_null());
    let m = r["message"].as_str().unwrap();
    assert!(m.starts_with("No match within 200 km of you (searched "), "{m}");
    let sp = &r["search"];
    assert_eq!(sp["coarse_zoom"], 9);
    assert_eq!(sp["searched_radius_m"], 200_000.0, "{sp}");
    assert_eq!(sp["stage"], "done");
    assert_eq!(sp["tiles_fetched"], sp["tiles_planned"], "{sp}");
    let wide = &r["diagnostics"]["wide"];
    assert_eq!(wide["chunks_searched"], wide["chunks"], "{wide}");
    for v in wide["refined"].as_array().unwrap() {
        assert_eq!(v["accepted"], false);
    }
}

/// One latitude label and the scale on a synthetic sheet 150 km from the
/// fix (north-east), R = 200 km: the label's band misses the near disc, so
/// only the band is searched (zoom 9), at a fraction of the disc's tiles.
#[test]
fn one_latitude_label_cuts_the_wide_search_to_a_band() {
    let _g = serial();
    let fix = LatLon::new(46.5, 8.0);
    let at = destination(fix, 50.0, 150_000.0);
    let (img, mut ocr) = synthetic_sheet(at);
    let (w, h) = (img.w, img.h);
    // The whole minute of latitude just south of the centre, labelled at its row.
    let lat_line = (at.lat * 60.0).floor() / 60.0;
    let scale = 50_000.0 * 0.0254 / 300.0;
    let k = 1.0 / at.lat.to_radians().cos();
    let row = h as f64 / 2.0 + (latlon_to_merc(at.lat, 0.0).1 - latlon_to_merc(lat_line, 0.0).1) / (scale * k);
    assert!((100.0..h as f64 - 100.0).contains(&row), "{row}");
    let (deg, min) = (lat_line.trunc(), ((lat_line - lat_line.trunc()) * 60.0).round());
    let text = format!("{deg:.0}°{min:02.0}'00\"N");
    ocr.as_array_mut().unwrap().push(json!({"text": text, "bbox": [20.0, row - 85.0, 30.0, 170.0], "confidence": 0.9}));
    let s = Session::new(img);
    let req = json!({"gps": gps(fix), "dpi": 300.0, "ocr": ocr, "methods": ["contours"],
                     "search_radius_m": 200_000.0});
    let (rs, served, log) = synthetic_loop(&s, &req, 64);
    let r = rs.last().unwrap();
    summary("latitude band", &rs, None);
    eprintln!("    {log}\n    band: {}", r["diagnostics"]["label_band"]);
    assert_eq!(r["status"], "ok", "{}", r["message"]);
    let (gx, gy) = homography(r).apply(w as f64 / 2.0, h as f64 / 2.0);
    let (glat, glon) = merc_to_latlon(gx, gy);
    assert!(mapme_core::geo::haversine_m(LatLon::new(glat, glon), at) < 60.0);
    let band = &r["diagnostics"]["label_band"];
    assert_eq!(band["name"], text.replace("00\"", "").as_str());
    let areas = r["diagnostics"]["search_areas"].as_array().unwrap();
    assert_eq!(areas[0]["outcome"], "outside the labels' band", "{areas:?}");
    let sp = &r["search"];
    assert_eq!(sp["coarse_zoom"], 9);
    // The full disc of 200 km at zoom 9 is ~100 coarse tiles plus the near
    // area's 81; the band needs a fraction.
    assert!(sp["tiles_planned"].as_u64().unwrap() < 60, "{sp}");
    assert!(served.iter().filter(|t| t.z == 11).count() < 40, "zoom-11 tiles: near area searched?");
}
