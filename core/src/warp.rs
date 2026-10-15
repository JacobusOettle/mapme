//! Photo mode: a smooth correction ("warp") for paper that does not lie flat.
//!
//! A photographed sheet that was folded or curls is not a plane, so no single
//! homography maps its pixels to the ground: on the real phone photo of the
//! Grootkloof sheet, folded in four, the best one is off by ~37 m median and
//! ~180 m at the corners. The full model is `merc(p) = H(p) + B(p)`, where `H`
//! is the best homography (`pixel_to_mercator`) and `B` the bilinear
//! interpolation of per-node offsets on a regular lattice over the image
//! (`Warp`; the JSON contract is in docs/ARCHITECTURE.md, *Georeference
//! response*).
//!
//! `fit_warp` estimates the node offsets by regularised least squares:
//!
//! * **data**: points traced along the identified grid lines must land on
//!   their grid value (one constraint across the line per point), and the
//!   DEM elevation along every contour segment must be constant (one
//!   constraint along the terrain gradient per sample; the segment's level is
//!   a free variable, eliminated by variable projection exactly like the
//!   refinement does, because it absorbs any offset a whole segment shares);
//! * **smoothness**: the bending energy of each offset component (second
//!   differences of the lattice) and a weak pull towards zero.
//!
//! Contours make the problem non-linear (the DEM is resampled where the
//! warped samples land): Gauss–Newton with Levenberg damping and robust
//! (soft-L1) re-weighting, coarse-to-fine on the smoothed DEM pyramid.
//! A warp is kept only if data it was not fitted to agree with it (grid lines
//! left out one at a time, contour segments split in two folds), it stays
//! fold-free (its pixel derivative below `MAX_DERIVATIVE_RATIO` times that of
//! `H`, so the inverse converges by fixed-point iteration) and its offsets are
//! plausible for paper that is merely not flat (`MAX_OFFSET_M`).

use crate::dem::DemMosaic;
use crate::geo::{latlon_to_merc, latlon_to_utm_zone, merc_to_latlon, Affine, Homography};
use crate::grid::{GridLine, LineAxis, ValuedLine};
use crate::image::Rgb8;
use crate::refine::PreparedDem;
use crate::skeleton::ContourSamples;
use rayon::prelude::*;
use serde::{Deserialize, Serialize};
use std::time::Instant;

/// Lattice nodes along the long side of the image (~120 px apart on a
/// phone photo: fine enough for the kinks of fold creases, coarse enough to
/// stay well determined between grid lines).
pub const LONG_SIDE_NODES: usize = 20;
/// Fold-free bound: the spectral norm of the warp's pixel derivative stays
/// below this times the smallest singular value of `H`'s.
pub const MAX_DERIVATIVE_RATIO: f64 = 0.35;
/// Largest offset (ground metres) a plausible warp applies where the map's
/// lines and contours are (beyond them, at the page margins, the lattice is
/// extrapolated).
pub const MAX_OFFSET_M: f64 = 300.0;
/// Fixed-point iterations of `Warp::invert` (at most; it stops below 1e-4 px).
const INVERT_ITERATIONS: usize = 30;

// ---------------------------------------------------------------------------
// The warp
// ---------------------------------------------------------------------------

/// Offsets on a regular lattice over the image (Web-Mercator metres, the
/// units `H` outputs). Node `(i, j)` (column `i`, row `j`) sits at pixel
/// `origin + (i·spacing[0], j·spacing[1])`; its offset is
/// `offsets_m[2·(j·cols + i)]` (x) and `[… + 1]` (y). Between nodes the
/// offset is interpolated bilinearly; beyond the lattice it is constant.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Warp {
    pub cols: usize,
    pub rows: usize,
    pub origin: [f64; 2],
    pub spacing: [f64; 2],
    pub offsets_m: Vec<f64>,
}

/// Nodes and bilinear weights of a pixel.
type Stencil = ([usize; 4], [f64; 4]);

impl Warp {
    /// Lattice size (cols, rows) for a `w` × `h` image: `LONG_SIDE_NODES`
    /// along the long side, about square cells.
    pub fn layout(w: usize, h: usize) -> (usize, usize) {
        let (w, h) = (w.max(1) as f64, h.max(1) as f64);
        let cell = w.max(h) / (LONG_SIDE_NODES - 1) as f64;
        let n = |side: f64| ((side / cell).round() as usize + 1).max(2);
        (n(w), n(h))
    }

    /// The zero warp on the lattice of a `w` × `h` image (origin (0, 0),
    /// spacing (w / (cols − 1), h / (rows − 1))).
    pub fn zero(w: usize, h: usize) -> Warp {
        let (cols, rows) = Warp::layout(w, h);
        Warp {
            cols,
            rows,
            origin: [0.0, 0.0],
            spacing: [w.max(1) as f64 / (cols - 1) as f64, h.max(1) as f64 / (rows - 1) as f64],
            offsets_m: vec![0.0; 2 * cols * rows],
        }
    }

    /// Consistent sizes and finite values.
    pub fn is_valid(&self) -> bool {
        self.cols >= 2
            && self.rows >= 2
            && self.offsets_m.len() == 2 * self.cols * self.rows
            && self.spacing.iter().all(|s| s.is_finite() && *s > 0.0)
            && self.origin.iter().all(|o| o.is_finite())
            && self.offsets_m.iter().all(|o| o.is_finite())
    }

    fn n_nodes(&self) -> usize {
        self.cols * self.rows
    }

    /// Pixel of node k.
    fn node_px(&self, k: usize) -> (f64, f64) {
        let (i, j) = (k % self.cols, k / self.cols);
        (self.origin[0] + i as f64 * self.spacing[0], self.origin[1] + j as f64 * self.spacing[1])
    }

    /// The four nodes around a pixel and their bilinear weights (lattice
    /// coordinates clamped to the lattice).
    fn stencil(&self, x: f64, y: f64) -> Stencil {
        let clamp = |v: f64, hi: usize| if v.is_finite() { v.clamp(0.0, hi as f64) } else { 0.0 };
        let u = clamp((x - self.origin[0]) / self.spacing[0], self.cols - 1);
        let v = clamp((y - self.origin[1]) / self.spacing[1], self.rows - 1);
        let i = (u.floor() as usize).min(self.cols - 2);
        let j = (v.floor() as usize).min(self.rows - 2);
        let (fu, fv) = (u - i as f64, v - j as f64);
        let k = j * self.cols + i;
        (
            [k, k + 1, k + self.cols, k + self.cols + 1],
            [(1.0 - fu) * (1.0 - fv), fu * (1.0 - fv), (1.0 - fu) * fv, fu * fv],
        )
    }

    /// Offset B(p) (Mercator metres); zero for an invalid warp.
    pub fn offset(&self, x: f64, y: f64) -> (f64, f64) {
        if !self.is_valid() {
            return (0.0, 0.0);
        }
        offset_of(&self.offsets_m, &self.stencil(x, y))
    }

    /// The full model H(p) + B(p).
    pub fn apply(&self, h: &Homography, x: f64, y: f64) -> (f64, f64) {
        let (mx, my) = h.apply(x, y);
        let (dx, dy) = self.offset(x, y);
        (mx + dx, my + dy)
    }

    /// Mercator → pixel through the full model by fixed-point iteration:
    /// p₀ = H⁻¹(m), pₖ₊₁ = H⁻¹(m − B(pₖ)). Converges because the warp is
    /// fold-free (`derivative_ratio` < 1).
    pub fn invert(&self, h: &Homography, mx: f64, my: f64) -> Option<(f64, f64)> {
        let hi = h.inverse()?;
        let mut p = hi.apply(mx, my);
        for _ in 0..INVERT_ITERATIONS {
            let (dx, dy) = self.offset(p.0, p.1);
            let q = hi.apply(mx - dx, my - dy);
            let step = (q.0 - p.0).hypot(q.1 - p.1);
            p = q;
            if step < 1e-4 {
                break;
            }
        }
        (p.0.is_finite() && p.1.is_finite()).then_some(p)
    }

    /// Largest node offset (Mercator metres).
    pub fn max_offset_m(&self) -> f64 {
        self.offsets_m.chunks_exact(2).map(|o| o[0].hypot(o[1])).fold(0.0, f64::max)
    }

    /// Largest ratio, over the lattice cells, of the spectral norm of the
    /// warp's pixel derivative to the smallest singular value of `H`'s (the
    /// warp is fold-free and invertible by fixed-point iteration when < 1;
    /// the core keeps it below `MAX_DERIVATIVE_RATIO`).
    pub fn derivative_ratio(&self, h: &Homography) -> f64 {
        if !self.is_valid() {
            return f64::INFINITY;
        }
        let o = &self.offsets_m;
        let (sx, sy) = (self.spacing[0], self.spacing[1]);
        let mut worst = 0.0f64;
        for j in 0..self.rows - 1 {
            for i in 0..self.cols - 1 {
                let k = j * self.cols + i;
                let n = [k, k + 1, k + self.cols, k + self.cols + 1];
                let d = |a: usize, b: usize, c: usize| o[2 * n[b] + c] - o[2 * n[a] + c];
                // ∂B/∂x along the cell's top and bottom edge, ∂B/∂y along its
                // left and right edge: the derivative is affine over the
                // cell, so its norm peaks at a corner.
                let dx = [[d(0, 1, 0) / sx, d(0, 1, 1) / sx], [d(2, 3, 0) / sx, d(2, 3, 1) / sx]];
                let dy = [[d(0, 2, 0) / sy, d(0, 2, 1) / sy], [d(1, 3, 0) / sy, d(1, 3, 1) / sy]];
                for (corner, (a, b)) in [(0, (0, 0)), (1, (0, 1)), (2, (1, 0)), (3, (1, 1))] {
                    let jb = [[dx[a][0], dy[b][0]], [dx[a][1], dy[b][1]]];
                    let (px, py) = self.node_px(n[corner]);
                    let la = h.local_affine(px, py);
                    let smin = singular_values([[la.a, la.b], [la.d, la.e]]).1;
                    let r = singular_values(jb).0 / smin;
                    worst = worst.max(if r.is_finite() { r } else { f64::INFINITY });
                }
            }
        }
        worst
    }

