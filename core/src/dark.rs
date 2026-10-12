//! Dark map rendering: a paper map shown dark, the way the app's UI is.
//!
//! A plain colour inversion turns a topographic map muddy (yellow roads and
//! pale green fills go dark brown) and inverts a photo's shading and paper
//! tint into blotches. Instead (see `docs/ARCHITECTURE.md`, "Dark map"):
//!
//! 1. **Profile** ([`DarkProfile::from_rgb8`] / [`DarkProfile::from_rgba`]),
//!    once per map from an overview of the whole map: the *paper grid* (the
//!    paper white per block: 90th percentile per channel, 3 × 3 maximum, 3 × 3
//!    box blur, floored at 0.2), and on the flat-fielded overview (`rgb /
//!    paper`) in OKLab the darkest ink level `ink`, the paper noise `dead` and
//!    the chroma `gain` that brings a photo's washed-out colours back.
//! 2. **Render** ([`DarkStyle::render`]), per tile: flat-field each pixel by
//!    the paper grid at its normalised map position, then map it through the
//!    style's 65³ lookup table (trilinear) of [`dark_pixel`]: lightness
//!    inverted in OKLab (paper → the app's night paper, black ink → light
//!    grey), each feature's hue kept, saturated colour lifted even when light,
//!    pale fills left as background, out-of-gamut colours brought in by
//!    reducing chroma at constant lightness and hue.
//!
//! A [`DarkStyle`] is immutable, `Send + Sync`, and renders on the calling
//! thread only: the app renders tiles on CATiledLayer's threads concurrently.

use crate::image::{Rgb8, RgbaView};
use rayon::prelude::*;
use serde_json::{json, Value};
use std::borrow::Cow;
use std::sync::LazyLock;

/// The app's night paper (`Theme.uiNightPaper`), sRGB 0..1: paper renders as this (#212124).
pub const NIGHT_PAPER_SRGB: [f64; 3] = [0.13, 0.13, 0.14];
/// OKLab lightness of the darkest ink after rendering (black → light grey).
pub const INK_LIGHTNESS: f64 = 0.90;
/// Exponent of the lightness curve `L′ = Lp + (0.90 − Lp) · f^0.78`.
pub const LIGHTNESS_GAMMA: f64 = 0.78;
/// Chroma below which a pixel counts as grey:
/// `gate = smoothstep(GREY_GATE.0, GREY_GATE.1, C · clamp(L / GREY_GATE_DARK_L, 0, 1))`.
pub const GREY_GATE: (f64, f64) = (0.012, 0.03);
/// Below this OKLab lightness the gate sees chroma scaled by `L / 0.25`, so noise around black
/// ink (e.g. sRGB (0, 0, 4), C 0.034) stays grey instead of turning into light blue.
pub const GREY_GATE_DARK_L: f64 = 0.25;
/// Gained chroma where saturated colour starts to count as a feature, and the ramp width.
pub const SATURATION_START: f64 = 0.07;
pub const SATURATION_RAMP: f64 = 0.08;
/// How strongly saturation alone lifts a pixel (`f = max(t, 0.8 · s)`).
pub const SATURATION_WEIGHT: f64 = 0.8;
/// Chroma boost of ink on the dark background.
pub const CHROMA_BOOST: f64 = 1.1;
/// Typical chroma of a printed map's coloured ink (median over clearly inked pixels: 0.135 on the
/// Grootkloof print, 0.08 on the phone photo of it): `gain = clamp(INK_CHROMA / median C, 1, 1.8)`.
pub const INK_CHROMA: f64 = 0.12;
/// Largest chroma gain for washed-out colours.
pub const MAX_GAIN: f64 = 1.8;
/// Lookup table nodes per axis (over flat-fielded sRGB 0..1).
pub const LUT_SIZE: usize = 65;
/// Overviews larger than this (long side) are box-downsampled before profiling.
pub const MAX_OVERVIEW_SIDE: usize = 2048;
/// Version of the profile JSON.
pub const PROFILE_VERSION: u64 = 1;

/// Paper white is never taken darker than this (sRGB), so deep shadow is not blown up.
const PAPER_FLOOR: f32 = 0.2;
/// Paper grid coordinates are quantised to 1/4096 of a cell, so tiles of any size that
/// cover the same map position compute the same paper value.
const GRID_QUANTUM: f64 = 4096.0;

// ---------------------------------------------------------------------------
// OKLab
// ---------------------------------------------------------------------------

/// sRGB transfer function (encoded 0..1 → linear 0..1).
#[inline]
pub fn srgb_to_linear(c: f64) -> f64 {
    if c <= 0.04045 {
        c / 12.92
    } else {
        ((c + 0.055) / 1.055).powf(2.4)
    }
}

/// Inverse sRGB transfer function (linear 0..1 → encoded 0..1).
#[inline]
pub fn linear_to_srgb(c: f64) -> f64 {
    if c <= 0.003_130_8 {
        12.92 * c
    } else {
        1.055 * c.powf(1.0 / 2.4) - 0.055
    }
}

/// Linear sRGB → OKLab (Björn Ottosson's matrices).
#[inline]
pub fn linear_to_oklab([r, g, b]: [f64; 3]) -> [f64; 3] {
    let l = (0.412_221_470_8 * r + 0.536_332_536_3 * g + 0.051_445_992_9 * b).cbrt();
    let m = (0.211_903_498_2 * r + 0.680_699_545_1 * g + 0.107_396_956_6 * b).cbrt();
    let s = (0.088_302_461_9 * r + 0.281_718_837_6 * g + 0.629_978_700_5 * b).cbrt();
    [
        0.210_454_255_3 * l + 0.793_617_785_0 * m - 0.004_072_046_8 * s,
        1.977_998_495_1 * l - 2.428_592_205_0 * m + 0.450_593_709_9 * s,
        0.025_904_037_1 * l + 0.782_771_766_2 * m - 0.808_675_766_0 * s,
    ]
}

