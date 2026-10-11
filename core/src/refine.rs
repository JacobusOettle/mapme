//! Fine alignment: make every contour segment have constant DEM elevation.
//!
//! Each contour segment traced on the map is a line of constant elevation.
//! Under the right pixel → Mercator transform T, the DEM elevations h(T(p))
//! sampled along a segment are therefore constant (up to DEM noise). We
//! minimise Σ ρ(h(T(pᵢ)) − c_seg(i)) over T and the per-segment constants c
//! (eliminated in closed form: robust weighted segment means), with a soft-L1
//! loss ρ, by Levenberg–Marquardt. The Jacobian is analytic: the DEM gradient
//! times ∂T/∂params, minus its segment mean (variable projection).
//!
//! The solve runs coarse-to-fine on a smoothed DEM pyramid (after a small
//! translation grid on the coarsest level) so initial errors of several
//! hundred metres still converge: first a 4-parameter similarity, then a full
//! affine if that lowers the cost meaningfully and stays near-conformal.

use crate::dem::{DemMosaic, MercBox};
use crate::geo::Affine;
use crate::image::GrayF32;
use crate::search::Candidate;
use crate::skeleton::ContourSamples;
use rayon::prelude::*;

#[derive(Debug, Clone)]
pub struct RefineResult {
    /// Pixel → Mercator transform after refinement.
    pub transform: Affine,
    /// Robust mean absolute within-segment elevation residual (metres):
    /// mean of min(|r|, `RESIDUAL_CLIP_M`).
    pub cost_m: f64,
    /// Median over segments of the elevation standard deviation (metres).
    pub median_seg_std_m: f64,
    /// Fraction of valid samples with |residual| < `INLIER_M`.
    pub inlier_fraction: f64,
    /// Fraction of samples that landed on valid DEM.
    pub dem_coverage: f64,
    pub iterations: usize,
    pub converged: bool,
    /// 1 − Σ clipped r² / Σ (h − mean h)²: how much of the elevation variation
    /// along the contours the per-segment constants explain (≈ 1 when right).
    pub explained_variance: f64,
    /// Standard deviation of the sampled elevations (terrain relief, metres).
    pub relief_m: f64,
    /// Valid samples / segments used.
    pub n_samples: usize,
    pub n_segments: usize,
    /// True if the final transform is a full affine (else a similarity).
    pub affine: bool,
    /// Estimated contour interval (metres): median elevation difference
    /// between neighbouring contour lines (across, not along, the lines).
    pub contour_interval_m: f64,
    /// `contour_interval_m` / robust residual sigma (1.4826·median|r|).
    /// Adjacent contours must differ by one interval, clearly above the DEM
    /// noise; a wrong fit squeezes many lines into little relief (≪ 1).
    pub interval_snr: f64,
}

/// Soft-L1 loss scale (metres) on the finest level (doubles per level).
const LOSS_SCALE_M: f64 = 3.0;
/// Samples within this (metres) of their segment's elevation are inliers.
pub const INLIER_M: f64 = 5.0;
/// Residual clip for `cost_m` and `explained_variance`.
const RESIDUAL_CLIP_M: f64 = 12.0;
/// Missing-DEM samples cost as if their residual were this large.
const MISSING_PENALTY_M: f64 = 10.0;
/// DEM smoothing (pixels of each pyramid level).
const DEM_SMOOTH_SIGMA: f64 = 0.7;
/// The similarity scale may move at most this far (log units) from the start.
const MAX_LOG_SCALE_CHANGE: f64 = 0.15;
/// Affine is kept only if it lowers the cost by this fraction…
const AFFINE_MIN_GAIN: f64 = 0.03;
/// …and its two axis scales differ by less than this ratio.
const AFFINE_MAX_ANISOTROPY: f64 = 1.08;
/// Segments need this many valid samples to count.
const MIN_SEG_SAMPLES: usize = 4;
/// The pyramid goes down to cells of about this size (Mercator metres).
const COARSEST_RES_M: f64 = 120.0;
/// Translation grid (Mercator metres) scanned on the coarsest level.
const GRID_HALF_M: f64 = 450.0;
const GRID_STEP_M: f64 = 150.0;
/// Best grid starts polished by LM on the coarsest level.
const GRID_STARTS: usize = 3;
/// Samples used on the coarse levels / the finest level: at most this many
/// (strided subsets).
const MAX_SAMPLES_COARSE: usize = 20_000;
const MAX_SAMPLES_FINE: usize = 150_000;

/// DEM prepared for refinement: NaN-aware smoothed pyramid (finest first).
pub struct PreparedDem {
    pub levels: Vec<DemMosaic>,
}

impl PreparedDem {
    /// Smooth the mosaic (σ = 0.7 px, NaN-aware) and build coarser levels
    /// (2× each) down to ~120 m cells. Do this once per mosaic.
    pub fn new(dem: &DemMosaic) -> PreparedDem {
        let mut levels = vec![smooth_nan(dem, DEM_SMOOTH_SIGMA)];
        while levels.last().unwrap().res * 2.0 <= COARSEST_RES_M * 1.5 {
            let d = levels.last().unwrap().downsample2();
            if d.w() < 8 || d.h() < 8 {
                break;
            }
            levels.push(smooth_nan(&d, DEM_SMOOTH_SIGMA));
        }
        PreparedDem { levels }
    }

