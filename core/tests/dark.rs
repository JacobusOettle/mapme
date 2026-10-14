//! Dark map rendering (`core/src/dark.rs`, `docs/ARCHITECTURE.md` "Dark map"):
//! the per-pixel formula's properties on the exact path, the lookup table
//! against the formula, seamless tiles and pyramid levels, the profiles of the
//! printed map, the real phone photo and a synthetic photo (with the evenness
//! of their paper after rendering), the C ABI, determinism and speed.
//!
//! Run with `cargo test --release --test dark -- --nocapture` to see the
//! metrics; `dark_dump` (ignored) writes before/after images to `$DARK_DUMP`.

mod common;
use common::*;
use mapme_core::dark::*;
use mapme_core::ffi::*;
use mapme_core::image::Rgb8;
use serde_json::Value;
use std::ffi::{c_char, CStr, CString};
use std::sync::{Mutex, MutexGuard, OnceLock};
use std::time::Instant;

/// Heavy tests run one at a time so timings are meaningful.
fn serial() -> MutexGuard<'static, ()> {
    static M: Mutex<()> = Mutex::new(());
    M.lock().unwrap_or_else(|e| e.into_inner())
}

/// Photo-like tone parameters: washed-out ink, noisy paper, the largest chroma gain.
const PHOTO: DarkParams = DarkParams { ink: 0.40, dead: 0.06, gain: 1.8 };

fn hex(c: u32) -> [f64; 3] {
    [(c >> 16) & 255, (c >> 8) & 255, c & 255].map(|v| v as f64 / 255.0)
}

/// OKLab of the dark rendering of a flat-fielded pixel (exact path).
fn out_lab(x: [f64; 3], p: &DarkParams) -> [f64; 3] {
    srgb_to_oklab(dark_pixel(x, p))
}

fn chroma(lab: [f64; 3]) -> f64 {
    lab[1].hypot(lab[2])
}

/// Absolute difference of the OKLab hues of two colours, degrees.
fn hue_diff(a: [f64; 3], b: [f64; 3]) -> f64 {
    let d = (a[2].atan2(a[1]) - b[2].atan2(b[1])).to_degrees();
    ((d + 540.0) % 360.0 - 180.0).abs()
}

fn lab_u8(c: &[u8]) -> [f64; 3] {
    srgb_to_oklab([c[0], c[1], c[2]].map(|v| v as f64 / 255.0))
}

/// Render an RGB image as a tile covering `rect` of the map (through RGBA, like the app).
fn render_rgb(style: &DarkStyle, img: &Rgb8, rect: TileRect) -> Rgb8 {
    let mut rgba = rgba_from_rgb(img);
    style.render(&mut rgba, img.w, img.h, img.w * 4, rect).unwrap();
    rgb_from_rgba(&rgba, img.w, img.h)
}

fn style_of(params: DarkParams) -> DarkStyle {
    DarkStyle::new(&DarkProfile { params, paper: PaperGrid::uniform([1.0; 3]), edge: None })
}

fn take_json(p: *mut c_char) -> Value {
    assert!(!p.is_null());
    let s = unsafe { CStr::from_ptr(p) }.to_str().expect("UTF-8").to_owned();
    unsafe { mm_string_free(p) };
    serde_json::from_str(&s).unwrap_or_else(|e| panic!("not JSON ({e}): {s}"))
}

fn print_profile(name: &str, p: &DarkProfile) {
    let edge = match &p.edge {
        None => "no edge bands".to_string(),
        Some(e) => {
            let max = |v: &[f32]| v.iter().copied().fold(0.0f32, f32::max);
            let (n, k) = (400usize, 400usize);
            let outside = (0..n * k)
                .filter(|i| {
                    e.outside((i % n) as f64 / n as f64 + 0.5 / n as f64, (i / n) as f64 / k as f64 + 0.5 / k as f64)
                })
                .count();
            format!(
                "edge bands up to left {:.4} right {:.4} top {:.4} bottom {:.4} ({:.2} % of the map)",
                max(&e.left),
                max(&e.right),
                max(&e.top),
                max(&e.bottom),
                100.0 * outside as f64 / (n * k) as f64
            )
        }
    };
    println!(
        "{name}: ink {:.4} (flare {:.4}), dead {:.4}, gain {:.4}, paper grid {}×{}, {edge}, profile JSON {} bytes",
        p.params.ink,
        p.params.flare(),
        p.params.dead,
        p.params.gain,
        p.paper.cols,
        p.paper.rows,
        p.to_json().len()
    );
}

// ---------------------------------------------------------------------------
// Fixtures
// ---------------------------------------------------------------------------

struct Fixture {
    /// The image tiles are cut from (full resolution).
    image: Rgb8,
    profile: DarkProfile,
    json: String,
}

/// map.jpg (3507×4960) and its profile from the app's overview (box-downsampled by 3 to
/// 1169×1653, long side ≤ 2048).
fn map_fixture() -> &'static Fixture {
    static F: OnceLock<Fixture> = OnceLock::new();
    F.get_or_init(|| {
        let image = load_map();
        let profile = DarkProfile::from_rgb8(&box_downsample(&image, 3)).unwrap();
        Fixture { json: profile.to_json(), image, profile }
    })
}

/// The real photo flattened by its page corners (1700×2232) and its profile.
fn photo_fixture() -> &'static Fixture {
    static F: OnceLock<Fixture> = OnceLock::new();
    F.get_or_init(|| {
        let image = real_photo_flattened();
        let profile = DarkProfile::from_rgb8(&image).unwrap();
        Fixture { json: profile.to_json(), image, profile }
    })
}

/// A synthetic photo of map.jpg (one-sided shadow ×0.55…1, vignetting, warm cast, blur,
/// noise), flattened by its exact paper corners; with the flattened pixel → map.jpg pixel map.
struct SynthFixture {
    image: Rgb8,
    to_map: mapme_core::geo::Homography,
    profile: DarkProfile,
}

fn synth_fixture() -> &'static SynthFixture {
    static F: OnceLock<SynthFixture> = OnceLock::new();
    F.get_or_init(|| {
        let map = &map_fixture().image;
        let cam = photo_camera(map);
        let photo = synth_photo(map, &cam, 11);
        let case = photo_case(map, &photo, &cam, 0.0);
        let profile = DarkProfile::from_rgb8(&case.image).unwrap();
        SynthFixture { image: case.image, to_map: case.to_map, profile }
    })
}

// ---------------------------------------------------------------------------
// Paper masks and evenness
// ---------------------------------------------------------------------------

/// Separable max (or min) filter of radius `r` over a w×h plane, edges clamped.
fn rank_filter(v: &[f32], w: usize, h: usize, r: usize, max: bool) -> Vec<f32> {
    let pick = |a: f32, b: f32| if max { a.max(b) } else { a.min(b) };
    let init = if max { f32::MIN } else { f32::MAX };
    let mut tmp = vec![0.0f32; w * h];
    for y in 0..h {
        for x in 0..w {
            let (x0, x1) = (x.saturating_sub(r), (x + r).min(w - 1));
            tmp[y * w + x] = v[y * w + x0..=y * w + x1].iter().copied().fold(init, pick);
        }
    }
    let mut out = vec![0.0f32; w * h];
    for y in 0..h {
        let (y0, y1) = (y.saturating_sub(r), (y + r).min(h - 1));
        for x in 0..w {
            out[y * w + x] = (y0..=y1).map(|yy| tmp[yy * w + x]).fold(init, pick);
        }
    }
    out
}

/// Paper pixels of a photo judged from the photo itself, independently of the profile:
/// within 7 % of the brightest pixel within 20 px, no pixel more than 12 % darker within
/// 4 px (no ink nearby), away from the page border (3 %).
fn photo_paper_mask(img: &Rgb8) -> Vec<bool> {
    let (w, h) = (img.w, img.h);
    let y: Vec<f32> =
        img.data.chunks_exact(3).map(|p| 0.2126 * p[0] as f32 + 0.7152 * p[1] as f32 + 0.0722 * p[2] as f32).collect();
    let local_max = rank_filter(&y, w, h, 20, true);
    let local_min = rank_filter(&y, w, h, 4, false);
    let (bx, by) = (w * 3 / 100, h * 3 / 100);
    (0..w * h)
        .map(|i| {
            let (px, py) = (i % w, i / w);
            let m = local_max[i];
            px >= bx
                && px < w - bx
                && py >= by
                && py < h - by
                && m > 80.0
                && y[i] >= 0.93 * m
                && local_min[i] >= 0.88 * m
        })
        .collect()
}