/// OKLab → linear sRGB (may be out of 0..1).
#[inline]
pub fn oklab_to_linear([l, a, b]: [f64; 3]) -> [f64; 3] {
    let l_ = l + 0.396_337_777_4 * a + 0.215_803_757_3 * b;
    let m_ = l - 0.105_561_345_8 * a - 0.063_854_172_8 * b;
    let s_ = l - 0.089_484_177_5 * a - 1.291_485_548_0 * b;
    let (l3, m3, s3) = (l_ * l_ * l_, m_ * m_ * m_, s_ * s_ * s_);
    [
        4.076_741_662_1 * l3 - 3.307_711_591_3 * m3 + 0.230_969_929_2 * s3,
        -1.268_438_004_6 * l3 + 2.609_757_401_1 * m3 - 0.341_319_396_5 * s3,
        -0.004_196_086_3 * l3 - 0.703_418_614_7 * m3 + 1.707_614_701_0 * s3,
    ]
}

/// Encoded sRGB 0..1 → OKLab.
#[inline]
pub fn srgb_to_oklab(rgb: [f64; 3]) -> [f64; 3] {
    linear_to_oklab(rgb.map(|c| srgb_to_linear(c.clamp(0.0, 1.0))))
}

fn in_gamut(rgb: [f64; 3]) -> bool {
    const EPS: f64 = 1e-9;
    rgb.iter().all(|&c| (-EPS..=1.0 + EPS).contains(&c))
}

/// OKLab → encoded sRGB 0..1. An out-of-gamut colour loses chroma (same lightness and hue)
/// until it fits, rather than being clipped per channel.
pub fn oklab_to_srgb(lab: [f64; 3]) -> [f64; 3] {
    let rgb = oklab_to_linear(lab);
    let lin = if in_gamut(rgb) {
        rgb
    } else {
        let [l, a, b] = lab;
        let l = l.clamp(0.0, 1.0);
        let (mut lo, mut hi) = (0.0, 1.0);
        for _ in 0..22 {
            let k = 0.5 * (lo + hi);
            if in_gamut(oklab_to_linear([l, k * a, k * b])) {
                lo = k;
            } else {
                hi = k;
            }
        }
        oklab_to_linear([l, lo * a, lo * b])
    };
    lin.map(|c| linear_to_srgb(c.clamp(0.0, 1.0)))
}

/// OKLab of the night paper.
pub fn night_paper_lab() -> [f64; 3] {
    static LAB: LazyLock<[f64; 3]> = LazyLock::new(|| srgb_to_oklab(NIGHT_PAPER_SRGB));
    *LAB
}

#[inline]
fn smoothstep(e0: f64, e1: f64, x: f64) -> f64 {
    let t = ((x - e0) / (e1 - e0)).clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

// ---------------------------------------------------------------------------
// The per-pixel formula
// ---------------------------------------------------------------------------

/// The map's tone parameters (from its profile).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct DarkParams {
    /// OKLab lightness of the map's darkest ink after flat-fielding (~0.15 print, ~0.4 photo).
    pub ink: f64,
    /// Darkening below paper white (in OKLab L) that still counts as paper (grain, JPEG noise).
    pub dead: f64,
    /// Chroma gain for washed-out colours (1 for print, up to 1.8 for a photo).
    pub gain: f64,
}

impl DarkParams {
    /// Typical parameters of a printed (PDF) map.
    pub const PRINT: DarkParams = DarkParams { ink: 0.15, dead: 0.02, gain: 1.0 };
}

/// The dark rendering of one flat-fielded pixel in OKLab, before gamut mapping:
/// `(L, a, b)` → `(L′, a′, b′)` (see `docs/ARCHITECTURE.md`, "Dark map").
pub fn dark_lab([l, a, b]: [f64; 3], p: &DarkParams) -> [f64; 3] {
    let paper = night_paper_lab();
    let c = a.hypot(b);
    // How much ink, by lightness.
    let t = (((1.0 - l) - p.dead) / ((1.0 - p.ink) - p.dead)).clamp(0.0, 1.0);
    // Grey stays grey: no amplified colour casts. Near black, where OKLab chroma is unstable
    // (JPEG noise around black text has C ≈ 0.03), a colour must be proportionally stronger.
    let gate = smoothstep(GREY_GATE.0, GREY_GATE.1, c * (l / GREY_GATE_DARK_L).clamp(0.0, 1.0));
    // Saturated colour is a feature even when light; pale fills stay background.
    let s = ((c * p.gain * gate - SATURATION_START) / SATURATION_RAMP).clamp(0.0, 1.0);
    let f = t.max(SATURATION_WEIGHT * s);
    let l2 = paper[0] + (INK_LIGHTNESS - paper[0]) * f.powf(LIGHTNESS_GAMMA);
    // The night paper's slight tint on paper, the feature's hue on ink.
    let w = (4.0 * f).clamp(0.0, 1.0);
    let k = w * CHROMA_BOOST * p.gain * gate;
    [l2, k * a + (1.0 - w) * paper[1], k * b + (1.0 - w) * paper[2]]
}

