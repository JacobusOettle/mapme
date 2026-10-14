//! Contour lines of any map style: thin lines of the contour ink, found at
//! the raster's effective resolution.
//!
//! A colour threshold on the raw pixels (`contour::auto_mask`) works on clean
//! scans with brown contours on white paper, but not on hillshaded, tinted,
//! faded or smoothly upsampled maps, where a contour pixel's colour is a blend
//! of the ink and whatever lies under it, and the lines are several pixels
//! wide and soft. This module instead
//!
//! 1. picks the **analysis level** (`estimate_level`): the integer box
//!    downsampling factor at which the map's lines are about as thin as on a
//!    native raster. A PDF whose 100 ppi raster the app renders at 300 dpi has
//!    every line spread over 3-4 soft pixels; averaging 3 × 3 blocks gives the
//!    raster back. Sharp 300 dpi scans stay at level 1;
//! 2. finds **line pixels** (`ink_lines`): pixels darker than the background on
//!    both sides of them, along the direction and at the distance (1-3 pixels)
//!    where that is most so (a two-sided valley test on the luminance). Edges
//!    of areas, tints and smooth hillshade gradients are not darker on both
//!    sides, so they never count;
//! 3. measures each line pixel's **ink**: its colour divided by the local
//!    background (the brighter of the two sides, per channel), i.e. the colour
//!    the line would have on white paper, averaged over the line pixels around
//!    it. Hillshade, area tints and yellowed paper divide out;
//! 4. chooses the **contour ink** among a few colour models of that ink (warm
//!    brown / tan / orange inks, a strict variant, grey and black) by how much
//!    of each model's lines form long connected lines rather than specks
//!    (`select_model`), with a prior for the warm inks most maps use.
//!
//! The mask is at the analysis level; `ContourProducts` maps its samples back
//! to base-image pixels exactly (level pixel i covers base pixels
//! [i·f, (i+1)·f)).

use crate::contour::{model_from_rgb, ContourColorModel};
use crate::image::{Mask, Rgb8};
use rayon::prelude::*;
use serde::Serialize;

/// Largest analysis level (box downsampling factor).
pub const MAX_LEVEL: usize = 4;

// ---------------------------------------------------------------------------
// Analysis level
// ---------------------------------------------------------------------------

/// Box-average downsample by an integer factor `f` (the last partial row and
/// column of blocks are dropped). Level pixel (i, j) covers base pixels
/// [i·f, (i+1)·f) × [j·f, (j+1)·f).
pub fn downsample_box(img: &Rgb8, f: usize) -> Rgb8 {
    if f <= 1 {
        return img.clone();
    }
    let (w, h) = (img.w / f, img.h / f);
    let mut data = vec![0u8; w * h * 3];
    if w == 0 || h == 0 {
        return Rgb8 { w, h, data };
    }
    let n = (f * f) as u32;
    data.par_chunks_mut(w * 3).enumerate().for_each(|(y, row)| {
        let mut acc = vec![0u32; w * 3];
        for yy in y * f..(y + 1) * f {
            let src = &img.data[yy * img.w * 3..(yy * img.w + w * f) * 3];
            for (x, a) in acc.chunks_exact_mut(3).enumerate() {
                for p in src[x * f * 3..(x + 1) * f * 3].chunks_exact(3) {
                    a[0] += p[0] as u32;
                    a[1] += p[1] as u32;
                    a[2] += p[2] as u32;
                }
            }
        }
        for (o, a) in row.iter_mut().zip(&acc) {
            *o = ((a + n / 2) / n) as u8;
        }
    });
    Rgb8 { w, h, data }
}

/// Windows sampled per image side for the level estimate.
const LEVEL_GRID: usize = 4;
/// Window side (level pixels).
const LEVEL_WINDOW: usize = 192;
/// Valley radii tried for the line-width statistic.
const WIDTH_RADII: usize = 6;
/// A line pixel's width class is the smallest radius whose valley depth
/// reaches this fraction of its deepest.
const WIDTH_FRACTION: f32 = 0.75;
/// Level 1 is kept while at least this share of the line contrast belongs to
/// lines resolved within 2 pixels (sharp native rasters: 0.12-0.8; rasters
/// upsampled 2-3×: 0.00-0.13).
const NATIVE_THIN_SHARE: f32 = 0.10;
/// Level 2 is taken when this share is reached there (2× upsampled rasters at
/// level 2: 0.33-0.55; 3× upsampled ones: 0.06-0.25); otherwise level 3…
const LEVEL2_THIN_SHARE: f32 = 0.30;
/// …unless lines are still wide there too (4× upsampling).
const LEVEL3_THIN_SHARE: f32 = 0.15;
/// Smallest analysis image side (level pixels).
const MIN_LEVEL_SIDE: usize = 320;

/// The analysis level and the line-width statistics it was chosen from.
#[derive(Debug, Clone, Serialize)]
pub struct LevelEstimate {
    pub level: usize,
    /// Per level 1..=MAX_LEVEL tried: the share of line contrast resolved
    /// within 2 pixels (−1 when not measured).
    pub thin_share: Vec<f32>,
    /// Line pixels measured at level 1.
    pub line_px: usize,
    /// Block size of a raster upsampled by pixel repetition (`block_factor`).
    pub block: Option<usize>,
}

/// Share of line contrast at radius ≤ 2 in windows of `img` downsampled by
/// `f`, and the number of line pixels measured.
fn thin_share(img: &Rgb8, f: usize) -> (f32, usize) {
    let side = LEVEL_WINDOW * f;
    if img.w < side || img.h < side {
        return (-1.0, 0);
    }
    let wins = level_windows(img, side);
    // Window of LEVEL_WINDOW level pixels plus a margin of WIDTH_RADII on
    // every side (level pixels outside the image repeat its edge).
    let (n, m) = (LEVEL_WINDOW, WIDTH_RADII);
    let nn = n + 2 * m;
    // Linear offsets of the side pairs per radius (x, y, two diagonals).
    let offs: Vec<[isize; 4]> = (1..=WIDTH_RADII as isize)
        .map(|r| {
            let k = ((r as f64 / std::f64::consts::SQRT_2).round() as isize).max(1);
            let s = nn as isize;
            [r, r * s, k * s + k, k - k * s]
        })
        .collect();
    let parts: Vec<([f64; WIDTH_RADII], usize)> = wins
        .par_iter()
        .map(|&(x0, y0)| {
            // Window luminance at the level (box average of f×f blocks).
            let (lw, lh) = ((img.w / f) as isize, (img.h / f) as isize);
            let (lx0, ly0) = ((x0 / f) as isize - m as isize, (y0 / f) as isize - m as isize);
            let mut lum = vec![0f32; nn * nn];
            for (y, row) in lum.chunks_exact_mut(nn).enumerate() {
                let ly = (ly0 + y as isize).clamp(0, lh - 1) as usize;
                for (x, o) in row.iter_mut().enumerate() {
                    let lx = (lx0 + x as isize).clamp(0, lw - 1) as usize;
                    let mut s = 0u32;
                    for yy in ly * f..(ly + 1) * f {
                        let i = (yy * img.w + lx * f) * 3;
                        for p in img.data[i..i + 3 * f].chunks_exact(3) {
                            s += p[0] as u32 + p[1] as u32 + p[2] as u32;
                        }
                    }
                    *o = s as f32 / (3 * f * f) as f32;
                }
            }
            let mut hist = [0f64; WIDTH_RADII];
            let mut count = 0usize;
            let mut depth = [0f32; WIDTH_RADII];
            for y in m..m + n {
                for x in m..m + n {
                    let i = (y * nn + x) as isize;
                    let c = lum[i as usize];
                    for (d, o) in depth.iter_mut().zip(&offs) {
                        let mut best = f32::NEG_INFINITY;
                        for &di in o {
                            best = best.max(lum[(i + di) as usize].min(lum[(i - di) as usize]) - c);
                        }
                        *d = best;
                    }
                    let mx = depth.iter().cloned().fold(f32::NEG_INFINITY, f32::max);
                    if mx < line_threshold(c + mx) {
                        continue;
                    }
                    let r = depth.iter().position(|&d| d >= WIDTH_FRACTION * mx).unwrap_or(0);
                    hist[r] += mx as f64;
                    count += 1;
                }
            }
            (hist, count)
        })
        .collect();
    let mut hist = [0f64; WIDTH_RADII];
    let mut count = 0;
    for (h, c) in parts {
        for (a, b) in hist.iter_mut().zip(h) {
            *a += b;
        }
        count += c;
    }
    let total: f64 = hist.iter().sum();
    if total <= 0.0 {
        return (-1.0, count);
    }
    (((hist[0] + hist[1]) / total) as f32, count)
}

