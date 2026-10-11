//! Coarse global search: orientation-field FFT correlation of map contours vs DEM.
//!
//! Contour lines run perpendicular to the terrain gradient, so the local
//! orientation of the map's contour pattern (structure tensor of the blurred
//! contour mask) must match the orientation of the DEM gradient. Both are
//! encoded as doubled-angle complex fields `z = |g|²·e^{2iφ}` (which makes the
//! two edges of a line, and uphill vs downhill, equivalent), normalised to
//! unit-ish magnitude, and compared with a normalised complex cross-correlation
//! computed by FFT over all translations, for every candidate scale and
//! rotation. The correlation peak gives where the map is and at what scale.
//!
//! Resolution: for each scale the template is built on the coarsest DEM
//! pyramid level (the given mosaic, 2×, 4×, … downsampled) on which its
//! smaller side still has ≥ `TARGET_SIDE` cells. This keeps every template at
//! a similar number of cells, so scores are comparable across scales and the
//! FFTs stay small; templates below `MIN_SIDE` cells are never used because
//! they produce spurious peaks. Peaks are ranked by their significance within
//! their own correlation surface (see `select_candidates`).
//!
//! Rotation: instead of rotating each template, the DEM field is resampled
//! once per rotation into the map's frame (positions rotated by θ, doubled
//! angles by −2θ), so templates stay axis-aligned and window energies are box
//! sums. A rotation sweep runs in two stages: all rotations with smaller
//! templates, then a local fine search around the best hypotheses.

use crate::dem::{DemMosaic, MercBox};
use crate::fft::{next_fast_len, Fft2, C32};
use crate::geo::{merc_to_latlon, Affine};
use crate::image::{GrayF32, Mask};
use rayon::prelude::*;
use std::sync::atomic::{AtomicUsize, Ordering};

/// Doubled-angle orientation field derived from the contour mask at a
/// reduced working resolution.
#[derive(Clone)]
pub struct MapField {
    /// Field size in cells.
    pub w: usize,
    pub h: usize,
    /// Base-image pixels per field cell.
    pub px_per_cell: f64,
    /// Raw structure-tensor components (Jxx−Jyy, 2Jxy) before smoothing to the template scale.
    pub re: Vec<f32>,
    pub im: Vec<f32>,
}

/// Field cell size in base-image pixels: 2 for a 300-dpi A3/A4 sheet, so the
/// blur below spans a typical (2–4 px) contour line.
pub fn field_px_per_cell(w: usize, h: usize) -> usize {
    ((w.max(h) as f64 / 2480.0).round() as usize).clamp(1, 4)
}

/// Gaussian sigma (field cells) applied to the contour density before Sobel.
const MAP_BLUR_SIGMA: f64 = 1.0;

/// Build the map field from the full-resolution contour mask.
pub fn map_field(mask: &Mask) -> MapField {
    let ppc = field_px_per_cell(mask.w, mask.h);
    let density = box_downsample_mask(mask, ppc);
    let g = density.gaussian_blur(MAP_BLUR_SIGMA);
    let (gx, gy) = g.sobel();
    let n = g.w * g.h;
    let mut re = vec![0.0f32; n];
    let mut im = vec![0.0f32; n];
    re.par_iter_mut().zip(im.par_iter_mut()).enumerate().for_each(|(i, (r, m))| {
        let (x, y) = (gx.data[i], gy.data[i]);
        *r = x * x - y * y;
        *m = 2.0 * x * y;
    });
    MapField { w: g.w, h: g.h, px_per_cell: ppc as f64, re, im }
}

/// Mean of each `f`×`f` pixel block (blocks past the edge count missing pixels as 0).
fn box_downsample_mask(mask: &Mask, f: usize) -> GrayF32 {
    let (w, h) = (mask.w.div_ceil(f), mask.h.div_ceil(f));
    let mut out = vec![0.0f32; w * h];
    let inv = 1.0 / (f * f) as f32;
    out.par_chunks_mut(w).enumerate().for_each(|(cy, row)| {
        for yy in cy * f..((cy + 1) * f).min(mask.h) {
            let src = &mask.data[yy * mask.w..(yy + 1) * mask.w];
            for (x, &v) in src.iter().enumerate() {
                row[x / f] += v as f32;
            }
        }
        for v in row.iter_mut() {
            *v *= inv;
        }
    });
    GrayF32 { w, h, data: out }
}

#[derive(Debug, Clone)]
pub struct SearchParams {
    /// GPS position in Mercator metres.
    pub center_merc: (f64, f64),
    /// Ground-metre radius around the GPS position the map centre may lie in.
    pub search_radius_m: f64,
    /// Ground metres per base-image pixel range to scan.
    pub scale_min: f64,
    pub scale_max: f64,
    /// Map-up bearings to try, degrees (default [0.0]). See `rotation_sweep`.
    pub rotations_deg: Vec<f64>,
    /// Max number of distinct candidates to return.
    pub top_k: usize,
}

impl Default for SearchParams {
    fn default() -> Self {
        SearchParams {
            center_merc: (0.0, 0.0),
            search_radius_m: 20_000.0,
            scale_min: 0.5,
            scale_max: 10.0,
            rotations_deg: vec![0.0],
            top_k: 3,
        }
    }
}

/// Rotations for a full sweep: 0, step, 2·step, … < 360. 10° is a good step
/// (the fine stage resolves the rest). A sweep costs ~ (360/step) times a
/// north-up search at a quarter of the resolution, so narrow the scale range
/// (e.g. from an OCR'd "1:25 000" and the dpi) when sweeping.
pub fn rotation_sweep(step_deg: f64) -> Vec<f64> {
    let step = step_deg.clamp(1.0, 180.0);
    let n = (360.0 / step).round().max(1.0) as usize;
    (0..n).map(|i| i as f64 * 360.0 / n as f64).collect()
}

#[derive(Debug, Clone)]
pub struct Candidate {
    /// Pixel → Mercator similarity.
    pub transform: Affine,
    /// Normalised correlation score of the peak.
    pub score: f32,
    /// Peak significance / best significance at a location ≥ 2 km away (any
    /// scale), where significance z = (NCC − mean) / std of the template's
    /// whole correlation surface (comparable across scales and DEM levels).
    pub peak_ratio: f32,
    /// Ground metres per pixel.
    pub scale_m_per_px: f64,
    pub rotation_deg: f64,
}

impl Candidate {
    /// Mercator position of the image centre.
    pub fn center_merc(&self, img_w: usize, img_h: usize) -> (f64, f64) {
        self.transform.apply(img_w as f64 / 2.0, img_h as f64 / 2.0)
    }
}

