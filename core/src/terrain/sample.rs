//! Sampling the store's tiles: a query takes a snapshot of the tiles it may touch at each zoom of
//! its chain (finest first), under the store's lock once, then samples without the lock.
//!
//! Coordinates: a zoom's *global pixel* coordinates are `u = (mx + πR) / res(z)`,
//! `v = (πR − my) / res(z)` (tile pixels, edges at integers, pixel `k`'s centre at `k + ½`), so
//! the same point at zoom `z − k` is `(u, v) / 2^k`. Tile columns wrap around the antimeridian;
//! rows outside the world count as missing.
//!
//! Fallback: a sample walks the chain from the finest zoom. Held tiles answer; tiles marked missing
//! (or outside the world) send it to the next coarser zoom; tiles neither held nor marked are
//! *needed* (the query lists them in `need_tiles`) and the sample also goes on to coarser zooms
//! for a partial answer, without listing their unknown tiles: `need_tiles` holds the tiles of the
//! zoom the query would use next.

use super::store::{Snapshot, TileState};
use crate::dem::{resolution, TileId, TILE_SIZE};
use crate::geo::EARTH_RADIUS;
use std::collections::HashMap;
use std::f64::consts::PI;
use std::sync::Arc;

const T: i64 = TILE_SIZE as i64;
/// Half the Web Mercator world (metres).
pub const HALF_WORLD: f64 = PI * EARTH_RADIUS;

/// One tile of a level's snapshot.
enum Cell {
    Present(Arc<Vec<f32>>),
    /// Marked missing, or a row outside the world.
    Missing,
    /// Neither held nor marked: the query needs it.
    Unknown,
    /// Not looked up (the query said it would not touch it): treated as missing.
    NotWanted,
}

/// A zoom's tiles as one query sees them.
pub struct Level {
    pub z: u8,
    /// Tiles per side at this zoom.
    pub n: i64,
    /// Mercator metres per tile pixel.
    pub res: f64,
    /// `(tx mod n, ty)` → index into `cells`.
    index: HashMap<(i64, i64), usize>,
    keys: Vec<(i64, i64)>,
    cells: Vec<Cell>,
    needed: Vec<bool>,
    /// The last tile looked up (its unwrapped column and row, and its index): consecutive samples
    /// mostly share a tile.
    last: Option<((i64, i64), usize)>,
    /// Some sample took its value from this level.
    pub contributed: bool,
    /// The last B-spline cell evaluated (its first tap's global pixel) and its polynomial.
    cell: Option<(i64, i64, [[f64; 4]; 4])>,
    /// The tile range `want_px` noted last: points along a line mostly fall in the same tiles.
    last_want: Option<(i64, i64, i64, i64)>,
}

/// The 4 × 4 elevations around a B-spline cell, rows along v.
type Taps = [[f32; 4]; 4];

/// What a level answers for a sample.
pub enum Sample<V> {
    Value(V),
    /// A tile of the stencil is missing: try coarser.
    Missing,
    /// A tile of the stencil is unknown (now marked needed): try coarser for a partial answer.
    Unknown,
}

impl Level {
    pub fn new(z: u8) -> Level {
        Level {
            z,
            n: 1i64 << z,
            res: resolution(z),
            index: HashMap::new(),
            keys: Vec::new(),
            cells: Vec::new(),
            needed: Vec::new(),
            last: None,
            contributed: false,
            cell: None,
            last_want: None,
        }
    }

    /// Global pixel coordinates of a Mercator point.
    #[inline(always)]
    pub fn merc_to_px(&self, mx: f64, my: f64) -> (f64, f64) {
        ((mx + HALF_WORLD) / self.res, (HALF_WORLD - my) / self.res)
    }

    #[inline(always)]
    fn key(&self, tx: i64, ty: i64) -> (i64, i64) {
        (tx.rem_euclid(self.n), ty)
    }

    /// Note that the query may touch tile `(tx, ty)` (unwrapped column).
    pub fn want_tile(&mut self, tx: i64, ty: i64) {
        let k = self.key(tx, ty);
        if !self.index.contains_key(&k) {
            self.index.insert(k, self.cells.len());
            self.keys.push(k);
            self.cells.push(Cell::NotWanted);
            self.needed.push(false);
        }
    }