/// Luminance step (summed over the channels) that counts as an edge between
/// two neighbouring pixels for the block test.
const BLOCK_EDGE: i32 = 72;
/// Edge gaps (pixels between consecutive edges along a row or column) up to
/// this length are counted.
const BLOCK_MAX_GAP: usize = 8;
/// A raster is blocky by `k` when at least this share of its edge gaps are
/// multiples of `k`…
const BLOCK_SHARE: f64 = 0.9;
/// …out of at least this many gaps.
const BLOCK_MIN_GAPS: u64 = 1000;

/// The block size of a raster upsampled by pixel repetition (nearest
/// neighbour; a PDF's low-resolution image drawn without interpolation), if
/// any: such a raster has edges only between its blocks, so the gaps between
/// consecutive edges along rows and columns are multiples of the block size
/// (gaps of 1 never occur), while on a native raster they take every length
/// (the two edges of a 1-pixel line are 1 apart). Measured on the level
/// estimate's windows, along both axes; 2 to `MAX_LEVEL`, else None.
pub fn block_factor(img: &Rgb8) -> Option<usize> {
    let side = LEVEL_WINDOW;
    if img.w < side || img.h < side || img.data.len() != img.w * img.h * 3 {
        return None;
    }
    let wins = level_windows(img, side);
    // Gap histograms along x and y.
    let hists: Vec<[[u64; BLOCK_MAX_GAP + 1]; 2]> = wins
        .par_iter()
        .map(|&(x0, y0)| {
            let lum = |x: usize, y: usize| {
                let i = (y * img.w + x) * 3;
                img.data[i] as i32 + img.data[i + 1] as i32 + img.data[i + 2] as i32
            };
            let mut hist = [[0u64; BLOCK_MAX_GAP + 1]; 2];
            for (axis, h) in hist.iter_mut().enumerate() {
                for a in 0..side {
                    let mut last: Option<usize> = None;
                    for b in 0..side - 1 {
                        let ((x, y), (x2, y2)) = if axis == 0 {
                            ((x0 + b, y0 + a), (x0 + b + 1, y0 + a))
                        } else {
                            ((x0 + a, y0 + b), (x0 + a, y0 + b + 1))
                        };
                        if (lum(x2, y2) - lum(x, y)).abs() > BLOCK_EDGE {
                            if let Some(l) = last {
                                if b - l <= BLOCK_MAX_GAP {
                                    h[b - l] += 1;
                                }
                            }
                            last = Some(b);
                        }
                    }
                }
            }
            hist
        })
        .collect();
    let mut total = [[0u64; BLOCK_MAX_GAP + 1]; 2];
    for h in &hists {
        for axis in 0..2 {
            for g in 0..=BLOCK_MAX_GAP {
                total[axis][g] += h[axis][g];
            }
        }
    }
    // Per axis: None without enough gaps to judge, Some(1) when not blocky.
    let factor = |h: &[u64; BLOCK_MAX_GAP + 1]| -> Option<usize> {
        let n: u64 = h.iter().sum();
        if n < BLOCK_MIN_GAPS {
            return None;
        }
        Some(
            (2..=MAX_LEVEL)
                .rev()
                .find(|&k| {
                    let m: u64 = (k..=BLOCK_MAX_GAP).step_by(k).map(|g| h[g]).sum();
                    m as f64 >= BLOCK_SHARE * n as f64
                })
                .unwrap_or(1),
        )
    };
    let k = match (factor(&total[0]), factor(&total[1])) {
        (Some(a), Some(b)) => a.min(b),
        (Some(a), None) | (None, Some(a)) => a,
        (None, None) => 1,
    };
    (k >= 2).then_some(k)
}

/// Top-left corners of the windows (side `side` base pixels) the level
/// estimate samples: a 4 × 4 grid over the central 80 % of the image
/// (margins hold legends).
fn level_windows(img: &Rgb8, side: usize) -> Vec<(usize, usize)> {
    let mut wins = Vec::new();
    for gy in 0..LEVEL_GRID {
        for gx in 0..LEVEL_GRID {
            let cx = img.w as f64 * (0.1 + 0.8 * (gx as f64 + 0.5) / LEVEL_GRID as f64);
            let cy = img.h as f64 * (0.1 + 0.8 * (gy as f64 + 0.5) / LEVEL_GRID as f64);
            let x0 = ((cx - side as f64 / 2.0).round().max(0.0) as usize).min(img.w - side);
            let y0 = ((cy - side as f64 / 2.0).round().max(0.0) as usize).min(img.h - side);
            wins.push((x0, y0));
        }
    }
    wins
}

/// The analysis level for `img`: 1 for sharp rasters, the upsampling factor
/// for rasters whose lines are spread over several soft pixels (see
/// `LevelEstimate`) or that repeat every pixel in blocks (`block_factor`).
/// Measured on 16 windows; ~10-30 ms.
pub fn estimate_level(img: &Rgb8) -> LevelEstimate {
    let mut thin = vec![-1.0f32; MAX_LEVEL];
    let (s1, line_px) = thin_share(img, 1);
    thin[0] = s1;
    let max_level = (img.w.min(img.h) / MIN_LEVEL_SIDE).clamp(1, MAX_LEVEL);
    let mut level = 1;
    if let Some(k) = block_factor(img) {
        return LevelEstimate { level: k.min(max_level), thin_share: thin, line_px, block: Some(k) };
    }
    // Too few lines to judge, or sharp: level 1.
    if (0.0..NATIVE_THIN_SHARE).contains(&s1) && line_px >= 2000 && max_level >= 2 {
        let (s2, _) = thin_share(img, 2);
        thin[1] = s2;
        level = 2;
        if (0.0..LEVEL2_THIN_SHARE).contains(&s2) && max_level >= 3 {
            let (s3, _) = thin_share(img, 3);
            thin[2] = s3;
            level = 3;
            if (0.0..LEVEL3_THIN_SHARE).contains(&s3) && max_level >= 4 {
                thin[3] = thin_share(img, 4).0;
                level = 4;
            }
        }
    }
    LevelEstimate { level, thin_share: thin, line_px, block: None }
}