/// Geometric scale step between searched scales.
pub const SCALE_STEP: f64 = 1.04;
/// Preferred minimum template side (cells) when picking the DEM level.
const TARGET_SIDE: f64 = 64.0;
/// Same for the first stage of a rotation sweep (smaller, cheaper templates).
const SWEEP_TARGET_SIDE: f64 = 32.0;
/// Templates smaller than this are not searched (spurious peaks).
pub const MIN_SIDE: f64 = 24.0;
/// A coarser level is used instead of FFT tiling while templates keep this many cells.
const TILED_MIN_SIDE: f64 = 40.0;
/// Largest FFT side; bigger windows are tiled.
const FFT_CAP: usize = 1024;
/// Gaussian sigma (template cells) applied after area-resampling the map field.
const TEMPLATE_SIGMA: f64 = 0.5;
/// Gaussian sigma (DEM cells) applied to the DEM orientation field.
const DEM_SIGMA: f64 = 1.0;
/// Candidates closer than this (ground metres) are the same location.
pub const DISTINCT_M: f64 = 2000.0;
/// Peaks kept per (template, rotation) after non-maximum suppression.
const PEAKS_PER_TEMPLATE: usize = 6;
/// Placements whose template window has less valid DEM than this are skipped.
const MIN_COVERAGE: f64 = 0.5;
/// Energy floor per template cell in the NCC denominator (|D|² units), so
/// flat or missing terrain cannot produce high scores from noise.
const ENERGY_FLOOR: f64 = 0.02;
/// Largest half-size (ground metres) of the coarse DEM box.
pub const MAX_BOX_HALF_M: f64 = 60_000.0;
/// Floor of the runner-up significance in `peak_ratio` (a search area with
/// no structure elsewhere must not give an unbounded ratio).
const MIN_RATIO_DENOM: f32 = 2.0;
/// Fine stage of a sweep: search radius (ground metres) around each hypothesis.
const SWEEP_LOCAL_RADIUS_M: f64 = 1500.0;

fn scale_list(p: &SearchParams) -> Vec<f64> {
    let (lo, hi) = (p.scale_min.max(1e-3), p.scale_max.max(p.scale_min.max(1e-3)));
    let n = ((hi / lo).ln() / SCALE_STEP.ln()).ceil().max(0.0) as usize + 1;
    if n == 1 {
        return vec![lo];
    }
    (0..n).map(|i| lo * (hi / lo).powf(i as f64 / (n - 1) as f64)).collect()
}

fn rotation_list(p: &SearchParams) -> Vec<f64> {
    if p.rotations_deg.is_empty() {
        vec![0.0]
    } else {
        p.rotations_deg.clone()
    }
}

fn is_zero_rotation(deg: f64) -> bool {
    let r = deg.rem_euclid(360.0);
    r < 1e-9 || (360.0 - r) < 1e-9
}

/// Mercator metres per ground metre at the GPS latitude.
fn merc_per_ground(p: &SearchParams) -> f64 {
    let (lat, _) = merc_to_latlon(p.center_merc.0, p.center_merc.1);
    1.0 / lat.to_radians().cos().max(1e-6)
}

/// Mercator box the coarse DEM must cover for these params and image size.
///
/// GPS ± (search radius + half the map extent at `scale_max`), plus a small
/// margin for gradients, capped at `MAX_BOX_HALF_M` ground metres per side.
pub fn required_dem_box(params: &SearchParams, img_w: usize, img_h: usize) -> MercBox {
    let k = merc_per_ground(params);
    let s = params.scale_max.max(params.scale_min) * k;
    let rots = rotation_list(params);
    let (hx, hy) = if rots.iter().all(|&r| is_zero_rotation(r)) {
        (img_w as f64 * s / 2.0, img_h as f64 * s / 2.0)
    } else {
        let d = ((img_w * img_w + img_h * img_h) as f64).sqrt() * s / 2.0;
        (d, d)
    };
    let r = params.search_radius_m.max(0.0) * k;
    let margin = 500.0 * k;
    let cap = MAX_BOX_HALF_M * k;
    let (cx, cy) = params.center_merc;
    let half_x = (r + hx + margin).min(cap);
    let half_y = (r + hy + margin).min(cap);
    MercBox { min_x: cx - half_x, min_y: cy - half_y, max_x: cx + half_x, max_y: cy + half_y }
}

/// Smallest ground scale (m/px) searchable on a DEM of `dem_res` Mercator
/// metres per cell for an image of this size (template side ≥ `MIN_SIDE`).
pub fn min_searchable_scale(img_w: usize, img_h: usize, dem_res: f64, lat_deg: f64) -> f64 {
    MIN_SIDE * dem_res * lat_deg.to_radians().cos() / img_w.min(img_h).max(1) as f64
}

/// Map-field pyramid (level q = 2^q field cells per cell), zero outside.
struct FieldPyramid {
    levels: Vec<(usize, usize, Vec<f32>, Vec<f32>)>,
}

impl FieldPyramid {
    fn new(f: &MapField) -> FieldPyramid {
        let mut levels = vec![(f.w, f.h, f.re.clone(), f.im.clone())];
        loop {
            let (w, h, re, im) = levels.last().unwrap();
            if *w < 64 || *h < 64 {
                break;
            }
            let (nw, nh) = (w.div_ceil(2), h.div_ceil(2));
            let half = |src: &[f32]| -> Vec<f32> {
                let mut out = vec![0.0f32; nw * nh];
                out.par_chunks_mut(nw).enumerate().for_each(|(y, row)| {
                    for (x, o) in row.iter_mut().enumerate() {
                        let mut s = 0.0;
                        for (dx, dy) in [(0, 0), (1, 0), (0, 1), (1, 1)] {
                            let (xx, yy) = (2 * x + dx, 2 * y + dy);
                            if xx < *w && yy < *h {
                                s += src[yy * w + xx];
                            }
                        }
                        *o = 0.25 * s;
                    }
                });
                out
            };
            let next = (nw, nh, half(re), half(im));
            levels.push(next);
        }
        FieldPyramid { levels }
    }
}

/// 1-D area-resampling weights: output cell i covers source [i/k, (i+1)/k).
fn area_weights(src_len: usize, k: f64) -> (usize, Vec<(usize, Vec<f32>)>) {
    let n = ((src_len as f64 * k).ceil() as usize).max(1);
    let w = (0..n)
        .map(|i| {
            let a = i as f64 / k;
            let b = (i + 1) as f64 / k;
            let s0 = a.floor() as usize;
            let s1 = (b.ceil() as usize).min(src_len);
            let ws = (s0..s1).map(|s| (((b.min(s as f64 + 1.0) - a.max(s as f64)).max(0.0)) * k) as f32).collect();
            (s0, ws)
        })
        .collect();
    (n, w)
}

