//! The user's two Gaia GPS PDF exports (fixtures/gaia-langalibalele,
//! fixtures/gaia-giants-castle): thin, faint light-brown contours over
//! hillshade and a park tint, one 100 ppi raster on an A4 page. The app
//! renders the page at 300 dpi, which spreads every line over 3-4 soft pixels
//! (map.jpg, bicubic) or repeats every pixel 3 x 3 (map_nearest.jpg, if
//! CoreGraphics honours the image's /Interpolate false); the raster itself is
//! native.png. The contour mask is made at the effective resolution
//! (`contour_lines`: level 3 for both page renders), and every variant must be
//! located from the map centre and 10 km off with the case's own tiles, and
//! answer nothing confident from 100 km away (where the fixture has no
//! terrain, as offline). The app's own requests on the page render (grid
//! and contours, its OCR text) must locate it from the centre and 15 km off,
//! and from ~450 km away fail honestly, then find it through the names on the
//! map geocoded into hints ("Search wider"). Accuracy at the 25 truth points:
//! median within max(30 m, 2 px), max within max(75 m, 4 px), as the contour
//! benchmark.
//! `cargo test --release --test gaia_contours -- --nocapture` prints the errors.

mod common;
use common::load_jpeg_file;
use mapme_core::dem::TileId;
use mapme_core::geo::{haversine_m, merc_to_latlon, Homography, LatLon};
use mapme_core::georef::{GeorefRequest, GeorefResponse, GpsFix, Session, Status};
use mapme_core::hints::{HintSource, SearchHint, DEFAULT_HINT_RADIUS_M};
use mapme_core::image::Rgb8;
use mapme_core::labels::OcrObservation;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, MutexGuard};

fn serial() -> MutexGuard<'static, ()> {
    static M: Mutex<()> = Mutex::new(());
    M.lock().unwrap_or_else(|e| e.into_inner())
}

/// One case file of a Gaia fixture (common benchmark format).
struct Case {
    dir: PathBuf,
    json: serde_json::Value,
    truth: Vec<((f64, f64), (f64, f64))>,
    m_per_px: f64,
}

impl Case {
    fn load(fixture: &str, file: &str) -> Case {
        let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../fixtures").join(fixture);
        let json: serde_json::Value = serde_json::from_slice(&std::fs::read(dir.join(file)).unwrap()).unwrap();
        let truth = json["truth_points"]
            .as_array()
            .unwrap()
            .iter()
            .map(|p| {
                let f = |k: &str| p[k].as_f64().unwrap();
                ((f("x"), f("y")), (f("lat"), f("lon")))
            })
            .collect();
        let m_per_px = json["m_per_px"].as_f64().unwrap();
        Case { dir, json, truth, m_per_px }
    }

    fn image(&self) -> Rgb8 {
        let path = self.dir.join(self.json["image"].as_str().unwrap());
        if path.extension().is_some_and(|e| e == "png") {
            load_png(&path)
        } else {
            load_jpeg_file(&path)
        }
    }

    fn ocr(&self) -> Vec<OcrObservation> {
        let v: serde_json::Value =
            serde_json::from_slice(&std::fs::read(self.dir.join(self.json["ocr"].as_str().unwrap())).unwrap()).unwrap();
        serde_json::from_value(v["observations"].clone()).unwrap()
    }

    fn gps(&self, variant: &str) -> (f64, f64) {
        let v = &self.json["gps_variants"][variant];
        (v[0].as_f64().unwrap(), v[1].as_f64().unwrap())
    }

    /// The app's request (its OCR text and the render dpi), contours only,
    /// through the need_dem loop with the fixture's tiles.
    fn run(&self, variant: &str) -> GeorefResponse {
        let (lat, lon) = self.gps(variant);
        let req = GeorefRequest {
            gps: Some(GpsFix { lat, lon, accuracy_m: Some(10.0) }),
            ocr: self.ocr(),
            dpi: self.json["dpi"].as_f64(),
            methods: vec!["contours".into()],
            ..Default::default()
        };
        self.run_request(&Session::new(self.image()), &req)
    }

