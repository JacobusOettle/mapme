//! Contour georeferencing on maps of many cartographic styles: the committed
//! regression subset of the synthetic style corpus (fixtures/styles, rendered
//! by fixtures/tools/synth_topo.py from Terrarium tiles of another zoom than
//! the core matches, generalised, offset by a smooth "survey" field and warped
//! by 1-2.5 px, with the clutter of real maps; see fixtures/styles/README.md).
//! Each case directory holds case.json in the common benchmark format (map
//! centre, 25 truth points pixel -> lat/lon, dpi, m/px, GPS variants), the map
//! as the app gets it (map.jpg), the true contour pixels and the minimal DEM
//! tiles of its region (fixtures/styles/dem/<region>).
//!
//! Per case: with the GPS at the map centre the contour path must locate it
//! (median error at the truth points within max(30 m, 2 px), max within
//! max(75 m, 4 px)); with the GPS 90-110 km away (outside the 50 km search)
//! it must never answer confidently wrong (status ok, confidence >= 0.5 and a
//! median error over 100 m). Cases the core cannot do yet are ignored with
//! the reason (the full benchmark, fixtures/tools/bench_contours.py over the
//! uncommitted corpus, has the per-style picture). Every test drives `Session`
//! like the app (request -> need_dem -> add the case's tiles or mark them
//! missing -> call again). Run with `cargo test --release --test styles`
//! (`-- --include-ignored --nocapture` prints every case's errors).

mod common;
use common::load_jpeg_file;
use mapme_core::dem::TileId;
use mapme_core::geo::{haversine_m, merc_to_latlon, Homography, LatLon};
use mapme_core::georef::{GeorefRequest, GeorefResponse, GpsFix, Session, Status};
use mapme_core::image::Rgb8;
use mapme_core::labels::OcrObservation;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, MutexGuard};

/// Heavy tests run one at a time (timings and memory stay meaningful).
fn serial() -> MutexGuard<'static, ()> {
    static M: Mutex<()> = Mutex::new(());
    M.lock().unwrap_or_else(|e| e.into_inner())
}

fn styles_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../fixtures/styles")
}

/// One case of fixtures/styles (case.json in the common benchmark format).
struct Case {
    name: String,
    dir: PathBuf,
    json: serde_json::Value,
    /// (pixel x, y) -> (lat, lon).
    truth: Vec<((f64, f64), (f64, f64))>,
    m_per_px: f64,
}

impl Case {
    fn load(name: &str) -> Case {
        let dir = styles_dir().join(name);
        let json: serde_json::Value =
            serde_json::from_slice(&std::fs::read(dir.join("case.json")).expect("case.json")).expect("case.json");
        let truth = json["truth_points"]
            .as_array()
            .expect("truth_points")
            .iter()
            .map(|p| {
                let f = |k: &str| p[k].as_f64().unwrap();
                ((f("x"), f("y")), (f("lat"), f("lon")))
            })
            .collect();
        let m_per_px = json["m_per_px"].as_f64().expect("m_per_px");
        Case { name: name.to_string(), dir, json, truth, m_per_px }
    }

    fn image(&self) -> Rgb8 {
        load_jpeg_file(&self.dir.join(self.json["image"].as_str().unwrap()))
    }

    fn point(&self, key: &str) -> (f64, f64) {
        let v = if key == "centre" { &self.json["gps"] } else { &self.json["gps_variants"][key] };
        (v[0].as_f64().unwrap(), v[1].as_f64().unwrap())
    }

    fn dem_dir(&self) -> PathBuf {
        self.dir.join(self.json["dem_dir"].as_str().unwrap())
    }

    fn ocr(&self) -> Vec<OcrObservation> {
        match self.json["ocr"].as_str() {
            Some(f) => {
                let v: serde_json::Value = serde_json::from_slice(&std::fs::read(self.dir.join(f)).unwrap()).unwrap();
                serde_json::from_value(v["observations"].clone()).unwrap()
            }
            None => Vec::new(),
        }
    }

