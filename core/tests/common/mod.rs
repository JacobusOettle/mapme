//! Shared fixture helpers for integration tests (fixtures/grootkloof).
#![allow(dead_code)]

use mapme_core::dem::{DemStore, TileId};
use mapme_core::geo::{latlon_to_merc, Affine, Homography};
use mapme_core::image::Rgb8;
use mapme_core::labels::OcrObservation;
use std::path::PathBuf;

pub mod synth;

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

// ---------------------------------------------------------------------------
// Photos for page detection: other cameras, tables and clutter
// ---------------------------------------------------------------------------

/// A phone camera and how it frames the paper.
#[derive(Clone, Copy, Debug)]
pub struct Shot {
    /// Photo size in pixels.
    pub w: usize,
    pub h: usize,
    /// Focal length in pixels.
    pub f_px: f64,
    /// Pitch, yaw and roll of the camera (degrees).
    pub tilt_x: f64,
    pub tilt_y: f64,
    pub roll: f64,
    /// Height of the paper's outline as a fraction of the photo height.
    pub fill: f64,
    /// Offset of the paper's outline from the photo centre (fraction of the photo size).
    pub shift: (f64, f64),
}

impl Shot {
    /// The default `synth_photo` camera (12 MP, see `photo_camera`).
    pub fn iphone_12mp() -> Shot {
        Shot {
            w: PHOTO_W,
            h: PHOTO_H,
            f_px: 2900.0,
            tilt_x: 14.0,
            tilt_y: -6.0,
            roll: 3.0,
            fill: 0.86,
            shift: (0.0, 0.0),
        }
    }

    /// The app's `SyntheticPhoto.Camera()` (24 MP, 26 mm, page 75 % of the height, off-centre).
    pub fn app_24mp() -> Shot {
        let (w, h) = (4284usize, 5712usize);
        let f_px = 26.0 / 43.266_615 * ((w * w + h * h) as f64).sqrt();
        Shot { w, h, f_px, tilt_x: 12.0, tilt_y: -7.0, roll: 2.5, fill: 0.75, shift: (0.015, -0.045) }
    }
}

/// map.jpg pixel → photo pixel for `shot` (generalises `camera_homography`).
pub fn shot_camera(map_w: usize, map_h: usize, shot: &Shot) -> Homography {
    let r = mat_mul(&mat_mul(&rot(0, shot.tilt_x), &rot(1, shot.tilt_y)), &rot(2, shot.roll));
    let mm = 25.4 / 300.0;
    let a: M3 = [[mm, 0.0, -(map_w as f64) / 2.0 * mm], [0.0, mm, -(map_h as f64) / 2.0 * mm], [0.0, 0.0, 1.0]];
    let (pw, ph) = (shot.w as f64, shot.h as f64);
    let cam = |d: f64| -> Homography {
        let k: M3 = [[shot.f_px, 0.0, pw / 2.0], [0.0, shot.f_px, ph / 2.0], [0.0, 0.0, 1.0]];
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
        d *= span / (shot.fill * ph);
    }
    let h = cam(d);
    let c = corners(&h);
    let (x0, x1) = (c.iter().map(|p| p.0).fold(f64::MAX, f64::min), c.iter().map(|p| p.0).fold(f64::MIN, f64::max));
    let (y0, y1) = (c.iter().map(|p| p.1).fold(f64::MAX, f64::min), c.iter().map(|p| p.1).fold(f64::MIN, f64::max));
    let ox = pw / 2.0 - (x0 + x1) / 2.0 + shot.shift.0 * pw;
    let oy = ph / 2.0 - (y0 + y1) / 2.0 + shot.shift.1 * ph;
    Homography { m: [1.0, 0.0, ox, 0.0, 1.0, oy, 0.0, 0.0, 1.0] }.mul(&h)
}

/// What the paper lies on.
#[derive(Clone, Copy, Debug)]
pub enum Table {
    /// Dark wood, as in the app's `SyntheticPhoto` (gradient 51,36,26 → 28,20,15).
    Dark,
    /// Mid-brown wood with grain (the colour `synth_photo` uses).
    Wood,
    /// Neutral mid-grey: paper and table differ in brightness only.
    Grey,
    /// A grey-green desk with random objects (rectangles of random colour, size and angle), some of
    /// them partly under the paper.
    Clutter(u64),
}