    /// Crop the mosaic to `b` first (cheaper smoothing for big mosaics).
    pub fn new_cropped(dem: &DemMosaic, b: &MercBox) -> PreparedDem {
        PreparedDem::new(&crop_mosaic(dem, b))
    }
}

/// Mercator box a fine (zoom 13) DEM should cover to refine from `init`:
/// the map footprint plus a margin for initial errors and scale changes.
pub fn refine_dem_box(init: &Affine, img_w: usize, img_h: usize) -> MercBox {
    let (w, h) = (img_w as f64, img_h as f64);
    let pts = [init.apply(0.0, 0.0), init.apply(w, 0.0), init.apply(w, h), init.apply(0.0, h)];
    let b = MercBox::from_points(&pts);
    let margin = (0.2 * b.width().max(b.height())).max(1500.0);
    b.expand(margin)
}

fn crop_mosaic(dem: &DemMosaic, b: &MercBox) -> DemMosaic {
    let (c0, r0) = dem.merc_to_px(b.min_x, b.max_y);
    let (c1, r1) = dem.merc_to_px(b.max_x, b.min_y);
    let c0 = (c0.floor() as i64 - 2).clamp(0, dem.w() as i64 - 1) as usize;
    let r0 = (r0.floor() as i64 - 2).clamp(0, dem.h() as i64 - 1) as usize;
    let c1 = (c1.ceil() as i64 + 3).clamp(c0 as i64 + 1, dem.w() as i64) as usize;
    let r1 = (r1.ceil() as i64 + 3).clamp(r0 as i64 + 1, dem.h() as i64) as usize;
    let (w, h) = (c1 - c0, r1 - r0);
    let mut data = vec![0.0f32; w * h];
    for y in 0..h {
        data[y * w..(y + 1) * w].copy_from_slice(&dem.grid.data[(r0 + y) * dem.w() + c0..(r0 + y) * dem.w() + c1]);
    }
    DemMosaic {
        res: dem.res,
        ox: dem.ox + c0 as f64 * dem.res,
        oy: dem.oy - r0 as f64 * dem.res,
        grid: GrayF32 { w, h, data },
    }
}

/// Gaussian smoothing that ignores NaN cells (NaN where < half the weight is valid).
fn smooth_nan(dem: &DemMosaic, sigma: f64) -> DemMosaic {
    let g = &dem.grid;
    let vals = GrayF32 { w: g.w, h: g.h, data: g.data.iter().map(|&v| if v.is_finite() { v } else { 0.0 }).collect() };
    let wts = GrayF32 { w: g.w, h: g.h, data: g.data.iter().map(|&v| v.is_finite() as u8 as f32).collect() };
    let sv = vals.gaussian_blur(sigma);
    let sw = wts.gaussian_blur(sigma);
    let data = sv.data.iter().zip(&sw.data).map(|(&v, &w)| if w >= 0.5 { v / w } else { f32::NAN }).collect();
    DemMosaic { res: dem.res, ox: dem.ox, oy: dem.oy, grid: GrayF32 { w: g.w, h: g.h, data } }
}

/// Transform parameterisation about the sample centroid (cx, cy).
#[derive(Debug, Clone, Copy, PartialEq)]
enum Model {
    /// [ln s, θ, tx, ty] as in `Affine::pixel_to_merc_similarity`.
    Similarity,
    /// [a, b, d, e, tx, ty].
    Affine,
}

impl Model {
    fn n(self) -> usize {
        match self {
            Model::Similarity => 4,
            Model::Affine => 6,
        }
    }
}

#[derive(Debug, Clone, Copy)]
struct Params {
    model: Model,
    p: [f64; 6],
    cx: f64,
    cy: f64,
}

impl Params {
    fn from_affine(t: &Affine, model: Model, cx: f64, cy: f64) -> Params {
        let (tx, ty) = t.apply(cx, cy);
        let p = match model {
            Model::Similarity => {
                // Closest similarity: a = s cos θ, b = −s sin θ, d = −s sin θ, e = −s cos θ.
                let sc = 0.5 * (t.a - t.e);
                let ss = -0.5 * (t.b + t.d);
                [(sc * sc + ss * ss).sqrt().max(1e-12).ln(), ss.atan2(sc), tx, ty, 0.0, 0.0]
            }
            Model::Affine => [t.a, t.b, t.d, t.e, tx, ty],
        };
        Params { model, p, cx, cy }
    }

    fn to_affine(self) -> Affine {
        match self.model {
            Model::Similarity => {
                Affine::pixel_to_merc_similarity(self.p[0].exp(), self.p[1], self.cx, self.cy, self.p[2], self.p[3])
            }
            Model::Affine => {
                let [a, b, d, e, tx, ty] = self.p;
                Affine { a, b, c: tx - a * self.cx - b * self.cy, d, e, f: ty - d * self.cx - e * self.cy }
            }
        }
    }
}