/// Separable area resample of (re, im) by factor k (< 1 shrinks); outside is 0.
fn area_resample(re: &[f32], im: &[f32], w: usize, h: usize, k: f64) -> (usize, usize, Vec<f32>, Vec<f32>) {
    let (nw, wx) = area_weights(w, k);
    let (nh, wy) = area_weights(h, k);
    // Horizontal.
    let mut tr = vec![0.0f32; nw * h];
    let mut ti = vec![0.0f32; nw * h];
    tr.par_chunks_mut(nw).zip(ti.par_chunks_mut(nw)).enumerate().for_each(|(y, (rr, ri))| {
        let (sr, si) = (&re[y * w..(y + 1) * w], &im[y * w..(y + 1) * w]);
        for (o, (s0, ws)) in wx.iter().enumerate() {
            let (mut a, mut b) = (0.0f32, 0.0f32);
            for (j, wt) in ws.iter().enumerate() {
                a += wt * sr[s0 + j];
                b += wt * si[s0 + j];
            }
            rr[o] = a;
            ri[o] = b;
        }
    });
    // Vertical.
    let mut or = vec![0.0f32; nw * nh];
    let mut oi = vec![0.0f32; nw * nh];
    or.par_chunks_mut(nw).zip(oi.par_chunks_mut(nw)).enumerate().for_each(|(o, (rr, ri))| {
        let (s0, ws) = &wy[o];
        for (j, wt) in ws.iter().enumerate() {
            let y = s0 + j;
            let (sr, si) = (&tr[y * nw..(y + 1) * nw], &ti[y * nw..(y + 1) * nw]);
            for (((a, b), c), d) in rr.iter_mut().zip(ri.iter_mut()).zip(sr).zip(si) {
                *a += wt * c;
                *b += wt * d;
            }
        }
    });
    (nw, nh, or, oi)
}

/// Normalise a complex field to unit-ish magnitude: z / max(|z|, p90(|z|)).
fn normalise(re: &mut [f32], im: &mut [f32], valid: Option<&[bool]>) {
    let mags: Vec<f32> = re
        .iter()
        .zip(im.iter())
        .enumerate()
        .filter(|(i, _)| valid.is_none_or(|v| v[*i]))
        .map(|(_, (a, b))| (a * a + b * b).sqrt())
        .filter(|&m| m > 0.0 && m.is_finite())
        .collect();
    if mags.is_empty() {
        return;
    }
    let g = GrayF32 { w: mags.len(), h: 1, data: mags };
    let p90 = g.percentile(90.0, true).max(1e-12);
    re.par_iter_mut().zip(im.par_iter_mut()).for_each(|(a, b)| {
        let m = (*a * *a + *b * *b).sqrt();
        let d = m.max(p90);
        if d.is_finite() && d > 0.0 {
            *a /= d;
            *b /= d;
        } else {
            *a = 0.0;
            *b = 0.0;
        }
    });
}

/// A built, normalised, north-up template (map frame).
struct Template {
    w: usize,
    h: usize,
    data: Vec<C32>,
    /// Template coordinates of the map centre.
    cx: f64,
    cy: f64,
    /// Exact Mercator metres per pixel.
    s_merc: f64,
    norm: f64,
}

fn build_template(pyr: &FieldPyramid, ppc: f64, img_w: usize, img_h: usize, s_merc: f64, res: f64) -> Option<Template> {
    // Template cells per field cell.
    let k = s_merc * ppc / res;
    if !(k.is_finite() && k > 0.0) {
        return None;
    }
    // Resample from the pyramid level whose cells are still ≤ half a template cell.
    let mut q = 0usize;
    while q + 1 < pyr.levels.len() && k * (1u64 << (q + 1)) as f64 <= 0.5 {
        q += 1;
    }
    let kq = k * (1u64 << q) as f64;
    let (fw, fh, re, im) = &pyr.levels[q];
    let (tw, th, tr, ti) = area_resample(re, im, *fw, *fh, kq);
    let mut tr = GrayF32 { w: tw, h: th, data: tr }.gaussian_blur(TEMPLATE_SIGMA).data;
    let mut ti = GrayF32 { w: tw, h: th, data: ti }.gaussian_blur(TEMPLATE_SIGMA).data;
    normalise(&mut tr, &mut ti, None);
    let data: Vec<C32> = tr.iter().zip(&ti).map(|(&a, &b)| C32::new(a, b)).collect();
    let norm = data.iter().map(|z| z.norm_sqr() as f64).sum::<f64>().sqrt();
    Some(Template {
        w: tw,
        h: th,
        data,
        cx: k * img_w as f64 / (2.0 * ppc),
        cy: k * img_h as f64 / (2.0 * ppc),
        s_merc: res * k / ppc,
        norm,
    })
}

/// Normalised DEM orientation field over a window of a pyramid level.
struct GridField {
    /// Window origin in level cells (may be outside the mosaic).
    x0: i64,
    y0: i64,
    w: usize,
    h: usize,
    d: Vec<C32>,
    valid: Vec<bool>,
}

impl GridField {
    /// Bilinear sample at window index coordinates (cell centres at integers);
    /// None unless all four cells are valid.
    #[inline]
    fn sample(&self, x: f64, y: f64) -> Option<C32> {
        if !(x >= 0.0 && y >= 0.0) {
            return None;
        }
        let (x0, y0) = (x.floor() as usize, y.floor() as usize);
        if x0 + 1 >= self.w || y0 + 1 >= self.h {
            return None;
        }
        let i = y0 * self.w + x0;
        let j = i + self.w;
        if !(self.valid[i] && self.valid[i + 1] && self.valid[j] && self.valid[j + 1]) {
            return None;
        }
        let (fx, fy) = ((x - x0 as f64) as f32, (y - y0 as f64) as f32);
        Some(
            self.d[i] * ((1.0 - fx) * (1.0 - fy))
                + self.d[i + 1] * (fx * (1.0 - fy))
                + self.d[j] * ((1.0 - fx) * fy)
                + self.d[j + 1] * (fx * fy),
        )
    }
}

/// Elevation grid of a pyramid level cropped to [x0, x0+w) × [y0, y0+h) (level cells); NaN outside.
fn crop(dem: &DemMosaic, x0: i64, y0: i64, w: usize, h: usize) -> Vec<f32> {
    let mut out = vec![f32::NAN; w * h];
    out.par_chunks_mut(w).enumerate().for_each(|(r, row)| {
        let yy = y0 + r as i64;
        if yy < 0 || yy >= dem.h() as i64 {
            return;
        }
        for (c, o) in row.iter_mut().enumerate() {
            let xx = x0 + c as i64;
            if xx >= 0 && xx < dem.w() as i64 {
                *o = dem.grid.get(xx as usize, yy as usize);
            }
        }
    });
    out
}

