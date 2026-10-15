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
//! colourful than the paper by `INK_LEVEL`) other than text (pieces of ink
//! the size of glyphs and words: a paragraph beside the map is a solid
//! block no opening removes), closed over gaps of a few cells (sparse
//! contours, a lake) and opened (a frame round the page, lines of text and
//! the scale bar are thinner than a map), the bounding box of the largest
//! 8-connected group and of those near its size, each cut along the blank
//! bands across it (two maps side by side: its parts), plus a cell. The
//! contour path runs on that rectangle when it covers at most `MAX_SHARE`
//! of the page; a map that fills its raster (a scan of a sheet with its
//! legend, a screenshot) keeps the whole raster. Where the map may lie, which
//! its fit must hold (`MapContent::extent`), is the rectangle, or the raster
//! when the map runs to its edges and is blank beside them.

use crate::contour_lines::RunComponents;
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
    /// Where the map may lie, which the fit must hold (`refine::Judge`): the
    /// rectangle, or the whole raster when the map runs to the raster's
    /// edges on three sides or more, so that what is blank beside it is the
    /// map's own blank part (sea, a glacier, the land beyond a park or a
    /// survey), not a page margin.
    pub extent: MapRect,
    /// The separate blocks of ink the rectangle bounds (two maps on one
    /// page, a map and a photo); empty when it is one block.
    pub parts: Vec<MapRect>,
    /// The largest block of ink, if any.
    pub found: Option<MapRect>,
    /// Its share of the raster.
    pub share: f64,
    /// The share of the cells outside it that hold ink (not text).
    pub outside_ink: f64,
    /// Cells left out as text (`TEXT_MAX_H_CELLS`).
    pub text_cells: usize,
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
/// A connected piece of ink at most this many cells tall and
/// `TEXT_MAX_W_CELLS` wide is a glyph, a word, a symbol or a dash: text on
/// the page (a route description, a trip report, a legend), not map. A
/// paragraph is a solid block of busy cells that no opening removes, and
/// would be taken into the map's rectangle 600-900 px away. Text is 1.2-2.5
/// cells tall at 300 dpi (10-12 pt); contour lines, tints and hillshade
/// connect over many cells, and the map's own labels lie among them.
const TEXT_MAX_H_CELLS: usize = 4;
const TEXT_MAX_W_CELLS: usize = 20;
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
/// The map's extent is the raster when its rectangle reaches this many of
/// the raster's edges (within the closing's reach: a frame or a margin of
/// a few cells).
const EDGES_FOR_RASTER: usize = 2;

/// The map's content area on `img` (see the module documentation).
pub fn map_content(img: &Rgb8) -> MapContent {
    let whole = MapRect::whole(img.w, img.h);
    let none = MapContent {
        rect: whole,
        extent: whole,
        parts: Vec::new(),
        found: None,
        share: 1.0,
        outside_ink: 0.0,
        text_cells: 0,
    };
    if img.w < 2 * MIN_SIDE_PX || img.h < 2 * MIN_SIDE_PX || img.data.len() != img.w * img.h * 3 {
        return none;
    }
    let cell = img.w.max(img.h).div_ceil(CELLS_LONG).max(1);
    let (cw, ch) = (img.w.div_ceil(cell), img.h.div_ceil(cell));
    let (map_ink, text_ink) = cell_ink(img, cell, cw, ch);
    let mut busy = vec![false; cw * ch];
    let mut text_cells = 0;
    for cy in 0..ch {
        let hh = ((cy + 1) * cell).min(img.h) - cy * cell;
        for cx in 0..cw {
            let ww = ((cx + 1) * cell).min(img.w) - cx * cell;
            let need = CELL_INK * (ww * hh) as f64;
            let i = cy * cw + cx;
            busy[i] = map_ink[i] as f64 >= need;
            text_cells += (!busy[i] && (map_ink[i] + text_ink[i]) as f64 >= need) as usize;
        }
    }
    // Closing bridges the gaps of sparse contours; opening then drops what
    // is thinner than a few cells: a frame drawn round the page, a line of
    // text, a scale bar (closing joins them to the map when they run close by).
    let closed = erode(&dilate(&busy, cw, ch, CLOSE_CELLS), cw, ch, CLOSE_CELLS);
    let opened = dilate(&erode(&closed, cw, ch, OPEN_CELLS), cw, ch, OPEN_CELLS);
    let groups = map_cells(&opened, &busy, cw, ch);
    let Some((x0, y0, x1, y1)) =
        groups.iter().copied().reduce(|a, b| (a.0.min(b.0), a.1.min(b.1), a.2.max(b.2), a.3.max(b.3)))
    else {
        return MapContent { text_cells, ..none };
    };
    // Cells to base pixels, one cell of margin.
    let to_px = |(x0, y0, x1, y1): (usize, usize, usize, usize)| MapRect {
        x0: (x0.saturating_sub(1) * cell).min(img.w),
        y0: (y0.saturating_sub(1) * cell).min(img.h),
        x1: ((x1 + 2) * cell).min(img.w),
        y1: ((y1 + 2) * cell).min(img.h),
    };
    let found = to_px((x0, y0, x1, y1));
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
    if !usable {
        return MapContent { found: Some(found), share, outside_ink, text_cells, ..none };
    }
    let edges = [found.x0 == 0, found.y0 == 0, found.x1 == img.w, found.y1 == img.h];
    let extent = if edges.iter().filter(|&&e| e).count() >= EDGES_FOR_RASTER { whole } else { found };
    let parts = if groups.len() > 1 { groups.into_iter().map(to_px).collect() } else { Vec::new() };
    MapContent { rect: found, extent, parts, found: Some(found), share, outside_ink, text_cells }
}