#[inline]
fn soft_l1(r: f64, c: f64) -> f64 {
    let z = r / c;
    2.0 * c * c * ((1.0 + z * z).sqrt() - 1.0)
}

#[inline]
fn soft_l1_weight(r: f64, c: f64) -> f64 {
    1.0 / (1.0 + (r / c).powi(2)).sqrt()
}

/// One evaluation: robust cost and (optionally) normal equations.
struct Eval {
    cost: f64,
    ata: [[f64; 6]; 6],
    atb: [f64; 6],
    /// Per-sample residual and elevation (NaN where unused).
    resid: Vec<f64>,
    elev: Vec<f64>,
}

/// Elevation, gradient and Mercator position of every sample (NaN = off DEM).
fn sample_all(d: &Data, dem: &DemMosaic, t: &Affine) -> Vec<[f64; 5]> {
    (0..d.xs.len())
        .into_par_iter()
        .map(|i| {
            let (mx, my) = t.apply(d.xs[i], d.ys[i]);
            match dem.sample_with_grad(mx, my) {
                Some((h, gx, gy)) => [h as f64, gx as f64, gy as f64, mx, my],
                None => [f64::NAN; 5],
            }
        })
        .collect()
}

/// Evaluate `par` on `dem` for all samples of `d`.
#[allow(clippy::needless_range_loop)]
fn evaluate_params(d: &Data, dem: &DemMosaic, par: &Params, c: f64, jac: bool) -> Eval {
    let t = par.to_affine();
    let n = d.xs.len();
    let np = par.model.n();
    let samp = sample_all(d, dem, &t);
    // Segments need enough valid samples.
    let mut cnt = vec![0usize; d.n_segs];
    for i in 0..n {
        if samp[i][0].is_finite() {
            cnt[d.seg[i] as usize] += 1;
        }
    }
    let used: Vec<bool> = (0..n).map(|i| samp[i][0].is_finite() && cnt[d.seg[i] as usize] >= MIN_SEG_SAMPLES).collect();
    // Robust segment means: plain mean, then one IRLS re-weighting.
    let seg_mean = |w: &dyn Fn(usize) -> f64| -> Vec<f64> {
        let mut sw = vec![0.0f64; d.n_segs];
        let mut sh = vec![0.0f64; d.n_segs];
        for i in 0..n {
            if used[i] {
                let (s, wi) = (d.seg[i] as usize, w(i));
                sw[s] += wi;
                sh[s] += wi * samp[i][0];
            }
        }
        sh.iter().zip(&sw).map(|(&a, &b)| if b > 0.0 { a / b } else { 0.0 }).collect()
    };
    let m0 = seg_mean(&|_| 1.0);
    let mean = seg_mean(&|i| soft_l1_weight(samp[i][0] - m0[d.seg[i] as usize], c));
    let mut resid = vec![f64::NAN; n];
    let mut elev = vec![f64::NAN; n];
    let mut w = vec![0.0f64; n];
    let mut cost = 0.0;
    let penalty = soft_l1(MISSING_PENALTY_M, c);
    for i in 0..n {
        if used[i] {
            let h = samp[i][0];
            let r = h - mean[d.seg[i] as usize];
            resid[i] = r;
            elev[i] = h;
            w[i] = soft_l1_weight(r, c);
            cost += soft_l1(r, c);
        } else {
            cost += penalty;
        }
    }
    let mut ata = [[0.0f64; 6]; 6];
    let mut atb = [0.0f64; 6];
    if jac {
        let row = |i: usize| -> [f64; 6] {
            let [_, gx, gy, mx, my] = samp[i];
            let p = &par.p;
            match par.model {
                Model::Similarity => {
                    let (dx, dy) = (mx - p[2], my - p[3]);
                    [gx * dx + gy * dy, gx * dy - gy * dx, gx, gy, 0.0, 0.0]
                }
                Model::Affine => {
                    let (x, y) = (d.xs[i] - par.cx, d.ys[i] - par.cy);
                    [gx * x, gx * y, gy * x, gy * y, gx, gy]
                }
            }
        };
        // Weighted segment means of the Jacobian rows (variable projection).
        let mut sj = vec![[0.0f64; 6]; d.n_segs];
        let mut sw = vec![0.0f64; d.n_segs];
        for i in 0..n {
            if used[i] {
                let (s, j) = (d.seg[i] as usize, row(i));
                sw[s] += w[i];
                for k in 0..np {
                    sj[s][k] += w[i] * j[k];
                }
            }
        }
        for v in sj.iter_mut().zip(&sw) {
            if *v.1 > 0.0 {
                for x in v.0.iter_mut() {
                    *x /= v.1;
                }
            }
        }
        for i in 0..n {
            if !used[i] {
                continue;
            }
            let s = d.seg[i] as usize;
            let mut j = row(i);
            for k in 0..np {
                j[k] -= sj[s][k];
            }
            let (wi, r) = (w[i], resid[i]);
            for a in 0..np {
                atb[a] += wi * j[a] * r;
                for b in a..np {
                    ata[a][b] += wi * j[a] * j[b];
                }
            }
        }
        for a in 0..np {
            for b in 0..a {
                ata[a][b] = ata[b][a];
            }
        }
    }
    Eval { cost, ata, atb, resid, elev }
}