    /// Note the tiles a stencil of `radius` pixels around global pixel `(u, v)` touches.
    pub fn want_px(&mut self, u: f64, v: f64, radius: f64) {
        if !(u.is_finite() && v.is_finite()) {
            return;
        }
        let (c0, c1) = (floor_i64(u - radius).div_euclid(T), floor_i64(u + radius).div_euclid(T));
        let (r0, r1) = (floor_i64(v - radius).div_euclid(T), floor_i64(v + radius).div_euclid(T));
        if self.last_want == Some((c0, c1, r0, r1)) {
            return;
        }
        self.last_want = Some((c0, c1, r0, r1));
        for ty in r0..=r1 {
            for tx in c0..=c1 {
                self.want_tile(tx, ty);
            }
        }
    }

    /// Note every tile under a Mercator rectangle grown by `pad_px` tile pixels. False when that
    /// is more than `max_tiles` tiles (nothing is noted then).
    pub fn want_rect(&mut self, min_x: f64, min_y: f64, max_x: f64, max_y: f64, pad_px: f64, max_tiles: usize) -> bool {
        let (u0, v0) = self.merc_to_px(min_x, max_y);
        let (u1, v1) = self.merc_to_px(max_x, min_y);
        if !(u0.is_finite() && u1.is_finite() && v0.is_finite() && v1.is_finite()) {
            return false;
        }
        let (c0, c1) = (((u0 - pad_px).floor() as i64).div_euclid(T), ((u1 + pad_px).floor() as i64).div_euclid(T));
        let r0 = ((v0 - pad_px).floor() as i64).div_euclid(T).max(-1);
        let r1 = ((v1 + pad_px).floor() as i64).div_euclid(T).min(self.n);
        let count = (c1 - c0 + 1).max(0) as u128 * (r1 - r0 + 1).max(0) as u128;
        if count > max_tiles as u128 {
            return false;
        }
        for ty in r0..=r1 {
            for tx in c0..=c1 {
                self.want_tile(tx, ty);
            }
        }
        true
    }

    /// Look the noted tiles up in the store (under its lock).
    pub fn fill(&mut self, snap: &mut Snapshot<'_>) {
        for (i, &(tx, ty)) in self.keys.iter().enumerate() {
            self.cells[i] = if ty < 0 || ty >= self.n {
                Cell::Missing
            } else {
                match snap.lookup(TileId { z: self.z, x: tx as u32, y: ty as u32 }) {
                    TileState::Present(d) => Cell::Present(d),
                    TileState::Missing => Cell::Missing,
                    TileState::Unknown => Cell::Unknown,
                }
            };
        }
    }

    /// Index of a tile's cell (None: not noted).
    #[inline(always)]
    fn cell_index(&mut self, tx: i64, ty: i64) -> Option<usize> {
        if let Some((lk, li)) = self.last {
            if lk == (tx, ty) {
                return Some(li);
            }
        }
        let i = *self.index.get(&self.key(tx, ty))?;
        self.last = Some(((tx, ty), i));
        Some(i)
    }

    /// The elevations of a tile, or what keeps it from being used (and notes an unknown tile as
    /// needed when `record`).
    #[inline(always)]
    fn tile(&mut self, tx: i64, ty: i64, record: bool) -> Result<&[f32], Sample<()>> {
        let Some(i) = self.cell_index(tx, ty) else {
            return Err(Sample::Missing);
        };
        match &self.cells[i] {
            Cell::Present(d) => Ok(d.as_slice()),
            Cell::Unknown => {
                if record {
                    self.needed[i] = true;
                }
                Err(Sample::Unknown)
            }
            Cell::Missing | Cell::NotWanted => Err(Sample::Missing),
        }
    }

    /// One elevation by global integer pixel (slow path).
    #[inline]
    fn pixel(&mut self, gx: i64, gy: i64, record: bool, status: &mut Sample<()>) -> f64 {
        match self.tile(gx.div_euclid(T), gy.div_euclid(T), record) {
            Ok(d) => d[(gy.rem_euclid(T) * T + gx.rem_euclid(T)) as usize] as f64,
            Err(s) => {
                if !matches!(status, Sample::Unknown) {
                    *status = s;
                }
                f64::NAN
            }
        }
    }