/// Paper pixels of the flattened synthetic photo from the truth: the map.jpg pixel it shows
/// is white paper with no ink within 10 map px.
fn synth_paper_mask(s: &SynthFixture, map: &Rgb8) -> Vec<bool> {
    let white: Vec<f32> = map
        .data
        .chunks_exact(3)
        .map(|p| {
            let (mx, mn) = (p[0].max(p[1]).max(p[2]), p[0].min(p[1]).min(p[2]));
            if mn >= 225 && mx - mn <= 30 {
                1.0
            } else {
                0.0
            }
        })
        .collect();
    let clean = rank_filter(&white, map.w, map.h, 10, false);
    let (w, h) = (s.image.w, s.image.h);
    let (bx, by) = (w * 2 / 100, h * 2 / 100);
    (0..w * h)
        .map(|i| {
            let (x, y) = (i % w, i / w);
            if x < bx || x >= w - bx || y < by || y >= h - by {
                return false;
            }
            let (mx, my) = s.to_map.apply(x as f64 + 0.5, y as f64 + 0.5);
            mx >= 0.0
                && my >= 0.0
                && (mx as usize) < map.w
                && (my as usize) < map.h
                && clean[my as usize * map.w + mx as usize] > 0.5
        })
        .collect()
}

/// Mean and standard deviation of OKLab L over the masked pixels.
fn l_stats(img: &Rgb8, mask: &[bool]) -> (f64, f64) {
    let ls: Vec<f64> = img.data.chunks_exact(3).zip(mask).filter(|(_, &m)| m).map(|(p, _)| lab_u8(p)[0]).collect();
    let n = ls.len() as f64;
    let mean = ls.iter().sum::<f64>() / n;
    (mean, (ls.iter().map(|l| (l - mean).powi(2)).sum::<f64>() / n).sqrt())
}

/// Input and output paper evenness (std of L over the paper pixels), printed.
fn evenness(name: &str, input: &Rgb8, output: &Rgb8, mask: &[bool]) -> (f64, f64) {
    let n = mask.iter().filter(|&&m| m).count();
    let (mi, si) = l_stats(input, mask);
    let (mo, so) = l_stats(output, mask);
    println!(
        "{name}: {n} paper pixels ({:.1} %): input L {mi:.3} ± {si:.4}, output L {mo:.3} ± {so:.4} (night paper L {:.3})",
        100.0 * n as f64 / mask.len() as f64,
        night_paper_lab()[0]
    );
    (si, so)
}

// ---------------------------------------------------------------------------
// The formula (exact path)
// ---------------------------------------------------------------------------

#[test]
fn paper_of_any_tint_becomes_night_paper() {
    let night = NIGHT_PAPER_SRGB;
    for p in [DarkParams::PRINT, PHOTO, DarkParams { ink: 0.65, dead: 0.12, gain: 1.0 }] {
        let out = dark_pixel([1.0, 1.0, 1.0], &p);
        for (o, n) in out.iter().zip(night) {
            assert!((o - n).abs() <= 1.0 / 255.0, "{p:?}: {out:?}");
        }
    }
    // Tinted paper under slightly uneven light, with ink: after the flat field the paper is white
    // wherever it is, and renders as the night paper (#212124).
    let (w, h) = (640, 800);
    for tint in [[255.0f32, 255.0, 255.0], [240.0, 230.0, 205.0], [205.0, 212.0, 222.0], [170.0, 160.0, 140.0]] {
        let mut img = Rgb8::from_rgb(w, h, vec![0; w * h * 3]).unwrap();
        let mut ink = vec![false; w * h];
        for y in 0..h {
            for x in 0..w {
                let light = 0.88 + 0.12 * (x as f32 / w as f32) * (0.8 + 0.2 * (y as f32 / h as f32));
                let line = x % 48 < 2 || (y + x / 3) % 61 < 2;
                ink[y * w + x] = line;
                let c = if line { [70.0, 50.0, 40.0] } else { tint };
                for (o, v) in img.data[(y * w + x) * 3..(y * w + x) * 3 + 3].iter_mut().zip(c) {
                    *o = (v * light).round() as u8;
                }
            }
        }
        let profile = DarkProfile::from_rgb8(&img).unwrap();
        let style = DarkStyle::from_json(&profile.to_json()).unwrap();
        let out = render_rgb(&style, &img, TileRect::FULL);
        let mut worst = 0i32;
        for y in 0..h {
            for x in 0..w {
                let near_ink = (y.saturating_sub(3)..(y + 4).min(h))
                    .any(|yy| (x.saturating_sub(3)..(x + 4).min(w)).any(|xx| ink[yy * w + xx]));
                if near_ink {
                    continue;
                }
                let o = &out.data[(y * w + x) * 3..(y * w + x) * 3 + 3];
                for (k, n) in [33, 33, 36].iter().enumerate() {
                    worst = worst.max((o[k] as i32 - n).abs());
                }
            }
        }
        println!(
            "tint {tint:?}: ink {:.3} dead {:.3}, worst paper deviation {worst}",
            profile.params.ink, profile.params.dead
        );
        assert!(worst <= 1, "paper of tint {tint:?} off the night paper by {worst}");
    }
}

#[test]
fn black_ink_becomes_light_grey() {
    for p in [DarkParams::PRINT, PHOTO, map_fixture().profile.params] {
        let lab = out_lab([0.0; 3], &p);
        assert!(lab[0] >= 0.88 && chroma(lab) < 0.01, "{p:?}: black → {lab:?}");
        // The map's darkest ink (a grey at its `ink` lightness) is drawn as light as black.
        let darkest = [linear_to_srgb(p.ink.powi(3)); 3];
        let lab = out_lab(darkest, &p);
        assert!(lab[0] >= 0.88, "{p:?}: darkest ink → {lab:?}");
    }
}

#[test]
fn lightness_order_is_inverted() {
    for p in [DarkParams::PRINT, PHOTO] {
        // Grey ramp and a brown ink ramp (ink coverage mixed with white in linear light).
        let brown = hex(0x8C5A32).map(srgb_to_linear);
        let ramps: [Box<dyn Fn(f64) -> [f64; 3]>; 2] =
            [Box::new(|v| [v; 3]), Box::new(move |v| brown.map(|b| linear_to_srgb(b + (1.0 - b) * v)))];
        for ramp in &ramps {
            let ls: Vec<f64> = (0..=255).map(|i| out_lab(ramp(i as f64 / 255.0), &p)[0]).collect();
            for i in 1..ls.len() {
                assert!(
                    ls[i] <= ls[i - 1] + 1e-9,
                    "{p:?}: lighter input {i} renders lighter ({} > {})",
                    ls[i],
                    ls[i - 1]
                );
            }
            assert!(ls[0] - ls[255] > 0.4, "{p:?}: range {} … {}", ls[0], ls[255]);
            // Strictly: between the darkest ink and paper, every step darker renders lighter,
            // except where saturation alone lifts the ink (a photo's dark brown with its chroma
            // gained: L′ at the lift's ceiling, `f = 0.8`).
            let ceiling =
                night_paper_lab()[0] + (INK_LIGHTNESS - night_paper_lab()[0]) * SATURATION_WEIGHT.powf(LIGHTNESS_GAMMA);
            for i in 1..ls.len() {
                let l = srgb_to_oklab(ramp(i as f64 / 255.0))[0];
                if l > p.ink + 0.01 && 1.0 - l > p.dead + 0.01 && ls[i - 1] < ceiling - 1e-6 {
                    assert!(
                        ls[i] < ls[i - 1],
                        "{p:?}: flat at {i} (input L {l:.3}, L′ {} {}, ceiling {ceiling})",
                        ls[i - 1],
                        ls[i]
                    );
                }
            }
        }
    }
}

#[test]
fn feature_hues_are_kept() {
    let colours = [
        ("brown contour", 0xB4825A),
        ("dark brown index contour", 0x8C5A32),
        ("blue river", 0x2E7DD7),
        ("light blue water", 0x6FA8DC),
        ("red route", 0xE5323C),
        ("pink boundary", 0xF07882),
        ("yellow road", 0xF2D42A),
        ("green vegetation", 0x8CB43C),
    ];
    for p in [DarkParams::PRINT, PHOTO, map_fixture().profile.params] {
        for (name, c) in colours {
            let lab = srgb_to_oklab(hex(c));
            let out = out_lab(hex(c), &p);
            let d = hue_diff(lab, out);
            assert!(d < 10.0, "{name} #{c:06X} with {p:?}: hue moved {d:.1}° ({lab:?} → {out:?})");
            assert!(chroma(out) > 0.03, "{name} lost its colour: {out:?}");
        }
    }
}