/// Samples in a refinement-friendly layout.
struct Data {
    xs: Vec<f64>,
    ys: Vec<f64>,
    seg: Vec<u32>,
    n_segs: usize,
    cx: f64,
    cy: f64,
}

impl Data {
    fn new(s: &ContourSamples) -> Data {
        let n = s.pts.len().max(1) as f64;
        let cx = s.pts.iter().map(|p| p[0] as f64).sum::<f64>() / n;
        let cy = s.pts.iter().map(|p| p[1] as f64).sum::<f64>() / n;
        Data {
            xs: s.pts.iter().map(|p| p[0] as f64).collect(),
            ys: s.pts.iter().map(|p| p[1] as f64).collect(),
            seg: s.seg.clone(),
            n_segs: s.n_segs,
            cx,
            cy,
        }
    }

    /// Every `stride`-th sample (same centroid, so parameters carry over).
    fn subset(&self, stride: usize) -> Data {
        let idx = (0..self.xs.len()).step_by(stride.max(1));
        let (mut xs, mut ys, mut seg) = (Vec::new(), Vec::new(), Vec::new());
        for i in idx {
            xs.push(self.xs[i]);
            ys.push(self.ys[i]);
            seg.push(self.seg[i]);
        }
        Data { xs, ys, seg, n_segs: self.n_segs, cx: self.cx, cy: self.cy }
    }
}

/// Levenberg–Marquardt from `start`. Returns (params, cost, iterations, converged).
fn lm(
    d: &Data,
    dem: &DemMosaic,
    start: Params,
    c: f64,
    max_iter: usize,
    log_scale_bounds: (f64, f64),
) -> (Params, f64, usize, bool) {
    let np = start.model.n();
    let mut par = start;
    let mut cur = evaluate_params(d, dem, &par, c, true);
    let mut lambda = 1e-3;
    let mut converged = false;
    let mut it = 0;
    while it < max_iter {
        it += 1;
        let mut m: Vec<Vec<f64>> = (0..np)
            .map(|a| {
                let mut row = cur.ata[a][..np].to_vec();
                row[a] += lambda * cur.ata[a][a].max(1e-12) + 1e-12;
                row.push(-cur.atb[a]);
                row
            })
            .collect();
        let delta = match crate::geo::solve_dense(&mut m) {
            Some(x) => x,
            None => {
                lambda *= 10.0;
                if lambda > 1e8 {
                    break;
                }
                continue;
            }
        };
        let mut cand = par;
        for (p, dv) in cand.p.iter_mut().zip(&delta) {
            *p += dv;
        }
        if cand.model == Model::Similarity {
            cand.p[0] = cand.p[0].clamp(log_scale_bounds.0, log_scale_bounds.1);
        }
        let next = evaluate_params(d, dem, &cand, c, true);
        if next.cost < cur.cost {
            let gain = (cur.cost - next.cost) / cur.cost.max(1e-12);
            // Step size in Mercator metres at the map extent.
            let step_m = match par.model {
                Model::Similarity => delta[2].hypot(delta[3]) + 3000.0 * (delta[0].abs() + delta[1].abs()),
                Model::Affine => delta[4].hypot(delta[5]) + 1500.0 * delta[..4].iter().map(|v| v.abs()).sum::<f64>(),
            };
            par = cand;
            cur = next;
            lambda = (lambda / 3.0).max(1e-9);
            if gain < 1e-5 || step_m < 0.05 {
                converged = true;
                break;
            }
        } else {
            lambda *= 4.0;
            if lambda > 1e8 {
                converged = true;
                break;
            }
        }
    }
    (par, cur.cost, it, converged)
}

/// Levenberg–Marquardt on similarity then affine parameters, starting at `init`.
/// `dem` is a fine mosaic (zoom 13) covering the map footprint plus margin.
pub fn refine(samples: &ContourSamples, dem: &DemMosaic, init: &Affine) -> RefineResult {
    refine_prepared(samples, &PreparedDem::new(dem), init)
}

