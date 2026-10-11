//! Digital elevation model: Terrarium tiles, tile math, mosaics and sampling.
//!
//! Tiles are slippy-map XYZ in Web Mercator, 256×256 px, Terrarium-encoded
//! (`elev = R·256 + G + B/256 − 32768`). Mosaic pixel (col,row) has its centre
//! at Mercator `(ox + (col+0.5)·res, oy − (row+0.5)·res)`.

use crate::geo::EARTH_RADIUS;
use crate::image::GrayF32;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::f64::consts::PI;
use std::sync::Arc;

pub const TILE_SIZE: usize = 256;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, PartialOrd, Ord)]
pub struct TileId {
    pub z: u8,
    pub x: u32,
    pub y: u32,
}

/// Axis-aligned box in Mercator metres.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct MercBox {
    pub min_x: f64,
    pub min_y: f64,
    pub max_x: f64,
    pub max_y: f64,
}

impl MercBox {
    pub fn around(cx: f64, cy: f64, half: f64) -> MercBox {
        MercBox { min_x: cx - half, min_y: cy - half, max_x: cx + half, max_y: cy + half }
    }
    pub fn from_points(pts: &[(f64, f64)]) -> MercBox {
        let mut b =
            MercBox { min_x: f64::INFINITY, min_y: f64::INFINITY, max_x: f64::NEG_INFINITY, max_y: f64::NEG_INFINITY };
        for &(x, y) in pts {
            b.min_x = b.min_x.min(x);
            b.min_y = b.min_y.min(y);
            b.max_x = b.max_x.max(x);
            b.max_y = b.max_y.max(y);
        }
        b
    }
    pub fn expand(&self, m: f64) -> MercBox {
        MercBox { min_x: self.min_x - m, min_y: self.min_y - m, max_x: self.max_x + m, max_y: self.max_y + m }
    }
    pub fn width(&self) -> f64 {
        self.max_x - self.min_x
    }
    pub fn height(&self) -> f64 {
        self.max_y - self.min_y
    }
}

/// Mercator metres per pixel of a zoom level (tile pixels).
pub fn resolution(z: u8) -> f64 {
    2.0 * PI * EARTH_RADIUS / (TILE_SIZE as f64 * (1u64 << z) as f64)
}

/// Mercator → fractional tile coordinates at zoom z.
pub fn merc_to_tile(z: u8, mx: f64, my: f64) -> (f64, f64) {
    let n = (1u64 << z) as f64;
    let half = PI * EARTH_RADIUS;
    ((mx + half) / (2.0 * half) * n, (half - my) / (2.0 * half) * n)
}

/// Mercator bounds of a tile.
pub fn tile_bounds(t: TileId) -> MercBox {
    let size = 2.0 * PI * EARTH_RADIUS / (1u64 << t.z) as f64;
    let half = PI * EARTH_RADIUS;
    let min_x = -half + t.x as f64 * size;
    let max_y = half - t.y as f64 * size;
    MercBox { min_x, min_y: max_y - size, max_x: min_x + size, max_y }
}

/// All tiles at zoom z intersecting the box (clamped to the world).
pub fn tiles_covering(z: u8, b: &MercBox) -> Vec<TileId> {
    let n = 1u64 << z;
    let (fx0, fy0) = merc_to_tile(z, b.min_x, b.max_y);
    let (fx1, fy1) = merc_to_tile(z, b.max_x, b.min_y);
    let clamp = |v: f64| -> u32 { (v.floor().max(0.0) as u64).min(n - 1) as u32 };
    let (x0, x1, y0, y1) = (clamp(fx0), clamp(fx1), clamp(fy0), clamp(fy1));
    let mut out = Vec::new();
    for y in y0..=y1 {
        for x in x0..=x1 {
            out.push(TileId { z, x, y });
        }
    }
    out
}

/// Decode a Terrarium PNG into 256×256 elevations (metres, row-major).
pub fn terrarium_decode_png(bytes: &[u8]) -> Result<Vec<f32>, String> {
    let mut dec = png::Decoder::new(bytes);
    dec.set_transformations(png::Transformations::EXPAND | png::Transformations::STRIP_16);
    let mut reader = dec.read_info().map_err(|e| format!("png header: {e}"))?;
    let mut buf = vec![0u8; reader.output_buffer_size()];
    let info = reader.next_frame(&mut buf).map_err(|e| format!("png data: {e}"))?;
    let (w, h) = (info.width as usize, info.height as usize);
    if w != TILE_SIZE || h != TILE_SIZE {
        return Err(format!("unexpected tile size {w}x{h}"));
    }
    let ch = match info.color_type {
        png::ColorType::Rgb => 3,
        png::ColorType::Rgba => 4,
        other => return Err(format!("unsupported colour type {other:?}")),
    };
    let mut out = vec![0.0f32; w * h];
    for (i, o) in out.iter_mut().enumerate() {
        let p = &buf[i * ch..i * ch + 3];
        *o = terrarium_value(p[0], p[1], p[2]);
    }
    Ok(out)
}