    /// First-order approximation of the full model at pixel (x, y) (central
    /// differences over a quarter of a lattice cell).
    pub fn local_affine(&self, h: &Homography, x: f64, y: f64) -> Affine {
        let e = 0.25 * self.spacing[0].min(self.spacing[1]).max(1e-6);
        let f = |x: f64, y: f64| self.apply(h, x, y);
        let (px0, px1, py0, py1) = (f(x - e, y), f(x + e, y), f(x, y - e), f(x, y + e));
        let (a, d) = ((px1.0 - px0.0) / (2.0 * e), (px1.1 - px0.1) / (2.0 * e));
        let (b, ee) = ((py1.0 - py0.0) / (2.0 * e), (py1.1 - py0.1) / (2.0 * e));
        let (cx, cy) = f(x, y);
        Affine { a, b, c: cx - a * x - b * y, d, e: ee, f: cy - d * x - ee * y }
    }

    /// A smooth pixel → Mercator `model` over a `w` × `h` image as the
    /// response carries it: the homography fitting it best (least squares on
    /// a 9 × 9 grid of image points), so a client ignoring the warp gets the
    /// best flat answer, plus the lattice of what it leaves
    /// (`LONG_SIDE_NODES` along the long side). None unless the warp is
    /// fold-free (`MAX_DERIVATIVE_RATIO`).
    pub fn from_model(w: usize, h: usize, model: impl Fn(f64, f64) -> (f64, f64)) -> Option<(Homography, Warp)> {
        let src: Vec<(f64, f64)> =
            (0..81).map(|k| (w as f64 * (k % 9) as f64 / 8.0, h as f64 * (k / 9) as f64 / 8.0)).collect();
        let dst: Vec<(f64, f64)> = src.iter().map(|&(x, y)| model(x, y)).collect();
        let hom = Homography::fit(&src, &dst)?.normalized();
        let mut warp = Warp::zero(w, h);
        for k in 0..warp.n_nodes() {
            let (x, y) = warp.node_px(k);
            let ((mx, my), (hx, hy)) = (model(x, y), hom.apply(x, y));
            warp.offsets_m[2 * k] = mx - hx;
            warp.offsets_m[2 * k + 1] = my - hy;
        }
        let warp = warp.rounded();
        (warp.is_valid() && warp.derivative_ratio(&hom) < MAX_DERIVATIVE_RATIO).then_some((hom, warp))
    }

    /// Offsets rounded to centimetres (compact JSON).
    pub fn rounded(mut self) -> Warp {
        for o in self.offsets_m.iter_mut() {
            *o = (*o * 100.0).round() / 100.0;
        }
        self
    }
}

fn offset_of(o: &[f64], st: &Stencil) -> (f64, f64) {
    let (n, w) = st;
    let mut d = (0.0, 0.0);
    for q in 0..4 {
        d.0 += w[q] * o[2 * n[q]];
        d.1 += w[q] * o[2 * n[q] + 1];
    }
    d
}

/// Singular values (largest, smallest) of a 2×2 matrix.
fn singular_values(m: [[f64; 2]; 2]) -> (f64, f64) {
    let (a, b, c, d) = (m[0][0], m[0][1], m[1][0], m[1][1]);
    let s1 = a * a + b * b + c * c + d * d;
    let det = (a * d - b * c).abs();
    let disc = (s1 * s1 - 4.0 * det * det).max(0.0).sqrt();
    (((s1 + disc) / 2.0).sqrt(), ((s1 - disc) / 2.0).max(0.0).sqrt())
}

// ---------------------------------------------------------------------------
// Normal equations
// ---------------------------------------------------------------------------

/// One linearised constraint `val · x ≈ t` on the unknowns (the x, y offsets
/// of the four nodes of a stencil), with weight `w` (1 / σ²).
#[derive(Clone, Copy)]
struct Row {
    idx: [usize; 8],
    val: [f64; 8],
    t: f64,
    w: f64,
}

impl Row {
    /// Constraint on the offset component along unit vector `n` at a stencil.
    fn along(st: &Stencil, n: (f64, f64), t: f64, w: f64) -> Row {
        let (k, wt) = st;
        let mut idx = [0usize; 8];
        let mut val = [0.0f64; 8];
        for q in 0..4 {
            idx[2 * q] = 2 * k[q];
            idx[2 * q + 1] = 2 * k[q] + 1;
            val[2 * q] = wt[q] * n.0;
            val[2 * q + 1] = wt[q] * n.1;
        }
        Row { idx, val, t, w }
    }

    fn dot(&self, x: &[f64]) -> f64 {
        self.idx.iter().zip(&self.val).map(|(&i, &v)| v * x[i]).sum()
    }
}

/// Dense normal equations `a·x = b` of size n (full symmetric storage).
#[derive(Clone)]
struct Normal {
    n: usize,
    a: Vec<f64>,
    b: Vec<f64>,
}

impl Normal {
    fn new(n: usize) -> Normal {
        Normal { n, a: vec![0.0; n * n], b: vec![0.0; n] }
    }

    fn add_row(&mut self, r: &Row) {
        let n = self.n;
        for p in 0..8 {
            let wv = r.w * r.val[p];
            if wv == 0.0 {
                continue;
            }
            let row = r.idx[p] * n;
            for q in 0..8 {
                self.a[row + r.idx[q]] += wv * r.val[q];
            }
            self.b[r.idx[p]] += wv * r.t;
        }
    }

    /// Rows sharing free parameters (`val·x ≈ t + c₀`, or with a slope
    /// `t + c₀ + c₁·s` when `s` gives each row's position), eliminated in
    /// closed form (Schur complement: variable projection).
    fn add_group(&mut self, rows: &[Row], s: Option<&[f64]>, g: &mut Vec<(usize, [f64; 2])>) {
        let basis = |i: usize| [1.0, s.map_or(0.0, |s| s[i])];
        let (mut m, mut mt) = ([[0.0f64; 2]; 2], [0.0f64; 2]);
        g.clear();
        for (ri, r) in rows.iter().enumerate() {
            self.add_row(r);
            let f = basis(ri);
            for a in 0..2 {
                mt[a] += r.w * f[a] * r.t;
                for b in 0..2 {
                    m[a][b] += r.w * f[a] * f[b];
                }
            }
            for p in 0..8 {
                let wv = r.w * r.val[p];
                g.push((r.idx[p], [wv * f[0], wv * f[1]]));
            }
        }
        // Inverse of the free parameters' block (a constant alone when the
        // slope is not wanted or not determined).
        let det = m[0][0] * m[1][1] - m[0][1] * m[1][0];
        let inv = if s.is_some() && det > 1e-9 * m[0][0] * m[1][1].max(1e-300) && det > 0.0 {
            [[m[1][1] / det, -m[0][1] / det], [-m[1][0] / det, m[0][0] / det]]
        } else if m[0][0] > 0.0 {
            [[1.0 / m[0][0], 0.0], [0.0, 0.0]]
        } else {
            return;
        };
        g.sort_unstable_by_key(|e| e.0);
        let mut c: Vec<(usize, [f64; 2])> = Vec::with_capacity(g.len());
        for &(i, v) in g.iter() {
            match c.last_mut() {
                Some(l) if l.0 == i => {
                    l.1[0] += v[0];
                    l.1[1] += v[1];
                }
                _ => c.push((i, v)),
            }
        }
        let n = self.n;
        for &(i, ci) in &c {
            let u = [inv[0][0] * ci[0] + inv[0][1] * ci[1], inv[1][0] * ci[0] + inv[1][1] * ci[1]];
            for &(j, cj) in &c {
                self.a[i * n + j] -= u[0] * cj[0] + u[1] * cj[1];
            }
            self.b[i] -= u[0] * mt[0] + u[1] * mt[1];
        }
    }

    fn add(&mut self, o: &Normal) {
        self.a.iter_mut().zip(&o.a).for_each(|(a, b)| *a += b);
        self.b.iter_mut().zip(&o.b).for_each(|(a, b)| *a += b);
    }

    /// Solve by Cholesky (None unless positive definite).
    fn solve(mut self) -> Option<Vec<f64>> {
        cholesky_solve(&mut self.a, self.n, &self.b)
    }
}

fn dot(a: &[f64], b: &[f64]) -> f64 {
    let mut s = [0.0f64; 4];
    let ca = a.chunks_exact(4);
    let cb = b.chunks_exact(4);
    let (ra, rb) = (ca.remainder(), cb.remainder());
    for (x, y) in ca.zip(cb) {
        s[0] += x[0] * y[0];
        s[1] += x[1] * y[1];
        s[2] += x[2] * y[2];
        s[3] += x[3] * y[3];
    }
    let mut t = (s[0] + s[1]) + (s[2] + s[3]);
    for (x, y) in ra.iter().zip(rb) {
        t += x * y;
    }
    t
}

/// In-place Cholesky (row-major lower triangle) and solve.
fn cholesky_solve(a: &mut [f64], n: usize, b: &[f64]) -> Option<Vec<f64>> {
    for i in 0..n {
        let (done, rest) = a.split_at_mut(i * n);
        let ri = &mut rest[..n];
        for j in 0..i {
            let rj = &done[j * n..j * n + n];
            let s = ri[j] - dot(&ri[..j], &rj[..j]);
            ri[j] = s / rj[j];
        }
        let d = ri[i] - dot(&ri[..i], &ri[..i]);
        if !(d > 0.0 && d.is_finite()) {
            return None;
        }
        ri[i] = d.sqrt();
    }
    // L y = b
    let mut y = b.to_vec();
    for i in 0..n {
        let s = y[i] - dot(&a[i * n..i * n + i], &y[..i]);
        y[i] = s / a[i * n + i];
    }
    // Lᵀ x = y
    for i in (0..n).rev() {
        let yi = y[i] / a[i * n + i];
        y[i] = yi;
        for (k, yk) in y.iter_mut().enumerate().take(i) {
            *yk -= a[i * n + k] * yi;
        }
    }
    y.iter().all(|v| v.is_finite()).then_some(y)
}

/// Bending energy of each offset component (second differences of the
/// lattice along x and y and the mixed one, weighted like the continuous
/// thin-plate energy on cells of `sx` × `sy` pixels) times `lambda`, plus
/// `mu` times the squared offsets (a weak pull towards zero).
struct Smooth {
    /// Second-difference stencils (nodes, coefficients) and their weights.
    stencils: Vec<(Vec<(usize, f64)>, f64)>,
    lambda: f64,
    mu: f64,
    n: usize,
}

