//! The user's two Gaia GPS exports of the uKhahlamba-Drakensberg
//! (fixtures/gaia-langalibalele: 1 mi bar, 9.1 m/px raster; fixtures/
//! gaia-giants-castle: 2 mi bar, 23.0 m/px raster), the first maps that are
//! no MCSA sheet: thin brown contours in feet over hillshading, spot heights
//! in feet, names wrapped over lines, the app's attribution and projection
//! printed on the page. What the search must do with them, beyond the
//! contour extraction of the 300-dpi page (`contour.rs`):
//!
//! * the raster imported as an image (`native.png`, its own OCR): found from
//!   the map centre, also at 23 m/px (outside the old 0.4–12 m/px range), and
//!   from 450 km away (the Magaliesberg, where the user's other maps are)
//!   through the places named on the map that the app geocodes;
//! * the page: the scale bar ("1 mi", "2 mi") read and measured as a prior;
//! * the names: no attribution or projection text, wrapped names joined,
//!   heights in feet as metres.
//!
//! Hermetic: the fixtures' Terrarium tiles, the rest marked missing as when
//! offline. Truth: truth.json (consensus of independent fits, ±9 m).

use mapme_core::dem::TileId;
use mapme_core::geo::{haversine_m, merc_to_latlon, Homography, LatLon};
use mapme_core::georef::{GeorefRequest, GeorefResponse, GpsFix, Session, Status};
use mapme_core::hints::{hint_queries, HintSource, SearchHint};
use mapme_core::image::Rgb8;
use mapme_core::labels::{classify_pois, OcrObservation, PoiKind};
use mapme_core::scalebar::find_scale_bar;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, MutexGuard};

mod common;
use common::load_jpeg_file;

/// Heavy tests run one at a time.
fn serial() -> MutexGuard<'static, ()> {
    static M: Mutex<()> = Mutex::new(());
    M.lock().unwrap_or_else(|e| e.into_inner())
}

fn dir(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../fixtures").join(name)
}

fn load_png(path: &Path) -> Rgb8 {
    let bytes = std::fs::read(path).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
    let mut dec = png::Decoder::new(std::io::Cursor::new(bytes));
    dec.set_transformations(png::Transformations::EXPAND | png::Transformations::STRIP_16);
    let mut reader = dec.read_info().expect("png header");
    let mut buf = vec![0u8; reader.output_buffer_size()];
    let info = reader.next_frame(&mut buf).expect("png frame");
    let (w, h) = (info.width as usize, info.height as usize);
    let channels = info.color_type.samples();
    let mut rgb = Vec::with_capacity(w * h * 3);
    for y in 0..h {
        for p in buf[y * info.line_size..y * info.line_size + w * channels].chunks_exact(channels) {
            if channels >= 3 {
                rgb.extend_from_slice(&p[..3]);
            } else {
                rgb.extend_from_slice(&[p[0], p[0], p[0]]);
            }
        }
    }
    Rgb8::from_rgb(w, h, rgb).expect("rgb")
}

fn ocr(path: &Path) -> Vec<OcrObservation> {
    let v: serde_json::Value = serde_json::from_slice(&std::fs::read(path).expect("ocr")).expect("ocr json");
    let obs = if v.is_array() { v } else { v["observations"].clone() };
    serde_json::from_value(obs).expect("observations")
}

/// The pixel → Mercator truth of `which` ("native" or "page").
fn truth(name: &str, which: &str) -> Homography {
    let v: serde_json::Value =
        serde_json::from_slice(&std::fs::read(dir(name).join("truth.json")).expect("truth")).expect("truth json");
    let m: Vec<f64> = v[which]["pixel_to_mercator"].as_array().unwrap().iter().map(|x| x.as_f64().unwrap()).collect();
    Homography { m: m.try_into().unwrap() }
}

/// Median and max ground error (metres) of `resp` against the truth over a
/// 5 × 5 grid of the image.
fn errors(resp: &GeorefResponse, truth: &Homography, w: usize, h: usize) -> (f64, f64) {
    let got = Homography { m: resp.pixel_to_mercator.expect("transform") };
    let mut e: Vec<f64> = Vec::new();
    for i in 0..5 {
        for j in 0..5 {
            let (x, y) = ((0.1 + 0.2 * i as f64) * w as f64, (0.1 + 0.2 * j as f64) * h as f64);
            let ll = |t: &Homography| {
                let (mx, my) = t.apply(x, y);
                let (la, lo) = merc_to_latlon(mx, my);
                LatLon::new(la, lo)
            };
            e.push(haversine_m(ll(&got), ll(truth)));
        }
    }
    e.sort_by(f64::total_cmp);
    (e[e.len() / 2], e[e.len() - 1])
}