/// Ink pixels per cell (`cell` px, `cw` × `ch` cells, row-major) of the map
/// and of text: connected pieces of ink (8-connected) within
/// `TEXT_MAX_H_CELLS` × `TEXT_MAX_W_CELLS` cells count as text.
fn cell_ink(img: &Rgb8, cell: usize, cw: usize, ch: usize) -> (Vec<u32>, Vec<u32>) {
    let paper = crate::contour_lines::paper_colour(img).map(|v| v as i32);
    let runs: Vec<Vec<(u32, u32)>> = img
        .data
        .par_chunks(img.w * 3)
        .map(|line| {
            let mut r = Vec::new();
            let mut start: Option<usize> = None;
            for (x, p) in line.chunks_exact(3).enumerate() {
                let ink = (0..3).any(|c| paper[c] - p[c] as i32 >= INK_LEVEL);
                match (ink, start) {
                    (true, None) => start = Some(x),
                    (false, Some(s)) => {
                        r.push((s as u32, x as u32));
                        start = None;
                    }
                    _ => {}
                }
            }
            if let Some(s) = start {
                r.push((s as u32, img.w as u32));
            }
            r
        })
        .collect();
    let comps = RunComponents::from_runs(runs);
    let mut bb = vec![(u32::MAX, u32::MAX, 0u32, 0u32); comps.sizes.len()];
    for (y, row) in comps.runs.iter().enumerate() {
        for (i, &(s, e)) in row.iter().enumerate() {
            let b = &mut bb[comps.run_label(y, i) as usize];
            *b = (b.0.min(s), b.1.min(y as u32), b.2.max(e), b.3.max(y as u32 + 1));
        }
    }
    let (max_h, max_w) = ((TEXT_MAX_H_CELLS * cell) as u32, (TEXT_MAX_W_CELLS * cell) as u32);
    let text: Vec<bool> = bb.iter().map(|b| b.3 - b.1 <= max_h && b.2 - b.0 <= max_w).collect();
    let rows: Vec<(Vec<u32>, Vec<u32>)> = (0..ch)
        .into_par_iter()
        .map(|cy| {
            let (mut m, mut t) = (vec![0u32; cw], vec![0u32; cw]);
            for y in cy * cell..((cy + 1) * cell).min(img.h) {
                for (i, &(s, e)) in comps.runs[y].iter().enumerate() {
                    let counts = if text[comps.run_label(y, i) as usize] { &mut t } else { &mut m };
                    let (s, e) = (s as usize, e as usize);
                    for (cx, c) in counts.iter_mut().enumerate().take((e - 1) / cell + 1).skip(s / cell) {
                        *c += (e.min((cx + 1) * cell) - s.max(cx * cell)) as u32;
                    }
                }
            }
            (m, t)
        })
        .collect();
    let (mut map_ink, mut text_ink) = (Vec::with_capacity(cw * ch), Vec::with_capacity(cw * ch));
    for (m, t) in rows {
        map_ink.extend(m);
        text_ink.extend(t);
    }
    (map_ink, text_ink)
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

/// Cell boxes (x0, y0, x1, y1, inclusive).
type CellBox = (usize, usize, usize, usize);

/// Bounding boxes (cells, inclusive) of the blocks of map cells (`m`, the
/// opened mask): its 8-connected groups, each cut along the blank bands
/// across it (`split_gutters`: two maps side by side, which closing joins),
/// that hold at least `MIN_PART` of the largest block's cells (a map cut in
/// two by a lake or the sea is one map; a block of legend text is not),
/// largest first.
fn map_cells(m: &[bool], busy: &[bool], w: usize, h: usize) -> Vec<CellBox> {
    let mut seen = vec![false; w * h];
    let mut blocks: Vec<(usize, CellBox)> = Vec::new();
    let mut stack = Vec::new();
    for start in 0..w * h {
        if !m[start] || seen[start] {
            continue;
        }
        seen[start] = true;
        stack.push(start);
        let mut bb = (usize::MAX, usize::MAX, 0usize, 0usize);
        while let Some(i) = stack.pop() {
            let (x, y) = (i % w, i / w);
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
        split_gutters(m, busy, w, bb, &mut blocks);
    }
    let Some(largest) = blocks.iter().map(|g| g.0).max() else { return Vec::new() };
    blocks.sort_by_key(|g| std::cmp::Reverse(g.0));
    blocks.into_iter().filter(|g| g.0 > 0 && g.0 as f64 >= MIN_PART * largest as f64).map(|g| g.1).collect()
}

/// A blank band at least this many cells wide right across a block of map
/// cells parts two blocks (maps laid side by side or one over the other on
/// a page: 100-120 px apart at 300 dpi, which closing bridges).
const GUTTER_CELLS: usize = 3;

/// Cut the block `bb` (cells of `m`) along its widest band of rows or
/// columns without a busy cell (`busy`) of `GUTTER_CELLS` or more,
/// recursively, and push each piece (its cells of `m`, its box) to `out`.
fn split_gutters(m: &[bool], busy: &[bool], w: usize, bb: CellBox, out: &mut Vec<(usize, CellBox)>) {
    let (x0, y0, x1, y1) = bb;
    let row_blank = |y: usize| !(x0..=x1).any(|x| busy[y * w + x]);
    let col_blank = |x: usize| !(y0..=y1).any(|y| busy[y * w + x]);
    // The widest interior band of blank lines along one axis: (start, end).
    let band = |lo: usize, hi: usize, blank: &dyn Fn(usize) -> bool| -> Option<(usize, usize)> {
        let (mut best, mut run): (Option<(usize, usize)>, Option<usize>) = (None, None);
        for v in lo..=hi {
            match (blank(v), run) {
                (true, None) => run = Some(v),
                (false, Some(s)) => {
                    if s > lo && v - s >= GUTTER_CELLS && best.is_none_or(|b| v - s > b.1 - b.0) {
                        best = Some((s, v));
                    }
                    run = None;
                }
                _ => {}
            }
        }
        best
    };
    // Tighten a box to its cells of `m`; None when it holds none.
    let tight = |(x0, y0, x1, y1): CellBox| -> Option<(usize, CellBox)> {
        let mut n = 0;
        let mut b = (usize::MAX, usize::MAX, 0usize, 0usize);
        for y in y0..=y1 {
            for x in x0..=x1 {
                if m[y * w + x] {
                    n += 1;
                    b = (b.0.min(x), b.1.min(y), b.2.max(x), b.3.max(y));
                }
            }
        }
        (n > 0).then_some((n, b))
    };
    let pieces = match (band(y0, y1, &row_blank), band(x0, x1, &col_blank)) {
        (Some(r), c) if c.is_none_or(|c| r.1 - r.0 >= c.1 - c.0) => [(x0, y0, x1, r.0 - 1), (x0, r.1, x1, y1)],
        (_, Some(c)) => [(x0, y0, c.0 - 1, y1), (c.1, y0, x1, y1)],
        _ => {
            if let Some(t) = tight(bb) {
                out.push(t);
            }
            return;
        }
    };
    for p in pieces {
        if let Some((_, b)) = tight(p) {
            split_gutters(m, busy, w, b, out);
        }
    }
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

    /// Glyph-like boxes (4 px strokes, 18 × 30 px, words of 2–7) in lines
    /// 60 px apart over `rect`: a paragraph of 10–12 pt text at 300 dpi.
    fn paragraph(img: &mut Rgb8, rect: MapRect) {
        let w = img.w;
        let mut k = 0usize;
        for base in (rect.y0 + 40..rect.y1).step_by(60) {
            let mut x = rect.x0;
            while x + 30 < rect.x1 {
                k += 1;
                if k.is_multiple_of(6) {
                    x += 16;
                    continue;
                }
                for y in base - 30..base {
                    for xx in x..x + 18 {
                        let edge = y < base - 26 || y >= base - 4 || xx < x + 4 || xx >= x + 14;
                        if edge && (!k.is_multiple_of(3) || xx < x + 4) {
                            let i = (y * w + xx) * 3;
                            img.data[i..i + 3].copy_from_slice(&[25, 25, 25]);
                        }
                    }
                }
                x += 24;
            }
        }
    }

    /// A paragraph of text beside the map is a solid block of busy cells,
    /// which the opening keeps: it is no map (its pieces of ink are glyphs),
    /// above, below or beside the map, close to it or far.
    #[test]
    fn text_is_no_map() {
        let map = MapRect { x0: 300, y0: 300, x1: 1418, y1: 1091 };
        for text in [
            MapRect { x0: 300, y0: 1150, x1: 2200, y1: 3300 },
            MapRect { x0: 1470, y0: 300, x1: 2300, y1: 1100 },
            MapRect { x0: 300, y0: 1900, x1: 2200, y1: 2600 },
        ] {
            let mut img = page(2480, 3508, map);
            paragraph(&mut img, text);
            let c = map_content(&img);
            let r = c.rect;
            assert!(r.x0 <= map.x0 && r.y0 <= map.y0 && r.x1 >= map.x1 && r.y1 >= map.y1, "{text:?}: {c:?}");
            assert!(r.x1 <= map.x1 + 60 && r.y1 <= map.y1 + 220 && c.share < 0.2, "{text:?}: {c:?}");
            assert!(c.text_cells > 500 && c.parts.is_empty() && c.extent == r, "{text:?}: {c:?}");
        }
    }

    /// Two maps side by side, 100 px apart (closing joins them): two parts
    /// of one rectangle. A map that runs to two edges of the raster or more
    /// (the rest blank) may lie on the blank part: its extent is the raster.
    #[test]
    fn parts_and_extent() {
        let (a, b) =
            (MapRect { x0: 200, y0: 400, x1: 1200, y1: 1100 }, MapRect { x0: 1300, y0: 400, x1: 2300, y1: 1100 });
        let mut img = page(3508, 2480, a);
        let other = page(3508, 2480, b);
        for y in b.y0..b.y1 {
            img.data[(y * 3508 + b.x0) * 3..(y * 3508 + b.x1) * 3]
                .copy_from_slice(&other.data[(y * 3508 + b.x0) * 3..(y * 3508 + b.x1) * 3]);
        }
        let c = map_content(&img);
        assert_eq!(c.parts.len(), 2, "{c:?}");
        let (p, q) = if c.parts[0].x0 < c.parts[1].x0 { (c.parts[0], c.parts[1]) } else { (c.parts[1], c.parts[0]) };
        assert!(p.x0 <= a.x0 && p.x1 >= a.x1 && p.x1 < b.x0 && q.x0 > a.x1 && q.x1 >= b.x1, "{c:?}");
        assert_eq!(c.extent, c.rect);
        // The western quarter of a full-bleed map, the rest blank.
        let strip = MapRect { x0: 0, y0: 0, x1: 900, y1: 2480 };
        let c = map_content(&page(3508, 2480, strip));
        assert!(c.rect.x1 < 1000 && c.extent.is_whole(3508, 2480), "{c:?}");
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
