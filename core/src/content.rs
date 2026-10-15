//! The map's content area on its page.
//!
//! A map is not always the whole raster: a 1 118 × 791 px export placed
//! unscaled on an A4 page rendered at 300 dpi (2 480 × 3 508 px), a sheet
//! reduced to a third of the page, a printout with a wide margin. Everything
//! the contour path sizes by the image (the shortest contour segment, the
//! sampling step, the orientation field's cells, the coarse-search template
//! and the scale range it can reach, the DEM boxes) must be sized by the
//! map instead: a 1 118 px map judged as a 3 508 px page loses the segments
//! under 54 px it needs and is searched with templates three times too
//! coarse ("No contour lines found", "No match").
//!
//! `map_content` finds the map as the largest block of ink on the paper:
//! cells of ~1/160 of the long side that hold ink (pixels darker or more
//! colourful than the paper by `INK_LEVEL`), closed over gaps of a few
//! cells (sparse contours, a lake) and opened (a frame round the page, lines
//! of text and the scale bar are thinner than a map), the bounding box of
//! the largest 8-connected group and of those near its size, plus a cell. The contour path runs on that rectangle when it
//! covers at most `MAX_SHARE` of the page; a map that fills its raster (a
//! scan of a sheet with its legend, a screenshot) keeps the whole raster.

use crate::image::Rgb8;
use rayon::prelude::*;
use serde::Serialize;

/// The part of the raster that holds the map: x0..x1 × y0..y1, base pixels.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize)]
pub struct MapRect {
    pub x0: usize,
    pub y0: usize,
    pub x1: usize,
    pub y1: usize,
}

impl MapRect {
    /// The whole `w` × `h` raster.
    pub fn whole(w: usize, h: usize) -> MapRect {
        MapRect { x0: 0, y0: 0, x1: w, y1: h }
    }

    pub fn w(&self) -> usize {
        self.x1 - self.x0
    }

    pub fn h(&self) -> usize {
        self.y1 - self.y0
    }

    /// Is this the whole `w` × `h` raster?
    pub fn is_whole(&self, w: usize, h: usize) -> bool {
        *self == MapRect::whole(w, h)
    }

    /// The rectangle as [x0, y0, x1, y1] (f64).
    pub fn bounds(&self) -> [f64; 4] {
        [self.x0 as f64, self.y0 as f64, self.x1 as f64, self.y1 as f64]
    }

    /// The pixels of `img` inside the rectangle.
    pub fn crop(&self, img: &Rgb8) -> Rgb8 {
        let (w, h) = (self.w(), self.h());
        let mut data = vec![0u8; w * h * 3];
        data.par_chunks_mut(w * 3).enumerate().for_each(|(y, row)| {
            let s = ((self.y0 + y) * img.w + self.x0) * 3;
            row.copy_from_slice(&img.data[s..s + w * 3]);
        });
        Rgb8 { w, h, data }
    }
}

/// What `map_content` found.
#[derive(Debug, Clone, Serialize)]
pub struct MapContent {
    /// The rectangle the contour path uses (the whole raster unless the map
    /// covers at most `MAX_SHARE` of it).
    pub rect: MapRect,
    /// The largest block of ink, if any.
    pub found: Option<MapRect>,
    /// Its share of the raster.
    pub share: f64,
    /// The share of the cells outside it that hold ink.
    pub outside_ink: f64,
}

/// Cells along the long side of the raster.
const CELLS_LONG: usize = 160;
/// A pixel is ink when one of its channels is this much darker than the
/// paper's (lines, tints, hillshade, text; scan noise and JPEG ringing on
/// blank paper stay well under it).
const INK_LEVEL: i32 = 24;
/// A cell is busy when this share of its pixels is ink (a sheet's contours
/// on white paper: 2-15 %; a line of text: 10-25 %; blank paper: ~0).
const CELL_INK: f64 = 0.02;
/// Closing radius (cells): bridges the gaps of sparse contours and lakes.
const CLOSE_CELLS: usize = 3;
/// Opening radius (cells): what is narrower than 2r + 1 cells is no map (a
/// title, lines of text, a frame round the page and what closing bridged
/// between them; a map is at least `MIN_SIDE_PX`, 14 cells or more).
const OPEN_CELLS: usize = 4;
/// Groups of map cells this share of the largest one's or more are map too.
const MIN_PART: f64 = 0.2;
/// The rectangle is used when it covers at most this share of the raster…
const MAX_SHARE: f64 = 0.75;
/// …has at least this many base pixels along each side…
const MIN_SIDE_PX: usize = 300;
/// …and at most this share of the cells outside it hold ink.
const MAX_OUTSIDE_INK: f64 = 0.08;