#[test]
fn greys_stay_grey() {
    for p in [DarkParams::PRINT, PHOTO] {
        for v in 0..=255 {
            let out = out_lab([v as f64 / 255.0; 3], &p);
            assert!(chroma(out) < 0.01, "{p:?}: grey {v} → chroma {}", chroma(out));
        }
    }
    // A photo's near-greys (white balance off, JPEG chroma noise) are not turned into colours by
    // the photo's chroma gain.
    let near_greys = [
        [200u8, 197, 192],
        [128, 126, 122],
        [90, 88, 93],
        [60, 62, 58],
        [170, 168, 172],
        [225, 222, 216],
        [110, 108, 104],
        [40, 40, 44],
        // JPEG noise around black ink: dark, but with OKLab chroma up to 0.035.
        [0, 0, 4],
        [4, 1, 0],
        [7, 2, 2],
        [3, 3, 8],
        [2, 0, 5],
        [0, 6, 2],
    ];
    for c in near_greys {
        let x = c.map(|v| v as f64 / 255.0);
        let cin = chroma(srgb_to_oklab(x));
        assert!(cin < 0.013 || c.iter().all(|&v| v < 10), "{c:?} is not near-grey: C {cin}");
        let out = out_lab(x, &PHOTO);
        assert!(chroma(out) < 0.01, "near-grey {c:?} (C {cin:.4}) → chroma {:.4}", chroma(out));
    }
}

#[test]
fn pale_fills_stay_dark_and_yellow_roads_stay_readable() {
    let print_like = [DarkParams::PRINT, DarkParams { gain: 1.3, ..DarkParams::PRINT }, map_fixture().profile.params];
    for p in print_like {
        for (name, c) in [("light green", 0xE3F0D0), ("light blue", 0xDCEBF7), ("cream", 0xFAF3DC)] {
            let l = out_lab(hex(c), &p)[0];
            assert!(l < 0.40, "{name} #{c:06X} with {p:?} renders at L {l:.3}");
        }
    }
    for p in [DarkParams::PRINT, PHOTO, map_fixture().profile.params] {
        let l = out_lab(hex(0xF2D42A), &p)[0];
        assert!(l > 0.6, "yellow road with {p:?} renders at L {l:.3}");
    }
}

/// WCAG contrast ratio of two encoded sRGB colours.
fn contrast(a: [f64; 3], b: [f64; 3]) -> f64 {
    let y = |c: [f64; 3]| {
        let l = c.map(srgb_to_linear);
        0.2126 * l[0] + 0.7152 * l[1] + 0.0722 * l[2]
    };
    let (ya, yb) = (y(a), y(b));
    (ya.max(yb) + 0.05) / (ya.min(yb) + 0.05)
}

fn delta_e(a: [f64; 3], b: [f64; 3]) -> f64 {
    ((a[0] - b[0]).powi(2) + (a[1] - b[1]).powi(2) + (a[2] - b[2]).powi(2)).sqrt()
}

/// The print's inks, rendered with its own profile (exact path). Spot inks (yellow and orange
/// roads, the red route, the cyan gate symbol) are lifted to L′ ≈ 0.8 even when light; tints
/// (the vegetation hatch, the boundary's pink band) only to a mid tone, so they stay below
/// the roads and the route instead of glaring; contours stay calm tan.
#[test]
fn print_inks_keep_their_hierarchy() {
    let p = map_fixture().profile.params;
    let inks: [(&str, [u8; 3]); 13] = [
        ("black text", [0, 0, 0]),
        ("yellow road", [255, 255, 2]),
        ("orange road", [254, 156, 2]),
        ("route red", [235, 10, 4]),
        ("dark route red", [179, 25, 23]),
        ("boundary pink", [249, 129, 128]),
        ("vegetation hatch", [185, 222, 101]),
        ("gate cyan", [0, 229, 255]),
        ("river blue", [13, 89, 230]),
        ("dashed river", [82, 144, 214]),
        ("contour tan", [180, 150, 120]),
        ("pale green fill", [212, 228, 168]),
        ("grey casing", [150, 150, 150]),
    ];
    let night = NIGHT_PAPER_SRGB;
    let mut out = std::collections::HashMap::new();
    for (name, c) in inks {
        let x = c.map(|v| v as f64 / 255.0);
        let o = dark_pixel(x, &p);
        let lab = srgb_to_oklab(o);
        let o8 = o.map(|v| (v * 255.0).round() as u8);
        println!(
            "{name:>17} #{:02X}{:02X}{:02X} → #{:02X}{:02X}{:02X}  L′ {:.3}  C {:.3}  {:.2}:1 on the night paper",
            c[0],
            c[1],
            c[2],
            o8[0],
            o8[1],
            o8[2],
            lab[0],
            chroma(lab),
            contrast(o, night)
        );
        out.insert(name, lab);
    }
    let l = |n: &str| out[n][0];
    assert!((l("yellow road") - 0.80).abs() <= 0.01, "yellow road L′ {:.3}", l("yellow road"));
    assert!(l("orange road") >= 0.75, "orange road L′ {:.3}", l("orange road"));
    assert!(l("gate cyan") >= 0.75, "gate cyan L′ {:.3}", l("gate cyan"));
    assert!(l("route red") >= 0.75, "route red L′ {:.3}", l("route red"));
    assert!(l("dashed river") >= 0.62, "dashed river L′ {:.3}", l("dashed river"));
    // Tints stay below the spot inks.
    assert!(l("vegetation hatch") <= 0.65, "vegetation hatch L′ {:.3}", l("vegetation hatch"));
    assert!(l("vegetation hatch") >= 0.5, "vegetation hatch L′ {:.3}", l("vegetation hatch"));
    assert!(chroma(out["vegetation hatch"]) > 0.08, "vegetation lost its green");
    let de = delta_e(out["route red"], out["boundary pink"]);
    assert!(de >= 0.1, "route and boundary band ΔE {de:.3}");
    // Contours stay calm, pale fills stay background.
    assert!((0.45..=0.62).contains(&l("contour tan")), "contour L′ {:.3}", l("contour tan"));
    assert!(l("pale green fill") < 0.4);
    assert!(l("black text") >= 0.88);
}

// ---------------------------------------------------------------------------
// Lookup table, tiles, levels
// ---------------------------------------------------------------------------

/// How far the lookup table is from the exact formula over a set of flat-fielded pixels.
struct LutError {
    /// Per-channel error in sRGB /255: largest, 99th and 99.9th percentile, mean.
    max: f64,
    p99: f64,
    p999: f64,
    mean: f64,
    /// Largest OKLab distance between the two renderings, and where.
    max_de: f64,
    worst: [u8; 3],
}

fn lut_error(style: &DarkStyle, xs: &[[f64; 3]]) -> LutError {
    let mut errs = Vec::with_capacity(xs.len() * 3);
    let (mut max_de, mut worst) = (0.0f64, [0u8; 3]);
    for x in xs {
        let lut = style.lut_rgb(x.map(|v| v as f32)).map(|v| v as f64 / 255.0);
        let exact = dark_pixel(*x, style.params());
        errs.extend((0..3).map(|k| (lut[k] - exact[k]).abs() * 255.0));
        let de = {
            let (a, b) = (srgb_to_oklab(lut), srgb_to_oklab(exact));
            ((a[0] - b[0]).powi(2) + (a[1] - b[1]).powi(2) + (a[2] - b[2]).powi(2)).sqrt()
        };
        if de > max_de {
            (max_de, worst) = (de, x.map(|v| (v * 255.0).round() as u8));
        }
    }
    let mean = errs.iter().sum::<f64>() / errs.len() as f64;
    errs.sort_by(f64::total_cmp);
    let q = |p: f64| errs[((errs.len() - 1) as f64 * p) as usize];
    LutError { max: q(1.0), p99: q(0.99), p999: q(0.999), mean, max_de, worst }
}