    /// Bilinear elevation at global pixel coordinates (pixel centres at `k + ½`).
    pub fn bilinear(&mut self, u: f64, v: f64, record: bool) -> Sample<f64> {
        let (c, r) = (u - 0.5, v - 0.5);
        if !(c.is_finite() && r.is_finite()) {
            return Sample::Missing;
        }
        let (x0, y0) = (floor_i64(c), floor_i64(r));
        let (fx, fy) = (c - x0 as f64, r - y0 as f64);
        let (lx, ly) = (x0.rem_euclid(T), y0.rem_euclid(T));
        let (a, b, cc, d);
        if lx < T - 1 && ly < T - 1 {
            match self.tile(x0.div_euclid(T), y0.div_euclid(T), record) {
                Ok(t) => {
                    let i = (ly * T + lx) as usize;
                    (a, b, cc, d) =
                        (t[i] as f64, t[i + 1] as f64, t[i + T as usize] as f64, t[i + T as usize + 1] as f64);
                }
                Err(Sample::Unknown) => return Sample::Unknown,
                Err(_) => return Sample::Missing,
            }
        } else {
            let mut st = Sample::Value(());
            a = self.pixel(x0, y0, record, &mut st);
            b = self.pixel(x0 + 1, y0, record, &mut st);
            cc = self.pixel(x0, y0 + 1, record, &mut st);
            d = self.pixel(x0 + 1, y0 + 1, record, &mut st);
            match st {
                Sample::Value(()) => {}
                Sample::Unknown => return Sample::Unknown,
                Sample::Missing => return Sample::Missing,
            }
        }
        Sample::Value((a * (1.0 - fx) + b * fx) * (1.0 - fy) + (cc * (1.0 - fx) + d * fx) * fy)
    }

    /// Cubic B-spline elevation and its derivatives along u and v (metres per tile pixel) at
    /// global pixel coordinates: a C² surface through the DEM's cells (slightly smoothed: about a
    /// Gaussian of half a cell), so contour lines are smooth curves and their screen width even.
    ///
    /// `cached`: evaluate the cell's bicubic polynomial (`BSPLINE_BASIS` · taps · basisᵀ), kept for
    /// the last cell, which pays when many samples share a cell (a render zoomed in); otherwise
    /// the separable weights straight on the taps. The two round differently: a caller picks one
    /// per query from its scale, so the same position always gives the same bits.
    pub fn bspline(&mut self, u: f64, v: f64, record: bool, cached: bool) -> Sample<(f64, f64, f64)> {
        let (c, r) = (u - 0.5, v - 0.5);
        if !(c.is_finite() && r.is_finite()) {
            return Sample::Missing;
        }
        let (x1, y1) = (floor_i64(c), floor_i64(r));
        let (t, s) = (c - x1 as f64, r - y1 as f64);
        if cached {
            if let Some((cx, cy, ref k)) = self.cell {
                if cx == x1 && cy == y1 {
                    return Sample::Value(eval_polynomial(k, t, s));
                }
            }
        }
        let p = match self.taps(x1, y1, record) {
            Ok(p) => p,
            Err(e) => return e,
        };
        if cached {
            let k = bspline_coefficients(&p);
            let out = eval_polynomial(&k, t, s);
            self.cell = Some((x1, y1, k));
            Sample::Value(out)
        } else {
            Sample::Value(eval_weights(&p, t, s))
        }
    }

    /// The 4 × 4 taps of the B-spline cell whose second tap is global pixel `(x1, y1)`.
    #[inline(always)]
    fn taps(&mut self, x1: i64, y1: i64, record: bool) -> Result<Taps, Sample<(f64, f64, f64)>> {
        let mut p = [[0.0f32; 4]; 4];
        let (lx, ly) = (x1.rem_euclid(T), y1.rem_euclid(T));
        if (1..T - 2).contains(&lx) && (1..T - 2).contains(&ly) {
            let tile = match self.tile(x1.div_euclid(T), y1.div_euclid(T), record) {
                Ok(t) => t,
                Err(Sample::Unknown) => return Err(Sample::Unknown),
                Err(_) => return Err(Sample::Missing),
            };
            let base = ((ly - 1) * T + lx - 1) as usize;
            for (j, row) in p.iter_mut().enumerate() {
                *row = tile[base + j * T as usize..][..4].try_into().expect("4 taps");
            }
        } else {
            let mut st = Sample::Value(());
            for (j, row) in p.iter_mut().enumerate() {
                for (i, tap) in row.iter_mut().enumerate() {
                    *tap = self.pixel(x1 - 1 + i as i64, y1 - 1 + j as i64, record, &mut st) as f32;
                }
            }
            match st {
                Sample::Value(()) => {}
                Sample::Unknown => return Err(Sample::Unknown),
                Sample::Missing => return Err(Sample::Missing),
            }
        }
        Ok(p)
    }