    /// `req` through the need_dem loop: the fixture's tiles, then the
    /// Magaliesberg's (fixtures/grootkloof: where the user's other maps
    /// are), the rest marked missing as when offline.
    fn run_request(&self, session: &Session, req: &GeorefRequest) -> GeorefResponse {
        let dirs = [self.dir.join(self.json["dem_dir"].as_str().unwrap()), self.dir.join("../grootkloof").join("dem")];
        for _ in 0..300 {
            let resp = session.georeference(req);
            if resp.status != Status::NeedDem {
                return resp;
            }
            for &t in &resp.need_tiles {
                match dirs.iter().find_map(|d| std::fs::read(tile_path(d, t)).ok()) {
                    Some(png) => session.add_dem_tile(t, &png).unwrap(),
                    None => session.mark_dem_tile_missing(t).unwrap(),
                }
            }
        }
        panic!("still need DEM tiles after 300 rounds");
    }

    /// Sorted ground errors (metres) at the truth points.
    fn errors(&self, resp: &GeorefResponse) -> Option<Vec<f64>> {
        let h = Homography { m: resp.pixel_to_mercator? };
        let mut e: Vec<f64> = self
            .truth
            .iter()
            .map(|&((x, y), (lat, lon))| {
                let (la, lo) = merc_to_latlon(h.apply(x, y).0, h.apply(x, y).1);
                haversine_m(LatLon { lat: la, lon: lo }, LatLon { lat, lon })
            })
            .collect();
        e.sort_by(f64::total_cmp);
        Some(e)
    }
}

fn tile_path(dir: &Path, t: TileId) -> PathBuf {
    dir.join(format!("{}_{}_{}.png", t.z, t.x, t.y))
}

fn load_png(path: &Path) -> Rgb8 {
    let mut dec = png::Decoder::new(std::fs::File::open(path).unwrap());
    dec.set_transformations(png::Transformations::EXPAND | png::Transformations::STRIP_16);
    let mut reader = dec.read_info().unwrap();
    let mut buf = vec![0u8; reader.output_buffer_size()];
    let info = reader.next_frame(&mut buf).unwrap();
    let ch = info.color_type.samples();
    let rgb: Vec<u8> = buf[..info.buffer_size()]
        .chunks_exact(ch)
        .flat_map(|p| if ch >= 3 { [p[0], p[1], p[2]] } else { [p[0]; 3] })
        .collect();
    Rgb8::from_rgb(info.width as usize, info.height as usize, rgb).unwrap()
}

/// Located within the benchmark's tolerance, the mask made at `level`.
fn located(fixture: &str, file: &str, variant: &str, level: u64) {
    let _g = serial();
    let case = Case::load(fixture, file);
    let resp = case.run(variant);
    let errs = case.errors(&resp);
    let ex = &resp.diagnostics["contours"]["extraction"];
    let s = format!(
        "{fixture}/{file} ({variant}): {:?} conf {:?} {:?} m/px (true {:.2}), errors {}: {} | extraction {ex}",
        resp.status,
        resp.confidence,
        resp.scale_m_per_px,
        case.m_per_px,
        errs.as_ref().map_or("-".into(), |e| format!("median {:.1} m, max {:.1} m", e[e.len() / 2], e[e.len() - 1])),
        resp.message
    );
    println!("{s}");
    assert_eq!(resp.status, Status::Ok, "{s}");
    assert_eq!(ex["level"].as_u64(), Some(level), "{s}");
    let e = errs.unwrap();
    let (tol_med, tol_max) = (30f64.max(2.0 * case.m_per_px), 75f64.max(4.0 * case.m_per_px));
    assert!(e[e.len() / 2] <= tol_med && e[e.len() - 1] <= tol_max, "{s}: tolerance {tol_med:.0} / {tol_max:.0} m");
    assert!(resp.confidence.unwrap() >= 0.7, "{s}");
}

/// 100 km away, without terrain there: no confident answer.
fn honest_when_far(fixture: &str) {
    let _g = serial();
    let case = Case::load(fixture, "case.json");
    let resp = case.run("far");
    println!("{fixture} (far): {:?} conf {:?}: {}", resp.status, resp.confidence, resp.message);
    assert!(resp.status != Status::Ok || resp.confidence.unwrap_or(0.0) < 0.5, "{}", resp.message);
}