/// The exact dark rendering of a flat-fielded linear-sRGB pixel, as encoded sRGB 0..1.
pub fn dark_linear(lin: [f64; 3], p: &DarkParams) -> [f64; 3] {
    oklab_to_srgb(dark_lab(linear_to_oklab(lin), p))
}

/// The exact dark rendering of a flat-fielded pixel (encoded sRGB 0..1, paper white = 1),
/// as encoded sRGB 0..1. The style's lookup table samples this.
pub fn dark_pixel(x: [f64; 3], p: &DarkParams) -> [f64; 3] {
    dark_linear(x.map(|c| srgb_to_linear(c.clamp(0.0, 1.0))), p)
}

// ---------------------------------------------------------------------------
// Paper grid
// ---------------------------------------------------------------------------

/// The paper white over the map: `cols × rows` cells of sRGB triples (0..1, row-major),
/// sampled bilinearly in normalised map coordinates with cell centres at
/// `((i + 0.5) / cols, (j + 0.5) / rows)`, clamped at the edges.
#[derive(Clone, Debug, PartialEq)]
pub struct PaperGrid {
    pub cols: usize,
    pub rows: usize,
    pub rgb: Vec<f32>,
}

/// Grid position along one axis: the two cells and the weight of the second.
#[inline]
fn grid_axis(u: f64, n: usize) -> (usize, usize, f32) {
    let g = (u * n as f64 - 0.5).clamp(0.0, (n - 1) as f64);
    let g = (g * GRID_QUANTUM).round() / GRID_QUANTUM;
    let i0 = (g as usize).min(n - 1);
    (i0, (i0 + 1).min(n - 1), (g - i0 as f64) as f32)
}

#[inline]
fn lerp3(a: [f32; 3], b: [f32; 3], t: f32) -> [f32; 3] {
    [a[0] + (b[0] - a[0]) * t, a[1] + (b[1] - a[1]) * t, a[2] + (b[2] - a[2]) * t]
}

impl PaperGrid {
    /// The same paper everywhere.
    pub fn uniform(rgb: [f32; 3]) -> PaperGrid {
        PaperGrid { cols: 1, rows: 1, rgb: rgb.to_vec() }
    }

    #[inline]
    pub fn cell(&self, i: usize, j: usize) -> [f32; 3] {
        let k = (j * self.cols + i) * 3;
        [self.rgb[k], self.rgb[k + 1], self.rgb[k + 2]]
    }

    /// Paper white at normalised map position (u, v).
    pub fn sample(&self, u: f64, v: f64) -> [f32; 3] {
        let (i0, i1, fx) = grid_axis(u, self.cols);
        let (j0, j1, fy) = grid_axis(v, self.rows);
        let a = lerp3(self.cell(i0, j0), self.cell(i0, j1), fy);
        let b = lerp3(self.cell(i1, j0), self.cell(i1, j1), fy);
        lerp3(a, b, fx)
    }

    /// One row of the grid interpolated at `v` (cols triples), for sampling along a tile row.
    fn row_at(&self, v: f64, out: &mut [[f32; 3]]) {
        let (j0, j1, fy) = grid_axis(v, self.rows);
        for (i, o) in out.iter_mut().enumerate().take(self.cols) {
            *o = lerp3(self.cell(i, j0), self.cell(i, j1), fy);
        }
    }