    /// The vertical half of the B-spline for a whole row of samples sharing one `v` (a Mercator
    /// render's output row): `band.col[x]` = Σ_j wy[j] · elevation(x, y1 − 1 + j) and `band.dcol`
    /// with the derivative weights, for global columns `x0 ..= x1`, summed in the order
    /// `eval_weights` sums them, so `RowBand::eval` gives the same bits as `bspline` (uncached) at
    /// every position of the row. False when a tile under the band is not held: sample pixel by
    /// pixel then (which also notes what is needed).
    pub fn row_band(&mut self, v: f64, x0: i64, x1: i64, band: &mut RowBand) -> bool {
        let r = v - 0.5;
        if !r.is_finite() || x1 < x0 || x1 - x0 > 1 << 16 {
            return false;
        }
        let y1 = floor_i64(r);
        let (wy, dwy) = bspline_weights((r - y1 as f64) as f32);
        let n = (x1 - x0 + 1) as usize;
        band.col.clear();
        band.col.resize(n, 0.0);
        band.dcol.clear();
        band.dcol.resize(n, 0.0);
        band.x0 = x0;
        band.valid = false;
        for j in 0..4 {
            let gy = y1 - 1 + j as i64;
            let (ty, ly) = (gy.div_euclid(T), gy.rem_euclid(T));
            let mut x = x0;
            while x <= x1 {
                let (tx, lx) = (x.div_euclid(T), x.rem_euclid(T));
                let end = (x1 - x + lx).min(T - 1); // last column of this tile in the band
                let Ok(tile) = self.tile(tx, ty, false) else { return false };
                let row = &tile[(ly * T) as usize..((ly + 1) * T) as usize];
                let base = (x - x0) as usize;
                for (k, &p) in row[lx as usize..=end as usize].iter().enumerate() {
                    band.col[base + k] += wy[j] * p;
                    band.dcol[base + k] += dwy[j] * p;
                }
                x += end - lx + 1;
            }
        }
        band.valid = true;
        self.contributed = true;
        true
    }

    /// The tiles noted needed, wrapped.
    fn needed_tiles(&self) -> impl Iterator<Item = TileId> + '_ {
        self.keys.iter().zip(&self.needed).filter(|(_, &n)| n).map(|(&(tx, ty), _)| TileId {
            z: self.z,
            x: tx as u32,
            y: ty as u32,
        })
    }
}

/// The uniform cubic B-spline's basis: tap k's weight at fraction t is Σ_p BASIS[p][k] tᵖ.
const BSPLINE_BASIS: [[f64; 4]; 4] = [
    [1.0 / 6.0, 4.0 / 6.0, 1.0 / 6.0, 0.0],
    [-3.0 / 6.0, 0.0, 3.0 / 6.0, 0.0],
    [3.0 / 6.0, -6.0 / 6.0, 3.0 / 6.0, 0.0],
    [-1.0 / 6.0, 3.0 / 6.0, -3.0 / 6.0, 1.0 / 6.0],
];

/// A cell's bicubic polynomial from its 4 × 4 taps (rows along v): k[q][p] multiplies s^q t^p.
fn bspline_coefficients(p: &[[f32; 4]; 4]) -> [[f64; 4]; 4] {
    let b = &BSPLINE_BASIS;
    let p = p.map(|row| row.map(|v| v as f64));
    // m[j][pw] = Σ_i p[j][i] b[pw][i]: each row of taps as a polynomial in t.
    let mut m = [[0.0; 4]; 4];
    for j in 0..4 {
        for pw in 0..4 {
            m[j][pw] = p[j][0] * b[pw][0] + p[j][1] * b[pw][1] + p[j][2] * b[pw][2] + p[j][3] * b[pw][3];
        }
    }
    let mut k = [[0.0; 4]; 4];
    for q in 0..4 {
        for pw in 0..4 {
            k[q][pw] = b[q][0] * m[0][pw] + b[q][1] * m[1][pw] + b[q][2] * m[2][pw] + b[q][3] * m[3][pw];
        }
    }
    k
}

