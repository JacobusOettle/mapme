//! Two more sheets of the Grootkloof series, MCSA Magaliesberg 1:25 000
//! Sheet 07 Castle Gorge and Sheet 02 Mhlabatini (fixtures/castle-gorge,
//! fixtures/mhlabatini). The layout is Grootkloof's (graticule labels printed
//! inside the frame over the map, legend box and logo bottom-left, credit
//! along the bottom); what differs is around the labels: Castle Gorge's
//! 27°34' label runs into the "GIS V1 Sept 2017 - Eras Gous" credit,
//! Mhlabatini's 27°18' meridian lies on the frame line and its 25°52' line
//! under the legend box, and both have long vertical green vegetation
//! hatching.
//!
//! Every test drives `Session` the way the app does (request → `need_dem` →
//! add the fixture's tiles or mark them missing → call again) and scores the
//! answer at the true graticule intersections (truth.json, measured
//! independently of the core). `ocr.json` is unedited RapidOCR output for
//! the app's OCR plan (fixtures/tools/ocr_like_app.py), a stand-in for Apple
//! Vision (Grootkloof's is hand-built and clean); the phone-OCR tests build
//! what Vision is likely to return from the printed labels instead. The
//! grid tests read the labels through `labels::coordinate_readings`, which
//! repairs the garbled reads (' read as / or 1, " as ~, * or /, ° as 9, -
//! or lost; `cargo test --release --test sheets -- --nocapture` prints what
//! the labels parse to and what the core answers). Run with
//! `cargo test --release`.

mod common;
use common::*;
use mapme_core::dem::TileId;
use mapme_core::geo::Homography;
use mapme_core::georef::{GeorefRequest, GeorefResponse, GpsFix, Method, Session, Status};
use mapme_core::image::Rgb8;
use mapme_core::labels::{parse_coordinate, CoordValue, OcrObservation, PoiKind};
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};

/// Heavy tests run one at a time (each holds a 3507×4960 session).
fn serial() -> std::sync::MutexGuard<'static, ()> {
    static M: Mutex<()> = Mutex::new(());
    M.lock().unwrap_or_else(|e| e.into_inner())
}

/// Grid fits must hit the graticule intersections this closely (ground metres).
const GRID_MAX_ERR_M: f64 = 5.0;
/// Contour fits likewise.
const CONTOUR_MAX_ERR_M: f64 = 30.0;

/// The phone-OCR degradation of a sheet's printed graticule labels.
struct PhoneOcr {
    /// Latitude / longitude labels (indices into truth.json's lines) that survive.
    keep_lat: [usize; 2],
    keep_lon: [usize; 2],
    /// The surviving longitude label (index into truth.json's lon_lines) read
    /// as one line with the "GIS V1 Sept 2017- Eras Gous" credit next to it.
    merged_lon: usize,
}

struct Sheet {
    dir: &'static str,
    /// Centre of the map frame (lat, lon).
    centre: (f64, f64),
    /// 10 km from the centre: inside the 20 km search radius.
    near: (f64, f64),
    /// 60 km due east of the centre: the map is outside the search radius.
    far: (f64, f64),
    phone: PhoneOcr,
}

const CASTLE_GORGE: Sheet = Sheet {
    dir: "castle-gorge",
    centre: (-25.8153, 27.5926),
    near: (-25.7516, 27.6631), // 7.1 km north, 7.1 km east
    far: (-25.8153, 28.1913),
    // Keep 25°48' and 25°50' (of 25°47'…25°51'), 27°34' and 27°36' (of 27°34'…27°37').
    phone: PhoneOcr { keep_lat: [1, 3], keep_lon: [0, 2], merged_lon: 0 },
};

const MHLABATINI: Sheet = Sheet {
    dir: "mhlabatini",
    centre: (-25.8288, 27.3311),
    near: (-25.8926, 27.2605), // 7.1 km south, 7.1 km west
    far: (-25.8288, 27.9299),
    // Keep 25°49' and 25°51' (of 25°48'…25°52'), 27°19' and 27°21' (of 27°18'…27°21').
    phone: PhoneOcr { keep_lat: [1, 3], keep_lon: [1, 3], merged_lon: 1 },
};

impl Sheet {
    fn path(&self) -> PathBuf {
        sheet_dir(self.dir)
    }

