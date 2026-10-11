//! Contour-line pixel classification.
//!
//! Contours on topographic maps are drawn in brown / tan / orange: muted tan
//! on many club and hiking maps (Grootkloof ~ (168,152,136)), mid brown on
//! USGS quads (~ (186,140,100)) and saturated orange-brown on Swiss / OS maps
//! (~ (210,140,70)). A pixel is a contour pixel when its HSV colour lies in
//! the model's box. The default box covers all of these while rejecting black
//! text, grey roads (low saturation), green vegetation, blue water, yellow
//! roads (hue > 45°) and the white / cream paper.

use crate::image::{rgb_to_hsv, GrayF32, Mask, Rgb8};
use rayon::prelude::*;
use serde::{Deserialize, Serialize};

/// Colour model deciding which pixels belong to contour lines.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ContourColorModel {
    /// Hue range in degrees (may wrap if min > max).
    pub hue_min: f32,
    pub hue_max: f32,
    pub sat_min: f32,
    pub sat_max: f32,
    pub val_min: f32,
    pub val_max: f32,
}

impl ContourColorModel {
    /// True if the colour belongs to this model.
    #[inline]
    pub fn matches(&self, rgb: [u8; 3]) -> bool {
        let (h, s, v) = rgb_to_hsv(rgb[0], rgb[1], rgb[2]);
        if s < self.sat_min || s > self.sat_max || v < self.val_min || v > self.val_max {
            return false;
        }
        if self.hue_min <= self.hue_max {
            h >= self.hue_min && h <= self.hue_max
        } else {
            h >= self.hue_min || h <= self.hue_max
        }
    }
}

/// Default model: brown / tan / orange contours (most topographic maps).
pub fn default_model() -> ContourColorModel {
    ContourColorModel { hue_min: 10.0, hue_max: 45.0, sat_min: 0.08, sat_max: 0.75, val_min: 0.35, val_max: 0.92 }
}

/// Muted tan / brown only (rejects saturated orange fills and roads).
pub fn muted_model() -> ContourColorModel {
    ContourColorModel { hue_min: 10.0, hue_max: 45.0, sat_min: 0.08, sat_max: 0.55, val_min: 0.35, val_max: 0.92 }
}

/// Saturated orange-brown only (rejects pale beige / sand area tints).
pub fn saturated_model() -> ContourColorModel {
    ContourColorModel { hue_min: 12.0, hue_max: 45.0, sat_min: 0.25, sat_max: 0.92, val_min: 0.40, val_max: 0.97 }
}

/// The models `auto_mask` chooses from, most general first.
pub fn candidate_models() -> Vec<ContourColorModel> {
    vec![default_model(), muted_model(), saturated_model()]
}

/// Model centred on a user-picked contour colour.
///
/// The box is tolerant: anti-aliased line pixels are lighter and less
/// saturated than the line core, and hue is noisy at low saturation. A grey
/// or black pick (saturation < 0.06) gives a hue-free model around its value.
pub fn model_from_rgb(rgb: [u8; 3]) -> ContourColorModel {
    let (h, s, v) = rgb_to_hsv(rgb[0], rgb[1], rgb[2]);
    let val_min = (v - 0.25).max(0.0);
    let val_max = (v + 0.2).min(0.97).max(v);
    if s < 0.06 {
        return ContourColorModel {
            hue_min: 0.0,
            hue_max: 360.0,
            sat_min: 0.0,
            sat_max: (s + 0.08).min(1.0),
            val_min: (v - 0.15).max(0.0),
            val_max: (v + 0.12).min(0.97).max(v),
        };
    }
    // Wider hue tolerance for muted colours, whose hue is poorly defined.
    let tol = 14.0 + 22.0 * ((0.3 - s) / 0.3).clamp(0.0, 1.0);
    let wrap = |x: f32| x.rem_euclid(360.0);
    ContourColorModel {
        hue_min: wrap(h - tol),
        hue_max: wrap(h + tol),
        sat_min: (s * 0.45).max(0.04),
        sat_max: (s * 1.6 + 0.1).min(1.0),
        val_min,
        val_max,
    }
}

// ---------------------------------------------------------------------------
// Photos of paper maps: illumination and white balance
// ---------------------------------------------------------------------------

/// Paper-background blocks per image side (longer side).
const PAPER_BLOCKS: f64 = 48.0;
/// Percentile of each block's channel values taken as its paper colour.
const PAPER_PERCENTILE: f64 = 0.9;
/// Closing radius (blocks): blocks covered by ink or tints borrow the paper
/// colour of their neighbours (a closing, unlike a plain max filter, leaves
/// smooth lighting gradients unbiased).
const PAPER_CLOSING_RADIUS: usize = 1;
/// Gaussian smoothing of the paper estimate (blocks).
const PAPER_SIGMA: f64 = 1.0;
/// Lowest paper level divided by (keeps black borders / table from exploding).
const PAPER_FLOOR: f32 = 16.0;

/// Contrast exponent of `normalize_illumination`: a camera blurs thin lines
/// into the paper, washing out their colour; `v^1.5` restores the saturation
/// of these light mixtures (it grows by about half near white) while paper
/// stays white.
pub const PHOTO_GAMMA: f32 = 1.5;

