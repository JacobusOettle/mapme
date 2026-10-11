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

/// Max ground error (metres) over the map frame of `h`, a georeference of a
/// warped copy of the map; `fwd` maps map.jpg pixels into the copy.
pub fn max_error_mapped(h: &Homography, fwd: &Homography) -> f64 {
    let t = truth();
    let lat_cos = (-25.85f64).to_radians().cos();
    let mut worst = 0.0f64;
    for i in 0..5 {
        for j in 0..5 {
            let x = 107.0 + (3399.0 - 107.0) * i as f64 / 4.0;
            let y = 106.0 + (4841.0 - 106.0) * j as f64 / 4.0;
            let (u, v) = fwd.apply(x, y);
            let a = h.apply(u, v);
            let b = t.apply(x, y);
            worst = worst.max((a.0 - b.0).hypot(a.1 - b.1) * lat_cos);
        }
    }
    worst
}

// ---------------------------------------------------------------------------
// Synthetic photos of the paper map (photo mode)
// ---------------------------------------------------------------------------

/// Portrait iPhone photo (12 MP).
pub const PHOTO_W: usize = 3024;
pub const PHOTO_H: usize = 4032;

type M3 = [[f64; 3]; 3];

fn mat_mul(a: &M3, b: &M3) -> M3 {
    let mut m = [[0.0; 3]; 3];
    for r in 0..3 {
        for c in 0..3 {
            m[r][c] = (0..3).map(|k| a[r][k] * b[k][c]).sum();
        }
    }
    m
}

fn rot(axis: usize, deg: f64) -> M3 {
    let (s, c) = deg.to_radians().sin_cos();
    match axis {
        0 => [[1.0, 0.0, 0.0], [0.0, c, -s], [0.0, s, c]],
        1 => [[c, 0.0, s], [0.0, 1.0, 0.0], [-s, 0.0, c]],
        _ => [[c, -s, 0.0], [s, c, 0.0], [0.0, 0.0, 1.0]],
    }
}

fn to_h(m: &M3) -> Homography {
    let s = m[2][2];
    Homography { m: [m[0][0], m[0][1], m[0][2], m[1][0], m[1][1], m[1][2], m[2][0], m[2][1], m[2][2]].map(|v| v / s) }
}

/// Pinhole camera looking at the paper lying on a table: map.jpg pixel →
/// photo pixel. The camera is pitched by `tilt_x`, yawed by `tilt_y` and
/// rolled by `roll` (degrees), focal length `f_px` (iPhone main camera
/// ≈ 2900 px for 4032 px), at the distance where the paper fills `fill` of
/// the photo height, and the paper is centred in the frame.
pub fn camera_homography(
    map_w: usize,
    map_h: usize,
    tilt_x: f64,
    tilt_y: f64,
    roll: f64,
    f_px: f64,
    fill: f64,
) -> Homography {
    let r = mat_mul(&mat_mul(&rot(0, tilt_x), &rot(1, tilt_y)), &rot(2, roll));
    // map pixel → paper millimetres around the paper centre (z = 0 plane).
    let mm = 25.4 / 300.0;
    let a: M3 = [[mm, 0.0, -(map_w as f64) / 2.0 * mm], [0.0, mm, -(map_h as f64) / 2.0 * mm], [0.0, 0.0, 1.0]];
    let cam = |d: f64| -> Homography {
        let k: M3 = [[f_px, 0.0, PHOTO_W as f64 / 2.0], [0.0, f_px, PHOTO_H as f64 / 2.0], [0.0, 0.0, 1.0]];
        let ext: M3 = [[r[0][0], r[0][1], 0.0], [r[1][0], r[1][1], 0.0], [r[2][0], r[2][1], d]];
        to_h(&mat_mul(&mat_mul(&k, &ext), &a))
    };
    let corners = |h: &Homography| -> Vec<(f64, f64)> {
        [(0.0, 0.0), (map_w as f64, 0.0), (map_w as f64, map_h as f64), (0.0, map_h as f64)]
            .iter()
            .map(|&(x, y)| h.apply(x, y))
            .collect()
    };
    let mut d = 400.0;
    for _ in 0..30 {
        let c = corners(&cam(d));
        let span = c.iter().map(|p| p.1).fold(f64::MIN, f64::max) - c.iter().map(|p| p.1).fold(f64::MAX, f64::min);
        d *= span / (fill * PHOTO_H as f64);
    }
    let h = cam(d);
    let c = corners(&h);
    let (x0, x1) = (c.iter().map(|p| p.0).fold(f64::MAX, f64::min), c.iter().map(|p| p.0).fold(f64::MIN, f64::max));
    let (y0, y1) = (c.iter().map(|p| p.1).fold(f64::MAX, f64::min), c.iter().map(|p| p.1).fold(f64::MIN, f64::max));
    let (ox, oy) = (PHOTO_W as f64 / 2.0 - (x0 + x1) / 2.0, PHOTO_H as f64 / 2.0 - (y0 + y1) / 2.0);
    Homography { m: [1.0, 0.0, ox, 0.0, 1.0, oy, 0.0, 0.0, 1.0] }.mul(&h)
}