/// A cell's polynomial at fractions (t, s): the value and its derivatives along t and s.
#[inline(always)]
fn eval_polynomial(k: &[[f64; 4]; 4], t: f64, s: f64) -> (f64, f64, f64) {
    let mut rows = [0.0; 4];
    let mut drows = [0.0; 4];
    for q in 0..4 {
        let c = &k[q];
        rows[q] = c[0] + t * (c[1] + t * (c[2] + t * c[3]));
        drows[q] = c[1] + t * (2.0 * c[2] + t * 3.0 * c[3]);
    }
    let h = rows[0] + s * (rows[1] + s * (rows[2] + s * rows[3]));
    let hu = drows[0] + s * (drows[1] + s * (drows[2] + s * drows[3]));
    let hv = rows[1] + s * (2.0 * rows[2] + s * 3.0 * rows[3]);
    (h, hu, hv)
}

/// Cubic B-spline weights and their derivatives at a fraction `t` (taps at −1 … +2).
#[inline(always)]
fn bspline_weights(t: f32) -> ([f32; 4], [f32; 4]) {
    const SIXTH: f32 = 1.0 / 6.0;
    let (t2, t3) = (t * t, t * t * t);
    let s = 1.0 - t;
    (
        [
            s * s * s * SIXTH,
            (3.0 * t3 - 6.0 * t2 + 4.0) * SIXTH,
            (-3.0 * t3 + 3.0 * t2 + 3.0 * t + 1.0) * SIXTH,
            t3 * SIXTH,
        ],
        [-s * s * 0.5, (3.0 * t2 - 4.0 * t) * 0.5, (-3.0 * t2 + 2.0 * t + 1.0) * 0.5, t2 * 0.5],
    )
}

#[inline(always)]
fn dot4(a: &[f32; 4], b: &[f32; 4]) -> f32 {
    (a[0] * b[0] + a[1] * b[1]) + (a[2] * b[2] + a[3] * b[3])
}

/// The B-spline at fractions (t, s) straight from the taps, in single precision (0.1 mm at
/// 3 000 m): value and derivatives along t and s. Columns first, four lanes at a time, so the
/// compiler vectorises it.
#[inline(always)]
fn eval_weights(p: &[[f32; 4]; 4], t: f64, s: f64) -> (f64, f64, f64) {
    let (wx, dwx) = bspline_weights(t as f32);
    let (wy, dwy) = bspline_weights(s as f32);
    let mut col = [0.0f32; 4];
    let mut dcol = [0.0f32; 4];
    for j in 0..4 {
        for i in 0..4 {
            col[i] += wy[j] * p[j][i];
            dcol[i] += dwy[j] * p[j][i];
        }
    }
    (dot4(&wx, &col) as f64, dot4(&dwx, &col) as f64, dot4(&wx, &dcol) as f64)
}

/// floor() as an integer, without the library call x86-64 makes of it.
#[inline(always)]
pub fn floor_i64(x: f64) -> i64 {
    let i = x as i64;
    if (i as f64) > x {
        i - 1
    } else {
        i
    }
}

/// The nearest integer (halves up), likewise without a library call.
#[inline(always)]
pub fn round_f64(x: f64) -> f64 {
    floor_i64(x + 0.5) as f64
}

/// One row band of B-spline vertical sums (`Level::row_band`).
#[derive(Default)]
pub struct RowBand {
    x0: i64,
    col: Vec<f32>,
    dcol: Vec<f32>,
    valid: bool,
}

impl RowBand {
    /// The B-spline and its derivatives along u and v at global column coordinate `u` of the
    /// band's row; None outside the band.
    #[inline(always)]
    pub fn eval(&self, u: f64) -> Option<(f64, f64, f64)> {
        let c = u - 0.5;
        if !(self.valid && c.is_finite()) {
            return None;
        }
        let x1 = floor_i64(c);
        let i = x1 - 1 - self.x0;
        if i < 0 || i as usize + 4 > self.col.len() {
            return None;
        }
        let i = i as usize;
        let (wx, dwx) = bspline_weights((c - x1 as f64) as f32);
        let col: &[f32; 4] = self.col[i..i + 4].try_into().expect("4 columns");
        let dcol: &[f32; 4] = self.dcol[i..i + 4].try_into().expect("4 columns");
        Some((dot4(&wx, col) as f64, dot4(&dwx, col) as f64, dot4(&wx, dcol) as f64))
    }
}

/// The zooms of a query, finest first.
pub struct Chain {
    pub levels: Vec<Level>,
}