    fn validate(&self) -> Result<(), String> {
        if self.cols == 0 || self.rows == 0 || self.cols > 4096 || self.rows > 4096 {
            return Err(format!("paper grid of {}×{} cells", self.cols, self.rows));
        }
        if self.rgb.len() != self.cols * self.rows * 3 {
            return Err(format!("paper grid has {} values for {}×{} cells", self.rgb.len(), self.cols, self.rows));
        }
        if self.rgb.iter().any(|v| !v.is_finite() || !(0.0..=1.0).contains(v)) {
            return Err("paper grid values must be in 0..1".into());
        }
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Profile
// ---------------------------------------------------------------------------

/// What the dark rendering needs to know about one map (stored with the map as JSON).
#[derive(Clone, Debug, PartialEq)]
pub struct DarkProfile {
    pub params: DarkParams,
    pub paper: PaperGrid,
}

fn round4(v: f64) -> f64 {
    (v * 1e4).round() / 1e4
}

/// Quantile with linear interpolation between order statistics (NumPy's default).
/// Reorders `v`. NaN for an empty slice.
pub fn quantile(v: &mut [f32], p: f64) -> f64 {
    if v.is_empty() {
        return f64::NAN;
    }
    let pos = p.clamp(0.0, 1.0) * (v.len() - 1) as f64;
    let lo = pos.floor() as usize;
    let (_, &mut a, right) = v.select_nth_unstable_by(lo, f32::total_cmp);
    let b = right.iter().copied().min_by(f32::total_cmp).unwrap_or(a);
    a as f64 + (b as f64 - a as f64) * (pos - lo as f64)
}

/// Value at quantile `p` of a 256-bin histogram of `n` samples (linear interpolation).
fn hist_quantile(hist: &[u32; 256], n: usize, p: f64) -> f64 {
    let pos = p * (n - 1) as f64;
    let lo = pos.floor() as usize;
    let nth = |k: usize| -> usize {
        let mut acc = 0usize;
        for (v, &c) in hist.iter().enumerate() {
            acc += c as usize;
            if acc > k {
                return v;
            }
        }
        255
    };
    let a = nth(lo) as f64;
    let b = nth((lo + 1).min(n - 1)) as f64;
    a + (b - a) * (pos - lo as f64)
}

/// Box-downsample by `factor` (partial blocks at the right and bottom edges average what they cover).
fn downsample<F: Fn(usize, usize) -> [u8; 3] + Sync>(w: usize, h: usize, factor: usize, get: F) -> Rgb8 {
    let (nw, nh) = (w.div_ceil(factor), h.div_ceil(factor));
    let mut data = vec![0u8; nw * nh * 3];
    data.par_chunks_mut(nw * 3).enumerate().for_each(|(oy, row)| {
        let (y0, y1) = (oy * factor, ((oy + 1) * factor).min(h));
        for (ox, px) in row.chunks_exact_mut(3).enumerate() {
            let (x0, x1) = (ox * factor, ((ox + 1) * factor).min(w));
            let mut sum = [0u32; 3];
            for y in y0..y1 {
                for x in x0..x1 {
                    let c = get(x, y);
                    for k in 0..3 {
                        sum[k] += c[k] as u32;
                    }
                }
            }
            let n = ((y1 - y0) * (x1 - x0)) as u32;
            for k in 0..3 {
                px[k] = ((sum[k] + n / 2) / n) as u8;
            }
        }
    });
    Rgb8 { w: nw, h: nh, data }
}

/// 3 × 3 filter over a `cols × rows` grid of triples, edges replicated.
fn filter3(g: &[[f32; 3]], cols: usize, rows: usize, max: bool) -> Vec<[f32; 3]> {
    let mut out = vec![[0.0f32; 3]; g.len()];
    for j in 0..rows {
        for i in 0..cols {
            let mut acc = if max { [f32::MIN; 3] } else { [0.0; 3] };
            for dj in [-1isize, 0, 1] {
                for di in [-1isize, 0, 1] {
                    let jj = (j as isize + dj).clamp(0, rows as isize - 1) as usize;
                    let ii = (i as isize + di).clamp(0, cols as isize - 1) as usize;
                    let c = g[jj * cols + ii];
                    for k in 0..3 {
                        acc[k] = if max { acc[k].max(c[k]) } else { acc[k] + c[k] };
                    }
                }
            }
            out[j * cols + i] = if max { acc } else { acc.map(|v| v / 9.0) };
        }
    }
    out
}

/// The paper grid of an overview (see the module docs).
fn paper_grid(ov: &Rgb8) -> PaperGrid {
    let (w, h) = (ov.w, ov.h);
    let block = (w.max(h) / 32).max(8);
    let cols = ((w as f64 / block as f64).round() as usize).max(1);
    let rows = ((h as f64 / block as f64).round() as usize).max(1);
    let cells: Vec<[f32; 3]> = (0..cols * rows)
        .into_par_iter()
        .map(|c| {
            let (i, j) = (c % cols, c / cols);
            let (x0, x1) = (i * w / cols, ((i + 1) * w / cols).max(i * w / cols + 1).min(w));
            let (y0, y1) = (j * h / rows, ((j + 1) * h / rows).max(j * h / rows + 1).min(h));
            let mut hist = [[0u32; 256]; 3];
            for y in y0..y1 {
                let row = &ov.data[(y * w + x0) * 3..(y * w + x1) * 3];
                for px in row.chunks_exact(3) {
                    for k in 0..3 {
                        hist[k][px[k] as usize] += 1;
                    }
                }
            }
            let n = (x1 - x0) * (y1 - y0);
            [0, 1, 2].map(|k| (hist_quantile(&hist[k], n, 0.9) / 255.0) as f32)
        })
        .collect();
    let g = filter3(&filter3(&cells, cols, rows, true), cols, rows, false);
    let rgb = g.iter().flat_map(|c| c.map(|v| round4(v.max(PAPER_FLOOR) as f64) as f32)).collect();
    PaperGrid { cols, rows, rgb }
}

impl DarkProfile {
    /// Profile of a map from an overview of the whole map (8-bit RGBA, alpha ignored).
    pub fn from_rgba(view: &RgbaView) -> Result<DarkProfile, String> {
        let factor = view.width.max(view.height).div_ceil(MAX_OVERVIEW_SIDE);
        let ov = downsample(view.width, view.height, factor, |x, y| view.rgb(x, y));
        Self::from_overview(&ov)
    }

    /// Profile of a map from an overview of the whole map (packed RGB).
    pub fn from_rgb8(img: &Rgb8) -> Result<DarkProfile, String> {
        let factor = img.w.max(img.h).div_ceil(MAX_OVERVIEW_SIDE);
        let ov = if factor <= 1 {
            Cow::Borrowed(img)
        } else {
            Cow::Owned(downsample(img.w, img.h, factor, |x, y| img.rgb(x, y)))
        };
        Self::from_overview(&ov)
    }

    fn from_overview(ov: &Rgb8) -> Result<DarkProfile, String> {
        let (w, h) = (ov.w, ov.h);
        if w < 8 || h < 8 || ov.data.len() != w * h * 3 {
            return Err(format!("Overview too small for a dark profile: {w}×{h} pixels"));
        }
        let paper = paper_grid(ov);
        // OKLab L and C of the flat-fielded overview.
        let lc: Vec<(f32, f32)> = (0..h)
            .into_par_iter()
            .flat_map_iter(|y| {
                let v = (y as f64 + 0.5) / h as f64;
                let paper = &paper;
                (0..w).map(move |x| {
                    let p = paper.sample((x as f64 + 0.5) / w as f64, v);
                    let c = ov.rgb(x, y);
                    let flat = [0, 1, 2].map(|k| (c[k] as f32 / 255.0 / p[k]).min(1.0) as f64);
                    let [l, a, b] = srgb_to_oklab(flat);
                    (l as f32, a.hypot(b) as f32)
                })
            })
            .collect();
        let mut ls: Vec<f32> = lc.iter().map(|p| p.0).collect();
        let ink = quantile(&mut ls, 0.007).clamp(0.12, 0.65);
        // Paper noise: the brighter 45 % of the pixels.
        let l55 = quantile(&mut ls, 0.55) as f32;
        let mut bright: Vec<f32> = ls.iter().copied().filter(|&l| l > l55).collect();
        if bright.is_empty() {
            bright = ls.iter().copied().filter(|&l| l >= l55).collect();
        }
        let med = quantile(&mut bright, 0.5);
        let mut dev: Vec<f32> = bright.iter().map(|&l| (l as f64 - med).abs() as f32).collect();
        let mad = quantile(&mut dev, 0.5);
        let dead = (1.0 - med + 3.0 * 1.4826 * mad).clamp(0.015, 0.12);
        // Chroma of clearly coloured ink.
        let mut chroma: Vec<f32> = lc
            .iter()
            .filter(|&&(l, c)| {
                let t = ((1.0 - l as f64) - dead) / ((1.0 - ink) - dead);
                t >= 0.5 && c >= 0.025
            })
            .map(|p| p.1)
            .collect();
        let gain =
            if chroma.len() < 200 { 1.0 } else { (INK_CHROMA / quantile(&mut chroma, 0.5)).clamp(1.0, MAX_GAIN) };
        Ok(DarkProfile { params: DarkParams { ink: round4(ink), dead: round4(dead), gain: round4(gain) }, paper })
    }

    /// `{"status":"ok","version":1,"ink","dead","gain","paper":{"cols","rows","rgb":[...]}}`.
    pub fn to_json(&self) -> String {
        json!({
            "status": "ok",
            "version": PROFILE_VERSION,
            "ink": self.params.ink,
            "dead": self.params.dead,
            "gain": self.params.gain,
            "paper": {"cols": self.paper.cols, "rows": self.paper.rows, "rgb": self.paper.rgb},
        })
        .to_string()
    }

    /// Parse and validate a profile written by [`DarkProfile::to_json`].
    pub fn from_json(s: &str) -> Result<DarkProfile, String> {
        let v: Value = serde_json::from_str(s).map_err(|e| format!("profile is not JSON: {e}"))?;
        if let Some(st) = v.get("status") {
            if st != "ok" {
                return Err(format!("profile status is {st}"));
            }
        }
        if v.get("version").and_then(Value::as_u64) != Some(PROFILE_VERSION) {
            return Err(format!("unsupported profile version {}", v.get("version").unwrap_or(&Value::Null)));
        }
        let num = |k: &str| -> Result<f64, String> {
            v.get(k).and_then(Value::as_f64).filter(|x| x.is_finite()).ok_or_else(|| format!("profile `{k}` missing"))
        };
        let params = DarkParams { ink: num("ink")?, dead: num("dead")?, gain: num("gain")? };
        if !(0.0..1.0).contains(&params.ink)
            || !(0.0..1.0).contains(&params.dead)
            || (1.0 - params.ink) - params.dead < 0.05
            || !(0.25..=4.0).contains(&params.gain)
        {
            return Err(format!("profile parameters out of range: {params:?}"));
        }
        let pv = v.get("paper").ok_or("profile `paper` missing")?;
        let dim = |k: &str| -> Result<usize, String> {
            pv.get(k).and_then(Value::as_u64).map(|n| n as usize).ok_or_else(|| format!("paper `{k}` missing"))
        };
        let rgb = pv
            .get("rgb")
            .and_then(Value::as_array)
            .ok_or("paper `rgb` missing")?
            .iter()
            .map(|x| x.as_f64().map(|x| x as f32).ok_or("paper `rgb` must hold numbers"))
            .collect::<Result<Vec<f32>, _>>()?;
        let paper = PaperGrid { cols: dim("cols")?, rows: dim("rows")?, rgb };
        paper.validate()?;
        Ok(DarkProfile { params, paper })
    }
}

// ---------------------------------------------------------------------------
// Style and rendering
// ---------------------------------------------------------------------------

/// The part of the map a tile covers, in normalised map coordinates (fractions of the full
/// map's width and height): pixel (x, y) of a `w × h` tile has its centre at
/// `(u0 + (x + 0.5) / w · du, v0 + (y + 0.5) / h · dv)`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct TileRect {
    pub u0: f64,
    pub v0: f64,
    pub du: f64,
    pub dv: f64,
}

impl TileRect {
    /// The whole map.
    pub const FULL: TileRect = TileRect { u0: 0.0, v0: 0.0, du: 1.0, dv: 1.0 };

