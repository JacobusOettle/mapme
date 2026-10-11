//! Grid / graticule georeferencing from straight grid lines + OCR labels.
//!
//! OWNER: grid agent. Contract (may be extended, not broken):
//!
//! *Line detection*: every thin dark (or strongly coloured) run across a row or
//! column is reduced to one sub-pixel centre. A projection-profile Hough over
//! ±5° then finds long straight lines, refined at 0.02° and by least squares.
//!
//! *Georeferencing*: coordinate labels are paired with nearby lines of their
//! axis and an exhaustive RANSAC fits `value = a + b·position` per axis.
//! Unlabelled lines that land on round grid values join in. Line
//! intersections become GCPs for an affine fit.

use crate::geo::{
    latlon_to_merc, latlon_to_utm, merc_to_latlon, utm_to_latlon, Affine, Homography, LatLon, Utm, EARTH_RADIUS,
};
use crate::image::Rgb8;
use crate::labels::{parse_coordinate, CoordValue, OcrObservation};
use rayon::prelude::*;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LineAxis {
    /// Roughly horizontal in the image (parallels / northings).
    Horizontal,
    /// Roughly vertical in the image (meridians / eastings).
    Vertical,
}

/// A long straight line: points p with p·n = rho, n = (cos θ, sin θ).
/// θ is in radians: ≈ π/2 for horizontal lines, ≈ 0 for vertical ones.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct GridLine {
    pub axis: LineAxis,
    pub theta: f64,
    pub rho: f64,
    /// Fraction of the image span covered by line pixels.
    pub coverage: f64,
}

impl GridLine {
    /// Line `p = p0 + (s − mid)·slope`, where `s` runs along the line (x for
    /// horizontal lines, y for vertical ones) and `p` across it.
    pub fn from_slope(axis: LineAxis, mid: f64, p0: f64, slope: f64, coverage: f64) -> GridLine {
        let alpha = slope.atan();
        let (sa, ca) = alpha.sin_cos();
        let rho = ca * p0 - sa * mid;
        let theta = match axis {
            LineAxis::Horizontal => std::f64::consts::FRAC_PI_2 + alpha,
            LineAxis::Vertical => -alpha,
        };
        GridLine { axis, theta, rho, coverage }
    }

    /// Position across the line at `s` along it: y at x = s for horizontal
    /// lines, x at y = s for vertical lines.
    pub fn pos_at(&self, s: f64) -> f64 {
        let (sn, cs) = self.theta.sin_cos();
        match self.axis {
            LineAxis::Horizontal => (self.rho - cs * s) / sn,
            LineAxis::Vertical => (self.rho - sn * s) / cs,
        }
    }

    /// Intersection with another line (None when parallel).
    pub fn intersect(&self, o: &GridLine) -> Option<(f64, f64)> {
        let (s1, c1) = self.theta.sin_cos();
        let (s2, c2) = o.theta.sin_cos();
        let det = c1 * s2 - s1 * c2;
        if det.abs() < 1e-9 {
            return None;
        }
        Some(((self.rho * s2 - s1 * o.rho) / det, (c1 * o.rho - c2 * self.rho) / det))
    }

    /// Rotation from the image axis in degrees, positive = clockwise on
    /// screen. A map rotated by φ gives φ for both families.
    pub fn tilt_deg(&self) -> f64 {
        match self.axis {
            LineAxis::Horizontal => (self.theta - std::f64::consts::FRAC_PI_2).to_degrees(),
            LineAxis::Vertical => self.theta.to_degrees(),
        }
    }
}

// ---------------------------------------------------------------------------
// Line detection
// ---------------------------------------------------------------------------

/// Minimum fraction of the image span a dark line must cover.
pub const MIN_COVERAGE: f64 = 0.35;
/// Coloured lines (blue UTM grids) must be longer: rivers and roads are coloured too.
const COLOUR_MIN_COVERAGE: f64 = 0.5;
/// Longest run across a line still treated as thin (pixels).
const MAX_RUN: usize = 7;
/// A thin run needs this many non-ink pixels to the next run on both sides.
const RUN_GAP: usize = 2;
const MAX_ANGLE_DEG: f64 = 5.0;
const COARSE_STEP_DEG: f64 = 0.25;
/// Profile window (± bins) absorbing the drift of a line between coarse angles.
const COARSE_WIN: usize = 4;
const FINE_RANGE_DEG: f64 = 0.3;
const FINE_STEP_DEG: f64 = 0.02;
/// Half-width of the band collected around a coarse line for refinement.
const BAND_PX: f64 = 14.0;
/// Points within this distance of a line are its inliers.
const INLIER_PX: f64 = 1.5;
/// Parallel lines closer than this are one line (double frame lines, thick borders).
const MERGE_PX: f64 = 16.0;
/// Largest hole in a line (fraction of the span), e.g. under a legend panel.
/// Separate boxes that happen to line up (legend and scale-bar frames) leave
/// a bigger one.
const MAX_GAP: f64 = 0.4;

#[derive(Clone, Copy)]
enum InkClass {
    /// Black / grey, low saturation.
    Dark,
    /// Strongly saturated colour (blue or magenta grids).
    Colour,
}

/// What makes a run of ink pixels a line crossing: every pixel at least
/// `weak`, the darkest at least `peak`, and the summed ink at least `mass`
/// (a thin anti-aliased line in a downscaled screenshot is a pair of light
/// grey pixels, but carries as much ink as one black pixel).
#[derive(Clone, Copy)]
struct RunSpec {
    weak: u8,
    peak: u8,
    mass: u32,
}

impl InkClass {
    fn spec(self) -> RunSpec {
        match self {
            InkClass::Dark => RunSpec { weak: 30, peak: 60, mass: 64 },
            InkClass::Colour => RunSpec { weak: 30, peak: 90, mass: 150 },
        }
    }
}

/// Per-pixel ink level for a class (0 = background).
fn ink_plane(img: &Rgb8, class: InkClass) -> Vec<u8> {
    let mut out = vec![0u8; img.w * img.h];
    out.par_chunks_mut(img.w).enumerate().for_each(|(y, row)| {
        let src = &img.data[y * img.w * 3..(y + 1) * img.w * 3];
        for (o, px) in row.iter_mut().zip(src.chunks_exact(3)) {
            let mx = px[0].max(px[1]).max(px[2]);
            let mn = px[0].min(px[1]).min(px[2]);
            let chroma = (mx - mn) as u32;
            *o = match class {
                InkClass::Dark => {
                    // Dark pixels may be tinted by what they are printed over;
                    // light ones must be grey (brown contours are not).
                    let neutral = if mx <= 110 { chroma <= 60 } else { chroma * 10 <= mx as u32 + 80 };
                    if neutral {
                        255 - mx
                    } else {
                        0
                    }
                }
                InkClass::Colour => {
                    if chroma >= 30 && chroma * 5 >= mx as u32 * 2 {
                        chroma as u8
                    } else {
                        0
                    }
                }
            };
        }
    });
    out
}

/// Sub-pixel centres of thin runs, grouped by scan line (CSR layout).
struct ScanPoints {
    start: Vec<usize>,
    pos: Vec<f32>,
}

impl ScanPoints {
    fn from_lists(lists: Vec<Vec<f32>>) -> ScanPoints {
        let total = lists.iter().map(Vec::len).sum();
        let mut start = Vec::with_capacity(lists.len() + 1);
        let mut pos = Vec::with_capacity(total);
        start.push(0);
        for l in lists {
            pos.extend_from_slice(&l);
            start.push(pos.len());
        }
        ScanPoints { start, pos }
    }

