//! Searching beyond the near area: request hints, hints from the map's own
//! coordinate labels, `hint_queries`, the chunked wide search and its
//! progress, all hermetic (fixture DEM tiles, or synthetic terrain served
//! tile by tile like the app's need_dem loop). Run with `cargo test --release`.

mod common;
use common::*;
use mapme_core::dem::{tile_bounds, TileId};
use mapme_core::geo::{destination, latlon_to_merc, merc_to_latlon, Homography, LatLon};
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
    // The wrong hint in both scale bands (no scale label: the small-scale band
    // too, on zoom-8 tiles), then the right one, found in the near zoom's band.
    let areas = r["diagnostics"]["search_areas"].as_array().unwrap();
    assert_eq!(areas.len(), 3, "{areas:?}");
    for a in &areas[..2] {
        assert_eq!(a["name"], "Elsewhere");
        assert_ne!(a["outcome"], "located");
    }
    assert!(areas[0]["band"]["max"].as_f64().unwrap() <= 12.0 && areas[1]["band"]["min"].as_f64().unwrap() >= 12.0);
    assert_eq!(areas[2]["name"], "Grootkloof");
    assert_eq!(areas[2]["outcome"], "located");
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
    // The sheets' own fixtures, once committed: emulated OCR and Apple
    // Vision's (iOS 26.2 simulator).
    for (dir, want) in [
        ("castle-gorge", vec!["CASTLE GORGE", "MAGALIESBERG"]),
        ("mhlabatini", vec!["MHLABATINI", "MAGALIESBERG"]),
        ("grootkloof", vec!["GROOTKLOOF", "MAGALIESBERG"]),
    ] {
        for file in ["ocr.json", "vision-ocr.json"] {
            let p = fixtures.join(dir).join(file);
            if p.exists() && !(dir == "grootkloof" && file == "ocr.json") {
                // The emulated OCR garbles the titles ("CASTEEGORGE"): only
                // Vision's must name them.
                let want = if file == "ocr.json" { Vec::new() } else { want.clone() };
                sheets.push((Box::leak(format!("{dir} {file}").into_boxed_str()), ocr_file(p), want));
            }
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
    // Apple Vision's reads: the confirmed titles first, in this order, and none of the misreads,
    // pieces and furniture Vision reads around them.
    let vision: [(&str, &[&str]); 3] = [
        ("grootkloof", &["HAMERKOP", "GROOTKLOOF", "MAGALIESBERG"]),
        ("castle-gorge", &["CASTLE GORGE", "MAGALIESBERG"]),
        ("mhlabatini", &["MAGALIESBERG", "MHLABATINI", "FERN KLOOF"]),
    ];
    for (dir, first) in vision {
        let q = hint_queries(&ocr_file(fixtures.join(dir).join("vision-ocr.json")));
        assert_eq!(&q[..first.len()], first, "{dir}: {q:?}");
        for bad in [
            "DOTKLOOF",
            "KDO8PROPERTY",
            "ual Change per year West",
            "HTA FAHIN",
            "ARIG BEACON",
            "KEED RIGHT",
            "B KONG POOL",
            "D LONG POOL",
            "NAMI LODGE",
            "EASY GUL",
            "LABATINI",
        ] {
            assert!(!q.iter().any(|x| x == bad), "{dir}: {bad} in {q:?}");
        }
        // Then names of several words: no single word before the last of them.
        let rest = &q[first.len()..];
        let multi = rest.iter().rposition(|x| x.contains(' ')).unwrap_or(0);
        assert!(rest[..multi].iter().all(|x| x.contains(' ')), "{dir}: {q:?}");
    }
    for (name, obs, want) in sheets {
        let q = hint_queries(&obs);
        eprintln!("[hint_queries] {name}: {q:?}");
        assert!(!q.is_empty() && q.len() <= 12, "{name}: {q:?}");
        for w in &want {
            let pos = q.iter().position(|x| x.eq_ignore_ascii_case(w));
            assert!(pos.is_some(), "{name}: {w} missing from {q:?}");
        }
        // The title and the series name come first.
        for w in want.iter().take(2) {
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
    let out = serve(&s, &req, 36, 80, &noise_terrain, &mut |_| {});
    let (rs, log) = (&out.responses, &out.log);
    let r = rs.last().unwrap();
    summary("synthetic wide, map outside", rs, None);
    eprintln!("    {log}");
    // Memory: the zoom-9 tiles go once their chunks are searched; at most
    // two chunks' worth (one searched, the next fetched with its checks)
    // are held at any time, on top of the near area's and refinements'.
    let z9 = out.served.iter().filter(|t| t.z == 9).count();
    let other = out.served.len() - z9;
    let max_held = *out.held.iter().max().unwrap();
    eprintln!(
        "    held at most {max_held} tiles (served {z9} zoom-9, {other} others), {} at the end",
        out.held.last().unwrap()
    );
    assert!(*out.held.last().unwrap() <= other, "zoom-9 tiles left after the search");
    assert!(max_held <= other + 2 * 36, "{max_held} tiles held");
    assert_eq!(out.asked_twice(9), 0, "zoom-9 tiles asked for twice");
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
    repeat_is_free(&s, &req, r);
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
    // The near disc is searched all the same (a misread label must not hide
    // a map next to the user), then the band's chunks first.
    let areas = r["diagnostics"]["search_areas"].as_array().unwrap();
    assert_eq!(areas[0]["area"], "near", "{areas:?}");
    let sp = &r["search"];
    assert_eq!(sp["coarse_zoom"], 9);
    let wide = &r["diagnostics"]["wide"];
    let band_chunks = wide["band_chunks"].as_u64().unwrap();
    assert!(band_chunks > 0 && wide["chunks_searched"].as_u64().unwrap() <= band_chunks, "{wide}");
    // The full disc of 200 km at zoom 9 is ~100 coarse tiles; the band's
    // chunks need a fraction.
    let z9 = served.iter().filter(|t| t.z == 9).count();
    assert!(z9 < 30, "{z9} zoom-9 tiles served");
}

// ---------------------------------------------------------------------------
// Adversarial: chunk boundaries, the antimeridian and poles, cancel and
// resume, tile caps, hints that should not hide others, misread labels,
// small maps, decoys, memory and the cost of every call.
// ---------------------------------------------------------------------------

use common::synth::*;
use mapme_core::search::{map_half_extent, SearchParams};
use mapme_core::wide::{plan_wide, wide_reach, WidePlan};

/// The wide plan the core makes around `fix` for a `w` × `h` sheet whose
/// label gives 1:`denom` at 300 dpi (scale range as `georef` derives it).
fn plan_for(fix: LatLon, radius: f64, w: usize, h: usize, denom: f64) -> WidePlan {
    let prior = sheet_scale(denom);
    let (lo, hi) = (0.75 * prior * fix.lat.to_radians().cos(), 1.25 * prior);
    let (zoom, reach) = wide_reach(radius, hi, w, h, fix.lat).unwrap();
    let origin = latlon_to_merc(fix.lat, fix.lon);
    let base = SearchParams { center_merc: origin, scale_min: lo, scale_max: hi, ..SearchParams::default() };
    plan_wide(origin, reach, 50_000.0, zoom, map_half_extent(&base, w, h), 64, None)
}

/// Ground distance (m) of the located map centre from `at`.
fn centre_off_m(r: &Value, w: usize, h: usize, at: LatLon) -> f64 {
    let (gx, gy) = homography(r).apply(w as f64 / 2.0, h as f64 / 2.0);
    let (glat, glon) = merc_to_latlon(gx, gy);
    mapme_core::geo::haversine_m(LatLon::new(glat, glon), at)
}

/// Run a synthetic sheet at `at` from `fix` (R, tile cap) and check it is
/// found within 60 m with a confident answer; returns what the loop saw.
fn find_sheet(name: &str, fix: LatLon, at: LatLon, radius: f64, max_tiles: usize) -> Served {
    let (img, ocr) = synthetic_sheet(at);
    let (w, h) = (img.w, img.h);
    let s = Session::new(img);
    let req = json!({"gps": gps(fix), "dpi": 300.0, "ocr": ocr, "methods": ["contours"],
                     "search_radius_m": radius, "max_tiles_per_round": max_tiles});
    let out = serve(&s, &req, max_tiles, 400, &noise_terrain, &mut |_| {});
    let r = out.last();
    summary(name, &out.responses, None);
    eprintln!("    {}", out.log);
    assert_eq!(r["status"], "ok", "{name}: {}", r["message"]);
    let off = centre_off_m(r, w, h, at);
    assert!(off < 60.0, "{name}: map centre {off:.0} m off");
    assert!(r["confidence"].as_f64().unwrap() >= 0.7, "{name}: {}", r["confidence"]);
    out
}

/// A sheet centred on the corner where four wide chunks meet (its extent
/// spans all four) is found, as is one centred on a chunk edge.
#[test]
fn map_across_chunk_boundaries_is_found() {
    let _g = serial();
    let fix = LatLon::new(46.5, 8.0);
    let radius = 500_000.0;
    let plan = plan_for(fix, radius, 1500, 1900, 50_000.0);
    assert_eq!(plan.zoom, 9);
    // The interior corner (shared by four chunks) nearest the fix.
    let mut corners: Vec<((f64, f64), f64)> = Vec::new();
    for c in &plan.chunks {
        let p = (c.placement.min_x, c.placement.min_y);
        let shared = plan
            .chunks
            .iter()
            .filter(|d| {
                let b = &d.placement;
                [(b.min_x, b.min_y), (b.max_x, b.min_y), (b.min_x, b.max_y), (b.max_x, b.max_y)]
                    .iter()
                    .any(|q| (q.0 - p.0).abs() < 1.0 && (q.1 - p.1).abs() < 1.0)
            })
            .count();
        let (la, lo) = merc_to_latlon(p.0, p.1);
        let d = mapme_core::geo::haversine_m(fix, LatLon::new(la, lo));
        if shared == 4 && d > 60_000.0 {
            corners.push((p, d));
        }
    }
    corners.sort_by(|a, b| a.1.total_cmp(&b.1));
    let (corner, d) = *corners.first().expect("an interior chunk corner");
    eprintln!("[chunk corner] {:.0} km from the fix, {} chunks", d / 1000.0, plan.chunks.len());
    assert!((55_000.0..radius - 20_000.0).contains(&d), "corner {d:.0} m from the fix");
    let (la, lo) = merc_to_latlon(corner.0, corner.1);
    let out = find_sheet("chunk corner", fix, LatLon::new(la, lo), radius, 64);
    assert_eq!(out.last()["diagnostics"]["selected"]["source"], "wide");
    // An edge: the middle of the corner's vertical boundary to the north.
    let edge = (corner.0, corner.1 + 0.5 * tile_bounds(TileId { z: 9, x: 0, y: 0 }).height());
    let (la, lo) = merc_to_latlon(edge.0, edge.1);
    find_sheet("chunk edge", fix, LatLon::new(la, lo), radius, 64);
}

/// Sheets just inside and just outside the near disc (49.3 / 50.7 km): the
/// near search and the wide search meet without a gap.
#[test]
fn map_on_the_near_and_wide_boundary_is_found() {
    let _g = serial();
    let fix = LatLon::new(46.5, 8.0);
    for d in [49_300.0, 50_700.0] {
        let at = destination(fix, 200.0, d);
        let out = find_sheet(&format!("boundary {:.1} km", d / 1000.0), fix, at, 120_000.0, 64);
        let src = &out.last()["diagnostics"]["selected"]["source"];
        if d > 50_000.0 {
            assert_eq!(src, "wide", "{d}");
        }
    }
}

/// A fix 75 km west of the antimeridian (Fiji) and a sheet 25 km east of
/// it: the search reaches across, asks only for tiles inside the world, and
/// plans as many tiles as the same search away from it.
#[test]
fn search_across_the_antimeridian() {
    let _g = serial();
    let fix = LatLon::new(-17.8, 179.3);
    let at = LatLon::new(-17.7, -179.75);
    let d = mapme_core::geo::haversine_m(fix, at);
    assert!((90_000.0..115_000.0).contains(&d), "{d}");
    // Planned tiles near and away from the antimeridian (first call only).
    let planned = |lon: f64| {
        let (img, ocr) = synthetic_sheet(LatLon::new(-17.7, 0.0));
        let s = Session::new(img);
        let req = json!({"gps": gps(LatLon::new(-17.8, lon)), "dpi": 300.0, "ocr": ocr, "methods": ["contours"],
                         "search_radius_m": 1_000_000.0});
        let r = call(&s, &req);
        for t in tiles_of(&r) {
            assert!(t.x < 1 << t.z && t.y < 1 << t.z, "{t:?}");
        }
        r["search"]["tiles_planned"].as_u64().unwrap()
    };
    let (here, away) = (planned(179.3), planned(20.0));
    eprintln!("[antimeridian] tiles planned for 1 000 km: {here} at 179.3°E, {away} at 20°E");
    assert!(here as f64 > 0.9 * away as f64 && (here as f64) < 1.1 * away as f64, "{here} vs {away}");
    find_sheet("antimeridian", fix, at, 200_000.0, 64);
}

/// First call of a search around `fix`: (tiles the whole search plans,
/// tiles the near area needs) for a typical sheet without a scale label.
fn first_call_tiles(fix: LatLon, radius: f64) -> (u64, usize, Value) {
    let s = Session::new(map().clone());
    let req = json!({"gps": gps(fix), "methods": ["contours"], "search_radius_m": radius, "max_tiles_per_round": 4096});
    let r = call(&s, &req);
    for t in tiles_of(&r) {
        assert!(t.x < 1 << t.z && t.y < 1 << t.z, "{t:?}");
    }
    (r["search"]["tiles_planned"].as_u64().unwrap(), tiles_of(&r).len(), r)
}

/// Far north (Tromsø 69.6°N, Longyearbyen 78.2°N, 84°N) Mercator tiles
/// shrink on the ground: the near area and a 1 000 km search plan about as
/// many tiles as at 26°S (a few hundred, not tens of thousands), and the
/// estimate says the same.
#[test]
fn high_latitudes_plan_bounded_searches() {
    let estimate = |lat: f64, lon: f64| -> Value {
        serde_json::from_str(&mapme_core::wide::search_estimate_json(
            &json!({"radius_m": 1_000_000.0, "gps": {"lat": lat, "lon": lon}}).to_string(),
        ))
        .unwrap()
    };
    let (ref_wide, ref_near, _) = first_call_tiles(LatLon::new(-25.85, 27.4), 1_000_000.0);
    let ref_est = estimate(-25.85, 27.4)["tiles"].as_f64().unwrap();
    eprintln!("[latitude] 25.9°S: {ref_wide} tiles planned for 1 000 km, near area {ref_near}, estimate {ref_est}");
    for (lat, lon) in [(69.65, 18.96), (78.22, 15.65), (84.0, 20.0)] {
        let (wide, near, r) = first_call_tiles(LatLon::new(lat, lon), 1_000_000.0);
        let est = estimate(lat, lon);
        eprintln!(
            "[latitude] {lat}°N: {wide} tiles planned for 1 000 km, near area {near} (zoom {}), estimate {} tiles \
             {} MB",
            r["search"]["coarse_zoom"], est["tiles"], est["approx_megabytes"]
        );
        assert!((wide as f64) < 2.0 * ref_wide as f64, "{lat}: {wide} tiles planned vs {ref_wide}");
        assert!((near as f64) < 2.0 * ref_near as f64, "{lat}: near area {near} tiles vs {ref_near}");
        assert!(est["tiles"].as_f64().unwrap() < 2.0 * ref_est, "{lat}: {est}");
    }
}

/// A small sheet (1.5 × 1.9 km at 1:25 000) searched over 1 000 km: too
/// small for the wide search's zooms (zoom 11 over 1 000 km would be ~18 000
/// tiles), so only the near disc is searched, and the answer says so.
#[test]
fn small_map_wide_search_stays_bounded() {
    let _g = serial();
    let fix = LatLon::new(46.5, 8.0);
    let (img, ocr) = sheet_of(destination(fix, 10.0, 400_000.0), 700, 900, 25_000.0, &noise_terrain);
    let s = Session::new(img);
    let req = json!({"gps": gps(fix), "dpi": 300.0, "ocr": ocr, "methods": ["contours"],
                     "search_radius_m": 1_000_000.0});
    let first = call(&s, &req);
    eprintln!("[small map] first call: {}", first["search"]);
    let planned = first["search"]["tiles_planned"].as_u64().unwrap();
    assert!(planned < 200, "{planned} tiles planned");
    let out = serve(&s, &req, 64, 40, &noise_terrain, &mut |_| {});
    let r = out.last();
    summary("small map, 1 000 km", &out.responses, None);
    assert_eq!(r["status"], "failed");
    let m = r["message"].as_str().unwrap();
    assert!(m.starts_with("No match within 50 km of you"), "{m}");
    assert!(m.contains("too small an area to be searched farther than 50 km"), "{m}");
    assert!(!m.contains("Search a wider area"), "{m}");
    assert_eq!(r["search"]["radius_m"], 1_000_000.0);
    assert_eq!(r["search"]["searched_radius_m"], 50_000.0);
    assert!(out.served.iter().all(|t| t.z >= 11), "wide tiles served for a map too small to search wide");
}

/// The app answers every round with the same request: after an `ok` or a
/// `failed` the same call answers the same at once (nothing is searched or
/// asked for again).
fn repeat_is_free(s: &Session, req: &Value, last: &Value) {
    let t = Instant::now();
    let again = call(s, req);
    let secs = t.elapsed().as_secs_f64();
    eprintln!("    repeat call: {} in {secs:.2} s", again["status"]);
    assert_eq!(again["status"], last["status"]);
    assert_eq!(again["message"], last["message"]);
    assert!(tiles_of(&again).is_empty());
    assert_eq!(again["pixel_to_mercator"], last["pixel_to_mercator"]);
    assert!(secs * 1e3 < time_budget_ms(1500.0), "a repeated call took {secs:.2} s");
}

/// Cancel the wide search several times mid-chunk: every cancelled call
/// returns at once and the next continues where the last stopped (no chunk
/// searched twice, no coarse tile asked for twice), then finds the sheet.
#[test]
fn cancel_mid_wide_search_resumes() {
    let _g = serial();
    let fix = LatLon::new(46.5, 8.0);
    let at = destination(fix, 120.0, 170_000.0);
    let (img, ocr) = synthetic_sheet(at);
    let (w, h) = (img.w, img.h);
    let s = std::sync::Arc::new(Session::new(img));
    let req = json!({"gps": gps(fix), "dpi": 300.0, "ocr": ocr, "methods": ["contours"],
                     "search_radius_m": 250_000.0});
    let mut cancels = 0;
    let mut prev_stage = String::new();
    let mut statuses: Vec<String> = Vec::new();
    let mut rs: Vec<Value> = Vec::new();
    let mut asked: std::collections::HashMap<TileId, usize> = std::collections::HashMap::new();
    let delays = [40u64, 120, 250, 500, 800];
    for _ in 0..200 {
        // Cancel calls of the wide stage at growing delays.
        let canceller = (matches!(prev_stage.as_str(), "wide" | "refine") && cancels < delays.len()).then(|| {
            let ms = delays[cancels];
            cancels += 1;
            let s2 = s.clone();
            std::thread::spawn(move || {
                std::thread::sleep(std::time::Duration::from_millis(ms));
                s2.cancel();
            })
        });
        let r = call(&s, &req);
        if let Some(c) = canceller {
            c.join().unwrap();
        }
        prev_stage = r["search"]["stage"].as_str().unwrap_or("").to_string();
        let cancelled = r["diagnostics"]["cancelled"] == true;
        statuses.push(format!(
            "{}:{}{}",
            r["status"].as_str().unwrap(),
            prev_stage,
            if cancelled { format!("(cancelled {})", r["timings_ms"]) } else { String::new() }
        ));
        if cancelled {
            // Cancelled calls report no progress object: keep the stage.
            prev_stage = "wide".into();
        }
        let need = r["status"] == "need_dem";
        if need {
            for t in tiles_of(&r) {
                s.add_dem_tile(t, &terrarium_png(t)).unwrap();
                *asked.entry(t).or_insert(0) += 1;
            }
        }
        rs.push(r);
        if !need && !cancelled {
            break;
        }
    }
    let r = rs.last().unwrap();
    eprintln!("[cancel] {cancels} cancels: {}", statuses.join(" "));
    summary("cancel and resume", &rs, None);
    let n_cancelled = rs.iter().filter(|x| x["diagnostics"]["cancelled"] == true).count();
    assert!(n_cancelled >= 1, "no call was cancelled");
    assert_eq!(r["status"], "ok", "{}", r["message"]);
    assert!(centre_off_m(r, w, h, at) < 60.0);
    let twice: Vec<&TileId> = asked.iter().filter(|(t, n)| t.z == 9 && **n > 1).map(|(t, _)| t).collect();
    assert!(twice.is_empty(), "coarse tiles asked for again: {twice:?}");
    let wide = &r["diagnostics"]["wide"];
    let found_in = wide["refined"].as_array().unwrap().iter().find(|v| v["accepted"] == true).unwrap()["chunk"].clone();
    assert_eq!(wide["chunks_searched"].as_u64().unwrap(), found_in.as_u64().unwrap() + 1, "{wide}");
    repeat_is_free(&s, &req, r);
}

/// One tile per answer (and a cap of 0, read as 1): the search still
/// finishes, every answer holds one tile, and the per-call overhead of the
/// many calls stays small. A huge cap asks for a whole area at once.
#[test]
fn tile_cap_of_one_and_huge() {
    let _g = serial();
    let fix = LatLon::new(46.5, 8.0);
    let at = destination(fix, 30.0, 58_000.0);
    let out = find_sheet("one tile per round", fix, at, 70_000.0, 1);
    let rounds = out.responses.len();
    let core: f64 = out.core_s.iter().sum();
    let tiles = out.served.len();
    eprintln!("    {rounds} rounds for {tiles} tiles, {:.3} s core per call", core / rounds as f64);
    assert!(rounds <= tiles + 3, "{rounds} rounds for {tiles} tiles");
    // The calls that searched nothing new: median per-call time.
    let mut per: Vec<f64> = out.core_s.clone();
    per.sort_by(|a, b| a.total_cmp(b));
    let median = per[per.len() / 2];
    assert!(median < 0.25, "median call {median:.3} s");
    // A cap of 0 is read as 1; a huge cap is read as 4 096.
    let s = Session::new(map().clone());
    let r = call(&s, &json!({"gps": gps(fix), "methods": ["contours"], "max_tiles_per_round": 0}));
    assert_eq!(tiles_of(&r).len(), 1);
    let r = call(&s, &json!({"gps": gps(fix), "methods": ["contours"], "max_tiles_per_round": 1e12}));
    assert!(tiles_of(&r).len() > 1);
    assert_eq!(r["diagnostics"]["tiles_requested"], r["diagnostics"]["tiles_unknown"], "{}", r["diagnostics"]);
}

/// A hint with a huge radius (searched at 50 km at most) does not count as
/// covering the other hints: the right one after it is still searched.
#[test]
fn huge_hint_radius_does_not_hide_other_hints() {
    let _g = serial();
    let wrong = destination(centre(), 90.0, 60_000.0);
    let right = destination(centre(), 200.0, 4_000.0);
    let s = Session::new(map().clone());
    let req = json!({
        "dpi": 300.0, "methods": ["contours"],
        "hints": [{"lat": wrong.lat, "lon": wrong.lon, "radius_m": 5e6, "source": "user", "name": "Somewhere"},
                  {"lat": right.lat, "lon": right.lon, "name": "Grootkloof"}]
    });
    let (rs, _) = fixture_loop(&s, &req, 12);
    let r = rs.last().unwrap();
    summary("huge hint radius", &rs, None);
    assert_eq!(r["status"], "ok", "{}", r["message"]);
    assert!(max_error_m(&homography(r)) < 40.0);
    assert_eq!(r["search"]["hints_planned"], 2, "{}", r["search"]);
}

/// The grid fit fails (one latitude label left, misread by a degree: 26°
/// for 25°) and the GPS is on the sheet: the near area is still searched
/// (the band the label gives must not overrule the fix), as before the
/// wide search existed.
#[test]
fn misread_latitude_label_does_not_skip_the_near_area() {
    let _g = serial();
    let obs: Vec<OcrObservation> = ocr_fixture()
        .into_iter()
        .filter(|o| !(o.text.contains('°') || o.text.contains('\'')) || o.text == "25°51'00\"S")
        .map(|mut o| {
            if o.text == "25°51'00\"S" {
                o.text = "26°51'00\"S".into();
            }
            o
        })
        .collect();
    let s = Session::new(map().clone());
    let req = json!({"gps": gps(centre()), "dpi": 300.0, "ocr": obs});
    let (rs, _) = fixture_loop(&s, &req, 12);
    let r = rs.last().unwrap();
    summary("misread latitude label", &rs, None);
    eprintln!("    band: {}", r["diagnostics"]["label_band"]);
    assert_eq!(r["status"], "ok", "{}", r["message"]);
    assert!(max_error_m(&homography(r)) < 40.0);
}

/// Terrain with a decoy: around `decoy` (Mercator, 14 km, tapered) a blend
/// of `alpha` × the terrain under the true sheet at `truth` and (1 − alpha)
/// × an unrelated landscape; `noise_terrain` elsewhere.
fn decoy_terrain(truth: (f64, f64), decoy: (f64, f64), alpha: f64) -> impl Fn(f64, f64) -> f64 + Sync {
    move |mx: f64, my: f64| {
        let d = (mx - decoy.0).hypot(my - decoy.1);
        let wgt = if d < 10_000.0 {
            1.0
        } else if d < 14_000.0 {
            let t = (14_000.0 - d) / 4_000.0;
            t * t * (3.0 - 2.0 * t)
        } else {
            0.0
        };
        if wgt == 0.0 {
            return noise_terrain(mx, my);
        }
        let copy = noise_terrain(mx - decoy.0 + truth.0, my - decoy.1 + truth.1);
        let other = noise_terrain_seeded(mx, my, 5);
        let blend = alpha * copy + (1.0 - alpha) * other;
        wgt * blend + (1.0 - wgt) * noise_terrain(mx, my)
    }
}

/// Decoys: the sheet's terrain partly copied elsewhere (`alpha` of it),
/// with the true place outside the search. Searched through a place hint
/// (zoom 11) and by the wide search (zoom 9). Returns (status, confidence)
/// of both.
fn decoy_case(alpha: f64) -> [(String, f64); 2] {
    let fix = LatLon::new(46.5, 8.0);
    let truth_ll = destination(fix, 0.0, 900_000.0);
    let truth = latlon_to_merc(truth_ll.lat, truth_ll.lon);
    let decoy_ll = destination(fix, 100.0, 170_000.0);
    let decoy = latlon_to_merc(decoy_ll.lat, decoy_ll.lon);
    let terrain = decoy_terrain(truth, decoy, alpha);
    let (img, ocr) = sheet_of(truth_ll, 1500, 1900, 50_000.0, &noise_terrain);
    let mut out: [(String, f64); 2] = Default::default();
    for (i, hint) in [true, false].into_iter().enumerate() {
        let s = Session::new(img.clone());
        // Through a place hint alone (no fix), or the wide search from the fix.
        let mut req = json!({"dpi": 300.0, "ocr": ocr, "methods": ["contours"], "search_radius_m": 250_000.0});
        if hint {
            let h = destination(decoy_ll, 45.0, 3_000.0);
            req["hints"] = json!([{"lat": h.lat, "lon": h.lon, "name": "Decoy", "radius_m": 15000}]);
        } else {
            req["gps"] = gps(fix);
        }
        let got = serve(&s, &req, 64, 400, &terrain, &mut |_| {});
        let r = got.last();
        let name = format!("decoy alpha {alpha} via {}", if hint { "hint" } else { "wide" });
        summary(&name, &got.responses, None);
        eprintln!("    {}", got.log);
        let conf = r["confidence"].as_f64().unwrap_or(0.0);
        if r["status"] == "ok" {
            let off = centre_off_m(r, img.w, img.h, decoy_ll);
            eprintln!("    [{name}] located {off:.0} m from the decoy centre, confidence {conf}");
        }
        out[i] = (r["status"].as_str().unwrap().to_string(), conf);
    }
    out
}

/// A decoy holding 65 % of the sheet's terrain (its true place outside the
/// search): passes the wide search's re-check gate (ratio ~4.5) but its
/// refinement fits at ~0.0, so it is never accepted, through a place hint or
/// the wide search. (From ~80 % likeness the decoy fits at ±20 m and is
/// accepted: it is then the same landscape; `decoy_likeness`.)
#[test]
fn partial_decoy_is_not_accepted() {
    let _g = serial();
    let [hint, wide] = decoy_case(0.65);
    assert_eq!(hint.0, "failed", "decoy accepted through the hint: {hint:?}");
    assert_eq!(wide.0, "failed", "decoy accepted by the wide search: {wide:?}");
}

/// MEASUREMENT: decoys of growing likeness (`DECOY_ALPHAS`, default
/// 0.5,0.65,0.8). Run with --ignored --nocapture.
#[test]
#[ignore]
fn decoy_likeness() {
    let alphas: Vec<f64> = std::env::var("DECOY_ALPHAS")
        .unwrap_or_else(|_| "0.5,0.65,0.8".into())
        .split(',')
        .filter_map(|v| v.parse().ok())
        .collect();
    for a in alphas {
        let r = decoy_case(a);
        eprintln!("[decoy] alpha {a}: hint {:?}, wide {:?}", r[0], r[1]);
    }
}

/// Svalbard (78°N): the near area searched at zoom 9 and the wide search at
/// zoom 7 (the zooms with the ground detail of zoom 11 and 9 at low
/// latitudes) find a sheet 120 km from the fix end to end.
#[test]
fn far_north_search_finds_the_sheet() {
    let _g = serial();
    let fix = LatLon::new(78.22, 15.65);
    let at = destination(fix, 100.0, 120_000.0);
    let out = find_sheet("Svalbard 120 km", fix, at, 200_000.0, 64);
    let r = out.last();
    assert_eq!(r["diagnostics"]["selected"]["source"], "wide");
    assert_eq!(r["diagnostics"]["wide"]["zoom"], 7, "{}", r["diagnostics"]["wide"]);
    // The near area at zoom 9 (refinements may use zoom 11-13).
    assert_eq!(out.responses[0]["search"]["coarse_zoom"], 9, "{}", out.responses[0]["search"]);
    let zooms: std::collections::BTreeSet<u8> = out.served.iter().map(|t| t.z).collect();
    eprintln!("    zooms served: {zooms:?}, {} tiles", out.served.len());
    assert!(!zooms.contains(&10), "{zooms:?}");
    assert!(out.served.len() < 400, "{} tiles", out.served.len());
}

/// Offline beyond the fixture: every wide tile is marked missing. The search
/// runs through all chunks without terrain and does not claim "no match
/// within 300 km": most of the area went unsearched.
#[test]
fn offline_wide_search_says_what_was_missing() {
    let _g = serial();
    let s = Session::new(map().clone());
    let req = json!({"gps": gps(LatLon::new(-25.85, 26.85)), "dpi": 300.0, "methods": ["contours"],
                     "search_radius_m": 300_000.0});
    let (rs, _) = fixture_loop(&s, &req, 40);
    let r = rs.last().unwrap();
    summary("offline wide", &rs, None);
    assert_eq!(r["status"], "failed");
    let m = r["message"].as_str().unwrap();
    assert!(m.starts_with("Terrain for much of the search area couldn't be downloaded ("), "{m}");
    assert!(!m.contains("No match within"), "{m}");
}

// ---------------------------------------------------------------------------
// Scale priors are priors: a misread scale bar, a printed scale that the
// page's dpi turns into a wrong one, and the scale bands' reach
// ---------------------------------------------------------------------------

/// Clear the bottom 90 px of `img` to white (a legend band) and draw a 2 px
/// black scale bar `px` long in it, with its label read as `text` centred
/// over it: the OCR read.
fn with_scale_bar(img: &mut Rgb8, px: f64, text: &str) -> Value {
    let (w, h) = (img.w, img.h);
    for y in h - 90..h {
        for x in 0..w {
            let i = (y * w + x) * 3;
            img.data[i..i + 3].copy_from_slice(&[255, 255, 255]);
        }
    }
    let (x0, yb) = (w / 3, h - 30);
    let x1 = x0 + px.round() as usize;
    for y in yb..yb + 2 {
        for x in x0..x1 {
            let i = (y * w + x) * 3;
            img.data[i..i + 3].copy_from_slice(&[20, 20, 20]);
        }
    }
    let cx = (x0 + x1) as f64 / 2.0;
    json!({"text": text, "bbox": [cx - 25.0, (yb - 40) as f64, 50.0, 25.0], "confidence": 0.9})
}

/// Hint and near areas are searched once a session. Eight hints and a
/// scale bar at 12.3 m/px (its band and the default range each cut at
/// 12 m/px: four bands) make 37 hint and near searches; they outnumbered
/// the session's cache of 32 and were all redone on every round after them
/// (2.2–5.3 s each, 3.4× the core time of the same search with 6 hints).
/// Once the wide search runs, no round may run a hint or near search again.
#[test]
fn hint_and_near_searches_are_not_redone() {
    let _g = serial();
    let fix = LatLon::new(46.5, 8.0);
    // The sheet lies far outside the search: nothing is found.
    let (mut img, _) = synthetic_sheet(destination(fix, 0.0, 400_000.0));
    let bar = with_scale_bar(&mut img, 2000.0 / 12.3, "2 km");
    let hints: Vec<Value> = (0..8)
        .map(|k| {
            let p = destination(fix, 45.0 * k as f64, 30_000.0);
            json!({"lat": p.lat, "lon": p.lon, "radius_m": 15_000.0})
        })
        .collect();
    let s = Session::new(img);
    let req = json!({"gps": gps(fix), "ocr": [bar], "hints": hints, "methods": ["contours"],
                     "search_radius_m": 70_000.0});
    let out = serve(&s, &req, 64, 200, &noise_terrain, &mut |_| {});
    let r = out.last();
    summary("8 hints, a bar at 12.3 m/px, 70 km", &out.responses, None);
    eprintln!("    {}", out.log);
    assert_eq!(r["status"], "failed", "{}", r["message"]);
    let areas = r["diagnostics"]["search_areas"].as_array().unwrap();
    assert!(areas.len() > 32, "{} hint and near searches", areas.len());
    // The round that finishes the near area starts the wide search; every
    // round after it (the wide chunks, their refinements, the answer) finds
    // the hint and near searches done.
    let first = out.responses.iter().position(|r| r["search"]["stage"] == "wide").expect(&out.log);
    let after = &out.responses[first + 1..];
    assert!(after.len() >= 2, "{}", out.log);
    for (i, r) in after.iter().enumerate() {
        assert!(
            r["timings_ms"]["coarse_search"].is_null(),
            "round {} searched again: {}",
            first + 2 + i,
            r["timings_ms"]
        );
    }
}

/// A 1:50 000 sheet 120 km from the fix whose "2 km" bar was read as
/// "2 mi" (a bar 1.6 times too long on the ground): the bar's scale is a
/// prior, not a constraint, in the wide search too (as in the hint and near
/// areas), so the map is still found.
#[test]
fn misread_scale_bar_does_not_hide_the_map_from_the_wide_search() {
    let _g = serial();
    let fix = LatLon::new(46.5, 8.0);
    let at = destination(fix, 70.0, 120_000.0);
    let (mut img, _) = synthetic_sheet(at);
    let bar = with_scale_bar(&mut img, 2000.0 / sheet_scale(50_000.0), "2 mi");
    let (w, h) = (img.w, img.h);
    let s = Session::new(img);
    let req = json!({"gps": gps(fix), "ocr": [bar], "methods": ["contours"], "search_radius_m": 150_000.0});
    let out = serve(&s, &req, 64, 120, &noise_terrain, &mut |_| {});
    let r = out.last();
    summary("misread bar, wide 120 km", &out.responses, None);
    eprintln!("    {}", out.log);
    let sb = &r["diagnostics"]["scale_bar"];
    assert!((sb["m_per_px"].as_f64().unwrap() / sheet_scale(50_000.0) - 1.609).abs() < 0.02, "{sb}");
    assert_eq!(r["status"], "ok", "{}", r["message"]);
    assert!(centre_off_m(r, w, h, at) < 60.0);
    assert_eq!(r["diagnostics"]["selected"]["source"], "wide", "{}", r["diagnostics"]["selected"]);
}

/// A small screen map (380 × 380 px at 20 m/px, 7.6 km across) 250 km from
/// the fix, no scale prior: too small at the near zoom's scales (up to
/// 12 m/px) for the wide search past 150 km, but at the coarse band's
/// scales it is searched out to the whole radius, and found there; no wide
/// tile is fetched for placements no band searches.
#[test]
fn coarse_scale_band_reaches_past_the_fine_band() {
    let _g = serial();
    let fix = LatLon::new(46.5, 8.0);
    let at = destination(fix, 200.0, 250_000.0);
    let denom = 20.0 * 300.0 / 0.0254;
    let (img, _) = sheet_of(at, 380, 380, denom, &noise_terrain);
    let s = Session::new(img);
    let req = json!({"gps": gps(fix), "methods": ["contours"], "search_radius_m": 300_000.0});
    let out = serve(&s, &req, 64, 200, &noise_terrain, &mut |_| {});
    let r = out.last();
    summary("small coarse map, wide 250 km", &out.responses, None);
    eprintln!("    {}", out.log);
    assert_eq!(r["status"], "ok", "{}", r["message"]);
    assert!(centre_off_m(r, 380, 380, at) < 100.0);
    assert_eq!(r["diagnostics"]["wide"]["radius_m"], 300_000.0, "{}", r["diagnostics"]["wide"]);
}

/// A 1:50 000 sheet whose page says "1:25 000" (a sheet printed reduced to
/// fit the page, or a misread label): the printed scale and the dpi put the
/// map at half its size, but its scale bar, measured in the image, says
/// otherwise; the bar's scale is searched too, and the map is found.
#[test]
fn scale_bar_overrules_a_printed_scale_that_disagrees() {
    let _g = serial();
    let at = LatLon::new(46.5, 8.0);
    let (mut img, _) = synthetic_sheet(at);
    let bar = with_scale_bar(&mut img, 2000.0 / sheet_scale(50_000.0), "2 km");
    let (w, h) = (img.w, img.h);
    let label = json!({"text": "1:25 000", "bbox": [1200.0, 1820.0, 120.0, 25.0], "confidence": 0.9});
    let s = Session::new(img);
    let req = json!({"gps": gps(destination(at, 30.0, 8_000.0)), "dpi": 300.0, "ocr": [label, bar],
                     "methods": ["contours"]});
    let out = serve(&s, &req, 64, 60, &noise_terrain, &mut |_| {});
    let r = out.last();
    summary("1:25 000 printed on a 1:50 000 sheet with its bar", &out.responses, None);
    assert_eq!(r["diagnostics"]["scale_denominator"], 25_000.0);
    assert_eq!(r["status"], "ok", "{}", r["message"]);
    assert!(centre_off_m(r, w, h, at) < 60.0);
}

/// A bar label read as "2 m" (for "2 mi" or "2 km"): a bar implying a few
/// millimetres per pixel is a misread, not a prior; the default range finds
/// the sheet as if there were no bar.
#[test]
fn implausible_scale_bar_is_ignored() {
    let _g = serial();
    let at = LatLon::new(46.5, 8.0);
    let (mut img, _) = synthetic_sheet(at);
    let bar = with_scale_bar(&mut img, 2000.0 / sheet_scale(50_000.0), "2 m");
    let (w, h) = (img.w, img.h);
    let s = Session::new(img);
    let req = json!({"gps": gps(destination(at, 30.0, 8_000.0)), "ocr": [bar], "methods": ["contours"]});
    let out = serve(&s, &req, 64, 60, &noise_terrain, &mut |_| {});
    let r = out.last();
    summary("bar read as 2 m", &out.responses, None);
    assert!(r["diagnostics"]["scale_bar"].is_null(), "{}", r["diagnostics"]["scale_bar"]);
    assert_eq!(r["diagnostics"]["scale_range"]["source"], "default");
    assert_eq!(r["status"], "ok", "{}", r["message"]);
    assert!(centre_off_m(r, w, h, at) < 60.0);
}

/// Brown strokes 60–120 px long at random angles on white paper: lines
/// that do not nest like contour lines (a photo's edges, hachures).
fn strokes_image(w: usize, h: usize, seed: u64) -> Rgb8 {
    let mut rng = Rng::new(seed);
    let mut data = vec![255u8; w * h * 3];
    for _ in 0..(w * h / 3000) {
        let (x0, y0) = (rng.next_f64() * w as f64, rng.next_f64() * h as f64);
        let a = rng.next_f64() * std::f64::consts::PI;
        let len = 60.0 + 60.0 * rng.next_f64();
        for i in 0..(len * 2.0) as usize {
            let t = i as f64 / 2.0;
            for d in [-0.5, 0.5] {
                let (x, y) = (x0 + t * a.cos() - d * a.sin(), y0 + t * a.sin() + d * a.cos());
                if x >= 0.0 && y >= 0.0 && (x as usize) < w && (y as usize) < h {
                    let k = (y as usize * w + x as usize) * 3;
                    data[k..k + 3].copy_from_slice(&[150, 100, 60]);
                }
            }
        }
    }
    Rgb8::from_rgb(w, h, data).unwrap()
}

/// An image whose lines do not nest like contour lines, with no scale on
/// it: with nothing in the inner 20 km the search stops there and says why,
/// and searches no wider even with a 200 km radius (no zoom-9 or -10 tile
/// is asked for).
#[test]
fn lines_unlike_contours_stop_the_search_at_20_km() {
    let _g = serial();
    let fix = LatLon::new(46.5, 8.0);
    let s = Session::new(strokes_image(1600, 1200, 9));
    let req = json!({"gps": gps(fix), "methods": ["contours"], "search_radius_m": 200_000.0});
    let out = serve(&s, &req, 64, 40, &noise_terrain, &mut |_| {});
    let r = out.last();
    summary("strokes at random angles, R 200 km", &out.responses, None);
    eprintln!("    {}", out.log);
    let c = &r["diagnostics"]["contours"];
    assert!(c["n_segments"].as_u64().unwrap() > 50, "{c}");
    assert!(c["organisation"]["parallel"].as_f64().unwrap() < 0.4, "{c}");
    assert_eq!(r["status"], "failed", "{r}");
    let m = r["message"].as_str().unwrap();
    assert!(m.starts_with("No match within 20 km of you (searched ") && m.contains("don't run like contour"), "{m}");
    assert_eq!(r["search"]["searched_radius_m"], 20_000.0, "{}", r["search"]);
    assert!(out.served.iter().all(|t| t.z >= 11), "tiles beyond the inner disc asked for");
    assert_eq!(r["diagnostics"]["search_stopped"]["reason"], "lines unlike contours");
}