/// `n` random pixels of a fixture, flat-fielded by its own paper grid (as the renderer does).
fn fixture_pixels(fx: &Fixture, n: usize, seed: u64) -> Vec<[f64; 3]> {
    let mut rng = Rng::new(seed);
    let (w, h) = (fx.image.w, fx.image.h);
    (0..n)
        .map(|_| {
            let (x, y) = ((rng.next_f64() * w as f64) as usize, (rng.next_f64() * h as f64) as usize);
            let p = fx.profile.paper.sample((x as f64 + 0.5) / w as f64, (y as f64 + 0.5) / h as f64);
            let c = fx.image.rgb(x, y);
            [0, 1, 2].map(|k| (c[k] as f32 / (255.0 * p[k])).min(1.0) as f64)
        })
        .collect()
}

/// The 65³ trilinear table against the exact formula. Map pixels (random pixels of the print and
/// of the real photo, flat-fielded) are within 2/255 at the 99th percentile and within an OKLab
/// distance of 0.02 (print) or 0.035 (photo: its chroma gain of 1.8 and the tint weight make the
/// formula steeper there; the flare removal runs as a per-channel curve before the table, so
/// its kink does not bend the table) everywhere. The largest per-channel errors (up to ~15/255) sit where the
/// gamut mapping pins a channel at 0 (e.g. the B of a bright orange), where sRGB encoding is
/// steepest: an OKLab distance of ~0.003 there. Over the whole cube (any colour, including the
/// most saturated ones the gamut mapping bends hardest) the errors are larger but rare.
#[test]
fn lookup_table_matches_the_formula() {
    let _g = serial();
    let report = |name: &str, e: &LutError| {
        println!(
            "LUT vs formula, {name}: per channel max {:.2}/255, p99.9 {:.2}, p99 {:.2}, mean {:.3}; OKLab ΔE max {:.4} (at sRGB {:?})",
            e.max, e.p999, e.p99, e.mean, e.max_de, e.worst
        );
    };
    for (name, fx, seed, max_de, tile_p999) in
        [("print pixels", map_fixture(), 0xDA2C, 0.02, 2), ("photo pixels", photo_fixture(), 0xDA2D, 0.035, 4)]
    {
        let style = DarkStyle::from_json(&fx.json).unwrap();
        let e = lut_error(&style, &fixture_pixels(fx, 100_000, seed));
        report(name, &e);
        assert!(e.p99 <= 2.0, "{name}: 99th percentile error {:.2}/255", e.p99);
        assert!(e.mean <= 0.25, "{name}: mean error {:.3}/255", e.mean);
        assert!(e.max_de <= max_de, "{name}: OKLab error {:.4} at {:?}", e.max_de, e.worst);
        // The 8-bit renders of a tile agree too.
        let (x, y, n) = (fx.image.w / 3, fx.image.h / 3, 512);
        let mut lut = rgba_from_rgb(&crop_rgb(&fx.image, x, y, n, n));
        let mut exact = lut.clone();
        let rect = TileRect::of_pixels(x, y, n, n, fx.image.w, fx.image.h);
        style.render(&mut lut, n, n, n * 4, rect).unwrap();
        style.render_exact(&mut exact, n, n, n * 4, rect).unwrap();
        let mut d: Vec<i32> = lut.iter().zip(&exact).map(|(a, b)| (*a as i32 - *b as i32).abs()).collect();
        d.sort_unstable();
        let (p999, max) = (d[(d.len() - 1) * 999 / 1000], d[d.len() - 1]);
        println!("  512×512 tile, LUT vs exact render: bytes differ by ≤ {p999} for 99.9 %, ≤ {max} for all");
        assert!(p999 <= tile_p999, "{name}: 8-bit render differs by {p999} at the 99.9th percentile");
    }
    for (name, p, seed) in [("whole cube, print", DarkParams::PRINT, 0xC0BE), ("whole cube, photo", PHOTO, 0xC0BF)] {
        let mut rng = Rng::new(seed);
        let xs: Vec<[f64; 3]> = (0..100_000).map(|_| [rng.next_f64(), rng.next_f64(), rng.next_f64()]).collect();
        let e = lut_error(&style_of(p), &xs);
        report(name, &e);
        assert!(e.mean <= 0.5, "{name}: mean error {:.3}/255", e.mean);
        assert!(e.p99 <= 8.0, "{name}: 99th percentile error {:.2}/255", e.p99);
        assert!(e.max_de <= 0.04, "{name}: OKLab error {:.4} at {:?}", e.max_de, e.worst);
    }
}

/// The `n × n` crop of `image` at `(cx, cy)` rendered as 2 × 2 and 3 × 3 tiles gives exactly
/// `whole`, its render as one tile.
fn assert_tilings_match(name: &str, style: &DarkStyle, image: &Rgb8, (cx, cy, n): (usize, usize, usize), whole: &Rgb8) {
    let (mw, mh) = (image.w, image.h);
    let crop = crop_rgb(image, cx, cy, n, n);
    for k in [2usize, 3] {
        let edges: Vec<usize> = (0..=k).map(|i| i * n / k).collect();
        let mut tiled = vec![0u8; n * n * 3];
        for j in 0..k {
            for i in 0..k {
                let (x0, y0, tw, th) = (edges[i], edges[j], edges[i + 1] - edges[i], edges[j + 1] - edges[j]);
                let tile = crop_rgb(&crop, x0, y0, tw, th);
                let out = render_rgb(style, &tile, TileRect::of_pixels(cx + x0, cy + y0, tw, th, mw, mh));
                for y in 0..th {
                    tiled[((y0 + y) * n + x0) * 3..((y0 + y) * n + x0 + tw) * 3]
                        .copy_from_slice(&out.data[y * tw * 3..(y + 1) * tw * 3]);
                }
            }
        }
        let diff = tiled.iter().zip(&whole.data).filter(|(a, b)| a != b).count();
        println!("{name}: {k}×{k} tiles vs one tile: {diff} bytes differ");
        assert_eq!(diff, 0, "{name}: {k}×{k} tiles differ from the single tile");
    }
}

#[test]
fn tiles_are_seamless_across_sizes_and_levels() {
    let _g = serial();
    let fx = map_fixture();
    let style = DarkStyle::from_json(&fx.json).unwrap();
    let (mw, mh) = (fx.image.w, fx.image.h);
    let (cx, cy, n) = (1200, 1700, 1024);
    let crop = crop_rgb(&fx.image, cx, cy, n, n);
    let whole = render_rgb(&style, &crop, TileRect::of_pixels(cx, cy, n, n, mw, mh));
    assert_tilings_match("print", &style, &fx.image, (cx, cy, n), &whole);
    // The real photo's top-right corner, across its edge bands (the table's slivers).
    let photo = photo_fixture();
    let pstyle = DarkStyle::from_json(&photo.json).unwrap();
    assert!(pstyle.edge().is_some());
    let (px, pn) = (photo.image.w - 600, 600);
    let pcrop = crop_rgb(&photo.image, px, 0, pn, pn);
    let pwhole = render_rgb(&pstyle, &pcrop, TileRect::of_pixels(px, 0, pn, pn, photo.image.w, photo.image.h));
    let night = pwhole.data.chunks_exact(3).filter(|p| p == &[33, 33, 36]).count();
    assert!(night * 3 > pn * pn, "the photo corner is mostly night paper");
    assert_tilings_match("photo corner", &pstyle, &photo.image, (px, 0, pn), &pwhole);
    // Pyramid level 1: the half-size crop rendered over the same rectangle ≈ the half-size render.
    let half = render_rgb(&style, &box_downsample(&crop, 2), TileRect::of_pixels(cx, cy, n, n, mw, mh));
    let reference = box_downsample(&whole, 2);
    let mad = half.data.iter().zip(&reference.data).map(|(a, b)| (*a as f64 - *b as f64).abs()).sum::<f64>()
        / half.data.len() as f64;
    let mean = |d: &[u8]| d.iter().map(|&v| v as f64).sum::<f64>() / d.len() as f64;
    let (m1, m0) = (mean(&half.data), mean(&reference.data));
    println!("level 1 vs half-size of level 0: mean abs diff {mad:.2}, mean level {m1:.2} vs {m0:.2}");
    assert!(mad < 6.0, "level 1 render differs from the half-size render by {mad:.2} on average");
    assert!((m1 - m0).abs() < 3.0, "level 1 is brighter or darker: {m1:.2} vs {m0:.2}");
}

// ---------------------------------------------------------------------------
// Profiles and paper evenness
// ---------------------------------------------------------------------------