    fn n_scans(&self) -> usize {
        self.start.len() - 1
    }

    fn scan(&self, s: usize) -> &[f32] {
        &self.pos[self.start[s]..self.start[s + 1]]
    }
}

/// Centres (ink-weighted, including the faint neighbours) of isolated runs of
/// at most `MAX_RUN` pixels that satisfy `spec`.
fn thin_run_centres(v: &[u8], spec: RunSpec, runs: &mut Vec<(usize, usize, bool)>, out: &mut Vec<f32>) {
    runs.clear();
    let n = v.len();
    let mut i = 0;
    while i < n {
        if v[i] >= spec.weak {
            let s = i;
            let mut peak = 0;
            while i < n && v[i] >= spec.weak {
                peak = peak.max(v[i]);
                i += 1;
            }
            runs.push((s, i, peak >= spec.peak));
        } else {
            i += 1;
        }
    }
    for (k, &(s, e, strong)) in runs.iter().enumerate() {
        // Isolated from other real ink (faint specks nearby do not matter).
        if !strong
            || e - s > MAX_RUN
            || (k > 0 && runs[k - 1].2 && s < runs[k - 1].1 + RUN_GAP)
            || (k + 1 < runs.len() && runs[k + 1].2 && runs[k + 1].0 < e + RUN_GAP)
        {
            continue;
        }
        let mass: u32 = v[s..e].iter().map(|&x| x as u32).sum();
        if mass < spec.mass {
            continue;
        }
        let a = s.saturating_sub(1);
        let b = (e + 1).min(n);
        let (mut sw, mut swx) = (0.0f32, 0.0f32);
        for (j, &val) in v[a..b].iter().enumerate() {
            let wv = val as f32;
            sw += wv;
            swx += wv * (a + j) as f32;
        }
        out.push(swx / sw);
    }
}

/// Runs along each row (centres are x positions) → vertical lines.
fn row_points(ink: &[u8], w: usize, spec: RunSpec) -> ScanPoints {
    let lists: Vec<Vec<f32>> = ink
        .par_chunks(w)
        .map_init(Vec::new, |runs, row| {
            let mut out = Vec::new();
            thin_run_centres(row, spec, runs, &mut out);
            out
        })
        .collect();
    ScanPoints::from_lists(lists)
}

/// Runs down each column (centres are y positions) → horizontal lines.
/// Columns are processed in blocks transposed into a local buffer.
fn column_points(ink: &[u8], w: usize, h: usize, spec: RunSpec) -> ScanPoints {
    const B: usize = 32;
    let blocks: Vec<Vec<Vec<f32>>> = (0..w.div_ceil(B))
        .into_par_iter()
        .map(|bi| {
            let x0 = bi * B;
            let x1 = (x0 + B).min(w);
            let bw = x1 - x0;
            let mut col = vec![0u8; bw * h];
            for y in 0..h {
                for (j, &v) in ink[y * w + x0..y * w + x1].iter().enumerate() {
                    col[j * h + y] = v;
                }
            }
            let mut runs = Vec::new();
            (0..bw)
                .map(|j| {
                    let mut out = Vec::new();
                    thin_run_centres(&col[j * h..(j + 1) * h], spec, &mut runs, &mut out);
                    out
                })
                .collect()
        })
        .collect();
    ScanPoints::from_lists(blocks.into_iter().flatten().collect())
}

fn window_sum(acc: &[u32], r: usize) -> Vec<u32> {
    let n = acc.len();
    let mut pre = Vec::with_capacity(n + 1);
    pre.push(0u32);
    let mut s = 0u32;
    for &v in acc {
        s += v;
        pre.push(s);
    }
    (0..n).map(|i| pre[(i + r + 1).min(n)] - pre[i.saturating_sub(r)]).collect()
}

/// Long straight lines of one family. Returns `(p0, slope, coverage)` with
/// `p = p0 + (s − mid)·slope`, `mid = (n_scans − 1)/2`.
fn hough_lines(pts: &ScanPoints, extent: usize, min_cov: f64) -> Vec<(f64, f64, f64)> {
    let n = pts.n_scans();
    if n < 16 || extent < 16 {
        return Vec::new();
    }
    let mid = (n as f64 - 1.0) / 2.0;
    let nk = (MAX_ANGLE_DEG / COARSE_STEP_DEG).round() as i64;
    let margin = (mid * MAX_ANGLE_DEG.to_radians().tan()).ceil() as i64 + 2 * MERGE_PX as i64;
    let nb = extent + 2 * margin as usize;
    let profiles: Vec<Vec<u32>> = (-nk..=nk)
        .into_par_iter()
        .map(|k| {
            let t = (k as f64 * COARSE_STEP_DEG).to_radians().tan();
            let mut acc = vec![0u32; nb];
            for s in 0..n {
                let off = (s as f64 - mid) * t - margin as f64;
                for &c in pts.scan(s) {
                    let b = (c as f64 - off).round();
                    if b >= 0.0 && b < nb as f64 {
                        acc[b as usize] += 1;
                    }
                }
            }
            window_sum(&acc, COARSE_WIN)
        })
        .collect();
    let mut best = vec![0u32; nb];
    let mut best_k = vec![0i64; nb];
    for (ki, p) in profiles.iter().enumerate() {
        for (b, &v) in p.iter().enumerate() {
            if v > best[b] {
                best[b] = v;
                best_k[b] = ki as i64 - nk;
            }
        }
    }
    let thr = (0.7 * min_cov * n as f64) as u32;
    let r = MERGE_PX as usize;
    let mut peaks = Vec::new();
    for (b, &v) in best.iter().enumerate() {
        if v < thr.max(1) {
            continue;
        }
        let lo = b.saturating_sub(r);
        let hi = (b + r + 1).min(nb);
        if (lo..hi).all(|j| best[j] < v || (best[j] == v && j >= b)) {
            let t = (best_k[b] as f64 * COARSE_STEP_DEG).to_radians().tan();
            peaks.push((b as f64 - margin as f64, t));
        }
    }
    let mut lines: Vec<(f64, f64, f64)> =
        peaks.par_iter().filter_map(|&(p0, t0)| refine_line(pts, mid, p0, t0)).filter(|l| l.2 >= min_cov).collect();
    lines.sort_by(|a, b| b.2.total_cmp(&a.2));
    let span = n as f64 - 1.0;
    let mut kept: Vec<(f64, f64, f64)> = Vec::new();
    for l in lines {
        let dup = kept.iter().any(|k| {
            let d0 = (l.0 - mid * l.1) - (k.0 - mid * k.1);
            let d1 = (l.0 + (span - mid) * l.1) - (k.0 + (span - mid) * k.1);
            d0.abs() < MERGE_PX && d1.abs() < MERGE_PX
        });
        if !dup {
            kept.push(l);
        }
    }
    kept
}