#[test]
fn langalibalele_page_centre() {
    located("gaia-langalibalele", "case.json", "centre", 3);
}

#[test]
fn langalibalele_page_offset() {
    located("gaia-langalibalele", "case.json", "offset", 3);
}

#[test]
fn langalibalele_far() {
    honest_when_far("gaia-langalibalele");
}

#[test]
fn langalibalele_nearest_render_centre() {
    located("gaia-langalibalele", "case_nearest.json", "centre", 3);
}

#[test]
fn langalibalele_native_raster_centre() {
    located("gaia-langalibalele", "case_native.json", "centre", 1);
}

#[test]
fn giants_castle_page_centre() {
    located("gaia-giants-castle", "case.json", "centre", 3);
}

#[test]
fn giants_castle_page_offset() {
    located("gaia-giants-castle", "case.json", "offset", 3);
}

#[test]
fn giants_castle_far() {
    honest_when_far("gaia-giants-castle");
}

#[test]
fn giants_castle_nearest_render_centre() {
    located("gaia-giants-castle", "case_nearest.json", "centre", 3);
}

// ---------------------------------------------------------------------------
// The app's requests for the page as it renders it (map.jpg) with its OCR
// text: grid and contours, as GeoreferenceService sends them. The GPS at the
// centre or 15 km off: located by the 50 km search. From the Magaliesberg
// (~450 km, where the user's other maps are): the 50 km search fails
// honestly and returns the names on the map; the app geocodes them into
// hints and "Search wider" (2 000 km) finds the map in a hint area.
// ---------------------------------------------------------------------------

/// The Magaliesberg, ~450 km north-west of both Gaia maps.
const MAGALIESBERG: (f64, f64) = (-25.83, 27.5);

impl Case {
    fn app_request(&self, gps: (f64, f64)) -> GeorefRequest {
        GeorefRequest {
            gps: Some(GpsFix { lat: gps.0, lon: gps.1, accuracy_m: Some(10.0) }),
            ocr: self.ocr(),
            dpi: self.json["dpi"].as_f64(),
            methods: vec!["grid".into(), "contours".into()],
            ..Default::default()
        }
    }

    /// The page's pixel → Mercator truth (truth.json).
    fn page_truth(&self) -> Homography {
        let v: serde_json::Value =
            serde_json::from_slice(&std::fs::read(self.dir.join("truth.json")).unwrap()).unwrap();
        let m: Vec<f64> =
            v["page"]["pixel_to_mercator"].as_array().unwrap().iter().map(|x| x.as_f64().unwrap()).collect();
        Homography { m: m.try_into().unwrap() }
    }

    /// A stand-in geocoder: a name the response offers to geocode, at the
    /// truth position of its label on the page, 4 km north-east of it (a
    /// gazetteer's point for a pass or a peak is not where the map prints
    /// its name). None for names not printed on the page.
    fn geocode(&self, query: &str) -> Option<(f64, f64)> {
        let first = query.split_whitespace().next()?;
        let o = self.ocr().into_iter().find(|o| o.text.contains(first))?;
        let (mx, my) = self.page_truth().apply(o.bbox[0] + o.bbox[2] / 2.0, o.bbox[1] + o.bbox[3] / 2.0);
        let (lat, lon) = merc_to_latlon(mx, my);
        Some(destination((lat, lon), 4_000.0, 45.0))
    }

    /// Located within the benchmark's tolerance with confidence ≥ 0.7.
    fn assert_located(&self, what: &str, resp: &GeorefResponse) {
        let errs = self.errors(resp);
        let s = format!(
            "{what}: {:?} {:?} conf {:?} {:?} m/px (true {:.2}), errors {}: {} | search {:?}",
            resp.status,
            resp.method,
            resp.confidence,
            resp.scale_m_per_px,
            self.m_per_px,
            errs.as_ref().map_or("-".into(), |e| format!(
                "median {:.1} m, max {:.1} m",
                e[e.len() / 2],
                e[e.len() - 1]
            )),
            resp.message,
            resp.search
        );
        println!("{s}");
        assert_eq!(resp.status, Status::Ok, "{s}");
        let e = errs.unwrap();
        let (tol_med, tol_max) = (30f64.max(2.0 * self.m_per_px), 75f64.max(4.0 * self.m_per_px));
        assert!(e[e.len() / 2] <= tol_med && e[e.len() - 1] <= tol_max, "{s}: tolerance {tol_med:.0} / {tol_max:.0} m");
        assert!(resp.confidence.unwrap() >= 0.7, "{s}");
    }
}