/// Small deterministic PRNG (xorshift64*) for test noise.
pub struct Rng(u64);

impl Rng {
    pub fn new(seed: u64) -> Rng {
        Rng(seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1)
    }

    pub fn next_f64(&mut self) -> f64 {
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        (self.0.wrapping_mul(0x2545_F491_4F6C_DD1D) >> 11) as f64 / (1u64 << 53) as f64
    }

    /// Standard normal (Box–Muller).
    pub fn normal(&mut self) -> f64 {
        let u = self.next_f64().max(1e-300);
        let v = self.next_f64();
        (-2.0 * u.ln()).sqrt() * (std::f64::consts::TAU * v).cos()
    }
}

/// Photograph `map` through `cam` (map pixel → photo pixel): a wooden table
/// around the paper, vignetting, a one-sided shadow (light × 0.55 … 1.0), a
/// warm colour cast, lens blur (σ = 1.2 px) and sensor noise (σ = 2.5).
pub fn synth_photo(map: &Rgb8, cam: &Homography, seed: u64) -> Rgb8 {
    use mapme_core::image::{warp_perspective, GrayF32};
    use rayon::prelude::*;
    let (pw, ph) = (PHOTO_W, PHOTO_H);
    let inv = cam.inverse().expect("camera homography");
    let photo = warp_perspective(map, &inv, pw, ph, [112, 82, 58]);
    let cast = [1.0f32, 0.90, 0.74].map(|c| c * 0.97);
    let light = |x: usize, y: usize| -> f32 {
        let (u, v) = ((x as f32 + 0.5) / pw as f32 * 2.0 - 1.0, (y as f32 + 0.5) / ph as f32 * 2.0 - 1.0);
        let vignette = 1.0 - 0.18 * (u * u + v * v);
        let s = ((0.8 * x as f32 + 0.25 * y as f32) / (0.7 * pw as f32)).clamp(0.0, 1.0);
        vignette * (0.55 + 0.45 * s * s * (3.0 - 2.0 * s))
    };
    let planes: Vec<GrayF32> = (0..3)
        .map(|c| {
            let mut p = photo.channel(c);
            p.data.par_chunks_mut(pw).enumerate().for_each(|(y, row)| {
                for (x, v) in row.iter_mut().enumerate() {
                    *v *= light(x, y) * cast[c];
                }
            });
            let mut b = p.gaussian_blur(1.2);
            b.data.par_chunks_mut(pw).enumerate().for_each(|(y, row)| {
                let mut rng = Rng::new(seed ^ ((c as u64) << 40) ^ (y as u64 + 1));
                for v in row.iter_mut() {
                    *v += 2.5 * rng.normal() as f32;
                }
            });
            b
        })
        .collect();
    Rgb8::from_channels(&planes[0], &planes[1], &planes[2]).unwrap()
}

/// Corners TL, TR, BR, BL of the paper in the photo.
pub fn paper_quad(cam: &Homography, map_w: usize, map_h: usize) -> [(f64, f64); 4] {
    [(0.0, 0.0), (map_w as f64, 0.0), (map_w as f64, map_h as f64), (0.0, map_h as f64)].map(|(x, y)| cam.apply(x, y))
}