// ---------------------------------------------------------------------------
// Line pixels and their ink
// ---------------------------------------------------------------------------

/// A line pixel must be darker than both sides by at least this many
/// luminance levels…
const LINE_MIN_DEPTH: f32 = 6.0;
/// …and by this fraction of the background luminance (faint lines on dark
/// hillshade keep a lower absolute contrast).
const LINE_REL_DEPTH: f32 = 0.05;

/// Valley depth needed for a line pixel over a background of luminance `bg`.
#[inline]
fn line_threshold(bg: f32) -> f32 {
    LINE_MIN_DEPTH.max(LINE_REL_DEPTH * bg)
}

/// Side offsets of the two-sided valley test: along x and y at 1, 2 and 3
/// pixels, diagonally at 1 and 2 steps (1.4 and 2.8 pixels).
const SIDES: [(isize, isize); 10] = [(1, 0), (0, 1), (1, 1), (1, -1), (2, 0), (0, 2), (3, 0), (0, 3), (2, 2), (2, -2)];

/// Line pixels of a raster and the side offset that found each.
pub struct InkLines {
    pub w: usize,
    pub h: usize,
    /// Valley depth (luminance levels × 3, i.e. summed over channels) of the
    /// line through each pixel; 0 where the pixel is not on a line.
    pub depth: Vec<u16>,
    /// Index into `SIDES` of the deepest valley (meaningful where depth > 0).
    pub side: Vec<u8>,
}

impl InkLines {
    pub fn count(&self) -> usize {
        self.depth.par_iter().filter(|&&d| d > 0).count()
    }
}

/// One side offset of the valley test over a row: depth = min(a, b) − c,
/// kept with the offset `k` where deeper than `best`. Written with bit-mask
/// selects over equal-length slices so that it vectorises (a conditional
/// store does not).
#[inline]
fn valley_update(best: &mut [i16], kbest: &mut [i16], sa: &[i16], sb: &[i16], c: &[i16], k: i16) {
    let n = best.len().min(kbest.len()).min(sa.len()).min(sb.len()).min(c.len());
    let (best, kbest, sa, sb, c) = (&mut best[..n], &mut kbest[..n], &sa[..n], &sb[..n], &c[..n]);
    for i in 0..n {
        let v = sa[i].min(sb[i]).wrapping_sub(c[i]);
        let m = ((v > best[i]) as i16).wrapping_neg();
        kbest[i] = (k & m) | (kbest[i] & !m);
        best[i] = best[i].max(v);
    }
}

/// Find the line pixels of `img` (see the module docs).
pub fn ink_lines(img: &Rgb8) -> InkLines {
    let (w, h) = (img.w, img.h);
    let mut depth = vec![0u16; w * h];
    let mut side = vec![0u8; w * h];
    if w < 8 || h < 3 || img.data.len() != w * h * 3 {
        return InkLines { w, h, depth, side };
    }
    let lum: Vec<i16> = img.data.par_chunks_exact(3).map(|p| p[0] as i16 + p[1] as i16 + p[2] as i16).collect();
    depth.par_chunks_mut(w).zip(side.par_chunks_mut(w)).enumerate().for_each(|(y, (drow, srow))| {
        let c = &lum[y * w..(y + 1) * w];
        let row = |yy: isize| {
            let yy = yy.clamp(0, h as isize - 1) as usize;
            &lum[yy * w..(yy + 1) * w]
        };
        let sides = |k: usize, x: usize| {
            let (dx, dy) = SIDES[k];
            let (ra, rb) = (row(y as isize + dy), row(y as isize - dy));
            let xa = (x as isize + dx).clamp(0, w as isize - 1) as usize;
            let xb = (x as isize - dx).clamp(0, w as isize - 1) as usize;
            ra[xa].min(rb[xb])
        };
        // Deepest valley (summed-channel levels) and its side index per pixel
        // of the row: i16 max / min / subtract / select over zipped slices,
        // which vectorises.
        let mut best = vec![i16::MIN; w];
        let mut kbest = vec![0i16; w];
        for (k, &(dx, dy)) in SIDES.iter().enumerate() {
            let (ra, rb) = (row(y as isize + dy), row(y as isize - dy));
            let ad = dx.unsigned_abs();
            let (lo, hi) = (ad, w - ad);
            let sa = &ra[(lo as isize + dx) as usize..(hi as isize + dx) as usize];
            let sb = &rb[(lo as isize - dx) as usize..(hi as isize - dx) as usize];
            valley_update(&mut best[lo..hi], &mut kbest[lo..hi], sa, sb, &c[lo..hi], k as i16);
            for x in (0..lo).chain(hi..w) {
                let v = sides(k, x) - c[x];
                if v > best[x] {
                    best[x] = v;
                    kbest[x] = k as i16;
                }
            }
        }
        // Threshold in summed-channel units: depth ≥ 3·max(MIN, REL·side / 3)
        // with the darker side at c + depth, i.e. depth·(1 − REL) ≥ REL·c.
        let min3 = (3.0 * LINE_MIN_DEPTH) as i16;
        for x in 0..w {
            let d = best[x];
            if d >= min3 && d as f32 * (1.0 - LINE_REL_DEPTH) >= LINE_REL_DEPTH * c[x] as f32 {
                drow[x] = d as u16;
                srow[x] = kbest[x] as u8;
            }
        }
    });
    InkLines { w, h, depth, side }
}

/// Absorption (1 − pixel / background, per channel) of line pixel (x, y),
/// the background being the brighter of the two sides per channel.
#[inline]
fn absorption(img: &Rgb8, lines: &InkLines, x: usize, y: usize) -> [f32; 3] {
    let (dx, dy) = SIDES[lines.side[y * lines.w + x] as usize];
    let (wi, hi) = (img.w as isize, img.h as isize);
    let at = |xx: isize, yy: isize| (yy.clamp(0, hi - 1) as usize * img.w + xx.clamp(0, wi - 1) as usize) * 3;
    let (ia, ib, ip) =
        (at(x as isize + dx, y as isize + dy), at(x as isize - dx, y as isize - dy), (y * img.w + x) * 3);
    let (a, b, p) = (&img.data[ia..ia + 3], &img.data[ib..ib + 3], &img.data[ip..ip + 3]);
    let mut out = [0f32; 3];
    for c in 0..3 {
        let bg = a[c].max(b[c]).max(8) as f32;
        out[c] = (1.0 - p[c] as f32 / bg).max(-1.0);
    }
    out
}