impl Chain {
    /// Zooms `z_want` down to `z_min` (both clamped so `z_min ≤ z_want`).
    pub fn new(z_want: u8, z_min: u8) -> Chain {
        let z_min = z_min.min(z_want);
        Chain { levels: (z_min..=z_want).rev().map(Level::new).collect() }
    }

    pub fn finest(&self) -> &Level {
        &self.levels[0]
    }

    /// Note the tiles a stencil of `radius` pixels at a Mercator point touches, at every zoom.
    pub fn want_merc(&mut self, mx: f64, my: f64, radius: f64) {
        for l in &mut self.levels {
            let (u, v) = l.merc_to_px(mx, my);
            l.want_px(u, v, radius);
        }
    }

    /// Look every noted tile up (one lock for all zooms).
    pub fn fill(&mut self, snap: &mut Snapshot<'_>) {
        for l in &mut self.levels {
            l.fill(snap);
        }
    }

    /// Bilinear elevation at a Mercator point through the chain, and the index of the level it
    /// came from.
    pub fn bilinear(&mut self, mx: f64, my: f64) -> Option<(f64, usize)> {
        let (u0, v0) = self.levels[0].merc_to_px(mx, my);
        self.bilinear_px(u0, v0)
    }

    /// As `bilinear`, from the finest level's global pixel coordinates.
    pub fn bilinear_px(&mut self, u0: f64, v0: f64) -> Option<(f64, usize)> {
        let mut record = true;
        let mut scale = 1.0;
        for (k, l) in self.levels.iter_mut().enumerate() {
            match l.bilinear(u0 * scale, v0 * scale, record) {
                Sample::Value(h) => {
                    l.contributed = true;
                    return Some((h, k));
                }
                Sample::Unknown => record = false,
                Sample::Missing => {}
            }
            scale *= 0.5;
        }
        None
    }

    /// B-spline elevation and derivatives (metres per finest-level pixel) from the finest level's
    /// global pixel coordinates.
    pub fn bspline_px(&mut self, u0: f64, v0: f64, cached: bool) -> Option<(f64, f64, f64)> {
        let mut record = true;
        let mut scale = 1.0;
        for l in self.levels.iter_mut() {
            match l.bspline(u0 * scale, v0 * scale, record, cached) {
                Sample::Value((h, hu, hv)) => {
                    l.contributed = true;
                    return Some((h, hu * scale, hv * scale));
                }
                Sample::Unknown => record = false,
                Sample::Missing => {}
            }
            scale *= 0.5;
        }
        None
    }

    /// The finest zoom any sample took its value from.
    pub fn zoom_used(&self) -> Option<u8> {
        self.levels.iter().find(|l| l.contributed).map(|l| l.z)
    }

    /// The needed tiles, nearest Mercator `(cx, cy)` first, at most `max`.
    pub fn need_tiles(&self, cx: f64, cy: f64, max: usize) -> Vec<TileId> {
        Chain::need_tiles_of(&[self], cx, cy, max)
    }

    /// The needed tiles of several chains (one query's), nearest `(cx, cy)` first, at most `max`.
    pub fn need_tiles_of(chains: &[&Chain], cx: f64, cy: f64, max: usize) -> Vec<TileId> {
        let mut v: Vec<(f64, TileId)> = Vec::new();
        for l in chains.iter().flat_map(|c| &c.levels) {
            let size = TILE_SIZE as f64 * l.res;
            let world = 2.0 * HALF_WORLD;
            for t in l.needed_tiles() {
                let tcx = -HALF_WORLD + (t.x as f64 + 0.5) * size;
                let tcy = HALF_WORLD - (t.y as f64 + 0.5) * size;
                let mut dx = (tcx - cx).abs() % world;
                dx = dx.min(world - dx);
                v.push((dx.hypot(tcy - cy), t));
            }
        }
        v.sort_by(|a, b| a.0.total_cmp(&b.0).then(a.1.cmp(&b.1)));
        v.dedup_by(|a, b| a.1 == b.1);
        v.into_iter().map(|(_, t)| t).take(max.max(1)).collect()
    }
}

/// The zoom bounds of a request: `max_zoom` (≤ `MAX_ZOOM`) and `min_zoom` (default max − 3).
pub fn zoom_bounds(max_zoom: u8, min_zoom: Option<u8>) -> Result<(u8, u8), String> {
    if max_zoom > super::MAX_ZOOM {
        return Err(format!("max_zoom {max_zoom} above {}", super::MAX_ZOOM));
    }
    let min = min_zoom.unwrap_or(max_zoom.saturating_sub(super::DEFAULT_ZOOM_FALLBACK));
    if min > max_zoom {
        return Err(format!("min_zoom {min} above max_zoom {max_zoom}"));
    }
    Ok((max_zoom, min))
}