    /// The app's request for this map with the GPS at `gps`: its text, the
    /// render dpi of a PDF page, contours only.
    fn request(&self, gps: (f64, f64)) -> GeorefRequest {
        GeorefRequest {
            gps: Some(GpsFix { lat: gps.0, lon: gps.1, accuracy_m: Some(10.0) }),
            ocr: self.ocr(),
            dpi: self.json["dpi"].as_f64(),
            methods: vec!["contours".into()],
            ..Default::default()
        }
    }

    /// Run the request -> need_dem loop with the case's tiles.
    fn run(&self, gps: (f64, f64)) -> GeorefResponse {
        let session = Session::new(self.image());
        let req = self.request(gps);
        let dem = self.dem_dir();
        for _ in 0..300 {
            let resp = session.georeference(&req);
            if resp.status != Status::NeedDem {
                return resp;
            }
            for &t in &resp.need_tiles {
                match std::fs::read(tile_path(&dem, t)) {
                    Ok(png) => session.add_dem_tile(t, &png).expect("tile"),
                    Err(_) => session.mark_dem_tile_missing(t).expect("mark missing"),
                }
            }
        }
        panic!("{}: still need DEM tiles after 300 rounds", self.name);
    }

    /// Sorted ground errors (metres) of `resp` at the truth points.
    fn errors(&self, resp: &GeorefResponse) -> Option<Vec<f64>> {
        let h = Homography { m: resp.pixel_to_mercator? };
        let mut e: Vec<f64> = self
            .truth
            .iter()
            .map(|&((x, y), (lat, lon))| {
                let (mx, my) = h.apply(x, y);
                let (la, lo) = merc_to_latlon(mx, my);
                haversine_m(LatLon { lat: la, lon: lo }, LatLon { lat, lon })
            })
            .collect();
        e.sort_by(f64::total_cmp);
        Some(e)
    }

    /// Accuracy required with the GPS at the centre: (median, max) metres.
    fn tolerance(&self) -> (f64, f64) {
        (30f64.max(2.0 * self.m_per_px), 75f64.max(4.0 * self.m_per_px))
    }
}

fn tile_path(dir: &Path, t: TileId) -> PathBuf {
    dir.join(format!("{}_{}_{}.png", t.z, t.x, t.y))
}

fn summary(case: &Case, label: &str, resp: &GeorefResponse, errs: &Option<Vec<f64>>) -> String {
    let e = match errs {
        Some(e) => format!("median {:.1} m, max {:.1} m", e[e.len() / 2], e[e.len() - 1]),
        None => "no transform".into(),
    };
    format!(
        "{} ({label}): {:?} conf {:?}, {e}, {:?} m/px (true {:.2}): {} | contours {} | scale range {}",
        case.name,
        resp.status,
        resp.confidence,
        resp.scale_m_per_px,
        case.m_per_px,
        resp.message,
        resp.diagnostics["contours"],
        resp.diagnostics["scale_range"]
    )
}

/// GPS at the map centre: located within the case's tolerance.
fn located_from_centre(name: &str) {
    let _g = serial();
    let case = Case::load(name);
    let resp = case.run(case.point("centre"));
    let errs = case.errors(&resp);
    let s = summary(&case, "centre", &resp, &errs);
    println!("{s}");
    assert_eq!(resp.status, Status::Ok, "{s}");
    let e = errs.unwrap();
    let (tol_med, tol_max) = case.tolerance();
    assert!(e[e.len() / 2] <= tol_med && e[e.len() - 1] <= tol_max, "{s}: tolerance {tol_med:.0} / {tol_max:.0} m");
}

/// GPS 90-110 km away: no confident wrong answer.
fn honest_when_far(name: &str) {
    let _g = serial();
    let case = Case::load(name);
    let resp = case.run(case.point("far"));
    let errs = case.errors(&resp);
    let s = summary(&case, "far", &resp, &errs);
    println!("{s}");
    if resp.status == Status::Ok && resp.confidence.unwrap_or(0.0) >= 0.5 {
        let e = errs.expect("an ok answer has a transform");
        assert!(e[e.len() / 2] <= 100.0, "CONFIDENT WRONG answer: {s}");
    }
}