/// The ink of every line pixel: its colour on white paper (255 × (1 −
/// absorption)), the absorption averaged over the line pixels within
/// `INK_RADIUS` weighted by their depth (a faint pixel's colour is noisy).
/// `None` off lines.
pub fn ink_colour(img: &Rgb8, lines: &InkLines, x: usize, y: usize) -> Option<[u8; 3]> {
    let (w, h) = (lines.w, lines.h);
    if lines.depth[y * w + x] == 0 {
        return None;
    }
    let mut acc = [0f32; 3];
    let mut wsum = 0f32;
    for yy in y.saturating_sub(INK_RADIUS)..=(y + INK_RADIUS).min(h - 1) {
        for xx in x.saturating_sub(INK_RADIUS)..=(x + INK_RADIUS).min(w - 1) {
            let d = lines.depth[yy * w + xx];
            if d == 0 {
                continue;
            }
            let a = absorption(img, lines, xx, yy);
            let wt = d as f32;
            for c in 0..3 {
                acc[c] += wt * a[c];
            }
            wsum += wt;
        }
    }
    Some(acc.map(|a| (255.0 * (1.0 - a / wsum)).round().clamp(0.0, 255.0) as u8))
}

// ---------------------------------------------------------------------------
// Contour ink models and their choice
// ---------------------------------------------------------------------------

/// A colour model of the contour ink: a box in hue, chroma and value of the
/// ink colour, with the prior weight of its score.
///
/// Chroma is absolute ((max − min channel) / 255), not HSV saturation: the
/// ink of a faded or faint line is a light colour whose saturation is as
/// noisy as its chroma is small, and JPEG's colour noise is about the same
/// number of levels at any brightness.
#[derive(Debug, Clone, PartialEq)]
pub struct InkModel {
    pub name: &'static str,
    /// Hue range in degrees (wraps when min > max).
    pub hue_min: f32,
    pub hue_max: f32,
    pub chroma_min: f32,
    pub chroma_max: f32,
    pub val_min: f32,
    pub val_max: f32,
    pub prior: f64,
    /// Takes only pixels no other (non-fallback) model takes.
    pub fallback: bool,
}

impl InkModel {
    #[inline]
    pub fn matches(&self, k: [u8; 3]) -> bool {
        self.matches_hcv(hcv(k))
    }

    /// `matches` for a colour's (hue, chroma, value) (`hcv`).
    #[inline]
    pub fn matches_hcv(&self, (h, c, v): (f32, f32, f32)) -> bool {
        if c < self.chroma_min || c > self.chroma_max || v < self.val_min || v > self.val_max {
            return false;
        }
        if self.hue_min <= 0.0 && self.hue_max >= 360.0 {
            true
        } else if self.hue_min <= self.hue_max {
            (self.hue_min..=self.hue_max).contains(&h)
        } else {
            h >= self.hue_min || h <= self.hue_max
        }
    }

    /// The HSV box closest to this model (diagnostics and the session's
    /// `model` field).
    pub fn color_model(&self) -> ContourColorModel {
        ContourColorModel {
            hue_min: self.hue_min,
            hue_max: self.hue_max,
            sat_min: self.chroma_min,
            sat_max: 1.0,
            val_min: self.val_min,
            val_max: self.val_max,
        }
    }
}

/// Hue (degrees), chroma ((max − min) / 255) and value (max / 255) of a colour.
#[inline]
pub fn hcv(k: [u8; 3]) -> (f32, f32, f32) {
    let (r, g, b) = (k[0] as f32, k[1] as f32, k[2] as f32);
    let mx = r.max(g).max(b);
    let d = mx - r.min(g).min(b);
    // Hue as `image::rgb_to_hsv` (without its fmod).
    let h = if d <= 0.0 {
        0.0
    } else if mx == r {
        let h = 60.0 * (g - b) / d;
        if h < 0.0 {
            h + 360.0
        } else {
            h
        }
    } else if mx == g {
        60.0 * ((b - r) / d + 2.0)
    } else {
        60.0 * ((r - g) / d + 4.0)
    };
    (h, d / 255.0, mx / 255.0)
}

/// A warm ink model (brown, tan, orange, red-brown, purple-brown) from
/// `chroma_min` up: the fainter the inks it admits, the wider its hues.
fn warm(name: &'static str, chroma_min: f32, hue_min: f32, hue_max: f32) -> InkModel {
    InkModel {
        name,
        hue_min,
        hue_max,
        chroma_min,
        chroma_max: 0.65,
        val_min: 0.15,
        val_max: 0.99,
        prior: 1.0,
        fallback: false,
    }
}

/// The built-in ink models `select_model` chooses from: warm inks from
/// faded to strong (the choice sets how faint an ink still counts), and grey
/// and black as the fallback.
pub fn ink_models() -> Vec<InkModel> {
    vec![
        warm("warm_faint", 0.015, 330.0, 62.0),
        warm("warm_pale", 0.025, 330.0, 62.0),
        warm("warm", 0.04, 335.0, 60.0),
        warm("warm_clear", 0.07, 340.0, 55.0),
        warm("warm_strong", 0.10, 340.0, 52.0),
        // Grey and black (photocopies, grey contours): text, grid lines and
        // roads share it; chosen only where there is next to no warm line.
        InkModel {
            name: "neutral",
            hue_min: 0.0,
            hue_max: 360.0,
            chroma_min: 0.0,
            chroma_max: 0.05,
            val_min: 0.0,
            val_max: 0.93,
            prior: 1.0,
            fallback: true,
        },
    ]
}

/// How a model's line pixels hang together.
#[derive(Debug, Clone, Serialize)]
pub struct ModelScore {
    pub name: &'static str,
    /// Line pixels of the model.
    pub pixels: usize,
    /// Pixels in connected lines of at least the segment length.
    pub long_px: usize,
    pub score: f64,
}

/// Weight of the pixels in short pieces (specks, text, noise) against a
/// model's score.
const SHORT_PENALTY: f64 = 1.0;

/// Radius (pixels) of the window over which a line pixel's ink is averaged
/// with its neighbours' (weighted by their depth): a faint or faded line's
/// colour is a few levels from grey, about the size of the JPEG noise, and
/// averaging along a few pixels of the line steadies it.
pub const INK_RADIUS: usize = 2;

/// Percentile of each channel taken as the paper colour of the whole map.
const PAPER_PERCENTILE: f64 = 0.98;

/// The paper colour of a map: per channel, the `PAPER_PERCENTILE` of its
/// pixels (paper is the brightest large thing on a map), at least 64.
pub fn paper_colour(img: &Rgb8) -> [f32; 3] {
    let hist = img
        .data
        .par_chunks(3 * 4096)
        .map(|chunk| {
            let mut h = [[0u32; 256]; 3];
            for p in chunk.chunks_exact(3) {
                for c in 0..3 {
                    h[c][p[c] as usize] += 1;
                }
            }
            h
        })
        .reduce(
            || [[0u32; 256]; 3],
            |mut a, b| {
                for c in 0..3 {
                    for v in 0..256 {
                        a[c][v] += b[c][v];
                    }
                }
                a
            },
        );
    let n = (img.w * img.h) as f64;
    [0, 1, 2].map(|c| {
        let target = (PAPER_PERCENTILE * n).ceil() as u64;
        let mut acc = 0u64;
        let mut v = 255usize;
        for (i, &k) in hist[c].iter().enumerate() {
            acc += k as u64;
            if acc >= target.max(1) {
                v = i;
                break;
            }
        }
        (v as f32).max(64.0)
    })
}