/// `refine` with a DEM prepared once (reuse it across candidates).
pub fn refine_prepared(samples: &ContourSamples, dem: &PreparedDem, init: &Affine) -> RefineResult {
    let mut full = Data::new(samples);
    if full.xs.len() > MAX_SAMPLES_FINE {
        full = full.subset(full.xs.len().div_ceil(MAX_SAMPLES_FINE));
    }
    if samples.pts.is_empty() || dem.levels.is_empty() {
        return metrics(&full, dem, init, 0, false, false);
    }
    let start = Params::from_affine(init, Model::Similarity, full.cx, full.cy);
    let bounds = (start.p[0] - MAX_LOG_SCALE_CHANGE, start.p[0] + MAX_LOG_SCALE_CHANGE);
    let n_levels = dem.levels.len();
    let coarse_stride = full.xs.len().div_ceil(MAX_SAMPLES_COARSE).max(1);
    let level_data = |lvl: usize| -> Data {
        // Finest level uses everything; coarser ones a strided subset.
        let stride = if lvl == 0 { 1 } else { coarse_stride.min(1 << lvl) };
        full.subset(stride)
    };
    let loss = |lvl: usize| LOSS_SCALE_M * (1u32 << lvl) as f64;
    let mut iterations = 0;
    // Coarsest level: cost on a translation grid, then LM from the best few.
    let lvl = n_levels - 1;
    let dc = level_data(lvl);
    let mut grid: Vec<(f64, Params)> = Vec::new();
    let n_steps = (GRID_HALF_M / GRID_STEP_M).round() as i32;
    for iy in -n_steps..=n_steps {
        for ix in -n_steps..=n_steps {
            let mut s = start;
            s.p[2] += ix as f64 * GRID_STEP_M;
            s.p[3] += iy as f64 * GRID_STEP_M;
            grid.push((evaluate_params(&dc, &dem.levels[lvl], &s, loss(lvl), false).cost, s));
        }
    }
    grid.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap_or(std::cmp::Ordering::Equal));
    let mut best: Option<(Params, f64)> = None;
    for (_, s) in grid.iter().take(GRID_STARTS) {
        let (p, cost, it, _) = lm(&dc, &dem.levels[lvl], *s, loss(lvl), 15, bounds);
        iterations += it;
        if best.as_ref().is_none_or(|b| cost < b.1) {
            best = Some((p, cost));
        }
    }
    let mut par = best.map_or(start, |b| b.0);
    // Finer levels.
    let mut converged = n_levels == 1;
    let fine_data = level_data(0);
    for lvl in (0..n_levels - 1).rev() {
        let dl = if lvl == 0 { None } else { Some(level_data(lvl)) };
        let (p, _, it, conv) = lm(dl.as_ref().unwrap_or(&fine_data), &dem.levels[lvl], par, loss(lvl), 25, bounds);
        par = p;
        iterations += it;
        converged = conv;
    }
    // Full affine on the finest level, kept only if clearly better and near-conformal.
    let fine = &dem.levels[0];
    let sim_cost = evaluate_params(&fine_data, fine, &par, LOSS_SCALE_M, false).cost;
    let aff_start = Params::from_affine(&par.to_affine(), Model::Affine, full.cx, full.cy);
    let (aff, aff_cost, it, aff_conv) = lm(&fine_data, fine, aff_start, LOSS_SCALE_M, 25, bounds);
    iterations += it;
    let ta = aff.to_affine();
    let mut out = par.to_affine();
    let mut is_affine = false;
    if aff_cost < sim_cost * (1.0 - AFFINE_MIN_GAIN)
        && anisotropy(&ta) < AFFINE_MAX_ANISOTROPY
        && (ta.mean_scale() / out.mean_scale()).ln().abs() < 0.05
    {
        out = ta;
        is_affine = true;
        converged = aff_conv;
    }
    metrics(&full, dem, &out, iterations, converged, is_affine)
}

/// Ratio of the larger to the smaller singular value of the linear part.
fn anisotropy(t: &Affine) -> f64 {
    let (a, b, c, d) = (t.a, t.b, t.d, t.e);
    let s1 = a * a + b * b + c * c + d * d;
    let det = (a * d - b * c).abs();
    let disc = (s1 * s1 - 4.0 * det * det).max(0.0).sqrt();
    let smax = ((s1 + disc) / 2.0).sqrt();
    let smin = ((s1 - disc) / 2.0).max(0.0).sqrt();
    if smin > 0.0 {
        smax / smin
    } else {
        f64::INFINITY
    }
}

/// Evaluate a transform without optimising (used to verify grid results).
pub fn evaluate(samples: &ContourSamples, dem: &DemMosaic, t: &Affine) -> RefineResult {
    evaluate_prepared(samples, &PreparedDem::new(dem), t)
}

/// `evaluate` with a prepared DEM.
pub fn evaluate_prepared(samples: &ContourSamples, dem: &PreparedDem, t: &Affine) -> RefineResult {
    metrics(&Data::new(samples), dem, t, 0, false, false)
}

