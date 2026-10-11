//! Page detection: find the sheet of paper (the map page) in a photo, so the
//! app can flatten it.
//!
//! Classic deterministic image analysis for a light sheet of paper on a
//! darker or differently coloured surface (wood, a dark or grey desk, a
//! cluttered table):
//!
//! 1. **Downsample** by block averaging to at most 800 px on the long side.
//! 2. **Paper score** per pixel: its luminance relative to the brightest
//!    surroundings (a max filter over 12 % of the image, smoothed), which
//!    cancels shading, vignetting and a one-sided shadow, times a Gaussian of
//!    its chromaticity distance to the paper's (the median chromaticity of the
//!    bright pixels), which separates the paper from wood and coloured objects
//!    whatever the colour of the light.
//! 3. **Segmentation**: Otsu threshold, a 5×5 closing (bridges ink lines
//!    across the page), a 3×3 opening (drops speckle and thin bridges to the
//!    surroundings), connected components. The page is the large, solid
//!    (area / convex hull area) component that does not run along the image
//!    border; its holes (map ink, a logo) are filled.
//! 4. **Coarse quad**: the largest-area quadrilateral on the component's
//!    convex hull.
//! 5. **Refinement** at full resolution, side by side: the paper → background
//!    step is located on profiles across the side (strongest drop of the
//!    paper score into a lasting low, so a printed frame line just inside the
//!    edge does not count; sub-pixel at the half-level crossing), and a line is fitted
//!    with a Huber loss to the edge points within 1 % of the image size of the
//!    coarse side, over its middle 80 %. Adjacent lines meet at the corners.
//!    Folded paper has wavy, layered edges and lifted corners: the gate drops
//!    bulges and layers, the fit averages the waviness, and the corners come
//!    from the sides rather than from the (often curled) tips.
//! 6. **Confidence** from the contrast of the step, the fraction of each side
//!    that shows a clear edge close to the fitted line and how straight it is
//!    (a folded sheet scores lower), the solidity of the region and the corner
//!    angles; halved when the page runs off the photo. Detection fails when no
//!    page stands out, or when the page fills (nearly) the whole frame (an
//!    already flat scan or screenshot).
//!
//! Coordinates are continuous pixel coordinates of the input raster (pixel
//! centres at integer + 0.5), corners in the order TL, TR, BR, BL as seen in
//! the image.

use crate::image::{Rgb8, RgbaView};
use rayon::prelude::*;

/// The page found in a photo.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PageQuad {
    /// Corners TL, TR, BR, BL (as seen in the image) in continuous pixel
    /// coordinates of the photo.
    pub corners: [(f64, f64); 4],
    /// 0..1: how clearly the page stands out along its four sides.
    pub confidence: f64,
    /// Quad area / image area.
    pub coverage: f64,
}

/// Find the page in a photo; `None` when no page stands out from its
/// surroundings or the page fills the whole frame (see [`find_page`]).
pub fn detect_page(img: &Rgb8) -> Option<PageQuad> {
    find_page(img).ok()
}

/// Like [`detect_page`], with the reason on failure.
pub fn find_page(img: &Rgb8) -> Result<PageQuad, String> {
    if img.data.len() < img.w * img.h * 3 {
        return Err("pixel buffer too short".into());
    }
    detect(&Src { data: &img.data, w: img.w, h: img.h, stride: img.w * 3, bpp: 3 })
}

/// [`find_page`] on borrowed RGBA pixels (alpha ignored), without a copy.
pub fn find_page_rgba(img: &RgbaView) -> Result<PageQuad, String> {
    detect(&Src { data: img.data, w: img.width, h: img.height, stride: img.stride, bpp: 4 })
}

/// Smallest image side accepted (pixels).
const MIN_SIDE: usize = 48;
/// Long side of the analysis image (pixels, at most).
const TARGET_LONG: usize = 800;
/// Radius of the max filter that estimates the local paper level (fraction of the long side).
const LEVEL_RADIUS: f64 = 0.12;
/// Chromaticity tolerance of the paper score.
const CHROMA_SIGMA: f32 = 0.05;
/// Smallest page accepted (fraction of the image area).
const MIN_PAGE_AREA: f64 = 0.03;
/// A page covering this much of the image is the whole frame (a scan), not a photo of a page.
const FULL_FRAME: f64 = 0.95;
/// Part of each side the line is fitted on (fractions of the coarse side).
const FIT_RANGE: (f64, f64) = (0.1, 0.9);
/// Edge points farther than this from the coarse side are ignored (fraction of the long side).
const GATE: f64 = 0.01;
/// Smallest paper-score drop that counts as an edge.
const MIN_STEP: f32 = 0.15;
/// Below this confidence nothing stands out.
const MIN_CONFIDENCE: f64 = 0.1;

type P2 = (f64, f64);

/// Borrowed 8-bit RGB(A) pixels.
#[derive(Clone, Copy)]
struct Src<'a> {
    data: &'a [u8],
    w: usize,
    h: usize,
    stride: usize,
    bpp: usize,
}

impl Src<'_> {
    #[inline]
    fn px(&self, x: usize, y: usize) -> [f32; 3] {
        let i = y * self.stride + x * self.bpp;
        [self.data[i] as f32, self.data[i + 1] as f32, self.data[i + 2] as f32]
    }

    /// Bilinear sample at continuous coordinates (pixel centres at + 0.5),
    /// clamped to the image.
    #[inline]
    fn sample(&self, u: f64, v: f64) -> [f32; 3] {
        let x = (u - 0.5).clamp(0.0, (self.w - 1) as f64);
        let y = (v - 0.5).clamp(0.0, (self.h - 1) as f64);
        let (x0, y0) = (x as usize, y as usize);
        let (x1, y1) = ((x0 + 1).min(self.w - 1), (y0 + 1).min(self.h - 1));
        let (fx, fy) = ((x - x0 as f64) as f32, (y - y0 as f64) as f32);
        let (a, b, c, d) = (self.px(x0, y0), self.px(x1, y0), self.px(x0, y1), self.px(x1, y1));
        let mut out = [0.0f32; 3];
        for (k, o) in out.iter_mut().enumerate() {
            let top = a[k] + (b[k] - a[k]) * fx;
            let bottom = c[k] + (d[k] - c[k]) * fx;
            *o = top + (bottom - top) * fy;
        }
        out
    }
}