/// DEM orientation field (gradient doubled angle, smoothed, normalised) over
/// the window [x0, x0+w) × [y0, y0+h) of a level.
fn grid_field(dem: &DemMosaic, x0: i64, y0: i64, w: usize, h: usize) -> GridField {
    let m = 2 + (3.0 * DEM_SIGMA).ceil() as usize;
    let (cw, ch) = (w + 2 * m, h + 2 * m);
    let e = crop(dem, x0 - m as i64, y0 - m as i64, cw, ch);
    let mut re = vec![0.0f32; cw * ch];
    let mut im = vec![0.0f32; cw * ch];
    let mut valid = vec![false; cw * ch];
    re.par_chunks_mut(cw).zip(im.par_chunks_mut(cw)).zip(valid.par_chunks_mut(cw)).enumerate().for_each(
        |(y, ((rr, ri), rv))| {
            if y == 0 || y + 1 >= ch {
                return;
            }
            for x in 1..cw - 1 {
                let c = e[y * cw + x];
                let (l, r) = (e[y * cw + x - 1], e[y * cw + x + 1]);
                let (u, d) = (e[(y - 1) * cw + x], e[(y + 1) * cw + x]);
                if !(c.is_finite() && l.is_finite() && r.is_finite() && u.is_finite() && d.is_finite()) {
                    continue;
                }
                let gx = 0.5 * (r - l);
                let gy = 0.5 * (d - u);
                rr[x] = gx * gx - gy * gy;
                ri[x] = 2.0 * gx * gy;
                rv[x] = true;
            }
        },
    );
    let br = GrayF32 { w: cw, h: ch, data: re }.gaussian_blur(DEM_SIGMA);
    let bi = GrayF32 { w: cw, h: ch, data: im }.gaussian_blur(DEM_SIGMA);
    let mut dre = vec![0.0f32; w * h];
    let mut dim = vec![0.0f32; w * h];
    let mut dv = vec![false; w * h];
    for y in 0..h {
        for x in 0..w {
            let s = (y + m) * cw + x + m;
            if valid[s] {
                dre[y * w + x] = br.data[s];
                dim[y * w + x] = bi.data[s];
                dv[y * w + x] = true;
            }
        }
    }
    normalise(&mut dre, &mut dim, Some(&dv));
    let d = dre.iter().zip(&dim).map(|(&a, &b)| C32::new(a, b)).collect();
    GridField { x0, y0, w, h, d, valid: dv }
}

/// One FFT tile of a frame.
struct Tile {
    /// Offset in the frame (cells).
    x0: usize,
    y0: usize,
    w: usize,
    h: usize,
    fft: Fft2,
    d_hat: Vec<C32>,
}

/// The DEM field resampled into the map's frame for one rotation: frame cell
/// (u, v) ↔ level edge coordinates G = gc + R(θ)·((u, v) + ½ − fc).
struct Frame {
    w: usize,
    h: usize,
    theta: f64,
    /// GPS position in level edge coordinates, and its frame coordinates.
    gc: (f64, f64),
    fc: (f64, f64),
    /// Integral images ((w+1)×(h+1)) of |D|² and validity.
    e_int: Vec<f64>,
    v_int: Vec<f64>,
    tiles: Vec<Tile>,
}

impl Frame {
    #[inline]
    fn box_sum(&self, int: &[f64], x: usize, y: usize, w: usize, h: usize) -> f64 {
        let s = self.w + 1;
        int[(y + h) * s + x + w] - int[y * s + x + w] - int[(y + h) * s + x] + int[y * s + x]
    }

    /// Frame coordinates → level edge coordinates.
    fn to_level(&self, u: f64, v: f64) -> (f64, f64) {
        let (s, c) = self.theta.sin_cos();
        let (du, dv) = (u - self.fc.0, v - self.fc.1);
        (self.gc.0 + c * du - s * dv, self.gc.1 + s * du + c * dv)
    }
}

fn integral(vals: &[f64], w: usize, h: usize) -> Vec<f64> {
    let s = w + 1;
    let mut out = vec![0.0f64; s * (h + 1)];
    for y in 0..h {
        let mut run = 0.0;
        for x in 0..w {
            run += vals[y * w + x];
            out[(y + 1) * s + x + 1] = out[y * s + x + 1] + run;
        }
    }
    out
}

/// Resample the grid field into the frame of rotation θ (size fw × fh), with
/// FFT tiles sized for templates up to `max_t` cells.
fn build_frame(g: &GridField, gc: (f64, f64), theta: f64, fw: usize, fh: usize, max_t: (usize, usize)) -> Frame {
    let zero = is_zero_rotation(theta.to_degrees());
    // North-up frames are an exact integer-offset crop of the grid field.
    let fc = if zero {
        (gc.0 - (gc.0 - fw as f64 / 2.0).floor(), gc.1 - (gc.1 - fh as f64 / 2.0).floor())
    } else {
        (fw as f64 / 2.0, fh as f64 / 2.0)
    };
    let (s, c) = theta.sin_cos();
    // Doubled angles turn by −2θ from the level grid into the map frame.
    let rot = C32::new((2.0 * theta).cos() as f32, -(2.0 * theta).sin() as f32);
    let mut d = vec![C32::new(0.0, 0.0); fw * fh];
    let mut valid = vec![false; fw * fh];
    d.par_chunks_mut(fw).zip(valid.par_chunks_mut(fw)).enumerate().for_each(|(v, (rd, rv))| {
        for u in 0..fw {
            let (du, dv) = (u as f64 + 0.5 - fc.0, v as f64 + 0.5 - fc.1);
            let (gx, gy) = (gc.0 + c * du - s * dv, gc.1 + s * du + c * dv);
            // Window index coordinates (cell centres at integers).
            let (wx, wy) = (gx - 0.5 - g.x0 as f64, gy - 0.5 - g.y0 as f64);
            let val = if zero {
                let (ix, iy) = (wx.round() as i64, wy.round() as i64);
                if ix >= 0 && iy >= 0 && (ix as usize) < g.w && (iy as usize) < g.h {
                    let i = iy as usize * g.w + ix as usize;
                    g.valid[i].then_some(g.d[i])
                } else {
                    None
                }
            } else {
                g.sample(wx, wy).map(|z| z * rot)
            };
            if let Some(z) = val {
                rd[u] = z;
                rv[u] = true;
            }
        }
    });
    let energy: Vec<f64> = d.iter().map(|z| z.norm_sqr() as f64).collect();
    let vals: Vec<f64> = valid.iter().map(|&v| v as u8 as f64).collect();
    let e_int = integral(&energy, fw, fh);
    let v_int = integral(&vals, fw, fh);
    // Tiles: one FFT window if it fits, else overlapping tiles so that every
    // placement of the largest template lies wholly inside some tile.
    let tile_axis = |n: usize, t: usize| -> Vec<(usize, usize)> {
        if next_fast_len(n) <= FFT_CAP || t >= FFT_CAP {
            return vec![(0, n)];
        }
        let stride = FFT_CAP + 1 - t;
        let mut v = Vec::new();
        let mut s = 0;
        loop {
            let len = FFT_CAP.min(n - s);
            v.push((s, len));
            if s + len >= n {
                break;
            }
            s += stride;
        }
        v
    };
    let mut tiles = Vec::new();
    for &(ty0, th) in &tile_axis(fh, max_t.1) {
        for &(tx0, tw) in &tile_axis(fw, max_t.0) {
            let (nw, nh) = (next_fast_len(tw), next_fast_len(th));
            let fft = Fft2::new(nw, nh);
            let mut buf = vec![C32::new(0.0, 0.0); nw * nh];
            for y in 0..th {
                buf[y * nw..y * nw + tw].copy_from_slice(&d[(ty0 + y) * fw + tx0..(ty0 + y) * fw + tx0 + tw]);
            }
            fft.forward(&mut buf);
            tiles.push(Tile { x0: tx0, y0: ty0, w: tw, h: th, fft, d_hat: buf });
        }
    }
    Frame { w: fw, h: fh, theta, gc, fc, e_int, v_int, tiles }
}