/// Flatten the lighting and white balance of a photographed paper map.
///
/// The paper colour is estimated per channel on a coarse grid of blocks
/// (each block's 90th percentile: most of a map is paper), closed with its
/// neighbouring blocks (so a block of dense ink or a small area tint does
/// not count as paper), padded by linear extrapolation, smoothed and
/// upsampled. Dividing by it makes the paper white everywhere, removing
/// shading, vignetting and colour casts; a contrast curve (`PHOTO_GAMMA`)
/// then restores the colour of lines washed out by the camera blur, so the
/// default contour colour model and the grid-line detector work as on a
/// scan. Blocks of another colour than the paper that are connected to the
/// image border (the table around a page flattened by corners slightly
/// outside it), and the ring of blocks holding the page edge, are blanked
/// to white.
pub fn normalize_illumination(img: &Rgb8) -> Rgb8 {
    normalize_illumination_gamma(img, PHOTO_GAMMA)
}

/// `normalize_illumination` with another contrast exponent (1 = plain division).
pub fn normalize_illumination_gamma(img: &Rgb8, gamma: f32) -> Rgb8 {
    let (w, h) = (img.w, img.h);
    if w == 0 || h == 0 || img.data.len() != w * h * 3 {
        return img.clone();
    }
    let paper = paper_background(img);
    let gamma = if gamma.is_finite() { gamma.clamp(0.2, 5.0) } else { 1.0 };
    let lut: Vec<u8> =
        (0..=1024).map(|i| (255.0 * (i as f32 / 1024.0).powf(gamma)).round().clamp(0.0, 255.0) as u8).collect();
    // Horizontal interpolation weights (block-centre aligned), shared by all rows.
    let (blk, pad) = (paper.block as f32, paper.pad as f32);
    let xw: Vec<(usize, usize, f32)> = (0..w)
        .map(|x| {
            let bx = ((x as f32 + 0.5) / blk - 0.5 + pad).clamp(0.0, (paper.nx - 1) as f32);
            let x0 = (bx.floor() as usize).min(paper.nx - 1);
            (x0, (x0 + 1).min(paper.nx - 1), bx - x0 as f32)
        })
        .collect();
    let nx0 = paper.nx - 2 * paper.pad;
    let mut data = vec![0u8; w * h * 3];
    data.par_chunks_mut(w * 3).enumerate().for_each(|(y, row)| {
        let by = ((y as f32 + 0.5) / blk - 0.5 + pad).clamp(0.0, (paper.ny - 1) as f32);
        let y0 = (by.floor() as usize).min(paper.ny - 1);
        let (y1, fy) = ((y0 + 1).min(paper.ny - 1), by - y0 as f32);
        // Paper level along this row at the block columns, per channel.
        let line: [Vec<f32>; 3] = [0usize, 1, 2].map(|c| {
            let p = &paper.planes[c];
            (0..paper.nx).map(|bx| p.get(bx, y0) * (1.0 - fy) + p.get(bx, y1) * fy).collect()
        });
        let off = &paper.off_page[(y / paper.block) * nx0..(y / paper.block + 1) * nx0];
        let src = &img.data[y * w * 3..(y + 1) * w * 3];
        for (x, ((o, p), &(x0, x1, fx))) in row.chunks_exact_mut(3).zip(src.chunks_exact(3)).zip(&xw).enumerate() {
            if off[x / paper.block] {
                o.copy_from_slice(&[255; 3]);
                continue;
            }
            for c in 0..3 {
                let bg = (line[c][x0] * (1.0 - fx) + line[c][x1] * fx).max(PAPER_FLOOR);
                o[c] = lut[((p[c] as f32 / bg).min(1.0) * 1024.0).round() as usize];
            }
        }
    });
    Rgb8 { w, h, data }
}

/// Smoothed per-channel paper colour on a grid of `block`-pixel blocks,
/// padded by `pad` linearly extrapolated blocks on every side (so the
/// smoothing and the interpolation near the image border follow the
/// lighting gradient instead of flattening it).
struct PaperGrid {
    block: usize,
    pad: usize,
    /// Padded grid size.
    nx: usize,
    ny: usize,
    planes: [GrayF32; 3],
    /// Unpadded blocks (row-major, `nx − 2·pad` wide) that lie outside the
    /// page: not paper and connected to the image border (the table a
    /// flattening with corners slightly outside the page leaves around it),
    /// plus the ring of blocks around them.
    off_page: Vec<bool>,
}

/// Blocks of linear extrapolation around the paper grid.
const PAPER_PAD: usize = 2;
/// A block is not paper when its colour's chromaticity differs from the
/// paper's by more than this (L1 over the r, g, b fractions; a wooden table
/// ≈ 0.2, shading 0, light map tints < 0.1). Brightness is not used: deep
/// shade on the page must not count as table, and a grey table is harmless.
const OFF_PAGE_CHROMA: f32 = 0.12;
/// The paper's chromaticity is the median over this brightest fraction of blocks.
const PAPER_REFERENCE_FRACTION: f64 = 0.3;

