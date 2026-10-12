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
    DarkStyle::new(&DarkProfile { params, paper: PaperGrid::uniform([1.0; 3]) })
}

fn take_json(p: *mut c_char) -> Value {
    assert!(!p.is_null());
    let s = unsafe { CStr::from_ptr(p) }.to_str().expect("UTF-8").to_owned();
    unsafe { mm_string_free(p) };
    serde_json::from_str(&s).unwrap_or_else(|e| panic!("not JSON ({e}): {s}"))
}

fn print_profile(name: &str, p: &DarkProfile) {
    println!(
        "{name}: ink {:.4}, dead {:.4}, gain {:.4}, paper grid {}×{}",
        p.params.ink, p.params.dead, p.params.gain, p.paper.cols, p.paper.rows
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
            // Strictly: between the darkest ink and paper, every step darker renders lighter.
            for i in 1..ls.len() {
                let l = srgb_to_oklab(ramp(i as f64 / 255.0))[0];
                if l > p.ink + 0.01 && 1.0 - l > p.dead + 0.01 {
                    assert!(ls[i] < ls[i - 1], "{p:?}: flat at {i} (input L {l:.3})");
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
/// distance of 0.02 everywhere. The largest per-channel errors (up to ~15/255) sit where the
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
    for (name, fx, seed) in [("print pixels", map_fixture(), 0xDA2C), ("photo pixels", photo_fixture(), 0xDA2D)] {
        let style = DarkStyle::from_json(&fx.json).unwrap();
        let e = lut_error(&style, &fixture_pixels(fx, 100_000, seed));
        report(name, &e);
        assert!(e.p99 <= 2.0, "{name}: 99th percentile error {:.2}/255", e.p99);
        assert!(e.mean <= 0.25, "{name}: mean error {:.3}/255", e.mean);
        assert!(e.max_de <= 0.02, "{name}: OKLab error {:.4} at {:?}", e.max_de, e.worst);
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
        assert!(p999 <= 2, "{name}: 8-bit render differs by {p999} at the 99.9th percentile");
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

#[test]
fn tiles_are_seamless_across_sizes_and_levels() {
    let _g = serial();
    let fx = map_fixture();
    let style = DarkStyle::from_json(&fx.json).unwrap();
    let (mw, mh) = (fx.image.w, fx.image.h);
    let (cx, cy, n) = (1200, 1700, 1024);
    let crop = crop_rgb(&fx.image, cx, cy, n, n);
    let whole = render_rgb(&style, &crop, TileRect::of_pixels(cx, cy, n, n, mw, mh));
    for k in [2usize, 3] {
        let edges: Vec<usize> = (0..=k).map(|i| i * n / k).collect();
        let mut tiled = vec![0u8; n * n * 3];
        for j in 0..k {
            for i in 0..k {
                let (x0, y0, tw, th) = (edges[i], edges[j], edges[i + 1] - edges[i], edges[j + 1] - edges[j]);
                let tile = crop_rgb(&crop, x0, y0, tw, th);
                let out = render_rgb(&style, &tile, TileRect::of_pixels(cx + x0, cy + y0, tw, th, mw, mh));
                for y in 0..th {
                    tiled[((y0 + y) * n + x0) * 3..((y0 + y) * n + x0 + tw) * 3]
                        .copy_from_slice(&out.data[y * tw * 3..(y + 1) * tw * 3]);
                }
            }
        }
        let diff = tiled.iter().zip(&whole.data).filter(|(a, b)| a != b).count();
        println!("{k}×{k} tiles vs one tile: {diff} bytes differ");
        assert_eq!(diff, 0, "{k}×{k} tiles differ from the single tile");
    }
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
    println!(
        "style (65³ LUT) build {:.1} ms (best of 3); 512×512 tile render {:.2} ms median, {:.2} ms best; profile of a 1169×1653 overview {profile_ms:.0} ms",
        builds[0], times[7], times[0]
    );
    assert!(builds[0] < 150.0, "style creation took {:.1} ms", builds[0]);
    assert!(times[7] < 30.0, "512×512 render took {:.2} ms", times[7]);
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
    for (name, x, y, w, h) in [("map_gully_1to1", 1200, 1700, 1200, 1000), ("map_legend_1to1", 100, 3850, 1500, 1050)] {
        let c = crop_rgb(&map.image, x, y, w, h);
        save(name, &c, &render_rgb(&style, &c, TileRect::of_pixels(x, y, w, h, mw, mh)));
    }

    let photo = photo_fixture();
    print_profile("real photo", &photo.profile);
    let style = DarkStyle::from_json(&photo.json).unwrap();
    let (pw, ph) = (photo.image.w, photo.image.h);
    save("photo_overview", &photo.image, &render_rgb(&style, &photo.image, TileRect::FULL));
    let (x, y, w, h) = (500, 850, 700, 600);
    let c = crop_rgb(&photo.image, x, y, w, h);
    save("photo_crop_1to1", &c, &render_rgb(&style, &c, TileRect::of_pixels(x, y, w, h, pw, ph)));

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
