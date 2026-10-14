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
//! terrain, as offline). Accuracy at the 25 truth points: median within
//! max(30 m, 2 px), max within max(75 m, 4 px), as the contour benchmark.
//! `cargo test --release --test gaia_contours -- --nocapture` prints the errors.

mod common;
use common::load_jpeg_file;
use mapme_core::dem::TileId;
use mapme_core::geo::{haversine_m, merc_to_latlon, Homography, LatLon};
use mapme_core::georef::{GeorefRequest, GeorefResponse, GpsFix, Session, Status};
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
        let session = Session::new(self.image());
        let dem = self.dir.join(self.json["dem_dir"].as_str().unwrap());
        for _ in 0..300 {
            let resp = session.georeference(&req);
            if resp.status != Status::NeedDem {
                return resp;
            }
            for &t in &resp.need_tiles {
                match std::fs::read(tile_path(&dem, t)) {
                    Ok(png) => session.add_dem_tile(t, &png).unwrap(),
                    Err(_) => session.mark_dem_tile_missing(t).unwrap(),
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