fn paper_background(img: &Rgb8) -> PaperGrid {
    let (w, h) = (img.w, img.h);
    let block = ((w.max(h) as f64 / PAPER_BLOCKS).round() as usize).clamp(8, 512);
    let (nx, ny) = (w.div_ceil(block), h.div_ceil(block));
    // Per block row: per block, per channel 256-bin histograms → percentile.
    let rows: Vec<Vec<[f32; 3]>> = (0..ny)
        .into_par_iter()
        .map(|by| {
            let mut hist = vec![[0u32; 256]; nx * 3];
            for y in by * block..((by + 1) * block).min(h) {
                let src = &img.data[y * w * 3..(y + 1) * w * 3];
                for (x, p) in src.chunks_exact(3).enumerate() {
                    let b = x / block;
                    for c in 0..3 {
                        hist[b * 3 + c][p[c] as usize] += 1;
                    }
                }
            }
            (0..nx)
                .map(|bx| {
                    let mut out = [0.0f32; 3];
                    for (c, o) in out.iter_mut().enumerate() {
                        let hc = &hist[bx * 3 + c];
                        let n: u32 = hc.iter().sum();
                        let target = (PAPER_PERCENTILE * n as f64).ceil() as u32;
                        let mut acc = 0u32;
                        for (v, &k) in hc.iter().enumerate() {
                            acc += k;
                            if acc >= target.max(1) {
                                *o = v as f32;
                                break;
                            }
                        }
                    }
                    out
                })
                .collect()
        })
        .collect();
    let blocks: Vec<[f32; 3]> = rows.into_iter().flatten().collect();
    let off_page = off_page_blocks(&blocks, nx, ny);
    let planes = [0usize, 1, 2].map(|c| {
        let raw = GrayF32 { w: nx, h: ny, data: blocks.iter().map(|v| v[c]).collect() };
        let closed = rank_filter(&rank_filter(&raw, PAPER_CLOSING_RADIUS, f32::max), PAPER_CLOSING_RADIUS, f32::min);
        pad_linear(&closed, PAPER_PAD).gaussian_blur(PAPER_SIGMA)
    });
    PaperGrid { block, pad: PAPER_PAD, nx: nx + 2 * PAPER_PAD, ny: ny + 2 * PAPER_PAD, planes, off_page }
}

/// Blocks outside the page (see `PaperGrid::off_page`). The paper colour is
/// the median chromaticity of the brightest blocks (paper is the brightest
/// thing on a map; tints and most tables are darker).
fn off_page_blocks(blocks: &[[f32; 3]], nx: usize, ny: usize) -> Vec<bool> {
    let lum = |p: &[f32; 3]| p[0] + p[1] + p[2];
    let chroma = |p: &[f32; 3]| {
        let l = lum(p).max(1.0);
        [p[0] / l, p[1] / l, p[2] / l]
    };
    let mut by_lum: Vec<&[f32; 3]> = blocks.iter().collect();
    by_lum.sort_by(|a, b| lum(b).total_cmp(&lum(a)));
    by_lum.truncate(((blocks.len() as f64 * PAPER_REFERENCE_FRACTION).ceil() as usize).max(1));
    let median = |mut v: Vec<f32>| -> f32 {
        v.sort_by(|a, b| a.total_cmp(b));
        v.get(v.len() / 2).copied().unwrap_or(0.0)
    };
    let c0 = [0, 1, 2].map(|k| median(by_lum.iter().map(|p| chroma(p)[k]).collect()));
    let not_paper: Vec<bool> = blocks
        .iter()
        .map(|p| {
            let c = chroma(p);
            (0..3).map(|k| (c[k] - c0[k]).abs()).sum::<f32>() > OFF_PAGE_CHROMA
        })
        .collect();
    // Flood from the border through blocks that are not paper (4-connected).
    let mut off = vec![false; nx * ny];
    let mut stack: Vec<usize> = (0..nx * ny)
        .filter(|&i| {
            let (x, y) = (i % nx, i / nx);
            (x == 0 || y == 0 || x + 1 == nx || y + 1 == ny) && not_paper[i]
        })
        .collect();
    for &i in &stack {
        off[i] = true;
    }
    while let Some(i) = stack.pop() {
        let (x, y) = (i % nx, i / nx);
        let nb = [(x > 0, i.wrapping_sub(1)), (x + 1 < nx, i + 1), (y > 0, i.wrapping_sub(nx)), (y + 1 < ny, i + nx)];
        for (ok, j) in nb {
            if ok && not_paper[j] && !off[j] {
                off[j] = true;
                stack.push(j);
            }
        }
    }
    // Plus the ring of blocks around it, which holds the page edge: the
    // sliver of table left there would read as a long, straight line.
    (0..nx * ny)
        .map(|i| {
            let (x, y) = (i % nx, i / nx);
            (y.saturating_sub(1)..=(y + 1).min(ny - 1))
                .any(|yy| (x.saturating_sub(1)..=(x + 1).min(nx - 1)).any(|xx| off[yy * nx + xx]))
        })
        .collect()
}