/// Fine angle search + least squares around a coarse line.
fn refine_line(pts: &ScanPoints, mid: f64, p0: f64, t0: f64) -> Option<(f64, f64, f64)> {
    let n = pts.n_scans();
    // (s − mid, residual to the coarse line)
    let mut band: Vec<(f32, f32)> = Vec::new();
    for s in 0..n {
        let ds = s as f64 - mid;
        let pred = p0 + ds * t0;
        let sc = pts.scan(s);
        let lo = sc.partition_point(|&c| (c as f64) < pred - BAND_PX);
        for &c in &sc[lo..] {
            let q = c as f64 - pred;
            if q > BAND_PX {
                break;
            }
            band.push((ds as f32, q as f32));
        }
    }
    if band.len() < 16 {
        return None;
    }
    // Fine search: 0.25 px bins, ±INLIER_PX window.
    const BINS_PER_PX: f64 = 4.0;
    let half = BAND_PX + 4.0;
    let nbins = (2.0 * half * BINS_PER_PX) as usize;
    let win = (INLIER_PX * BINS_PER_PX) as usize;
    let a0 = t0.atan();
    let nf = (FINE_RANGE_DEG / FINE_STEP_DEG).round() as i64;
    let mut best = (0u32, 0.0f64, 0.0f64);
    let mut hist = vec![0u32; nbins];
    for k in -nf..=nf {
        let dt = (a0 + (k as f64 * FINE_STEP_DEG).to_radians()).tan() - t0;
        hist.fill(0);
        for &(ds, q) in &band {
            let b = ((q as f64 - ds as f64 * dt + half) * BINS_PER_PX).floor();
            if b >= 0.0 && b < nbins as f64 {
                hist[b as usize] += 1;
            }
        }
        let ws = window_sum(&hist, win);
        if let Some((bi, &v)) = ws.iter().enumerate().max_by_key(|&(i, &v)| (v, std::cmp::Reverse(i))) {
            if v > best.0 {
                best = (v, (bi as f64 + 0.5) / BINS_PER_PX - half, dt);
            }
        }
    }
    let (mut a, mut b) = (best.1, best.2);
    for _ in 0..3 {
        let (mut sn, mut sx, mut sy, mut sxx, mut sxy) = (0.0f64, 0.0, 0.0, 0.0, 0.0);
        for &(ds, q) in &band {
            let (x, y) = (ds as f64, q as f64);
            if (y - (a + b * x)).abs() <= INLIER_PX {
                sn += 1.0;
                sx += x;
                sy += y;
                sxx += x * x;
                sxy += x * y;
            }
        }
        if sn < 16.0 {
            return None;
        }
        let den = sn * sxx - sx * sx;
        if den.abs() < 1e-9 {
            return None;
        }
        b = (sn * sxy - sx * sy) / den;
        a = (sy - b * sx) / sn;
    }
    // Coverage counts only solid stretches of line; isolated text strokes or
    // symbols that happen to touch the line do not, and neither do they
    // bridge a hole between unrelated boxes that line up.
    let min_seg = (n as f64 / 150.0).max(8.0) as usize;
    // (first scan, last scan, inlier scans); band is ordered by scan.
    let mut segs: Vec<(f32, f32, usize)> = Vec::new();
    for &(ds, q) in &band {
        if (q as f64 - (a + b * ds as f64)).abs() > INLIER_PX {
            continue;
        }
        match segs.last_mut() {
            Some(s) if ds == s.1 => {}
            Some(s) if ds - s.1 <= 3.0 => {
                s.1 = ds;
                s.2 += 1;
            }
            _ => segs.push((ds, ds, 1)),
        }
    }
    segs.retain(|s| s.2 >= min_seg);
    let covered: usize = segs.iter().map(|s| s.2).sum();
    let max_gap = segs.windows(2).map(|w| w[1].0 - w[0].1).fold(0.0f32, f32::max);
    let slope = t0 + b;
    if slope.atan().to_degrees().abs() > MAX_ANGLE_DEG + FINE_RANGE_DEG || max_gap as f64 > MAX_GAP * n as f64 {
        return None;
    }
    Some((p0 + a, slope, covered as f64 / n as f64))
}

fn detect_class(img: &Rgb8, class: InkClass, min_cov: f64) -> Vec<GridLine> {
    let ink = ink_plane(img, class);
    let spec = class.spec();
    let (hp, vp) = rayon::join(|| column_points(&ink, img.w, img.h, spec), || row_points(&ink, img.w, spec));
    drop(ink);
    let (hl, vl) = rayon::join(|| hough_lines(&hp, img.h, min_cov), || hough_lines(&vp, img.w, min_cov));
    let mid_x = (img.w as f64 - 1.0) / 2.0;
    let mid_y = (img.h as f64 - 1.0) / 2.0;
    hl.into_iter()
        .map(|(p, t, c)| GridLine::from_slope(LineAxis::Horizontal, mid_x, p, t, c))
        .chain(vl.into_iter().map(|(p, t, c)| GridLine::from_slope(LineAxis::Vertical, mid_y, p, t, c)))
        .collect()
}

fn along_extent(img_w: f64, img_h: f64, axis: LineAxis) -> f64 {
    match axis {
        LineAxis::Horizontal => img_w,
        LineAxis::Vertical => img_h,
    }
}

fn same_line(a: &GridLine, b: &GridLine, img_w: f64, img_h: f64) -> bool {
    if a.axis != b.axis {
        return false;
    }
    let e = along_extent(img_w, img_h, a.axis);
    (a.pos_at(0.0) - b.pos_at(0.0)).abs() < MERGE_PX && (a.pos_at(e) - b.pos_at(e)).abs() < MERGE_PX
}

/// Detect long straight thin lines within ±5° of horizontal/vertical.
///
/// Dark neutral lines covering ≥ 35 % of the image span, plus strongly
/// coloured lines (blue UTM grids) covering ≥ 50 %. Horizontal lines come
/// first, each family sorted by position.
pub fn detect_lines(img: &Rgb8) -> Vec<GridLine> {
    if img.w < 32 || img.h < 32 || img.data.len() != img.w * img.h * 3 {
        return Vec::new();
    }
    let (mut lines, colour) = rayon::join(
        || detect_class(img, InkClass::Dark, MIN_COVERAGE),
        || detect_class(img, InkClass::Colour, COLOUR_MIN_COVERAGE),
    );
    let (w, h) = (img.w as f64, img.h as f64);
    for l in colour {
        if !lines.iter().any(|m| same_line(m, &l, w, h)) {
            lines.push(l);
        }
    }
    let key = |l: &GridLine| match l.axis {
        LineAxis::Horizontal => (0, l.pos_at(w / 2.0)),
        LineAxis::Vertical => (1, l.pos_at(h / 2.0)),
    };
    lines.sort_by(|a, b| {
        let (ka, kb) = (key(a), key(b));
        ka.0.cmp(&kb.0).then(ka.1.total_cmp(&kb.1))
    });
    lines
}