#[inline]
pub fn terrarium_value(r: u8, g: u8, b: u8) -> f32 {
    (r as f32 * 256.0 + g as f32 + b as f32 / 256.0) - 32768.0
}

#[derive(Clone)]
enum TileState {
    Present(Arc<Vec<f32>>),
    Missing,
}

/// Tile cache owned by a georeferencing session.
#[derive(Default, Clone)]
pub struct DemStore {
    tiles: HashMap<TileId, TileState>,
}

impl DemStore {
    pub fn new() -> DemStore {
        DemStore::default()
    }

    pub fn add_png(&mut self, t: TileId, png: &[u8]) -> Result<(), String> {
        let v = terrarium_decode_png(png)?;
        self.tiles.insert(t, TileState::Present(Arc::new(v)));
        Ok(())
    }

    pub fn add_elevations(&mut self, t: TileId, elev: Vec<f32>) {
        assert_eq!(elev.len(), TILE_SIZE * TILE_SIZE);
        self.tiles.insert(t, TileState::Present(Arc::new(elev)));
    }

    pub fn mark_missing(&mut self, t: TileId) {
        self.tiles.entry(t).or_insert(TileState::Missing);
    }

    /// True if the tile was added or marked missing.
    pub fn known(&self, t: &TileId) -> bool {
        self.tiles.contains_key(t)
    }

    pub fn present(&self, t: &TileId) -> bool {
        matches!(self.tiles.get(t), Some(TileState::Present(_)))
    }

    /// Tiles from `wanted` that are neither present nor marked missing.
    pub fn unknown(&self, wanted: &[TileId]) -> Vec<TileId> {
        wanted.iter().copied().filter(|t| !self.known(t)).collect()
    }

    /// Fraction of `wanted` tiles that are present.
    pub fn coverage(&self, wanted: &[TileId]) -> f64 {
        if wanted.is_empty() {
            return 0.0;
        }
        wanted.iter().filter(|t| self.present(t)).count() as f64 / wanted.len() as f64
    }

    pub fn len(&self) -> usize {
        self.tiles.len()
    }

    pub fn is_empty(&self) -> bool {
        self.tiles.is_empty()
    }

    /// Assemble a mosaic at zoom z covering `b`; missing tiles are NaN.
    pub fn mosaic(&self, z: u8, b: &MercBox) -> DemMosaic {
        let tiles = tiles_covering(z, b);
        let x0 = tiles.iter().map(|t| t.x).min().unwrap_or(0);
        let x1 = tiles.iter().map(|t| t.x).max().unwrap_or(0);
        let y0 = tiles.iter().map(|t| t.y).min().unwrap_or(0);
        let y1 = tiles.iter().map(|t| t.y).max().unwrap_or(0);
        let cols = (x1 - x0 + 1) as usize;
        let rows = (y1 - y0 + 1) as usize;
        let w = cols * TILE_SIZE;
        let h = rows * TILE_SIZE;
        let mut data = vec![f32::NAN; w * h];
        for t in &tiles {
            if let Some(TileState::Present(v)) = self.tiles.get(t) {
                let ox = (t.x - x0) as usize * TILE_SIZE;
                let oy = (t.y - y0) as usize * TILE_SIZE;
                for r in 0..TILE_SIZE {
                    data[(oy + r) * w + ox..(oy + r) * w + ox + TILE_SIZE]
                        .copy_from_slice(&v[r * TILE_SIZE..(r + 1) * TILE_SIZE]);
                }
            }
        }
        let tb = tile_bounds(TileId { z, x: x0, y: y0 });
        DemMosaic { res: resolution(z), ox: tb.min_x, oy: tb.max_y, grid: GrayF32 { w, h, data } }
    }
}

/// Elevation raster aligned to Web Mercator.
#[derive(Clone)]
pub struct DemMosaic {
    /// Mercator metres per pixel.
    pub res: f64,
    /// Mercator x of the left edge of column 0.
    pub ox: f64,
    /// Mercator y of the top edge of row 0.
    pub oy: f64,
    /// Elevations in metres; NaN where unknown.
    pub grid: GrayF32,
}

impl DemMosaic {
    pub fn w(&self) -> usize {
        self.grid.w
    }
    pub fn h(&self) -> usize {
        self.grid.h
    }

    /// Mercator → continuous pixel coordinates (integer = pixel centre).
    #[inline]
    pub fn merc_to_px(&self, mx: f64, my: f64) -> (f64, f64) {
        ((mx - self.ox) / self.res - 0.5, (self.oy - my) / self.res - 0.5)
    }

    #[inline]
    pub fn px_to_merc(&self, c: f64, r: f64) -> (f64, f64) {
        (self.ox + (c + 0.5) * self.res, self.oy - (r + 0.5) * self.res)
    }

    pub fn bounds(&self) -> MercBox {
        MercBox {
            min_x: self.ox,
            min_y: self.oy - self.h() as f64 * self.res,
            max_x: self.ox + self.w() as f64 * self.res,
            max_y: self.oy,
        }
    }