#[test]
fn profile_of_the_printed_map() {
    let _g = serial();
    let fx = map_fixture();
    let p = fx.profile.params;
    print_profile("map.jpg overview", &fx.profile);
    assert!(p.ink < 0.25, "ink {}", p.ink);
    assert!((1.0..=1.3).contains(&p.gain), "gain {}", p.gain);
    assert!(p.dead <= 0.03, "dead {}", p.dead);
    // A print has no flare and no edge bands (its full-bleed content is never cropped).
    assert_eq!(p.flare(), 0.0);
    assert!(fx.profile.edge.is_none() && !fx.json.contains("edge"));
    // The paper of a PDF render is white everywhere.
    let lowest = fx.profile.paper.rgb.iter().copied().fold(1.0f32, f32::min);
    assert!(lowest > 0.9, "paper grid down to {lowest}");
    // The C ABI gives the same profile.
    let ov = box_downsample(&fx.image, 3);
    let rgba = rgba_from_rgb(&ov);
    let v = take_json(unsafe { mm_dark_profile(rgba.as_ptr(), ov.w as u32, ov.h as u32, ov.w as u32 * 4) });
    assert_eq!(v, serde_json::from_str::<Value>(&fx.json).unwrap());
    // Rendered, the map's paper is the night paper.
    let style = DarkStyle::from_json(&fx.json).unwrap();
    let out = render_rgb(&style, &ov, TileRect::FULL);
    let night = out.data.chunks_exact(3).filter(|p| p == &[33, 33, 36]).count();
    println!(
        "map overview: {:.1} % of the pixels are exactly the night paper",
        100.0 * night as f64 / (ov.w * ov.h) as f64
    );
    assert!(night * 2 > ov.w * ov.h);
}

#[test]
fn profile_of_the_real_photo_and_its_even_paper() {
    let _g = serial();
    let fx = photo_fixture();
    let p = fx.profile.params;
    print_profile("real photo (flattened)", &fx.profile);
    assert!((0.3..=0.5).contains(&p.ink), "ink {}", p.ink);
    assert!((0.03..=0.09).contains(&p.dead), "dead {}", p.dead);
    assert!(p.gain > 1.3, "gain {}", p.gain);
    let style = DarkStyle::from_json(&fx.json).unwrap();
    let out = render_rgb(&style, &fx.image, TileRect::FULL);
    let mask = photo_paper_mask(&fx.image);
    let (si, so) = evenness("real photo paper", &fx.image, &out, &mask);
    assert!(so < 0.02, "rendered paper uneven: std L {so:.4} (input {si:.4})");
    assert!(so < si / 2.0);
    let (w, h) = (out.w, out.h);
    let labs: Vec<[f64; 3]> = out.data.chunks_exact(3).map(lab_u8).collect();
    // The wooden table showing at the page's left and right edges (1.2 % of the image, bright
    // orange) is not drawn: bright orange pixels within 3 % of those edges.
    let band = w * 3 / 100;
    let orange = labs
        .iter()
        .enumerate()
        .filter(|&(i, lab)| {
            let (x, hue) = (i % w, lab[2].atan2(lab[1]).to_degrees());
            (x < band || x >= w - band) && lab[0] > 0.6 && chroma(*lab) > 0.08 && (40.0..80.0).contains(&hue)
        })
        .count();
    let orange_pct = 100.0 * orange as f64 / (w * h) as f64;
    // The flare removed, the photo's ink is as calm as the print's: inside the page (5 % in from
    // its edges) the mean lightness and the share of bright pixels.
    let inner: Vec<f64> = labs
        .iter()
        .enumerate()
        .filter(|&(i, _)| {
            let (x, y) = (i % w, i / w);
            x >= w / 20 && x < w - w / 20 && y >= h / 20 && y < h - h / 20
        })
        .map(|(_, lab)| lab[0])
        .collect();
    let mean = inner.iter().sum::<f64>() / inner.len() as f64;
    let bright = 100.0 * inner.iter().filter(|&&l| l > 0.55).count() as f64 / inner.len() as f64;
    println!(
        "real photo render: {orange_pct:.3} % bright orange at the left and right edges; inside the page mean L {mean:.3}, {bright:.1} % above L 0.55"
    );
    assert!(orange_pct < 0.05, "table slivers drawn: {orange_pct:.3} %");
    assert!(mean <= 0.335, "photo renders hazy: mean L {mean:.3}");
    assert!(bright <= 7.0, "photo renders busy: {bright:.1} % above L 0.55");
}

#[test]
fn synthetic_photo_paper_is_even() {
    let _g = serial();
    let s = synth_fixture();
    print_profile("synthetic photo (flattened)", &s.profile);
    let style = DarkStyle::from_json(&s.profile.to_json()).unwrap();
    let out = render_rgb(&style, &s.image, TileRect::FULL);
    let mask = synth_paper_mask(s, &map_fixture().image);
    let (si, so) = evenness("synthetic photo paper", &s.image, &out, &mask);
    assert!(so < 0.02, "rendered paper uneven: std L {so:.4} (input {si:.4})");
    assert!(so < si / 2.0);
}

// ---------------------------------------------------------------------------
// Digital maps: flat paper, no flare, no edge bands
// ---------------------------------------------------------------------------

/// The print profiled from the overview the app uses (pyramid level 2 of the 3507 × 4960 sheet,
/// 876 × 1240), from the tests' 1169 × 1653 one, and from larger and smaller ones: the darkest
/// 0.7 % of an overview's pixels depend on its scale (L 0.12 at 1/3, 0.21 at 1/4), which on a
/// print must not turn into a flare removal (that is for a photo's veiling light). A print gets
/// the same white paper, no flare and no edge bands at any scale.
#[test]
fn print_profile_does_not_depend_on_the_overview_scale() {
    let _g = serial();
    let map = &map_fixture().image;
    for f in [2, 3, 4, 6] {
        let p = DarkProfile::from_rgb8(&box_downsample(map, f)).unwrap();
        print_profile(&format!("map.jpg at 1/{f}"), &p);
        assert_eq!(p.params.flare(), 0.0, "1/{f}: a print with a flare: {:?}", p.params);
        assert!(p.params.ink <= FLARE_FREE_INK, "1/{f}: {:?}", p.params);
        assert!(p.params.dead <= 0.03 && p.params.gain == 1.0, "1/{f}: {:?}", p.params);
        assert!(p.edge.is_none(), "1/{f}: edge bands on a print");
        assert!(p.paper.rgb.iter().all(|&v| v == 1.0), "1/{f}: paper not white everywhere");
    }
}

/// A print with a large solid fill (a sea or a lake over the left 40 % of the sheet, the map's
/// ink on it): the fill is not mistaken for paper, so it renders as one even tone. Taken for
/// paper beyond a cell or two from its shore, it faded into the night paper away from the shore,
/// with a glowing band along it (L′ 0.25 … 0.44 over the sea's bare pixels).
#[test]
fn large_fills_on_a_print_render_evenly() {
    let _g = serial();
    let ov = box_downsample(&map_fixture().image, 4);
    let (w, h) = (ov.w, ov.h);
    for fill in [0xAAD3DF, 0x6FA8DC] {
        let f = hex(fill);
        let mut img = ov.clone();
        // The sea's bare pixels: white paper before the fill was printed over it.
        let mut bare = vec![];
        for y in 0..h {
            for x in 0..w * 2 / 5 {
                let i = (y * w + x) * 3;
                if img.data[i..i + 3] == [255, 255, 255] {
                    bare.push(i);
                }
                for (k, fk) in f.iter().enumerate() {
                    img.data[i + k] = (img.data[i + k] as f64 * fk).round() as u8;
                }
            }
        }
        let profile = DarkProfile::from_rgb8(&img).unwrap();
        let style = DarkStyle::from_json(&profile.to_json()).unwrap();
        let out = render_rgb(&style, &img, TileRect::FULL);
        let mut ls: Vec<f64> = bare.iter().map(|&i| lab_u8(&out.data[i..i + 3])[0]).collect();
        ls.sort_by(f64::total_cmp);
        let (lo, hi) = (ls[ls.len() / 100], ls[ls.len() * 99 / 100]);
        println!(
            "fill #{fill:06X}: {:?}, paper grid {}×{}; its {} bare pixels render at L′ {lo:.3} … {hi:.3} (1st … 99th percentile)",
            profile.params,
            profile.paper.cols,
            profile.paper.rows,
            ls.len()
        );
        assert!(hi - lo <= 0.01, "fill #{fill:06X} renders unevenly: L′ {lo:.3} … {hi:.3}");
        assert!(lo >= night_paper_lab()[0] + 0.08, "fill #{fill:06X} renders as the night paper");
        assert_eq!(profile.params.gain, 1.0);
    }
}