// ---------------------------------------------------------------------------
// Georeferencing
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Gcp {
    pub x: f64,
    pub y: f64,
    pub lat: f64,
    pub lon: f64,
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GridMethod {
    /// Labels matched to detected grid lines (accurate).
    Lines,
    /// Labels only (no lines found): coarse, should be refined.
    LabelsOnly,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GridResult {
    pub transform: Homography,
    pub method: GridMethod,
    /// RMS residual of the GCP fit in ground metres.
    pub rms_m: f64,
    pub n_lat: usize,
    pub n_lon: usize,
    pub gcps: Vec<Gcp>,
    /// OCR observations (indices) that were used as coordinate labels.
    pub used_labels: Vec<usize>,
}

const MIN_GROUND_M_PER_PX: f64 = 0.05;
const MAX_GROUND_M_PER_PX: f64 = 200.0;
/// Labels whose centre lies in this outer fraction of the image label the
/// lines of the nearer edge (left/right → latitude, top/bottom → longitude).
const EDGE_BAND: f64 = 0.12;
/// Full grid values must lie this close to the GPS position (metres).
const FULL_GRID_GATE_M: f64 = 150_000.0;
/// Abbreviated km labels resolve to the nearest value within this distance.
const ABBREV_GRID_GATE_M: f64 = 60_000.0;
/// Grid lines deviating more than this from their family's median tilt are ignored.
const MAX_FAMILY_TILT_DEG: f64 = 0.75;
/// Bounds the exhaustive RANSAC (hypotheses grow with the square of this).
const MAX_HYPOTHESIS_PAIRS: usize = 240;
/// Distinct fits kept per axis and slope sign for the joint choice.
const TOP_FITS: usize = 6;

/// Grid intervals for graticules, in degrees (DMS and decimal).
const GEO_UNITS: [f64; 27] = [
    10.0,
    5.0,
    2.0,
    1.0,
    0.5,
    1.0 / 3.0,
    0.25,
    0.2,
    1.0 / 6.0,
    0.1,
    1.0 / 12.0,
    0.05,
    1.0 / 30.0,
    0.025,
    0.02,
    1.0 / 60.0,
    0.01,
    1.0 / 120.0,
    0.005,
    1.0 / 180.0,
    1.0 / 240.0,
    1.0 / 360.0,
    0.002,
    1.0 / 720.0,
    0.001,
    1.0 / 1800.0,
    1.0 / 3600.0,
];
/// Grid intervals for projected grids, in metres.
const UTM_UNITS: [f64; 10] = [100_000.0, 50_000.0, 20_000.0, 10_000.0, 5_000.0, 2_000.0, 1_000.0, 500.0, 200.0, 100.0];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Sys {
    /// Latitude / longitude graticule.
    Geo,
    /// UTM eastings / northings in the GPS zone.
    Utm,
}

/// X: the value changes along image x and is carried by vertical lines
/// (longitude / easting). Y: horizontal lines (latitude / northing).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Ax {
    X,
    Y,
}

#[derive(Debug, Clone)]
struct Cand {
    obs: usize,
    ax: Ax,
    /// Degrees (Geo) or metres (Utm).
    value: f64,
    /// Value in the axis' linear frame (Mercator metres or UTM metres).
    lin: f64,
    cx: f64,
    cy: f64,
    bw: f64,
    bh: f64,
    /// Abbreviated grid label: only trusted when it sits on a grid line.
    abbrev: bool,
}

impl Cand {
    /// (along the axis' lines, across them, extent across).
    fn geom(&self) -> (f64, f64, f64) {
        match self.ax {
            Ax::Y => (self.cx, self.cy, self.bh),
            Ax::X => (self.cy, self.cx, self.bw),
        }
    }

    fn text_height(&self) -> f64 {
        self.bw.min(self.bh)
    }
}

struct Ctx {
    w: f64,
    h: f64,
    gps: Option<LatLon>,
    utm: Option<Utm>,
}

fn lin_of(sys: Sys, ax: Ax, v: f64) -> f64 {
    match (sys, ax) {
        (Sys::Geo, Ax::X) => EARTH_RADIUS * v.to_radians(),
        (Sys::Geo, Ax::Y) => latlon_to_merc(v, 0.0).1,
        (Sys::Utm, _) => v,
    }
}

fn value_of(sys: Sys, ax: Ax, lin: f64) -> f64 {
    match (sys, ax) {
        (Sys::Geo, Ax::X) => (lin / EARTH_RADIUS).to_degrees(),
        (Sys::Geo, Ax::Y) => merc_to_latlon(0.0, lin).0,
        (Sys::Utm, _) => lin,
    }
}

fn edge_axis(o: &OcrObservation, ctx: &Ctx) -> Option<Ax> {
    let (cx, cy) = o.center();
    let ex = (cx / ctx.w).min(1.0 - cx / ctx.w);
    let ey = (cy / ctx.h).min(1.0 - cy / ctx.h);
    if ex < EDGE_BAND && ey >= EDGE_BAND {
        Some(Ax::Y)
    } else if ey < EDGE_BAND && ex >= EDGE_BAND {
        Some(Ax::X)
    } else {
        None
    }
}

/// Readings of a degree value without hemisphere. The axis comes from the
/// label's position; an unsigned value may be either hemisphere (the line
/// fit keeps the sign that gives a non-mirrored map). Readings close to the
/// GPS fix win when there are any.
fn degree_readings(v: f64, o: &OcrObservation, ctx: &Ctx) -> Vec<(Ax, f64)> {
    let axes = match edge_axis(o, ctx) {
        Some(a) => vec![a],
        None => vec![Ax::X, Ax::Y],
    };
    let signs: Vec<f64> = if v > 0.0 { vec![v, -v] } else { vec![v] };
    let all: Vec<(Ax, f64)> = axes
        .iter()
        .flat_map(|&ax| signs.iter().map(move |&s| (ax, s)))
        .filter(|&(ax, s)| s.abs() <= if ax == Ax::Y { 90.0 } else { 180.0 })
        .collect();
    if let Some(g) = ctx.gps {
        let near: Vec<(Ax, f64)> = all
            .iter()
            .copied()
            .filter(|&(ax, s)| match ax {
                Ax::Y => (s - g.lat).abs() <= 3.0,
                Ax::X => (s - g.lon).abs() <= 3.0 / g.lat.to_radians().cos().max(0.1),
            })
            .collect();
        if !near.is_empty() {
            return near;
        }
    }
    all
}

/// Axis hint from a grid label's suffix: "…mE"/"…E" eastings, "…mN"/"…N" northings.
fn grid_axis_hint(text: &str) -> Option<Ax> {
    match text.trim().chars().last()?.to_ascii_uppercase() {
        'E' => Some(Ax::X),
        'N' => Some(Ax::Y),
        _ => None,
    }
}

/// Abbreviated grid label: 2–4 digits, optionally with "km", "mE", "mN", "E", "N".
fn parse_abbreviated(text: &str) -> Option<(f64, usize)> {
    let t: String = text.chars().filter(|c| !c.is_whitespace()).collect();
    let mut body = t.as_str();
    for suf in ["mE", "mN", "km", "E", "N"] {
        if let Some(b) = body.strip_suffix(suf) {
            body = b;
            break;
        }
    }
    if !(2..=4).contains(&body.len()) || !body.chars().all(|c| c.is_ascii_digit()) {
        return None;
    }
    Some((body.parse().ok()?, body.len()))
}

fn plausible_utm(ax: Ax, v: f64, u: &Utm, gate: f64) -> bool {
    match ax {
        Ax::X => (100_000.0..=900_000.0).contains(&v) && (v - u.easting).abs() <= gate,
        Ax::Y => (0.0..=10_000_000.0).contains(&v) && (v - u.northing).abs() <= gate,
    }
}

/// Coordinate label candidates for the graticule and UTM systems.
fn label_candidates(obs: &[OcrObservation], ctx: &Ctx) -> (Vec<Cand>, Vec<Cand>) {
    let mut geo = Vec::new();
    let mut utm = Vec::new();
    for (i, o) in obs.iter().enumerate() {
        if !o.bbox.iter().all(|v| v.is_finite()) {
            continue;
        }
        let (cx, cy) = o.center();
        let mk = |sys: Sys, ax: Ax, value: f64, abbrev: bool| Cand {
            obs: i,
            ax,
            value,
            lin: lin_of(sys, ax, value),
            cx,
            cy,
            bw: o.bbox[2].abs(),
            bh: o.bbox[3].abs(),
            abbrev,
        };
        let hint = grid_axis_hint(&o.text);
        let axes = hint.map_or(vec![Ax::X, Ax::Y], |a| vec![a]);
        match parse_coordinate(&o.text) {
            Some(CoordValue::Lat(v)) => geo.push(mk(Sys::Geo, Ax::Y, v, false)),
            Some(CoordValue::Lon(v)) => geo.push(mk(Sys::Geo, Ax::X, v, false)),
            Some(CoordValue::Degrees(v)) => {
                for (ax, v) in degree_readings(v, o, ctx) {
                    geo.push(mk(Sys::Geo, ax, v, false));
                }
            }
            Some(CoordValue::Grid(v)) => {
                if let Some(u) = &ctx.utm {
                    for &ax in &axes {
                        if plausible_utm(ax, v, u, FULL_GRID_GATE_M) {
                            utm.push(mk(Sys::Utm, ax, v, false));
                        }
                    }
                }
            }
            None => {
                if let (Some(u), Some((n, digits))) = (&ctx.utm, parse_abbreviated(&o.text)) {
                    let period = 10f64.powi(digits as i32 + 3);
                    let base = n * 1000.0;
                    for &ax in &axes {
                        let target = if ax == Ax::X { u.easting } else { u.northing };
                        let v = base + ((target - base) / period).round() * period;
                        if plausible_utm(ax, v, u, ABBREV_GRID_GATE_M) {
                            utm.push(mk(Sys::Utm, ax, v, true));
                        }
                    }
                }
            }
        }
    }
    (geo, utm)
}

/// One axis' fit `lin = a + b·t` (t = line position at the image centre, or
/// the label centre for label-only fits).
#[derive(Debug, Clone)]
struct AxisFit {
    a: f64,
    b: f64,
    from_lines: bool,
    /// (line index, grid value) for every line used.
    lines: Vec<(usize, f64)>,
    /// Inlier candidate indices.
    inliers: Vec<usize>,
    score: f64,
    cost: f64,
    /// RMS label residual (pixels) of label-only fits.
    rms_px: f64,
}

impl AxisFit {
    fn values(&self, cands: &[Cand]) -> Vec<f64> {
        let mut v: Vec<f64> = if self.from_lines {
            self.lines.iter().map(|l| l.1).collect()
        } else {
            self.inliers.iter().map(|&i| cands[i].value).collect()
        };
        sort_dedup(&mut v);
        v
    }

    fn better_than(&self, o: &AxisFit) -> bool {
        self.score > o.score || (self.score == o.score && self.cost < o.cost)
    }
}

/// The best few distinct fits of one axis, for slope b > 0 (`.0[0]`) and
/// b < 0 (`.0[1]`), best first. Several are kept because the best fit of one
/// axis alone can be a coincidence among many labels that the other axis
/// then contradicts (scale mismatch).
#[derive(Default)]
struct TopFits([Vec<AxisFit>; 2]);

impl TopFits {
    fn push(&mut self, f: AxisFit) {
        let list = &mut self.0[usize::from(f.b < 0.0)];
        match list.iter().position(|o| o.lines == f.lines && o.inliers == f.inliers) {
            Some(i) if f.better_than(&list[i]) => list[i] = f,
            Some(_) => return,
            None => list.push(f),
        }
        list.sort_by(|a, b| {
            if a.better_than(b) {
                std::cmp::Ordering::Less
            } else if b.better_than(a) {
                std::cmp::Ordering::Greater
            } else {
                std::cmp::Ordering::Equal
            }
        });
        list.truncate(TOP_FITS);
    }

    fn sign(&self, s: usize) -> &[AxisFit] {
        &self.0[s]
    }
}

fn sort_dedup(v: &mut Vec<f64>) {
    v.sort_by(|a, b| a.total_cmp(b));
    v.dedup_by(|a, b| (*a - *b).abs() <= 1e-9 * a.abs().max(1.0));
}

fn median(mut v: Vec<f64>) -> Option<f64> {
    v.retain(|x| x.is_finite());
    if v.is_empty() {
        return None;
    }
    v.sort_by(|a, b| a.total_cmp(b));
    Some(v[v.len() / 2])
}

/// Largest standard grid interval dividing every value and not exceeding the
/// spacing of adjacent lines.
fn grid_unit(values: &[f64], step: f64, sys: Sys) -> Option<f64> {
    let (units, tol): (&[f64], f64) = match sys {
        Sys::Geo => (&GEO_UNITS, 0.2 / 3600.0),
        Sys::Utm => (&UTM_UNITS, 0.5),
    };
    units
        .iter()
        .copied()
        .filter(|&u| u <= 1.1 * step && values.iter().all(|&v| ((v / u).round() * u - v).abs() <= tol))
        .max_by(|a, b| a.total_cmp(b))
}

fn plausible_scale(b: f64, ground_per_lin: f64) -> bool {
    let g = b.abs() * ground_per_lin;
    g.is_finite() && (MIN_GROUND_M_PER_PX..=MAX_GROUND_M_PER_PX).contains(&g)
}

fn line_fit(pts: &[(f64, f64)]) -> Option<(f64, f64)> {
    let n = pts.len() as f64;
    if pts.len() < 2 {
        return None;
    }
    let (mx, my) = pts.iter().fold((0.0, 0.0), |s, p| (s.0 + p.0 / n, s.1 + p.1 / n));
    let (mut sxx, mut sxy) = (0.0, 0.0);
    for p in pts {
        sxx += (p.0 - mx) * (p.0 - mx);
        sxy += (p.0 - mx) * (p.1 - my);
    }
    if sxx <= 1e-12 {
        return None;
    }
    let b = sxy / sxx;
    Some((my - b * mx, b))
}

struct AxisLines<'a> {
    sys: Sys,
    ax: Ax,
    cands: &'a [Cand],
    /// Line positions at the image centre.
    ts: Vec<f64>,
    /// Candidate lines of every label with a placement weight: 1 for a label
    /// centred on the line, falling off as it sits further away.
    near: Vec<Vec<(usize, f64)>>,
    spacing: f64,
    tol: f64,
}