/// Grow `g` by `p` cells on every side, continuing the slope of the two
/// outermost cells (values kept ≥ `PAPER_FLOOR`).
fn pad_linear(g: &GrayF32, p: usize) -> GrayF32 {
    let extend = |v: &[f32]| -> Vec<f32> {
        let n = v.len();
        let (sl, sr) = if n >= 2 { (v[0] - v[1], v[n - 1] - v[n - 2]) } else { (0.0, 0.0) };
        let left = (1..=p).rev().map(|i| (v[0] + sl * i as f32).max(PAPER_FLOOR));
        let right = (1..=p).map(|i| (v[n - 1] + sr * i as f32).max(PAPER_FLOOR));
        left.chain(v.iter().copied()).chain(right).collect()
    };
    let (w2, h2) = (g.w + 2 * p, g.h + 2 * p);
    let rows: Vec<Vec<f32>> = (0..g.h).map(|y| extend(&g.data[y * g.w..(y + 1) * g.w])).collect();
    let mut out = GrayF32::new(w2, h2);
    for x in 0..w2 {
        let col: Vec<f32> = rows.iter().map(|r| r[x]).collect();
        for (y, v) in extend(&col).into_iter().enumerate() {
            out.set(x, y, v);
        }
    }
    out
}

/// The model widened for faint, blurred line pixels (hysteresis "weak" class).
pub fn relaxed_model(m: &ContourColorModel) -> ContourColorModel {
    let wraps = m.hue_min > m.hue_max;
    let full = !wraps && m.hue_min <= 0.0 && m.hue_max >= 360.0;
    let (hue_min, hue_max) = if full {
        (m.hue_min, m.hue_max)
    } else {
        ((m.hue_min - 6.0).rem_euclid(360.0), (m.hue_max + 6.0).rem_euclid(360.0))
    };
    ContourColorModel {
        hue_min,
        hue_max,
        sat_min: m.sat_min * 0.5,
        sat_max: (m.sat_max + 0.1).min(1.0),
        val_min: (m.val_min - 0.05).max(0.0),
        val_max: (m.val_max + 0.04).min(0.96).max(m.val_max),
    }
}

/// Regions of a photo mask that fit a square of this radius are not lines.
const PHOTO_THICK_RADIUS: usize = 3;

/// Contour mask of a (normalised) photo: pixels of `model` (or of the
/// automatic choice) plus, by hysteresis, the fainter pixels of the relaxed
/// model connected to them (camera blur and noise break thin lines into
/// dashes otherwise), cleaned with a majority vote. Regions thicker than a
/// line (a (2·3+1)-pixel square fits) are removed with a margin: fills,
/// the page edge, and dense contours the camera blurred into one blob,
/// whose skeleton would not follow any contour.
pub fn photo_mask(img: &Rgb8, model: Option<&ContourColorModel>) -> (Mask, ContourColorModel) {
    let (strong, model) = match model {
        Some(m) => (extract_mask(img, m), m.clone()),
        None => auto_mask(img),
    };
    let weak = extract_mask(img, &relaxed_model(&model));
    let mut mask = majority_filter(&grow_into(&strong, &weak));
    let thick = morph(&morph(&mask, PHOTO_THICK_RADIUS, false), PHOTO_THICK_RADIUS + 1, true);
    mask.data.par_iter_mut().zip(thick.data.par_iter()).for_each(|(m, &t)| *m &= 1 - t);
    (mask, model)
}

/// Square dilation (`grow`) or erosion of radius `r` (outside the image
/// counts as unset).
fn morph(mask: &Mask, r: usize, grow: bool) -> Mask {
    let (w, h) = (mask.w, mask.h);
    let mut out = Mask::new(w, h);
    if w == 0 || h == 0 {
        return out;
    }
    // A window decides by its count of set pixels (a full window has 2r + 1).
    let decide = |count: usize| -> u8 {
        if grow {
            (count > 0) as u8
        } else {
            (count == 2 * r + 1) as u8
        }
    };
    let mut horiz = vec![0u8; w * h];
    horiz.par_chunks_mut(w).enumerate().for_each(|(y, row)| {
        let src = &mask.data[y * w..(y + 1) * w];
        for (x, o) in row.iter_mut().enumerate() {
            let (x0, x1) = (x.saturating_sub(r), (x + r).min(w - 1));
            *o = decide(src[x0..=x1].iter().filter(|&&v| v != 0).count());
        }
    });
    out.data.par_chunks_mut(w).enumerate().for_each(|(y, row)| {
        let (y0, y1) = (y.saturating_sub(r), (y + r).min(h - 1));
        for (x, o) in row.iter_mut().enumerate() {
            *o = decide((y0..=y1).filter(|&yy| horiz[yy * w + x] != 0).count());
        }
    });
    out
}

/// `seed` plus every pixel of `allowed` 8-connected to it through `allowed`.
pub fn grow_into(seed: &Mask, allowed: &Mask) -> Mask {
    let (w, h) = (seed.w, seed.h);
    let mut out = seed.clone();
    if w == 0 || h == 0 || allowed.w != w || allowed.h != h {
        return out;
    }
    let mut stack: Vec<usize> = (0..w * h).filter(|&i| seed.data[i] != 0).collect();
    while let Some(i) = stack.pop() {
        let (x, y) = (i % w, i / w);
        for yy in y.saturating_sub(1)..=(y + 1).min(h - 1) {
            for xx in x.saturating_sub(1)..=(x + 1).min(w - 1) {
                let j = yy * w + xx;
                if out.data[j] == 0 && allowed.data[j] != 0 {
                    out.data[j] = 1;
                    stack.push(j);
                }
            }
        }
    }
    out
}