/// A sparse print (thin black lines and an orange track on white, 0.5 % ink): the darkest 0.7 %
/// of its pixels are paper, which made it look like a photo with a heavy flare (`ink` 0.65,
/// φ 0.27) whose removal turned the orange track salmon. A print has no flare: its colours keep
/// their hue.
#[test]
fn a_sparse_print_keeps_its_colours() {
    let (w, h) = (1200usize, 1700usize);
    let orange = [230u8, 120, 20];
    let mut img = Rgb8::from_rgb(w, h, vec![255; w * h * 3]).unwrap();
    for y in 0..h {
        for x in 0..w {
            let c = if x % 400 == 7 || y % 500 == 9 {
                [0, 0, 0]
            } else if (x + y) % 600 < 2 {
                orange
            } else {
                continue;
            };
            img.data[(y * w + x) * 3..][..3].copy_from_slice(&c);
        }
    }
    let profile = DarkProfile::from_rgb8(&img).unwrap();
    print_profile("sparse print", &profile);
    assert_eq!(profile.params.flare(), 0.0, "{:?}", profile.params);
    let out = render_rgb(&DarkStyle::new(&profile), &img, TileRect::FULL);
    let at = |x: usize, y: usize| lab_u8(&out.data[(y * w + x) * 3..][..3]);
    let (track, line) = (at(600, 0), at(7, 100));
    let d = hue_diff(lab_u8(&orange), track);
    println!(
        "orange track → L′ {:.3} C {:.3}, hue moved {d:.1}°; black line → L′ {:.3}",
        track[0],
        chroma(track),
        line[0]
    );
    assert!(d < 10.0, "orange track hue moved {d:.1}°");
    assert!(line[0] >= 0.88);
}

// ---------------------------------------------------------------------------
// C ABI, determinism, threads, speed
// ---------------------------------------------------------------------------

#[test]
fn c_abi_round_trip() {
    let _g = serial();
    let fx = map_fixture();
    let ov = box_downsample(&fx.image, 8);
    // Rows with padding, like a CGContext's.
    let stride = ov.w * 4 + 16;
    let mut buf = vec![7u8; stride * ov.h];
    for y in 0..ov.h {
        for x in 0..ov.w {
            buf[y * stride + x * 4..y * stride + x * 4 + 3].copy_from_slice(&ov.data[(y * ov.w + x) * 3..][..3]);
            buf[y * stride + x * 4 + 3] = 200;
        }
    }
    let v = take_json(unsafe { mm_dark_profile(buf.as_ptr(), ov.w as u32, ov.h as u32, stride as u32) });
    assert_eq!(v["status"], "ok", "{v}");
    assert_eq!(v["version"], 1);
    let (cols, rows) = (v["paper"]["cols"].as_u64().unwrap(), v["paper"]["rows"].as_u64().unwrap());
    assert_eq!(v["paper"]["rgb"].as_array().unwrap().len() as u64, cols * rows * 3);
    for k in ["ink", "dead", "gain"] {
        assert!(v[k].as_f64().unwrap().is_finite());
    }
    let json = CString::new(v.to_string()).unwrap();
    let style = unsafe { mm_dark_style_new(json.as_ptr()) };
    assert!(!style.is_null());
    let mut tile = buf.clone();
    let rc = unsafe {
        mm_dark_render(style, tile.as_mut_ptr(), ov.w as u32, ov.h as u32, stride as u32, 0.0, 0.0, 1.0, 1.0)
    };
    assert_eq!(rc, MM_OK);
    // Same pixels as the Rust API; alpha and row padding untouched.
    let rust = DarkStyle::from_json(&v.to_string()).unwrap();
    let mut expect = buf.clone();
    rust.render(&mut expect, ov.w, ov.h, stride, TileRect::FULL).unwrap();
    assert_eq!(tile, expect);
    for y in 0..ov.h {
        assert!(tile[y * stride + ov.w * 4..(y + 1) * stride].iter().all(|&b| b == 7));
        assert!((0..ov.w).all(|x| tile[y * stride + x * 4 + 3] == 200));
    }
    assert_ne!(tile, buf);
    unsafe { mm_dark_style_free(style) };
}

#[test]
fn c_abi_rejects_bad_input() {
    let px = vec![255u8; 64 * 64 * 4];
    let failed = |v: Value| {
        assert_eq!(v["status"], "failed", "{v}");
        assert!(v["message"].as_str().is_some_and(|m| !m.is_empty()));
    };
    unsafe {
        failed(take_json(mm_dark_profile(std::ptr::null(), 64, 64, 256)));
        failed(take_json(mm_dark_profile(px.as_ptr(), 0, 64, 256)));
        failed(take_json(mm_dark_profile(px.as_ptr(), 64, 0, 256)));
        failed(take_json(mm_dark_profile(px.as_ptr(), 64, 64, 255)));
        failed(take_json(mm_dark_profile(px.as_ptr(), 4, 4, 16)));
        failed(take_json(mm_dark_profile(px.as_ptr(), 40_000, 1, 160_000)));

        assert!(mm_dark_style_new(std::ptr::null()).is_null());
        for bad in [
            "",
            "{",
            "null",
            "[]",
            r#"{"status":"failed","message":"x"}"#,
            r#"{"status":"ok","version":1,"ink":0.2,"dead":0.02,"gain":1.0}"#,
            r#"{"status":"ok","version":2,"ink":0.2,"dead":0.02,"gain":1.0,"paper":{"cols":1,"rows":1,"rgb":[1,1,1]}}"#,
            r#"{"status":"ok","version":1,"ink":0.2,"dead":0.02,"gain":1.0,"paper":{"cols":2,"rows":1,"rgb":[1,1,1]}}"#,
            r#"{"status":"ok","version":1,"ink":0.2,"dead":0.02,"gain":1.0,"paper":{"cols":1,"rows":1,"rgb":[1,1,"x"]}}"#,
            r#"{"status":"ok","version":1,"ink":0.97,"dead":0.02,"gain":1.0,"paper":{"cols":1,"rows":1,"rgb":[1,1,1]}}"#,
        ] {
            let c = CString::new(bad).unwrap();
            assert!(mm_dark_style_new(c.as_ptr()).is_null(), "accepted {bad}");
        }
        let invalid_utf8 = [0xFFu8, 0xFE, 0];
        assert!(mm_dark_style_new(invalid_utf8.as_ptr() as *const c_char).is_null());
        mm_dark_style_free(std::ptr::null_mut());

        let good = CString::new(
            r#"{"status":"ok","version":1,"ink":0.2,"dead":0.02,"gain":1.0,"paper":{"cols":1,"rows":1,"rgb":[1,1,1]}}"#,
        )
        .unwrap();
        let style = mm_dark_style_new(good.as_ptr());
        assert!(!style.is_null());
        let mut tile = vec![255u8; 32 * 32 * 4];
        let orig = tile.clone();
        let p = tile.as_mut_ptr();
        assert_eq!(mm_dark_render(std::ptr::null(), p, 32, 32, 128, 0.0, 0.0, 1.0, 1.0), MM_ERR_ARGUMENT);
        assert_eq!(mm_dark_render(style, std::ptr::null_mut(), 32, 32, 128, 0.0, 0.0, 1.0, 1.0), MM_ERR_ARGUMENT);
        assert_eq!(mm_dark_render(style, p, 0, 32, 128, 0.0, 0.0, 1.0, 1.0), MM_ERR_ARGUMENT);
        assert_eq!(mm_dark_render(style, p, 32, 0, 128, 0.0, 0.0, 1.0, 1.0), MM_ERR_ARGUMENT);
        assert_eq!(mm_dark_render(style, p, 32, 32, 127, 0.0, 0.0, 1.0, 1.0), MM_ERR_ARGUMENT);
        assert_eq!(mm_dark_render(style, p, 40_000, 1, 160_000, 0.0, 0.0, 1.0, 1.0), MM_ERR_ARGUMENT);
        for (u0, v0, du, dv) in [
            (f64::NAN, 0.0, 1.0, 1.0),
            (0.0, f64::INFINITY, 1.0, 1.0),
            (0.0, 0.0, 0.0, 1.0),
            (0.0, 0.0, 1.0, -0.5),
            (0.0, 0.0, f64::NAN, 1.0),
        ] {
            assert_eq!(mm_dark_render(style, p, 32, 32, 128, u0, v0, du, dv), MM_ERR_ARGUMENT);
        }
        assert_eq!(tile, orig, "a rejected call wrote pixels");
        // A tile reaching beyond the map (edge tiles) is fine: the paper grid is clamped.
        assert_eq!(mm_dark_render(style, p, 32, 32, 128, 0.9, -0.1, 0.25, 0.25), MM_OK);
        assert!(tile.chunks_exact(4).all(|q| q == [33, 33, 36, 255]));
        mm_dark_style_free(style);
    }
}