impl Smooth {
    fn new(warp: &Warp, lambda: f64, mu: f64) -> Smooth {
        let (cols, rows) = (warp.cols, warp.rows);
        let (sx, sy) = (warp.spacing[0], warp.spacing[1]);
        let (wxx, wyy, wxy) = ((sy / sx).powi(2), (sx / sy).powi(2), 2.0);
        let k = |i: usize, j: usize| j * cols + i;
        let mut stencils = Vec::new();
        for j in 0..rows {
            for i in 1..cols - 1 {
                stencils.push((vec![(k(i - 1, j), 1.0), (k(i, j), -2.0), (k(i + 1, j), 1.0)], wxx));
            }
        }
        for j in 1..rows - 1 {
            for i in 0..cols {
                stencils.push((vec![(k(i, j - 1), 1.0), (k(i, j), -2.0), (k(i, j + 1), 1.0)], wyy));
            }
        }
        for j in 0..rows - 1 {
            for i in 0..cols - 1 {
                stencils.push((
                    vec![(k(i, j), 1.0), (k(i + 1, j), -1.0), (k(i, j + 1), -1.0), (k(i + 1, j + 1), 1.0)],
                    wxy,
                ));
            }
        }
        Smooth { stencils, lambda, mu, n: 2 * warp.n_nodes() }
    }

    fn diff(nodes: &[(usize, f64)], x: &[f64], c: usize) -> f64 {
        nodes.iter().map(|&(p, v)| v * x[2 * p + c]).sum()
    }

    fn cost(&self, x: &[f64]) -> f64 {
        let mut e = 0.0;
        for (nodes, w) in &self.stencils {
            for c in 0..2 {
                e += self.lambda * w * Smooth::diff(nodes, x, c).powi(2);
            }
        }
        e + self.mu * x.iter().map(|v| v * v).sum::<f64>()
    }

    /// Normal equations.
    fn normal(&self) -> Normal {
        let n = self.n;
        let mut reg = Normal::new(n);
        for (nodes, w) in &self.stencils {
            for c in 0..2 {
                let wr = self.lambda * w;
                for &(p, vp) in nodes {
                    for &(q, vq) in nodes {
                        reg.a[(2 * p + c) * n + 2 * q + c] += wr * vp * vq;
                    }
                }
            }
        }
        for i in 0..n {
            reg.a[i * n + i] += self.mu;
        }
        reg
    }
}

// ---------------------------------------------------------------------------
// Fitting: inputs, outputs
// ---------------------------------------------------------------------------

/// Identified grid lines to fit a warp to (`GridResult::lines`).
pub struct GridLines<'a> {
    pub lines: &'a [ValuedLine],
    /// Zone and hemisphere of a UTM grid (values in metres); None for a
    /// graticule (values in degrees).
    pub utm_zone: Option<(u8, bool)>,
}

/// Contour samples and the DEM pyramid (`PreparedDem`) to fit a warp to.
pub struct ContourInput<'a> {
    pub samples: &'a ContourSamples,
    pub dem: &'a PreparedDem,
}

/// What to fit a warp to on top of `transform` (pixel → Mercator).
pub struct WarpInput<'a> {
    /// Image size (pixels).
    pub w: usize,
    pub h: usize,
    pub transform: Homography,
    /// The normalised photo the lines are traced in (else their detected
    /// courses are used as they are).
    pub image: Option<&'a Rgb8>,
    pub grid: Option<GridLines<'a>>,
    /// Other printed straight lines (the map frame, grid lines without a
    /// value): straight on the paper, so straight on the ground.
    pub straight: &'a [GridLine],
    pub contours: Option<ContourInput<'a>>,
}

/// How a warp was fitted (`diagnostics.warp`). Distances are ground metres
/// except the contour costs (metres of elevation).
#[derive(Debug, Clone, Default, Serialize)]
pub struct WarpDiagnostics {
    /// "grid" (grid lines, plus contours when the DEM was at hand) or "contours".
    pub source: String,
    /// The warp passed every check and is in the response.
    pub accepted: bool,
    /// Why it was dropped.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rejected: Option<String>,
    pub cols: usize,
    pub rows: usize,
    pub n_lines: usize,
    pub n_straight_lines: usize,
    pub n_line_points: usize,
    pub n_segments: usize,
    pub n_samples: usize,
    /// RMS distance of the traced grid-line points from their grid value
    /// under H alone and under H + warp (fitted).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rms_before_m: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rms_after_m: Option<f64>,
    /// The same for each line predicted by a warp fitted without it, and
    /// under H alone for the lines that count there.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub holdout_rms_m: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub holdout_before_m: Option<f64>,
    /// Robust mean |elevation residual| of the contour samples (metres of
    /// elevation) under H alone and under H + warp.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub contour_cost_before_m: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub contour_cost_after_m: Option<f64>,
    /// Smallest relative cost drop of a contour half (segments split in two)
    /// under the warp fitted without it.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub contour_holdout_gain: Option<f64>,
    /// Median within-segment elevation standard deviation after the warp.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub median_seg_std_m: Option<f64>,
    /// Largest offset where the map's lines and contours are.
    pub max_offset_m: f64,
    /// `Warp::derivative_ratio` (fold-free below `MAX_DERIVATIVE_RATIO`).
    pub derivative_ratio: f64,
    /// Linear solves (Gauss–Newton and robust re-weighting steps).
    pub iterations: usize,
    pub ms: f64,
}

/// A fitted warp (None when it was not worth it or failed a check) and how.
#[derive(Debug, Clone)]
pub struct WarpFit {
    pub warp: Option<Warp>,
    pub diag: WarpDiagnostics,
}

// ---------------------------------------------------------------------------
// Fitting: data terms
// ---------------------------------------------------------------------------

/// Grid-line points: one every this many pixels along an identified line,
/// where it was seen…
const LINE_STEP_PX: f64 = 8.0;
/// …each good to about this many pixels across the line.
const LINE_SIGMA_PX: f64 = 1.5;
/// Neighbouring line points closer than this (pixels) have correlated
/// errors: their weights are scaled by step / this.
const CORRELATION_PX: f64 = 16.0;
/// The same for contour samples (along a contour; the DEM's own errors are
/// what correlates them, over a cell or so).
const CONTOUR_CORRELATION_PX: f64 = 5.0;
/// Elevation residual scale of a contour sample (metres, finest level).
const CONTOUR_SIGMA_M: f64 = 3.0;
/// Contour segments need this many samples on the DEM.
const MIN_SEG_SAMPLES: usize = 4;
/// Samples off the DEM cost as if their residual were this (metres).
const MISSING_PENALTY_M: f64 = 10.0;
/// Contour samples used at most (strided subset).
const MAX_SAMPLES: usize = 80_000;
/// Smoothness: expected second difference of an offset component per
/// lattice cell, as a fraction of the cell size (both in Mercator metres).
const SMOOTH_SLOPE: f64 = 0.03;
/// Prior spread of the offsets (Mercator metres): the weak pull to zero.
const PRIOR_OFFSET_M: f64 = 150.0;

#[inline]
fn soft_l1(r: f64, c: f64) -> f64 {
    // 2c²(√(1 + z²) − 1), written so it stays exact for |z| ≪ 1.
    let z = r / c;
    2.0 * r * r / ((1.0 + z * z).sqrt() + 1.0)
}

#[inline]
fn soft_l1_weight(r: f64, c: f64) -> f64 {
    1.0 / (1.0 + (r / c).powi(2)).sqrt()
}

/// A point traced on a line: its residual across the line is `e0 + n·B(p)`
/// (Mercator metres; `e0` under H alone), measured from the line's grid
/// value, or for a straight line of unknown value from a free offset and
/// slope along it (`s` runs from −1 to 1 over the line).
struct LinePt {
    st: Stencil,
    n: (f64, f64),
    e0: f64,
    s: f64,
}

/// What a traced line says about the ground.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum LineKind {
    /// A grid line with its value: it lies exactly there.
    Valued,
    /// A printed line parallel to a graticule (the map frame): a parallel or
    /// meridian of unknown value.
    Parallel,
    /// Another printed straight line: straight on the ground, anywhere.
    Straight,
}

/// A line traced in the image.
struct TracedLine {
    pts: Vec<LinePt>,
    kind: LineKind,
}

impl TracedLine {
    /// Residuals under offsets x (a straight line's after removing the best
    /// offset and slope).
    fn residuals(&self, x: &[f64]) -> Vec<f64> {
        let mut e: Vec<f64> = self
            .pts
            .iter()
            .map(|p| {
                let (dx, dy) = offset_of(x, &p.st);
                p.e0 + p.n.0 * dx + p.n.1 * dy
            })
            .collect();
        let (a, b) = match self.kind {
            LineKind::Valued => return e,
            LineKind::Parallel => (e.iter().sum::<f64>() / e.len().max(1) as f64, 0.0),
            LineKind::Straight => line_fit(self.pts.iter().map(|p| p.s).zip(e.iter().copied())),
        };
        for (v, p) in e.iter_mut().zip(&self.pts) {
            *v -= a + b * p.s;
        }
        e
    }
}

/// Least-squares `y ≈ a + b·x`.
fn line_fit(pts: impl Iterator<Item = (f64, f64)>) -> (f64, f64) {
    let (mut n, mut sx, mut sy, mut sxx, mut sxy) = (0.0, 0.0, 0.0, 0.0, 0.0);
    for (x, y) in pts {
        n += 1.0;
        sx += x;
        sy += y;
        sxx += x * x;
        sxy += x * y;
    }
    let den = n * sxx - sx * sx;
    if n == 0.0 {
        return (0.0, 0.0);
    }
    if den.abs() < 1e-12 * n * n {
        return (sy / n, 0.0);
    }
    let b = (n * sxy - sx * sy) / den;
    ((sy - b * sx) / n, b)
}