/// 3×3 majority vote (a pixel is set when at least 5 of its 3×3
/// neighbourhood are; outside the image counts as unset). Removes the
/// speckle and ragged edges sensor noise leaves on colour masks of photos,
/// which would otherwise break the skeleton into short spurs.
pub fn majority_filter(mask: &Mask) -> Mask {
    let (w, h) = (mask.w, mask.h);
    let mut out = Mask::new(w, h);
    if w == 0 || h == 0 {
        return out;
    }
    out.data.par_chunks_mut(w).enumerate().for_each(|(y, row)| {
        let rows: Vec<&[u8]> =
            (y.saturating_sub(1)..=(y + 1).min(h - 1)).map(|yy| &mask.data[yy * w..(yy + 1) * w]).collect();
        // Column sums of the (up to) three rows, then a sliding window of three.
        let col: Vec<u8> = (0..w).map(|x| rows.iter().map(|r| r[x]).sum()).collect();
        for (x, o) in row.iter_mut().enumerate() {
            let s: u8 = col[x.saturating_sub(1)..=(x + 1).min(w - 1)].iter().sum();
            *o = (s >= 5) as u8;
        }
    });
    out
}

/// Square max (`f32::max`) or min (`f32::min`) filter of radius `r`
/// (windows clamped at the borders); for small block grids.
fn rank_filter(g: &GrayF32, r: usize, pick: fn(f32, f32) -> f32) -> GrayF32 {
    let mut out = GrayF32::new(g.w, g.h);
    for y in 0..g.h {
        for x in 0..g.w {
            let mut m = g.get(x, y);
            for yy in y.saturating_sub(r)..=(y + r).min(g.h - 1) {
                for xx in x.saturating_sub(r)..=(x + r).min(g.w - 1) {
                    m = pick(m, g.get(xx, yy));
                }
            }
            out.set(x, y, m);
        }
    }
    out
}

/// Classify every pixel; 1 = contour.
pub fn extract_mask(img: &Rgb8, model: &ContourColorModel) -> Mask {
    let mut m = Mask::new(img.w, img.h);
    if img.w == 0 {
        return m;
    }
    m.data.par_chunks_mut(img.w).enumerate().for_each(|(y, row)| {
        let src = &img.data[y * img.w * 3..(y + 1) * img.w * 3];
        for (o, p) in row.iter_mut().zip(src.chunks_exact(3)) {
            *o = model.matches([p[0], p[1], p[2]]) as u8;
        }
    });
    m
}

/// How contour-like each candidate's mask is (higher is better). Exposed for
/// diagnostics; `auto_mask` keeps the best.
#[derive(Debug, Clone)]
pub struct MaskStats {
    pub fraction: f64,
    /// Fraction of mask pixels that survive an erosion (area fills, not lines).
    pub solid_fraction: f64,
    pub score: f64,
}

/// Try a few built-in models and keep the one whose mask looks most like
/// contour lines. Returns the mask and the model used.
///
/// Contour lines are thin, so a good mask has many pixels that vanish under
/// an erosion (radius ~ line half-width) and few that survive it (area tints,
/// fills). Score = thin pixels − 3 × solid pixels, discounted when the
/// coverage is implausible (< 0.2 % or > 40 % of the image).
pub fn auto_mask(img: &Rgb8) -> (Mask, ContourColorModel) {
    let models = candidate_models();
    let (bits, stats) = model_bits(img, &models);
    let best = stats
        .iter()
        .enumerate()
        .fold((0usize, f64::NEG_INFINITY), |acc, (i, s)| if s.score > acc.1 { (i, s.score) } else { acc })
        .0;
    let mut m = Mask::new(img.w, img.h);
    let bit = 1u8 << best;
    m.data.par_iter_mut().zip(bits.par_iter()).for_each(|(o, &b)| *o = (b & bit != 0) as u8);
    (m, models[best].clone())
}