/// Per-pixel bits of the models (bit i = models[i]) matching each line pixel's
/// ink. A line drawn over a coloured background has one of two inks,
/// depending on how the map was made: printed inks overprint (multiply) what
/// lies under them, so the ink is the pixel divided by its background
/// (`ink_colour`); digital maps paint opaque or semi-transparent lines over
/// their tints and hillshade, so the ink is the pixel itself, white-balanced
/// by the map's paper (`paper_colour`: a yellowed print's paper turns white).
/// Both are averaged over the line pixels within `INK_RADIUS` (weighted by
/// depth), and a model takes the pixel when either matches; the fallback
/// (grey / black) only when no other model takes it and both are neutral.
pub fn model_bits(img: &Rgb8, lines: &InkLines, models: &[InkModel]) -> Vec<u8> {
    assert!(models.len() <= 8);
    let (w, h) = (lines.w, lines.h);
    let mut bits = vec![0u8; w * h];
    if w == 0 || h == 0 {
        return bits;
    }
    let paper = paper_colour(img);
    let inv_paper = paper.map(|p| 1.0 / p);
    // Bands of rows; each computes, for its rows plus INK_RADIUS halo rows on
    // each side, running sums along x of the depth-weighted absorption,
    // weight and paper-balanced colour of the line pixels; a window sum is
    // then two lookups per row. The band buffer is reused.
    const BAND: usize = 32;
    let stride = w + 1;
    let init = || vec![[0f32; 7]; (BAND + 2 * INK_RADIUS) * stride];
    bits.par_chunks_mut(w * BAND).enumerate().for_each_init(init, |pre, (bi, out)| {
        let y0 = bi * BAND;
        let y1 = (y0 + BAND).min(h);
        let (h0, h1) = (y0.saturating_sub(INK_RADIUS), (y1 + INK_RADIUS).min(h));
        for y in h0..h1 {
            let row = &mut pre[(y - h0) * stride..(y - h0 + 1) * stride];
            let mut run = [0f32; 7];
            row[0] = run;
            for (x, &d) in lines.depth[y * w..(y + 1) * w].iter().enumerate() {
                if d > 0 {
                    let a = absorption(img, lines, x, y);
                    let p = &img.data[(y * w + x) * 3..(y * w + x) * 3 + 3];
                    let wt = d as f32;
                    for c in 0..3 {
                        run[c] += wt * a[c];
                        run[c + 4] += wt * p[c] as f32 * inv_paper[c];
                    }
                    run[3] += wt;
                }
                row[x + 1] = run;
            }
        }
        for y in y0..y1 {
            let orow = &mut out[(y - y0) * w..(y - y0 + 1) * w];
            let drow = &lines.depth[y * w..(y + 1) * w];
            for (x, b) in orow.iter_mut().enumerate() {
                if drow[x] == 0 {
                    continue;
                }
                let (xa, xb) = (x.saturating_sub(INK_RADIUS), (x + INK_RADIUS + 1).min(w));
                let mut s = [0f32; 7];
                for yy in y.saturating_sub(INK_RADIUS)..=(y + INK_RADIUS).min(h - 1) {
                    let r = &pre[(yy - h0) * stride..(yy - h0 + 1) * stride];
                    let (lo, hi) = (&r[xa], &r[xb]);
                    for c in 0..7 {
                        s[c] += hi[c] - lo[c];
                    }
                }
                let inv = 255.0 / s[3];
                let (mut km, mut kp) = ([0u8; 3], [0u8; 3]);
                for c in 0..3 {
                    km[c] = (255.5 - s[c] * inv).clamp(0.0, 255.0) as u8;
                    kp[c] = (s[c + 4] * inv + 0.5).clamp(0.0, 255.0) as u8;
                }
                let (cm, cp) = (hcv(km), hcv(kp));
                for (i, m) in models.iter().enumerate() {
                    if !m.fallback && (m.matches_hcv(cm) || m.matches_hcv(cp)) {
                        *b |= 1 << i;
                    }
                }
                if *b == 0 {
                    for (i, m) in models.iter().enumerate() {
                        if m.fallback && m.matches_hcv(cm) && m.matches_hcv(cp) {
                            *b |= 1 << i;
                        }
                    }
                }
            }
        }
    });
    bits
}

/// 8-connected components of a binary plane, labelled by runs of set pixels.
pub struct RunComponents {
    /// Runs [x0, x1) per row.
    pub runs: Vec<Vec<(u32, u32)>>,
    /// Index of each row's first run in `label`.
    offsets: Vec<usize>,
    /// Component of every run (row-major run order), and component sizes.
    pub label: Vec<u32>,
    pub sizes: Vec<u32>,
}

impl RunComponents {
    /// Components of the pixels of the `w`-wide plane `plane` where `set(value)`.
    pub fn new<T: Copy + Sync>(plane: &[T], w: usize, set: impl Fn(T) -> bool + Sync) -> RunComponents {
        let h = plane.len().checked_div(w).unwrap_or(0);
        let runs: Vec<Vec<(u32, u32)>> = plane
            .par_chunks(w.max(1))
            .take(h)
            .map(|row| {
                let mut r = Vec::new();
                let mut x = 0;
                while x < w {
                    if set(row[x]) {
                        let s = x;
                        while x < w && set(row[x]) {
                            x += 1;
                        }
                        r.push((s as u32, x as u32));
                    } else {
                        x += 1;
                    }
                }
                r
            })
            .collect();
        RunComponents::from_runs(runs)
    }

    /// Components of the runs (per row, sorted, [x0, x1)).
    pub fn from_runs(runs: Vec<Vec<(u32, u32)>>) -> RunComponents {
        let mut offsets = Vec::with_capacity(runs.len());
        let mut n = 0usize;
        for r in &runs {
            offsets.push(n);
            n += r.len();
        }
        let mut parent: Vec<u32> = (0..n as u32).collect();
        fn find(p: &mut [u32], mut i: u32) -> u32 {
            while p[i as usize] != i {
                p[i as usize] = p[p[i as usize] as usize];
                i = p[i as usize];
            }
            i
        }
        for y in 1..runs.len() {
            let (prev, cur) = (&runs[y - 1], &runs[y]);
            let (po, co) = (offsets[y - 1], offsets[y]);
            let mut j = 0;
            for (i, &(s, e)) in cur.iter().enumerate() {
                // 8-connected: previous-row runs overlapping [s − 1, e + 1).
                while j < prev.len() && prev[j].1 < s {
                    j += 1;
                }
                let mut k = j;
                while k < prev.len() && prev[k].0 <= e {
                    let (a, b) = (find(&mut parent, (co + i) as u32), find(&mut parent, (po + k) as u32));
                    if a != b {
                        parent[a.max(b) as usize] = a.min(b);
                    }
                    k += 1;
                }
            }
        }
        // Compact labels.
        let mut label = vec![0u32; n];
        let mut ids = vec![u32::MAX; n];
        let mut sizes = Vec::new();
        for (y, r) in runs.iter().enumerate() {
            for (i, &(s, e)) in r.iter().enumerate() {
                let idx = offsets[y] + i;
                let root = find(&mut parent, idx as u32) as usize;
                if ids[root] == u32::MAX {
                    ids[root] = sizes.len() as u32;
                    sizes.push(0);
                }
                label[idx] = ids[root];
                sizes[ids[root] as usize] += e - s;
            }
        }
        RunComponents { runs, offsets, label, sizes }
    }

