//! Page detection (`page::find_page`, `mm_detect_page`): the sheet of paper
//! in a photo, for flattening. Checked on the real phone photo of the printed
//! Grootkloof sheet (folded in four, on a wooden table; corners against
//! `fixtures/grootkloof/photo/truth.json` `page_quad`), on synthetic photos
//! (several cameras, tables and lights, clutter around and under the page),
//! and on images without a page to find. Run with `cargo test --release`.

mod common;
use common::*;
use mapme_core::ffi::{mm_detect_page, mm_string_free};
use mapme_core::image::Rgb8;
use mapme_core::page::{detect_page, find_page, PageQuad};
use serde_json::Value;
use std::ffi::CStr;
use std::path::Path;
use std::sync::{Mutex, MutexGuard, OnceLock};
use std::time::Instant;

/// Heavy tests run one at a time so timings are meaningful.
fn serial() -> MutexGuard<'static, ()> {
    static M: Mutex<()> = Mutex::new(());
    M.lock().unwrap_or_else(|e| e.into_inner())
}

fn load_jpeg(path: &Path) -> Rgb8 {
    let bytes = std::fs::read(path).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
    let mut dec = jpeg_decoder::Decoder::new(std::io::Cursor::new(bytes));
    let px = dec.decode().expect("decode jpeg");
    let info = dec.info().unwrap();
    assert_eq!(info.pixel_format, jpeg_decoder::PixelFormat::RGB24);
    Rgb8::from_rgb(info.width as usize, info.height as usize, px).unwrap()
}

/// The real phone photo (1932×2576) of the printed sheet.
fn real_photo() -> &'static Rgb8 {
    static P: OnceLock<Rgb8> = OnceLock::new();
    P.get_or_init(|| load_jpeg(&fixture_dir().join("photo/printed-map-photo.jpg")))
}

/// Ground-truth paper corners TL, TR, BR, BL of the real photo.
fn real_page_quad() -> [(f64, f64); 4] {
    let v: Value = serde_json::from_slice(&std::fs::read(fixture_dir().join("photo/truth.json")).unwrap()).unwrap();
    let q = v["page_quad"].as_array().unwrap();
    std::array::from_fn(|i| (q[i][0].as_f64().unwrap(), q[i][1].as_f64().unwrap()))
}

fn diagonal(w: usize, h: usize) -> f64 {
    (w as f64).hypot(h as f64)
}

fn corner_errors(found: &[(f64, f64); 4], truth: &[(f64, f64); 4]) -> [f64; 4] {
    std::array::from_fn(|i| (found[i].0 - truth[i].0).hypot(found[i].1 - truth[i].1))
}

fn quad_area(q: &[(f64, f64); 4]) -> f64 {
    (0..4).map(|i| q[i].0 * q[(i + 1) % 4].1 - q[(i + 1) % 4].0 * q[i].1).sum::<f64>().abs() / 2.0
}

/// The corners as seen in the image, TL first: clockwise on screen (positive
/// shoelace area in y-down coordinates), starting at the corner nearest the
/// top-left (smallest x + y) — unambiguous unless the page is turned ~45°.
fn as_seen(q: &[(f64, f64); 4]) -> [(f64, f64); 4] {
    let mut v = q.to_vec();
    let signed: f64 = (0..4).map(|i| v[i].0 * v[(i + 1) % 4].1 - v[(i + 1) % 4].0 * v[i].1).sum();
    if signed < 0.0 {
        v.reverse();
    }
    let first = (0..4).min_by(|&a, &b| (v[a].0 + v[a].1).total_cmp(&(v[b].0 + v[b].1))).unwrap();
    std::array::from_fn(|i| v[(first + i) % 4])
}

/// RGBA copy of `img` with `pad` extra bytes at the end of every row.
fn rgba_padded(img: &Rgb8, pad: usize) -> (Vec<u8>, usize) {
    let stride = img.w * 4 + pad;
    let mut out = vec![0xA5u8; stride * img.h];
    for y in 0..img.h {
        for x in 0..img.w {
            let o = y * stride + x * 4;
            out[o..o + 3].copy_from_slice(&img.data[(y * img.w + x) * 3..][..3]);
            out[o + 3] = 255;
        }
    }
    (out, stride)
}

/// `mm_detect_page` as the app calls it; the parsed JSON.
fn detect_ffi(rgba: *const u8, w: u32, h: u32, stride: u32) -> Value {
    let p = unsafe { mm_detect_page(rgba, w, h, stride) };
    assert!(!p.is_null());
    let s = unsafe { CStr::from_ptr(p) }.to_str().expect("UTF-8").to_owned();
    unsafe { mm_string_free(p) };
    serde_json::from_str(&s).unwrap_or_else(|e| panic!("not JSON ({e}): {s}"))
}

fn report(name: &str, p: &PageQuad, errs: &[f64; 4], diag: f64, ms: f64) {
    eprintln!(
        "[page] {name:10} corners off {:?} px = {:?} % of the diagonal; confidence {:.2}, coverage {:.3}, {ms:.0} ms",
        errs.map(|e| (e * 10.0).round() / 10.0),
        errs.map(|e| (e / diag * 1e4).round() / 100.0),
        p.confidence,
        p.coverage
    );
}