/// Per-pixel bit set of matching models (bit i = models[i]) plus their stats.
pub fn model_bits(img: &Rgb8, models: &[ContourColorModel]) -> (Vec<u8>, Vec<MaskStats>) {
    assert!(models.len() <= 8);
    let (w, h) = (img.w, img.h);
    let mut bits = vec![0u8; w * h];
    if w == 0 || h == 0 {
        return (bits, Vec::new());
    }
    bits.par_chunks_mut(w).enumerate().for_each(|(y, row)| {
        let src = &img.data[y * w * 3..(y + 1) * w * 3];
        for (o, p) in row.iter_mut().zip(src.chunks_exact(3)) {
            let (hh, s, v) = rgb_to_hsv(p[0], p[1], p[2]);
            let mut b = 0u8;
            for (i, m) in models.iter().enumerate() {
                let hue_ok = if m.hue_min <= m.hue_max {
                    hh >= m.hue_min && hh <= m.hue_max
                } else {
                    hh >= m.hue_min || hh <= m.hue_max
                };
                if hue_ok && s >= m.sat_min && s <= m.sat_max && v >= m.val_min && v <= m.val_max {
                    b |= 1 << i;
                }
            }
            *o = b;
        }
    });
    let r = erosion_radius(w, h);
    let eroded = erode_bits(&bits, w, h, r);
    let count = |plane: &[u8]| -> [usize; 8] {
        plane
            .par_chunks(w)
            .map(|row| {
                let mut c = [0usize; 8];
                for &b in row {
                    if b != 0 {
                        for (i, ci) in c.iter_mut().enumerate() {
                            *ci += ((b >> i) & 1) as usize;
                        }
                    }
                }
                c
            })
            .reduce(
                || [0usize; 8],
                |mut a, b| {
                    for (x, y) in a.iter_mut().zip(b) {
                        *x += y;
                    }
                    a
                },
            )
    };
    let total = count(&bits);
    let solid = count(&eroded);
    let n = (w * h) as f64;
    let stats = (0..models.len())
        .map(|i| {
            let c = total[i] as f64;
            let sld = solid[i] as f64;
            let fraction = c / n;
            let mut score = (c - sld) - 3.0 * sld;
            if !(0.002..=0.4).contains(&fraction) {
                score = score.min(0.0) * 10.0 + score.max(0.0) * 0.1;
            }
            MaskStats { fraction, solid_fraction: if c > 0.0 { sld / c } else { 0.0 }, score }
        })
        .collect();
    (bits, stats)
}

/// Erosion radius ~ the half-width of a thick (index) contour line.
fn erosion_radius(w: usize, h: usize) -> usize {
    ((w.max(h) as f64 / 2480.0).round() as usize).clamp(1, 4)
}