    /// Pixels `[x, x + w) × [y, y + h)` of a `map_w × map_h` map.
    pub fn of_pixels(x: usize, y: usize, w: usize, h: usize, map_w: usize, map_h: usize) -> TileRect {
        let (mw, mh) = (map_w as f64, map_h as f64);
        TileRect { u0: x as f64 / mw, v0: y as f64 / mh, du: w as f64 / mw, dv: h as f64 / mh }
    }

    fn validate(&self) -> Result<(), String> {
        let finite = [self.u0, self.v0, self.du, self.dv].iter().all(|v| v.is_finite());
        if !finite || self.du <= 0.0 || self.dv <= 0.0 {
            return Err(format!("invalid tile rectangle {self:?}"));
        }
        Ok(())
    }
}

/// An immutable dark style for one map: its tone parameters, its paper grid and the
/// precomputed 65³ lookup table of [`dark_pixel`] over flat-fielded sRGB.
pub struct DarkStyle {
    params: DarkParams,
    paper: PaperGrid,
    /// Output sRGB × 255 at node `(r, g, b)` (index `(r · 65 + g) · 65 + b`, node k at k / 64).
    lut: Box<[Node]>,
    /// Every node with all coordinates ≥ this renders as the night paper, so a flat-fielded
    /// pixel with every channel ≥ it is paper without interpolating (the same bytes).
    paper_from: f32,
    /// The rendered night paper, 8-bit.
    paper_rgb: [u8; 3],
}

/// One lookup table entry: output sRGB × 255 (fourth lane unused), 16-byte aligned so an
/// entry is one load and the interpolation runs on all lanes at once.
#[derive(Clone, Copy, Default)]
#[repr(C, align(16))]
struct Node([f32; 4]);

#[inline(always)]
fn lerp4(a: [f32; 4], b: [f32; 4], t: f32) -> [f32; 4] {
    [a[0] + (b[0] - a[0]) * t, a[1] + (b[1] - a[1]) * t, a[2] + (b[2] - a[2]) * t, a[3] + (b[3] - a[3]) * t]
}

// Tiles render on many threads with one style.
const _: fn() = || {
    fn send_sync<T: Send + Sync>() {}
    send_sync::<DarkStyle>();
};

impl DarkStyle {
    /// Build the style (and its lookup table) for a profile.
    pub fn new(profile: &DarkProfile) -> DarkStyle {
        let p = profile.params;
        let n = LUT_SIZE;
        let lin: Vec<f64> = (0..n).map(|k| srgb_to_linear(k as f64 / (n - 1) as f64)).collect();
        let mut lut = vec![Node::default(); n * n * n].into_boxed_slice();
        lut.par_chunks_mut(n * n).enumerate().for_each(|(r, plane)| {
            for (gi, row) in plane.chunks_exact_mut(n).enumerate() {
                for (bi, o) in row.iter_mut().enumerate() {
                    let c = dark_linear([lin[r], lin[gi], lin[bi]], &p);
                    *o = Node([(c[0] * 255.0) as f32, (c[1] * 255.0) as f32, (c[2] * 255.0) as f32, 0.0]);
                }
            }
        });
        // The corner of the cube that is all paper (paper grain and noise, up to `dead`).
        let white = lut[n * n * n - 1].0;
        let index = |r: usize, g: usize, b: usize| (r * n + g) * n + b;
        let mut from = n - 1;
        while from > 0 {
            let k = from - 1;
            let shell_is_paper =
                (k..n).all(|r| (k..n).all(|g| (k..n).all(|b| r.min(g).min(b) != k || lut[index(r, g, b)].0 == white)));
            if !shell_is_paper {
                break;
            }
            from = k;
        }
        let paper_rgb = [0, 1, 2].map(|k| (white[k] + 0.5) as u8);
        DarkStyle { params: p, paper: profile.paper.clone(), lut, paper_from: from as f32 / (n - 1) as f32, paper_rgb }
    }