/// The request → need_dem loop with the fixture's tiles (the rest missing).
fn run(img: Rgb8, req: &GeorefRequest, dem: &Path) -> GeorefResponse {
    let session = Session::new(img);
    for _ in 0..300 {
        let resp = session.georeference(req);
        if resp.status != Status::NeedDem {
            return resp;
        }
        for &t in &resp.need_tiles {
            match std::fs::read(tile_path(dem, t)) {
                Ok(png) => session.add_dem_tile(t, &png).expect("tile"),
                Err(_) => session.mark_dem_tile_missing(t).expect("mark missing"),
            }
        }
    }
    panic!("still need DEM tiles after 300 rounds");
}

fn tile_path(dir: &Path, t: TileId) -> PathBuf {
    dir.join(format!("{}_{}_{}.png", t.z, t.x, t.y))
}

fn native_request(name: &str, gps: (f64, f64)) -> GeorefRequest {
    GeorefRequest {
        gps: Some(GpsFix { lat: gps.0, lon: gps.1, accuracy_m: Some(10.0) }),
        ocr: ocr(&dir(name).join("ocr_native.json")),
        methods: vec!["grid".into(), "contours".into()],
        ..Default::default()
    }
}

fn report(what: &str, resp: &GeorefResponse) -> String {
    format!(
        "{what}: {:?} conf {:?} {:?} m/px: {} | areas {} | refined {}",
        resp.status,
        resp.confidence,
        resp.scale_m_per_px,
        resp.message,
        resp.diagnostics["search_areas"],
        resp.diagnostics["refined"]
    )
}

/// Map B's raster is 23.0 m/px: beyond the old 0.4–12 m/px range, it is
/// found in the coarse scale band (zoom-10 tiles), refined on zoom 12.
#[test]
fn giants_castle_raster_found_at_23_m_per_px() {
    let _g = serial();
    let name = "gaia-giants-castle";
    let img = load_png(&dir(name).join("native.png"));
    let (w, h) = (img.w, img.h);
    let resp = run(img, &native_request(name, (-29.326791, 29.423245)), &dir(name).join("dem"));
    let s = report("giants castle native, GPS at the centre", &resp);
    println!("{s}");
    assert_eq!(resp.status, Status::Ok, "{s}");
    assert!(resp.confidence.unwrap() >= 0.7, "{s}");
    let (med, max) = errors(&resp, &truth(name, "native"), w, h);
    println!("error median {med:.1} m, max {max:.1} m");
    assert!(med <= 30.0 && max <= 60.0, "{s}: {med:.1} / {max:.1} m");
    assert!((resp.scale_m_per_px.unwrap() / 22.99 - 1.0).abs() < 0.02, "{s}");
    let band = &resp.diagnostics["search_areas"].as_array().unwrap().last().unwrap()["band"];
    assert!(band["min"].as_f64().unwrap() >= 12.0 && band["zoom"].as_u64().unwrap() < 11, "{s}");
}

/// The same raster with the GPS 100 km east of it (outside the 50 km
/// search): no confident answer from the wrong terrain, in either band.
#[test]
fn giants_castle_raster_far_fails_honestly() {
    let _g = serial();
    let name = "gaia-giants-castle";
    let img = load_png(&dir(name).join("native.png"));
    let (w, h) = (img.w, img.h);
    let resp = run(img, &native_request(name, (-29.326791, 30.453607)), &dir(name).join("dem"));
    let s = report("giants castle native, GPS 100 km east", &resp);
    println!("{s}");
    if resp.status == Status::Ok && resp.confidence.unwrap_or(0.0) >= 0.5 {
        let (med, _) = errors(&resp, &truth(name, "native"), w, h);
        assert!(med <= 100.0, "CONFIDENT WRONG: {s}");
    }
}

