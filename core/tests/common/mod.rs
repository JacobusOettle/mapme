//! Shared fixture helpers for integration tests (fixtures/grootkloof).
#![allow(dead_code)]

use mapme_core::dem::{DemStore, TileId};
use mapme_core::geo::{latlon_to_merc, Affine, Homography};
use mapme_core::image::Rgb8;
use mapme_core::labels::OcrObservation;
use std::path::PathBuf;

pub fn fixture_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../fixtures/grootkloof")
}

/// The real Grootkloof map (3507×4960, 300 dpi) as packed RGB.
pub fn load_map() -> Rgb8 {
    let bytes = std::fs::read(fixture_dir().join("map.jpg")).expect("map.jpg");
    let mut dec = jpeg_decoder::Decoder::new(std::io::Cursor::new(bytes));
    let px = dec.decode().expect("decode jpeg");
    let info = dec.info().unwrap();
    assert_eq!(info.pixel_format, jpeg_decoder::PixelFormat::RGB24);
    Rgb8::from_rgb(info.width as usize, info.height as usize, px).unwrap()
}

/// Same image as RGBA bytes (what the app passes over the FFI).
pub fn load_map_rgba() -> (Vec<u8>, usize, usize) {
    let m = load_map();
    let mut out = vec![255u8; m.w * m.h * 4];
    for i in 0..m.w * m.h {
        out[i * 4..i * 4 + 3].copy_from_slice(&m.data[i * 3..i * 3 + 3]);
    }
    (out, m.w, m.h)
}

/// Every DEM tile in fixtures/grootkloof/dem (zooms 11, 12, 13).
pub fn dem_tiles() -> Vec<(TileId, Vec<u8>)> {
    let mut v = Vec::new();
    for e in std::fs::read_dir(fixture_dir().join("dem")).unwrap() {
        let p = e.unwrap().path();
        let name = p.file_stem().unwrap().to_string_lossy().to_string();
        let parts: Vec<u32> = name.split('_').map(|s| s.parse().unwrap()).collect();
        v.push((TileId { z: parts[0] as u8, x: parts[1], y: parts[2] }, std::fs::read(&p).unwrap()));
    }
    v.sort_by_key(|t| t.0);
    v
}

pub fn dem_store() -> DemStore {
    let mut s = DemStore::new();
    for (t, b) in dem_tiles() {
        s.add_png(t, &b).unwrap();
    }
    s
}

pub fn ocr_fixture() -> Vec<OcrObservation> {
    let v: serde_json::Value = serde_json::from_slice(&std::fs::read(fixture_dir().join("ocr.json")).unwrap()).unwrap();
    serde_json::from_value(v["observations"].clone()).unwrap()
}

/// Ground-truth pixel → Mercator affine from the measured graticule (±1 m).
pub fn truth() -> Affine {
    let v: serde_json::Value =
        serde_json::from_slice(&std::fs::read(fixture_dir().join("truth.json")).unwrap()).unwrap();
    let mut src = Vec::new();
    let mut dst = Vec::new();
    for la in v["lat_lines"].as_array().unwrap() {
        for lo in v["lon_lines"].as_array().unwrap() {
            src.push((lo["x"].as_f64().unwrap(), la["y"].as_f64().unwrap()));
            dst.push(latlon_to_merc(la["lat"].as_f64().unwrap(), lo["lon"].as_f64().unwrap()));
        }
    }
    Affine::fit(&src, &dst).unwrap()
}

/// Max ground error (metres) of `h` vs truth over a 5×5 grid across the map frame.
pub fn max_error_m(h: &Homography) -> f64 {
    let t = truth();
    let lat_cos = (-25.85f64).to_radians().cos();
    let mut worst = 0.0f64;
    for i in 0..5 {
        for j in 0..5 {
            let x = 107.0 + (3399.0 - 107.0) * i as f64 / 4.0;
            let y = 106.0 + (4841.0 - 106.0) * j as f64 / 4.0;
            let a = h.apply(x, y);
            let b = t.apply(x, y);
            worst = worst.max(((a.0 - b.0).powi(2) + (a.1 - b.1).powi(2)).sqrt() * lat_cos);
        }
    }
    worst
}

/// Grootkloof map centre (lat, lon).
pub const CENTER: (f64, f64) = (-25.8499, 27.4000);