/// The point `d` metres from `from` on bearing `brg` degrees (sphere).
fn destination(from: (f64, f64), d: f64, brg: f64) -> (f64, f64) {
    let (p1, l1, b, a) = (from.0.to_radians(), from.1.to_radians(), brg.to_radians(), d / 6_371_008.8);
    let p2 = (p1.sin() * a.cos() + p1.cos() * a.sin() * b.cos()).asin();
    let l2 = l1 + (b.sin() * a.sin() * p1.cos()).atan2(a.cos() - p1.sin() * p2.sin());
    (p2.to_degrees(), l2.to_degrees())
}

/// The page with the GPS at the centre and 15 km south-west of it.
fn app_near(fixture: &str) {
    let _g = serial();
    let case = Case::load(fixture, "case.json");
    let centre = case.gps("centre");
    for (what, gps) in [("centre", centre), ("15 km south-west", destination(centre, 15_000.0, 225.0))] {
        let resp = case.run_request(&Session::new(case.image()), &case.app_request(gps));
        case.assert_located(&format!("{fixture} page, GPS {what}"), &resp);
    }
}

/// From the Magaliesberg: the 50 km search answers nothing confident and
/// offers the map's names; their geocoded places as hints, a 2 000 km
/// search finds the map in one of them.
fn app_far_then_hints(fixture: &str) {
    let _g = serial();
    let case = Case::load(fixture, "case.json");
    let session = Session::new(case.image());
    let first = case.run_request(&session, &case.app_request(MAGALIESBERG));
    let s =
        format!("{fixture} from the Magaliesberg: {:?} conf {:?}: {}", first.status, first.confidence, first.message);
    println!("{s} | queries {:?}", first.hint_queries);
    assert!(first.status != Status::Ok || first.confidence.unwrap_or(0.0) < 0.5, "{s}");
    let queries = first.hint_queries.clone().unwrap_or_default();
    // As HintGeocoder: places merged within 5 km, 15 km hint areas.
    let mut hints: Vec<SearchHint> = Vec::new();
    for q in &queries {
        if let Some((lat, lon)) = case.geocode(q) {
            let near = |h: &SearchHint| haversine_m(LatLon { lat, lon }, h.latlon()) < 5_000.0;
            if !hints.iter().any(near) {
                hints.push(SearchHint {
                    lat,
                    lon,
                    radius_m: DEFAULT_HINT_RADIUS_M,
                    source: HintSource::Place,
                    name: Some(q.clone()),
                });
            }
        }
    }
    assert!(!hints.is_empty(), "{fixture}: no query geocoded: {queries:?}");
    let mut req = case.app_request(MAGALIESBERG);
    req.search_radius_m = 2_000_000.0;
    req.hints = hints.clone();
    let resp = case.run_request(&session, &req);
    case.assert_located(&format!("{fixture} from the Magaliesberg, {} hints", hints.len()), &resp);
    let tried = resp.search.as_ref().map_or(0, |p| p.hints_tried);
    assert!((1..=hints.len()).contains(&tried), "{fixture}: hints tried {tried} of {}", hints.len());
}

#[test]
fn langalibalele_app_centre_and_15_km_off() {
    app_near("gaia-langalibalele");
}

#[test]
fn giants_castle_app_centre_and_15_km_off() {
    app_near("gaia-giants-castle");
}

#[test]
fn langalibalele_app_far_then_place_hints() {
    app_far_then_hints("gaia-langalibalele");
}

#[test]
fn giants_castle_app_far_then_place_hints() {
    app_far_then_hints("gaia-giants-castle");
}