#[test]
fn real_photo_page_corners() {
    let _g = serial();
    let img = real_photo();
    let truth = real_page_quad();
    let diag = diagonal(img.w, img.h);
    let t = Instant::now();
    let p = find_page(img).unwrap_or_else(|e| panic!("no page found: {e}"));
    let ms = t.elapsed().as_secs_f64() * 1e3;
    let errs = corner_errors(&p.corners, &truth);
    report("real", &p, &errs, diag, ms);
    for (i, e) in errs.iter().enumerate() {
        // 0.5 % of the diagonal = 16 px; the folded sheet's edges wave by ±25 px.
        assert!(*e <= 0.005 * diag, "corner {i} off by {e:.1} px ({:.2} %)", 100.0 * e / diag);
    }
    // Folded paper: clear edges, not quite straight.
    assert!(p.confidence >= 0.5 && p.confidence < 0.95, "confidence {}", p.confidence);
    let area = quad_area(&p.corners) / (img.w * img.h) as f64;
    assert!((p.coverage - area).abs() < 1e-9 && (0.6..0.75).contains(&p.coverage), "coverage {}", p.coverage);
    assert_eq!(detect_page(img), Some(p));
}

#[test]
fn real_photo_through_the_c_abi() {
    let _g = serial();
    let img = real_photo();
    let want = find_page(img).unwrap();
    let (rgba, stride) = rgba_padded(img, 64);
    let r = detect_ffi(rgba.as_ptr(), img.w as u32, img.h as u32, stride as u32);
    assert_eq!(r["status"], "ok", "{r}");
    let quad = r["quad"].as_array().unwrap();
    assert_eq!(quad.len(), 4);
    for (q, w) in quad.iter().zip(want.corners) {
        let (x, y) = (q[0].as_f64().unwrap(), q[1].as_f64().unwrap());
        assert!((x - w.0).abs() < 1e-6 && (y - w.1).abs() < 1e-6, "{q} vs {w:?}");
    }
    assert!((r["confidence"].as_f64().unwrap() - want.confidence).abs() < 1e-9);
    assert!((r["coverage"].as_f64().unwrap() - want.coverage).abs() < 1e-9);
    let errs = corner_errors(&want.corners, &real_page_quad());
    assert!(errs.iter().all(|&e| e <= 0.005 * diagonal(img.w, img.h)), "{errs:?}");
}

fn synthetic_cases() -> Vec<(&'static str, Shot, Table)> {
    let base = Shot::iphone_12mp();
    vec![
        ("wood", base, Table::Wood),
        ("dark-24mp", Shot::app_24mp(), Table::Dark),
        ("grey", Shot { tilt_x: 25.0, roll: -8.0, fill: 0.65, ..base }, Table::Grey),
        ("clutter3", Shot { fill: 0.6, roll: 20.0, tilt_x: 10.0, ..base }, Table::Clutter(3)),
        ("clutter5", Shot { fill: 0.7, roll: -12.0, tilt_y: 15.0, ..base }, Table::Clutter(5)),
        ("clutter8", Shot { fill: 0.55, roll: 35.0, tilt_x: 20.0, ..base }, Table::Clutter(8)),
        ("landscape", Shot { w: 4032, h: 3024, roll: 92.0, fill: 0.8, ..base }, Table::Wood),
        ("wide-grey", Shot { f_px: 2200.0, tilt_x: 30.0, tilt_y: 10.0, fill: 0.7, ..base }, Table::Grey),
        ("dark-tight", Shot { fill: 0.92, ..base }, Table::Dark),
    ]
}

#[test]
fn synthetic_photos_on_tables() {
    let _g = serial();
    let map = load_map();
    for (name, shot, table) in synthetic_cases() {
        let cam = shot_camera(map.w, map.h, &shot);
        let img = synth_scene(&map, &cam, shot.w, shot.h, table, 11);
        let truth = as_seen(&paper_quad(&cam, map.w, map.h));
        let diag = diagonal(shot.w, shot.h);
        let t = Instant::now();
        let p = find_page(&img).unwrap_or_else(|e| panic!("{name}: no page found: {e}"));
        let ms = t.elapsed().as_secs_f64() * 1e3;
        let errs = corner_errors(&p.corners, &truth);
        report(name, &p, &errs, diag, ms);
        for (i, e) in errs.iter().enumerate() {
            assert!(*e <= 0.0025 * diag, "{name}: corner {i} off by {e:.2} px ({:.3} %)", 100.0 * e / diag);
        }
        assert!(p.confidence >= 0.8, "{name}: confidence {}", p.confidence);
        assert_eq!(as_seen(&p.corners), p.corners, "{name}: corner order");
    }
}