    /// Component of row `y`'s `i`-th run.
    pub fn run_label(&self, y: usize, i: usize) -> u32 {
        self.label[self.offsets[y] + i]
    }
}

/// Sizes of the 8-connected components of the pixels where `bits & bit != 0`.
pub fn component_sizes(bits: &[u8], w: usize, h: usize, bit: u8) -> Vec<u32> {
    if w == 0 || h == 0 {
        return Vec::new();
    }
    RunComponents::new(&bits[..w * h], w, |b| b & bit != 0).sizes
}

/// Component sizes of each of the first `n` bit planes of `bits` (one scan
/// for all planes; zero bytes, most of a map, are skipped 8 at a time).
pub fn plane_component_sizes(bits: &[u8], w: usize, h: usize, n: usize) -> Vec<Vec<u32>> {
    let n = n.min(8);
    if w == 0 || h == 0 || n == 0 {
        return vec![Vec::new(); n];
    }
    // Per row: the runs of every plane.
    let rows: Vec<Vec<Vec<(u32, u32)>>> = bits[..w * h]
        .par_chunks(w)
        .map(|row| {
            let mut runs = vec![Vec::new(); n];
            let mut start = [0u32; 8];
            let mut prev = 0u8;
            let mut x = 0usize;
            while x < w {
                if prev == 0 && x + 8 <= w && u64::from_ne_bytes(row[x..x + 8].try_into().unwrap()) == 0 {
                    x += 8;
                    continue;
                }
                let q = row[x];
                if q != prev {
                    let (started, ended) = (q & !prev, prev & !q);
                    for i in 0..n {
                        if started >> i & 1 != 0 {
                            start[i] = x as u32;
                        }
                        if ended >> i & 1 != 0 {
                            runs[i].push((start[i], x as u32));
                        }
                    }
                    prev = q;
                }
                x += 1;
            }
            for (i, r) in runs.iter_mut().enumerate() {
                if prev >> i & 1 != 0 {
                    r.push((start[i], w as u32));
                }
            }
            runs
        })
        .collect();
    (0..n)
        .into_par_iter()
        .map(|i| RunComponents::from_runs(rows.iter().map(|r| r[i].clone()).collect()).sizes)
        .collect()
}

/// Clear the 8-connected components of `mask` smaller than `min_px` pixels
/// (`contour::remove_small_components`, by runs: fast on large rasters).
pub fn remove_small_runs(mask: &mut Mask, min_px: usize) {
    let w = mask.w;
    if w == 0 || mask.h == 0 {
        return;
    }
    let cc = RunComponents::new(&mask.data, w, |v| v != 0);
    let small: Vec<bool> = cc.sizes.iter().map(|&s| (s as usize) < min_px).collect();
    if !small.iter().any(|&s| s) {
        return;
    }
    mask.data.par_chunks_mut(w).enumerate().for_each(|(y, row)| {
        for (i, &(s, e)) in cc.runs[y].iter().enumerate() {
            if small[cc.run_label(y, i) as usize] {
                row[s as usize..e as usize].fill(0);
            }
        }
    });
}

/// A fallback (grey / black) model is chosen only when the best other
/// model's long lines are under this fraction of its own: black and grey
/// lines are on every map (text, grid, roads, frames), so they are the
/// contours only on maps with next to no coloured lines.
const FALLBACK_MAX_RATIO: f64 = 0.05;
/// A fallback model's lines must also look like a map's contours: mostly
/// long lines (grey contours and photocopies: ≥ 0.89 of their pixels; the
/// black linework of a map without contours, text-heavy: ~0.6)…
const FALLBACK_MIN_LONG_SHARE: f64 = 0.8;
/// …covering a fair part of the map (photocopied and grey contours: 2-40 %
/// of the image; a sheet's roads, grid and frame alone: ~0.5 %).
const FALLBACK_MIN_COVER: f64 = 0.015;

/// Score every model (`ModelScore`) and return the index of the best.
pub fn select_model(bits: &[u8], w: usize, h: usize, models: &[InkModel], min_px: usize) -> (usize, Vec<ModelScore>) {
    let planes = plane_component_sizes(bits, w, h, models.len());
    let scores: Vec<ModelScore> = models
        .iter()
        .zip(&planes)
        .map(|(m, sizes)| {
            let pixels: usize = sizes.iter().map(|&s| s as usize).sum();
            let long_px: usize = sizes.iter().filter(|&&s| s as usize >= min_px).map(|&s| s as usize).sum();
            let short = (pixels - long_px) as f64;
            let score = m.prior * (long_px as f64 - SHORT_PENALTY * short);
            ModelScore { name: m.name, pixels, long_px, score }
        })
        .collect();
    let best_of = |fallback: bool| {
        scores
            .iter()
            .enumerate()
            .filter(|(i, _)| models[*i].fallback == fallback)
            .max_by(|a, b| a.1.score.total_cmp(&b.1.score))
            .map(|(i, _)| i)
    };
    let best = match (best_of(false), best_of(true)) {
        (Some(c), Some(f)) => {
            let fl = scores[f].long_px as f64;
            let take_fallback = scores[f].score > scores[c].score
                && (scores[c].long_px as f64) < FALLBACK_MAX_RATIO * fl
                && fl >= FALLBACK_MIN_LONG_SHARE * scores[f].pixels as f64
                && fl >= FALLBACK_MIN_COVER * (w * h) as f64;
            if take_fallback {
                f
            } else {
                c
            }
        }
        (Some(c), None) => c,
        (None, Some(f)) => f,
        (None, None) => 0,
    };
    (best, scores)
}

/// Mask components smaller than this (level pixels) are specks.
const MIN_COMPONENT: usize = 8;

/// A contour mask at its analysis level, as the session matches it.
pub struct LevelMask {
    /// Contour pixels; one pixel covers `level` × `level` base pixels.
    pub mask: Mask,
    pub level: usize,
    pub model: ContourColorModel,
    /// How the mask was made (diagnostics).
    pub diag: serde_json::Value,
}

/// The contour mask the session matches for this raster (`img`: the base
/// raster, or for photos its normalised copy): photos keep `photo_mask` at
/// full resolution; everything else is `extract`ed at its analysis level.
/// `min_len` is the shortest contour segment in base pixels.
pub fn contour_mask(img: &Rgb8, picked: Option<[u8; 3]>, photo: bool, min_len: usize) -> LevelMask {
    if photo {
        let (mask, model) = crate::contour::photo_mask(img, picked.map(model_from_rgb).as_ref());
        return LevelMask { mask, level: 1, model, diag: serde_json::json!({"method": "photo_valleys", "level": 1}) };
    }
    let ex = extract(img, picked, min_len);
    let diag = serde_json::json!({
        "method": "ink_lines",
        "level": ex.level,
        "thin_share": ex.level_estimate.thin_share.iter().map(|v| (v * 1000.0).round() / 1000.0).collect::<Vec<_>>(),
        "block": ex.level_estimate.block,
        "ink": ex.model_name,
        "line_fraction": ((ex.line_px as f64 / (ex.mask.w * ex.mask.h).max(1) as f64) * 1e5).round() / 1e5,
        "scores": ex.scores,
    });
    LevelMask { mask: ex.mask, level: ex.level, model: ex.model, diag }
}