fn detect(src: &Src) -> Result<PageQuad, String> {
    let (w, h) = (src.w, src.h);
    if w < MIN_SIDE || h < MIN_SIDE {
        return Err(format!("The image is too small to find a page in ({w}×{h})."));
    }
    if src.bpp < 3 || src.stride < w * src.bpp || src.data.len() < src.stride * (h - 1) + w * src.bpp {
        return Err("pixel buffer too short".into());
    }
    let long = w.max(h) as f64;
    let k = w.max(h).div_ceil(TARGET_LONG).max(1);
    let low = downsample(src, k);
    let field = PaperField::new(low)?;
    let mask = field.mask()?;
    let comp = best_component(&mask, field.w, field.h)?;
    let coarse = max_area_quad(&comp.hull).ok_or("No page found in the photo.")?;
    let coarse = order_corners(coarse.map(|(x, y)| (x * k as f64, y * k as f64)));
    let image_area = (w * h) as f64;
    if quad_area(&coarse) / image_area >= FULL_FRAME + 0.02 {
        return Err("The page fills the whole image (already flat?).".into());
    }

    // Refine each side at full resolution and intersect neighbouring sides.
    let sides: Vec<Side> = (0..4).map(|i| refine_side(src, &field, coarse[i], coarse[(i + 1) % 4], long)).collect();
    let mut corners = coarse;
    let max_move = 0.05 * long;
    for (i, c) in corners.iter_mut().enumerate() {
        let prev = &sides[(i + 3) % 4];
        let next = &sides[i];
        if let Some(p) = intersect(&prev.line, &next.line) {
            if dist(p, *c) <= max_move {
                *c = p;
            }
        }
    }
    let corners = order_corners(corners);
    if !is_convex(&corners) {
        return Err("No page found in the photo (the outline is not a quadrilateral).".into());
    }
    let coverage = (quad_area(&corners) / image_area).min(1.0);
    if coverage >= FULL_FRAME {
        return Err("The page fills the whole image (already flat?).".into());
    }
    if coverage < MIN_PAGE_AREA {
        return Err("No page found in the photo.".into());
    }
    // Confidence: every side must show a clear edge along most of its length,
    // close to a straight line (folded or curled paper costs up to 40 %), and
    // the outline must look like a sheet seen at a plausible angle.
    let side_conf: Vec<f64> = sides
        .iter()
        .map(|s| {
            let support = smoothstep(s.inlier_fraction, 0.3, 0.8);
            let contrast = smoothstep(s.contrast, 0.1, 0.4);
            let straight = 0.6 + 0.4 * smoothstep(s.straight_fraction, 0.2, 0.8);
            support * contrast * straight
        })
        .collect();
    let mut confidence = side_conf.iter().map(|c| c.max(1e-6)).product::<f64>().powf(0.25);
    confidence *= smoothstep(comp.solidity, 0.75, 0.95) * smoothstep(min_angle_deg(&corners), 25.0, 45.0);
    // A page running off the photo is only partly seen.
    let outside = corners.iter().any(|&(x, y)| x < 0.0 || y < 0.0 || x > w as f64 || y > h as f64);
    if comp.border > 0.002 || outside {
        confidence *= 0.5;
    }
    if side_conf.iter().any(|&c| c < 0.05) {
        confidence = confidence.min(0.2);
    }
    if confidence < MIN_CONFIDENCE {
        return Err("No page stands out from its surroundings.".into());
    }
    Ok(PageQuad { corners, confidence, coverage })
}

// ---------------------------------------------------------------------------
// Low-resolution analysis
// ---------------------------------------------------------------------------

/// Block-averaged copy of the image (`k`×`k` blocks; leftover edge pixels dropped).
struct Low {
    w: usize,
    h: usize,
    k: usize,
    rgb: Vec<[f32; 3]>,
}

fn downsample(src: &Src, k: usize) -> Low {
    let rgb = if src.bpp == 4 { block_average::<4>(src, k) } else { block_average::<3>(src, k) };
    Low { w: src.w / k, h: src.h / k, k, rgb }
}

fn block_average<const BPP: usize>(src: &Src, k: usize) -> Vec<[f32; 3]> {
    let (lw, lh) = (src.w / k, src.h / k);
    let mut rgb = vec![[0.0f32; 3]; lw * lh];
    let norm = 1.0 / (k * k) as f32;
    rgb.par_chunks_mut(lw).enumerate().for_each(|(ly, row)| {
        let mut acc = vec![[0u32; 3]; lw];
        for y in ly * k..(ly + 1) * k {
            let line = &src.data[y * src.stride..y * src.stride + lw * k * BPP];
            for (a, block) in acc.iter_mut().zip(line.chunks_exact(k * BPP)) {
                let mut s = [0u32; 3];
                for px in block.chunks_exact(BPP) {
                    s[0] += px[0] as u32;
                    s[1] += px[1] as u32;
                    s[2] += px[2] as u32;
                }
                a[0] += s[0];
                a[1] += s[1];
                a[2] += s[2];
            }
        }
        for (o, a) in row.iter_mut().zip(&acc) {
            *o = [a[0] as f32 * norm, a[1] as f32 * norm, a[2] as f32 * norm];
        }
    });
    rgb
}

#[inline]
fn luma(c: [f32; 3]) -> f32 {
    0.299 * c[0] + 0.587 * c[1] + 0.114 * c[2]
}

/// How much a pixel of colour `c` looks like the paper: luminance relative to
/// the local paper level `level`, times a Gaussian of the chromaticity
/// distance to the paper's chromaticity `chroma`.
#[inline]
fn paper_score(c: [f32; 3], level: f32, chroma: [f32; 3]) -> f32 {
    let s = c[0] + c[1] + c[2];
    if s <= 1e-3 {
        return 0.0;
    }
    let rel = (luma(c) / level.max(1.0)).min(1.0);
    let d2: f32 = (0..3).map(|i| (c[i] / s - chroma[i]).powi(2)).sum();
    rel * (-d2 / (CHROMA_SIGMA * CHROMA_SIGMA)).exp()
}

/// The paper score on the low-resolution image, and what it was made from.
struct PaperField {
    w: usize,
    h: usize,
    k: usize,
    /// Local paper level (smoothed max filter of the luminance).
    level: Vec<f32>,
    /// Chromaticity (r, g, b over their sum) of the paper.
    chroma: [f32; 3],
    /// Paper score 0..1.
    score: Vec<f32>,
    /// Histogram of `score`.
    score_hist: Hist,
}

impl PaperField {
    fn new(low: Low) -> Result<PaperField, String> {
        let (w, h) = (low.w, low.h);
        let y: Vec<f32> = low.rgb.par_iter().map(|&c| luma(c)).collect();
        let r = ((LEVEL_RADIUS * w.max(h) as f64).round() as usize).max(1);
        let level = level_field(&y, w, h, r);
        // Paper chromaticity: median over the bright pixels that are near
        // their local level (the paper is the brightest large surface).
        let y80 = Hist::of(&y, 0.0, 256.0, 256).quantile(0.8);
        let mut sel = [Hist::new(0.0, 1.0, 1024), Hist::new(0.0, 1.0, 1024), Hist::new(0.0, 1.0, 1024)];
        for ((c, &l), &m) in low.rgb.iter().zip(&y).zip(&level) {
            let s = c[0] + c[1] + c[2];
            if l >= y80 && l > 0.85 * m && s > 30.0 {
                for (hist, v) in sel.iter_mut().zip(c) {
                    hist.add(v / s);
                }
            }
        }
        if sel[0].total < 64 {
            return Err("No page stands out from its surroundings (image too dark or flat).".into());
        }
        let chroma = sel.map(|hist| hist.quantile(0.5));
        let score: Vec<f32> = low.rgb.par_iter().zip(&level).map(|(&c, &m)| paper_score(c, m, chroma)).collect();
        let score_hist = Hist::of(&score, 0.0, 1.0, 256);
        Ok(PaperField { w, h, k: low.k, level, chroma, score, score_hist })
    }