#[test]
fn no_page_or_part_of_one() {
    let _g = serial();
    let map = load_map();
    let base = Shot::iphone_12mp();
    // The paper lies outside the frame: only the table (and clutter) is seen.
    for table in [Table::Wood, Table::Grey, Table::Dark, Table::Clutter(3), Table::Clutter(8)] {
        let shot = Shot { shift: (1.5, 0.0), ..base };
        let cam = shot_camera(map.w, map.h, &shot);
        let img = synth_scene(&map, &cam, shot.w, shot.h, table, 5);
        let r = find_page(&img);
        assert!(r.is_err(), "{table:?}: found {r:?}");
    }
    // Half of the page runs off the photo: its visible part, with low confidence.
    let shot = Shot { shift: (0.45, 0.0), ..base };
    let cam = shot_camera(map.w, map.h, &shot);
    let img = synth_scene(&map, &cam, shot.w, shot.h, Table::Wood, 5);
    if let Ok(p) = find_page(&img) {
        assert!(p.confidence < 0.5, "cut-off page: confidence {}", p.confidence);
    }
}

#[test]
fn a_flat_scan_is_not_a_photo_of_a_page() {
    let _g = serial();
    let map = load_map();
    match find_page(&map) {
        Err(e) => eprintln!("[page] map.jpg: {e}"),
        Ok(p) => assert!(p.coverage >= 0.95, "map.jpg: {p:?}"),
    }
    let (rgba, stride) = rgba_padded(&map, 0);
    let r = detect_ffi(rgba.as_ptr(), map.w as u32, map.h as u32, stride as u32);
    assert!(r["status"] == "failed" || r["coverage"].as_f64().unwrap() >= 0.95, "{r}");
}

#[test]
fn blank_tiny_and_invalid_images() {
    for v in [0u8, 128, 255] {
        let img = Rgb8::from_rgb(640, 480, vec![v; 640 * 480 * 3]).unwrap();
        assert!(find_page(&img).is_err(), "uniform {v}");
        let (rgba, stride) = rgba_padded(&img, 16);
        let r = detect_ffi(rgba.as_ptr(), 640, 480, stride as u32);
        assert_eq!(r["status"], "failed", "{r}");
        assert!(r["message"].as_str().is_some_and(|m| !m.is_empty()));
    }
    // Noise is not a page either.
    let mut rng = Rng::new(3);
    let noise: Vec<u8> = (0..400 * 300 * 3).map(|_| (rng.next_f64() * 255.0) as u8).collect();
    assert!(find_page(&Rgb8::from_rgb(400, 300, noise).unwrap()).is_err());
    // Too small, or not what the header promises.
    for (w, h) in [(1usize, 1usize), (10, 10), (47, 400), (400, 2)] {
        let img = Rgb8::from_rgb(w, h, vec![200; w * h * 3]).unwrap();
        assert!(find_page(&img).is_err(), "{w}x{h}");
        let (rgba, stride) = rgba_padded(&img, 0);
        assert_eq!(detect_ffi(rgba.as_ptr(), w as u32, h as u32, stride as u32)["status"], "failed");
    }
    assert!(find_page(&Rgb8 { w: 100, h: 100, data: vec![0; 99] }).is_err());
    let px = vec![0u8; 64 * 64 * 4];
    assert_eq!(detect_ffi(std::ptr::null(), 64, 64, 256)["status"], "failed");
    assert_eq!(detect_ffi(px.as_ptr(), 0, 64, 256)["status"], "failed");
    assert_eq!(detect_ffi(px.as_ptr(), 64, 0, 256)["status"], "failed");
    assert_eq!(detect_ffi(px.as_ptr(), 64, 64, 255)["status"], "failed");
    // Rejected from the size alone, before any pixel is read.
    assert_eq!(detect_ffi(px.as_ptr(), 40_000, 64, 160_000)["status"], "failed");
    assert_eq!(detect_ffi(px.as_ptr(), 30_000, 30_000, 120_000)["status"], "failed");
}

#[test]
fn speed_on_a_24_megapixel_photo() {
    let _g = serial();
    let map = load_map();
    let shot = Shot::app_24mp();
    let cam = shot_camera(map.w, map.h, &shot);
    let img = synth_scene(&map, &cam, shot.w, shot.h, Table::Wood, 9);
    let (rgba, stride) = rgba_padded(&img, 0);
    let truth = as_seen(&paper_quad(&cam, map.w, map.h));
    let mut best = f64::MAX;
    for _ in 0..4 {
        let t = Instant::now();
        let r = detect_ffi(rgba.as_ptr(), img.w as u32, img.h as u32, stride as u32);
        best = best.min(t.elapsed().as_secs_f64() * 1e3);
        assert_eq!(r["status"], "ok", "{r}");
        let q = r["quad"].as_array().unwrap();
        for (c, w) in q.iter().zip(truth) {
            assert!((c[0].as_f64().unwrap() - w.0).hypot(c[1].as_f64().unwrap() - w.1) < 3.0, "{c} vs {w:?}");
        }
    }
    eprintln!("[page] {}x{} RGBA through mm_detect_page: {best:.0} ms (best of 4)", img.w, img.h);
    assert!(best < time_budget_ms(150.0), "page detection took {best:.0} ms on 24 MP");
}