/// From the Magaliesberg, 450 km away (where the user's other maps are), a
/// wide search with the names on the map geocoded into hints finds both
/// rasters in the hint area of the named place.
#[test]
fn both_rasters_found_from_450_km_through_place_names() {
    let _g = serial();
    for (name, place, at) in [
        ("gaia-giants-castle", "Giant's Castle", (-29.35, 29.483)),
        ("gaia-langalibalele", "Langalibalele Pass", (-29.2833, 29.4333)),
    ] {
        let img = load_png(&dir(name).join("native.png"));
        let (w, h) = (img.w, img.h);
        let mut req = native_request(name, (-25.83, 27.5));
        req.search_radius_m = 2_000_000.0;
        req.hints = vec![SearchHint {
            lat: at.0,
            lon: at.1,
            radius_m: 15_000.0,
            source: HintSource::Place,
            name: Some(place.into()),
        }];
        let q = resp_queries(&req);
        // The raster's OCR may clip a letter ("Langalibalel"): a prefix.
        let stem: String = place.chars().take(8).collect();
        assert!(q.iter().any(|n| n.contains(&stem)), "{name}: {q:?}");
        let resp = run(img, &req, &dir(name).join("dem"));
        let s = report(&format!("{name} from the Magaliesberg, hint {place}"), &resp);
        println!("{s}");
        assert_eq!(resp.status, Status::Ok, "{s}");
        assert!(resp.confidence.unwrap() >= 0.7, "{s}");
        let (med, max) = errors(&resp, &truth(name, "native"), w, h);
        println!("error median {med:.1} m, max {max:.1} m");
        assert!(med <= 30.0 && max <= 60.0, "{s}: {med:.1} / {max:.1} m");
        assert_eq!(resp.diagnostics["selected"]["candidate"], 0, "{s}");
        assert_eq!(resp.search.as_ref().unwrap().hints_tried, 1, "{s}");
    }
}

/// The names the request's OCR would give the app to geocode.
fn resp_queries(req: &GeorefRequest) -> Vec<String> {
    hint_queries(&req.ocr)
}

/// The printed scale bars of the pages ("1 mi", "2 mi" over a thin line with
/// end ticks) read and measured between the tick centres: the ground m/px
/// across the page (the page is 1.6 % anisotropic) within 0.5 %.
#[test]
fn gaia_page_scale_bars() {
    for (name, truth_m_per_px) in [("gaia-langalibalele", 3.0289), ("gaia-giants-castle", 7.6496)] {
        let img = load_jpeg_file(&dir(name).join("map.jpg"));
        let bar = find_scale_bar(&img, &ocr(&dir(name).join("ocr.json"))).expect("scale bar");
        println!("{name}: {bar:?}");
        assert_eq!(bar.source, "line");
        assert!((bar.m_per_px / truth_m_per_px - 1.0).abs() < 0.005, "{name}: {bar:?}");
    }
}

/// POIs and hint queries of the pages: no attribution or projection text,
/// wrapped names joined, heights in feet as metres.
#[test]
fn gaia_names_and_heights() {
    let credit = |n: &str| {
        let l = n.to_lowercase().replace(' ', "");
        ["gaia", "openstreet", "mercator", "epsg", "declination"].iter().any(|w| l.contains(w))
    };
    for name in ["gaia-langalibalele", "gaia-giants-castle"] {
        let obs = ocr(&dir(name).join("ocr.json"));
        let pois = classify_pois(&obs);
        let q = hint_queries(&obs);
        println!("{name}: {q:?}\n{pois:?}");
        assert!(pois.iter().all(|p| !credit(&p.name)), "{name}: {pois:?}");
        assert!(q.iter().all(|n| !credit(n)), "{name}: {q:?}");
        // Mount Durnford, 10807 ft, on both maps.
        let durnford = pois.iter().find(|p| p.name == "Mount Durnford").expect("Mount Durnford");
        assert_eq!(durnford.kind, PoiKind::Peak);
        assert!((durnford.elevation_m.unwrap() - 3294.0).abs() < 1.0, "{durnford:?}");
        // Heights in metres, as printed in feet.
        for p in pois.iter().filter(|p| p.kind == PoiKind::SpotHeight) {
            let printed: f64 = p.name.parse().unwrap();
            assert!((p.elevation_m.unwrap() - printed * 0.3048).abs() < 0.1, "{p:?}");
        }
        // Wrapped names first among the queries.
        assert!(q[..3].iter().all(|n| n.split_whitespace().count() >= 2), "{name}: {q:?}");
    }
    let q = hint_queries(&ocr(&dir("gaia-giants-castle").join("ocr.json")));
    for n in ["Bannerman Hut", "Langalibalele Pass", "Ntubeni Pass", "Mount Durnford"] {
        assert!(q.iter().any(|x| x == n), "{n}: {q:?}");
    }
    let pois = classify_pois(&ocr(&dir("gaia-giants-castle").join("ocr.json")));
    assert!(pois.iter().any(|p| p.name.starts_with("Giant's Cast") && p.elevation_m == Some(3315.0)), "{pois:?}");
}