/// Unit normal (Mercator) of the grid line of `value` through `m` and the
/// signed distance of `m` from it (Mercator metres).
fn line_residual(axis: LineAxis, value: f64, utm: Option<(u8, bool)>, m: (f64, f64)) -> Option<((f64, f64), f64)> {
    let horizontal = axis == LineAxis::Horizontal;
    match utm {
        None => {
            let (tx, ty) = latlon_to_merc(if horizontal { value } else { 0.0 }, if horizontal { 0.0 } else { value });
            Some(if horizontal { ((0.0, 1.0), m.1 - ty) } else { ((1.0, 0.0), m.0 - tx) })
        }
        Some((zone, _)) => {
            let f = |x: f64, y: f64| {
                let (lat, lon) = merc_to_latlon(x, y);
                let u = latlon_to_utm_zone(lat, lon, zone);
                if horizontal {
                    u.northing
                } else {
                    u.easting
                }
            };
            const D: f64 = 10.0;
            let g = ((f(m.0 + D, m.1) - f(m.0 - D, m.1)) / (2.0 * D), (f(m.0, m.1 + D) - f(m.0, m.1 - D)) / (2.0 * D));
            let gn = g.0.hypot(g.1);
            let v = f(m.0, m.1);
            (gn > 1e-6 && v.is_finite()).then(|| ((g.0 / gn, g.1 / gn), (v - value) / gn))
        }
    }
}

/// Pixels along a line where it was seen, every `LINE_STEP_PX`, inside the image.
fn line_pixels(l: &GridLine, w: f64, h: f64) -> Vec<(f64, f64)> {
    let extent = if l.axis == LineAxis::Horizontal { w } else { h };
    let (lo, hi) = l.bend.map_or((0.0, extent), |b| (b.lo.max(0.0), b.hi.min(extent)));
    let mut out = Vec::new();
    let mut s = lo;
    while s <= hi {
        let (x, y) = l.point_at(s);
        s += LINE_STEP_PX;
        if x >= 0.0 && y >= 0.0 && x <= w && y <= h {
            out.push((x, y));
        }
    }
    out
}

// ---------------------------------------------------------------------------
// Line tracing
// ---------------------------------------------------------------------------

/// Other straight lines must have been seen along this fraction of the
/// image, and at most a tenth of them within this distance (pixels) of an
/// identified line.
const STRAIGHT_MIN_COVER: f64 = 0.4;
const STRAIGHT_MIN_GAP_PX: f64 = 6.0;
/// With identified grid lines, only a frame line this far (pixels) beyond
/// the outermost one of its family is used.
const FRAME_MIN_GAP_PX: f64 = 25.0;
/// A straight line's trace is ragged (it jumped between features) when the
/// median bend between consecutive points exceeds this (pixels), or more
/// than a tenth of them bend more than `STRAIGHT_SHARP_PX`.
const STRAIGHT_MAX_MEDIAN_BEND_PX: f64 = 0.8;
const STRAIGHT_SHARP_PX: f64 = 3.0;
/// Half-width (pixels) of the search across a line around its detected
/// course (beyond where it was seen, a fold may bend it this far away).
const TRACE_HALF_PX: isize = 40;
/// Along-line step of the trace (pixels).
const TRACE_STEP_PX: usize = 2;
/// A crossing needs this much more grey ink than the paper on both sides of it.
const TRACE_MIN_CONTRAST: f32 = 15.0;
/// The traced course is a running median over ± this many steps…
const TRACE_SMOOTH_STEPS: usize = 10;
/// …and crossings are re-picked within these distances of it (pixels).
const TRACE_TOLERANCES: [f64; 4] = [8.0, 4.0, 2.5, 1.5];
/// Beyond its ends the line is followed by extrapolating the crossings
/// accepted over this many pixels behind…
const TRACE_LOOKBACK_PX: usize = 48;
/// …taking the nearest crossing within this distance of the prediction
/// (pixels), plus this fraction of the gap since the last one…
const TRACE_TOL_PER_GAP: f64 = 0.03;
/// …until a gap this long (pixels).
const TRACE_MAX_GAP_PX: usize = 120;

/// Follow a printed line in the normalised photo (`contour::normalize_illumination`)
/// from its detected course `l`. Candidates are, every `TRACE_STEP_PX` along
/// the line, the thin grey crossings near the course (sub-pixel centres of
/// local ink maxima that stand out from the paper on both sides; coloured
/// ink such as brown contours does not count). The crossings nearest a
/// running median of the picks, with a shrinking tolerance, are the line;
/// beyond the stretch found that way it is followed step by step (the
/// crossing nearest the line extrapolated from the last ones), so it is
/// kept where a fold bends it away from the course it was detected on.
/// Returns its points every `LINE_STEP_PX` where it was seen (pixel-index
/// coordinates, like `GridLine`).
pub fn trace_printed_line(img: &Rgb8, l: &GridLine) -> Vec<(f64, f64)> {
    let horizontal = l.axis == LineAxis::Horizontal;
    let (extent, across) = if horizontal { (img.w, img.h) } else { (img.h, img.w) };
    if extent < 4 * TRACE_LOOKBACK_PX || across < 16 {
        return Vec::new();
    }
    let ink = |s: usize, c: isize| -> f32 {
        if c < 0 || c as usize >= across {
            return 0.0;
        }
        let p = if horizontal { img.rgb(s, c as usize) } else { img.rgb(c as usize, s) };
        crate::contour::photo_grey_ink(p) as f32
    };
    const SIDE: isize = 6;
    let scans: Vec<usize> = (0..extent).step_by(TRACE_STEP_PX).collect();
    let course: Vec<f64> = scans.iter().map(|&s| l.pos_at(s as f64)).collect();
    // Candidate crossings per scan, as offsets from the course.
    let cands: Vec<Vec<f64>> = scans
        .par_iter()
        .zip(&course)
        .map(|(&s, &c)| {
            let c0 = c.round() as isize;
            let lo = c0 - TRACE_HALF_PX - SIDE;
            let v: Vec<f32> = (lo..=c0 + TRACE_HALF_PX + SIDE).map(|q| ink(s, q)).collect();
            let side = |r: std::ops::RangeInclusive<usize>| v[r].iter().fold(f32::MAX, |m, &x| m.min(x));
            let mut out = Vec::new();
            for k in SIDE as usize..v.len() - SIDE as usize {
                let (a, b, cc) = (v[k - 1], v[k], v[k + 1]);
                if !(b >= a && b > cc) {
                    continue;
                }
                let bg = side(k - SIDE as usize..=k - 3).max(side(k + 3..=k + SIDE as usize));
                if b - bg < TRACE_MIN_CONTRAST {
                    continue;
                }
                let den = a - 2.0 * b + cc;
                let d = if den < 0.0 { (0.5 * (a - cc) / den) as f64 } else { 0.0 };
                out.push((lo + k as isize) as f64 + d.clamp(-0.5, 0.5) - c);
            }
            out
        })
        .collect();
    let n = scans.len();
    let nearest = |i: usize, q0: f64, tol: f64| -> Option<f64> {
        cands[i]
            .iter()
            .copied()
            .filter(|q| (q - q0).abs() <= tol)
            .min_by(|a, b| (a - q0).abs().total_cmp(&(b - q0).abs()))
    };
    let mut curve = vec![0.0f64; n];
    let mut picks: Vec<Option<f64>> = vec![None; n];
    for tol in TRACE_TOLERANCES {
        for i in 0..n {
            picks[i] = nearest(i, curve[i], tol);
        }
        // Running median of the picks; gaps interpolated between medians.
        let mut med: Vec<Option<f64>> = vec![None; n];
        let mut win = Vec::new();
        for (i, m) in med.iter_mut().enumerate() {
            win.clear();
            win.extend(
                picks[i.saturating_sub(TRACE_SMOOTH_STEPS)..(i + TRACE_SMOOTH_STEPS + 1).min(n)].iter().flatten(),
            );
            if win.len() * 3 >= TRACE_SMOOTH_STEPS {
                *m = median(&mut win);
            }
        }
        let known: Vec<usize> = (0..n).filter(|&i| med[i].is_some()).collect();
        if known.is_empty() {
            return Vec::new();
        }
        for (i, c) in curve.iter_mut().enumerate() {
            let k = known.partition_point(|&j| j < i);
            *c = match (k.checked_sub(1).map(|k| known[k]), known.get(k)) {
                (_, Some(&j)) if j == i => med[j].unwrap(),
                (Some(a), Some(&b)) => {
                    let f = (i - a) as f64 / (b - a) as f64;
                    med[a].unwrap() * (1.0 - f) + med[b].unwrap() * f
                }
                (Some(a), None) => med[a].unwrap(),
                (None, Some(&b)) => med[b].unwrap(),
                (None, None) => 0.0,
            };
        }
    }
    let tol = TRACE_TOLERANCES[TRACE_TOLERANCES.len() - 1];
    for i in 0..n {
        picks[i] = picks[i].filter(|q| (q - curve[i]).abs() <= tol);
    }
    // Beyond the ends: step by step, extrapolating the last crossings.
    let back = TRACE_LOOKBACK_PX / TRACE_STEP_PX;
    let ends = (picks.iter().position(Option::is_some), picks.iter().rposition(Option::is_some));
    if let (Some(first), Some(last)) = ends {
        for (start, dir) in [(last as isize, 1isize), (first as isize, -1)] {
            let (mut i, mut gap) = (start, 0usize);
            loop {
                i += dir;
                if i < 0 || i as usize >= n || gap * TRACE_STEP_PX > TRACE_MAX_GAP_PX {
                    break;
                }
                let (mut sn, mut sx, mut sy, mut sxx, mut sxy) = (0.0, 0.0, 0.0, 0.0, 0.0);
                let mut k = 1isize;
                while sn < back as f64 && k <= (back + gap) as isize {
                    let j = i - dir * k;
                    if j < 0 || j as usize >= n {
                        break;
                    }
                    if let Some(q) = picks[j as usize] {
                        let x = -(k as f64);
                        sn += 1.0;
                        sx += x;
                        sy += q;
                        sxx += x * x;
                        sxy += x * q;
                    }
                    k += 1;
                }
                if sn < 4.0 {
                    break;
                }
                let den = sn * sxx - sx * sx;
                let slope = if den.abs() > 1e-9 { (sn * sxy - sx * sy) / den } else { 0.0 };
                let pred = (sy - slope * sx) / sn;
                let iu = i as usize;
                picks[iu] = nearest(iu, pred, tol + TRACE_TOL_PER_GAP * (gap * TRACE_STEP_PX) as f64);
                gap = if picks[iu].is_some() { 0 } else { gap + 1 };
            }
        }
    }
    // Points every LINE_STEP_PX: the mean of the crossings there.
    let per = ((LINE_STEP_PX as usize) / TRACE_STEP_PX).max(1);
    let mut out = Vec::new();
    for c in (0..n).collect::<Vec<_>>().chunks(per) {
        let got: Vec<(f64, f64)> =
            c.iter().filter_map(|&i| picks[i].map(|q| (scans[i] as f64, course[i] + q))).collect();
        if got.len() * 2 >= c.len() {
            let k = got.len() as f64;
            let (s, q) = got.iter().fold((0.0, 0.0), |a, g| (a.0 + g.0 / k, a.1 + g.1 / k));
            out.push(if horizontal { (s, q) } else { (q, s) });
        }
    }
    out
}