/// Segment length and sampling step (level pixels) at analysis level `level`
/// for the base-pixel ones of `skeleton::default_sampling`.
pub fn level_sampling(min_len: usize, step: usize, level: usize) -> (usize, usize) {
    let f = level.max(1);
    ((min_len as f64 / f as f64).round().max(10.0) as usize, (step as f64 / f as f64).round().max(1.0) as usize)
}

/// Map samples taken on a level-`level` mask to base-image pixels: level
/// pixel centre (i + 0.5) is base position (i + 0.5)·level, the centre of the
/// block it averages.
pub fn scale_samples(s: &mut crate::skeleton::ContourSamples, level: usize) {
    if level > 1 {
        let f = level as f32;
        for p in &mut s.pts {
            p[0] *= f;
            p[1] *= f;
        }
    }
}

/// Field cell size (mask pixels) at analysis level `level` for an image of
/// `w` × `h` base pixels: about `search::field_px_per_cell` base pixels.
pub fn level_field_cell(w: usize, h: usize, level: usize) -> usize {
    let base = crate::search::field_px_per_cell(w, h);
    ((base as f64 / level.max(1) as f64).round() as usize).max(1)
}

/// The contour mask of an image at its analysis level.
pub struct LineExtraction {
    /// Contour pixels at the analysis level (`level` base pixels per pixel).
    pub mask: Mask,
    pub level: usize,
    pub model: ContourColorModel,
    pub model_name: &'static str,
    pub level_estimate: LevelEstimate,
    pub scores: Vec<ModelScore>,
    /// Line pixels found (all inks), level pixels.
    pub line_px: usize,
}