/// The map's content area on `img` (see the module documentation).
pub fn map_content(img: &Rgb8) -> MapContent {
    let whole = MapRect::whole(img.w, img.h);
    let none = MapContent { rect: whole, found: None, share: 1.0, outside_ink: 0.0 };
    if img.w < 2 * MIN_SIDE_PX || img.h < 2 * MIN_SIDE_PX || img.data.len() != img.w * img.h * 3 {
        return none;
    }
    let cell = img.w.max(img.h).div_ceil(CELLS_LONG).max(1);
    let (cw, ch) = (img.w.div_ceil(cell), img.h.div_ceil(cell));
    let paper = crate::contour_lines::paper_colour(img).map(|v| v as i32);
    // Ink pixels per cell, a band of cell rows at a time.
    let counts: Vec<Vec<u32>> = (0..ch)
        .into_par_iter()
        .map(|cy| {
            let mut row = vec![0u32; cw];
            for y in cy * cell..((cy + 1) * cell).min(img.h) {
                let line = &img.data[y * img.w * 3..(y + 1) * img.w * 3];
                for (x, p) in line.chunks_exact(3).enumerate() {
                    let ink = (0..3).any(|c| paper[c] - p[c] as i32 >= INK_LEVEL);
                    row[x / cell] += ink as u32;
                }
            }
            row
        })
        .collect();
    let mut busy = vec![false; cw * ch];
    for cy in 0..ch {
        let hh = ((cy + 1) * cell).min(img.h) - cy * cell;
        for cx in 0..cw {
            let ww = ((cx + 1) * cell).min(img.w) - cx * cell;
            busy[cy * cw + cx] = counts[cy][cx] as f64 >= CELL_INK * (ww * hh) as f64;
        }
    }
    // Closing bridges the gaps of sparse contours; opening then drops what
    // is thinner than a few cells: a frame drawn round the page, a line of
    // text, a scale bar (closing joins them to the map when they run close by).
    let closed = erode(&dilate(&busy, cw, ch, CLOSE_CELLS), cw, ch, CLOSE_CELLS);
    let opened = dilate(&erode(&closed, cw, ch, OPEN_CELLS), cw, ch, OPEN_CELLS);
    let Some((x0, y0, x1, y1)) = map_cells(&opened, cw, ch) else { return none };
    // Cells to base pixels, one cell of margin.
    let found = MapRect {
        x0: (x0.saturating_sub(1) * cell).min(img.w),
        y0: (y0.saturating_sub(1) * cell).min(img.h),
        x1: ((x1 + 2) * cell).min(img.w),
        y1: ((y1 + 2) * cell).min(img.h),
    };
    let share = (found.w() * found.h()) as f64 / (img.w * img.h) as f64;
    // Ink left outside it: blank paper with a frame and some text, or the
    // sparse part of a map (flat ground, its few contours too far apart to
    // close).
    let (xa, ya, xb, yb) = (x0.saturating_sub(1), y0.saturating_sub(1), (x1 + 1).min(cw - 1), (y1 + 1).min(ch - 1));
    let (mut out, mut out_busy) = (0usize, 0usize);
    for cy in 0..ch {
        for cx in 0..cw {
            if !(xa..=xb).contains(&cx) || !(ya..=yb).contains(&cy) {
                out += 1;
                out_busy += busy[cy * cw + cx] as usize;
            }
        }
    }
    let outside_ink = if out > 0 { out_busy as f64 / out as f64 } else { 0.0 };
    let usable =
        share <= MAX_SHARE && outside_ink <= MAX_OUTSIDE_INK && found.w() >= MIN_SIDE_PX && found.h() >= MIN_SIDE_PX;
    MapContent { rect: if usable { found } else { whole }, found: Some(found), share, outside_ink }
}

/// Binary dilation by a (2r+1)² square (separable).
fn dilate(m: &[bool], w: usize, h: usize, r: usize) -> Vec<bool> {
    let mut a = vec![false; w * h];
    for y in 0..h {
        for x in 0..w {
            a[y * w + x] = (x.saturating_sub(r)..(x + r + 1).min(w)).any(|xx| m[y * w + xx]);
        }
    }
    let mut b = vec![false; w * h];
    for y in 0..h {
        for x in 0..w {
            b[y * w + x] = (y.saturating_sub(r)..(y + r + 1).min(h)).any(|yy| a[yy * w + x]);
        }
    }
    b
}

/// Binary erosion by a (2r+1)² square; outside the grid counts as set (a map
/// reaching the raster's edge keeps its edge cells).
fn erode(m: &[bool], w: usize, h: usize, r: usize) -> Vec<bool> {
    let inv: Vec<bool> = m.iter().map(|&v| !v).collect();
    dilate(&inv, w, h, r).into_iter().map(|v| !v).collect()
}