/// Along-line coordinate of each pixel, −1…1.
fn along(px: &[(f64, f64)], axis: LineAxis) -> Vec<f64> {
    let s: Vec<f64> = px.iter().map(|p| if axis == LineAxis::Horizontal { p.0 } else { p.1 }).collect();
    let (lo, hi) = s.iter().fold((f64::MAX, f64::MIN), |a, &v| (a.0.min(v), a.1.max(v)));
    let (mid, half) = (0.5 * (lo + hi), (0.5 * (hi - lo)).max(1.0));
    s.iter().map(|v| (v - mid) / half).collect()
}

/// (along, across) coordinates of traced points of a line of `axis`.
fn along_across(px: &[(f64, f64)], axis: LineAxis) -> Vec<(f64, f64)> {
    px.iter().map(|&(x, y)| if axis == LineAxis::Horizontal { (x, y) } else { (y, x) }).collect()
}

/// Fraction of the points of trace `a` that lie within `tol` pixels (across)
/// of trace `b` of the same axis.
fn overlap(a: &[(f64, f64)], b: &[(f64, f64)], tol: f64) -> f64 {
    if a.is_empty() || b.is_empty() {
        return 0.0;
    }
    let near = a
        .iter()
        .filter(|p| {
            let k = b.partition_point(|q| q.0 < p.0);
            [k.wrapping_sub(1), k]
                .iter()
                .any(|&j| b.get(j).is_some_and(|q| (q.0 - p.0).abs() <= LINE_STEP_PX && (q.1 - p.1).abs() <= tol))
        })
        .count();
    near as f64 / a.len() as f64
}

/// A trace that jumps between features rather than following one line:
/// the median bend between consecutive points (second difference, pixels)
/// is large, or too many points bend sharply.
fn ragged(t: &[(f64, f64)]) -> bool {
    let mut d: Vec<f64> = t
        .windows(3)
        .filter(|w| w[2].0 - w[0].0 <= 2.5 * LINE_STEP_PX)
        .map(|w| (w[0].1 - 2.0 * w[1].1 + w[2].1).abs())
        .collect();
    let sharp = d.iter().filter(|&&v| v > STRAIGHT_SHARP_PX).count();
    median(&mut d).is_none_or(|m| m > STRAIGHT_MAX_MEDIAN_BEND_PX) || sharp * 10 > d.len()
}

fn traced_lines(input: &WarpInput, lattice: &Warp) -> Vec<TracedLine> {
    let (w, h) = (input.w as f64, input.h as f64);
    let t = &input.transform;
    let line_pixels = |l: &GridLine| match input.image {
        Some(img) => trace_printed_line(img, l),
        None => line_pixels(l, w, h),
    };
    let mut out = Vec::new();
    // Traces of the lines used, for the overlap checks: (axis, along/across, index in out).
    let mut traces: Vec<(LineAxis, Vec<(f64, f64)>)> = Vec::new();
    if let Some(g) = &input.grid {
        for vl in g.lines {
            let px = line_pixels(&vl.line);
            let s = along(&px, vl.line.axis);
            let pts = px
                .iter()
                .zip(s)
                .filter_map(|(&(x, y), s)| {
                    let (n, e0) = line_residual(vl.line.axis, vl.value, g.utm_zone, t.apply(x, y))?;
                    Some(LinePt { st: lattice.stencil(x, y), n, e0, s })
                })
                .collect();
            traces.push((vl.line.axis, along_across(&px, vl.line.axis)));
            out.push(TracedLine { pts, kind: LineKind::Valued });
        }
    }
    // A graticule's frame and other long lines of its families are
    // parallels and meridians too.
    let graticule = input.grid.as_ref().is_some_and(|g| g.utm_zone.is_none() && !g.lines.is_empty());
    let n_valued = traces.len();
    let mut straight: Vec<(GridLine, Vec<(f64, f64)>)> = Vec::new();
    for l in input.straight {
        let extent = if l.axis == LineAxis::Horizontal { w } else { h };
        let px = line_pixels(l);
        let aa = along_across(&px, l.axis);
        // Long, smooth, apart from the identified lines (a crease or a road
        // beside one would fight it) and not a second trace of another.
        if (px.len() as f64) * LINE_STEP_PX < STRAIGHT_MIN_COVER * extent
            || ragged(&aa)
            || traces[..n_valued].iter().any(|(ax, tr)| *ax == l.axis && overlap(&aa, tr, STRAIGHT_MIN_GAP_PX) > 0.1)
        {
            continue;
        }
        match straight
            .iter_mut()
            .find(|(o, tr)| o.axis == l.axis && overlap(&aa, tr, 4.0).max(overlap(tr, &aa, 4.0)) > 0.3)
        {
            Some(dup) if dup.1.len() < aa.len() => *dup = (*l, aa),
            Some(_) => {}
            None => straight.push((*l, aa)),
        }
    }
    // Beside identified grid lines only the map frame is worth it: per
    // family and side, the longest line beyond the outermost identified one.
    let across = |tr: &[(f64, f64)]| median(&mut tr.iter().map(|p| p.1).collect::<Vec<_>>()).unwrap_or(f64::NAN);
    for axis in [LineAxis::Horizontal, LineAxis::Vertical] {
        let pos: Vec<f64> = traces[..n_valued].iter().filter(|t| t.0 == axis).map(|t| across(&t.1)).collect();
        if pos.is_empty() {
            continue;
        }
        let (lo, hi) = pos.iter().fold((f64::MAX, f64::MIN), |a, &p| (a.0.min(p), a.1.max(p)));
        let mut best: [Option<usize>; 2] = [None, None];
        for (i, (l, tr)) in straight.iter().enumerate().filter(|(_, (l, _))| l.axis == axis) {
            let _ = l;
            let a = across(tr);
            let side = if a < lo - FRAME_MIN_GAP_PX {
                0
            } else if a > hi + FRAME_MIN_GAP_PX {
                1
            } else {
                continue;
            };
            if best[side].is_none_or(|b| straight[b].1.len() < tr.len()) {
                best[side] = Some(i);
            }
        }
        let keep: Vec<usize> = best.iter().flatten().copied().collect();
        let mut i = 0;
        straight.retain(|(l, _)| {
            i += 1;
            l.axis != axis || keep.contains(&(i - 1))
        });
    }
    for (l, aa) in straight {
        let px: Vec<(f64, f64)> = along_across(&aa, l.axis);
        // The line's direction on the ground, as H puts it.
        let m: Vec<(f64, f64)> = px.iter().map(|&(x, y)| t.apply(x, y)).collect();
        let k = m.len() as f64;
        let c = m.iter().fold((0.0, 0.0), |a, p| (a.0 + p.0 / k, a.1 + p.1 / k));
        let (mut sxx, mut sxy, mut syy) = (0.0, 0.0, 0.0);
        for p in &m {
            let (dx, dy) = (p.0 - c.0, p.1 - c.1);
            sxx += dx * dx;
            sxy += dx * dy;
            syy += dy * dy;
        }
        let theta = 0.5 * (2.0 * sxy).atan2(sxx - syy);
        let (n, kind) = match graticule {
            true if theta.cos().abs() >= theta.sin().abs() => ((0.0, 1.0), LineKind::Parallel),
            true => ((1.0, 0.0), LineKind::Parallel),
            false => ((-theta.sin(), theta.cos()), LineKind::Straight),
        };
        let s = along(&px, l.axis);
        let pts = px
            .iter()
            .zip(&m)
            .zip(s)
            .map(|((&(x, y), p), s)| LinePt {
                st: lattice.stencil(x, y),
                n,
                e0: n.0 * (p.0 - c.0) + n.1 * (p.1 - c.1),
                s,
            })
            .collect();
        out.push(TracedLine { pts, kind });
    }
    out.retain(|l| l.pts.len() >= 8);
    out
}

struct Lines {
    lines: Vec<TracedLine>,
    /// Residual scale (Mercator metres) and weight per point.
    sigma: f64,
    dens: f64,
}

impl Lines {
    fn cost(&self, x: &[f64]) -> f64 {
        let k = 2.0 * self.sigma;
        let sum: f64 = self.lines.iter().flat_map(|l| l.residuals(x)).map(|e| soft_l1(e, k)).sum();
        self.dens * sum / self.sigma.powi(2)
    }

    /// Normal equations at x (robust weights from the residuals there),
    /// leaving out line `skip`.
    fn normal(&self, x: &[f64], n: usize, skip: Option<usize>) -> Normal {
        let k = 2.0 * self.sigma;
        let mut nm = Normal::new(n);
        let mut scratch = Vec::new();
        for (li, l) in self.lines.iter().enumerate() {
            if Some(li) == skip {
                continue;
            }
            let rows: Vec<Row> = l
                .pts
                .iter()
                .zip(l.residuals(x))
                .map(|(p, e)| Row::along(&p.st, p.n, -p.e0, self.dens * soft_l1_weight(e, k) / self.sigma.powi(2)))
                .collect();
            match l.kind {
                LineKind::Valued => rows.iter().for_each(|r| nm.add_row(r)),
                LineKind::Parallel => nm.add_group(&rows, None, &mut scratch),
                LineKind::Straight => {
                    let s: Vec<f64> = l.pts.iter().map(|p| p.s).collect();
                    nm.add_group(&rows, Some(&s), &mut scratch);
                }
            }
        }
        nm
    }