    /// Build the style from profile JSON.
    pub fn from_json(s: &str) -> Result<DarkStyle, String> {
        DarkProfile::from_json(s).map(|p| DarkStyle::new(&p))
    }

    pub fn params(&self) -> &DarkParams {
        &self.params
    }

    pub fn paper(&self) -> &PaperGrid {
        &self.paper
    }

    /// The lookup table's rendering of a flat-fielded pixel (sRGB 0..1), as sRGB × 255.
    #[inline]
    pub fn lut_rgb(&self, x: [f32; 3]) -> [f32; 3] {
        const N: usize = LUT_SIZE;
        const SCALE: f32 = (LUT_SIZE - 1) as f32;
        let split = |v: f32| -> (usize, f32) {
            let f = v.clamp(0.0, 1.0) * SCALE;
            let i = (f as i32).clamp(0, N as i32 - 2);
            (i as usize, f - i as f32)
        };
        let ((r, tr), (g, tg), (b, tb)) = (split(x[0]), split(x[1]), split(x[2]));
        let i000 = (r * N + g) * N + b;
        // The two nodes along b as one fixed-size fetch (one bounds check, no aliasing doubts).
        let edge = |i: usize| -> [f32; 4] {
            let pair: &[Node; 2] = self.lut[i..i + 2].try_into().expect("two nodes");
            lerp4(pair[0].0, pair[1].0, tb)
        };
        let c0 = lerp4(edge(i000), edge(i000 + N), tg);
        let c1 = lerp4(edge(i000 + N * N), edge(i000 + N * N + N), tg);
        let c = lerp4(c0, c1, tr);
        [c[0], c[1], c[2]]
    }

    /// Render a tile in place (8-bit RGBA rows `stride` bytes apart, alpha untouched).
    /// `rect` is the part of the map the tile covers. Runs on the calling thread.
    pub fn render(&self, rgba: &mut [u8], w: usize, h: usize, stride: usize, rect: TileRect) -> Result<(), String> {
        // Paper is the most common pixel by far; where all the table nodes around it are the
        // night paper, interpolating them gives exactly the night paper.
        let (from, paper) = (self.paper_from, self.paper_rgb);
        self.render_with(rgba, w, h, stride, rect, |x| {
            if x[0] >= from && x[1] >= from && x[2] >= from {
                paper
            } else {
                self.lut_rgb(x).map(|v| (v + 0.5) as u8)
            }
        })
    }