    /// Local paper level at a full-resolution point (bilinear).
    fn level_at(&self, u: f64, v: f64) -> f32 {
        let x = (u / self.k as f64 - 0.5).clamp(0.0, (self.w - 1) as f64);
        let y = (v / self.k as f64 - 0.5).clamp(0.0, (self.h - 1) as f64);
        let (x0, y0) = (x as usize, y as usize);
        let (x1, y1) = ((x0 + 1).min(self.w - 1), (y0 + 1).min(self.h - 1));
        let (fx, fy) = ((x - x0 as f64) as f32, (y - y0 as f64) as f32);
        let g = |x: usize, y: usize| self.level[y * self.w + x];
        let top = g(x0, y0) + (g(x1, y0) - g(x0, y0)) * fx;
        let bottom = g(x0, y1) + (g(x1, y1) - g(x0, y1)) * fx;
        top + (bottom - top) * fy
    }

    /// Paper mask: Otsu threshold of the score, closed with a 5×5 square
    /// (bridges dark lines across the page) and opened with a 3×3 one
    /// (drops speckle and thin bridges to the surroundings).
    fn mask(&self) -> Result<Vec<u8>, String> {
        let (lo, hi) = (self.score_hist.quantile(0.01), self.score_hist.quantile(0.99));
        if hi - lo < 0.2 {
            return Err("No page stands out from its surroundings.".into());
        }
        let t = self.score_hist.otsu().clamp(0.25, 0.75);
        let (w, h) = (self.w, self.h);
        let m: Vec<u8> = self.score.iter().map(|&s| (s > t) as u8).collect();
        let closed = morph(&morph(&m, w, h, 2, true), w, h, 2, false);
        Ok(morph(&morph(&closed, w, h, 1, false), w, h, 1, true))
    }
}

/// Local paper level: the maximum of `y` over a (2r+1)² window, smoothed by
/// a box of half that radius. The level is smooth, so it is computed on a
/// grid `f` times coarser (block maxima first) and interpolated back.
fn level_field(y: &[f32], w: usize, h: usize, r: usize) -> Vec<f32> {
    let f = (r / 8).max(1);
    let (cw, ch) = (w.div_ceil(f), h.div_ceil(f));
    let mut pooled = vec![f32::MIN; cw * ch];
    pooled.par_chunks_mut(cw).enumerate().for_each(|(cy, row)| {
        for yy in cy * f..((cy + 1) * f).min(h) {
            for (cx, o) in row.iter_mut().enumerate() {
                let line = &y[yy * w + cx * f..yy * w + ((cx + 1) * f).min(w)];
                *o = line.iter().fold(*o, |a, &b| a.max(b));
            }
        }
    });
    let rc = r.div_ceil(f);
    let coarse = box_filter(&max_filter(&pooled, cw, ch, rc), cw, ch, rc.div_ceil(2));
    let at = |v: f64, n: usize| {
        let c = ((v + 0.5) / f as f64 - 0.5).clamp(0.0, (n - 1) as f64);
        let i = (c as usize).min(n.saturating_sub(2));
        (i, (i + 1).min(n - 1), (c - i as f64) as f32)
    };
    let cols: Vec<(usize, usize, f32)> = (0..w).map(|x| at(x as f64, cw)).collect();
    let mut out = vec![0.0f32; w * h];
    out.par_chunks_mut(w).enumerate().for_each(|(yy, row)| {
        let (y0, y1, fy) = at(yy as f64, ch);
        let (r0, r1) = (&coarse[y0 * cw..(y0 + 1) * cw], &coarse[y1 * cw..(y1 + 1) * cw]);
        for (o, &(x0, x1, fx)) in row.iter_mut().zip(&cols) {
            let top = r0[x0] + (r0[x1] - r0[x0]) * fx;
            let bottom = r1[x0] + (r1[x1] - r1[x0]) * fx;
            *o = top + (bottom - top) * fy;
        }
    });
    out
}

/// Histogram of values in `lo..hi` (values outside go to the end bins).
#[derive(Clone)]
struct Hist {
    lo: f32,
    hi: f32,
    bins: Vec<u64>,
    total: u64,
}

impl Hist {
    fn new(lo: f32, hi: f32, n: usize) -> Hist {
        Hist { lo, hi, bins: vec![0; n.max(1)], total: 0 }
    }

    fn of(v: &[f32], lo: f32, hi: f32, n: usize) -> Hist {
        let mut h = Hist::new(lo, hi, n);
        for &x in v {
            h.add(x);
        }
        h
    }

    #[inline]
    fn add(&mut self, v: f32) {
        let n = self.bins.len();
        let i = ((v - self.lo) / (self.hi - self.lo) * n as f32).clamp(0.0, (n - 1) as f32) as usize;
        self.bins[i] += 1;
        self.total += 1;
    }

    fn edge(&self, i: usize) -> f32 {
        self.lo + i as f32 * (self.hi - self.lo) / self.bins.len() as f32
    }

    /// Value at quantile `q` (0..1), rounded down to its bin's lower edge (so
    /// "≥ quantile" keeps the whole bin); `lo` when empty.
    fn quantile(&self, q: f64) -> f32 {
        let target = (q.clamp(0.0, 1.0) * self.total as f64).ceil().max(1.0) as u64;
        let mut acc = 0;
        for (i, &n) in self.bins.iter().enumerate() {
            acc += n;
            if acc >= target {
                return self.edge(i);
            }
        }
        if self.total == 0 {
            self.lo
        } else {
            self.edge(self.bins.len() - 1)
        }
    }

    /// Otsu threshold: the bin boundary that maximises the between-class variance.
    fn otsu(&self) -> f32 {
        let sum: f64 = self.bins.iter().enumerate().map(|(i, &n)| i as f64 * n as f64).sum();
        let (mut w0, mut s0, mut best, mut t) = (0u64, 0.0f64, -1.0f64, self.bins.len() / 2);
        for (i, &n) in self.bins.iter().enumerate() {
            w0 += n;
            s0 += i as f64 * n as f64;
            if w0 == 0 || w0 == self.total {
                continue;
            }
            let (a, b) = (w0 as f64, (self.total - w0) as f64);
            let between = a * b * (s0 / a - (sum - s0) / b).powi(2);
            if between > best {
                best = between;
                t = i;
            }
        }
        self.edge(t + 1)
    }
}