#[test]
fn style_cases_are_consistent() {
    let mut n = 0;
    for e in std::fs::read_dir(styles_dir()).unwrap() {
        let p = e.unwrap().path();
        if !p.join("case.json").exists() {
            continue;
        }
        let case = Case::load(p.file_name().unwrap().to_str().unwrap());
        assert!(case.truth.len() >= 9, "{}", case.name);
        assert!(case.dem_dir().is_dir(), "{}: {}", case.name, case.dem_dir().display());
        assert!(case.dir.join("contour_mask.png").exists(), "{}", case.name);
        // The truth spans the map: neighbouring points are m_per_px apart in pixels.
        let ((x0, y0), (la0, lo0)) = case.truth[0];
        let ((x1, y1), (la1, lo1)) = case.truth[case.truth.len() - 1];
        let px = (x1 - x0).hypot(y1 - y0);
        let m = haversine_m(LatLon { lat: la0, lon: lo0 }, LatLon { lat: la1, lon: lo1 });
        assert!((m / px / case.m_per_px - 1.0).abs() < 0.02, "{}: {} m over {} px", case.name, m, px);
        let far = case.point("far");
        let c = case.point("centre");
        let d = haversine_m(LatLon { lat: c.0, lon: c.1 }, LatLon { lat: far.0, lon: far.1 });
        assert!(d > 80_000.0, "{}: far GPS only {d:.0} m away", case.name);
        let img = case.image();
        assert_eq!(
            [img.w as u64, img.h as u64],
            [case.json["image_size"][0].as_u64().unwrap(), case.json["image_size"][1].as_u64().unwrap()]
        );
        n += 1;
    }
    assert!(n >= 10, "only {n} style cases");
}

// ---------------------------------------------------------------------------
// The cases (fixtures/styles/<name>). Centre: located; far: honest.
// ---------------------------------------------------------------------------

#[test]
fn gaia_drakensberg_centre() {
    located_from_centre("gaia-drakensberg");
}

#[test]
fn gaia_drakensberg_far() {
    honest_when_far("gaia-drakensberg");
}

#[test]
fn sparse_drakensberg_centre() {
    located_from_centre("sparse-drakensberg");
}

#[test]
fn sparse_drakensberg_far() {
    honest_when_far("sparse-drakensberg");
}

#[test]
fn mcsa_drakensberg_centre() {
    located_from_centre("mcsa-drakensberg");
}

#[test]
fn mcsa_drakensberg_far() {
    honest_when_far("mcsa-drakensberg");
}

#[test]
fn swiss_patagonia_centre() {
    located_from_centre("swiss-patagonia");
}

#[test]
fn swiss_patagonia_far() {
    honest_when_far("swiss-patagonia");
}

#[test]
fn otm_patagonia_centre() {
    located_from_centre("otm-patagonia");
}

#[test]
fn otm_patagonia_far() {
    honest_when_far("otm-patagonia");
}

#[test]
fn os_patagonia_centre() {
    located_from_centre("os-patagonia");
}

#[test]
fn os_patagonia_far() {
    honest_when_far("os-patagonia");
}

#[test]
fn usgs_tasmania_centre() {
    located_from_centre("usgs-tasmania");
}

#[test]
fn usgs_tasmania_far() {
    honest_when_far("usgs-tasmania");
}

#[test]
fn bw_tasmania_centre() {
    located_from_centre("bw-tasmania");
}

#[test]
fn bw_tasmania_far() {
    honest_when_far("bw-tasmania");
}

#[test]
fn grey_tasmania_centre() {
    located_from_centre("grey-tasmania");
}

#[test]
fn grey_tasmania_far() {
    honest_when_far("grey-tasmania");
}

#[test]
fn kompass_himalaya_centre() {
    located_from_centre("kompass-himalaya");
}

#[test]
fn kompass_himalaya_far() {
    honest_when_far("kompass-himalaya");
}

#[test]
fn faded_himalaya_centre() {
    located_from_centre("faded-himalaya");
}

#[test]
fn faded_himalaya_far() {
    honest_when_far("faded-himalaya");
}

#[test]
fn dense_himalaya_centre() {
    located_from_centre("dense-himalaya");
}

#[test]
fn dense_himalaya_far() {
    honest_when_far("dense-himalaya");
}