impl AxisLines<'_> {
    fn evaluate(&self, a: f64, b: f64) -> Option<AxisFit> {
        let mut inl: Vec<(usize, usize)> = Vec::new();
        let (mut cost, mut weight) = (0.0, 0.0);
        for (ci, cl) in self.near.iter().enumerate() {
            let tp = (self.cands[ci].lin - a) / b;
            let best = cl.iter().map(|&(li, w)| ((self.ts[li] - tp).abs(), li, w)).min_by(|x, y| x.0.total_cmp(&y.0));
            if let Some((r, li, w)) = best {
                if r < self.tol {
                    inl.push((ci, li));
                    cost += r;
                    weight += w;
                }
            }
        }
        let mut vals: Vec<f64> = inl.iter().map(|&(ci, _)| self.cands[ci].value).collect();
        sort_dedup(&mut vals);
        if vals.len() < 2 {
            return None;
        }
        let mut line_val: Vec<Option<f64>> = vec![None; self.ts.len()];
        for &(ci, li) in &inl {
            line_val[li].get_or_insert(self.cands[ci].value);
        }
        // Unlabelled lines on round grid values.
        let tm = self.ts.iter().sum::<f64>() / self.ts.len() as f64;
        let step =
            (value_of(self.sys, self.ax, a + b * (tm + self.spacing)) - value_of(self.sys, self.ax, a + b * tm)).abs();
        if let Some(u) = grid_unit(&vals, step, self.sys) {
            let v0 = value_of(self.sys, self.ax, a + b * tm);
            let u_px = ((lin_of(self.sys, self.ax, v0 + u) - lin_of(self.sys, self.ax, v0)) / b).abs();
            if u_px >= 8.0 * self.tol {
                for (li, lv) in line_val.iter_mut().enumerate() {
                    if lv.is_some() {
                        continue;
                    }
                    let v = value_of(self.sys, self.ax, a + b * self.ts[li]);
                    let vs = (v / u).round() * u;
                    let r = ((lin_of(self.sys, self.ax, vs) - a) / b - self.ts[li]).abs();
                    if r < self.tol {
                        *lv = Some(vs);
                        cost += r;
                    }
                }
            }
        }
        let lines: Vec<(usize, f64)> = line_val.iter().enumerate().filter_map(|(li, v)| v.map(|v| (li, v))).collect();
        let mut inliers: Vec<usize> = inl.iter().map(|x| x.0).collect();
        inliers.dedup();
        Some(AxisFit { a, b, from_lines: true, score: weight + lines.len() as f64, lines, inliers, cost, rms_px: 0.0 })
    }

    fn refit(&self, f: &AxisFit) -> Option<AxisFit> {
        let pts: Vec<(f64, f64)> = f.lines.iter().map(|&(li, v)| (self.ts[li], lin_of(self.sys, self.ax, v))).collect();
        let (a, b) = line_fit(&pts)?;
        self.evaluate(a, b)
    }
}

