//! Synthetic terrain anywhere on Earth, served tile by tile like the app's
//! need_dem loop, and map sheets drawn from it (tests/wide.rs).

use mapme_core::dem::{resolution, tile_bounds, TileId, TILE_SIZE};
use mapme_core::geo::{latlon_to_merc, Affine, LatLon};
use mapme_core::georef::Session;
use mapme_core::image::Rgb8;
use rayon::prelude::*;
use serde_json::{json, Value};
use std::collections::{HashMap, HashSet};
use std::time::Instant;

/// Elevation (metres) at a Mercator point.
pub type Terrain = dyn Fn(f64, f64) -> f64 + Sync;

/// Smooth random terrain anywhere on Earth: hashed value noise (smoothstep
/// interpolation) in four octaves over ground-ish metres.
pub fn noise_terrain(mx: f64, my: f64) -> f64 {
    noise_terrain_seeded(mx, my, 0)
}

/// `noise_terrain` with another seed (an unrelated landscape).
pub fn noise_terrain_seeded(mx: f64, my: f64, seed: u64) -> f64 {
    fn hash(i: i64, j: i64, o: u64) -> f64 {
        let mut h = (i as u64).wrapping_mul(0x9E37_79B9_7F4A_7C15) ^ (j as u64).wrapping_mul(0xC2B2_AE3D_27D4_EB4F) ^ o;
        h ^= h >> 29;
        h = h.wrapping_mul(0xBF58_476D_1CE4_E5B9);
        h ^= h >> 32;
        (h >> 11) as f64 / (1u64 << 53) as f64 * 2.0 - 1.0
    }
    let s = |t: f64| t * t * (3.0 - 2.0 * t);
    let mut z = 1200.0;
    for (o, &(lambda, amp)) in [(9_000.0, 320.0), (3_200.0, 130.0), (1_200.0, 45.0), (450.0, 12.0)].iter().enumerate() {
        let (u, v) = (mx / lambda, my / lambda);
        let (i, j) = (u.floor(), v.floor());
        let (fu, fv) = (s(u - i), s(v - j));
        let (i, j) = (i as i64, j as i64);
        let o = o as u64 * 7919 + seed.wrapping_mul(0x2545_F491_4F6C_DD1D);
        let a = hash(i, j, o) * (1.0 - fu) + hash(i + 1, j, o) * fu;
        let b = hash(i, j + 1, o) * (1.0 - fu) + hash(i + 1, j + 1, o) * fu;
        z += amp * (a * (1.0 - fv) + b * fv);
    }
    z
}

/// A Terrarium PNG of any tile of `terrain`.
pub fn terrarium_png_of(t: TileId, terrain: &Terrain) -> Vec<u8> {
    let b = tile_bounds(t);
    let res = resolution(t.z);
    let mut rgb = vec![0u8; TILE_SIZE * TILE_SIZE * 3];
    for r in 0..TILE_SIZE {
        for c in 0..TILE_SIZE {
            let e = terrain(b.min_x + (c as f64 + 0.5) * res, b.max_y - (r as f64 + 0.5) * res) + 32768.0;
            let e = e.clamp(0.0, 65535.99);
            let i = (r * TILE_SIZE + c) * 3;
            rgb[i] = (e / 256.0).floor() as u8;
            rgb[i + 1] = (e.floor() as i64).rem_euclid(256) as u8;
            rgb[i + 2] = ((e - e.floor()) * 256.0).floor() as u8;
        }
    }
    let mut out = Vec::new();
    {
        let mut enc = png::Encoder::new(&mut out, TILE_SIZE as u32, TILE_SIZE as u32);
        enc.set_color(png::ColorType::Rgb);
        enc.set_depth(png::BitDepth::Eight);
        let mut w = enc.write_header().unwrap();
        w.write_image_data(&rgb).unwrap();
    }
    out
}

/// A Terrarium PNG of any tile of `noise_terrain`.
pub fn terrarium_png(t: TileId) -> Vec<u8> {
    terrarium_png_of(t, &noise_terrain)
}

/// Ground metres per pixel of a 1:`denom` sheet scanned at 300 dpi.
pub fn sheet_scale(denom: f64) -> f64 {
    denom * 0.0254 / 300.0
}