    /// Bilinear elevation at a Mercator point; NaN outside or near missing data.
    #[inline]
    pub fn sample(&self, mx: f64, my: f64) -> f32 {
        let (c, r) = self.merc_to_px(mx, my);
        self.grid.bilinear(c, r).unwrap_or(f32::NAN)
    }

    /// Elevation and its gradient (metres per Mercator metre) at a Mercator point.
    /// Returns None outside the grid or where data is missing.
    #[inline]
    pub fn sample_with_grad(&self, mx: f64, my: f64) -> Option<(f32, f32, f32)> {
        let (c, r) = self.merc_to_px(mx, my);
        let g = &self.grid;
        if !(c >= 0.0 && r >= 0.0 && c < (g.w - 1) as f64 && r < (g.h - 1) as f64) {
            return None;
        }
        let x0 = c.floor() as usize;
        let y0 = r.floor() as usize;
        let fx = (c - x0 as f64) as f32;
        let fy = (r - y0 as f64) as f32;
        let a = g.get(x0, y0);
        let b = g.get(x0 + 1, y0);
        let cc = g.get(x0, y0 + 1);
        let d = g.get(x0 + 1, y0 + 1);
        if !(a.is_finite() && b.is_finite() && cc.is_finite() && d.is_finite()) {
            return None;
        }
        let v = a * (1.0 - fx) * (1.0 - fy) + b * fx * (1.0 - fy) + cc * (1.0 - fx) * fy + d * fx * fy;
        let dcol = (b - a) * (1.0 - fy) + (d - cc) * fy;
        let drow = (cc - a) * (1.0 - fx) + (d - b) * fx;
        let res = self.res as f32;
        // row increases southwards (−y), so dh/dmy = −drow/res
        Some((v, dcol / res, -drow / res))
    }

    /// 2× downsample (NaN-aware mean).
    pub fn downsample2(&self) -> DemMosaic {
        let (w, h) = (self.w() / 2, self.h() / 2);
        let mut data = vec![f32::NAN; w * h];
        for y in 0..h {
            for x in 0..w {
                let mut s = 0.0;
                let mut n = 0;
                for (dx, dy) in [(0, 0), (1, 0), (0, 1), (1, 1)] {
                    let v = self.grid.get(2 * x + dx, 2 * y + dy);
                    if v.is_finite() {
                        s += v;
                        n += 1;
                    }
                }
                if n > 0 {
                    data[y * w + x] = s / n as f32;
                }
            }
        }
        DemMosaic { res: self.res * 2.0, ox: self.ox, oy: self.oy, grid: GrayF32 { w, h, data } }
    }

    /// Fraction of finite cells.
    pub fn valid_fraction(&self) -> f64 {
        self.grid.data.iter().filter(|v| v.is_finite()).count() as f64 / self.grid.data.len().max(1) as f64
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::geo::latlon_to_merc;

    #[test]
    fn tile_math() {
        let (mx, my) = latlon_to_merc(-25.84, 27.40);
        let (tx, ty) = merc_to_tile(12, mx, my);
        assert_eq!((tx.floor() as u32, ty.floor() as u32), (2359, 2352));
        let b = tile_bounds(TileId { z: 12, x: 2359, y: 2352 });
        assert!(mx >= b.min_x && mx <= b.max_x && my >= b.min_y && my <= b.max_y);
        let ts = tiles_covering(12, &MercBox::around(mx, my, 100.0));
        assert!(ts.contains(&TileId { z: 12, x: 2359, y: 2352 }));
        assert!((resolution(0) - 156_543.033_928).abs() < 1e-3);
    }

    #[test]
    fn mosaic_sampling_and_gradient() {
        let mut s = DemStore::new();
        let t = TileId { z: 10, x: 500, y: 600 };
        // A plane: elevation = 2*col + 3*row
        let mut e = vec![0.0f32; TILE_SIZE * TILE_SIZE];
        for r in 0..TILE_SIZE {
            for c in 0..TILE_SIZE {
                e[r * TILE_SIZE + c] = 2.0 * c as f32 + 3.0 * r as f32;
            }
        }
        s.add_elevations(t, e);
        let b = tile_bounds(t);
        let m = s.mosaic(10, &b.expand(-10.0));
        assert_eq!((m.w(), m.h()), (256, 256));
        let (mx, my) = m.px_to_merc(10.25, 20.5);
        let v = m.sample(mx, my);
        assert!((v - (2.0 * 10.25 + 3.0 * 20.5)).abs() < 1e-3, "{v}");
        let (h, gx, gy) = m.sample_with_grad(mx, my).unwrap();
        assert!((h - v).abs() < 1e-4);
        assert!((gx - 2.0 / m.res as f32).abs() < 1e-6);
        assert!((gy + 3.0 / m.res as f32).abs() < 1e-6);
        let d = m.downsample2();
        assert_eq!(d.w(), 128);
        assert!(s.unknown(&[t, TileId { z: 10, x: 501, y: 600 }]).len() == 1);
    }
}