fn metrics(d: &Data, dem: &PreparedDem, t: &Affine, iterations: usize, converged: bool, affine: bool) -> RefineResult {
    let empty = RefineResult {
        transform: *t,
        cost_m: f64::INFINITY,
        median_seg_std_m: f64::INFINITY,
        inlier_fraction: 0.0,
        dem_coverage: 0.0,
        iterations,
        converged,
        explained_variance: 0.0,
        relief_m: 0.0,
        n_samples: 0,
        n_segments: 0,
        affine,
        contour_interval_m: 0.0,
        interval_snr: 0.0,
    };
    let Some(fine) = dem.levels.first() else {
        return empty;
    };
    if d.xs.is_empty() {
        return empty;
    }
    let par = Params::from_affine(t, Model::Affine, d.cx, d.cy);
    let e = evaluate_params(d, fine, &par, LOSS_SCALE_M, false);
    let n = d.xs.len();
    let on_dem = (0..n)
        .filter(|&i| {
            let (mx, my) = t.apply(d.xs[i], d.ys[i]);
            fine.sample(mx, my).is_finite()
        })
        .count();
    let used: Vec<usize> = (0..n).filter(|&i| e.resid[i].is_finite()).collect();
    if used.is_empty() {
        return RefineResult { dem_coverage: on_dem as f64 / n as f64, ..empty };
    }
    let nu = used.len() as f64;
    let cost_m = used.iter().map(|&i| e.resid[i].abs().min(RESIDUAL_CLIP_M)).sum::<f64>() / nu;
    let inliers = used.iter().filter(|&&i| e.resid[i].abs() < INLIER_M).count() as f64 / nu;
    let mean_h = used.iter().map(|&i| e.elev[i]).sum::<f64>() / nu;
    let var_h = used.iter().map(|&i| (e.elev[i] - mean_h).powi(2)).sum::<f64>() / nu;
    let var_r = used.iter().map(|&i| e.resid[i].abs().min(RESIDUAL_CLIP_M).powi(2)).sum::<f64>() / nu;
    let explained_variance = if var_h > 0.0 { 1.0 - var_r / var_h } else { 0.0 };
    // Per-segment standard deviations.
    let mut s1 = vec![0.0f64; d.n_segs];
    let mut s2 = vec![0.0f64; d.n_segs];
    let mut cnt = vec![0usize; d.n_segs];
    for &i in &used {
        let s = d.seg[i] as usize;
        s1[s] += e.elev[i];
        s2[s] += e.elev[i] * e.elev[i];
        cnt[s] += 1;
    }
    let mut stds: Vec<f64> = (0..d.n_segs)
        .filter(|&s| cnt[s] >= MIN_SEG_SAMPLES)
        .map(|s| {
            let m = s1[s] / cnt[s] as f64;
            (s2[s] / cnt[s] as f64 - m * m).max(0.0).sqrt()
        })
        .collect();
    let n_segments = stds.len();
    let median_seg_std_m = median(&mut stds).unwrap_or(f64::INFINITY);
    let mut abs_r: Vec<f64> = used.iter().map(|&i| e.resid[i].abs()).collect();
    let sigma_r = 1.4826 * median(&mut abs_r).unwrap_or(f64::INFINITY);
    let contour_interval_m = interval_estimate(d, &e, &used);
    let interval_snr = if sigma_r > 0.0 { contour_interval_m / sigma_r } else { 0.0 };
    RefineResult {
        transform: *t,
        cost_m,
        median_seg_std_m,
        inlier_fraction: inliers,
        dem_coverage: on_dem as f64 / n as f64,
        iterations,
        converged,
        explained_variance,
        relief_m: var_h.sqrt(),
        n_samples: used.len(),
        n_segments,
        affine,
        contour_interval_m,
        interval_snr,
    }
}

fn median(v: &mut [f64]) -> Option<f64> {
    if v.is_empty() {
        return None;
    }
    let k = v.len() / 2;
    Some(*v.select_nth_unstable_by(k, |a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal)).1)
}

/// Queries used for the contour-interval estimate.
const INTERVAL_QUERIES: usize = 4000;

/// Median elevation difference between each sampled contour and the nearest
/// other contour across it (within ~1/60 of the image diagonal, at > 60° to
/// the line so pieces of the same line are not taken as neighbours).
fn interval_estimate(d: &Data, e: &Eval, used: &[usize]) -> f64 {
    use std::collections::HashMap;
    if used.len() < 10 {
        return 0.0;
    }
    let (mut xmin, mut xmax, mut ymin, mut ymax) = (f64::MAX, f64::MIN, f64::MAX, f64::MIN);
    for &i in used {
        xmin = xmin.min(d.xs[i]);
        xmax = xmax.max(d.xs[i]);
        ymin = ymin.min(d.ys[i]);
        ymax = ymax.max(d.ys[i]);
    }
    let radius = ((xmax - xmin).hypot(ymax - ymin) / 60.0).max(10.0);
    let key = |i: usize| ((d.xs[i] / radius).floor() as i64, (d.ys[i] / radius).floor() as i64);
    let mut grid: HashMap<(i64, i64), Vec<usize>> = HashMap::new();
    for &i in used {
        grid.entry(key(i)).or_default().push(i);
    }
    let seg_mean = |i: usize| e.elev[i] - e.resid[i];
    let stride = used.len().div_ceil(INTERVAL_QUERIES).max(1);
    let mut diffs: Vec<f64> = used
        .par_iter()
        .step_by(stride)
        .filter_map(|&i| {
            let s = d.seg[i];
            // Tangent from the neighbouring samples of the same segment.
            let prev = if i > 0 && d.seg[i - 1] == s { i - 1 } else { i };
            let next = if i + 1 < d.xs.len() && d.seg[i + 1] == s { i + 1 } else { i };
            let (tx, ty) = (d.xs[next] - d.xs[prev], d.ys[next] - d.ys[prev]);
            let tn = tx.hypot(ty);
            if tn == 0.0 {
                return None;
            }
            let (kx, ky) = key(i);
            let mut best: Option<(f64, usize)> = None;
            for gy in ky - 1..=ky + 1 {
                for gx in kx - 1..=kx + 1 {
                    for &j in grid.get(&(gx, gy)).map(|v| v.as_slice()).unwrap_or(&[]) {
                        if d.seg[j] == s {
                            continue;
                        }
                        let (vx, vy) = (d.xs[j] - d.xs[i], d.ys[j] - d.ys[i]);
                        let dist = vx.hypot(vy);
                        if dist > radius || dist == 0.0 || (vx * tx + vy * ty).abs() > 0.5 * dist * tn {
                            continue;
                        }
                        if best.is_none_or(|b| dist < b.0) {
                            best = Some((dist, j));
                        }
                    }
                }
            }
            best.map(|(_, j)| (seg_mean(i) - seg_mean(j)).abs())
        })
        .collect();
    median(&mut diffs).unwrap_or(0.0)
}