    fn valued(&self) -> impl Iterator<Item = (usize, &TracedLine)> {
        self.lines.iter().enumerate().filter(|l| l.1.kind == LineKind::Valued)
    }
}

/// Contour samples prepared for the warp fit.
struct Contours {
    st: Vec<Stencil>,
    /// H(p) of every sample.
    hm: Vec<(f64, f64)>,
    /// Sample indices of every segment.
    segs: Vec<Vec<usize>>,
    dens: f64,
}

/// One evaluation of the contour term.
struct ContourEval {
    /// Elevation, gradient (per Mercator metre) and residual from the
    /// segment's level of every sample (NaN where unused).
    elev: Vec<f64>,
    grad: Vec<(f64, f64)>,
    resid: Vec<f64>,
    /// Robust cost (χ² units) of the evaluated segments, and the mean
    /// |residual| clipped at 12 m over their used samples.
    cost: f64,
    mean_abs_m: f64,
    median_seg_std_m: f64,
}

impl Contours {
    fn new(s: &ContourSamples, lattice: &Warp, t: &Homography, w: usize, h: usize) -> Contours {
        let stride = s.pts.len().div_ceil(MAX_SAMPLES).max(1);
        let mut segs: Vec<Vec<usize>> = vec![Vec::new(); s.n_segs];
        let mut st = Vec::new();
        let mut hm = Vec::new();
        for i in (0..s.pts.len()).step_by(stride) {
            let (x, y) = (s.pts[i][0] as f64, s.pts[i][1] as f64);
            if let Some(seg) = segs.get_mut(s.seg[i] as usize) {
                seg.push(st.len());
                st.push(lattice.stencil(x, y));
                hm.push(t.apply(x, y));
            }
        }
        segs.retain(|s| s.len() >= MIN_SEG_SAMPLES);
        // Samples are a few pixels apart along a contour; DEM errors are
        // correlated over a cell or so.
        let (_, step) = crate::skeleton::default_sampling(w, h);
        let dens = ((step * stride) as f64 / CONTOUR_CORRELATION_PX).min(1.0);
        Contours { st, hm, segs, dens }
    }

    fn n_samples(&self) -> usize {
        self.segs.iter().map(Vec::len).sum()
    }

    /// Evaluate the segments `which(seg)` selects on `dem` under offsets x
    /// with loss scale c.
    fn evaluate(&self, dem: &DemMosaic, x: &[f64], c: f64, which: &dyn Fn(usize) -> bool) -> ContourEval {
        let n = self.st.len();
        let samp: Vec<Option<(f32, f32, f32)>> = (0..n)
            .into_par_iter()
            .map(|i| {
                let (dx, dy) = offset_of(x, &self.st[i]);
                dem.sample_with_grad(self.hm[i].0 + dx, self.hm[i].1 + dy)
            })
            .collect();
        let mut elev = vec![f64::NAN; n];
        let mut grad = vec![(0.0, 0.0); n];
        let mut resid = vec![f64::NAN; n];
        let (mut cost, mut sum_abs, mut used) = (0.0, 0.0, 0usize);
        let penalty = soft_l1(MISSING_PENALTY_M, c);
        let mut stds = Vec::new();
        for (si, seg) in self.segs.iter().enumerate() {
            if !which(si) {
                continue;
            }
            let valid: Vec<usize> = seg.iter().copied().filter(|&i| samp[i].is_some()).collect();
            cost += penalty * (seg.len() - if valid.len() >= MIN_SEG_SAMPLES { valid.len() } else { 0 }) as f64;
            if valid.len() < MIN_SEG_SAMPLES {
                continue;
            }
            let hv: Vec<f64> = valid.iter().map(|&i| samp[i].unwrap().0 as f64).collect();
            let m0 = hv.iter().sum::<f64>() / hv.len() as f64;
            let (mut sw, mut sh) = (0.0, 0.0);
            for &v in &hv {
                let w = soft_l1_weight(v - m0, c);
                sw += w;
                sh += w * v;
            }
            let level = sh / sw;
            let var = hv.iter().map(|v| (v - m0).powi(2)).sum::<f64>() / hv.len() as f64;
            stds.push(var.sqrt());
            for (&i, &v) in valid.iter().zip(&hv) {
                let (_, gx, gy) = samp[i].unwrap();
                let r = v - level;
                elev[i] = v;
                grad[i] = (gx as f64, gy as f64);
                resid[i] = r;
                cost += soft_l1(r, c);
                sum_abs += r.abs().min(12.0);
                used += 1;
            }
        }
        let median_seg_std_m = median(&mut stds).unwrap_or(f64::INFINITY);
        ContourEval {
            elev,
            grad,
            resid,
            cost: cost * self.dens / CONTOUR_SIGMA_M.powi(2),
            mean_abs_m: if used > 0 { sum_abs / used as f64 } else { f64::INFINITY },
            median_seg_std_m,
        }
    }

    /// Normal equations of the evaluated segments linearised at x (the
    /// segment levels eliminated), robust weights at loss scale c.
    fn normal(&self, ev: &ContourEval, x: &[f64], c: f64, n: usize, which: &dyn Fn(usize) -> bool) -> Normal {
        let mut nm = Normal::new(n);
        let mut rows = Vec::new();
        let mut scratch = Vec::new();
        for (si, seg) in self.segs.iter().enumerate() {
            if !which(si) {
                continue;
            }
            rows.clear();
            for &i in seg {
                if !ev.resid[i].is_finite() {
                    continue;
                }
                let w = self.dens * soft_l1_weight(ev.resid[i], c) / CONTOUR_SIGMA_M.powi(2);
                let mut row = Row::along(&self.st[i], ev.grad[i], 0.0, w);
                row.t = row.dot(x) - ev.elev[i];
                rows.push(row);
            }
            if rows.len() >= MIN_SEG_SAMPLES {
                nm.add_group(&rows, None, &mut scratch);
            }
        }
        nm
    }
}

fn median(v: &mut [f64]) -> Option<f64> {
    if v.is_empty() {
        return None;
    }
    let k = v.len() / 2;
    Some(*v.select_nth_unstable_by(k, |a, b| a.total_cmp(b)).1)
}

// ---------------------------------------------------------------------------
// Fitting
// ---------------------------------------------------------------------------

/// Gauss–Newton iterations on the coarser and the finest DEM level.
const GN_ITERATIONS: [usize; 2] = [8, 5];
/// …stopping when an iteration lowers the objective by less than this fraction.
const GN_MIN_GAIN: f64 = 1e-3;
/// The warp is kept when the grid lines, each predicted by the warp fitted
/// without it, end up at most this fraction of their RMS distance under H
/// alone…
const LINE_HOLDOUT_RATIO: f64 = 0.85;
/// …and at least this many pixels (at the map scale) closer…
const LINE_HOLDOUT_MIN_GAIN_PX: f64 = 1.0;
/// …and when the robust cost of each half of the contour segments (split by
/// segment parity) drops by at least this fraction under the warp fitted
/// without that half.
const CONTOUR_HOLDOUT_GAIN: f64 = 0.06;
/// Held-out line residuals are clipped here (Mercator metres) so a stray
/// trace point cannot decide.
const HOLDOUT_CLIP_M: f64 = 150.0;