/// A correlation peak (map centre position in Mercator), for diagnostics.
#[derive(Debug, Clone)]
pub struct Peak {
    /// Normalised correlation.
    pub score: f32,
    /// Mean and standard deviation of the template's whole NCC surface.
    pub surface_mean: f32,
    pub surface_std: f32,
    /// Map-centre Mercator position.
    pub mx: f64,
    pub my: f64,
    /// Mercator metres per pixel.
    pub s_merc: f64,
    pub rot_deg: f64,
    /// DEM pyramid level and template size (cells).
    pub level: usize,
    pub tw: usize,
    pub th: usize,
    scale_idx: usize,
    rot_idx: usize,
}

impl Peak {
    /// Peak height in standard deviations of its template's NCC surface.
    pub fn z(&self) -> f32 {
        (self.score - self.surface_mean) / self.surface_std.max(1e-6)
    }
}

fn surface_stats(ncc: &[f32]) -> (f32, f32) {
    let (mut n, mut s, mut s2) = (0usize, 0.0f64, 0.0f64);
    for &v in ncc {
        if v.is_finite() {
            n += 1;
            s += v as f64;
            s2 += (v as f64) * (v as f64);
        }
    }
    if n < 2 {
        return (0.0, 1.0);
    }
    let m = s / n as f64;
    (m as f32, ((s2 / n as f64 - m * m).max(0.0)).sqrt() as f32)
}

/// Correlate one template over a frame; returns the NCC surface over all
/// placements (fw−tw+1)×(fh−th+1), −∞ where invalid or outside the disc of
/// `radius` frame cells around the GPS position.
fn correlate(t: &Template, f: &Frame, radius: f64) -> (usize, usize, Vec<f32>) {
    if t.w > f.w || t.h > f.h {
        return (0, 0, Vec::new());
    }
    let (pw, ph) = (f.w - t.w + 1, f.h - t.h + 1);
    let mut ncc = vec![f32::NEG_INFINITY; pw * ph];
    let r2 = radius * radius;
    let n_cells = (t.w * t.h) as f64;
    let floor = ENERGY_FLOOR * n_cells;
    let norm = t.norm.max(1e-12);
    for tile in &f.tiles {
        if t.w > tile.w || t.h > tile.h {
            continue;
        }
        let (fw, fh) = (tile.fft.w, tile.fft.h);
        let mut buf = vec![C32::new(0.0, 0.0); fw * fh];
        for y in 0..t.h {
            buf[y * fw..y * fw + t.w].copy_from_slice(&t.data[y * t.w..(y + 1) * t.w]);
        }
        tile.fft.forward(&mut buf);
        buf.par_iter_mut().zip(tile.d_hat.par_iter()).for_each(|(a, d)| *a = d * a.conj());
        tile.fft.inverse(&mut buf);
        let (tpw, tph) = (tile.w - t.w + 1, tile.h - t.h + 1);
        let rows: Vec<(usize, Vec<(usize, f32)>)> = (0..tph)
            .into_par_iter()
            .map(|ly| {
                let iy = tile.y0 + ly;
                let mut row = Vec::with_capacity(tpw);
                let dy = iy as f64 + t.cy - f.fc.1;
                for lx in 0..tpw {
                    let ix = tile.x0 + lx;
                    let dx = ix as f64 + t.cx - f.fc.0;
                    if dx * dx + dy * dy > r2 {
                        continue;
                    }
                    let cover = f.box_sum(&f.v_int, ix, iy, t.w, t.h);
                    if cover < MIN_COVERAGE * n_cells {
                        continue;
                    }
                    let energy = f.box_sum(&f.e_int, ix, iy, t.w, t.h);
                    let c = buf[ly * fw + lx].re as f64;
                    row.push((ix, (c / (norm * (energy.max(0.0) + floor).sqrt())) as f32));
                }
                (iy, row)
            })
            .collect();
        for (iy, row) in rows {
            for (ix, s) in row {
                let o = &mut ncc[iy * pw + ix];
                if s > *o {
                    *o = s;
                }
            }
        }
    }
    (pw, ph, ncc)
}

/// Non-maximum suppression: up to `n` peaks at least `rad` cells apart, with
/// sub-cell (parabolic) positions.
fn find_peaks(ncc: &[f32], pw: usize, ph: usize, n: usize, rad: f64) -> Vec<(f32, f64, f64)> {
    let mut work = ncc.to_vec();
    let mut out = Vec::new();
    let r = rad.ceil() as isize;
    for _ in 0..n {
        let (mut bi, mut bv) = (usize::MAX, f32::NEG_INFINITY);
        for (i, &v) in work.iter().enumerate() {
            if v > bv {
                bv = v;
                bi = i;
            }
        }
        if bi == usize::MAX || !bv.is_finite() {
            break;
        }
        let (x, y) = ((bi % pw) as isize, (bi / pw) as isize);
        let at = |xx: isize, yy: isize| -> Option<f32> {
            if xx < 0 || yy < 0 || xx >= pw as isize || yy >= ph as isize {
                return None;
            }
            let v = ncc[yy as usize * pw + xx as usize];
            v.is_finite().then_some(v)
        };
        let sub = |a: Option<f32>, c: f32, b: Option<f32>| -> f64 {
            match (a, b) {
                (Some(a), Some(b)) => {
                    let den = (a - 2.0 * c + b) as f64;
                    if den < 0.0 {
                        (0.5 * (a - b) as f64 / den).clamp(-0.5, 0.5)
                    } else {
                        0.0
                    }
                }
                _ => 0.0,
            }
        };
        let fx = x as f64 + sub(at(x - 1, y), bv, at(x + 1, y));
        let fy = y as f64 + sub(at(x, y - 1), bv, at(x, y + 1));
        out.push((bv, fx, fy));
        for yy in (y - r).max(0)..=(y + r).min(ph as isize - 1) {
            for xx in (x - r).max(0)..=(x + r).min(pw as isize - 1) {
                let (ddx, ddy) = ((xx - x) as f64, (yy - y) as f64);
                if ddx * ddx + ddy * ddy <= rad * rad {
                    work[yy as usize * pw + xx as usize] = f32::NEG_INFINITY;
                }
            }
        }
    }
    out
}