/// 1-D sliding maximum over `[i - r, i + r]` (van Herk / Gil–Werman).
fn max_1d(src: &[f32], r: usize, out: &mut [f32]) {
    let n = src.len();
    let m = 2 * r + 1;
    let len = n + 2 * r;
    let at = |i: usize| if i >= r && i - r < n { src[i - r] } else { f32::MIN };
    let mut g = vec![f32::MIN; len];
    let mut hh = vec![f32::MIN; len];
    for i in 0..len {
        g[i] = if i % m == 0 { at(i) } else { g[i - 1].max(at(i)) };
    }
    for i in (0..len).rev() {
        hh[i] = if i % m == m - 1 || i == len - 1 { at(i) } else { hh[i + 1].max(at(i)) };
    }
    for (i, o) in out.iter_mut().enumerate().take(n) {
        *o = hh[i].max(g[i + 2 * r]);
    }
}

/// 1-D box mean over `[i - r, i + r]` clipped to the signal.
fn box_1d(src: &[f32], r: usize, out: &mut [f32]) {
    let n = src.len();
    let mut cs = vec![0.0f64; n + 1];
    for i in 0..n {
        cs[i + 1] = cs[i] + src[i] as f64;
    }
    for (i, o) in out.iter_mut().enumerate().take(n) {
        let (a, b) = (i.saturating_sub(r), (i + r + 1).min(n));
        *o = ((cs[b] - cs[a]) / (b - a) as f64) as f32;
    }
}

fn transpose(src: &[f32], w: usize, h: usize) -> Vec<f32> {
    let mut out = vec![0.0f32; w * h];
    out.par_chunks_mut(h).enumerate().for_each(|(x, col)| {
        for (y, o) in col.iter_mut().enumerate() {
            *o = src[y * w + x];
        }
    });
    out
}

/// Apply a 1-D filter along rows, then along columns.
fn separable(src: &[f32], w: usize, h: usize, f: impl Fn(&[f32], &mut [f32]) + Sync) -> Vec<f32> {
    let mut a = vec![0.0f32; w * h];
    a.par_chunks_mut(w).zip(src.par_chunks(w)).for_each(|(o, s)| f(s, o));
    let t = transpose(&a, w, h);
    let mut b = vec![0.0f32; w * h];
    b.par_chunks_mut(h).zip(t.par_chunks(h)).for_each(|(o, s)| f(s, o));
    transpose(&b, h, w)
}

/// Max over a (2r+1)² square.
fn max_filter(src: &[f32], w: usize, h: usize, r: usize) -> Vec<f32> {
    separable(src, w, h, |s, o| max_1d(s, r, o))
}

/// Mean over a (2r+1)² square, clipped at the borders.
fn box_filter(src: &[f32], w: usize, h: usize, r: usize) -> Vec<f32> {
    separable(src, w, h, |s, o| box_1d(s, r, o))
}

/// Binary dilation (or erosion) with a (2r+1)² square; the outside of the
/// image does not count.
fn morph(m: &[u8], w: usize, h: usize, r: usize, dilate: bool) -> Vec<u8> {
    // Window [i - r, i + r] clipped to 0..n: set if any (dilation) or all
    // (erosion) of its `count` set pixels; counts kept as running sums.
    let decide = |count: u32, i: usize, n: usize| {
        let len = ((i + r).min(n - 1) - i.saturating_sub(r) + 1) as u32;
        (if dilate { count > 0 } else { count == len }) as u8
    };
    let mut a = vec![0u8; w * h];
    a.par_chunks_mut(w).zip(m.par_chunks(w)).for_each(|(o, s)| {
        let mut count: u32 = s[..r.min(w)].iter().map(|&v| (v != 0) as u32).sum();
        for i in 0..w {
            if i + r < w {
                count += (s[i + r] != 0) as u32;
            }
            if i > r {
                count -= (s[i - r - 1] != 0) as u32;
            }
            o[i] = decide(count, i, w);
        }
    });
    let mut out = vec![0u8; w * h];
    let mut count = vec![0u32; w];
    let add = |count: &mut [u32], row: &[u8], sign: bool| {
        for (c, &v) in count.iter_mut().zip(row) {
            if v != 0 {
                if sign {
                    *c += 1;
                } else {
                    *c -= 1;
                }
            }
        }
    };
    for y in 0..r.min(h) {
        add(&mut count, &a[y * w..(y + 1) * w], true);
    }
    for y in 0..h {
        if y + r < h {
            add(&mut count, &a[(y + r) * w..(y + r + 1) * w], true);
        }
        if y > r {
            add(&mut count, &a[(y - r - 1) * w..(y - r) * w], false);
        }
        for (o, &c) in out[y * w..(y + 1) * w].iter_mut().zip(&count) {
            *o = decide(c, y, h);
        }
    }
    out
}

/// A candidate page region.
struct Component {
    /// Convex hull of the (hole-filled) region in low-resolution pixel-corner coordinates.
    hull: Vec<P2>,
    /// Filled area / hull area.
    solidity: f64,
    /// Fraction of the image border the region touches.
    border: f64,
}

/// The page among the mask's connected components: large, solid (area close
/// to its convex hull's) and not running along the image border.
fn best_component(mask: &[u8], w: usize, h: usize) -> Result<Component, String> {
    let mut label = vec![0u32; w * h];
    let mut stack = Vec::new();
    let min_area = (MIN_PAGE_AREA * (w * h) as f64 * 0.5) as usize;
    let border_len = (2 * (w + h) - 4) as f64;
    let mut best: Option<(f64, Component)> = None;
    let mut next = 0u32;
    for start in 0..w * h {
        if mask[start] == 0 || label[start] != 0 {
            continue;
        }
        next += 1;
        let id = next;
        label[start] = id;
        stack.push(start);
        let (mut x0, mut x1, mut y0, mut y1) = (w, 0, h, 0);
        let (mut count, mut on_border) = (0usize, 0usize);
        while let Some(i) = stack.pop() {
            let (x, y) = (i % w, i / w);
            count += 1;
            (x0, x1, y0, y1) = (x0.min(x), x1.max(x), y0.min(y), y1.max(y));
            if x == 0 || y == 0 || x == w - 1 || y == h - 1 {
                on_border += 1;
            }
            let mut visit = |j: usize| {
                if mask[j] != 0 && label[j] == 0 {
                    label[j] = id;
                    stack.push(j);
                }
            };
            if x > 0 {
                visit(i - 1);
            }
            if x + 1 < w {
                visit(i + 1);
            }
            if y > 0 {
                visit(i - w);
            }
            if y + 1 < h {
                visit(i + w);
            }
        }
        if count < min_area {
            continue;
        }
        // Fill holes: flood the bounding box (plus a frame) from outside.
        let (bw, bh) = (x1 - x0 + 3, y1 - y0 + 3);
        let inside = |bx: usize, by: usize| -> bool {
            bx >= 1 && by >= 1 && bx <= bw - 2 && by <= bh - 2 && label[(by - 1 + y0) * w + bx - 1 + x0] == id
        };
        let mut outside = vec![false; bw * bh];
        let mut n_out = 1usize;
        outside[0] = true;
        stack.push(0);
        while let Some(i) = stack.pop() {
            let (bx, by) = (i % bw, i / bw);
            let mut visit = |nx: usize, ny: usize| {
                let j = ny * bw + nx;
                if !outside[j] && !inside(nx, ny) {
                    outside[j] = true;
                    n_out += 1;
                    stack.push(j);
                }
            };
            if bx > 0 {
                visit(bx - 1, by);
            }
            if bx + 1 < bw {
                visit(bx + 1, by);
            }
            if by > 0 {
                visit(bx, by - 1);
            }
            if by + 1 < bh {
                visit(bx, by + 1);
            }
        }
        let filled = (bw * bh - n_out) as f64;
        // Hull of the row extents (pixel corners).
        let mut pts = Vec::with_capacity(4 * (y1 - y0 + 1));
        for y in y0..=y1 {
            let row = &label[y * w + x0..=y * w + x1];
            if let (Some(a), Some(b)) = (row.iter().position(|&l| l == id), row.iter().rposition(|&l| l == id)) {
                let (a, b, yf) = ((x0 + a) as f64, (x0 + b + 1) as f64, y as f64);
                pts.extend([(a, yf), (a, yf + 1.0), (b, yf), (b, yf + 1.0)]);
            }
        }
        let hull = convex_hull(pts);
        let hull_area = polygon_area(&hull).abs().max(1.0);
        let solidity = (filled / hull_area).min(1.0);
        let border = on_border as f64 / border_len;
        let score = filled * solidity.powi(3) * (1.0 - border).max(0.0).powi(2);
        if best.as_ref().is_none_or(|b| score > b.0) {
            best = Some((score, Component { hull, solidity, border }));
        }
    }
    match best {
        Some((s, c)) if s > 0.0 && c.hull.len() >= 4 => Ok(c),
        _ => Err("No page found in the photo.".into()),
    }
}