// ---------------------------------------------------------------------------
// Honest accuracy when the contours cover part of the map: the adversarial
// review's two repros on the native raster of map A. The reported ± (rms_m)
// must bound the error anywhere on the image, and the fit must not take up
// a shear that only the covered part holds (the free affine extrapolated it:
// 38/96 m off at confidence 0.994 with ±18 m; 51/132 m at 0.915).
// ---------------------------------------------------------------------------

impl Case {
    /// native.png's pixel → Mercator truth (truth.json).
    fn native_truth(&self) -> Homography {
        let v: serde_json::Value =
            serde_json::from_slice(&std::fs::read(self.dir.join("truth.json")).unwrap()).unwrap();
        let m: Vec<f64> =
            v["native"]["pixel_to_mercator"].as_array().unwrap().iter().map(|x| x.as_f64().unwrap()).collect();
        Homography { m: m.try_into().unwrap() }
    }
}

/// `img` (native.png's pixels from `off` on) located with the GPS at its
/// centre, contours only; returns the response and the median and largest
/// ground error over a 9 × 9 grid spanning the whole image, corners included.
fn native_part(img: Rgb8, off: (f64, f64)) -> (GeorefResponse, Option<(f64, f64)>) {
    let case = Case::load("gaia-langalibalele", "case_native.json");
    let truth = case.native_truth();
    let (w, h) = (img.w as f64, img.h as f64);
    let at = |x: f64, y: f64| {
        let (mx, my) = truth.apply(x + off.0, y + off.1);
        merc_to_latlon(mx, my)
    };
    let (lat, lon) = at(w / 2.0, h / 2.0);
    let req = GeorefRequest {
        gps: Some(GpsFix { lat, lon, accuracy_m: Some(10.0) }),
        methods: vec!["contours".into()],
        ..Default::default()
    };
    let resp = case.run_request(&Session::new(img), &req);
    let errs = resp.pixel_to_mercator.map(|m| {
        let fit = Homography { m };
        let mut e: Vec<f64> = (0..81)
            .map(|k| {
                let (x, y) = (w * (k % 9) as f64 / 8.0, h * (k / 9) as f64 / 8.0);
                let (mx, my) = fit.apply(x, y);
                let (la, lo) = merc_to_latlon(mx, my);
                let (tl, tn) = at(x, y);
                haversine_m(LatLon { lat: la, lon: lo }, LatLon { lat: tl, lon: tn })
            })
            .collect();
        e.sort_by(f64::total_cmp);
        (e[40], e[80])
    });
    (resp, errs)
}

/// Located, the error everywhere on the image within the reported ± and
/// `max_m`, and no free affine kept (`refine`: a similarity, or one with
/// the page's aspect ratio when the whole map supports it).
fn assert_honest(label: &str, resp: &GeorefResponse, errs: Option<(f64, f64)>, max_m: f64) {
    let refined = &resp.diagnostics["refined"];
    let kept: Vec<&str> = refined["stages"]
        .as_array()
        .map(|s| s.iter().filter(|s| s["kept"] == true).filter_map(|s| s["model"].as_str()).collect())
        .unwrap_or_default();
    let s = format!(
        "{label}: {:?} conf {:?} ±{:?} m, errors (median, max) {errs:?}, kept {kept:?}, spread {} m, cover {}: {}",
        resp.status, resp.confidence, resp.rms_m, refined["spread_m"], refined["extent_cover"], resp.message
    );
    println!("{s}");
    assert_eq!(resp.status, Status::Ok, "{s}");
    assert!(resp.confidence.unwrap() >= 0.5, "{s}");
    let (_, max) = errs.unwrap();
    assert!(max <= resp.rms_m.unwrap(), "{s}: the error exceeds the reported ±");
    assert!(max <= max_m, "{s}");
    assert!(kept == ["similarity"] || kept == ["aniso"], "{s}");
}

/// JPEG quality 25 (the reviewer's file, native_q25.jpg): the contours
/// survive only on the Lesotho side, the western 40 % of the map.
#[test]
fn langalibalele_native_jpeg_q25_is_honest() {
    let _g = serial();
    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../fixtures/gaia-langalibalele");
    let (resp, errs) = native_part(load_jpeg_file(&dir.join("native_q25.jpg")), (0.0, 0.0));
    assert_honest("native q25", &resp, errs, 50.0);
    // Contours over less than half of the image.
    assert!(resp.diagnostics["refined"]["extent_cover"].as_f64().unwrap() < 0.6);
}