    /// [`DarkStyle::render`] with the exact formula per pixel instead of the lookup table
    /// (slow; for tests and reference renders).
    pub fn render_exact(
        &self,
        rgba: &mut [u8],
        w: usize,
        h: usize,
        stride: usize,
        rect: TileRect,
    ) -> Result<(), String> {
        self.render_with(rgba, w, h, stride, rect, |x| {
            dark_pixel(x.map(|v| v as f64), &self.params).map(|v| (v * 255.0 + 0.5) as u8)
        })
    }

    fn render_with<F: Fn([f32; 3]) -> [u8; 3]>(
        &self,
        rgba: &mut [u8],
        w: usize,
        h: usize,
        stride: usize,
        rect: TileRect,
        shade: F,
    ) -> Result<(), String> {
        rect.validate()?;
        if w == 0 || h == 0 || stride < w * 4 {
            return Err(format!("invalid tile: {w}×{h} pixels, {stride} bytes per row"));
        }
        let need = stride.checked_mul(h - 1).and_then(|n| n.checked_add(w * 4)).ok_or("tile too large")?;
        if rgba.len() < need {
            return Err(format!("tile buffer holds {} bytes, needs {need}", rgba.len()));
        }
        let cols = self.paper.cols;
        // Grid columns and weights per tile column (the same for every row).
        let xs: Vec<(usize, usize, f32)> =
            (0..w).map(|x| grid_axis(rect.u0 + (x as f64 + 0.5) / w as f64 * rect.du, cols)).collect();
        let mut row3 = vec![[0.0f32; 3]; cols];
        // The paper grid interpolated at this row, × 255 (fourth lane unused): the flat field
        // is `min(c / (255 · paper), 1)` per channel.
        let mut paper_row = vec![[1.0f32; 4]; cols];
        for y in 0..h {
            self.paper.row_at(rect.v0 + (y as f64 + 0.5) / h as f64 * rect.dv, &mut row3);
            for (o, c) in paper_row.iter_mut().zip(&row3) {
                *o = [255.0 * c[0], 255.0 * c[1], 255.0 * c[2], 1.0];
            }
            let row = &mut rgba[y * stride..y * stride + w * 4];
            for (px, &(i0, i1, fx)) in row.chunks_exact_mut(4).zip(&xs) {
                let p = lerp4(paper_row[i0], paper_row[i1], fx);
                let x =
                    [(px[0] as f32 / p[0]).min(1.0), (px[1] as f32 / p[1]).min(1.0), (px[2] as f32 / p[2]).min(1.0)];
                let o = shade(x);
                px[..3].copy_from_slice(&o);
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hue_deg(lab: [f64; 3]) -> f64 {
        lab[2].atan2(lab[1]).to_degrees()
    }

    #[test]
    fn oklab_reference_values() {
        let white = srgb_to_oklab([1.0, 1.0, 1.0]);
        assert!((white[0] - 1.0).abs() < 1e-6 && white[1].abs() < 1e-6 && white[2].abs() < 1e-6, "{white:?}");
        let black = srgb_to_oklab([0.0, 0.0, 0.0]);
        assert!(black.iter().all(|v| v.abs() < 1e-9), "{black:?}");
        // Pure sRGB red (Ottosson's reference values).
        let red = srgb_to_oklab([1.0, 0.0, 0.0]);
        for (got, want) in red.iter().zip([0.627_955, 0.224_863, 0.125_846]) {
            assert!((got - want).abs() < 1e-4, "{red:?}");
        }
        for rgb in [[0.2, 0.5, 0.9], [0.95, 0.83, 0.16], [0.13, 0.13, 0.14], [0.6, 0.45, 0.3]] {
            let back = oklab_to_srgb(srgb_to_oklab(rgb));
            for (a, b) in back.iter().zip(rgb) {
                assert!((a - b).abs() < 1e-6, "{rgb:?} → {back:?}");
            }
        }
    }

    #[test]
    fn gamut_mapping_keeps_lightness_and_hue() {
        // Far outside the gamut: a light, very saturated blue and a dark saturated yellow.
        for lab in [[0.8, -0.1, -0.3], [0.4, 0.05, 0.3], [0.9, 0.25, 0.1]] {
            assert!(!in_gamut(oklab_to_linear(lab)));
            let out = srgb_to_oklab(oklab_to_srgb(lab));
            assert!((out[0] - lab[0]).abs() < 2e-3, "{lab:?} → {out:?}");
            let dh = (hue_deg(out) - hue_deg(lab) + 540.0) % 360.0 - 180.0;
            assert!(dh.abs() < 1.0, "{lab:?} → {out:?}");
            assert!(out[1].hypot(out[2]) < lab[1].hypot(lab[2]));
        }
    }

    #[test]
    fn smoothstep_and_quantile() {
        assert_eq!(smoothstep(0.012, 0.03, 0.0), 0.0);
        assert_eq!(smoothstep(0.012, 0.03, 0.05), 1.0);
        assert!((smoothstep(0.0, 1.0, 0.5) - 0.5).abs() < 1e-12);
        let mut v = vec![5.0f32, 1.0, 4.0, 2.0, 3.0];
        assert_eq!(quantile(&mut v, 0.5), 3.0);
        assert!((quantile(&mut v, 0.9) - 4.6).abs() < 1e-6);
        assert_eq!(quantile(&mut v, 0.0), 1.0);
        assert_eq!(quantile(&mut v, 1.0), 5.0);
        let mut hist = [0u32; 256];
        for x in [1usize, 2, 3, 4, 5] {
            hist[x] += 1;
        }
        assert!((hist_quantile(&hist, 5, 0.9) - 4.6).abs() < 1e-9);
    }

    #[test]
    fn paper_grid_sampling() {
        let g = PaperGrid { cols: 2, rows: 1, rgb: vec![0.5, 0.5, 0.5, 1.0, 0.8, 0.6] };
        assert_eq!(g.sample(0.25, 0.5), [0.5, 0.5, 0.5]);
        assert_eq!(g.sample(0.75, 0.5), [1.0, 0.8, 0.6]);
        assert_eq!(g.sample(0.0, 0.0), [0.5, 0.5, 0.5]); // clamped at the edge
        assert_eq!(g.sample(1.5, 7.0), [1.0, 0.8, 0.6]);
        let mid = g.sample(0.5, 0.5);
        assert!((mid[0] - 0.75).abs() < 1e-6 && (mid[2] - 0.55).abs() < 1e-6);
    }

    #[test]
    fn paper_becomes_night_paper_and_black_becomes_light() {
        for p in [DarkParams::PRINT, DarkParams { ink: 0.4, dead: 0.06, gain: 1.8 }] {
            let paper = dark_pixel([1.0, 1.0, 1.0], &p);
            for (a, b) in paper.iter().zip(NIGHT_PAPER_SRGB) {
                assert!((a - b).abs() < 1e-6, "{paper:?}");
            }
            let ink = srgb_to_oklab(dark_pixel([0.0, 0.0, 0.0], &p));
            assert!((ink[0] - INK_LIGHTNESS).abs() < 1e-6 && ink[1].hypot(ink[2]) < 1e-6, "{ink:?}");
        }
    }

    #[test]
    fn profile_json_round_trip_and_validation() {
        let profile = DarkProfile {
            params: DarkParams { ink: 0.1534, dead: 0.0213, gain: 1.0712 },
            paper: PaperGrid {
                cols: 2,
                rows: 2,
                rgb: vec![0.9412, 0.93, 0.9, 1.0, 1.0, 1.0, 0.5, 0.25, 0.2, 0.7, 0.7, 0.7],
            },
        };
        let s = profile.to_json();
        assert_eq!(DarkProfile::from_json(&s).unwrap(), profile);
        let mut v: Value = serde_json::from_str(&s).unwrap();
        for (path, bad) in [
            ("version", json!(2)),
            ("ink", json!("x")),
            ("gain", json!(0.0)),
            ("dead", json!(0.9)),
            ("status", json!("failed")),
        ] {
            let mut w = v.clone();
            w[path] = bad;
            assert!(DarkProfile::from_json(&w.to_string()).is_err(), "{path}");
        }
        v["paper"]["cols"] = json!(3);
        assert!(DarkProfile::from_json(&v.to_string()).is_err());
        v["paper"]["cols"] = json!(2);
        v["paper"]["rgb"][4] = json!(1.5);
        assert!(DarkProfile::from_json(&v.to_string()).is_err());
        assert!(DarkProfile::from_json("{").is_err());
        assert!(DarkProfile::from_json("[]").is_err());
    }

    #[test]
    fn paper_corner_of_the_table_is_exactly_the_night_paper() {
        for (p, expect_from) in [(DarkParams::PRINT, 0.985), (DarkParams { ink: 0.4, dead: 0.06, gain: 1.8 }, 0.95)] {
            let style = DarkStyle::new(&DarkProfile { params: p, paper: PaperGrid::uniform([1.0; 3]) });
            assert!(style.paper_from <= expect_from, "{p:?}: paper corner from {}", style.paper_from);
            assert_eq!(style.paper_rgb, [33, 33, 36]);
            // Interpolating inside the corner gives the same bytes as the shortcut.
            let mut s = 7u32;
            let mut next = || {
                s = s.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
                (s >> 8) as f32 / (1u32 << 24) as f32
            };
            for _ in 0..20_000 {
                let x = [0, 1, 2].map(|_| style.paper_from + (1.0 - style.paper_from) * next());
                assert_eq!(style.lut_rgb(x).map(|v| (v + 0.5) as u8), style.paper_rgb, "{x:?}");
            }
            // One node below the corner is no longer all paper.
            let below = style.paper_from - 1.0 / 64.0;
            let n = LUT_SIZE;
            let k = (below * 64.0).round() as usize;
            let differs = (k..n).any(|r| {
                (k..n).any(|g| (k..n).any(|b| style.lut[(r * n + g) * n + b].0 != style.lut[n * n * n - 1].0))
            });
            assert!(differs);
        }
    }

    #[test]
    fn render_rejects_bad_arguments() {
        let style = DarkStyle::new(&DarkProfile { params: DarkParams::PRINT, paper: PaperGrid::uniform([1.0; 3]) });
        let mut buf = vec![255u8; 16 * 4];
        assert!(style.render(&mut buf, 4, 4, 16, TileRect::FULL).is_ok());
        assert_eq!(&buf[..4], &[33, 33, 36, 255]);
        assert!(style.render(&mut buf, 0, 4, 16, TileRect::FULL).is_err());
        assert!(style.render(&mut buf, 4, 4, 15, TileRect::FULL).is_err());
        assert!(style.render(&mut buf, 4, 5, 16, TileRect::FULL).is_err());
        let bad = [
            TileRect { du: 0.0, ..TileRect::FULL },
            TileRect { dv: -1.0, ..TileRect::FULL },
            TileRect { u0: f64::NAN, ..TileRect::FULL },
            TileRect { du: f64::INFINITY, ..TileRect::FULL },
        ];
        for r in bad {
            assert!(style.render(&mut buf, 4, 4, 16, r).is_err(), "{r:?}");
        }
    }
}