/// Degenerate overviews (blank, black, one colour, tiny, very thin, noise, a checkerboard, a
/// ramp) profile without panics through the C ABI: too small ones fail, the others give a
/// profile that builds a style and renders, the lookup table close to the formula.
#[test]
fn degenerate_overviews_profile_without_panics() {
    let solid = |w: usize, h: usize, c: [u8; 3]| Rgb8::from_rgb(w, h, c.repeat(w * h)).unwrap();
    let mut rng = Rng::new(5);
    let noise = Rgb8::from_rgb(300, 300, (0..300 * 300 * 3).map(|_| (rng.next_f64() * 256.0) as u8).collect()).unwrap();
    let checker = Rgb8::from_rgb(
        256,
        256,
        (0..256 * 256).flat_map(|i| [if ((i % 256) / 4 + (i / 256) / 4) % 2 == 0 { 0 } else { 255 }; 3]).collect(),
    )
    .unwrap();
    let ramp = Rgb8::from_rgb(512, 512, (0..512 * 512).flat_map(|i| [((i % 512) / 2) as u8; 3]).collect()).unwrap();
    let cases = [
        ("white", solid(64, 64, [255; 3]), true),
        ("black", solid(64, 64, [0; 3]), true),
        ("mid grey", solid(64, 64, [128; 3]), true),
        ("red", solid(64, 64, [255, 0, 0]), true),
        ("1×1", solid(1, 1, [200; 3]), false),
        ("7×100", solid(7, 100, [200; 3]), false),
        ("8×8", solid(8, 8, [10; 3]), true),
        ("8×2048", solid(8, 2048, [240; 3]), true),
        ("2048×8", solid(2048, 8, [240; 3]), true),
        ("4100×17", solid(4100, 17, [240; 3]), false),
        ("noise", noise, true),
        ("checkerboard", checker, true),
        ("ramp", ramp, true),
    ];
    for (name, img, ok) in cases {
        let rgba = rgba_from_rgb(&img);
        let v = take_json(unsafe { mm_dark_profile(rgba.as_ptr(), img.w as u32, img.h as u32, img.w as u32 * 4) });
        assert_eq!(v["status"] == "ok", ok, "{name}: {v}");
        if !ok {
            continue;
        }
        let json = v.to_string();
        let style = DarkStyle::from_json(&json).unwrap_or_else(|e| panic!("{name}: {e}: {json}"));
        let tile = crop_rgb(&img, 0, 0, img.w.min(64), img.h.min(64));
        let rect = TileRect::of_pixels(0, 0, tile.w, tile.h, img.w, img.h);
        let lut = render_rgb(&style, &tile, rect);
        let mut exact = rgba_from_rgb(&tile);
        style.render_exact(&mut exact, tile.w, tile.h, tile.w * 4, rect).unwrap();
        let exact = rgb_from_rgba(&exact, tile.w, tile.h);
        let worst = lut.data.iter().zip(&exact.data).map(|(a, b)| a.abs_diff(*b)).max().unwrap();
        println!("{name}: {:?}, edge bands {}, LUT vs exact ≤ {worst}", style.params(), style.edge().is_some());
        assert!(worst <= 24, "{name}: LUT differs from the formula by {worst}");
    }
}

/// Random valid profiles (any paper, edge bands, tone parameters at the limits the JSON
/// accepts) render random tiles (padded strides, rectangles far outside the map or at the
/// limits of f64) without panics, the lookup table close to the formula.
#[test]
fn random_profiles_and_tiles_render_without_panics() {
    let mut rng = Rng::new(77);
    for it in 0..60 {
        let ink = 0.02 + rng.next_f64() * 0.8;
        let dead = rng.next_f64() * (0.9 - ink).min(0.5);
        let gain = 0.25 + rng.next_f64() * 3.75;
        let (cols, rows) = (1 + (rng.next_f64() * 40.0) as usize, 1 + (rng.next_f64() * 40.0) as usize);
        // Paper down to 0 (a value the profile never writes, but the JSON accepts).
        let floor = if it % 7 == 0 { 0.0 } else { 0.2 };
        let rgb: Vec<f32> = (0..cols * rows * 3).map(|_| (rng.next_f64() as f32).max(floor)).collect();
        let edge = (it % 2 == 0).then(|| EdgeInset {
            left: (0..1 + it % 5).map(|_| (rng.next_f64() * 0.05) as f32).collect(),
            right: vec![0.0; 3],
            top: (0..1 + it % 3).map(|_| (rng.next_f64() * 0.25) as f32).collect(),
            bottom: vec![0.25],
        });
        let profile =
            DarkProfile { params: DarkParams { ink, dead, gain }, paper: PaperGrid { cols, rows, rgb }, edge };
        let json = profile.to_json();
        let style = DarkStyle::from_json(&json).unwrap_or_else(|e| panic!("{e}: {json}"));
        let mut worst = 0;
        for _ in 0..4 {
            let (w, h) = (1 + (rng.next_f64() * 70.0) as usize, 1 + (rng.next_f64() * 70.0) as usize);
            let stride = w * 4 + (rng.next_f64() * 9.0) as usize;
            let mut exact: Vec<u8> = (0..stride * h).map(|_| (rng.next_f64() * 256.0) as u8).collect();
            let rect = match (rng.next_f64() * 6.0) as usize {
                0 => TileRect { u0: -1e300, v0: 1e300, du: 1e300, dv: 1e-300 },
                1 => TileRect { u0: 0.99, v0: 0.99, du: 0.5, dv: 0.5 },
                2 => TileRect { u0: -0.3, v0: -0.3, du: 1.6, dv: 1.6 },
                3 => TileRect { u0: f64::MAX, v0: -f64::MAX, du: f64::MAX, dv: f64::MAX },
                _ => TileRect {
                    u0: rng.next_f64(),
                    v0: rng.next_f64(),
                    du: rng.next_f64() + 1e-9,
                    dv: rng.next_f64() + 1e-9,
                },
            };
            let mut lut = exact.clone();
            style.render(&mut lut, w, h, stride, rect).unwrap();
            style.render_exact(&mut exact, w, h, stride, rect).unwrap();
            worst = worst.max(lut.iter().zip(&exact).map(|(a, b)| a.abs_diff(*b)).max().unwrap());
        }
        println!("profile {it}: {:?}, {cols}×{rows} cells: LUT vs exact ≤ {worst}", style.params());
        assert!(worst <= 96, "profile {it}: LUT differs from the formula by {worst}");
    }
}

/// Freeing a style twice, or freeing a pointer that never was a style, is ignored rather than
/// undefined behaviour (a double free corrupted the heap).
#[test]
fn c_abi_style_double_free_is_ignored() {
    let good = CString::new(
        r#"{"status":"ok","version":1,"ink":0.2,"dead":0.02,"gain":1.0,"paper":{"cols":1,"rows":1,"rgb":[1,1,1]}}"#,
    )
    .unwrap();
    unsafe {
        let a = mm_dark_style_new(good.as_ptr());
        assert!(!a.is_null());
        mm_dark_style_free(a);
        mm_dark_style_free(a);
        let mut not_a_style = [0u64; 8];
        mm_dark_style_free(not_a_style.as_mut_ptr() as *mut MMDarkStyle);
        // Styles made afterwards work as usual.
        let b = mm_dark_style_new(good.as_ptr());
        let mut tile = vec![255u8; 8 * 8 * 4];
        assert_eq!(mm_dark_render(b, tile.as_mut_ptr(), 8, 8, 32, 0.0, 0.0, 1.0, 1.0), MM_OK);
        assert!(tile.chunks_exact(4).all(|q| q == [33, 33, 36, 255]));
        mm_dark_style_free(b);
        mm_dark_style_free(b);
    }
}