/// The central 50 % of the raster as the reviewer saved it (JPEG quality
/// 88, native_crop50.jpg: 559 × 395 px from (279, 197), 29 segments), and
/// cut from native.png losslessly.
#[test]
fn langalibalele_native_central_crop_is_honest() {
    let _g = serial();
    let (x0, y0, w, h) = (279, 197, 559, 395);
    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../fixtures/gaia-langalibalele");
    let (resp, errs) = native_part(load_jpeg_file(&dir.join("native_crop50.jpg")), (x0 as f64, y0 as f64));
    assert_honest("native central 50 % (JPEG)", &resp, errs, 75.0);
    let full = Case::load("gaia-langalibalele", "case_native.json").image();
    let mut data = Vec::with_capacity(w * h * 3);
    for y in y0..y0 + h {
        data.extend_from_slice(&full.data[(y * full.w + x0) * 3..(y * full.w + x0 + w) * 3]);
    }
    let (resp, errs) = native_part(Rgb8::from_rgb(w, h, data).unwrap(), (x0 as f64, y0 as f64));
    assert_honest("native central 50 % (lossless)", &resp, errs, 75.0);
}

/// The page render of map A with all but its western quarter blank (painted
/// the paper's colour, its frame with it: the sea, a glacier, the land
/// beyond a park as some maps leave them). The contours hold the west only;
/// the user may be anywhere on the map. Judged over the inked strip alone,
/// the page's 1.6 % aspect ratio was dropped and extrapolated across the
/// blank part: 57/129 m off at 0.996 with ±43 m (75 % blank), 80/175 m at
/// 1.0 with ±30 m (85 %). Now the map's extent is the raster when its
/// rectangle reaches two of the raster's edges or more: within the ±
/// everywhere, or no answer.
#[test]
fn langalibalele_page_blank_east_is_honest() {
    let _g = serial();
    let case = Case::load("gaia-langalibalele", "case.json");
    let frame: Vec<f64> = case.json["map_frame_px"].as_array().unwrap().iter().map(|v| v.as_f64().unwrap()).collect();
    for keep in [0.25, 0.15] {
        let mut img = case.image();
        // The paper: the 98th percentile of each channel.
        let paper: Vec<u8> = (0..3)
            .map(|c| {
                let mut v: Vec<u8> = img.data.iter().skip(c).step_by(3).copied().collect();
                let k = v.len() * 98 / 100;
                *v.select_nth_unstable(k).1
            })
            .collect();
        let x_blank = (frame[0] + keep * (frame[2] - frame[0])).ceil() as usize;
        for y in 0..img.h {
            for x in x_blank..img.w {
                let i = (y * img.w + x) * 3;
                img.data[i..i + 3].copy_from_slice(&paper);
            }
        }
        let (lat, lon) = case.gps("centre");
        let req = GeorefRequest {
            gps: Some(GpsFix { lat, lon, accuracy_m: Some(10.0) }),
            dpi: case.json["dpi"].as_f64(),
            methods: vec!["contours".into()],
            ..Default::default()
        };
        let resp = case.run_request(&Session::new(img), &req);
        let errs = case.errors(&resp);
        let s = format!(
            "{:.0} % blank: {:?} conf {:?} ±{:?} m, errors (median, max) {:?}, extent {}, cover {}: {}",
            100.0 * (1.0 - keep),
            resp.status,
            resp.confidence,
            resp.rms_m,
            errs.as_ref().map(|e| (e[e.len() / 2], e[e.len() - 1])),
            resp.diagnostics["contours"]["map_extent"],
            resp.diagnostics["refined"]["extent_cover"],
            resp.message
        );
        println!("{s}");
        if let Some(e) = errs {
            assert!(e[e.len() - 1] <= resp.rms_m.unwrap(), "{s}: the error exceeds the reported ±");
        }
    }
}