/// Fit a warp on top of `input.transform`; see the module docs.
pub fn fit_warp(input: &WarpInput) -> WarpFit {
    let t0 = Instant::now();
    let lattice = Warp::zero(input.w, input.h);
    let n = 2 * lattice.n_nodes();
    let t = &input.transform;
    let (cx, cy) = (input.w as f64 / 2.0, input.h as f64 / 2.0);
    let merc_px = t.local_affine(cx, cy).det().abs().sqrt();
    let ground = merc_to_latlon(0.0, t.apply(cx, cy).1).0.to_radians().cos();
    let mut diag = WarpDiagnostics {
        source: if input.grid.is_some() { "grid" } else { "contours" }.into(),
        cols: lattice.cols,
        rows: lattice.rows,
        ..Default::default()
    };
    let done = |mut diag: WarpDiagnostics, warp: Option<Warp>, why: Option<&str>| {
        diag.rejected = why.map(str::to_string);
        diag.accepted = warp.is_some();
        diag.ms = round_dp(t0.elapsed().as_secs_f64() * 1e3, 1);
        WarpFit { warp: warp.map(Warp::rounded), diag }
    };
    if !(merc_px.is_finite() && merc_px > 0.0 && ground.is_finite() && ground > 0.0) {
        return done(diag, None, Some("degenerate transform"));
    }
    let cell_m = merc_px * 0.5 * (lattice.spacing[0] + lattice.spacing[1]);
    let smooth = Smooth::new(&lattice, (SMOOTH_SLOPE * cell_m).powi(-2), PRIOR_OFFSET_M.powi(-2));
    let reg = smooth.normal();
    let lines = Some(Lines {
        lines: traced_lines(input, &lattice),
        sigma: LINE_SIGMA_PX * merc_px,
        dens: (LINE_STEP_PX / CORRELATION_PX).min(1.0),
    })
    .filter(|l| !l.lines.is_empty());
    let contours = input
        .contours
        .as_ref()
        .map(|c| (Contours::new(c.samples, &lattice, t, input.w, input.h), c.dem))
        .filter(|(c, dem)| c.segs.len() >= 10 && !dem.levels.is_empty());
    if let Some(l) = &lines {
        diag.n_lines = l.valued().count();
        diag.n_straight_lines = l.lines.len() - diag.n_lines;
        diag.n_line_points = l.lines.iter().map(|l| l.pts.len()).sum();
    }
    if let Some((c, _)) = &contours {
        diag.n_segments = c.segs.len();
        diag.n_samples = c.n_samples();
    }
    let all = |_: usize| true;
    let line_cost = |x: &[f64]| lines.as_ref().map_or(0.0, |l| l.cost(x));
    let mut x = vec![0.0; n];
    match (&contours, &lines) {
        (None, None) => return done(diag, None, Some("no constraints")),
        (None, Some(l)) => {
            // Linear: a few robust re-weightings.
            for _ in 0..4 {
                let mut nm = l.normal(&x, n, None);
                nm.add(&reg);
                match nm.solve() {
                    Some(s) => x = s,
                    None => return done(diag, None, Some("singular system")),
                }
                diag.iterations += 1;
            }
        }
        (Some((c, dem)), _) => {
            // Coarse-to-fine Gauss–Newton with Levenberg–Marquardt damping.
            let top = dem.levels.len().min(GN_ITERATIONS.len()) - 1;
            for lvl in (0..=top).rev() {
                let d = &dem.levels[lvl];
                let loss = CONTOUR_SIGMA_M * (1u32 << lvl) as f64;
                let mut ev = c.evaluate(d, &x, loss, &all);
                let mut f = line_cost(&x) + ev.cost + smooth.cost(&x);
                let mut nu = 1e-3;
                for _ in 0..GN_ITERATIONS[GN_ITERATIONS.len() - 1 - lvl.min(GN_ITERATIONS.len() - 1)] {
                    let mut nm = c.normal(&ev, &x, loss, n, &all);
                    if let Some(l) = &lines {
                        nm.add(&l.normal(&x, n, None));
                    }
                    nm.add(&reg);
                    let mut gain = None;
                    for _ in 0..5 {
                        let mut damped = nm.clone();
                        for (i, xi) in x.iter().enumerate() {
                            let a = damped.a[i * n + i];
                            damped.a[i * n + i] += nu * a;
                            damped.b[i] += nu * a * xi;
                        }
                        diag.iterations += 1;
                        let Some(xn) = damped.solve() else {
                            nu *= 10.0;
                            continue;
                        };
                        let evn = c.evaluate(d, &xn, loss, &all);
                        let fnew = line_cost(&xn) + evn.cost + smooth.cost(&xn);
                        if fnew < f {
                            gain = Some((f - fnew) / f.max(1e-12));
                            (x, ev, f) = (xn, evn, fnew);
                            nu = (nu / 3.0).max(1e-6);
                            break;
                        }
                        nu *= 4.0;
                    }
                    if gain.is_none_or(|g| g < GN_MIN_GAIN) {
                        break;
                    }
                }
            }
        }
    }

    // Held-out checks at the final linearisation.
    let fine = contours.as_ref().map(|(c, dem)| (c, &dem.levels[0]));
    let ev_fin = fine.map(|(c, d)| c.evaluate(d, &x, CONTOUR_SIGMA_M, &all));
    let contour_normal = |keep: &dyn Fn(usize) -> bool| -> Option<Normal> {
        let (c, _) = fine?;
        Some(c.normal(ev_fin.as_ref()?, &x, CONTOUR_SIGMA_M, n, keep))
    };
    let mut why: Option<&str> = None;
    if fine.is_none() && lines.as_ref().is_none_or(|l| l.valued().count() < 2) {
        // Straight lines alone say nothing about where the map lies.
        why = Some("nothing to check the warp against");
    }
    if let Some(l) = lines.as_ref().filter(|l| l.valued().count() >= 2) {
        let rms = |e: &mut dyn Iterator<Item = f64>| -> f64 {
            let (mut s, mut k) = (0.0, 0usize);
            for v in e {
                s += v * v;
                k += 1;
            }
            (s / k.max(1) as f64).sqrt() * ground
        };
        let zero = vec![0.0; n];
        diag.rms_before_m = Some(round_dp(rms(&mut l.valued().flat_map(|(_, v)| v.residuals(&zero))), 1));
        diag.rms_after_m = Some(round_dp(rms(&mut l.valued().flat_map(|(_, v)| v.residuals(&x))), 1));
        let others = contour_normal(&all);
        let (mut before, mut after) = (Vec::new(), Vec::new());
        for (li, tl) in l.valued() {
            let mut nm = l.normal(&x, n, Some(li));
            nm.add(&reg);
            if let Some(o) = &others {
                nm.add(o);
            }
            let Some(xl) = nm.solve() else { continue };
            before.extend(tl.residuals(&zero).into_iter().map(|e| e.clamp(-HOLDOUT_CLIP_M, HOLDOUT_CLIP_M)));
            after.extend(tl.residuals(&xl).into_iter().map(|e| e.clamp(-HOLDOUT_CLIP_M, HOLDOUT_CLIP_M)));
        }
        let (rb, ra) = (rms(&mut before.into_iter()), rms(&mut after.into_iter()));
        diag.holdout_rms_m = Some(round_dp(ra, 1));
        diag.holdout_before_m = Some(round_dp(rb, 1));
        if !(ra <= LINE_HOLDOUT_RATIO * rb && rb - ra >= LINE_HOLDOUT_MIN_GAIN_PX * merc_px * ground) {
            why = Some("grid lines left out do not agree with the warp");
        }
    }
    if let (Some((c, d)), Some(ev)) = (fine, &ev_fin) {
        let zero = vec![0.0; n];
        let ev0 = c.evaluate(d, &zero, CONTOUR_SIGMA_M, &all);
        diag.contour_cost_before_m = Some(round_dp(ev0.mean_abs_m, 3));
        diag.contour_cost_after_m = Some(round_dp(ev.mean_abs_m, 3));
        diag.median_seg_std_m = Some(round_dp(ev.median_seg_std_m, 3));
        let line_n = lines.as_ref().map(|l| l.normal(&x, n, None));
        let mut worst = f64::INFINITY;
        for fold in 0..2usize {
            let Some(mut nm) = contour_normal(&|s| s % 2 != fold) else { continue };
            nm.add(&reg);
            if let Some(ln) = &line_n {
                nm.add(ln);
            }
            let gain = match nm.solve() {
                Some(xf) => {
                    let held = |s: usize| s % 2 == fold;
                    let c0 = c.evaluate(d, &zero, CONTOUR_SIGMA_M, &held).cost;
                    let c1 = c.evaluate(d, &xf, CONTOUR_SIGMA_M, &held).cost;
                    1.0 - c1 / c0.max(1e-12)
                }
                None => f64::NEG_INFINITY,
            };
            worst = worst.min(gain);
        }
        diag.contour_holdout_gain = Some(round_dp(worst, 4));
        if worst.is_nan() || worst < CONTOUR_HOLDOUT_GAIN {
            why = why.or(Some("contour segments left out do not agree with the warp"));
        }
    }
    let warp = Warp { offsets_m: x, ..lattice };
    // Largest offset where the map's data are (the lattice beyond them,
    // e.g. at the page margins, is extrapolated).
    let at = |st: &Stencil| {
        let (dx, dy) = offset_of(&warp.offsets_m, st);
        dx.hypot(dy)
    };
    let mut max_supported =
        lines.iter().flat_map(|l| &l.lines).flat_map(|l| &l.pts).map(|p| at(&p.st)).fold(0.0, f64::max);
    if let Some((c, _)) = &contours {
        max_supported = c.st.iter().map(at).fold(max_supported, f64::max);
    }
    diag.max_offset_m = round_dp(max_supported * ground, 1);
    diag.derivative_ratio = round_dp(warp.derivative_ratio(t), 4);
    if diag.max_offset_m > MAX_OFFSET_M {
        why = why.or(Some("implausibly large offsets"));
    }
    if diag.derivative_ratio.is_nan() || diag.derivative_ratio > MAX_DERIVATIVE_RATIO {
        why = why.or(Some("the warp would fold"));
    }
    match why {
        Some(w) => done(diag, None, Some(w)),
        None => done(diag, Some(warp), None),
    }
}