/// Run the coarse search. `dem` is a coarse mosaic (zoom 11, ~76 m) covering
/// `required_dem_box`. Returns up to `top_k` candidates, best first.
/// `cancel()` is polled between scales; `progress(f)` receives 0..1.
pub fn coarse_search(
    field: &MapField,
    dem: &DemMosaic,
    img_w: usize,
    img_h: usize,
    params: &SearchParams,
    cancel: &(dyn Fn() -> bool + Sync),
    progress: &(dyn Fn(f32) + Sync),
) -> Vec<Candidate> {
    let rots = rotation_list(params);
    if rots.len() <= 1 {
        let peaks = search_peaks_with(field, dem, img_w, img_h, params, TARGET_SIDE, cancel, progress);
        return select_candidates(&peaks, img_w, img_h, params);
    }
    // Rotation sweep, stage 1: every rotation with smaller templates.
    let stage1 = |f: f32| progress(0.8 * f);
    let peaks = search_peaks_with(field, dem, img_w, img_h, params, SWEEP_TARGET_SIDE, cancel, &stage1);
    let mut cands = select_candidates(&peaks, img_w, img_h, params);
    // Stage 2: around each hypothesis, finer rotations and scales at full resolution.
    let mut sorted = rots.iter().map(|r| r.rem_euclid(360.0)).collect::<Vec<_>>();
    sorted.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let step = sorted.windows(2).map(|w| w[1] - w[0]).filter(|&d| d > 1e-6).fold(10.0f64, f64::min);
    let n = cands.len().max(1);
    for (k, c) in cands.iter_mut().enumerate() {
        if cancel() {
            return Vec::new();
        }
        let local = SearchParams {
            center_merc: c.center_merc(img_w, img_h),
            search_radius_m: SWEEP_LOCAL_RADIUS_M,
            scale_min: c.scale_m_per_px / SCALE_STEP.powi(2),
            scale_max: c.scale_m_per_px * SCALE_STEP.powi(2),
            rotations_deg: (-2..=2).map(|i| c.rotation_deg + i as f64 * step / 4.0).collect(),
            top_k: 1,
        };
        let sub = |f: f32| progress(0.8 + 0.2 * (k as f32 + f) / n as f32);
        let lp = search_peaks_with(field, dem, img_w, img_h, &local, TARGET_SIDE, cancel, &sub);
        if let Some(best) = lp.iter().max_by(|a, b| a.score.partial_cmp(&b.score).unwrap_or(std::cmp::Ordering::Equal))
        {
            let (lat, _) = merc_to_latlon(best.mx, best.my);
            c.transform = Affine::pixel_to_merc_similarity(
                best.s_merc,
                best.rot_deg.to_radians(),
                img_w as f64 / 2.0,
                img_h as f64 / 2.0,
                best.mx,
                best.my,
            );
            c.scale_m_per_px = best.s_merc * lat.to_radians().cos();
            c.rotation_deg = best.rot_deg.rem_euclid(360.0);
            c.score = best.score;
        }
    }
    progress(1.0);
    cands
}

/// All correlation peaks (every scale and rotation), unsorted. Diagnostics;
/// `coarse_search` turns these into candidates.
pub fn search_peaks(
    field: &MapField,
    dem: &DemMosaic,
    img_w: usize,
    img_h: usize,
    params: &SearchParams,
    cancel: &(dyn Fn() -> bool + Sync),
    progress: &(dyn Fn(f32) + Sync),
) -> Vec<Peak> {
    search_peaks_with(field, dem, img_w, img_h, params, TARGET_SIDE, cancel, progress)
}

#[allow(clippy::too_many_arguments)]
fn search_peaks_with(
    field: &MapField,
    dem: &DemMosaic,
    img_w: usize,
    img_h: usize,
    params: &SearchParams,
    target_side: f64,
    cancel: &(dyn Fn() -> bool + Sync),
    progress: &(dyn Fn(f32) + Sync),
) -> Vec<Peak> {
    if img_w == 0 || img_h == 0 || dem.w() < 4 || dem.h() < 4 || field.w == 0 || field.h == 0 {
        return Vec::new();
    }
    let kmg = merc_per_ground(params);
    let r0 = dem.res;
    let rots = rotation_list(params);
    let any_rot = rots.iter().any(|&r| !is_zero_rotation(r));
    // Plan: each scale on its DEM level: the coarsest level keeping
    // ≥ target_side cells, or coarser while the search window would need FFT
    // tiling and the template keeps ≥ TILED_MIN_SIDE cells.
    let radius_cells0 = params.search_radius_m.max(0.0) * kmg / r0;
    let mut plan: Vec<(usize, f64, usize)> = Vec::new(); // (scale index, s_merc, level)
    for (si, &s) in scale_list(params).iter().enumerate() {
        let s_merc = s * kmg;
        let m0 = img_w.min(img_h) as f64 * s_merc / r0;
        if m0 < MIN_SIDE {
            continue;
        }
        let big0 = img_w.max(img_h) as f64 * s_merc / r0;
        let at = |l: usize| (1u64 << l) as f64;
        let mut level = 0usize;
        while level < 8
            && (m0 / at(level + 1) >= target_side
                || (m0 / at(level + 1) >= TILED_MIN_SIDE
                    && next_fast_len(((2.0 * radius_cells0 + big0) / at(level)).ceil() as usize + 4) > FFT_CAP))
        {
            level += 1;
        }
        plan.push((si, s_merc, level));
    }
    if plan.is_empty() {
        return Vec::new();
    }
    let n_levels = plan.iter().map(|p| p.2).max().unwrap() + 1;
    let mut pyramid = vec![dem.clone()];
    for _ in 1..n_levels {
        let next = pyramid.last().unwrap().downsample2();
        if next.w() < 4 || next.h() < 4 {
            break;
        }
        pyramid.push(next);
    }
    for p in plan.iter_mut() {
        p.2 = p.2.min(pyramid.len() - 1);
    }
    let fpyr = FieldPyramid::new(field);
    let ppc = field.px_per_cell;
    let total = (plan.len() + pyramid.len()) * rots.len();
    let done = AtomicUsize::new(0);
    let tick = || {
        let d = done.fetch_add(1, Ordering::Relaxed) + 1;
        progress((d as f32 / total as f32).min(1.0));
    };
    let radius_m = params.search_radius_m.max(0.0) * kmg;
    let mut peaks = Vec::new();
    for (lvl, ldem) in pyramid.iter().enumerate() {
        let lplan: Vec<&(usize, f64, usize)> = plan.iter().filter(|p| p.2 == lvl).collect();
        if lplan.is_empty() {
            (0..rots.len()).for_each(|_| tick());
            continue;
        }
        if cancel() {
            return Vec::new();
        }
        let res = ldem.res;
        let templates: Vec<(usize, Template)> = lplan
            .par_iter()
            .filter_map(|&&(si, s_merc, _)| build_template(&fpyr, ppc, img_w, img_h, s_merc, res).map(|t| (si, t)))
            .collect();
        if templates.is_empty() {
            continue;
        }
        let max_w = templates.iter().map(|t| t.1.w).max().unwrap();
        let max_h = templates.iter().map(|t| t.1.h).max().unwrap();
        let rc = radius_m / res;
        // Frame: every placement whose map centre lies in the search disc.
        let fw = (2.0 * rc).ceil() as usize + max_w + 4;
        let fh = (2.0 * rc).ceil() as usize + max_h + 4;
        let gc = ((params.center_merc.0 - ldem.ox) / res, (ldem.oy - params.center_merc.1) / res);
        // Grid window covering every rotated frame (or just the frame).
        let (hx, hy) = if any_rot {
            let d = ((fw * fw + fh * fh) as f64).sqrt() / 2.0 + 3.0;
            (d, d)
        } else {
            (fw as f64 / 2.0 + 3.0, fh as f64 / 2.0 + 3.0)
        };
        let gx0 = (gc.0 - hx).floor() as i64;
        let gy0 = (gc.1 - hy).floor() as i64;
        let gw = (2.0 * hx).ceil() as usize + 2;
        let gh = (2.0 * hy).ceil() as usize + 2;
        // Skip if the window misses the mosaic entirely.
        if gx0 + gw as i64 <= 0 || gy0 + gh as i64 <= 0 || gx0 >= ldem.w() as i64 || gy0 >= ldem.h() as i64 {
            continue;
        }
        let grid = grid_field(ldem, gx0, gy0, gw, gh);
        let rad_cells = DISTINCT_M * kmg / res;
        for (ri, &rot) in rots.iter().enumerate() {
            if cancel() {
                return Vec::new();
            }
            let frame = build_frame(&grid, gc, rot.to_radians(), fw, fh, (max_w, max_h));
            tick();
            let fpeaks: Vec<Peak> = templates
                .par_iter()
                .flat_map_iter(|(si, t)| {
                    if cancel() {
                        return Vec::new();
                    }
                    let (pw, ph, ncc) = correlate(t, &frame, rc);
                    let (mu, sd) = surface_stats(&ncc);
                    let out: Vec<Peak> = find_peaks(&ncc, pw, ph, PEAKS_PER_TEMPLATE, rad_cells)
                        .into_iter()
                        .map(|(score, fx, fy)| {
                            let (lx, ly) = frame.to_level(fx + t.cx, fy + t.cy);
                            Peak {
                                score,
                                surface_mean: mu,
                                surface_std: sd,
                                mx: ldem.ox + lx * res,
                                my: ldem.oy - ly * res,
                                s_merc: t.s_merc,
                                rot_deg: rot,
                                level: lvl,
                                tw: t.w,
                                th: t.h,
                                scale_idx: *si,
                                rot_idx: ri,
                            }
                        })
                        .collect();
                    tick();
                    out
                })
                .collect();
            peaks.extend(fpeaks);
        }
    }
    if cancel() {
        return Vec::new();
    }
    peaks
}