/// Exhaustive RANSAC over (label, line) pairs. Returns the best fit for
/// b > 0 and for b < 0.
fn solve_axis_lines(cands: &[Cand], lines: &[GridLine], ax: Ax, sys: Sys, ctx: &Ctx, ground_per_lin: f64) -> TopFits {
    let mut out = TopFits::default();
    if lines.len() < 2 || cands.len() < 2 {
        return out;
    }
    let centre = match ax {
        Ax::Y => ctx.w / 2.0,
        Ax::X => ctx.h / 2.0,
    };
    let ts: Vec<f64> = lines.iter().map(|l| l.pos_at(centre)).collect();
    let mut sorted = ts.clone();
    sorted.sort_by(|a, b| a.total_cmp(b));
    let spacing = median(sorted.windows(2).map(|w| w[1] - w[0]).collect()).unwrap_or(0.0);
    if spacing <= 2.0 {
        return out;
    }
    // Up to three candidate lines per label, as (line, distance), nearest first.
    let near_d: Vec<Vec<(usize, f64)>> = cands
        .iter()
        .map(|c| {
            let (along, across, ext) = c.geom();
            // Labels sit on or right next to their line (margin labels are centred on
            // the line end; inside labels run along it), never half a spacing away.
            let gate = (0.5 * ext + 2.0 * c.text_height() + 4.0).min(0.5 * spacing);
            let mut d: Vec<(usize, f64)> = lines
                .iter()
                .enumerate()
                .map(|(li, l)| (li, (l.pos_at(along) - across).abs()))
                .filter(|x| x.1 <= gate)
                .collect();
            d.sort_by(|x, y| x.1.total_cmp(&y.1));
            d.truncate(3);
            d
        })
        .collect();
    let mut pairs: Vec<(usize, usize)> =
        near_d.iter().enumerate().flat_map(|(ci, ls)| ls.iter().map(move |&(li, _)| (ci, li))).collect();
    if pairs.len() > MAX_HYPOTHESIS_PAIRS {
        // Very many candidates (noisy OCR): hypotheses only from the labels
        // closest to a line, each with its nearest line.
        let mut firsts: Vec<(f64, usize, usize)> =
            near_d.iter().enumerate().filter_map(|(ci, ls)| ls.first().map(|&(li, d)| (d, ci, li))).collect();
        firsts.sort_by(|x, y| x.0.total_cmp(&y.0));
        pairs = firsts.into_iter().take(MAX_HYPOTHESIS_PAIRS).map(|(_, ci, li)| (ci, li)).collect();
    }
    let near = near_d
        .into_iter()
        .zip(cands)
        .map(|(ls, c)| {
            let s = 0.5 * c.text_height() + 4.0;
            ls.into_iter().map(|(li, d)| (li, 1.0 / (1.0 + (d / s).powi(2)))).collect()
        })
        .collect();
    let al = AxisLines { sys, ax, cands, ts, near, spacing, tol: (0.005 * spacing).max(3.0) };
    for (k, &(ci, li)) in pairs.iter().enumerate() {
        for &(cj, lj) in &pairs[k + 1..] {
            if ci == cj || li == lj {
                continue;
            }
            let dl = cands[ci].lin - cands[cj].lin;
            let dt = al.ts[li] - al.ts[lj];
            if dl.abs() < 1e-6 || dt.abs() < 1.0 {
                continue;
            }
            let b = dl / dt;
            if !plausible_scale(b, ground_per_lin) {
                continue;
            }
            let a = cands[ci].lin - b * al.ts[li];
            if let Some(f) = al.evaluate(a, b) {
                out.push(f);
            }
        }
    }
    let mut refined = TopFits::default();
    for f in out.0.into_iter().flatten() {
        let mut best = f;
        for _ in 0..2 {
            match al.refit(&best) {
                Some(r) if (r.b > 0.0) == (best.b > 0.0) && !best.better_than(&r) => best = r,
                _ => break,
            }
        }
        refined.push(best);
    }
    refined
}

/// Label-only fit on label centres (assumes an axis-aligned map).
fn solve_axis_labels(cands: &[Cand], ground_per_lin: f64) -> TopFits {
    let mut out = TopFits::default();
    let idx: Vec<usize> = (0..cands.len()).filter(|&i| !cands[i].abbrev).take(MAX_HYPOTHESIS_PAIRS).collect();
    if idx.len() < 2 {
        return out;
    }
    let pos = |c: &Cand| c.geom().1;
    let th = median(idx.iter().map(|&i| cands[i].text_height()).collect()).unwrap_or(20.0);
    let tol = (1.5 * th).max(8.0);
    let evaluate = |a: f64, b: f64| -> Option<AxisFit> {
        let mut inliers = Vec::new();
        let (mut cost, mut ss) = (0.0, 0.0);
        for &i in &idx {
            let r = (pos(&cands[i]) - (cands[i].lin - a) / b).abs();
            if r < tol {
                inliers.push(i);
                cost += r;
                ss += r * r;
            }
        }
        let mut vals: Vec<f64> = inliers.iter().map(|&i| cands[i].value).collect();
        sort_dedup(&mut vals);
        if vals.len() < 2 {
            return None;
        }
        let rms_px = (ss / inliers.len() as f64).sqrt();
        Some(AxisFit { a, b, from_lines: false, lines: Vec::new(), score: inliers.len() as f64, inliers, cost, rms_px })
    };
    for (k, &i) in idx.iter().enumerate() {
        for &j in &idx[k + 1..] {
            let dl = cands[i].lin - cands[j].lin;
            let dt = pos(&cands[i]) - pos(&cands[j]);
            if dl.abs() < 1e-6 || dt.abs() < tol {
                continue;
            }
            let b = dl / dt;
            if !plausible_scale(b, ground_per_lin) {
                continue;
            }
            if let Some(f) = evaluate(cands[i].lin - b * pos(&cands[i]), b) {
                out.push(f);
            }
        }
    }
    let mut refined = TopFits::default();
    for f in out.0.into_iter().flatten() {
        let pts: Vec<(f64, f64)> = f.inliers.iter().map(|&i| (pos(&cands[i]), cands[i].lin)).collect();
        let r = line_fit(&pts).and_then(|(a, b)| evaluate(a, b)).filter(|r| (r.b > 0.0) == (f.b > 0.0));
        refined.push(match r {
            Some(r) if r.score >= f.score => r,
            _ => f,
        });
    }
    refined
}