/// Contour mask of `img` (the base raster): the analysis level, the line
/// pixels there and the ink model (`picked`: the user's contour colour, else
/// the automatic choice). `min_len` is the shortest contour segment in base
/// pixels.
pub fn extract(img: &Rgb8, picked: Option<[u8; 3]>, min_len: usize) -> LineExtraction {
    let est = estimate_level(img);
    let level = est.level;
    let small;
    let src = if level > 1 {
        small = downsample_box(img, level);
        &small
    } else {
        img
    };
    let lines = ink_lines(src);
    let line_px = lines.count();
    let min_px = (min_len / level).max(10);
    let (mask, model, model_name, scores) = match picked {
        Some(rgb) => {
            // The picked colour is a pixel colour (ink over its background):
            // line pixels of that raw colour.
            let m = model_from_rgb(rgb);
            let mut mask = Mask::new(src.w, src.h);
            mask.data.par_chunks_mut(src.w.max(1)).enumerate().for_each(|(y, row)| {
                for (x, o) in row.iter_mut().enumerate() {
                    *o = (lines.depth[y * src.w + x] > 0 && m.matches(src.rgb(x, y))) as u8;
                }
            });
            (mask, m, "picked", Vec::new())
        }
        None => {
            let models = ink_models();
            let bits = model_bits(src, &lines, &models);
            let (best, scores) = select_model(&bits, src.w, src.h, &models, min_px);
            let mut mask = Mask::new(src.w, src.h);
            let bit = 1u8 << best;
            mask.data.par_iter_mut().zip(bits.par_iter()).for_each(|(o, &b)| *o = (b & bit != 0) as u8);
            (mask, models[best].color_model(), models[best].name, scores)
        }
    };
    let mut mask = mask;
    remove_small_runs(&mut mask, MIN_COMPONENT);
    LineExtraction { mask, level, model, model_name, level_estimate: est, scores, line_px }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// White paper with wavy 1-px lines of `line` colour, every 14 px.
    fn paper(w: usize, h: usize, line: [u8; 3], bg: impl Fn(usize, usize) -> [u8; 3]) -> Rgb8 {
        let mut data = vec![0u8; w * h * 3];
        for y in 0..h {
            for x in 0..w {
                data[(y * w + x) * 3..(y * w + x) * 3 + 3].copy_from_slice(&bg(x, y));
            }
        }
        for k in 0..h / 14 {
            for x in 0..w {
                let y = (7.0 + k as f64 * 14.0 + 4.0 * (x as f64 / 30.0 + k as f64).sin()).round() as usize;
                if y < h {
                    let i = (y * w + x) * 3;
                    let b = [data[i], data[i + 1], data[i + 2]];
                    // Ink multiplies the background (as printed).
                    let v = [0, 1, 2].map(|c| (b[c] as u32 * line[c] as u32 / 255) as u8);
                    data[i..i + 3].copy_from_slice(&v);
                }
            }
        }
        Rgb8::from_rgb(w, h, data).unwrap()
    }

    /// Smooth (bilinear) upsampling by an integer factor.
    fn upsample(img: &Rgb8, f: usize) -> Rgb8 {
        let (w, h) = (img.w * f, img.h * f);
        let mut data = vec![0u8; w * h * 3];
        for y in 0..h {
            let sy = ((y as f64 + 0.5) / f as f64 - 0.5).clamp(0.0, (img.h - 1) as f64);
            let (y0, fy) = (sy.floor() as usize, sy.fract());
            let y1 = (y0 + 1).min(img.h - 1);
            for x in 0..w {
                let sx = ((x as f64 + 0.5) / f as f64 - 0.5).clamp(0.0, (img.w - 1) as f64);
                let (x0, fx) = (sx.floor() as usize, sx.fract());
                let x1 = (x0 + 1).min(img.w - 1);
                for c in 0..3 {
                    let g = |xx: usize, yy: usize| img.data[(yy * img.w + xx) * 3 + c] as f64;
                    let v = (g(x0, y0) * (1.0 - fx) + g(x1, y0) * fx) * (1.0 - fy)
                        + (g(x0, y1) * (1.0 - fx) + g(x1, y1) * fx) * fy;
                    data[(y * w + x) * 3 + c] = v.round() as u8;
                }
            }
        }
        Rgb8::from_rgb(w, h, data).unwrap()
    }

    const TAN: [u8; 3] = [200, 170, 130];

    #[test]
    fn downsample_box_averages_blocks() {
        let img = Rgb8::from_rgb(
            4,
            2,
            vec![0, 0, 0, 30, 30, 30, 60, 60, 60, 90, 90, 90, 0, 0, 0, 30, 30, 30, 60, 60, 60, 90, 90, 90],
        )
        .unwrap();
        let d = downsample_box(&img, 2);
        assert_eq!((d.w, d.h), (2, 1));
        assert_eq!(d.data, vec![15, 15, 15, 75, 75, 75]);
    }

    #[test]
    fn level_follows_upsampling() {
        let native = paper(900, 700, TAN, |_, _| [255, 255, 255]);
        let e = estimate_level(&native);
        assert_eq!((e.level, e.block), (1, None), "{e:?}");
        // Smooth 3× upsampling: lines of 1.5 px or less at level 2, 1 px at 3.
        let e = estimate_level(&upsample(&native, 3));
        assert!((2..=3).contains(&e.level) && e.block.is_none(), "{e:?}");
        let e = estimate_level(&upsample(&native, 2));
        assert_eq!(e.level, 2, "{e:?}");
        // Pixel repetition: the block size.
        let e = estimate_level(&upsample_nearest(&native, 3));
        assert_eq!((e.level, e.block), (3, Some(3)), "{e:?}");
        let e = estimate_level(&upsample_nearest(&native, 2));
        assert_eq!((e.level, e.block), (2, Some(2)), "{e:?}");
    }

    /// Nearest-neighbour (pixel repetition) upsampling by an integer factor.
    fn upsample_nearest(img: &Rgb8, f: usize) -> Rgb8 {
        let (w, h) = (img.w * f, img.h * f);
        let mut data = vec![0u8; w * h * 3];
        for y in 0..h {
            for x in 0..w {
                data[(y * w + x) * 3..(y * w + x) * 3 + 3].copy_from_slice(&img.rgb(x / f, y / f));
            }
        }
        Rgb8::from_rgb(w, h, data).unwrap()
    }

    /// Wavy line k of `paper` at native column x (row of its centre).
    fn line_y(k: usize, x: f64) -> f64 {
        (7.0 + k as f64 * 14.0 + 4.0 * (x / 30.0 + k as f64).sin()).round()
    }

    #[test]
    fn samples_map_back_to_base_pixels() {
        // A 3× smooth upsample analysed at its level: every sample, mapped
        // back to base pixels, lies on a line of the upsampled image (the
        // native line y covers base rows [3y, 3y + 3), centre 3y + 1.5).
        let native = paper(500, 400, TAN, |_, _| [255, 255, 255]);
        let img = upsample(&native, 3);
        let (min_len, step) = crate::skeleton::default_sampling(img.w, img.h);
        let lm = contour_mask(&img, None, false, min_len);
        assert!(lm.level >= 2, "{}", lm.diag);
        let (ml, st) = level_sampling(min_len, step, lm.level);
        let mut s = crate::skeleton::contour_samples(&lm.mask, ml, st);
        scale_samples(&mut s, lm.level);
        assert!(s.len() > 1000, "{} samples", s.len());
        let mut worst = 0f64;
        for p in &s.pts {
            let xn = p[0] as f64 / 3.0;
            let off = (0..native.h / 14)
                .map(|k| (p[1] as f64 - (3.0 * line_y(k, xn.floor()) + 1.5)).abs())
                .fold(f64::INFINITY, f64::min);
            worst = worst.max(off);
        }
        // Within a native pixel's height plus the line's slope over one sample.
        assert!(worst <= 3.0, "sample {worst:.2} base px off its line");
    }

    #[test]
    fn warm_lines_over_hillshade_and_tint() {
        // Tan lines over a shaded, green-tinted background: the ink divides out.
        let img = paper(600, 420, TAN, |x, y| {
            let shade = 0.7 + 0.3 * ((x as f64 / 90.0).sin() * (y as f64 / 70.0).cos()).abs();
            let base = if x > 300 { [215.0, 235.0, 200.0] } else { [245.0, 243.0, 238.0] };
            base.map(|v: f64| (v * shade).round() as u8)
        });
        let ex = extract(&img, None, 60);
        assert_eq!(ex.level, 1);
        assert!(ex.model_name.starts_with("warm"), "{} {:?}", ex.model_name, ex.scores);
        // Every line is found along most of its length, nothing else.
        let mut on = 0;
        let mut total = 0;
        for k in 0..30 {
            for x in 5..595 {
                let y = (7.0 + k as f64 * 14.0 + 4.0 * (x as f64 / 30.0 + k as f64).sin()).round() as usize;
                if y < 420 {
                    total += 1;
                    on += ex.mask.get(x, y) as usize;
                }
            }
        }
        assert!(on as f64 > 0.95 * total as f64, "{on} of {total}");
        assert!(ex.mask.count() < total + total / 10, "{} mask pixels", ex.mask.count());
    }

    #[test]
    fn grey_lines_choose_the_neutral_ink() {
        let img = paper(600, 420, [110, 110, 110], |_, _| [250, 250, 250]);
        let ex = extract(&img, None, 60);
        assert_eq!(ex.model_name, "neutral", "{:?}", ex.scores);
        assert!(ex.mask.count() > 30 * 500);
    }

    #[test]
    fn black_linework_without_contours_is_no_contours() {
        // A grid, a few roads and lots of text in black on white: nothing to
        // match against terrain, so no mask (not the linework as contours).
        let (w, h) = (900usize, 700usize);
        let mut data = vec![255u8; w * h * 3];
        let mut ink = |x: usize, y: usize| {
            if x < w && y < h {
                data[(y * w + x) * 3..(y * w + x) * 3 + 3].copy_from_slice(&[20, 20, 20]);
            }
        };
        for i in 0..w.max(h) {
            for g in [150, 450, 750] {
                ink(g, i);
                ink(i, g.min(h - 1));
            }
            // Two curvy roads.
            ink(i, (300.0 + 60.0 * (i as f64 / 90.0).sin()) as usize);
            ink((500.0 + 40.0 * (i as f64 / 70.0).cos()) as usize, i);
        }
        // Text: 3 × 5 px glyph blocks every 12 px in rows.
        for row in (20..h).step_by(40) {
            for col in (10..w - 10).step_by(12) {
                for (dx, dy) in [(0, 0), (1, 0), (2, 0), (0, 2), (1, 2), (2, 2), (0, 4), (1, 4), (2, 4), (0, 1), (2, 3)]
                {
                    ink(col + dx, row + dy);
                }
            }
        }
        let img = Rgb8::from_rgb(w, h, data).unwrap();
        let ex = extract(&img, None, 60);
        assert_ne!(ex.model_name, "neutral", "{:?}", ex.scores);
        assert!(ex.mask.count() < 200, "{} mask pixels", ex.mask.count());
    }

    #[test]
    fn components_are_8_connected() {
        let (w, h) = (6, 4);
        let mut bits = vec![0u8; w * h];
        for &(x, y) in &[(0, 0), (1, 1), (2, 2), (3, 2), (5, 0), (5, 3)] {
            bits[y * w + x] = 1;
        }
        let mut s = component_sizes(&bits, w, h, 1);
        s.sort();
        assert_eq!(s, vec![1, 1, 4]);
    }

    #[test]
    fn plane_components_match_single_planes() {
        // Pseudo-random 3-plane pattern with long zero stretches.
        let (w, h) = (97, 61);
        let mut state = 12345u32;
        let bits: Vec<u8> = (0..w * h)
            .map(|i| {
                state = state.wrapping_mul(1103515245).wrapping_add(12345);
                if (i / 13) % 3 == 0 {
                    0
                } else {
                    ((state >> 16) & 7) as u8
                }
            })
            .collect();
        let planes = plane_component_sizes(&bits, w, h, 3);
        for (i, p) in planes.iter().enumerate() {
            let mut a = p.clone();
            let mut b = component_sizes(&bits, w, h, 1 << i);
            a.sort();
            b.sort();
            assert_eq!(a, b, "plane {i}");
        }
        // Small runs removal keeps exactly the large components.
        let mut m = Mask { w, h, data: bits.iter().map(|&b| b & 1).collect() };
        let before = component_sizes(&m.data, w, h, 1);
        remove_small_runs(&mut m, 5);
        let mut after = component_sizes(&m.data, w, h, 1);
        let mut kept: Vec<u32> = before.into_iter().filter(|&s| s >= 5).collect();
        after.sort();
        kept.sort();
        assert_eq!(after, kept);
    }
}