/// The paper quad as the app's corner detection might find it: every corner
/// off by `frac` of the photo size, in fixed directions that do not cancel:
/// into the page for `frac` > 0 (the flattened image crops the page edge),
/// out of it for `frac` < 0 (a band of table shows around the page).
pub fn imperfect_quad(q: &[(f64, f64); 4], frac: f64) -> [(f64, f64); 4] {
    let d = frac * PHOTO_W.max(PHOTO_H) as f64;
    let dirs = [(1.0, 0.4), (-0.6, 1.0), (-1.0, -0.3), (0.5, -1.0)];
    let mut out = *q;
    for (o, (dx, dy)) in out.iter_mut().zip(dirs) {
        let n = f64::hypot(dx, dy);
        o.0 += d * dx / n;
        o.1 += d * dy / n;
    }
    out
}

/// Flatten the photo by a quad the way a document scanner does
/// (CIPerspectiveCorrection): output size from the longer of each pair of
/// opposite edges. Returns the image and the rectified pixel → photo pixel map.
pub fn rectify(photo: &Rgb8, quad: &[(f64, f64); 4]) -> (Rgb8, Homography) {
    let len = |a: (f64, f64), b: (f64, f64)| (a.0 - b.0).hypot(a.1 - b.1);
    let w = len(quad[0], quad[1]).max(len(quad[3], quad[2])).round();
    let h = len(quad[0], quad[3]).max(len(quad[1], quad[2])).round();
    let src = [(0.0, 0.0), (w, 0.0), (w, h), (0.0, h)];
    let hr = Homography::fit(&src, quad).expect("quad homography");
    (mapme_core::image::warp_perspective(photo, &hr, w as usize, h as usize, [0, 0, 0]), hr)
}

/// A photographed and flattened copy of the Grootkloof map.
pub struct PhotoCase {
    /// The flattened image (what the app hands to the core).
    pub image: Rgb8,
    /// Flattened pixel → map.jpg pixel.
    pub to_map: Homography,
    /// map.jpg pixel → flattened pixel.
    pub from_map: Homography,
}

impl PhotoCase {
    /// Ground-truth pixel → Mercator transform of the flattened image.
    pub fn truth(&self) -> Homography {
        truth().to_homography().mul(&self.to_map)
    }

    /// Max ground error over the map frame of a georeference of the flattened image.
    pub fn max_error_m(&self, h: &Homography) -> f64 {
        max_error_mapped(h, &self.from_map)
    }

    /// The OCR fixture moved into the flattened image (bounding boxes of the
    /// transformed boxes, text kept).
    pub fn ocr(&self) -> Vec<OcrObservation> {
        ocr_fixture()
            .into_iter()
            .map(|o| {
                let [x, y, w, h] = o.bbox;
                let pts = [(x, y), (x + w, y), (x + w, y + h), (x, y + h)].map(|(px, py)| self.from_map.apply(px, py));
                let x0 = pts.iter().map(|p| p.0).fold(f64::MAX, f64::min);
                let x1 = pts.iter().map(|p| p.0).fold(f64::MIN, f64::max);
                let y0 = pts.iter().map(|p| p.1).fold(f64::MAX, f64::min);
                let y1 = pts.iter().map(|p| p.1).fold(f64::MIN, f64::max);
                OcrObservation { bbox: [x0, y0, x1 - x0, y1 - y0], ..o }
            })
            .collect()
    }
}

/// Camera used by the photo tests: pitched 14°, yawed 6°, rolled 3°.
pub fn photo_camera(map: &Rgb8) -> Homography {
    camera_homography(map.w, map.h, 14.0, -6.0, 3.0, 2900.0, 0.86)
}

/// Photograph the map, then flatten it with corners off by `corner_error`
/// (fraction of the photo size).
pub fn photo_case(map: &Rgb8, photo: &Rgb8, cam: &Homography, corner_error: f64) -> PhotoCase {
    let quad = imperfect_quad(&paper_quad(cam, map.w, map.h), corner_error);
    let (image, rect_to_photo) = rectify(photo, &quad);
    let photo_to_map = cam.inverse().expect("camera inverse");
    let to_map = photo_to_map.mul(&rect_to_photo);
    let from_map = to_map.inverse().expect("composite inverse");
    PhotoCase { image, to_map, from_map }
}