fn to_latlon(sys: Sys, vx: f64, vy: f64, ctx: &Ctx) -> Option<LatLon> {
    match sys {
        Sys::Geo => Some(LatLon::new(vy, vx)),
        Sys::Utm => {
            let u = ctx.utm?;
            Some(utm_to_latlon(&Utm { zone: u.zone, north: u.north, easting: vx, northing: vy }))
        }
    }
}

struct Solution {
    affine: Affine,
    method: GridMethod,
    rms_m: f64,
    n_lat: usize,
    n_lon: usize,
    gcps: Vec<Gcp>,
    used: Vec<usize>,
    score: f64,
}

impl Solution {
    fn better_than(&self, o: &Solution) -> bool {
        let rank = |s: &Solution| u8::from(s.method == GridMethod::Lines);
        // North-up wins ties: unsigned degree labels also fit a map turned 180°.
        let up = |s: &Solution| u8::from(s.affine.e < 0.0);
        (rank(self), self.score, up(self), -self.rms_m) > (rank(o), o.score, up(o), -o.rms_m)
    }
}

#[allow(clippy::too_many_arguments)]
fn combine(
    sys: Sys,
    cx: &[Cand],
    cy: &[Cand],
    fx: &AxisFit,
    fy: &AxisFit,
    lines_x: &[GridLine],
    lines_y: &[GridLine],
    ctx: &Ctx,
) -> Option<Solution> {
    let from_lines = fx.from_lines && fy.from_lines;
    let mut src = Vec::new();
    let mut gcps = Vec::new();
    if from_lines {
        for &(lx, vx) in &fx.lines {
            for &(ly, vy) in &fy.lines {
                let Some(p) = lines_y[ly].intersect(&lines_x[lx]) else { continue };
                let ll = to_latlon(sys, vx, vy, ctx)?;
                src.push(p);
                gcps.push(Gcp { x: p.0, y: p.1, lat: ll.lat, lon: ll.lon });
            }
        }
    } else {
        for vx in fx.values(cx) {
            for vy in fy.values(cy) {
                let px = (lin_of(sys, Ax::X, vx) - fx.a) / fx.b;
                let py = (lin_of(sys, Ax::Y, vy) - fy.a) / fy.b;
                let ll = to_latlon(sys, vx, vy, ctx)?;
                src.push((px, py));
                gcps.push(Gcp { x: px, y: py, lat: ll.lat, lon: ll.lon });
            }
        }
    }
    if src.len() < 3 || !gcps.iter().all(|g| g.lat.abs() <= 85.0 && g.lon.abs() <= 180.0) {
        return None;
    }
    let dst: Vec<(f64, f64)> = gcps.iter().map(|g| latlon_to_merc(g.lat, g.lon)).collect();
    let affine = Affine::fit(&src, &dst)?;
    if affine.det() >= 0.0 {
        return None;
    }
    let lat_c = gcps.iter().map(|g| g.lat).sum::<f64>() / gcps.len() as f64;
    let g = lat_c.to_radians().cos();
    let scale = affine.mean_scale() * g;
    if !(MIN_GROUND_M_PER_PX..=MAX_GROUND_M_PER_PX).contains(&scale) {
        return None;
    }
    // Mercator metres per pixel along image y vs x: 1 for conformal maps
    // (Mercator, UTM/TM), 1/cos φ for plain lat/lon (equirectangular) maps.
    // Labels paired with the wrong lines break this.
    let ratio = affine.b.hypot(affine.e) / affine.a.hypot(affine.d);
    let tol = if from_lines { 0.06 } else { 0.25 };
    if !((ratio - 1.0).abs() <= tol || (ratio * g - 1.0).abs() <= tol) {
        return None;
    }
    let mut ss = 0.0;
    for ((p, d), gc) in src.iter().zip(&dst).zip(&gcps) {
        let q = affine.apply(p.0, p.1);
        let e = (q.0 - d.0).hypot(q.1 - d.1) * gc.lat.to_radians().cos();
        ss += e * e;
    }
    let mut rms_m = (ss / src.len() as f64).sqrt();
    let method = if from_lines { GridMethod::Lines } else { GridMethod::LabelsOnly };
    if !from_lines {
        // Label placement uncertainty: residual scatter plus half a text height.
        let label_fits: Vec<&AxisFit> = [fx, fy].into_iter().filter(|f| !f.from_lines).collect();
        let th = median(
            label_fits
                .iter()
                .flat_map(|f| {
                    let cs = if std::ptr::eq(*f, fx) { cx } else { cy };
                    f.inliers.iter().map(move |&i| cs[i].text_height())
                })
                .collect(),
        )
        .unwrap_or(20.0);
        let res_px = (label_fits.iter().map(|f| f.rms_px * f.rms_px).sum::<f64>() / label_fits.len() as f64).sqrt();
        rms_m = (rms_m * rms_m + (res_px * scale).powi(2) + (0.5 * th * scale).powi(2)).sqrt();
    }
    let obs_of = |cs: &[Cand], f: &AxisFit| {
        let mut v: Vec<usize> = f.inliers.iter().map(|&i| cs[i].obs).collect();
        v.sort_unstable();
        v.dedup();
        v
    };
    let lon_obs = obs_of(cx, fx);
    let lat_obs = obs_of(cy, fy);
    let mut used: Vec<usize> = lon_obs.iter().chain(&lat_obs).copied().collect();
    used.sort_unstable();
    used.dedup();
    Some(Solution {
        affine,
        method,
        rms_m,
        n_lat: lat_obs.len(),
        n_lon: lon_obs.len(),
        gcps,
        used,
        score: fx.score + fy.score,
    })
}

fn solve_system(sys: Sys, cands: &[Cand], lines_x: &[GridLine], lines_y: &[GridLine], ctx: &Ctx) -> Option<Solution> {
    let cx: Vec<Cand> = cands.iter().filter(|c| c.ax == Ax::X).cloned().collect();
    let cy: Vec<Cand> = cands.iter().filter(|c| c.ax == Ax::Y).cloned().collect();
    if cx.len() < 2 || cy.len() < 2 {
        return None;
    }
    let ref_lat = match sys {
        Sys::Geo => median(cy.iter().map(|c| c.value).collect()).unwrap_or(0.0),
        Sys::Utm => ctx.gps.map_or(0.0, |g| g.lat),
    };
    let gpl = match sys {
        Sys::Geo => ref_lat.to_radians().cos(),
        Sys::Utm => 1.0,
    };
    let lx = solve_axis_lines(&cx, lines_x, Ax::X, sys, ctx, gpl);
    let ly = solve_axis_lines(&cy, lines_y, Ax::Y, sys, ctx, gpl);
    let bx = solve_axis_labels(&cx, gpl);
    let by = solve_axis_labels(&cy, gpl);
    let mut best: Option<Solution> = None;
    // Not mirrored: x and y slopes of opposite signs (north-up or south-up).
    // Line fits rank first; label-only fits stand in when no combination of
    // line fits passes the checks in `combine`.
    for (sx, sy) in [(0usize, 1usize), (1, 0)] {
        for fx in lx.sign(sx).iter().chain(bx.sign(sx)) {
            for fy in ly.sign(sy).iter().chain(by.sign(sy)) {
                if let Some(s) = combine(sys, &cx, &cy, fx, fy, lines_x, lines_y, ctx) {
                    if best.as_ref().is_none_or(|b| s.better_than(b)) {
                        best = Some(s);
                    }
                }
            }
        }
    }
    best
}