// ---------------------------------------------------------------------------
// Geometry
// ---------------------------------------------------------------------------

#[inline]
fn cross(o: P2, a: P2, b: P2) -> f64 {
    (a.0 - o.0) * (b.1 - o.1) - (a.1 - o.1) * (b.0 - o.0)
}

#[inline]
fn dist(a: P2, b: P2) -> f64 {
    (a.0 - b.0).hypot(a.1 - b.1)
}

/// Signed shoelace area (positive for TL, TR, BR, BL order in image coordinates).
fn polygon_area(p: &[P2]) -> f64 {
    (0..p.len()).map(|i| p[i].0 * p[(i + 1) % p.len()].1 - p[(i + 1) % p.len()].0 * p[i].1).sum::<f64>() / 2.0
}

fn quad_area(q: &[P2; 4]) -> f64 {
    polygon_area(q).abs()
}

/// Convex hull (Andrew's monotone chain), no collinear points.
fn convex_hull(mut pts: Vec<P2>) -> Vec<P2> {
    pts.sort_by(|a, b| a.0.total_cmp(&b.0).then(a.1.total_cmp(&b.1)));
    pts.dedup();
    if pts.len() < 3 {
        return pts;
    }
    let mut hull: Vec<P2> = Vec::with_capacity(2 * pts.len());
    for pass in 0..2 {
        let start = hull.len();
        let iter: Box<dyn Iterator<Item = &P2>> =
            if pass == 0 { Box::new(pts.iter()) } else { Box::new(pts.iter().rev()) };
        for &p in iter {
            while hull.len() >= start + 2 && cross(hull[hull.len() - 2], hull[hull.len() - 1], p) <= 0.0 {
                hull.pop();
            }
            hull.push(p);
        }
        hull.pop();
    }
    hull
}

/// Largest-area quadrilateral with its corners on the vertices of the convex
/// polygon `p` (O(n²): for a fixed first corner, the best second and fourth
/// corners only move forward as the third does).
fn max_area_quad(p: &[P2]) -> Option<[P2; 4]> {
    let n = p.len();
    if n < 4 {
        return None;
    }
    let tri = |a: usize, b: usize, c: usize| cross(p[a % n], p[b % n], p[c % n]).abs();
    let mut best = (0.0, [0usize; 4]);
    for a in 0..n {
        let (mut b, mut d) = (a + 1, a + 3);
        for c in a + 2..a + n - 1 {
            b = b.max(a + 1);
            while b + 1 < c && tri(a, b + 1, c) >= tri(a, b, c) {
                b += 1;
            }
            d = d.max(c + 1);
            while d + 1 < a + n && tri(a, c, d + 1) >= tri(a, c, d) {
                d += 1;
            }
            let area = tri(a, b, c) + tri(a, c, d);
            if area > best.0 {
                best = (area, [a, b, c, d]);
            }
        }
    }
    (best.0 > 0.0).then(|| best.1.map(|i| p[i % n]))
}

/// Corners in TL, TR, BR, BL order as seen in the image: clockwise on screen,
/// starting so that each corner is closest to its corner of the bounding box.
fn order_corners(q: [P2; 4]) -> [P2; 4] {
    let c = (q.iter().map(|p| p.0).sum::<f64>() / 4.0, q.iter().map(|p| p.1).sum::<f64>() / 4.0);
    let mut v = q.to_vec();
    v.sort_by(|a, b| (a.1 - c.1).atan2(a.0 - c.0).total_cmp(&(b.1 - c.1).atan2(b.0 - c.0)));
    let (x0, x1) = (v.iter().map(|p| p.0).fold(f64::MAX, f64::min), v.iter().map(|p| p.0).fold(f64::MIN, f64::max));
    let (y0, y1) = (v.iter().map(|p| p.1).fold(f64::MAX, f64::min), v.iter().map(|p| p.1).fold(f64::MIN, f64::max));
    let boxc = [(x0, y0), (x1, y0), (x1, y1), (x0, y1)];
    let shift = (0..4)
        .min_by(|&s, &t| {
            let cost = |s: usize| (0..4).map(|i| dist(v[(i + s) % 4], boxc[i]).powi(2)).sum::<f64>();
            cost(s).total_cmp(&cost(t))
        })
        .unwrap_or(0);
    [v[shift % 4], v[(shift + 1) % 4], v[(shift + 2) % 4], v[(shift + 3) % 4]]
}

/// Strictly convex with positive orientation.
fn is_convex(q: &[P2; 4]) -> bool {
    (0..4).all(|i| cross(q[i], q[(i + 1) % 4], q[(i + 2) % 4]) > 0.0)
}

/// Smallest interior angle of the quad (degrees).
fn min_angle_deg(q: &[P2; 4]) -> f64 {
    (0..4)
        .map(|i| {
            let (p, c, n) = (q[(i + 3) % 4], q[i], q[(i + 1) % 4]);
            let (a, b) = ((p.0 - c.0, p.1 - c.1), (n.0 - c.0, n.1 - c.1));
            (a.0 * b.1 - a.1 * b.0).abs().atan2(a.0 * b.0 + a.1 * b.1).to_degrees()
        })
        .fold(180.0, f64::min)
}