/// A `w` × `h` sheet of `terrain` at 1:`denom` (300 dpi, 20 m contours,
/// brown on white, north up) centred at `at`, and its "1:… " scale label.
pub fn sheet_of(at: LatLon, w: usize, h: usize, denom: f64, terrain: &Terrain) -> (Rgb8, Value) {
    let scale = sheet_scale(denom);
    let c = latlon_to_merc(at.lat, at.lon);
    let k = 1.0 / at.lat.to_radians().cos();
    let tr = Affine::pixel_to_merc_similarity(scale * k, 0.0, w as f64 / 2.0, h as f64 / 2.0, c.0, c.1);
    let lvl: Vec<i64> = (0..w * h)
        .into_par_iter()
        .map(|i| {
            let (mx, my) = tr.apply((i % w) as f64 + 0.5, (i / w) as f64 + 0.5);
            (terrain(mx, my) / 20.0).floor() as i64
        })
        .collect();
    let mut data = vec![255u8; w * h * 3];
    for y in 0..h {
        for x in 0..w {
            let l = lvl[y * w + x];
            if (x + 1 < w && lvl[y * w + x + 1] != l) || (y + 1 < h && lvl[(y + 1) * w + x] != l) {
                for (dx, dy) in [(0usize, 0usize), (1, 0), (0, 1)] {
                    if x + dx < w && y + dy < h {
                        let i = ((y + dy) * w + x + dx) * 3;
                        data[i..i + 3].copy_from_slice(&[168, 120, 70]);
                    }
                }
            }
        }
    }
    let label = format!("1:{} {:03}", (denom / 1000.0).floor() as u64, (denom % 1000.0) as u64);
    let ocr = json!([{"text": label, "bbox": [w as f64 * 0.8, h as f64 - 50.0, 120.0, 25.0], "confidence": 0.9}]);
    (Rgb8::from_rgb(w, h, data).unwrap(), ocr)
}

/// A 1:50 000 sheet of `noise_terrain` (6.4 × 8.0 km) centred at `at`, and
/// its "1:50 000" label.
pub fn synthetic_sheet(at: LatLon) -> (Rgb8, Value) {
    sheet_of(at, 1500, 1900, 50_000.0, &noise_terrain)
}

/// What the app's loop saw: every response, the tiles served (and how
/// often each was asked for), core seconds per call and a summary line.
pub struct Served {
    pub responses: Vec<Value>,
    pub served: HashSet<TileId>,
    pub asked: HashMap<TileId, usize>,
    pub core_s: Vec<f64>,
    /// DEM tiles the session held after each call.
    pub held: Vec<usize>,
    pub log: String,
}

impl Served {
    pub fn last(&self) -> &Value {
        self.responses.last().unwrap()
    }

    /// Tiles of zoom `z` asked for more than once.
    pub fn asked_twice(&self, z: u8) -> usize {
        self.asked.iter().filter(|(t, n)| t.z == z && **n > 1).count()
    }
}

pub fn tiles_of(r: &Value) -> Vec<TileId> {
    r["need_tiles"]
        .as_array()
        .map(|v| {
            v.iter()
                .map(|t| TileId {
                    z: t["z"].as_u64().unwrap() as u8,
                    x: t["x"].as_u64().unwrap() as u32,
                    y: t["y"].as_u64().unwrap() as u32,
                })
                .collect()
        })
        .unwrap_or_default()
}

/// The app's loop with `terrain` served for every tile asked for (at most
/// `max_tiles` per answer, checked), up to `max_rounds` calls; `before`
/// runs before every call (round index).
pub fn serve(
    s: &Session,
    req: &Value,
    max_tiles: usize,
    max_rounds: usize,
    terrain: &Terrain,
    before: &mut dyn FnMut(usize),
) -> Served {
    let t = Instant::now();
    let mut out = Served {
        responses: Vec::new(),
        served: HashSet::new(),
        asked: HashMap::new(),
        core_s: Vec::new(),
        held: Vec::new(),
        log: "".into(),
    };
    let mut tiles_s = 0.0;
    let mut rounds: Vec<String> = Vec::new();
    for round in 0..max_rounds {
        before(round);
        let tc = Instant::now();
        let r: Value = serde_json::from_str(&s.georeference_json(&req.to_string())).unwrap();
        out.core_s.push(tc.elapsed().as_secs_f64());
        out.held.push(s.dem_tiles_held());
        let tiles = tiles_of(&r);
        rounds.push(format!("{}:{}", r["search"]["stage"].as_str().unwrap_or("-"), tiles.len()));
        let need = r["status"] == "need_dem";
        if need {
            assert!(!tiles.is_empty() && tiles.len() <= max_tiles, "{} tiles in one round", tiles.len());
            let tt = Instant::now();
            let pngs: Vec<(TileId, Vec<u8>)> = tiles.par_iter().map(|&t| (t, terrarium_png_of(t, terrain))).collect();
            tiles_s += tt.elapsed().as_secs_f64();
            for (t, png) in pngs {
                s.add_dem_tile(t, &png).unwrap();
                out.served.insert(t);
                *out.asked.entry(t).or_insert(0) += 1;
            }
        }
        out.responses.push(r);
        if !need {
            out.log = format!(
                "{:.1} s total ({:.1} s core, {:.1} s making tiles), {} tiles served, {} rounds: {}",
                t.elapsed().as_secs_f64(),
                out.core_s.iter().sum::<f64>(),
                tiles_s,
                out.served.len(),
                rounds.len(),
                rounds.join(" ")
            );
            return out;
        }
    }
    panic!("still need_dem after {max_rounds} rounds: {}", rounds.join(" "));
}

/// The app's loop with synthetic terrain served for every tile asked for.
/// Returns the responses, the tiles served and a per-round log.
pub fn synthetic_loop(s: &Session, req: &Value, max_tiles: usize) -> (Vec<Value>, HashSet<TileId>, String) {
    let r = serve(s, req, max_tiles, 80, &noise_terrain, &mut |_| {});
    (r.responses, r.served, r.log)
}