/// Lines of one family that are parallel to the family's consensus: grid
/// lines are, stray straight features (a fence, a road) usually are not.
fn grid_family(lines: &[GridLine], axis: LineAxis) -> Vec<GridLine> {
    let fam: Vec<GridLine> =
        lines.iter().filter(|l| l.axis == axis && l.theta.is_finite() && l.rho.is_finite()).copied().collect();
    if fam.len() < 3 {
        return fam;
    }
    let Some(med) = median(fam.iter().map(GridLine::tilt_deg).collect()) else { return fam };
    fam.into_iter().filter(|l| (l.tilt_deg() - med).abs() <= MAX_FAMILY_TILT_DEG).collect()
}

/// Full grid pipeline. `gps` resolves UTM zone / abbreviated grid labels and
/// the hemisphere of degree labels without one.
pub fn georeference_from_grid(
    img: &Rgb8,
    lines: &[GridLine],
    obs: &[OcrObservation],
    gps: Option<LatLon>,
) -> Option<GridResult> {
    let gps = gps.filter(|g| g.lat.is_finite() && g.lon.is_finite() && g.lat.abs() <= 84.0);
    let ctx =
        Ctx { w: img.w.max(1) as f64, h: img.h.max(1) as f64, gps, utm: gps.map(|g| latlon_to_utm(g.lat, g.lon)) };
    let (geo, utm) = label_candidates(obs, &ctx);
    let lines_x = grid_family(lines, LineAxis::Vertical);
    let lines_y = grid_family(lines, LineAxis::Horizontal);
    let mut best: Option<Solution> = None;
    for (sys, cands) in [(Sys::Geo, &geo), (Sys::Utm, &utm)] {
        if let Some(s) = solve_system(sys, cands, &lines_x, &lines_y, &ctx) {
            if best.as_ref().is_none_or(|b| s.better_than(b)) {
                best = Some(s);
            }
        }
    }
    let s = best?;
    Some(GridResult {
        transform: s.affine.to_homography(),
        method: s.method,
        rms_m: s.rms_m,
        n_lat: s.n_lat,
        n_lon: s.n_lon,
        gcps: s.gcps,
        used_labels: s.used,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn blank(w: usize, h: usize) -> Rgb8 {
        Rgb8::from_rgb(w, h, vec![255u8; w * h * 3]).unwrap()
    }

    fn put(img: &mut Rgb8, x: usize, y: usize, v: [u8; 3]) {
        let i = (y * img.w + x) * 3;
        img.data[i..i + 3].copy_from_slice(&v);
    }

    #[test]
    fn line_geometry() {
        let h = GridLine::from_slope(LineAxis::Horizontal, 500.0, 300.0, 0.01, 1.0);
        assert!((h.pos_at(500.0) - 300.0).abs() < 1e-9);
        assert!((h.pos_at(600.0) - 301.0).abs() < 1e-9);
        let v = GridLine::from_slope(LineAxis::Vertical, 400.0, 200.0, -0.02, 1.0);
        assert!((v.pos_at(400.0) - 200.0).abs() < 1e-9);
        assert!((v.pos_at(500.0) - 198.0).abs() < 1e-9);
        let (x, y) = h.intersect(&v).unwrap();
        assert!((h.pos_at(x) - y).abs() < 1e-9 && (v.pos_at(y) - x).abs() < 1e-9);
        assert!((h.tilt_deg() - 0.01f64.atan().to_degrees()).abs() < 1e-9);
    }

    #[test]
    fn detects_tilted_lines_and_ignores_thick_bars_and_diagonals() {
        let (w, h) = (1200, 900);
        let mut img = blank(w, h);
        let t = 1.5f64.to_radians().tan();
        for x in 0..w {
            // 2 px tilted horizontal line through (600, 300).
            let y = 300.0 + (x as f64 - 599.5) * t;
            for dy in 0..2 {
                put(&mut img, x, (y.floor() as usize) + dy, [20, 20, 20]);
            }
            // Thick 12 px bar (not a thin line).
            for yy in 600..612 {
                put(&mut img, x, yy, [0, 0, 0]);
            }
            // Diagonal at 30°.
            let yd = 50.0 + x as f64 * 0.577;
            if yd < h as f64 {
                put(&mut img, x, yd as usize, [0, 0, 0]);
            }
        }
        for y in 0..h {
            put(&mut img, 250, y, [10, 10, 10]);
            // Blue grid line.
            put(&mut img, 900, y, [20, 90, 230]);
            put(&mut img, 901, y, [20, 90, 230]);
        }
        let lines = detect_lines(&img);
        let hs: Vec<&GridLine> = lines.iter().filter(|l| l.axis == LineAxis::Horizontal).collect();
        let vs: Vec<&GridLine> = lines.iter().filter(|l| l.axis == LineAxis::Vertical).collect();
        assert_eq!(hs.len(), 1, "{lines:?}");
        assert_eq!(vs.len(), 2, "{lines:?}");
        assert!((hs[0].pos_at(599.5) - 300.0).abs() < 0.3, "{:?}", hs[0]);
        assert!((hs[0].tilt_deg() - 1.5).abs() < 0.05, "{}", hs[0].tilt_deg());
        assert!((vs[0].pos_at(450.0) - 250.0).abs() < 0.3);
        assert!((vs[1].pos_at(450.0) - 900.5).abs() < 0.3);
    }

    #[test]
    fn grid_units() {
        let m = 1.0 / 60.0;
        assert_eq!(grid_unit(&[25.0 + 49.0 * m, 25.0 + 51.0 * m], m * 1.0001, Sys::Geo), Some(GEO_UNITS[15]));
        assert_eq!(grid_unit(&[540_000.0, 542_000.0], 1000.0, Sys::Utm), Some(1000.0));
        // 25.8167° is 25°49'00.1": still a 30" multiple; 25.8123° is no round value.
        assert_eq!(grid_unit(&[25.8167], 0.01, Sys::Geo), Some(1.0 / 120.0));
        assert_eq!(grid_unit(&[25.8123], 0.01, Sys::Geo), None);
    }

    #[test]
    fn abbreviated_labels() {
        assert_eq!(parse_abbreviated("41"), Some((41.0, 2)));
        assert_eq!(parse_abbreviated("541 mE"), Some((541.0, 3)));
        assert_eq!(parse_abbreviated("7141N"), Some((7141.0, 4)));
        assert_eq!(parse_abbreviated("5"), None);
        assert_eq!(parse_abbreviated("KWAMI"), None);
    }
}