/// Smooth ramp 0 → 1 between `lo` and `hi`.
fn ramp(x: f64, lo: f64, hi: f64) -> f64 {
    let t = ((x - lo) / (hi - lo)).clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

/// How well a refined transform explains the map's contours (0..1), from the
/// fine fit alone (no search evidence). Used to verify grid results too.
/// Every factor must hold (they multiply):
///
/// * explained variance: ≥ 0.97 → 1, ≤ 0.80 → 0 (right fits explain ~99.9 %
///   of the elevation variation along contours, wrong ones well under 90 %);
/// * median within-segment std: ≤ 4 m → 1, ≥ 12 m → 0 (DEM noise is ~2–3 m);
/// * contour-interval SNR (neighbouring lines must differ by one interval,
///   above the residual noise): ≥ 2.0 → 1, ≤ 1.0 → 0. Grootkloof (5 m
///   contours, SRTM) gives 2.8; wrong fits give < 0.8, because they squeeze
///   many lines into little relief or scatter them over unrelated terrain;
/// * a plausible estimated contour interval: ≥ 1.6 m → 1, ≤ 0.8 m → 0
///   (topographic maps use 1–50 m; wrong fits on smooth terrain imply ~0.5 m);
/// * DEM coverage: ≥ 0.8 → 1, ≤ 0.3 → 0;
/// * enough evidence: ≥ 20 segments and ≥ 30 m relief, else scaled down.
pub fn fit_confidence(r: &RefineResult) -> f64 {
    if r.n_samples == 0 || !r.cost_m.is_finite() {
        return 0.0;
    }
    let ev = ramp(r.explained_variance, 0.80, 0.97);
    let sd = 1.0 - ramp(r.median_seg_std_m, 4.0, 12.0);
    let snr = ramp(r.interval_snr, 1.0, 2.0);
    let interval = ramp(r.contour_interval_m, 0.8, 1.6);
    let cov = ramp(r.dem_coverage, 0.3, 0.8);
    let evidence = ramp(r.n_segments as f64, 5.0, 20.0) * ramp(r.relief_m, 10.0, 30.0);
    (ev * sd * snr * interval * cov * evidence).clamp(0.0, 1.0)
}

/// Overall confidence (0..1) that a contour match is right, combining the
/// coarse search's distinctiveness with the fine fit.
///
/// * search: peak_ratio (significance of the best location over the best
///   other location ≥ 2 km away) ≥ 1.6 → 1, ≤ 1.15 → 0; true matches score
///   ≈ 2, a map outside the search area ≈ 1.0–1.1;
/// * fit: `fit_confidence`.
///
/// The result is 0.35·search + 0.65·fit, but never more than fit + 0.1 or
/// 2·search + 0.1: both pieces of evidence must agree. Thresholds: ≥ 0.7 is
/// a confident match, ≤ 0.3 means "not found".
pub fn confidence(c: &Candidate, r: &RefineResult) -> f64 {
    combine(search_evidence(c), fit_confidence(r))
}

fn search_evidence(c: &Candidate) -> f64 {
    ramp(c.peak_ratio as f64, 1.15, 1.6)
}

fn combine(search: f64, fit: f64) -> f64 {
    (0.35 * search + 0.65 * fit).min(fit + 0.1).min(2.0 * search + 0.1).clamp(0.0, 1.0)
}

/// Pick the best of several refined candidates (e.g. the coarse top-k, each
/// refined on its own fine DEM) and return (index, confidence).
///
/// The winner is the best fit (ties go to the earlier, more significant
/// candidate). When its fit beats every other candidate's by a clear margin
/// (fit-confidence gap ≥ 0.7 → full, ≤ 0.3 → none), that counts as search
/// evidence too, so a right match in repetitive terrain (ambiguous coarse
/// peak ratio) is still accepted once the alternatives were refined and failed.
pub fn select_refined(results: &[(Candidate, RefineResult)]) -> Option<(usize, f64)> {
    if results.is_empty() {
        return None;
    }
    let fits: Vec<f64> = results.iter().map(|(_, r)| fit_confidence(r)).collect();
    let best = fits.iter().enumerate().fold(0, |b, (i, &f)| if f > fits[b] + 1e-9 { i } else { b });
    let mut search = search_evidence(&results[best].0);
    if results.len() > 1 {
        let second = fits.iter().enumerate().filter(|&(i, _)| i != best).map(|(_, &f)| f).fold(0.0, f64::max);
        search = search.max(ramp(fits[best] - second, 0.3, 0.7));
    }
    Some((best, combine(search, fits[best])))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn params_roundtrip() {
        let t = Affine::pixel_to_merc_similarity(2.1, 0.05, 1750.0, 2480.0, 3.05e6, -2.98e6);
        let p = Params::from_affine(&t, Model::Similarity, 1000.0, 2000.0);
        let back = p.to_affine();
        for (x, y) in [(0.0, 0.0), (3500.0, 4900.0)] {
            let (a, b) = (t.apply(x, y), back.apply(x, y));
            assert!((a.0 - b.0).abs() < 1e-6 && (a.1 - b.1).abs() < 1e-6);
        }
        let q = Params::from_affine(&t, Model::Affine, 1000.0, 2000.0).to_affine();
        assert!((q.a - t.a).abs() < 1e-12 && (q.f - t.f).abs() < 1e-6);
        assert!((anisotropy(&t) - 1.0).abs() < 1e-9);
    }

    #[test]
    fn ramp_bounds() {
        assert_eq!(ramp(0.0, 1.0, 2.0), 0.0);
        assert_eq!(ramp(3.0, 1.0, 2.0), 1.0);
        assert!((ramp(1.5, 1.0, 2.0) - 0.5).abs() < 1e-12);
    }

    /// Synthetic terrain: contours of a known surface, sampled through a known
    /// transform, are recovered from a 300 m offset start.
    #[test]
    fn recovers_synthetic_offset() {
        use crate::dem::{DemStore, TILE_SIZE};
        let z = 13u8;
        let elev = |mx: f64, my: f64| -> f64 {
            let (u, v) = (mx / 900.0, my / 700.0);
            300.0 * (u.sin() * v.cos()) + 80.0 * ((mx + 2.0 * my) / 400.0).sin()
        };
        let truth = Affine::pixel_to_merc_similarity(2.0, 0.02, 1000.0, 1200.0, 3.0e6, -2.98e6);
        let fp = refine_dem_box(&truth, 2000, 2400);
        let mut store = DemStore::new();
        for t in crate::dem::tiles_covering(z, &fp) {
            let b = crate::dem::tile_bounds(t);
            let res = crate::dem::resolution(z);
            let mut e = vec![0.0f32; TILE_SIZE * TILE_SIZE];
            for r in 0..TILE_SIZE {
                for c in 0..TILE_SIZE {
                    let (mx, my) = (b.min_x + (c as f64 + 0.5) * res, b.max_y - (r as f64 + 0.5) * res);
                    e[r * TILE_SIZE + c] = elev(mx, my) as f32;
                }
            }
            store.add_elevations(t, e);
        }
        let dem = store.mosaic(z, &fp);
        // Trace contours (every 10 m) of the true surface in pixel space by
        // marching along x in each row and recording level crossings.
        let mut s = ContourSamples::default();
        let mut seg_of_level = std::collections::HashMap::new();
        for y in (0..2400).step_by(6) {
            let mut prev = None;
            for x in 0..2000 {
                let (mx, my) = truth.apply(x as f64 + 0.5, y as f64 + 0.5);
                let lvl = (elev(mx, my) / 10.0).floor() as i64;
                if let Some(p) = prev {
                    if p != lvl {
                        let key = (lvl.max(p), y / 240, x / 250);
                        let n = seg_of_level.len() as u32;
                        let id = *seg_of_level.entry(key).or_insert(n);
                        s.pts.push([x as f32, y as f32 + 0.5]);
                        s.seg.push(id);
                    }
                }
                prev = Some(lvl);
            }
        }
        s.n_segs = seg_of_level.len();
        let init = Affine::pixel_to_merc_similarity(2.04, 0.0, 1000.0, 1200.0, 3.0e6 + 250.0, -2.98e6 - 200.0);
        let r = refine(&s, &dem, &init);
        let (a, b) = (r.transform.apply(1000.0, 1200.0), truth.apply(1000.0, 1200.0));
        let err = ((a.0 - b.0).powi(2) + (a.1 - b.1).powi(2)).sqrt();
        assert!(err < 6.0, "centre error {err} m, {r:?}");
        assert!((r.transform.mean_scale() / 2.0 - 1.0).abs() < 0.005, "{r:?}");
        assert!(r.explained_variance > 0.99, "{r:?}");
    }
}