struct Object {
    c: (f64, f64),
    half: (f64, f64),
    dir: (f64, f64),
    rgb: [f32; 3],
}

fn clutter_objects(seed: u64, w: f64, h: f64) -> Vec<Object> {
    let mut rng = Rng::new(seed);
    let n = 10 + (rng.next_f64() * 6.0) as usize;
    (0..n)
        .map(|_| {
            let c = (rng.next_f64() * w, rng.next_f64() * h);
            let half = (w * (0.03 + 0.12 * rng.next_f64()), h * (0.015 + 0.08 * rng.next_f64()));
            let a = rng.next_f64() * std::f64::consts::PI;
            // Any colour but paper white (a second sheet next to the page is another story).
            let rgb = loop {
                let v = [0, 1, 2].map(|_| 20.0 + 215.0 * rng.next_f64() as f32);
                let (mx, mn) = (v[0].max(v[1]).max(v[2]), v[0].min(v[1]).min(v[2]));
                if mn < 150.0 || mx - mn > 60.0 {
                    break v;
                }
            };
            Object { c, half, dir: (a.cos(), a.sin()), rgb }
        })
        .collect()
}

/// Photograph `map` through `cam` (map pixel → photo pixel) onto a `w`×`h` photo: the paper on
/// `table` with a soft drop shadow, then the lighting of `synth_photo` (vignetting, a one-sided
/// shadow ×0.55 … 1, a warm cast), lens blur σ 1.2 px and sensor noise σ 2.5.
pub fn synth_scene(map: &Rgb8, cam: &Homography, w: usize, h: usize, table: Table, seed: u64) -> Rgb8 {
    use mapme_core::image::GrayF32;
    use rayon::prelude::*;
    let inv = cam.inverse().expect("camera homography");
    let quad = paper_quad(cam, map.w, map.h);
    let (wf, hf) = (w as f64, h as f64);
    // Drop shadow: the paper outline shifted down and right, blurred.
    let (sx, sy, sigma) = (0.004 * wf, 0.006 * wf, 0.007 * wf);
    let edges: Vec<((f64, f64), (f64, f64))> = (0..4)
        .map(|i| {
            let (a, b) = (quad[i], quad[(i + 1) % 4]);
            let len = (b.0 - a.0).hypot(b.1 - a.1);
            ((a.0 + sx, a.1 + sy), ((b.1 - a.1) / len, -(b.0 - a.0) / len))
        })
        .collect();
    let shadow = |x: f64, y: f64| -> f32 {
        let d = edges.iter().map(|&(a, n)| (x - a.0) * n.0 + (y - a.1) * n.1).fold(f64::MIN, f64::max);
        (0.5 / (1.0 + (1.702 * d / sigma).exp())) as f32
    };
    let objects = match table {
        Table::Clutter(s) => clutter_objects(s, wf, hf),
        _ => Vec::new(),
    };
    let table_rgb = |x: f64, y: f64| -> [f32; 3] {
        match table {
            Table::Dark => {
                let t = ((x / wf + y / hf) / 2.0) as f32;
                [51.0 - 23.0 * t, 36.0 - 16.0 * t, 26.0 - 11.0 * t]
            }
            Table::Wood => {
                let grain = 1.0
                    + 0.07 * (std::f64::consts::TAU * (y + 25.0 * (x / 300.0).sin()) / 37.0).sin()
                    + 0.05 * (x / 911.0 + y / 1500.0).sin();
                [112.0, 82.0, 58.0].map(|c| c * grain as f32)
            }
            Table::Grey => {
                let mottle = 1.0 + 0.02 * (x / 97.0).sin() * (y / 131.0).cos();
                [128.0 * mottle as f32; 3]
            }
            Table::Clutter(_) => {
                let mut c = [92.0f32, 104.0, 96.0];
                for o in &objects {
                    let (dx, dy) = (x - o.c.0, y - o.c.1);
                    let (u, v) = (dx * o.dir.0 + dy * o.dir.1, -dx * o.dir.1 + dy * o.dir.0);
                    if u.abs() <= o.half.0 && v.abs() <= o.half.1 {
                        c = o.rgb;
                    }
                }
                c
            }
        }
    };
    let mut scene = vec![[0.0f32; 3]; w * h];
    scene.par_chunks_mut(w).enumerate().for_each(|(y, row)| {
        let yc = y as f64 + 0.5;
        for (x, px) in row.iter_mut().enumerate() {
            let xc = x as f64 + 0.5;
            let (u, v) = inv.apply(xc, yc);
            *px = match map.sample(u, v) {
                Some(c) => c,
                None => {
                    let s = 1.0 - shadow(xc, yc);
                    table_rgb(xc, yc).map(|c| c * s)
                }
            };
        }
    });
    let cast = [1.0f32, 0.90, 0.74].map(|c| c * 0.97);
    let light = |x: usize, y: usize| -> f32 {
        let (u, v) = ((x as f32 + 0.5) / w as f32 * 2.0 - 1.0, (y as f32 + 0.5) / h as f32 * 2.0 - 1.0);
        let vignette = 1.0 - 0.18 * (u * u + v * v);
        let s = ((0.8 * x as f32 + 0.25 * y as f32) / (0.7 * w as f32)).clamp(0.0, 1.0);
        vignette * (0.55 + 0.45 * s * s * (3.0 - 2.0 * s))
    };
    let planes: Vec<GrayF32> = (0..3)
        .map(|c| {
            let mut p = GrayF32::new(w, h);
            p.data.par_chunks_mut(w).zip(scene.par_chunks(w)).enumerate().for_each(|(y, (row, src))| {
                for (x, (v, s)) in row.iter_mut().zip(src).enumerate() {
                    *v = s[c] * light(x, y) * cast[c];
                }
            });
            let mut b = p.gaussian_blur(1.2);
            b.data.par_chunks_mut(w).enumerate().for_each(|(y, row)| {
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

/// Write `img` as a PNG (debugging aid).
pub fn save_png(img: &Rgb8, path: &std::path::Path) {
    let file = std::io::BufWriter::new(std::fs::File::create(path).expect("create png"));
    let mut enc = png::Encoder::new(file, img.w as u32, img.h as u32);
    enc.set_color(png::ColorType::Rgb);
    enc.set_depth(png::BitDepth::Eight);
    enc.write_header().unwrap().write_image_data(&img.data).unwrap();
}

// ---------------------------------------------------------------------------
// Image helpers and the real photo (dark map tests)
// ---------------------------------------------------------------------------

/// Decode a JPEG file as packed RGB.
pub fn load_jpeg_file(path: &std::path::Path) -> Rgb8 {
    let bytes = std::fs::read(path).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
    let mut dec = jpeg_decoder::Decoder::new(std::io::Cursor::new(bytes));
    let px = dec.decode().expect("decode jpeg");
    let info = dec.info().unwrap();
    assert_eq!(info.pixel_format, jpeg_decoder::PixelFormat::RGB24);
    Rgb8::from_rgb(info.width as usize, info.height as usize, px).unwrap()
}

/// The real phone photo (1932×2576) of the printed sheet, folded, on a wooden table.
pub fn real_photo_image() -> Rgb8 {
    load_jpeg_file(&fixture_dir().join("photo/printed-map-photo.jpg"))
}

/// Ground-truth paper corners TL, TR, BR, BL of the real photo (`truth.json` `page_quad`).
pub fn real_photo_page_quad() -> [(f64, f64); 4] {
    let v: serde_json::Value =
        serde_json::from_slice(&std::fs::read(fixture_dir().join("photo/truth.json")).unwrap()).unwrap();
    let q = v["page_quad"].as_array().unwrap();
    std::array::from_fn(|i| (q[i][0].as_f64().unwrap(), q[i][1].as_f64().unwrap()))
}

/// The real photo flattened by its page corners, as the app does (`rectify`, ~1700×2232).
pub fn real_photo_flattened() -> Rgb8 {
    rectify(&real_photo_image(), &real_photo_page_quad()).0
}

/// Packed RGB → RGBA (alpha 255).
pub fn rgba_from_rgb(img: &Rgb8) -> Vec<u8> {
    let mut out = vec![255u8; img.w * img.h * 4];
    for (o, i) in out.chunks_exact_mut(4).zip(img.data.chunks_exact(3)) {
        o[..3].copy_from_slice(i);
    }
    out
}

/// Tightly packed RGBA → packed RGB.
pub fn rgb_from_rgba(rgba: &[u8], w: usize, h: usize) -> Rgb8 {
    let data = rgba.chunks_exact(4).take(w * h).flat_map(|p| [p[0], p[1], p[2]]).collect();
    Rgb8::from_rgb(w, h, data).unwrap()
}

/// Pixels `[x, x + w) × [y, y + h)` of `img`.
pub fn crop_rgb(img: &Rgb8, x: usize, y: usize, w: usize, h: usize) -> Rgb8 {
    assert!(x + w <= img.w && y + h <= img.h, "crop outside the image");
    let mut data = Vec::with_capacity(w * h * 3);
    for row in y..y + h {
        data.extend_from_slice(&img.data[(row * img.w + x) * 3..(row * img.w + x + w) * 3]);
    }
    Rgb8::from_rgb(w, h, data).unwrap()
}

/// Box-downsample by an integer factor (the image is cut to a multiple of it).
pub fn box_downsample(img: &Rgb8, f: usize) -> Rgb8 {
    let (w, h) = (img.w / f, img.h / f);
    let n = (f * f) as u32;
    let mut data = vec![0u8; w * h * 3];
    for y in 0..h {
        for x in 0..w {
            for c in 0..3 {
                let mut s = 0u32;
                for dy in 0..f {
                    for dx in 0..f {
                        s += img.data[((y * f + dy) * img.w + x * f + dx) * 3 + c] as u32;
                    }
                }
                data[(y * w + x) * 3 + c] = ((s + n / 2) / n) as u8;
            }
        }
    }
    Rgb8::from_rgb(w, h, data).unwrap()
}

/// `a` and `b` next to each other, tops aligned, with a `gap` px mid-grey gutter.
pub fn side_by_side(a: &Rgb8, b: &Rgb8, gap: usize) -> Rgb8 {
    let (w, h) = (a.w + gap + b.w, a.h.max(b.h));
    let mut data = vec![128u8; w * h * 3];
    for (img, x0) in [(a, 0), (b, a.w + gap)] {
        for y in 0..img.h {
            data[(y * w + x0) * 3..(y * w + x0 + img.w) * 3]
                .copy_from_slice(&img.data[y * img.w * 3..(y + 1) * img.w * 3]);
        }
    }
    Rgb8::from_rgb(w, h, data).unwrap()
}

// ---------------------------------------------------------------------------
// Other sheets of the series (fixtures/castle-gorge, fixtures/mhlabatini)
// ---------------------------------------------------------------------------

/// `fixtures/<name>`.
pub fn sheet_dir(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../fixtures").join(name)
}

/// The `observations` of `<dir>/ocr.json`.
pub fn load_ocr(dir: &std::path::Path) -> Vec<OcrObservation> {
    let v: serde_json::Value = serde_json::from_slice(&std::fs::read(dir.join("ocr.json")).unwrap()).unwrap();
    serde_json::from_value(v["observations"].clone()).unwrap()
}

/// A graticule intersection: (pixel (x, y), (lat, lon)).
pub type GraticulePoint = ((f64, f64), (f64, f64));

/// The graticule intersections of `<dir>/truth.json`.
pub fn graticule_truth(dir: &std::path::Path) -> Vec<GraticulePoint> {
    let v: serde_json::Value = serde_json::from_slice(&std::fs::read(dir.join("truth.json")).unwrap()).unwrap();
    let mut out = Vec::new();
    for la in v["lat_lines"].as_array().unwrap() {
        for lo in v["lon_lines"].as_array().unwrap() {
            out.push((
                (lo["x"].as_f64().unwrap(), la["y"].as_f64().unwrap()),
                (la["lat"].as_f64().unwrap(), lo["lon"].as_f64().unwrap()),
            ));
        }
    }
    out
}

/// Ground distance (m) at the worst of `pts` (`graticule_truth`) between where `h`
/// (pixel → Web Mercator) puts the intersection and where it is.
pub fn max_graticule_error_m(h: &Homography, pts: &[GraticulePoint]) -> f64 {
    use mapme_core::geo::{haversine_m, merc_to_latlon, LatLon};
    pts.iter()
        .map(|&((x, y), (lat, lon))| {
            let (mx, my) = h.apply(x, y);
            let (la, lo) = merc_to_latlon(mx, my);
            if la.is_finite() && lo.is_finite() {
                haversine_m(LatLon::new(la, lo), LatLon::new(lat, lon))
            } else {
                f64::INFINITY
            }
        })
        .fold(0.0, f64::max)
}