#[test]
fn rendering_is_deterministic_and_thread_safe() {
    let _g = serial();
    let fx = map_fixture();
    // The same overview gives the same profile, byte for byte.
    let again = DarkProfile::from_rgb8(&box_downsample(&fx.image, 3)).unwrap();
    assert_eq!(again.to_json(), fx.json);
    let json = CString::new(fx.json.clone()).unwrap();
    let style = unsafe { mm_dark_style_new(json.as_ptr()) };
    assert!(!style.is_null());
    let (mw, mh) = (fx.image.w, fx.image.h);
    let n = 256;
    let tiles: Vec<(usize, usize)> = (0..16).map(|i| (300 + (i % 4) * 700, 400 + (i / 4) * 1000)).collect();
    let render_one = |style: *const MMDarkStyle, (x, y): (usize, usize)| -> Vec<u8> {
        let mut rgba = rgba_from_rgb(&crop_rgb(&fx.image, x, y, n, n));
        let r = TileRect::of_pixels(x, y, n, n, mw, mh);
        let rc = unsafe {
            mm_dark_render(style, rgba.as_mut_ptr(), n as u32, n as u32, n as u32 * 4, r.u0, r.v0, r.du, r.dv)
        };
        assert_eq!(rc, MM_OK);
        rgba
    };
    let serial_out: Vec<Vec<u8>> = tiles.iter().map(|&t| render_one(style, t)).collect();
    // Four threads share the style, as CATiledLayer's do.
    let addr = style as usize;
    let parallel_out: Vec<Vec<u8>> = std::thread::scope(|s| {
        let handles: Vec<_> = (0..4)
            .map(|k| {
                let tiles = &tiles;
                let render_one = &render_one;
                s.spawn(move || {
                    let style = addr as *const MMDarkStyle;
                    (0..tiles.len())
                        .filter(|i| i % 4 == k)
                        .map(|i| (i, render_one(style, tiles[i])))
                        .collect::<Vec<_>>()
                })
            })
            .collect();
        let mut all: Vec<(usize, Vec<u8>)> = handles.into_iter().flat_map(|h| h.join().unwrap()).collect();
        all.sort_by_key(|p| p.0);
        all.into_iter().map(|p| p.1).collect()
    });
    assert_eq!(serial_out, parallel_out);
    // A second style from the same JSON renders the same bytes.
    let style2 = unsafe { mm_dark_style_new(json.as_ptr()) };
    assert_eq!(render_one(style2, tiles[5]), serial_out[5]);
    unsafe {
        mm_dark_style_free(style);
        mm_dark_style_free(style2);
    }
}

#[test]
fn speed() {
    let _g = serial();
    let fx = map_fixture();
    let mut builds: Vec<f64> = (0..3)
        .map(|_| {
            let t = Instant::now();
            let s = DarkStyle::from_json(&fx.json).unwrap();
            let ms = t.elapsed().as_secs_f64() * 1e3;
            drop(s);
            ms
        })
        .collect();
    builds.sort_by(f64::total_cmp);
    let style = DarkStyle::from_json(&fx.json).unwrap();
    let (x, y, n) = (1500, 2000, 512);
    let src = rgba_from_rgb(&crop_rgb(&fx.image, x, y, n, n));
    let rect = TileRect::of_pixels(x, y, n, n, fx.image.w, fx.image.h);
    let mut times: Vec<f64> = (0..15)
        .map(|_| {
            let mut tile = src.clone();
            let t = Instant::now();
            style.render(&mut tile, n, n, n * 4, rect).unwrap();
            t.elapsed().as_secs_f64() * 1e3
        })
        .collect();
    times.sort_by(f64::total_cmp);
    let t0 = Instant::now();
    let _ = DarkProfile::from_rgb8(&box_downsample(&fx.image, 3)).unwrap();
    let profile_ms = t0.elapsed().as_secs_f64() * 1e3;
    // A photo tile (flare curve, edge bands) at the page's top-right corner.
    let photo = photo_fixture();
    let pstyle = DarkStyle::from_json(&photo.json).unwrap();
    let (px, py) = (photo.image.w - n, 0);
    let psrc = rgba_from_rgb(&crop_rgb(&photo.image, px, py, n, n));
    let prect = TileRect::of_pixels(px, py, n, n, photo.image.w, photo.image.h);
    let mut ptimes: Vec<f64> = (0..15)
        .map(|_| {
            let mut tile = psrc.clone();
            let t = Instant::now();
            pstyle.render(&mut tile, n, n, n * 4, prect).unwrap();
            t.elapsed().as_secs_f64() * 1e3
        })
        .collect();
    ptimes.sort_by(f64::total_cmp);
    let t0 = Instant::now();
    let _ = DarkProfile::from_rgb8(&photo.image).unwrap();
    let photo_profile_ms = t0.elapsed().as_secs_f64() * 1e3;
    println!(
        "style (65³ LUT) build {:.1} ms (best of 3); 512×512 tile render {:.2} ms median, {:.2} ms best (photo {:.2} ms median); profile of a 1169×1653 overview {profile_ms:.0} ms (photo, 850×1116: {photo_profile_ms:.0} ms)",
        builds[0], times[7], times[0], ptimes[7]
    );
    assert!(builds[0] < time_budget_ms(150.0), "style creation took {:.1} ms", builds[0]);
    assert!(times[7] < time_budget_ms(30.0), "512×512 render took {:.2} ms", times[7]);
    assert!(ptimes[7] < time_budget_ms(30.0), "512×512 photo render took {:.2} ms", ptimes[7]);
}

// ---------------------------------------------------------------------------
// Visual check
// ---------------------------------------------------------------------------

/// Before/after images of the map, the real photo and a synthetic photo, written to
/// `$DARK_DUMP` (default: the session scratchpad). Run with
/// `cargo test --release --test dark dark_dump -- --ignored --nocapture`.
#[test]
#[ignore]
fn dark_dump() {
    let _g = serial();
    let dir = std::env::var("DARK_DUMP").map(std::path::PathBuf::from).unwrap_or_else(|_| {
        std::path::PathBuf::from(
            "/tmp/claude-0/-home-user-mapme/14512810-7919-5485-a6cb-a2459e12f0d6/scratchpad/dark-dump",
        )
    });
    std::fs::create_dir_all(&dir).unwrap();
    let save = |name: &str, before: &Rgb8, after: &Rgb8| {
        let path = dir.join(format!("{name}.png"));
        save_png(&side_by_side(before, after, 12), &path);
        println!("wrote {}", path.display());
    };

    let map = map_fixture();
    print_profile("map", &map.profile);
    let style = DarkStyle::from_json(&map.json).unwrap();
    let (mw, mh) = (map.image.w, map.image.h);
    let ov = box_downsample(&map.image, 3);
    save("map_overview", &ov, &render_rgb(&style, &ov, TileRect::FULL));
    for (name, x, y, w, h) in [
        ("map_gully_1to1", 1200, 1700, 1200, 1000),
        ("map_legend_1to1", 100, 3850, 1500, 1050),
        ("map_vegetation_1to1", 900, 700, 1000, 800),
    ] {
        let c = crop_rgb(&map.image, x, y, w, h);
        save(name, &c, &render_rgb(&style, &c, TileRect::of_pixels(x, y, w, h, mw, mh)));
    }

    let photo = photo_fixture();
    print_profile("real photo", &photo.profile);
    std::fs::write(dir.join("photo_profile.json"), &photo.json).unwrap();
    let style = DarkStyle::from_json(&photo.json).unwrap();
    let (pw, ph) = (photo.image.w, photo.image.h);
    save("photo_overview", &photo.image, &render_rgb(&style, &photo.image, TileRect::FULL));
    for (name, x, y, w, h) in [
        ("photo_crop_1to1", 500, 850, 700, 600),
        ("photo_corner_1to1", pw - 400, 0, 400, 400),
        ("photo_fold_1to1", 0, 1550, 700, 400),
    ] {
        let c = crop_rgb(&photo.image, x, y, w, h);
        save(name, &c, &render_rgb(&style, &c, TileRect::of_pixels(x, y, w, h, pw, ph)));
    }

    let synth = synth_fixture();
    print_profile("synthetic photo", &synth.profile);
    let style = DarkStyle::from_json(&synth.profile.to_json()).unwrap();
    let half = box_downsample(&synth.image, 2);
    save("synth_photo_level1", &half, &render_rgb(&style, &half, TileRect::FULL));
    let (sw, sh) = (synth.image.w, synth.image.h);
    let (x, y, w, h) = (sw / 3, sh / 3, 900, 800);
    let c = crop_rgb(&synth.image, x, y, w, h);
    save("synth_photo_crop_1to1", &c, &render_rgb(&style, &c, TileRect::of_pixels(x, y, w, h, sw, sh)));
}