    fn map(&self) -> Rgb8 {
        static MAPS: OnceLock<Mutex<Vec<(&'static str, Rgb8)>>> = OnceLock::new();
        let mut maps = MAPS.get_or_init(|| Mutex::new(Vec::new())).lock().unwrap_or_else(|e| e.into_inner());
        if let Some((_, m)) = maps.iter().find(|(d, _)| *d == self.dir) {
            return m.clone();
        }
        let m = load_jpeg_file(&self.path().join("map.jpg"));
        maps.push((self.dir, m.clone()));
        m
    }

    fn truth(&self) -> Vec<GraticulePoint> {
        graticule_truth(&self.path())
    }

    fn truth_json(&self) -> serde_json::Value {
        serde_json::from_slice(&std::fs::read(self.path().join("truth.json")).unwrap()).unwrap()
    }

    /// The fixture OCR (RapidOCR stand-in for Apple Vision).
    fn ocr(&self) -> Vec<OcrObservation> {
        load_ocr(&self.path())
    }
}

fn gps((lat, lon): (f64, f64)) -> Option<GpsFix> {
    Some(GpsFix { lat, lon, accuracy_m: Some(10.0) })
}

/// Contour search radius of these tests (the core's default when they were
/// written; it is 50 km now): the fixture DEM covers the searches it implies,
/// and `far` (60 km) stays outside it either way.
const SEARCH_RADIUS_M: f64 = 20_000.0;

/// What the app sends for a PDF page rendered at 300 dpi.
fn request(ocr: Vec<OcrObservation>, at: (f64, f64), methods: &[&str]) -> GeorefRequest {
    GeorefRequest {
        gps: gps(at),
        ocr,
        dpi: Some(300.0),
        methods: methods.iter().map(|m| m.to_string()).collect(),
        search_radius_m: SEARCH_RADIUS_M,
        ..GeorefRequest::default()
    }
}

/// The app's DEM loop: tiles from `dem` when there, else marked missing
/// (offline). `dem: None` is a phone without any terrain data.
fn run(sheet: &Sheet, req: &GeorefRequest, dem: Option<&Path>) -> GeorefResponse {
    run_map(sheet, sheet.map(), req, dem)
}

/// `run` on another raster of the sheet.
fn run_map(sheet: &Sheet, map: Rgb8, req: &GeorefRequest, dem: Option<&Path>) -> GeorefResponse {
    let session = Session::new(map);
    let (mut added, mut missing) = (Vec::new(), Vec::new());
    for _ in 0..8 {
        let r = session.georeference(req);
        if r.status != Status::NeedDem {
            let zooms = |v: &[TileId]| {
                let mut z: Vec<String> = v.iter().map(|t| t.z.to_string()).collect();
                z.dedup();
                z.join("/")
            };
            println!(
                "[{}] DEM tiles: {} added (zoom {}), {} marked missing (zoom {})",
                sheet.dir,
                added.len(),
                zooms(&added),
                missing.len(),
                zooms(&missing)
            );
            return r;
        }
        assert!(!r.need_tiles.is_empty());
        for &t in &r.need_tiles {
            match dem.and_then(|d| std::fs::read(d.join(format!("{}_{}_{}.png", t.z, t.x, t.y))).ok()) {
                Some(png) => {
                    session.add_dem_tile(t, &png).unwrap();
                    added.push(t);
                }
                None => {
                    session.mark_dem_tile_missing(t).unwrap();
                    missing.push(t);
                }
            }
        }
    }
    panic!("still need_dem after 8 rounds");
}

/// Worst ground error (m) of the response at the true graticule intersections.
fn error_m(sheet: &Sheet, r: &GeorefResponse) -> f64 {
    let Some(m) = r.pixel_to_mercator else { return f64::INFINITY };
    max_graticule_error_m(&Homography { m }, &sheet.truth())
}

fn report(name: &str, sheet: &Sheet, r: &GeorefResponse) -> f64 {
    let err = error_m(sheet, r);
    let d = &r.diagnostics;
    println!(
        "[{} {name}] {:?} {:?} conf {:?} rms {:?} → max error {err:.1} m at the graticule intersections: {}",
        sheet.dir, r.status, r.method, r.confidence, r.rms_m, r.message
    );
    for k in ["grid", "scale_range", "selected", "best_confidence", "candidates", "spot_heights_off_dem"] {
        if let Some(v) = d.get(k) {
            println!("    {k}: {v}");
        }
    }
    let pois: Vec<String> = r.pois.iter().map(|p| format!("{}({:?})", p.name, p.kind)).collect();
    println!("    {} POIs: {}", pois.len(), pois.join(", "));
    err
}

/// The observations the label parser reads as a latitude or longitude, and the
/// graticule label reads it rejects (left label strip, bottom label row).
fn print_coordinate_labels(ocr: &[OcrObservation]) {
    for o in ocr {
        let (cx, cy) = o.center();
        match parse_coordinate(&o.text) {
            Some(c @ (CoordValue::Lat(_) | CoordValue::Lon(_))) => {
                println!("    parsed   {:?} at ({cx:.0}, {cy:.0}): {c:?}", o.text)
            }
            _ if cx < 200.0 || cy > 4790.0 => println!("    rejected {:?} at ({cx:.0}, {cy:.0})", o.text),
            _ => {}
        }
    }
}

// ---------------------------------------------------------------------------
// Phone OCR of the printed graticule labels
// ---------------------------------------------------------------------------

fn dms(v: f64, hemi: char) -> String {
    let a = v.abs();
    let (d, m) = (a.trunc(), ((a - a.trunc()) * 60.0).round());
    format!("{d:.0}°{m:02.0}'00\"{hemi}")
}

/// The printed labels, as a clean read gives them: latitudes vertical just
/// inside the left frame and centred on their line, longitudes centred on
/// theirs along the bottom (boxes measured on map.jpg). Returns (lat, lon).
fn printed_labels(sheet: &Sheet) -> (Vec<OcrObservation>, Vec<OcrObservation>) {
    let t = sheet.truth_json();
    let obs = |text: String, bbox: [f64; 4]| OcrObservation { text, bbox, confidence: 0.9 };
    let lat = t["lat_lines"]
        .as_array()
        .unwrap()
        .iter()
        .map(|l| obs(dms(l["lat"].as_f64().unwrap(), 'S'), [121.0, l["y"].as_f64().unwrap() - 80.0, 37.0, 160.0]))
        .collect();
    let lon = t["lon_lines"]
        .as_array()
        .unwrap()
        .iter()
        .map(|l| obs(dms(l["lon"].as_f64().unwrap(), 'E'), [l["x"].as_f64().unwrap() - 79.0, 4803.0, 158.0, 31.0]))
        .collect();
    (lat, lon)
}

/// The imprint along the bottom edge, x 436…773 on every sheet of the series.
const CREDIT: &str = "GIS V1 Sept 2017- Eras Gous";
const CREDIT_BBOX: [f64; 4] = [436.0, 4806.0, 337.0, 28.0];

/// How the phone OCR degrades the labels: half of them are always dropped
/// (2 per axis survive, the rest read cleanly), then optionally:
#[derive(Clone, Copy)]
struct Degrade {
    /// The marks garbled in every survivor (° → *, ' → /, " → ~; each mark on both axes).
    garble: bool,
    /// One surviving longitude label read as one line with the credit next to it.
    merge: bool,
}

/// The fixture OCR without its reads of the graticule labels and the
/// credit (left label strip and bottom label row), plus the phone-OCR
/// reads of the surviving labels.
fn phone_ocr(sheet: &Sheet, how: Degrade) -> Vec<OcrObservation> {
    let mut out: Vec<OcrObservation> = sheet
        .ocr()
        .into_iter()
        .filter(|o| {
            let (cx, cy) = o.center();
            cx >= 200.0 && cy <= 4790.0
        })
        .collect();
    let (lat, lon) = printed_labels(sheet);
    let p = &sheet.phone;
    // Each mark is garbled on both axes: ° → * , ' → / and " → ~.
    let garble =
        |s: &str, marks: &[(char, char)]| marks.iter().fold(s.to_string(), |s, &(a, b)| s.replace(a, &b.to_string()));
    let lat_marks: [&[(char, char)]; 2] = [&[('°', '*')], &[('\'', '/'), ('"', '~')]];
    let lon_marks: [&[(char, char)]; 2] = [&[('°', '*'), ('\'', '/')], &[('"', '~')]];
    for (k, &i) in p.keep_lat.iter().enumerate() {
        let mut o = lat[i].clone();
        if how.garble {
            o.text = garble(&o.text, lat_marks[k]);
        }
        out.push(o);
    }
    for (k, &i) in p.keep_lon.iter().enumerate() {
        let mut o = lon[i].clone();
        if how.garble {
            o.text = garble(&o.text, lon_marks[k]);
        }
        if how.merge && i == p.merged_lon {
            let [x, y, w, h] = o.bbox;
            let [cx, cy, cw, ch] = CREDIT_BBOX;
            let (x0, y0) = (x.min(cx), y.min(cy));
            let (x1, y1) = ((x + w).max(cx + cw), (y + h).max(cy + ch));
            o.text = if x < cx { format!("{} {CREDIT}", o.text) } else { format!("{CREDIT} {}", o.text) };
            o.bbox = [x0, y0, x1 - x0, y1 - y0];
        }
        out.push(o);
    }
    if !how.merge {
        out.push(OcrObservation { text: CREDIT.into(), bbox: CREDIT_BBOX, confidence: 0.7 });
    }
    out
}

// ---------------------------------------------------------------------------
// Checks shared by both sheets
// ---------------------------------------------------------------------------

fn check_fixture(sheet: &Sheet) {
    let m = sheet.map();
    assert_eq!((m.w, m.h), (3507, 4960));
    let t = sheet.truth_json();
    let (lat, lon) = (t["lat_lines"].as_array().unwrap(), t["lon_lines"].as_array().unwrap());
    assert_eq!((lat.len(), lon.len()), (5, 4));
    // 1' of latitude ≈ 973.5 px, 1' of longitude ≈ 876.3 px (1:25 000 Web Mercator at 300 dpi).
    for w in lat.windows(2) {
        let dy = w[1]["y"].as_f64().unwrap() - w[0]["y"].as_f64().unwrap();
        let dlat = (w[0]["lat"].as_f64().unwrap() - w[1]["lat"].as_f64().unwrap()) * 60.0;
        assert!((dlat - 1.0).abs() < 1e-9 && (dy - 973.6).abs() < 0.5, "{dy} px for {dlat}'");
    }
    for w in lon.windows(2) {
        let dx = w[1]["x"].as_f64().unwrap() - w[0]["x"].as_f64().unwrap();
        let dlon = (w[1]["lon"].as_f64().unwrap() - w[0]["lon"].as_f64().unwrap()) * 60.0;
        assert!((dlon - 1.0).abs() < 1e-9 && (dx - 876.3).abs() < 0.3, "{dx} px for {dlon}'");
    }
    // The centre used by the tests is the centre of the map frame.
    let pts = sheet.truth();
    let (x0, y0) = pts[0].0;
    let (x1, y1) = pts[pts.len() - 1].0;
    let (la0, lo0) = pts[0].1;
    let (la1, lo1) = pts[pts.len() - 1].1;
    let f = t["map_frame_px"].as_array().unwrap().iter().map(|v| v.as_f64().unwrap()).collect::<Vec<_>>();
    let (cx, cy) = ((f[0] + f[2]) / 2.0, (f[1] + f[3]) / 2.0);
    let lat_c = la0 + (la1 - la0) * (cy - y0) / (y1 - y0);
    let lon_c = lo0 + (lo1 - lo0) * (cx - x0) / (x1 - x0);
    assert!((lat_c - sheet.centre.0).abs() < 2e-4 && (lon_c - sheet.centre.1).abs() < 2e-4, "{lat_c} {lon_c}");
    assert!(sheet.ocr().len() > 100);
    let tiles = std::fs::read_dir(sheet.path().join("dem")).unwrap().count();
    assert!(tiles > 40, "{tiles} DEM tiles");
}

/// (1) Fixture OCR, the user 60 km away and offline: the grid alone.
fn grid_far_offline(sheet: &Sheet) {
    let _g = serial();
    print_coordinate_labels(&sheet.ocr());
    let r = run(sheet, &request(sheet.ocr(), sheet.far, &["grid", "contours"]), None);
    let err = report("grid, GPS 60 km away, no DEM", sheet, &r);
    assert_eq!(r.status, Status::Ok, "{}", r.message);
    assert_eq!(r.method, Some(Method::Grid), "{}", r.message);
    assert!(err <= GRID_MAX_ERR_M, "max error {err:.1} m");
}

/// (2), (3) Contours alone (the OCR only gives the 1:25 000 scale label).
fn contours_at(sheet: &Sheet, at: (f64, f64), what: &str) {
    let _g = serial();
    let r = run(sheet, &request(sheet.ocr(), at, &["contours"]), Some(&sheet.path().join("dem")));
    let err = report(what, sheet, &r);
    assert_eq!(r.status, Status::Ok, "{}", r.message);
    assert_eq!(r.method, Some(Method::Contours));
    assert!(err <= CONTOUR_MAX_ERR_M, "max error {err:.1} m");
    // The spot heights were checked against the terrain the match used.
    assert!(r.diagnostics.get("spot_heights_off_dem").is_some(), "{}", r.diagnostics);
}

/// (4) Contours with the map outside the search radius: no answer, not a wrong one.
/// The fixture has zoom-11 tiles out there but no zoom-13 ones, so the coarse
/// candidates are refined on the zoom-11 fallback (with the zoom-13 tiles under
/// today's candidates added, every fit confidence was 0 too).
fn contours_far_fails(sheet: &Sheet) {
    let _g = serial();
    let r = run(sheet, &request(sheet.ocr(), sheet.far, &["contours"]), Some(&sheet.path().join("dem")));
    let err = report("contours, GPS 60 km away", sheet, &r);
    assert_eq!(r.status, Status::Failed, "confident wrong answer {err:.0} m off: {}", r.message);
}

/// (5) Phone OCR of the labels, the user 60 km away and offline.
fn grid_phone(sheet: &Sheet, how: Degrade, what: &str) {
    let _g = serial();
    let ocr = phone_ocr(sheet, how);
    print_coordinate_labels(&ocr);
    let r = run(sheet, &request(ocr, sheet.far, &["grid", "contours"]), None);
    let err = report(what, sheet, &r);
    assert_eq!(r.status, Status::Ok, "{}", r.message);
    assert_eq!(r.method, Some(Method::Grid), "{}", r.message);
    assert!(err <= GRID_MAX_ERR_M, "max error {err:.1} m");
}

// ---------------------------------------------------------------------------
// Castle Gorge (Sheet 07)
// ---------------------------------------------------------------------------

#[test]
fn castle_gorge_fixture_is_consistent() {
    check_fixture(&CASTLE_GORGE);
}

#[test]
fn castle_gorge_grid_far_gps_offline() {
    grid_far_offline(&CASTLE_GORGE);
}

#[test]
fn castle_gorge_contours_gps_at_centre() {
    contours_at(&CASTLE_GORGE, CASTLE_GORGE.centre, "contours, GPS at the centre");
}

#[test]
fn castle_gorge_contours_gps_10km_off_centre() {
    contours_at(&CASTLE_GORGE, CASTLE_GORGE.near, "contours, GPS 10 km off centre");
}

#[test]
fn castle_gorge_contours_gps_60km_away_fails() {
    contours_far_fails(&CASTLE_GORGE);
}

#[test]
fn castle_gorge_grid_phone_ocr_half_the_labels() {
    grid_phone(&CASTLE_GORGE, Degrade { garble: false, merge: false }, "grid, phone OCR: half the labels");
}

#[test]
fn castle_gorge_grid_phone_ocr_garbled_marks() {
    grid_phone(
        &CASTLE_GORGE,
        Degrade { garble: true, merge: false },
        "grid, phone OCR: half the labels, marks garbled",
    );
}

#[test]
fn castle_gorge_grid_phone_ocr_label_merged_with_credit() {
    grid_phone(
        &CASTLE_GORGE,
        Degrade { garble: false, merge: true },
        "grid, phone OCR: half the labels, 27°34' merged with the credit",
    );
}

#[test]
fn castle_gorge_grid_phone_ocr_garbled_and_merged() {
    grid_phone(
        &CASTLE_GORGE,
        Degrade { garble: true, merge: true },
        "grid, phone OCR: half the labels, garbled, 27°34' merged",
    );
}

// ---------------------------------------------------------------------------
// Mhlabatini (Sheet 02)
// ---------------------------------------------------------------------------

#[test]
fn mhlabatini_fixture_is_consistent() {
    check_fixture(&MHLABATINI);
}

#[test]
fn mhlabatini_grid_far_gps_offline() {
    grid_far_offline(&MHLABATINI);
}

#[test]
fn mhlabatini_contours_gps_at_centre() {
    contours_at(&MHLABATINI, MHLABATINI.centre, "contours, GPS at the centre");
}

#[test]
fn mhlabatini_contours_gps_10km_off_centre() {
    contours_at(&MHLABATINI, MHLABATINI.near, "contours, GPS 10 km off centre");
}

#[test]
fn mhlabatini_contours_gps_60km_away_fails() {
    contours_far_fails(&MHLABATINI);
}

#[test]
fn mhlabatini_grid_phone_ocr_half_the_labels() {
    grid_phone(&MHLABATINI, Degrade { garble: false, merge: false }, "grid, phone OCR: half the labels");
}

#[test]
fn mhlabatini_grid_phone_ocr_garbled_marks() {
    grid_phone(&MHLABATINI, Degrade { garble: true, merge: false }, "grid, phone OCR: half the labels, marks garbled");
}

#[test]
fn mhlabatini_grid_phone_ocr_label_merged_with_credit() {
    grid_phone(
        &MHLABATINI,
        Degrade { garble: false, merge: true },
        "grid, phone OCR: half the labels, 27°19' merged with the credit",
    );
}

#[test]
fn mhlabatini_grid_phone_ocr_garbled_and_merged() {
    grid_phone(
        &MHLABATINI,
        Degrade { garble: true, merge: true },
        "grid, phone OCR: half the labels, garbled, 27°19' merged",
    );
}

// ---------------------------------------------------------------------------
// Points of interest read from the map (RapidOCR stand-in OCR of all three sheets)
// ---------------------------------------------------------------------------

/// The POIs of a grid answer from `ocr` (the user 60 km away, no DEM).
fn sheet_pois(dir: &str, map: Rgb8, ocr: Vec<OcrObservation>, far: (f64, f64)) -> Vec<mapme_core::labels::PoiLabel> {
    let s = Session::new(map);
    let r = s.georeference(&request(ocr, far, &["grid"]));
    assert_eq!(r.status, Status::Ok, "{dir}: {}", r.message);
    println!("[{dir}] {} POIs:", r.pois.len());
    for p in &r.pois {
        let e = p.elevation_m.map(|e| format!(" {e:.0} m")).unwrap_or_default();
        println!("    {:?} {:?}{e} ({:.0}, {:.0})", p.kind, p.name, p.x, p.y);
    }
    r.pois
}

#[test]
fn pois_on_the_sheets() {
    let _g = serial();
    let groot = sheet_dir("grootkloof");
    let rapid = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/data/grootkloof-rapidocr.json");
    let v: serde_json::Value = serde_json::from_slice(&std::fs::read(rapid).unwrap()).unwrap();
    let groot_rapid: Vec<OcrObservation> = serde_json::from_value(v["observations"].clone()).unwrap();
    let groot_map = load_jpeg_file(&groot.join("map.jpg"));
    let far_groot = (-25.8499, 27.9988);
    let lists = [
        ("grootkloof (hand-built OCR)", sheet_pois("grootkloof", groot_map.clone(), load_ocr(&groot), far_groot)),
        ("grootkloof (RapidOCR)", sheet_pois("grootkloof-rapidocr", groot_map, groot_rapid, far_groot)),
        ("castle-gorge", sheet_pois("castle-gorge", CASTLE_GORGE.map(), CASTLE_GORGE.ocr(), CASTLE_GORGE.far)),
        ("mhlabatini", sheet_pois("mhlabatini", MHLABATINI.map(), MHLABATINI.ocr(), MHLABATINI.far)),
    ];
    for (what, pois) in &lists {
        // Every spot height on these sheets has four digits: the misreads with a digit lost
        // ("478" for 1478, "283" next to "1283") are gone.
        for p in pois.iter().filter(|p| p.kind == PoiKind::SpotHeight) {
            assert!(p.elevation_m.is_some_and(|e| e >= 1000.0), "{what}: {p:?}");
        }
        // No legend entry, imprint or title.
        for p in pois {
            let n = p.name.to_lowercase();
            for bad in ["peren", "inter", "stati", "place of", "gous", "gisv", "mcsa", "trig bea"] {
                assert!(!n.contains(bad), "{what}: {p:?}");
            }
        }
        // No piece of another POI next to it.
        let key = |s: &str| s.chars().filter(|c| c.is_alphanumeric()).collect::<String>().to_lowercase();
        for a in pois.iter() {
            for b in pois.iter().filter(|b| b.name.len() > a.name.len()) {
                // Within the longer one's extent (~24 px a character on these sheets).
                let near = (a.x - b.x).abs() < 12.0 * b.name.len() as f64 && (a.y - b.y).abs() < 40.0;
                assert!(!(near && key(&b.name).contains(&key(&a.name))), "{what}: {a:?} is a piece of {b:?}");
            }
        }
    }
    let names = |i: usize| lists[i].1.iter().map(|p| p.name.as_str()).collect::<Vec<_>>();
    let [groot, groot_rapid, castle, mhl] = [0, 1, 2, 3].map(names);
    assert_eq!(groot, vec!["KWAMI LODGE", "SUGARBUSH HILL COUNTRY COTTAGE", "1428", "1412", "1523"]);
    for n in ["KWAMI LODGE", "SPARKLING WATERS HOTEL", "WOODY GULLY", "CAVE KLOOF", "1614"] {
        assert!(groot_rapid.contains(&n), "{n}: {groot_rapid:?}");
    }
    for n in ["LONG POOL", "LIGHTNING RAVINE", "NEEDLE GULLY", "PINACLE GULLY", "KRAAL", "1769", "1283"] {
        assert!(castle.contains(&n), "{n}: {castle:?}");
    }
    for n in ["AASVOELKRANS", "AASVOEL GULLY", "MHLABATINIPARKING", "MOUNT SERENE", "1754"] {
        assert!(mhl.contains(&n), "{n}: {mhl:?}");
    }
    let serene = lists[3].1.iter().find(|p| p.name == "MOUNT SERENE").unwrap();
    assert_eq!((serene.kind, serene.elevation_m), (PoiKind::Peak, Some(1471.0)));
    // 100, 102 and 81 POIs before the clean-up (legend entries, pieces of labels, misread
    // heights); what is left is the map's names and heights, read as well as the OCR read them.
    assert!(groot_rapid.len() <= 55 && castle.len() <= 60 && mhl.len() <= 40, "{groot_rapid:?} {castle:?} {mhl:?}");
}

// ---------------------------------------------------------------------------
// Vegetation hatching (Mhlabatini)
// ---------------------------------------------------------------------------

/// Mhlabatini's green vegetation hatching drawn ~4000 px long (the hatched
/// block at x 2900–3300, y 150–700 repeated down the sheet), longer than the
/// coloured lines a grid needs: still no grid line, and the grid still fits.
#[test]
fn mhlabatini_long_vegetation_hatching_is_no_grid() {
    let _g = serial();
    let mut map = MHLABATINI.map();
    for y in 700..4200 {
        let src = 150 + (y - 150) % 550;
        for x in 2900..3300 {
            let (s, d) = ((src * map.w + x) * 3, (y * map.w + x) * 3);
            map.data.copy_within(s..s + 3, d);
        }
    }
    let lines = mapme_core::grid::detect_lines(&map);
    let vs: Vec<f64> = lines
        .iter()
        .filter(|l| l.axis == mapme_core::grid::LineAxis::Vertical)
        .map(|l| l.pos_at(map.h as f64 / 2.0))
        .collect();
    println!("vertical lines: {vs:?}");
    assert!(vs.iter().all(|x| !(2850.0..3350.0).contains(x)), "hatching taken for a line: {vs:?}");
    let truth = MHLABATINI.truth_json();
    for l in truth["lon_lines"].as_array().unwrap() {
        let x = l["x"].as_f64().unwrap();
        assert!(vs.iter().any(|v| (v - x).abs() < 1.5), "meridian at {x} lost: {vs:?}");
    }
    let s = Session::new(map);
    let r = s.georeference(&request(MHLABATINI.ocr(), MHLABATINI.far, &["grid"]));
    let err = report("grid, long vegetation hatching", &MHLABATINI, &r);
    assert_eq!(r.method, Some(Method::Grid), "{}", r.message);
    assert!(err <= GRID_MAX_ERR_M, "max error {err:.1} m");
}

// ---------------------------------------------------------------------------
// One latitude label read
// ---------------------------------------------------------------------------

/// The phone read one latitude label (`lat_text` at line `lat`, or its printed
/// text) and the sheet's phone-OCR pair of longitude labels.
fn one_latitude_label(sheet: &Sheet, lat: usize, lat_text: Option<&str>) -> Vec<OcrObservation> {
    let mut out: Vec<OcrObservation> = sheet
        .ocr()
        .into_iter()
        .filter(|o| {
            let (cx, cy) = o.center();
            cx >= 200.0 && cy <= 4790.0
        })
        .collect();
    let (lats, lons) = printed_labels(sheet);
    let mut l = lats[lat].clone();
    if let Some(t) = lat_text {
        l.text = t.into();
    }
    out.push(l);
    out.extend(sheet.phone.keep_lon.iter().map(|&i| lons[i].clone()));
    out.push(OcrObservation { text: CREDIT.into(), bbox: CREDIT_BBOX, confidence: 0.7 });
    out
}

/// One latitude label: the latitudes follow from the longitudes' scale and the
/// graticule lines. Offline and 60 km away nothing can confirm it, so it is
/// answered flagged for checking; with the terrain under the map it is
/// confirmed; read wrong, the terrain rules it out and no answer is given.
fn lone_latitude_label(sheet: &Sheet, lat: usize, misread: &str) {
    let _g = serial();
    let req = request(one_latitude_label(sheet, lat, None), sheet.far, &["grid", "contours"]);
    let r = run(sheet, &req, None);
    let err = report("one latitude label, no DEM", sheet, &r);
    assert_eq!((r.status, r.method), (Status::Ok, Some(Method::Grid)), "{}", r.message);
    assert!(err <= GRID_MAX_ERR_M, "max error {err:.1} m");
    assert!(r.confidence.unwrap() < 0.5 && r.message.contains("Only one latitude label"), "{}", r.message);
    assert_eq!(r.diagnostics["grid"]["lone_label"], true);

    let r = run(sheet, &req, Some(&sheet.path().join("dem")));
    let err = report("one latitude label, DEM under the map", sheet, &r);
    assert_eq!((r.status, r.method), (Status::Ok, Some(Method::Grid)), "{}", r.message);
    assert!(err <= GRID_MAX_ERR_M, "max error {err:.1} m");
    assert!(r.confidence.unwrap() >= 0.9 && r.message.contains("confirmed by the terrain"), "{}", r.message);

    let req = request(one_latitude_label(sheet, lat, Some(misread)), sheet.far, &["grid", "contours"]);
    let r = run(sheet, &req, Some(&sheet.path().join("dem")));
    let err = report("one latitude label read wrong, DEM under the map", sheet, &r);
    println!("    contour_check: {}", r.diagnostics["contour_check"]);
    assert_eq!(r.diagnostics["contour_check"]["agrees"], false, "{}", r.diagnostics);
    assert_eq!(r.status, Status::Failed, "wrong answer {err:.0} m off: {}", r.message);
}

/// The sheet without its colours (contours, vegetation, water): black ink on
/// white, like a road map or a monochrome copy with a graticule.
fn without_colour(map: &Rgb8) -> Rgb8 {
    let mut m = map.clone();
    for px in m.data.chunks_exact_mut(3) {
        let (hi, lo) = (px.iter().max().unwrap(), px.iter().min().unwrap());
        if hi - lo > 20 || *lo > 120 {
            px.copy_from_slice(&[255, 255, 255]);
        }
    }
    m
}

#[test]
fn one_latitude_label_on_a_map_without_contours() {
    // Nothing on the map to hold against the terrain: the check can neither confirm nor rule
    // out the one-label fit, which is answered flagged for checking, as offline, not rejected
    // as if the terrain disagreed.
    let _g = serial();
    let sheet = &MHLABATINI;
    let map = without_colour(&sheet.map());
    let req = request(one_latitude_label(sheet, 2, None), sheet.far, &["grid", "contours"]);
    let r = run_map(sheet, map, &req, Some(&sheet.path().join("dem")));
    let err = report("one latitude label, no contours on the map, DEM under it", sheet, &r);
    println!("    contour_check: {}", r.diagnostics["contour_check"]);
    assert_eq!((r.status, r.method), (Status::Ok, Some(Method::Grid)), "{}", r.message);
    assert!(err <= GRID_MAX_ERR_M, "max error {err:.1} m");
    assert!(r.confidence.unwrap() < 0.5 && r.message.contains("Only one latitude label"), "{}", r.message);
}

#[test]
fn castle_gorge_one_latitude_label() {
    // 25°49' on its line; misread as 25°48'.
    lone_latitude_label(&CASTLE_GORGE, 2, "25°48'00\"S");
}

#[test]
fn mhlabatini_one_latitude_label() {
    // 25°50' on its line; misread as 25°56'.
    lone_latitude_label(&MHLABATINI, 2, "25°56'00\"S");
}

// ---------------------------------------------------------------------------
// Apple Vision's own reads
// ---------------------------------------------------------------------------

/// `<sheet>/vision-ocr.json`: what Apple Vision (the app's OCRService, iOS 26.2 simulator) read
/// on the sheet as MapImporter renders it; its boxes are map.jpg pixels.
fn vision_ocr(dir: &Path) -> Vec<OcrObservation> {
    let v: serde_json::Value = serde_json::from_slice(&std::fs::read(dir.join("vision-ocr.json")).unwrap()).unwrap();
    serde_json::from_value(v["observations"].clone()).unwrap()
}

#[test]
fn vision_ocr_locates_the_sheets() {
    // The app's request (grid and contours, dpi 300) with the GPS 60–90 km away in Pretoria
    // and no terrain data: only the grid can answer. Vision reads three to four labels per
    // axis on every sheet (and one 25°52' label as 25°50', which the others outvote).
    let _g = serial();
    let pretoria = (-25.75, 28.19);
    for dir in ["grootkloof", "castle-gorge", "mhlabatini"] {
        let path = sheet_dir(dir);
        let map = load_jpeg_file(&path.join("map.jpg"));
        let s = Session::new(map);
        let r = s.georeference(&request(vision_ocr(&path), pretoria, &["grid", "contours"]));
        let r = if r.status == Status::NeedDem {
            for &t in &r.need_tiles {
                s.mark_dem_tile_missing(t).unwrap();
            }
            s.georeference(&request(vision_ocr(&path), pretoria, &["grid", "contours"]))
        } else {
            r
        };
        let err = r
            .pixel_to_mercator
            .map_or(f64::INFINITY, |m| max_graticule_error_m(&Homography { m }, &graticule_truth(&path)));
        println!(
            "[{dir} Vision] {:?} {:?} conf {:?}, max error {err:.1} m: {}",
            r.status, r.method, r.confidence, r.message
        );
        assert_eq!((r.status, r.method), (Status::Ok, Some(Method::Grid)), "{dir}: {}", r.message);
        assert!(err <= GRID_MAX_ERR_M, "{dir}: max error {err:.1} m");
        assert!(r.confidence.unwrap() >= 0.9, "{dir}: {:?}", r.confidence);
        assert_eq!(r.diagnostics["grid"]["lone_label"], false, "{dir}");
        // Spot heights: the sheets' heights all have four digits (Vision drops leading 1s too:
        // "287", "338", "124" on Mhlabatini).
        let heights: Vec<f64> =
            r.pois.iter().filter(|p| p.kind == PoiKind::SpotHeight).filter_map(|p| p.elevation_m).collect();
        assert!(heights.len() >= 10, "{dir}: {heights:?}");
        assert!(heights.iter().all(|&h| h >= 1000.0), "{dir}: {heights:?}");
        // No graticule label, legend entry or title among the POIs.
        for p in &r.pois {
            let n = p.name.to_lowercase();
            for bad in ["°", "00\"", "peren", "trig bea", "mcsa", "magaliesberg sheet", "annual change"] {
                assert!(!n.contains(bad), "{dir}: {p:?}");
            }
        }
    }
}

/// The Nearby list on Apple Vision's reads (the app's request, the GPS 60–90 km away): every
/// name and spot height printed on the sheet that Vision read, once, and nothing else.
///
/// Checked against the sheets' images. Before the clean-up of Vision's misreads the lists also
/// held "MIODLE RIDGEKIOOF" (a second read of MIDDLE RIDGE KLOOF), "UAMERKOD KI OOF" (of HAMERKOP
/// KLOOF), "KEED RIGHT" (KEEP RIGHT), pieces of ROOIKRANTZ GULLY ("ROOIKRI", "ANTZ", "PHANL
/// GULLI"), the imprint "ISV1 Sept" and "KUMB." (KUMBA, cut by the frame), and read "WCASTLE GORGE
/// WATERFALL (LOWER)", "1KWAMI LODGE", "B KONG POOL", "STILE 1.7Km", "PIHRANA POO", "CASCADE KL",
/// "EASY GUL", "GÁTE" and "AČCOMMODATION"; the spot height "1501 A" (its triangle read as an A)
/// was lost. Known misreads that stay (no second read to tell): "HAMERKOR GATE" (HAMERKOP),
/// "MILORHO LODGE", "MARETEWA RIVER" (MARETLWANE), "KUDU KONAÍ", "PARAMOUNT CONFERENCE & ADVENTU"
/// and the height "2015" (a 1700 contour label read across); "ZIMBABWE" and "HAMERKOP" are the
/// first words of the turned labels ZIMBABWE KLOOF and HAMERKOP GULLY, whose other word Vision
/// did not read. Not read at all: MIDDLE RIDGE (taken for a piece of MIDDLE RIDGE KLOOF), CAVE and
/// CAVE KLOOF (a legend word, lettering read as large as the title) and FERN KLOOF (idem).
#[test]
fn vision_pois_are_the_names_printed_on_the_sheets() {
    let _g = serial();
    let pretoria = (-25.75, 28.19);
    let sheets: [(&str, &[&str]); 3] = [
        (
            "grootkloof",
            &[
                "SUGARBUSH HILL COUNTRY COTTAGE",
                "PARAMOUNT CONFERENCE & ADVENTU",
                "RIETFONTEIN VALLEY",
                "HAMERKOR GATE",
                "SPARKLING WATERS HOTEL",
                "BARTLETT FARM PARKING",
                "BARTLETT/FARM GATE",
                "MILORHO LODGE",
                "GROOTKLOOF PEDESTRIAN GATE",
                "YSTERHOUT KLOOF",
                "WOODPECKER GULLY",
                "CANDELABRA",
                "ROTSBOOM KLOOF",
                "EASY GULLY",
                "WOODY GULLY",
                "HAMERKOP",
                "GROOTKLOOF",
                "HAMERKOP KLOOF",
                "KWAMI LODGE",
                "1496",
                "1468",
                "1394",
                "1388",
                "1502",
                "1392",
                "1446",
                "1424",
                "1372",
                "1479",
                "1494",
                "1488",
                "1518",
                "1526",
                "1614",
                "1536",
                "1504",
                "1582",
                "1719",
                "1741",
                "1716",
                "1696",
                "1742",
                "2015",
                "1749",
                "1742",
                "1719",
                "1416",
                "1754",
                "1556",
                "1471",
                "1533",
                "1514",
                "1516",
                "1504",
            ],
        ),
        (
            "castle-gorge",
            &[
                "BUSSHBREAK ACCOMMODATION",
                "FEATHERS B&B",
                "KUDU KONAÍ",
                "STAFF HOUSES",
                "MARETLWANE BUSH SCHOOL",
                "MARETEWA RIVER",
                "AGTERSKOT",
                "TENTED CAMP",
                "MIDDLE RIDGE KLOOF",
                "ZIMBABWE",
                "ZIMBABWE VLAKTE",
                "BRAMBLE GULLY",
                "CASTLE GORGE WATERFALL (LOWER)",
                "CASTLE GORGE WATERFALL (UPPER)",
                "GRASSY POOL",
                "STILE",
                "PIHRANA POOL",
                "RAND WATER BOX 2",
                "TURN RIGHT AT SIGN",
                "VEHICLE GATE",
                "KEEP RIGHT",
                "RAND WATER BOX 1",
                "LONG POOL",
                "KRAAL",
                "UPPER CASTLE GULLY",
                "SIGN POST",
                "NO ENTRY SIGN",
                "PRIVATE PROPERTY",
                "KEEP LEFT",
                "CASCADE KLOOF",
                "CROSSING",
                "JOINS DIRT ROAD",
                "MAGALIESBERG MOUNTAIN LODGE",
                "GAUTENG",
                "NORTH WEST",
                "PINACLE GULLY",
                "LIGHTNING RAVINE",
                "NEEDLE GULLY",
                "SHOOP MOUNTAIN LODGE",
                "1246",
                "1529",
                "1492",
                "1501",
                "1546",
                "1598",
                "1558",
                "1556",
                "1497",
                "1503",
                "1568",
                "1716",
                "1769",
                "1331",
            ],
        ),
        (
            // Clean before the clean-up of Vision's misreads; unchanged by it.
            "mhlabatini",
            &[
                "BERGHEIN",
                "MHLABATINI PARKING",
                "MHLABATINI KLOOF",
                "CAMP GULLY",
                "RETIEP'SKLOOF CATCHMENT AREA",
                "AASVOËL GULLY",
                "AASVOËLKRANS",
                "AASVOELKRANS RESORT",
                "AM HANG SELF-CATERIN",
                "MOUNT SERENE",
                "OLIFANTSHOEK PRIMARY SCHOOL",
                "NALE",
                "1248",
                "1239",
                "1259",
                "1316",
                "1718",
                "1213",
                "1754",
                "1482",
                "1688",
                "1512",
                "1626",
                "1324",
                "1636",
                "1649",
                "1464",
                "1312",
                "1413",
                "1728",
                "1202",
                "1472",
                "1306",
            ],
        ),
    ];
    for (dir, want) in sheets {
        let path = sheet_dir(dir);
        let s = Session::new(load_jpeg_file(&path.join("map.jpg")));
        let r = s.georeference(&request(vision_ocr(&path), pretoria, &["grid"]));
        assert_eq!((r.status, r.method), (Status::Ok, Some(Method::Grid)), "{dir}: {}", r.message);
        let mut got: Vec<&str> = r.pois.iter().map(|p| p.name.as_str()).collect();
        let mut want = want.to_vec();
        got.sort_unstable();
        want.sort_unstable();
        let missing: Vec<&&str> = want.iter().filter(|n| !got.contains(n)).collect();
        let extra: Vec<&&str> = got.iter().filter(|n| !want.contains(n)).collect();
        assert_eq!(got, want, "{dir}: missing {missing:?}, extra {extra:?}");
        let kind = |n: &str| r.pois.iter().find(|p| p.name == n).map(|p| p.kind);
        if dir == "castle-gorge" {
            assert_eq!(kind("PIHRANA POOL"), Some(PoiKind::Water));
            assert_eq!(kind("LONG POOL"), Some(PoiKind::Water));
            assert_eq!(kind("1501"), Some(PoiKind::SpotHeight));
        }
    }
}