fn ground_dist(a: (f64, f64), b: (f64, f64)) -> f64 {
    let (lat, _) = merc_to_latlon(0.5 * (a.0 + b.0), 0.5 * (a.1 + b.1));
    ((a.0 - b.0).powi(2) + (a.1 - b.1).powi(2)).sqrt() * lat.to_radians().cos()
}

/// Distinct candidates (≥ `DISTINCT_M` apart), most significant first, with
/// peak ratios and a parabolic scale refinement from the neighbouring scales.
///
/// Peaks are ranked by significance z = (NCC − mean)/std of their own
/// template's correlation surface rather than by raw NCC: small templates
/// have noisy surfaces whose maxima reach high NCC by chance, and z makes
/// templates of different sizes and DEM levels comparable.
fn select_candidates(peaks: &[Peak], img_w: usize, img_h: usize, params: &SearchParams) -> Vec<Candidate> {
    let mut order: Vec<usize> = (0..peaks.len()).collect();
    order.sort_by(|&a, &b| peaks[b].z().partial_cmp(&peaks[a].z()).unwrap_or(std::cmp::Ordering::Equal));
    let mut chosen: Vec<usize> = Vec::new();
    for &i in &order {
        if chosen.len() >= params.top_k.max(1) {
            break;
        }
        let p = (peaks[i].mx, peaks[i].my);
        if chosen.iter().all(|&j| ground_dist(p, (peaks[j].mx, peaks[j].my)) >= DISTINCT_M) {
            chosen.push(i);
        }
    }
    chosen
        .into_iter()
        .map(|i| {
            let pk = &peaks[i];
            let here = (pk.mx, pk.my);
            let other =
                order.iter().map(|&j| &peaks[j]).find(|q| ground_dist(here, (q.mx, q.my)) >= DISTINCT_M).map(|q| q.z());
            let peak_ratio = match other {
                Some(o) => pk.z().max(0.0) / o.max(MIN_RATIO_DENOM),
                None => 1.0,
            };
            // Parabolic refinement of log-scale from the same location at the
            // neighbouring scales (same rotation).
            let near = |scale_idx: usize| -> Option<f32> {
                peaks
                    .iter()
                    .filter(|q| {
                        q.scale_idx == scale_idx
                            && q.rot_idx == pk.rot_idx
                            && ground_dist(here, (q.mx, q.my)) < 0.5 * DISTINCT_M
                    })
                    .map(|q| q.score)
                    .fold(None, |a: Option<f32>, s| Some(a.map_or(s, |a| a.max(s))))
            };
            let mut s_merc = pk.s_merc;
            if pk.scale_idx > 0 {
                if let (Some(a), Some(b)) = (near(pk.scale_idx - 1), near(pk.scale_idx + 1)) {
                    let c = pk.score;
                    let den = (a - 2.0 * c + b) as f64;
                    if den < 0.0 {
                        let off = (0.5 * (a - b) as f64 / den).clamp(-0.5, 0.5);
                        s_merc *= SCALE_STEP.powf(off);
                    }
                }
            }
            let transform = Affine::pixel_to_merc_similarity(
                s_merc,
                pk.rot_deg.to_radians(),
                img_w as f64 / 2.0,
                img_h as f64 / 2.0,
                pk.mx,
                pk.my,
            );
            let (lat, _) = merc_to_latlon(pk.mx, pk.my);
            Candidate {
                transform,
                score: pk.score,
                peak_ratio,
                scale_m_per_px: s_merc * lat.to_radians().cos(),
                rotation_deg: pk.rot_deg.rem_euclid(360.0),
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scales_are_geometric() {
        let p = SearchParams { scale_min: 1.0, scale_max: 2.0, ..Default::default() };
        let s = scale_list(&p);
        assert!((s[0] - 1.0).abs() < 1e-12 && (s[s.len() - 1] - 2.0).abs() < 1e-12);
        for w in s.windows(2) {
            assert!(w[1] / w[0] <= SCALE_STEP + 1e-9 && w[1] / w[0] > 1.03);
        }
        assert_eq!(rotation_sweep(10.0).len(), 36);
        assert_eq!(rotation_sweep(90.0), vec![0.0, 90.0, 180.0, 270.0]);
    }

    #[test]
    fn area_resample_exact_geometry() {
        // A single bright column at x = 50..51 in a 100-wide field lands at
        // template x = 50·k for k = 0.1.
        let (w, h) = (100usize, 40usize);
        let mut re = vec![0.0f32; w * h];
        for y in 0..h {
            re[y * w + 50] = 1.0;
        }
        let im = vec![0.0f32; w * h];
        let (nw, nh, r, _) = area_resample(&re, &im, w, h, 0.1);
        assert_eq!((nw, nh), (10, 4));
        let col: Vec<f32> = (0..nw).map(|x| r[x]).collect();
        assert!((col[5] - 0.1).abs() < 1e-6, "{col:?}");
        assert!(col.iter().enumerate().all(|(i, &v)| i == 5 || v == 0.0));
        // Mass is preserved (scaled by k²).
        let total: f32 = r.iter().sum();
        assert!((total - 40.0 * 0.01).abs() < 1e-4);
    }

    #[test]
    fn dem_box_covers_radius_and_map() {
        let p = SearchParams {
            center_merc: crate::geo::latlon_to_merc(-25.85, 27.4),
            search_radius_m: 20_000.0,
            scale_min: 0.5,
            scale_max: 10.0,
            ..Default::default()
        };
        let b = required_dem_box(&p, 3507, 4960);
        let k = 1.0 / (-25.85f64).to_radians().cos();
        assert!(b.width() / 2.0 >= (20_000.0 + 3507.0 * 5.0) * k);
        assert!(b.height() / 2.0 >= (20_000.0 + 4960.0 * 5.0) * k);
        assert!(b.height() / 2.0 <= MAX_BOX_HALF_M * k + 1.0);
    }

    #[test]
    fn frame_rotation_maps_back() {
        // A frame rotated by θ maps its reference point to the GPS and moves
        // "map right" (+u) along bearing 90° + θ.
        let g = GridField { x0: 0, y0: 0, w: 4, h: 4, d: vec![C32::new(0.0, 0.0); 16], valid: vec![false; 16] };
        let th = 30f64.to_radians();
        let f = build_frame(&g, (100.0, 200.0), th, 40, 30, (8, 8));
        let (x, y) = f.to_level(f.fc.0, f.fc.1);
        assert!((x - 100.0).abs() < 1e-9 && (y - 200.0).abs() < 1e-9);
        let (x, y) = f.to_level(f.fc.0 + 10.0, f.fc.1);
        // Level y points south: +u maps to east·cosθ + south·sinθ.
        assert!((x - 100.0 - 10.0 * th.cos()).abs() < 1e-9 && (y - 200.0 - 10.0 * th.sin()).abs() < 1e-9);
    }

    /// A rotation sweep finds a map rotated 30° on synthetic terrain: the
    /// position, the scale and the rotation.
    #[test]
    fn rotation_sweep_finds_rotated_map() {
        use crate::dem::{DemStore, TILE_SIZE};
        // Non-periodic terrain: two octaves of blurred noise on a 75 m grid
        // (deterministic LCG), bilinearly interpolated.
        let centre = (3.0e6, -2.9e6);
        let mut seed = 12345u64;
        let mut rnd = || {
            seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
            (seed >> 11) as f32 / (1u64 << 53) as f32 - 0.5
        };
        let (gn, cell) = (420usize, 75.0f64);
        let mut grid = vec![0.0f32; gn * gn];
        for (sigma, amp) in [(12.0, 300.0f32), (4.0, 40.0)] {
            let noise = GrayF32 { w: gn, h: gn, data: (0..gn * gn).map(|_| rnd()).collect() }.gaussian_blur(sigma);
            let sd = (noise.data.iter().map(|v| v * v).sum::<f32>() / noise.data.len() as f32).sqrt();
            for (g, v) in grid.iter_mut().zip(&noise.data) {
                *g += amp * v / sd;
            }
        }
        let terrain = GrayF32 { w: gn, h: gn, data: grid };
        let half = gn as f64 * cell / 2.0;
        let elev = |mx: f64, my: f64| -> f64 {
            let (x, y) = ((mx - centre.0 + half) / cell, (centre.1 + half - my) / cell);
            terrain.bilinear(x.clamp(0.0, gn as f64 - 1.0), y.clamp(0.0, gn as f64 - 1.0)).unwrap_or(0.0) as f64
        };
        let z = 12u8;
        let b = MercBox::around(centre.0, centre.1, 13_000.0);
        let mut store = DemStore::new();
        for t in crate::dem::tiles_covering(z, &b) {
            let tb = crate::dem::tile_bounds(t);
            let res = crate::dem::resolution(z);
            let e: Vec<f32> = (0..TILE_SIZE * TILE_SIZE)
                .into_par_iter()
                .map(|i| {
                    let (r, c) = (i / TILE_SIZE, i % TILE_SIZE);
                    elev(tb.min_x + (c as f64 + 0.5) * res, tb.max_y - (r as f64 + 0.5) * res) as f32
                })
                .collect();
            store.add_elevations(t, e);
        }
        let dem = store.mosaic(z, &b);
        // The "map": a mask of 10 m contours, rotated 30°, 3.6 m/px ground.
        let (lat, _) = merc_to_latlon(centre.0, centre.1);
        let lc = lat.to_radians().cos();
        let (w, h) = (900usize, 1200usize);
        let truth_center = (centre.0 + 2500.0, centre.1 - 1500.0);
        let tr = Affine::pixel_to_merc_similarity(
            3.6 / lc,
            30f64.to_radians(),
            450.0,
            600.0,
            truth_center.0,
            truth_center.1,
        );
        let lvl: Vec<i64> = (0..w * h)
            .into_par_iter()
            .map(|i| {
                let (mx, my) = tr.apply((i % w) as f64 + 0.5, (i / w) as f64 + 0.5);
                (elev(mx, my) / 10.0).floor() as i64
            })
            .collect();
        let mut mask = Mask::new(w, h);
        for y in 0..h - 1 {
            for x in 0..w - 1 {
                let l = lvl[y * w + x];
                if l != lvl[y * w + x + 1] || l != lvl[(y + 1) * w + x] {
                    mask.set(x, y, true);
                }
            }
        }
        let field = map_field(&mask);
        let params = SearchParams {
            center_merc: centre,
            search_radius_m: 6000.0,
            scale_min: 3.0,
            scale_max: 4.4,
            rotations_deg: rotation_sweep(10.0),
            top_k: 2,
        };
        let c = coarse_search(&field, &dem, w, h, &params, &|| false, &|_| {});
        let got = c[0].center_merc(w, h);
        let err = ((got.0 - truth_center.0).powi(2) + (got.1 - truth_center.1).powi(2)).sqrt() * lc;
        assert!(err < 100.0, "{err} {:?}", c[0]);
        let dr = (c[0].rotation_deg - 30.0 + 540.0).rem_euclid(360.0) - 180.0;
        assert!(dr.abs() <= 3.0, "{:?}", c[0]);
        assert!((c[0].scale_m_per_px / 3.6 - 1.0).abs() < 0.04, "{:?}", c[0]);
        assert!(c[0].peak_ratio > 1.3, "{:?}", c);
    }
}