/// Bitwise erosion with a (2r+1)² square: a bit survives only if it is set
/// in the whole neighbourhood (outside the image counts as unset).
fn erode_bits(bits: &[u8], w: usize, h: usize, r: usize) -> Vec<u8> {
    let mut horiz = vec![0u8; w * h];
    horiz.par_chunks_mut(w).enumerate().for_each(|(y, row)| {
        let src = &bits[y * w..(y + 1) * w];
        for x in r..w.saturating_sub(r) {
            let mut b = 0xffu8;
            for v in &src[x - r..=x + r] {
                b &= v;
            }
            row[x] = b;
        }
    });
    let mut out = vec![0u8; w * h];
    out.par_chunks_mut(w).enumerate().for_each(|(y, row)| {
        if y < r || y + r >= h {
            return;
        }
        row.copy_from_slice(&horiz[(y - r) * w..(y - r + 1) * w]);
        for yy in (y - r + 1)..=(y + r) {
            for (o, v) in row.iter_mut().zip(&horiz[yy * w..(yy + 1) * w]) {
                *o &= v;
            }
        }
    });
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    const GROOTKLOOF: [u8; 3] = [168, 152, 136];
    const USGS: [u8; 3] = [186, 140, 100];
    const SWISS: [u8; 3] = [210, 140, 70];

    #[test]
    fn default_accepts_common_contour_palettes() {
        let m = default_model();
        for c in [GROOTKLOOF, USGS, SWISS, [150, 110, 70], [200, 170, 140], [232, 154, 90]] {
            assert!(m.matches(c), "{c:?} should be a contour colour");
        }
    }

    #[test]
    fn default_rejects_other_map_colours() {
        let m = default_model();
        let others: [[u8; 3]; 14] = [
            [0, 0, 0],       // black text
            [60, 60, 60],    // dark grey
            [150, 150, 150], // grey road
            [200, 198, 195], // light grey
            [255, 255, 255], // white paper
            [250, 245, 230], // cream paper
            [100, 160, 60],  // green vegetation
            [180, 210, 120], // light green
            [60, 120, 200],  // blue water
            [150, 200, 240], // light blue
            [240, 220, 60],  // yellow road
            [255, 230, 0],   // saturated yellow
            [230, 40, 40],   // red route
            [240, 160, 170], // pink boundary
        ];
        for c in others {
            assert!(!m.matches(c), "{c:?} should not be a contour colour");
        }
    }

    #[test]
    fn picked_colour_model() {
        for c in [GROOTKLOOF, USGS, SWISS, [120, 80, 50]] {
            let m = model_from_rgb(c);
            assert!(m.matches(c));
            // Slightly lighter (anti-aliased) and darker variants.
            let lighter = [c[0].saturating_add(15), c[1].saturating_add(15), c[2].saturating_add(15)];
            let darker = [c[0] - 20, c[1] - 20, c[2] - 20];
            assert!(m.matches(lighter) && m.matches(darker), "{c:?} {m:?}");
            assert!(!m.matches([60, 120, 200]) && !m.matches([255, 255, 255]) && !m.matches([0, 0, 0]));
            assert!(!m.matches([100, 160, 60]));
        }
        // Red contours wrap around hue 0.
        let m = model_from_rgb([200, 40, 50]);
        assert!(m.hue_min > m.hue_max);
        assert!(m.matches([200, 40, 50]) && m.matches([200, 50, 40]));
        assert!(!m.matches([60, 120, 200]));
        // Grey contours: hue-free model.
        let m = model_from_rgb([128, 128, 128]);
        assert!(m.matches([128, 128, 128]) && m.matches([135, 130, 128]));
        assert!(!m.matches([255, 255, 255]) && !m.matches([0, 0, 0]) && !m.matches([210, 140, 70]));
    }

    fn synthetic(line: [u8; 3], fill: Option<[u8; 3]>) -> Rgb8 {
        let (w, h) = (600usize, 400usize);
        let mut data = vec![255u8; w * h * 3];
        let mut put = |x: usize, y: usize, c: [u8; 3]| data[(y * w + x) * 3..(y * w + x) * 3 + 3].copy_from_slice(&c);
        if let Some(f) = fill {
            for y in 250..390 {
                for x in 20..580 {
                    put(x, y, f);
                }
            }
        }
        // Wavy 2-px lines.
        for k in 0..12 {
            for x in 0..w {
                let yc = 15.0 + k as f64 * 30.0 + 8.0 * (x as f64 / 40.0 + k as f64).sin();
                for dy in 0..2 {
                    let y = yc as usize + dy;
                    if y < h {
                        put(x, y, line);
                    }
                }
            }
        }
        Rgb8::from_rgb(w, h, data).unwrap()
    }

    #[test]
    fn extract_mask_on_lines() {
        let img = synthetic(GROOTKLOOF, None);
        let m = extract_mask(&img, &default_model());
        assert!(m.fraction() > 0.05 && m.fraction() < 0.1, "{}", m.fraction());
        let (am, model) = auto_mask(&img);
        assert_eq!(model, default_model());
        assert_eq!(am.count(), m.count());
    }

    #[test]
    fn auto_mask_avoids_area_tints() {
        // Saturated orange contours over a pale beige area tint: the default
        // model also grabs the tint; the saturated model only the lines.
        let img = synthetic(SWISS, Some([232, 218, 195]));
        assert!(default_model().matches([232, 218, 195]));
        let (m, model) = auto_mask(&img);
        assert_eq!(model, saturated_model());
        assert!(!m.get(300, 380), "tint must be excluded");
        let line_px = (0..img.w).filter(|&x| m.get(x, (15.0 + 8.0 * (x as f64 / 40.0).sin()) as usize)).count();
        assert!(line_px > img.w * 9 / 10, "{line_px}");
    }

    /// `img` lit like a photo: a one-sided shadow (× 0.55 at the left edge),
    /// vignetting and a warm colour cast.
    fn photographed(img: &Rgb8) -> Rgb8 {
        let (w, h) = (img.w as f32, img.h as f32);
        let cast = [1.0f32, 0.9, 0.74];
        let mut out = img.clone();
        for y in 0..img.h {
            for x in 0..img.w {
                let v = (y as f32 - h / 2.0) / (h / 2.0);
                let light = (0.55 + 0.45 * x as f32 / w) * (1.0 - 0.15 * v * v);
                for (c, k) in cast.iter().enumerate() {
                    let i = (y * img.w + x) * 3 + c;
                    out.data[i] = (img.data[i] as f32 * light * k).round() as u8;
                }
            }
        }
        out
    }

    /// Brown wavy contours plus two black vertical grid lines on white paper.
    fn paper_map() -> Rgb8 {
        let mut img = synthetic(GROOTKLOOF, None);
        for y in 0..img.h {
            for x in [150usize, 151, 450, 451] {
                img.data[(y * img.w + x) * 3..(y * img.w + x) * 3 + 3].copy_from_slice(&[20, 20, 20]);
            }
        }
        img
    }

    #[test]
    fn normalisation_whitens_shaded_tinted_paper_and_keeps_lines() {
        let clean = paper_map();
        let photo = photographed(&clean);
        let norm = normalize_illumination(&photo);
        let ink = extract_mask(&clean, &default_model());
        let dark = |m: &Rgb8, x: usize, y: usize| m.rgb(x, y).iter().all(|&c| c < 100);
        // Paper: at least 3 px from any line pixel of the clean image.
        let near_ink = |x: usize, y: usize| {
            (y.saturating_sub(3)..=(y + 3).min(clean.h - 1))
                .any(|yy| (x.saturating_sub(3)..=(x + 3).min(clean.w - 1)).any(|xx| clean.rgb(xx, yy) != [255; 3]))
        };
        let (mut paper, mut grey, mut worst_low, mut worst_spread) = (0, 0, 255u8, 0u8);
        for y in (0..clean.h).step_by(3) {
            for x in (0..clean.w).step_by(3) {
                if near_ink(x, y) {
                    continue;
                }
                let p = norm.rgb(x, y);
                let (lo, hi) = (*p.iter().min().unwrap(), *p.iter().max().unwrap());
                grey += (lo < 235) as usize;
                worst_low = worst_low.min(lo);
                worst_spread = worst_spread.max(hi - lo);
                paper += 1;
            }
        }
        assert!(paper > 5000);
        // White within a few levels everywhere but in the image corners, where
        // the shadow and the vignetting together fall off fastest.
        assert!(grey * 100 < paper, "{grey} of {paper} paper samples below 235");
        assert!(worst_low >= 220, "paper not white: {worst_low}");
        assert!(worst_spread <= 8, "colour cast left: {worst_spread}");
        // The raw photo's shaded paper is brown enough to pass as contour colour…
        let raw = extract_mask(&photo, &default_model());
        assert!(raw.fraction() > 3.0 * ink.fraction(), "{} vs {}", raw.fraction(), ink.fraction());
        // …the normalised one is not, and the contours are found in shadow and light.
        let m = extract_mask(&norm, &default_model());
        let (mut hit, mut total, mut false_pos) = (0, 0, 0);
        for y in 0..clean.h {
            for x in 0..clean.w {
                match (ink.get(x, y), m.get(x, y)) {
                    (true, got) => {
                        total += 1;
                        hit += got as usize;
                    }
                    (false, true) => false_pos += 1,
                    _ => {}
                }
            }
        }
        assert!(hit as f64 > 0.95 * total as f64, "{hit} of {total} contour pixels");
        assert!((false_pos as f64) < 0.01 * (clean.w * clean.h) as f64, "{false_pos} false positives");
        // Black grid lines stay black and neutral.
        for y in (0..clean.h).step_by(17) {
            assert!(dark(&norm, 150, y) && dark(&norm, 451, y), "grid line lost at y {y}: {:?}", norm.rgb(150, y));
        }
    }

    #[test]
    fn normalisation_of_flat_and_degenerate_images() {
        let grey = Rgb8::from_rgb(64, 64, vec![200; 64 * 64 * 3]).unwrap();
        assert!(normalize_illumination(&grey).data.iter().all(|&v| v == 255));
        let black = Rgb8::from_rgb(10, 10, vec![0; 300]).unwrap();
        assert!(normalize_illumination(&black).data.iter().all(|&v| v == 0));
        let empty = Rgb8 { w: 0, h: 0, data: Vec::new() };
        assert!(normalize_illumination(&empty).data.is_empty());
        let one = Rgb8::from_rgb(1, 1, vec![10, 20, 30]).unwrap();
        // Levels below the paper floor are divided by the floor, not themselves.
        assert_eq!(normalize_illumination(&one).data[1..], [255, 255]);
    }

    #[test]
    fn photo_mask_bridges_faint_dashes() {
        // A contour whose every 10th–13th pixel is faint (blurred into the paper).
        let (w, h) = (300usize, 40usize);
        let mut data = vec![255u8; w * h * 3];
        let faint = [224u8, 218, 213];
        for x in 0..w {
            for y in 19..21 {
                let c = if x % 10 >= 6 { faint } else { GROOTKLOOF };
                data[(y * w + x) * 3..(y * w + x) * 3 + 3].copy_from_slice(&c);
            }
        }
        // Isolated noise specks of contour colour.
        for &(x, y) in &[(50usize, 5usize), (200, 33), (120, 2)] {
            data[(y * w + x) * 3..(y * w + x) * 3 + 3].copy_from_slice(&GROOTKLOOF);
        }
        let img = Rgb8::from_rgb(w, h, data).unwrap();
        assert!(!default_model().matches(faint) && relaxed_model(&default_model()).matches(faint));
        let plain = extract_mask(&img, &default_model());
        assert!((0..w).filter(|&x| plain.get(x, 19)).count() < w * 7 / 10);
        let (m, model) = photo_mask(&img, None);
        assert_eq!(model, default_model());
        assert!((2..w - 2).all(|x| m.get(x, 19) || m.get(x, 20)), "gaps left");
        assert!(!m.get(50, 5) && !m.get(200, 33) && !m.get(120, 2), "specks kept");
        assert!(m.count() < 3 * w, "{}", m.count());
    }

    #[test]
    fn relaxed_model_widens_and_wraps() {
        let r = relaxed_model(&default_model());
        assert!(r.hue_min < 10.0 && r.hue_max > 45.0 && r.sat_min < 0.08 && r.val_max > 0.92 && r.val_max <= 0.96);
        let red = relaxed_model(&model_from_rgb([200, 40, 50]));
        assert!(red.hue_min > red.hue_max);
        let grey = model_from_rgb([128, 128, 128]);
        let rg = relaxed_model(&grey);
        assert_eq!((rg.hue_min, rg.hue_max), (grey.hue_min, grey.hue_max));
    }

    #[test]
    fn majority_and_growth() {
        let mut m = Mask::new(9, 9);
        m.set(1, 1, true);
        for x in 0..9 {
            m.set(x, 4, true);
            m.set(x, 5, true);
        }
        let f = majority_filter(&m);
        assert!(!f.get(1, 1) && f.get(4, 4) && f.get(4, 5) && !f.get(4, 7));
        let mut seed = Mask::new(9, 9);
        seed.set(0, 0, true);
        let mut allowed = Mask::new(9, 9);
        for i in 0..9 {
            allowed.set(i, i, true);
        }
        allowed.set(8, 0, true);
        let g = grow_into(&seed, &allowed);
        assert!((0..9).all(|i| g.get(i, i)) && !g.get(8, 0));
    }
}