/// The coarsest zoom whose cell (Mercator metres per tile pixel) is at most `cell_merc`, within
/// `[z_min, z_max]`.
pub fn coarsest_zoom_with_cell(cell_merc: f64, z_min: u8, z_max: u8) -> u8 {
    if !(cell_merc.is_finite() && cell_merc > 0.0) {
        return z_max;
    }
    (z_min..=z_max).find(|&z| resolution(z) <= cell_merc * (1.0 + 1e-9)).unwrap_or(z_max)
}

#[cfg(test)]
mod tests {
    use super::super::store::Tiles;
    use super::*;

    /// Two tiles side by side with rough terrain, and a chain over them.
    fn rough_chain() -> Chain {
        let tiles = Tiles::new(64 << 20);
        for (x, seed) in [(2358u32, 1u32), (2359, 7)] {
            let e: Vec<f32> = (0..TILE_SIZE * TILE_SIZE)
                .map(|i| {
                    let (c, r) = ((i % TILE_SIZE) as f32, (i / TILE_SIZE) as f32);
                    let noise = ((i as u32).wrapping_mul(2_654_435_761) >> 28) as f32;
                    1200.0 + 80.0 * (c * 0.071 + seed as f32).sin() * (r * 0.053).cos() + noise
                })
                .collect();
            tiles.insert(TileId { z: 12, x, y: 2352 }, e);
        }
        let mut chain = Chain::new(12, 12);
        for x in [2358, 2359] {
            chain.levels[0].want_tile(x, 2352);
        }
        chain.fill(&mut tiles.snapshot());
        chain
    }

    #[test]
    fn row_bands_give_the_same_bits_as_pixel_samples() {
        let mut chain = rough_chain();
        let (x_lo, y) = (2358.0 * 256.0, 2352.0 * 256.0);
        for row in 0..40 {
            let v = y + 3.0 + row as f64 * 6.173;
            let mut band = RowBand::default();
            // Across the two tiles' shared edge.
            let (u0, u1) = (x_lo + 2.5, x_lo + 509.5);
            assert!(chain.levels[0].row_band(v, floor_i64(u0 - 0.5) - 1, floor_i64(u1 - 0.5) + 2, &mut band));
            for k in 0..=1000 {
                let u = u0 + (u1 - u0) * k as f64 / 1000.0;
                let Sample::Value(direct) = chain.levels[0].bspline(u, v, true, false) else { panic!("no sample") };
                assert_eq!(band.eval(u), Some(direct), "u {u}, v {v}");
            }
        }
        // A band over a tile that is not held: refused.
        let mut band = RowBand::default();
        assert!(!chain.levels[0].row_band(y + 100.0, (x_lo as i64) - 300, x_lo as i64, &mut band));
        assert_eq!(band.eval(x_lo), None);
    }

    #[test]
    fn cached_polynomials_match_the_weights() {
        let mut chain = rough_chain();
        let (x, y) = (2358.0 * 256.0 + 37.0, 2352.0 * 256.0 + 91.0);
        for k in 0..400 {
            let (u, v) = (x + k as f64 * 0.0731, y + k as f64 * 0.0517);
            let Sample::Value(a) = chain.levels[0].bspline(u, v, true, true) else { panic!("no sample") };
            let Sample::Value(b) = chain.levels[0].bspline(u, v, true, false) else { panic!("no sample") };
            assert!((a.0 - b.0).abs() < 1e-3 && (a.1 - b.1).abs() < 1e-3 && (a.2 - b.2).abs() < 1e-3, "{a:?} {b:?}");
        }
    }

    #[test]
    fn rounding_helpers() {
        for x in [-2.5, -1.0, -0.2, 0.0, 0.7, 3.0, 1e12] {
            assert_eq!(floor_i64(x), x.floor() as i64);
        }
        assert_eq!(round_f64(2.5), 3.0);
        assert_eq!(round_f64(-0.4), 0.0);
        assert_eq!(coarsest_zoom_with_cell(40.0, 9, 13), 12);
        assert_eq!(coarsest_zoom_with_cell(1.0, 9, 13), 13);
        assert_eq!(coarsest_zoom_with_cell(1e6, 9, 13), 9);
    }
}