fn round_dp(v: f64, decimals: i32) -> f64 {
    let k = 10f64.powi(decimals);
    if v.is_finite() {
        (v * k).round() / k
    } else {
        v
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::grid::{Bend, BEND_KNOTS};

    /// Deterministic pseudo-random numbers in [0, 1).
    fn rng(seed: &mut u64) -> f64 {
        *seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
        (*seed >> 11) as f64 / (1u64 << 53) as f64
    }

    #[test]
    fn layout_follows_the_contract() {
        let w = Warp::zero(1703, 2232);
        assert_eq!((w.cols, w.rows), (15, LONG_SIDE_NODES));
        assert_eq!(w.origin, [0.0, 0.0]);
        assert!((w.spacing[0] - 1703.0 / 14.0).abs() < 1e-12 && (w.spacing[1] - 2232.0 / 19.0).abs() < 1e-12);
        assert_eq!(w.offsets_m.len(), 2 * 15 * 20);
        assert!(w.is_valid());
        let w = Warp::zero(4000, 1000);
        assert_eq!((w.cols, w.rows), (LONG_SIDE_NODES, 6));
        let w = Warp::zero(10, 1);
        assert!(w.cols >= 2 && w.rows >= 2 && w.is_valid());
    }

    #[test]
    fn cholesky_solves_spd_systems() {
        let mut seed = 7;
        let n = 37;
        let m: Vec<f64> = (0..n * n).map(|_| rng(&mut seed) - 0.5).collect();
        // A = MᵀM + I
        let mut a = vec![0.0; n * n];
        for i in 0..n {
            for j in 0..n {
                a[i * n + j] =
                    (0..n).map(|k| m[k * n + i] * m[k * n + j]).sum::<f64>() + if i == j { 1.0 } else { 0.0 };
            }
        }
        let x0: Vec<f64> = (0..n).map(|_| rng(&mut seed) * 10.0 - 5.0).collect();
        let b: Vec<f64> = (0..n).map(|i| dot(&a[i * n..(i + 1) * n], &x0)).collect();
        let x = cholesky_solve(&mut a.clone(), n, &b).unwrap();
        assert!(x.iter().zip(&x0).all(|(p, q)| (p - q).abs() < 1e-9), "{x:?}");
        // Not positive definite.
        let mut z = vec![1.0, 0.0, 0.0, -1.0];
        assert!(cholesky_solve(&mut z, 2, &[1.0, 1.0]).is_none());
    }

    /// Eliminating a group's free offset (and slope) in closed form gives the
    /// same solution as solving for them explicitly.
    #[test]
    fn group_elimination_matches_explicit_free_parameters() {
        let mut seed = 3;
        let n = 12;
        for with_slope in [false, true] {
            let groups: Vec<Vec<(Row, f64)>> = (0..5)
                .map(|_| {
                    (0..9)
                        .map(|_| {
                            let mut idx = [0usize; 8];
                            let mut val = [0.0; 8];
                            for k in 0..8 {
                                idx[k] = (rng(&mut seed) * n as f64) as usize;
                                val[k] = rng(&mut seed) - 0.5;
                            }
                            let row = Row { idx, val, t: rng(&mut seed) * 4.0 - 2.0, w: 0.5 + rng(&mut seed) };
                            (row, rng(&mut seed) * 2.0 - 1.0)
                        })
                        .collect()
                })
                .collect();
            // Closed form.
            let mut nm = Normal::new(n);
            let mut scratch = Vec::new();
            for g in &groups {
                let rows: Vec<Row> = g.iter().map(|r| r.0).collect();
                let s: Vec<f64> = g.iter().map(|r| r.1).collect();
                nm.add_group(&rows, with_slope.then_some(&s[..]), &mut scratch);
            }
            for i in 0..n {
                nm.a[i * n + i] += 0.1;
            }
            let x = nm.solve().unwrap();
            // Explicit: unknowns x, then (c₀, c₁) per group.
            let k = if with_slope { 2 } else { 1 };
            let na = n + k * groups.len();
            let mut a = vec![0.0; na * na];
            let mut b = vec![0.0; na];
            for (gi, g) in groups.iter().enumerate() {
                for (r, s) in g {
                    let mut full = vec![0.0; na];
                    for q in 0..8 {
                        full[r.idx[q]] += r.val[q];
                    }
                    full[n + k * gi] = -1.0;
                    if with_slope {
                        full[n + k * gi + 1] = -s;
                    }
                    for i in 0..na {
                        b[i] += r.w * full[i] * r.t;
                        for j in 0..na {
                            a[i * na + j] += r.w * full[i] * full[j];
                        }
                    }
                }
            }
            for i in 0..n {
                a[i * na + i] += 0.1;
            }
            let xe = cholesky_solve(&mut a, na, &b).unwrap();
            for i in 0..n {
                assert!((x[i] - xe[i]).abs() < 1e-8, "slope {with_slope}: {} vs {}", x[i], xe[i]);
            }
        }
    }

    #[test]
    fn bending_energy_ignores_affine_offsets() {
        let w = Warp::zero(900, 1300);
        let sm = Smooth::new(&w, 1.0, 0.0);
        let x: Vec<f64> = (0..w.n_nodes())
            .flat_map(|k| {
                let (px, py) = w.node_px(k);
                [3.0 + 0.02 * px - 0.01 * py, -1.0 + 0.005 * px + 0.03 * py]
            })
            .collect();
        assert!(sm.cost(&x) < 1e-12, "{}", sm.cost(&x));
        let bent: Vec<f64> = x.iter().enumerate().map(|(i, v)| v + if i == 2 * 40 { 5.0 } else { 0.0 }).collect();
        assert!(sm.cost(&bent) > 10.0);
        // The normal equations are the same quadratic form.
        let reg = sm.normal();
        let n = reg.n;
        let q: f64 = (0..n).map(|i| bent[i] * dot(&reg.a[i * n..(i + 1) * n], &bent)).sum();
        assert!((q - sm.cost(&bent)).abs() < 1e-9 * q.max(1.0));
    }

    /// A dark grey line, kinked like at a fold crease, crossed by brown
    /// contours and with a grey dash beside it: the trace follows the line
    /// from a straight course that misses the kink.
    #[test]
    fn trace_follows_a_kinked_line() {
        let (w, h) = (900usize, 300usize);
        let mut img = Rgb8::from_rgb(w, h, vec![250u8; w * h * 3]).unwrap();
        let truth = |x: f64| if x < 450.0 { 150.0 } else { 150.0 + 0.04 * (x - 450.0) };
        let mut put = |x: usize, y: usize, c: [u8; 3]| {
            let i = (y * w + x) * 3;
            img.data[i..i + 3].copy_from_slice(&c);
        };
        for x in 0..w {
            // A line ~2 px wide, anti-aliased: pixel row y covers [y, y + 1).
            let top = truth(x as f64 + 0.5) - 1.0;
            for yy in (top.floor() as usize)..=(top.floor() as usize + 2) {
                let cover = ((yy as f64 + 1.0).min(top + 2.0) - (yy as f64).max(top)).clamp(0.0, 1.0);
                let v = (250.0 - 170.0 * cover) as u8;
                put(x, yy, [v, v, v]);
            }
            // Brown contours crossing.
            if x % 60 < 2 {
                for yy in 100..200 {
                    put(x, yy, [150, 110, 80]);
                }
            }
            // A grey dash 9 px above, over part of the line.
            if (200..260).contains(&x) {
                put(x, 141, [90, 90, 90]);
            }
        }
        let course = GridLine::from_slope(LineAxis::Horizontal, 449.5, 149.5, 0.0, 1.0);
        let pts = trace_printed_line(&img, &course);
        assert!(pts.len() as f64 * LINE_STEP_PX > 0.85 * w as f64, "{} points", pts.len());
        for &(x, y) in &pts {
            // Pixel-index coordinates: the line's centre row index is truth − 0.5.
            let e = y - (truth(x + 0.5) - 0.5);
            assert!(e.abs() < 0.5, "({x}, {y}): {e:.2} px off");
        }
    }

    /// A horizontal or vertical grid line through `across(s)` (as `grid.rs`
    /// traces one: a chord plus knot offsets).
    fn bent_line(axis: LineAxis, n: usize, across: impl Fn(f64) -> f64) -> GridLine {
        let ds = (n - 1) as f64 / (BEND_KNOTS - 1) as f64;
        let knots: Vec<f64> = (0..BEND_KNOTS).map(|k| across(k as f64 * ds)).collect();
        let p0 = knots.iter().sum::<f64>() / BEND_KNOTS as f64;
        let mid = (n as f64 - 1.0) / 2.0;
        let mut l = GridLine::from_slope(axis, mid, p0, 0.0, 1.0);
        l.bend =
            Some(Bend { ds, offsets: std::array::from_fn(|k| (knots[k] - p0) as f32), lo: 0.0, hi: (n - 1) as f64 });
        l
    }

    /// Graticule lines of a sheet whose paper bends by `b` (Mercator metres)
    /// on top of a north-up 4 m/px homography.
    type Bending<'a> = &'a dyn Fn(f64, f64) -> (f64, f64);

    fn bent_graticule(w: usize, h: usize, b: Bending) -> (Homography, Vec<ValuedLine>) {
        let (x0, y0) = latlon_to_merc(-25.80, 27.36);
        let t = Homography { m: [4.0, 0.0, x0, 0.0, -4.0, y0, 0.0, 0.0, 1.0] };
        let mut lines = Vec::new();
        for k in 0..5 {
            let target = y0 - 4.0 * (150.0 + 300.0 * k as f64);
            let line = bent_line(LineAxis::Horizontal, w, |x| {
                let mut y = 0.0;
                for _ in 0..30 {
                    y = (y0 - target + b(x, y).1) / 4.0;
                }
                y
            });
            lines.push(ValuedLine { line, value: merc_to_latlon(x0, target).0, labelled: true });
        }
        for k in 0..4 {
            let target = x0 + 4.0 * (100.0 + 330.0 * k as f64);
            let line = bent_line(LineAxis::Vertical, h, |y| {
                let mut x = 0.0;
                for _ in 0..30 {
                    x = (target - x0 - b(x, y).0) / 4.0;
                }
                x
            });
            lines.push(ValuedLine { line, value: merc_to_latlon(target, y0).1, labelled: true });
        }
        (t, lines)
    }

    #[test]
    fn grid_lines_recover_a_smooth_bend_and_a_flat_sheet_gets_none() {
        let (w, h) = (1200usize, 1500usize);
        let bend = |x: f64, y: f64| {
            let (u, v) = (x / w as f64, y / h as f64);
            (30.0 * (std::f64::consts::PI * v).sin() * u, 25.0 * (std::f64::consts::TAU * u).sin() * (0.5 + v))
        };
        let (t, lines) = bent_graticule(w, h, &bend);
        let fit_lines = |lines: &[ValuedLine], straight: &[GridLine]| -> WarpFit {
            fit_warp(&WarpInput {
                w,
                h,
                transform: t,
                image: None,
                grid: (!lines.is_empty()).then_some(GridLines { lines, utm_zone: None }),
                straight,
                contours: None,
            })
        };
        let fit = fit_lines(&lines, &[]);
        let warp = fit.warp.unwrap_or_else(|| panic!("{:?}", fit.diag));
        assert_eq!(fit.diag.n_lines, 9);
        assert!(fit.diag.derivative_ratio < 0.1, "{:?}", fit.diag);
        // Across each line the warp puts it on its value…
        for vl in &lines {
            let n = if vl.line.axis == LineAxis::Horizontal { w } else { h };
            for s in (0..n).step_by(50) {
                let (x, y) = vl.line.point_at(s as f64);
                let (bx, by) = bend(x, y);
                let (wx, wy) = warp.offset(x, y);
                let e = if vl.line.axis == LineAxis::Horizontal { wy - by } else { wx - bx };
                assert!(e.abs() < 4.0, "{:?} line at ({x:.0}, {y:.0}): {e:.1} m", vl.line.axis);
            }
        }
        // …and between the lines it is about right too.
        for (x, y) in [(600.0, 600.0), (300.0, 900.0), (900.0, 400.0)] {
            let (bx, by) = bend(x, y);
            let (wx, wy) = warp.offset(x, y);
            assert!((wx - bx).hypot(wy - by) < 6.0, "({x}, {y}): {:?} vs {:?}", (wx, wy), (bx, by));
        }
        let (before, held_out) = (fit.diag.holdout_before_m.unwrap(), fit.diag.holdout_rms_m.unwrap());
        assert!(held_out < 0.5 * before, "held out {held_out} vs {before}");
        // A flat sheet: nothing to correct, no warp.
        let (_, flat) = bent_graticule(w, h, &|_, _| (0.0, 0.0));
        let fit = fit_lines(&flat, &[]);
        assert!(fit.warp.is_none(), "{:?}", fit.diag);
        assert_eq!(fit.diag.rejected.as_deref(), Some("grid lines left out do not agree with the warp"));
        // Straight lines alone cannot place a warp.
        let straight: Vec<GridLine> = lines.iter().map(|l| l.line).collect();
        let fit = fit_lines(&[], &straight);
        assert!(fit.warp.is_none() && fit.diag.n_straight_lines == 9, "{:?}", fit.diag);
    }
}