/// Bounding box (cells, inclusive) of the 8-connected groups of set cells
/// holding at least `MIN_PART` of the largest one's cells (a map cut in two
/// by a lake or the sea is one map; a block of legend text is not).
fn map_cells(m: &[bool], w: usize, h: usize) -> Option<(usize, usize, usize, usize)> {
    let mut seen = vec![false; w * h];
    let mut groups: Vec<(usize, (usize, usize, usize, usize))> = Vec::new();
    let mut stack = Vec::new();
    for start in 0..w * h {
        if !m[start] || seen[start] {
            continue;
        }
        seen[start] = true;
        stack.push(start);
        let (mut n, mut bb) = (0usize, (usize::MAX, usize::MAX, 0usize, 0usize));
        while let Some(i) = stack.pop() {
            let (x, y) = (i % w, i / w);
            n += 1;
            bb = (bb.0.min(x), bb.1.min(y), bb.2.max(x), bb.3.max(y));
            for yy in y.saturating_sub(1)..(y + 2).min(h) {
                for xx in x.saturating_sub(1)..(x + 2).min(w) {
                    let j = yy * w + xx;
                    if m[j] && !seen[j] {
                        seen[j] = true;
                        stack.push(j);
                    }
                }
            }
        }
        groups.push((n, bb));
    }
    let largest = groups.iter().map(|g| g.0).max()?;
    groups
        .iter()
        .filter(|g| g.0 as f64 >= MIN_PART * largest as f64)
        .map(|g| g.1)
        .reduce(|a, b| (a.0.min(b.0), a.1.min(b.1), a.2.max(b.2), a.3.max(b.3)))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A white page with a textured block (a "map") and a few lines of text.
    fn page(w: usize, h: usize, map: MapRect) -> Rgb8 {
        let mut data = vec![250u8; w * h * 3];
        for y in map.y0..map.y1 {
            for x in map.x0..map.x1 {
                let i = (y * w + x) * 3;
                // Tint with thin brown lines every 9 px.
                let line = (x + 2 * y) % 9 == 0;
                let px = if line { [150, 110, 70] } else { [225, 235, 215] };
                data[i..i + 3].copy_from_slice(&px);
            }
        }
        // A frame round the page, close to the map's top edge.
        for x in 20..w - 20 {
            for y in [20, 21, h - 21, h - 20] {
                data[(y * w + x) * 3..(y * w + x) * 3 + 3].copy_from_slice(&[20, 20, 20]);
            }
        }
        for y in 20..h - 20 {
            for x in [20, 21, w - 21, w - 20] {
                data[(y * w + x) * 3..(y * w + x) * 3 + 3].copy_from_slice(&[20, 20, 20]);
            }
        }
        // Text lines under the map.
        for k in 0..3 {
            let y0 = map.y1 + 40 + 30 * k;
            for y in y0..(y0 + 12).min(h) {
                for x in (map.x0..(map.x0 + 500).min(w)).filter(|x| x % 5 < 2) {
                    let i = (y * w + x) * 3;
                    data[i..i + 3].copy_from_slice(&[30, 30, 30]);
                }
            }
        }
        Rgb8 { w, h, data }
    }

    #[test]
    fn small_map_on_a_large_page() {
        let map = MapRect { x0: 400, y0: 50, x1: 1518, y1: 841 };
        let c = map_content(&page(2480, 3508, map));
        let r = c.rect;
        assert!(r.x0 <= map.x0 && r.y0 <= map.y0 && r.x1 >= map.x1 && r.y1 >= map.y1, "{c:?}");
        // The text lines under the map may join it; the blank page does not.
        assert!(r.x0 + 60 >= map.x0 && r.y0 + 60 >= map.y0 && r.x1 <= map.x1 + 60, "{c:?}");
        assert!(r.y1 <= map.y1 + 220 && c.share < 0.2, "{c:?}");
        // The crop holds the map's pixels.
        let img = page(2480, 3508, map);
        let crop = r.crop(&img);
        assert_eq!((crop.w, crop.h), (r.w(), r.h()));
        assert_eq!(crop.rgb(map.x0 - r.x0 + 3, map.y0 - r.y0 + 3), img.rgb(map.x0 + 3, map.y0 + 3));
    }

    #[test]
    fn full_bleed_maps_and_blank_pages_keep_the_raster() {
        let full = MapRect { x0: 0, y0: 0, x1: 1600, y1: 1200 };
        let c = map_content(&page(1600, 1200, full));
        assert!(c.rect.is_whole(1600, 1200), "{c:?}");
        // A sheet with a margin: most of the raster is map.
        let sheet = MapRect { x0: 120, y0: 150, x1: 2380, y1: 3000 };
        assert!(map_content(&page(2480, 3508, sheet)).rect.is_whole(2480, 3508));
        // Blank paper: nothing found, the whole raster.
        let blank = Rgb8 { w: 1000, h: 1400, data: vec![248u8; 1000 * 1400 * 3] };
        let c = map_content(&blank);
        assert!(c.rect.is_whole(1000, 1400) && c.found.is_none(), "{c:?}");
    }
}