fn smoothstep(x: f64, lo: f64, hi: f64) -> f64 {
    let t = ((x - lo) / (hi - lo)).clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

/// A line through `p` with unit direction `d`.
#[derive(Debug, Clone, Copy)]
struct Line {
    p: P2,
    d: P2,
}

impl Line {
    fn through(a: P2, b: P2) -> Line {
        let l = dist(a, b).max(1e-12);
        Line { p: a, d: ((b.0 - a.0) / l, (b.1 - a.1) / l) }
    }

    /// Signed distance of `q` from the line.
    #[inline]
    fn offset(&self, q: P2) -> f64 {
        (q.0 - self.p.0) * -self.d.1 + (q.1 - self.p.1) * self.d.0
    }
}

fn intersect(a: &Line, b: &Line) -> Option<P2> {
    let den = a.d.0 * b.d.1 - a.d.1 * b.d.0;
    if den.abs() < 1e-9 {
        return None;
    }
    let t = ((b.p.0 - a.p.0) * b.d.1 - (b.p.1 - a.p.1) * b.d.0) / den;
    Some((a.p.0 + t * a.d.0, a.p.1 + t * a.d.1))
}

/// Weighted total-least-squares line.
fn fit_line_weighted(pts: &[P2], w: &[f64]) -> Option<Line> {
    let sw: f64 = w.iter().sum();
    if sw <= 0.0 {
        return None;
    }
    let mx = pts.iter().zip(w).map(|(p, w)| w * p.0).sum::<f64>() / sw;
    let my = pts.iter().zip(w).map(|(p, w)| w * p.1).sum::<f64>() / sw;
    let (mut sxx, mut sxy, mut syy) = (0.0, 0.0, 0.0);
    for (p, w) in pts.iter().zip(w) {
        let (dx, dy) = (p.0 - mx, p.1 - my);
        sxx += w * dx * dx;
        sxy += w * dx * dy;
        syy += w * dy * dy;
    }
    let theta = 0.5 * (2.0 * sxy).atan2(sxx - syy);
    Some(Line { p: (mx, my), d: (theta.cos(), theta.sin()) })
}

/// Line fit with a Huber loss of scale `c` (iteratively reweighted).
fn fit_line_huber(pts: &[P2], c: f64) -> Option<Line> {
    if pts.len() < 2 {
        return None;
    }
    let mut w = vec![1.0; pts.len()];
    let mut line = fit_line_weighted(pts, &w)?;
    for _ in 0..40 {
        for (wi, p) in w.iter_mut().zip(pts) {
            let r = line.offset(*p).abs();
            *wi = if r <= c { 1.0 } else { c / r };
        }
        let next = fit_line_weighted(pts, &w)?;
        let moved = next.offset(line.p).abs() + (next.d.0 * line.d.1 - next.d.1 * line.d.0).abs() * 1e3;
        line = next;
        if moved < 1e-4 {
            break;
        }
    }
    Some(line)
}

// ---------------------------------------------------------------------------
// Full-resolution refinement
// ---------------------------------------------------------------------------

/// A refined side of the page.
struct Side {
    line: Line,
    /// Fraction of the profiles with a clear edge within the gate of the line.
    inlier_fraction: f64,
    /// Fraction of the profiles with a clear edge within a quarter of the gate
    /// of the line (straightness: lower for folded or curled paper).
    straight_fraction: f64,
    /// Median paper-score drop at those edges.
    contrast: f64,
}

/// Locate the paper edge across the coarse side `a → b` (interior on the
/// right-hand side in image coordinates) and fit a line to it.
fn refine_side(src: &Src, field: &PaperField, a: P2, b: P2, long: f64) -> Side {
    let chord = Line::through(a, b);
    let len = dist(a, b);
    let out_n = (chord.d.1, -chord.d.0);
    let gate = (GATE * long).max(3.0);
    let half = ((0.0015 * long).round() as usize).max(2);
    // The profile covers the gate plus the step's averaging windows.
    let reach = (gate.ceil() as usize + 4 * half + 2) as i64;
    let strip = (0.0006 * long).max(1.0);
    let n_pos = ((len / 4.0) as usize).clamp(16, 240);
    let found: Vec<Option<(P2, f64, f32)>> = (0..n_pos)
        .into_par_iter()
        .map(|j| {
            let t = FIT_RANGE.0 + (FIT_RANGE.1 - FIT_RANGE.0) * (j as f64 + 0.5) / n_pos as f64;
            let base = (a.0 + t * (b.0 - a.0), a.1 + t * (b.1 - a.1));
            let level = field.level_at(base.0, base.1);
            let prof: Vec<f32> = (-reach..=reach)
                .map(|s| {
                    let c = (base.0 + s as f64 * out_n.0, base.1 + s as f64 * out_n.1);
                    let mut acc = [0.0f32; 3];
                    for o in [-strip, 0.0, strip] {
                        let v = src.sample(c.0 + o * chord.d.0, c.1 + o * chord.d.1);
                        for i in 0..3 {
                            acc[i] += v[i] / 3.0;
                        }
                    }
                    paper_score(acc, level, field.chroma)
                })
                .collect();
            let s = edge_offset(&prof, half)?;
            let off = s.0 - reach as f64;
            Some(((base.0 + off * out_n.0, base.1 + off * out_n.1), off, s.1))
        })
        .collect();
    let pts: Vec<P2> = found.iter().flatten().filter(|f| f.1.abs() <= gate).map(|f| f.0).collect();
    let huber = (0.0005 * long).max(1.0);
    let line = if pts.len() >= 8.max(n_pos / 8) { fit_line_huber(&pts, huber).unwrap_or(chord) } else { chord };
    let mut drops: Vec<f32> = found.iter().flatten().filter(|f| line.offset(f.0).abs() <= gate).map(|f| f.2).collect();
    let straight = found.iter().flatten().filter(|f| line.offset(f.0).abs() <= 0.25 * gate).count();
    let inlier_fraction = drops.len() as f64 / n_pos as f64;
    let straight_fraction = straight as f64 / n_pos as f64;
    drops.sort_by(f32::total_cmp);
    let contrast = drops.get(drops.len() / 2).map_or(0.0, |&d| d as f64);
    Side { line, inlier_fraction, straight_fraction, contrast }
}

/// Position (in samples, sub-pixel) of the strongest paper → background drop
/// on a profile running outwards, with its height; None when there is none.
/// The drop compares `half` samples before with `3·half` samples after, so a
/// dark line printed on the page (it ends in paper again) scores lower than
/// the page edge (background beyond).
fn edge_offset(p: &[f32], half: usize) -> Option<(f64, f32)> {
    let n = p.len();
    if n < 4 * half + 2 {
        return None;
    }
    let mut cs = vec![0.0f32; n + 1];
    for i in 0..n {
        cs[i + 1] = cs[i] + p[i];
    }
    let mean = |a: usize, b: usize| (cs[b] - cs[a]) / (b - a).max(1) as f32;
    // Drop between sample j - 1 and j.
    let (mut best, mut bj) = (f32::MIN, 0usize);
    for j in half..=n - 3 * half {
        let d = mean(j - half, j) - mean(j, j + 3 * half);
        if d > best {
            best = d;
            bj = j;
        }
    }
    if best < MIN_STEP {
        return None;
    }
    let hi = mean(bj.saturating_sub(3 * half), bj - half);
    let lo = mean(bj + half, (bj + 3 * half).min(n));
    let mid = (hi + lo) / 2.0;
    // Half-level crossing closest to the drop.
    let mut pos: Option<f64> = None;
    for i in bj.saturating_sub(half).max(1)..(bj + half).min(n) {
        let (u, v) = (p[i - 1], p[i]);
        if u >= mid && v < mid {
            let x = (i - 1) as f64 + ((u - mid) / (u - v)) as f64;
            if pos.is_none_or(|q| (x - bj as f64 + 0.5).abs() < (q - bj as f64 + 0.5).abs()) {
                pos = Some(x);
            }
        }
    }
    Some((pos.unwrap_or(bj as f64 - 0.5), best))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn naive_max(v: &[f32], r: usize) -> Vec<f32> {
        (0..v.len())
            .map(|i| v[i.saturating_sub(r)..(i + r + 1).min(v.len())].iter().cloned().fold(f32::MIN, f32::max))
            .collect()
    }

    #[test]
    fn sliding_max_matches_naive() {
        let v: Vec<f32> = (0..97).map(|i| ((i * 37 % 23) as f32).sin()).collect();
        for r in [1, 2, 5, 13, 60, 200] {
            let mut out = vec![0.0; v.len()];
            max_1d(&v, r, &mut out);
            assert_eq!(out, naive_max(&v, r), "r = {r}");
        }
        let img: Vec<f32> = (0..30 * 20).map(|i| ((i * 7919) % 101) as f32).collect();
        let m = max_filter(&img, 30, 20, 3);
        for (y, x) in [(0usize, 0usize), (10, 15), (19, 29), (5, 2)] {
            let mut want = f32::MIN;
            for yy in y.saturating_sub(3)..(y + 4).min(20) {
                for xx in x.saturating_sub(3)..(x + 4).min(30) {
                    want = want.max(img[yy * 30 + xx]);
                }
            }
            assert_eq!(m[y * 30 + x], want);
        }
    }

    #[test]
    fn morphology_matches_naive() {
        let (w, h) = (23usize, 17usize);
        let m: Vec<u8> = (0..w * h).map(|i| ((i * 7919 + i / 5) % 7 < 4) as u8).collect();
        for r in [1usize, 2, 3] {
            for dilate in [false, true] {
                let got = morph(&m, w, h, r, dilate);
                for y in 0..h {
                    for x in 0..w {
                        let win = (y.saturating_sub(r)..(y + r + 1).min(h))
                            .flat_map(|yy| (x.saturating_sub(r)..(x + r + 1).min(w)).map(move |xx| (xx, yy)));
                        let want = if dilate {
                            win.clone().any(|(xx, yy)| m[yy * w + xx] != 0)
                        } else {
                            win.clone().all(|(xx, yy)| m[yy * w + xx] != 0)
                        };
                        assert_eq!(got[y * w + x] != 0, want, "r {r} dilate {dilate} at {x},{y}");
                    }
                }
            }
        }
    }

    #[test]
    fn level_field_follows_the_bright_surroundings() {
        let (w, h) = (300usize, 200usize);
        let flat = vec![90.0f32; w * h];
        assert!(level_field(&flat, w, h, 36).iter().all(|v| (v - 90.0).abs() < 1e-3));
        // A bright square: the level near it is its brightness, far away the background's.
        let mut y = vec![50.0f32; w * h];
        for yy in 80..120 {
            for xx in 130..170 {
                y[yy * w + xx] = 200.0;
            }
        }
        let l = level_field(&y, w, h, 24);
        assert!((l[100 * w + 150] - 200.0).abs() < 1.0);
        assert!((l[10 * w + 10] - 50.0).abs() < 1.0);
    }

    #[test]
    fn box_filter_keeps_constant_and_averages() {
        let img = vec![3.0f32; 25 * 17];
        assert!(box_filter(&img, 25, 17, 4).iter().all(|v| (v - 3.0).abs() < 1e-5));
        let mut out = vec![0.0; 5];
        box_1d(&[0.0, 0.0, 6.0, 0.0, 0.0], 1, &mut out);
        assert_eq!(out, [0.0, 2.0, 2.0, 2.0, 0.0]);
    }

    #[test]
    fn histogram_quantiles_and_otsu() {
        let v: Vec<f32> = (0..1000).map(|i| if i % 3 == 0 { 0.2 + (i % 7) as f32 * 0.01 } else { 0.8 }).collect();
        let h = Hist::of(&v, 0.0, 1.0, 256);
        let t = h.otsu();
        assert!(t > 0.26 && t < 0.8, "{t}");
        assert!((h.quantile(0.5) - 0.8).abs() < 0.005);
        assert!((h.quantile(0.0) - 0.2).abs() < 0.005);
        let ramp: Vec<f32> = (0..1000).map(|i| i as f32 / 1000.0 * 255.0).collect();
        let h = Hist::of(&ramp, 0.0, 256.0, 256);
        assert!((h.quantile(0.8) - 203.7).abs() < 1.0, "{}", h.quantile(0.8));
        assert_eq!(Hist::new(0.0, 1.0, 10).quantile(0.5), 0.0);
    }

    #[test]
    fn largest_quad_on_a_hull() {
        // A rotated rectangle with its sides bulging slightly (extra hull vertices).
        let corners = [(10.0, 20.0), (110.0, 5.0), (130.0, 140.0), (30.0, 155.0)];
        let mut pts = Vec::new();
        for i in 0..4 {
            let (a, b) = (corners[i], corners[(i + 1) % 4]);
            for k in 0..10 {
                let t = k as f64 / 10.0;
                let bulge = 0.8 * (std::f64::consts::PI * t).sin();
                let n = ((b.1 - a.1) / dist(a, b), -(b.0 - a.0) / dist(a, b));
                pts.push((a.0 + t * (b.0 - a.0) + bulge * n.0, a.1 + t * (b.1 - a.1) + bulge * n.1));
            }
        }
        let hull = convex_hull(pts);
        assert!(hull.len() > 8);
        let q = order_corners(max_area_quad(&hull).unwrap());
        for (got, want) in q.iter().zip(&corners) {
            assert!(dist(*got, *want) < 1e-9, "{got:?} vs {want:?}");
        }
        assert!(max_area_quad(&hull[..3]).is_none());
    }

    #[test]
    fn corner_order_is_tl_tr_br_bl() {
        let want = [(10.0, 10.0), (90.0, 14.0), (95.0, 120.0), (5.0, 110.0)];
        for perm in [[2, 0, 3, 1], [3, 2, 1, 0], [1, 2, 3, 0]] {
            let q = perm.map(|i| want[i]);
            assert_eq!(order_corners(q), want);
        }
        assert!(is_convex(&want));
        assert!(!is_convex(&[want[0], want[2], want[1], want[3]]));
    }

    #[test]
    fn huber_line_ignores_outliers() {
        let mut pts: Vec<P2> = (0..100).map(|i| (i as f64, 0.5 * i as f64 + 3.0)).collect();
        for i in (0..100).step_by(10) {
            pts[i].1 += 40.0;
        }
        let l = fit_line_huber(&pts, 1.0).unwrap();
        for x in [0.0, 50.0, 99.0] {
            assert!(l.offset((x, 0.5 * x + 3.0)).abs() < 0.3);
        }
        let v = Line::through((7.0, 0.0), (7.0, 10.0));
        let p = intersect(&l, &v).unwrap();
        assert!(dist(p, (7.0, 6.5)) < 0.3);
        assert!(intersect(&v, &v).is_none());
    }

    #[test]
    fn edge_offset_finds_the_step() {
        // Paper (1.0) then background (0.1), crossing at 20.25 with a ramp.
        let p: Vec<f32> = (0..41)
            .map(|i| match i {
                ..=19 => 1.0,
                20 => 0.55 + 0.45 * 0.5,
                21 => 0.55 - 0.45 * 0.5,
                _ => 0.1,
            })
            .collect();
        let (x, d) = edge_offset(&p, 3).unwrap();
        assert!((x - 20.5).abs() < 0.01, "{x}");
        assert!(d > 0.7, "{d}");
        assert!(edge_offset(&[0.5; 41], 3).is_none());
        // A black line printed near the page edge is darker than the table
        // beyond the edge, but it is not the edge.
        let p: Vec<f32> = (0..50)
            .map(|i| match i {
                10..=12 => 0.05,
                ..=24 => 1.0,
                _ => 0.4,
            })
            .collect();
        let (x, _) = edge_offset(&p, 3).unwrap();
        assert!((x - 24.5).abs() < 0.01, "{x}");
    }

    /// A white page (with a black frame and lines, like a map) on a brown
    /// table, inside a small image: corners from pixel coverage.
    fn page_image(w: usize, h: usize, q: [P2; 4]) -> Rgb8 {
        let inside = |x: f64, y: f64, q: &[P2; 4]| (0..4).all(|i| cross(q[i], q[(i + 1) % 4], (x, y)) >= 0.0);
        let shrink = |f: f64| {
            let c = (q.iter().map(|p| p.0).sum::<f64>() / 4.0, q.iter().map(|p| p.1).sum::<f64>() / 4.0);
            q.map(|p| (c.0 + f * (p.0 - c.0), c.1 + f * (p.1 - c.1)))
        };
        let (frame_out, frame_in) = (shrink(0.9), shrink(0.89));
        let mut data = vec![0u8; w * h * 3];
        for y in 0..h {
            for x in 0..w {
                // 4×4 supersampling.
                let mut acc = [0.0f32; 3];
                for sy in 0..4 {
                    for sx in 0..4 {
                        let (u, v) = (x as f64 + (sx as f64 + 0.5) / 4.0, y as f64 + (sy as f64 + 0.5) / 4.0);
                        let c = if !inside(u, v, &q) {
                            [120.0, 80.0, 50.0]
                        } else if (inside(u, v, &frame_out) && !inside(u, v, &frame_in))
                            || (u as usize).is_multiple_of(40)
                        {
                            [20.0, 20.0, 20.0]
                        } else {
                            [235.0, 232.0, 225.0]
                        };
                        for i in 0..3 {
                            acc[i] += c[i] / 16.0;
                        }
                    }
                }
                data[(y * w + x) * 3..(y * w + x) * 3 + 3].copy_from_slice(&acc.map(|v| v.round() as u8));
            }
        }
        Rgb8::from_rgb(w, h, data).unwrap()
    }

    #[test]
    fn finds_a_page_on_a_table() {
        let q = [(80.3, 60.7), (500.2, 90.1), (470.6, 610.4), (60.9, 560.2)];
        let img = page_image(560, 680, q);
        let p = find_page(&img).unwrap();
        for (got, want) in p.corners.iter().zip(&q) {
            assert!(dist(*got, *want) < 1.0, "{got:?} vs {want:?}");
        }
        assert!(p.confidence > 0.8, "{}", p.confidence);
        let area = quad_area(&q) / (560.0 * 680.0);
        assert!((p.coverage - area).abs() < 0.01);
        // The same through RGBA with padded rows.
        let stride = 560 * 4 + 12;
        let mut rgba = vec![7u8; stride * 680];
        for y in 0..680 {
            for x in 0..560 {
                rgba[y * stride + x * 4..y * stride + x * 4 + 3].copy_from_slice(&img.data[(y * 560 + x) * 3..][..3]);
            }
        }
        let view = RgbaView::new(&rgba, 560, 680, stride).unwrap();
        assert_eq!(find_page_rgba(&view).unwrap(), p);
    }

    #[test]
    fn nothing_to_find() {
        let blank = Rgb8::from_rgb(300, 200, vec![200; 300 * 200 * 3]).unwrap();
        assert!(find_page(&blank).is_err());
        let black = Rgb8::from_rgb(300, 200, vec![0; 300 * 200 * 3]).unwrap();
        assert!(find_page(&black).is_err());
        let tiny = Rgb8::from_rgb(20, 20, vec![255; 20 * 20 * 3]).unwrap();
        assert!(find_page(&tiny).is_err());
        let broken = Rgb8 { w: 100, h: 100, data: vec![0; 10] };
        assert!(find_page(&broken).is_err());
        // A page filling the frame is a scan, not a photo of a page.
        let full = page_image(400, 500, [(-2.0, -2.0), (402.0, -2.0), (402.0, 502.0), (-2.0, 502.0)]);
        assert!(find_page(&full).is_err());
    }

    #[test]
    fn extreme_shapes_do_not_panic() {
        // Very long thin images shrink to a few analysis pixels across.
        for (w, h) in [(48usize, 9000usize), (9000, 48), (48, 48)] {
            let data: Vec<u8> = (0..w * h * 3).map(|i| ((i * 2_654_435_761usize) >> 13) as u8).collect();
            let _ = find_page(&Rgb8::from_rgb(w, h, data).unwrap());
            let q =
                [(10.0, 10.0), (w as f64 - 10.0, 12.0), (w as f64 - 12.0, h as f64 - 10.0), (12.0, h as f64 - 12.0)];
            if w * h < 1_000_000 {
                let _ = find_page(&page_image(w, h, q));
            }
        }
    }
}
