//! The terrain engine's behaviour (docs/design/explorer.md, Terrain; docs/ARCHITECTURE.md, "C ABI",
//! Terrain): the store's byte bound and least-recently-used eviction under concurrency, elevations
//! and profiles on synthetic tiles (a cone's ascent equals its height within the smoothing), the
//! zoom fallback and `need_tiles` order, grids with unknown parts, renders that agree across any
//! tiling and draw lines exactly where the contours are, the interval rule, tile plans against
//! `tiles_covering`, circles, the antimeridian and the trailhead caps; on the real Grootkloof sheet:
//! spot heights, the DEM's contours falling on the printed ones, and the time budgets.

mod common;

use common::{dem_tiles, time_budget_ms, truth};
use mapme_core::dem::{resolution, tile_bounds, tiles_covering, MercBox, TileId, TILE_SIZE};
use mapme_core::geo::{haversine_m, latlon_to_merc, merc_to_latlon, LatLon};
use mapme_core::terrain::*;
use mapme_core::warp::Warp;
use serde_json::{json, Value};
use std::collections::BTreeSet;
use std::time::Instant;

const LAT: f64 = -25.85;
const LON: f64 = 27.40;

/// Synthetic tiles at zoom `z` over a Mercator box: elevations `f(mx, my)` at pixel centres.
fn synth(store: &TerrainStore, z: u8, b: &MercBox, f: &dyn Fn(f64, f64) -> f64) {
    let res = resolution(z);
    for t in tiles_covering(z, b) {
        let tb = tile_bounds(t);
        let mut e = vec![0f32; TILE_SIZE * TILE_SIZE];
        for r in 0..TILE_SIZE {
            for c in 0..TILE_SIZE {
                e[r * TILE_SIZE + c] = f(tb.min_x + (c as f64 + 0.5) * res, tb.max_y - (r as f64 + 0.5) * res) as f32;
            }
        }
        store.add_elevations(t, e).unwrap();
    }
}

fn centre() -> (f64, f64) {
    latlon_to_merc(LAT, LON)
}

/// A cone `height` m high, `radius_m` (ground) wide, on a 1000 m plain, at (LAT, LON).
fn cone(height: f64, radius_m: f64) -> impl Fn(f64, f64) -> f64 {
    let (cx, cy) = centre();
    let k = LAT.to_radians().cos();
    move |mx, my| {
        let r = (mx - cx).hypot(my - cy) * k;
        1000.0 + height * (1.0 - r / radius_m).max(0.0)
    }
}

/// Answer as JSON (what the C ABI returns).
fn json_of<T: serde::Serialize>(answer: Result<T, String>) -> Value {
    serde_json::from_str(&answer_json(answer)).unwrap()
}

/// Run a query, marking every tile it asks for missing, until it is complete.
fn resolve_missing(store: &TerrainStore, mut q: impl FnMut() -> Value) -> Value {
    for _ in 0..20 {
        let v = q();
        if v["status"] != "need_dem" {
            return v;
        }
        let need: Vec<TileId> = serde_json::from_value(v["need_tiles"].clone()).unwrap();
        for t in need {
            store.mark_missing(t).unwrap();
        }
    }
    panic!("never complete");
}

// ---------------------------------------------------------------------------------------------
// Store
// ---------------------------------------------------------------------------------------------

#[test]
fn store_bound_and_eviction_under_concurrency() {
    // 4 MiB hold 16 tiles of 256 KiB.
    let store = TerrainStore::new(4 << 20);
    let flat = vec![500f32; TILE_SIZE * TILE_SIZE];
    std::thread::scope(|s| {
        for k in 0..4u32 {
            let (store, flat) = (&store, &flat);
            s.spawn(move || {
                for i in 0..10u32 {
                    store.add_elevations(TileId { z: 12, x: 2000 + k * 10 + i, y: 2352 }, flat.clone()).unwrap();
                    // Queries meanwhile see a consistent store.
                    let r = store.elevations(&parse_request(r#"{"points":[[-25.84,27.4]]}"#).unwrap()).unwrap();
                    assert_eq!(r.elevations_m.len(), 1);
                }
            });
        }
    });
    let st = store.stats();
    assert_eq!(st.tiles, 16, "{st:?}");
    assert_eq!(st.bytes, 16 * TILE_BYTES);
    assert!(st.bytes <= st.max_bytes);
    assert_eq!(st.evicted, 24);
}

#[test]
fn least_recently_used_tile_goes_first() {
    let store = TerrainStore::new(4 << 20);
    let tile = |i: u32| TileId { z: 13, x: 4710 + i, y: 4704 };
    for i in 0..16 {
        store.add_elevations(tile(i), vec![100.0 + i as f32; TILE_SIZE * TILE_SIZE]).unwrap();
    }
    // Use tile 0, then add one more: tile 1 is now the least recently used.
    let point = |t: TileId| {
        let b = tile_bounds(t);
        let (lat, lon) = merc_to_latlon((b.min_x + b.max_x) / 2.0, (b.min_y + b.max_y) / 2.0);
        json!({"points": [[lat, lon]], "min_zoom": 13})
    };
    let ask = |t: TileId| json_of(store.elevations(&parse_request(&point(t).to_string()).unwrap()));
    assert_eq!(ask(tile(0))["status"], "ok");
    store.add_elevations(tile(16), vec![1.0; TILE_SIZE * TILE_SIZE]).unwrap();
    assert_eq!(ask(tile(0))["elevations_m"][0], 100.0);
    let v = ask(tile(1));
    assert_eq!(v["status"], "need_dem", "{v}");
    assert_eq!(v["need_tiles"][0], json!({"z": 13, "x": 4711, "y": 4704}));
    assert_eq!(store.stats().evicted, 1);
}

#[test]
fn undecodable_tiles_are_marked_missing_and_good_ones_clear_the_mark() {
    let store = TerrainStore::new(0);
    let t = TileId { z: 12, x: 2359, y: 2352 };
    let png = [0x89u8, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A, 0, 0, 0];
    assert!(matches!(store.add_png(t, &png), Err(TerrainError::Decode(_))));
    assert_eq!(store.stats().missing, 1);
    // A held tile is not marked missing; a good PNG clears the mark.
    let (tid, bytes) = dem_tiles().into_iter().find(|(id, _)| id.z == 12).unwrap();
    store.mark_missing(tid).unwrap();
    store.add_png(tid, &bytes).unwrap();
    let st = store.stats();
    assert_eq!((st.tiles, st.missing), (1, 1));
    store.mark_missing(tid).unwrap();
    assert_eq!(store.stats().missing, 1);
    store.clear_missing();
    assert_eq!(store.stats().missing, 0);
}

// ---------------------------------------------------------------------------------------------
// Elevations, zoom fallback, need_tiles
// ---------------------------------------------------------------------------------------------

#[test]
fn elevations_are_bilinear_on_a_plane() {
    let store = TerrainStore::new(0);
    let (cx, cy) = centre();
    let plane = move |mx: f64, my: f64| 1200.0 + 0.03 * (mx - cx) - 0.02 * (my - cy);
    synth(&store, 13, &MercBox::around(cx, cy, 3000.0), &plane);
    let pts: Vec<[f64; 2]> = (0..20)
        .map(|k| {
            let (lat, lon) = merc_to_latlon(cx - 2000.0 + k as f64 * 211.3, cy + 1500.0 - k as f64 * 157.7);
            [lat, lon]
        })
        .collect();
    let r = store.elevations(&parse_request(&json!({ "points": pts }).to_string()).unwrap()).unwrap();
    assert_eq!(r.status, Status::Ok);
    assert_eq!(r.zoom, Some(13));
    for (p, e) in pts.iter().zip(&r.elevations_m) {
        let (mx, my) = latlon_to_merc(p[0], p[1]);
        assert!((e.unwrap() - plane(mx, my)).abs() < 0.02, "{e:?} vs {}", plane(mx, my));
    }
}

#[test]
fn fallback_to_coarser_zooms_and_need_tiles_nearest_first() {
    let store = TerrainStore::new(0);
    let (cx, cy) = centre();
    synth(&store, 12, &MercBox::around(cx, cy, 14_000.0), &|_, _| 777.0);
    // Points 5 km apart (a zoom-13 tile is 4.9 km): zoom 13 is wanted, zoom 12 answers meanwhile.
    let pts: Vec<[f64; 2]> = (-2..=2)
        .map(|k| {
            let (lat, lon) = merc_to_latlon(cx + k as f64 * 5000.0, cy);
            [lat, lon]
        })
        .collect();
    let req: ElevationsRequest = parse_request(&json!({ "points": pts }).to_string()).unwrap();
    let r = store.elevations(&req).unwrap();
    assert_eq!(r.status, Status::NeedDem);
    assert!(r.elevations_m.iter().all(|e| *e == Some(777.0)));
    assert_eq!(r.zoom, Some(12));
    assert!(r.need_tiles.iter().all(|t| t.z == 13));
    // Nearest the points' centre first.
    let dist = |t: &TileId| {
        let b = tile_bounds(*t);
        ((b.min_x + b.max_x) / 2.0 - cx).hypot((b.min_y + b.max_y) / 2.0 - cy)
    };
    let d: Vec<f64> = r.need_tiles.iter().map(dist).collect();
    assert!(d.windows(2).all(|w| w[0] <= w[1]), "{d:?}");
    assert_eq!(r.need_tiles.len(), 5);
    // Marked missing: complete at zoom 12.
    for t in &r.need_tiles {
        store.mark_missing(*t).unwrap();
    }
    let r = store.elevations(&req).unwrap();
    assert_eq!((r.status, r.zoom), (Status::Ok, Some(12)));
    // max_tiles bounds the list; nothing at all: null, and zooms below min_zoom are not asked for.
    let empty = TerrainStore::new(0);
    let r = empty.elevations(&parse_request(&json!({ "points": pts, "max_tiles": 2 }).to_string()).unwrap()).unwrap();
    assert_eq!(r.need_tiles.len(), 2);
    assert!(r.elevations_m.iter().all(|e| e.is_none()));
    let v = resolve_missing(&empty, || json_of(empty.elevations(&req)));
    assert_eq!(v["status"], "ok");
    assert!(v["zoom"].is_null());
    let asked: usize = (10..=13u8)
        .map(|z| {
            let set: BTreeSet<TileId> = pts
                .iter()
                .map(|p| {
                    let (mx, my) = latlon_to_merc(p[0], p[1]);
                    tiles_covering(z, &MercBox::around(mx, my, 0.0))[0]
                })
                .collect();
            set.len()
        })
        .sum();
    assert_eq!(empty.stats().missing, asked, "zooms 13, 12, 11, 10 asked for, then nothing");
}

#[test]
fn bad_requests_fail() {
    let store = TerrainStore::new(0);
    for req in [
        json!({"points": [[95.0, 0.0]]}),
        json!({"points": [[0.0, 0.0]], "max_zoom": 16}),
        json!({"points": [[0.0, 0.0]], "max_zoom": 10, "min_zoom": 11}),
    ] {
        assert_eq!(json_of(store.elevations(&parse_request(&req.to_string()).unwrap()))["status"], "failed", "{req}");
    }
    for req in [json!({"points": []}), json!({"points": [[0.0, 0.0]], "spacing_m": 0.0})] {
        assert_eq!(json_of(store.profile(&parse_request(&req.to_string()).unwrap()))["status"], "failed", "{req}");
    }
}

// ---------------------------------------------------------------------------------------------
// Profiles
// ---------------------------------------------------------------------------------------------

fn cone_store() -> TerrainStore {
    let store = TerrainStore::new(0);
    let (cx, cy) = centre();
    synth(&store, 13, &MercBox::around(cx, cy, 7000.0), &cone(400.0, 3000.0));
    store
}

#[test]
fn a_cones_ascent_equals_its_height() {
    let store = cone_store();
    let (cx, cy) = centre();
    let k = LAT.to_radians().cos();
    let p = |dx_m: f64, dy_m: f64| {
        let (lat, lon) = merc_to_latlon(cx + dx_m / k, cy + dy_m / k);
        [lat, lon]
    };
    // West to east over the summit.
    let req: ProfileRequest =
        parse_request(&json!({ "points": [p(-4000.0, 0.0), p(4000.0, 0.0)] }).to_string()).unwrap();
    let r = store.profile(&req).unwrap();
    assert_eq!(r.status, Status::Ok);
    assert_eq!(r.zoom, Some(13));
    let d =
        haversine_m(LatLon::new(req.points[0][0], req.points[0][1]), LatLon::new(req.points[1][0], req.points[1][1]));
    assert!((r.distance_m - d).abs() < 0.01, "{} vs {d}", r.distance_m);
    assert!((r.distance_m - 8000.0).abs() < 30.0);
    let (asc, desc) = (r.ascent_m.unwrap(), r.descent_m.unwrap());
    assert!((asc - 400.0).abs() < 6.0, "ascent {asc}");
    assert!((desc - 400.0).abs() < 6.0, "descent {desc}");
    assert!((r.min_m.unwrap() - 1000.0).abs() < 0.5);
    assert!((r.max_m.unwrap() - 1400.0).abs() < 6.0, "{:?}", r.max_m);
    // Samples: evenly spaced, first at 0, last at the end; vertices at 0 and the length.
    assert!(r.samples.len() <= DEFAULT_MAX_SAMPLES && r.samples.len() > 500, "{}", r.samples.len());
    assert_eq!(r.samples[0].0, 0.0);
    assert!((r.samples.last().unwrap().0 - r.distance_m).abs() < 0.02);
    let steps: Vec<f64> = r.samples.windows(2).map(|w| w[1].0 - w[0].0).collect();
    assert!(steps.iter().all(|s| (s - steps[0]).abs() < 0.02), "uneven samples");
    assert_eq!(r.vertex_distance_m, vec![0.0, r.distance_m]);
    assert!(r.vertex_elevation_m.iter().all(|e| (e.unwrap() - 1000.0).abs() < 0.01));
    assert_eq!(r.missing_fraction, 0.0);

    // max_samples: exactly that many, evenly spaced over the same length.
    let few: ProfileRequest = parse_request(
        &json!({ "points": [p(-4000.0, 0.0), p(0.0, 500.0), p(4000.0, 0.0)], "max_samples": 50 }).to_string(),
    )
    .unwrap();
    let r2 = store.profile(&few).unwrap();
    assert_eq!(r2.samples.len(), 50);
    assert!((r2.samples[49].0 - r2.distance_m).abs() < 0.02);
    assert_eq!(r2.vertex_distance_m.len(), 3);
    assert!((r2.vertex_distance_m[1] - 4031.1).abs() < 20.0, "{:?}", r2.vertex_distance_m);

    // Around the summit at constant height: no climbing worth counting.
    let ring: Vec<[f64; 2]> =
        (0..=72).map(|i| (i as f64 * 5.0f64).to_radians()).map(|a| p(1500.0 * a.cos(), 1500.0 * a.sin())).collect();
    let r3 = store.profile(&parse_request(&json!({ "points": ring }).to_string()).unwrap()).unwrap();
    assert!(r3.ascent_m.unwrap() < 3.0 && r3.descent_m.unwrap() < 3.0, "{:?} {:?}", r3.ascent_m, r3.descent_m);
    assert!((r3.min_m.unwrap() - 1200.0).abs() < 3.0);
}

#[test]
fn profiles_without_terrain_say_so() {
    let store = TerrainStore::new(0);
    let req: ProfileRequest =
        parse_request(&json!({ "points": [[LAT, LON], [LAT + 0.01, LON + 0.01]] }).to_string()).unwrap();
    let r = store.profile(&req).unwrap();
    assert_eq!(r.status, Status::NeedDem);
    assert!(r.ascent_m.is_none() && r.min_m.is_none());
    assert!(r.samples.iter().all(|s| s.1.is_none()));
    assert!(r.distance_m > 1400.0);
    let v = resolve_missing(&store, || json_of(store.profile(&req)));
    assert_eq!(v["status"], "ok");
    assert_eq!(v["missing_fraction"], 1.0);
    assert!(v["ascent_m"].is_null());
    // A single point: no length, its elevation.
    let one = cone_store();
    let r = one.profile(&parse_request(&json!({ "points": [[LAT, LON]] }).to_string()).unwrap()).unwrap();
    assert_eq!(r.distance_m, 0.0);
    assert_eq!(r.samples.len(), 1);
    assert!((r.samples[0].1.unwrap() - 1400.0).abs() < 1.0);
    assert_eq!(r.ascent_m, Some(0.0));
}

#[test]
fn profiles_cross_the_antimeridian_the_short_way() {
    let store = TerrainStore::new(0);
    let r = store
        .profile(&parse_request(&json!({ "points": [[-17.0, 179.99], [-17.0, -179.99]] }).to_string()).unwrap())
        .unwrap();
    assert!((r.distance_m - 2127.0).abs() < 5.0, "{}", r.distance_m);
    let xs: BTreeSet<u32> = r.need_tiles.iter().map(|t| t.x).collect();
    assert_eq!(xs, BTreeSet::from([0, 8191]), "{:?}", r.need_tiles);
}

// ---------------------------------------------------------------------------------------------
// Grids
// ---------------------------------------------------------------------------------------------

#[test]
fn grids_follow_the_terrain_and_leave_unknown_parts_nan() {
    let store = TerrainStore::new(0);
    let (cx, cy) = centre();
    // Terrain only west of a zoom-12 tile edge near the centre.
    let edge = tile_bounds(tiles_covering(12, &MercBox::around(cx, cy, 0.0))[0]).min_x;
    let plane = move |mx: f64, my: f64| 1500.0 + 0.01 * (mx - cx) - 0.02 * (my - cy);
    synth(
        &store,
        12,
        &MercBox { min_x: edge - 9000.0, min_y: cy - 6000.0, max_x: edge - 1.0, max_y: cy + 6000.0 },
        &plane,
    );
    let rect = [edge - 2500.0, cy - 2000.0, edge + 2500.0, cy + 2000.0];
    let req: GridRequest = parse_request(&json!({ "rect": rect }).to_string()).unwrap();
    let (cols, rows) = (64usize, 48usize);
    let mut heights = vec![0f32; cols * rows];
    let v = resolve_missing(&store, || json_of(store.grid(&req, &mut heights, cols, rows)));
    assert_eq!(v["zoom"], 12, "{v}");
    let mut unknown = 0;
    for j in 0..rows {
        for i in 0..cols {
            let mx = rect[0] + i as f64 * (rect[2] - rect[0]) / (cols - 1) as f64;
            let my = rect[3] - j as f64 * (rect[3] - rect[1]) / (rows - 1) as f64;
            let h = heights[j * cols + i];
            if mx < edge - 100.0 {
                assert!((h as f64 - plane(mx, my)).abs() < 0.05, "({i}, {j}): {h} vs {}", plane(mx, my));
            } else if mx > edge + 100.0 {
                assert!(h.is_nan(), "({i}, {j}) east of the terrain: {h}");
            }
            unknown += h.is_nan() as usize;
        }
    }
    let mf = v["missing_fraction"].as_f64().unwrap();
    assert!((mf - unknown as f64 / (cols * rows) as f64).abs() < 1e-9 && mf > 0.4 && mf < 0.6, "{mf}");
    let lo = v["min_m"].as_f64().unwrap();
    assert!((lo - plane(rect[0], rect[3])).abs() < 0.05, "{lo}");
}

// ---------------------------------------------------------------------------------------------
// Rendering
// ---------------------------------------------------------------------------------------------

/// A store with an east–west ramp (`slope` metres per Mercator metre, 0 at cx + `zero_at`) at
/// zooms 11–13 around the centre.
fn ramp_store(slope: f64, zero_at: f64) -> TerrainStore {
    let store = TerrainStore::new(256 << 20);
    let (cx, cy) = centre();
    for z in 11..=13 {
        synth(&store, z, &MercBox::around(cx, cy, 5000.0), &move |mx, _| slope * (mx - cx - zero_at));
    }
    store
}

fn render(store: &TerrainStore, req: &Value, w: usize, h: usize) -> (Vec<u8>, Value) {
    let parsed: RenderRequest = parse_request(&req.to_string()).unwrap();
    let mut rgba = vec![0u8; w * h * 4];
    let v = json_of(store.render(&parsed, &mut rgba, w, h, w * 4));
    (rgba, v)
}

#[test]
fn lines_are_drawn_exactly_where_the_contours_are() {
    // 0.05 m per Mercator metre: a 20 m contour every 400 m, every 51.2 pixels of 7.8125 m.
    let store = ramp_store(0.05, -2000.0);
    let (cx, cy) = centre();
    let rect = [cx - 2000.0, cy - 2000.0, cx + 2000.0, cy + 2000.0];
    let (w, h) = (512usize, 512usize);
    let req = json!({"space": "mercator", "rect": rect, "style": "dark", "interval_m": 20.0, "line_scale": 2.0,
                     "background": [0, 0, 0, 0], "labels": false});
    let (rgba, v) = render(&store, &req, w, h);
    assert_eq!(v["status"], "ok", "{v}");
    assert_eq!((v["interval_m"].as_f64(), v["index_interval_m"].as_f64()), (Some(20.0), Some(100.0)));
    let row = h / 2;
    for i in 0..w {
        let mx = rect[0] + (i as f64 + 0.5) * 4000.0 / w as f64;
        let elev = 0.05 * (mx - cx + 2000.0);
        let off_px = ((elev / 20.0).round() * 20.0 - elev).abs() / (0.05 * 4000.0 / w as f64);
        let alpha = rgba[(row * w + i) * 4 + 3];
        if off_px < 0.3 {
            assert!(alpha > 150, "column {i}: {off_px:.2} px from a contour, alpha {alpha}");
        } else if off_px > 2.0 {
            assert_eq!(alpha, 0, "column {i}: {off_px:.2} px from any contour, alpha {alpha}");
        }
    }
    // Index lines (every 100 m) carry more ink than the others: 1.6× wider.
    let ink_at = |level: f64| {
        let i0 = (level / 0.05 / (4000.0 / w as f64)) as usize;
        (i0 - 6..i0 + 6).map(|i| rgba[(row * w + i) * 4 + 3] as f64 / 255.0).sum::<f64>()
    };
    let (index, minor) = (ink_at(100.0), ink_at(120.0));
    assert!(index > 1.4 * minor && (index - 2.24 * 0.95).abs() < 0.3, "{index:.2} vs {minor:.2}");
}

#[test]
fn below_sea_level_only_the_coast_is_drawn() {
    // −200 m at the west edge to +200 m at the east: contours at 0, 20, … east of the middle.
    let store = ramp_store(0.05, 0.0);
    let (cx, cy) = centre();
    let rect = [cx - 4000.0, cy - 1000.0, cx + 4000.0, cy + 1000.0];
    let (w, h) = (512usize, 128usize);
    let req = json!({"space": "mercator", "rect": rect, "style": "paper", "interval_m": 20.0, "line_scale": 1.0,
                     "background": [0, 0, 0, 0], "hillshade": 0.5, "labels": false});
    let (rgba, _) = render(&store, &req, w, h);
    let alpha_at = |i: usize| rgba[(64 * w + i) * 4 + 3];
    // West of the coast (beyond the half-interval around it): nothing, not even hillshade.
    let coast = w / 2;
    assert!((0..coast - 8).all(|i| alpha_at(i) == 0), "lines or shade at sea");
    assert!((coast - 2..coast + 2).any(|i| alpha_at(i) > 100), "no coast line");
    assert!((coast + 8..w).any(|i| alpha_at(i) > 100), "no contours on land");
}

#[test]
fn the_interval_follows_the_scale_and_unknown_terrain_is_background() {
    let store = TerrainStore::new(0);
    let (cx, cy) = centre();
    let rect = [cx - 2000.0, cy - 2000.0, cx + 2000.0, cy + 2000.0];
    for (scale, interval) in [(1.0, 5.0), (2.4, 10.0), (6.0, 25.0), (30.0, 200.0)] {
        let req = json!({"space": "mercator", "rect": rect, "style": "dark", "scale_m_per_pt": scale});
        let (rgba, v) = render(&store, &req, 64, 64);
        assert_eq!(v["interval_m"].as_f64(), Some(interval), "scale {scale}");
        assert_eq!(v["index_interval_m"].as_f64(), Some(5.0 * interval));
        // No terrain: the dark style's paper everywhere, opaque, and the tiles asked for.
        assert_eq!(v["status"], "need_dem");
        assert_eq!(v["missing_fraction"], 1.0);
        assert!(rgba.chunks_exact(4).all(|p| p == [27, 27, 29, 255]), "{:?}", &rgba[..4]);
    }
    assert_eq!(auto_interval_m(2.4), 10.0);
}

/// A map 1024 px square over the centre with perspective and a warp (a photo of a sheet).
fn photo_map() -> (Value, Value) {
    let (cx, cy) = centre();
    let h = [2.0, 0.15, cx - 1000.0, -0.1, -2.0, cy + 1000.0, 2e-5 / 1000.0, 1e-5 / 1000.0, 1.0];
    let mut warp = Warp::zero(1024, 1024);
    for (k, o) in warp.offsets_m.iter_mut().enumerate() {
        *o = 15.0 * ((k as f64) * 0.37).sin();
    }
    (json!(h), serde_json::to_value(&warp).unwrap())
}

#[test]
fn renders_agree_across_tilings() {
    let store = cone_store();
    let (cx, cy) = centre();
    for z in [11u8, 12] {
        synth(&store, z, &MercBox::around(cx, cy, 7000.0), &cone(400.0, 3000.0));
    }
    // Map space, perspective and warp, a level's tiles sharing scale_m_per_pt: identical pixels.
    let (hom, warp) = photo_map();
    let base = |rect: [f64; 4]| {
        json!({"space": "map", "rect": rect, "pixel_to_mercator": hom, "warp": warp, "style": "dark",
               "scale_m_per_pt": 4.0, "line_scale": 2.0, "hillshade": 0.3})
    };
    let (whole, v) = render(&store, &base([-512.0, -512.0, 1536.0, 1536.0]), 256, 256);
    assert_eq!(v["status"], "ok", "{v}");
    let mut differing = 0usize;
    for ty in 0..4 {
        for tx in 0..4 {
            let r = [-512.0 + tx as f64 * 512.0, -512.0 + ty as f64 * 512.0, tx as f64 * 512.0, ty as f64 * 512.0];
            let (tile, _) = render(&store, &base(r), 64, 64);
            for j in 0..64 {
                for i in 0..64 {
                    let a = &tile[(j * 64 + i) * 4..][..4];
                    let b = &whole[((ty * 64 + j) * 256 + tx * 64 + i) * 4..][..4];
                    differing += (a != b) as usize;
                }
            }
        }
    }
    assert_eq!(differing, 0, "map-space tiles differ from the whole");
    // Mercator tiles of one zoom (as the world view's overlay cuts them): the same, to within
    // rounding of the tile edges' coordinates.
    let tb = tile_bounds(TileId { z: 13, x: 4717, y: 4704 });
    let mreq = |r: [f64; 4]| {
        json!({"space": "mercator", "rect": r, "style": "paper", "hillshade": 0.35, "line_scale": 2.0,
                                    "background": [0, 0, 0, 0]})
    };
    let (whole, _) = render(&store, &mreq([tb.min_x, tb.min_y, tb.max_x, tb.max_y]), 256, 256);
    let half = (tb.max_x - tb.min_x) / 2.0;
    let (mut diff, mut worst) = (0usize, 0u8);
    for (qx, qy) in [(0, 0), (1, 0), (0, 1), (1, 1)] {
        let r = [
            tb.min_x + qx as f64 * half,
            tb.max_y - (qy + 1) as f64 * half,
            tb.min_x + (qx + 1) as f64 * half,
            tb.max_y - qy as f64 * half,
        ];
        let (tile, _) = render(&store, &mreq(r), 128, 128);
        for j in 0..128 {
            for i in 0..128 {
                for c in 0..4 {
                    let d = tile[(j * 128 + i) * 4 + c].abs_diff(whole[((qy * 128 + j) * 256 + qx * 128 + i) * 4 + c]);
                    diff += (d > 0) as usize;
                    worst = worst.max(d);
                }
            }
        }
    }
    assert!(worst <= 2 && diff < 256 * 256 * 4 / 1000, "mercator quarters: {diff} values differ, by up to {worst}");
}

#[test]
fn many_threads_render_one_store_alike() {
    let store = cone_store();
    let (hom, warp) = photo_map();
    let req = |k: usize| {
        let r =
            [(k % 4) as f64 * 256.0, (k / 4) as f64 * 256.0, (k % 4 + 1) as f64 * 256.0, (k / 4 + 1) as f64 * 256.0];
        json!({"space": "map", "rect": r, "pixel_to_mercator": hom, "warp": warp, "style": "paper",
               "scale_m_per_pt": 2.0, "line_scale": 2.0, "hillshade": 0.35})
    };
    let alone: Vec<Vec<u8>> = (0..16).map(|k| render(&store, &req(k), 128, 128).0).collect();
    std::thread::scope(|s| {
        for t in 0..4 {
            let (store, alone, req) = (&store, &alone, &req);
            s.spawn(move || {
                for k in (0..16).map(|k| (k + t * 5) % 16) {
                    assert!(render(store, &req(k), 128, 128).0 == alone[k], "tile {k} differs on thread {t}");
                }
            });
        }
    });
}

// ---------------------------------------------------------------------------------------------
// The real sheet: spot heights, contours on the printed ones, time budgets
// ---------------------------------------------------------------------------------------------

fn grootkloof_store() -> TerrainStore {
    let store = TerrainStore::new(256 << 20);
    for (t, png) in dem_tiles() {
        store.add_png(t, &png).unwrap();
    }
    store
}

fn truth_h() -> [f64; 9] {
    let a = truth();
    [a.a, a.b, a.c, a.d, a.e, a.f, 0.0, 0.0, 1.0]
}

#[test]
fn spot_heights_of_the_sheet() {
    // Trig beacons and spot heights printed on the Grootkloof sheet (map pixels, height).
    let spots =
        [(1645.0, 2152.0, 1518.0), (1888.0, 2352.0, 1526.0), (1584.0, 2411.0, 1556.0), (1717.0, 2566.0, 1582.0)];
    let a = truth();
    let pts: Vec<[f64; 2]> = spots
        .iter()
        .map(|&(x, y, _)| {
            let (mx, my) = a.apply(x, y);
            let (lat, lon) = merc_to_latlon(mx, my);
            [lat, lon]
        })
        .collect();
    let store = grootkloof_store();
    let v = resolve_missing(&store, || {
        json_of(store.elevations(&parse_request(&json!({ "points": pts }).to_string()).unwrap()))
    });
    assert_eq!(v["zoom"], 13);
    for (s, e) in spots.iter().zip(v["elevations_m"].as_array().unwrap()) {
        let e = e.as_f64().unwrap();
        assert!((e - s.2).abs() < 20.0, "spot height {} read as {e:.1}", s.2);
    }
}

#[test]
fn map_space_lines_follow_the_homography_and_the_warp() {
    // A map turned 20° at 2 m per pixel, its paper bent by up to 25 m (the warp), over an east–west
    // ramp: every line pixel must sit where H(p) + B(p) reaches a contour level.
    let store = ramp_store(0.05, -2000.0);
    let (cx, cy) = centre();
    let (c, s) = (20f64.to_radians().cos(), 20f64.to_radians().sin());
    let h = [2.0 * c, 2.0 * s, cx - 1000.0, 2.0 * s, -2.0 * c, cy + 1000.0, 0.0, 0.0, 1.0];
    let mut warp = Warp::zero(1000, 1000);
    for (k, o) in warp.offsets_m.iter_mut().enumerate() {
        *o = 25.0 * ((k as f64) * 0.61).sin();
    }
    let n = 400usize;
    let req = json!({"space": "map", "rect": [0, 0, 1000, 1000], "pixel_to_mercator": h,
                     "warp": serde_json::to_value(&warp).unwrap(), "style": "dark", "interval_m": 20.0,
                     "line_scale": 2.0, "background": [0, 0, 0, 0], "labels": false});
    let (rgba, v) = render(&store, &req, n, n);
    assert_eq!(v["status"], "ok", "{v}");
    let elev = |x: f64, y: f64| {
        let mx = h[0] * x + h[1] * y + h[2] + warp.offset(x, y).0;
        0.05 * (mx - cx + 2000.0)
    };
    let (mut on, mut checked) = (0, 0);
    for j in (0..n).step_by(7) {
        for i in 0..n {
            let (x, y) = ((i as f64 + 0.5) * 2.5, (j as f64 + 0.5) * 2.5);
            let e = elev(x, y);
            // Metres per buffer pixel of the ramp here, from the neighbours.
            let g = (elev(x + 2.5, y) - elev(x - 2.5, y)).hypot(elev(x, y + 2.5) - elev(x, y - 2.5)) / 2.0;
            let off_px = ((e / 20.0).round() * 20.0 - e).abs() / g;
            let alpha = rgba[(j * n + i) * 4 + 3];
            if off_px < 0.25 {
                checked += 1;
                on += (alpha > 120) as usize;
            } else if off_px > 2.0 {
                assert_eq!(alpha, 0, "({i}, {j}) is {off_px:.1} px from any contour");
            }
        }
    }
    assert!(checked > 50 && on == checked, "{on} of {checked} contour crossings drawn");
}

#[test]
fn the_time_budgets_hold() {
    // Adding tiles: a PNG decode each.
    let pngs = dem_tiles();
    let t = Instant::now();
    let store = TerrainStore::new(256 << 20);
    for (id, png) in &pngs {
        store.add_png(*id, png).unwrap();
    }
    let add_ms = t.elapsed().as_secs_f64() * 1e3 / pngs.len() as f64;
    let h = truth_h();
    let best = |f: &mut dyn FnMut()| {
        (0..5)
            .map(|_| {
                let t = Instant::now();
                f();
                t.elapsed().as_secs_f64() * 1e3
            })
            .fold(f64::INFINITY, f64::min)
    };
    // A 512 px tile of the terrain beyond a sheet: map space, hillshade, labels.
    let req: RenderRequest = parse_request(
        &json!({"space": "map", "rect": [1000, 1500, 2024, 2524], "pixel_to_mercator": h, "style": "dark",
                "scale_m_per_pt": 2.4, "line_scale": 2.0, "hillshade": 0.25})
        .to_string(),
    )
    .unwrap();
    let mut rgba = vec![0u8; 512 * 512 * 4];
    assert_eq!(store.render(&req, &mut rgba, 512, 512, 2048).unwrap().status, Status::Ok);
    let ms = best(&mut || {
        store.render(&req, &mut rgba, 512, 512, 2048).unwrap();
    });
    // And a zoomed-out Mercator tile: a new DEM cell for nearly every pixel.
    let (x0, y0) = latlon_to_merc(-25.90, 27.33);
    let (x1, y1) = latlon_to_merc(-25.78, 27.47);
    let mreq: RenderRequest = parse_request(
        &json!({"space": "mercator", "rect": [x0, y0, x1, y1], "style": "dark", "line_scale": 2.0, "hillshade": 0.25})
            .to_string(),
    )
    .unwrap();
    let mms = best(&mut || {
        store.render(&mreq, &mut rgba, 512, 512, 2048).unwrap();
    });
    // A profile of ~1000 samples along a 9 km line.
    let a = truth();
    let ll = |x: f64, y: f64| {
        let (mx, my) = a.apply(x, y);
        let (lat, lon) = merc_to_latlon(mx, my);
        [lat, lon]
    };
    let preq: ProfileRequest =
        parse_request(&json!({ "points": [ll(300.0, 300.0), ll(1800.0, 2500.0), ll(3200.0, 4700.0)] }).to_string())
            .unwrap();
    assert_eq!(store.profile(&preq).unwrap().status, Status::Ok);
    let pms = best(&mut || {
        store.profile(&preq).unwrap();
    });
    // A 384² grid for the 3D view.
    let (gx0, gy0) = a.apply(0.0, 4960.0);
    let (gx1, gy1) = a.apply(3507.0, 0.0);
    let greq: GridRequest =
        parse_request(&json!({ "rect": [gx0, gy0, gx1, gy1], "max_zoom": 12 }).to_string()).unwrap();
    let mut heights = vec![0f32; 384 * 384];
    assert_eq!(store.grid(&greq, &mut heights, 384, 384).unwrap().status, Status::Ok);
    let gms = best(&mut || {
        store.grid(&greq, &mut heights, 384, 384).unwrap();
    });
    eprintln!(
        "add a tile {add_ms:.2} ms; render 512² map {ms:.1} ms, mercator {mms:.1} ms; profile {pms:.2} ms; grid 384² \
         {gms:.1} ms"
    );
    assert!(add_ms < time_budget_ms(5.0), "add_png {add_ms:.2} ms");
    assert!(ms < time_budget_ms(30.0), "map render {ms:.1} ms");
    assert!(mms < time_budget_ms(30.0), "mercator render {mms:.1} ms");
    assert!(pms < time_budget_ms(20.0), "profile {pms:.1} ms");
    assert!(gms < time_budget_ms(30.0), "grid {gms:.1} ms");
}

// ---------------------------------------------------------------------------------------------
// Tile plans
// ---------------------------------------------------------------------------------------------

fn plan(req: Value) -> PlanResponse {
    tile_plan(&parse_request(&req.to_string()).unwrap()).unwrap()
}

/// The tiles of a zoom's ranges, checking they are disjoint.
fn plan_tiles(z: &PlanZoom) -> BTreeSet<(u32, u32)> {
    let mut set = BTreeSet::new();
    for r in &z.ranges {
        for y in r[2]..=r[3] {
            for x in r[0]..=r[1] {
                assert!(set.insert((x, y)), "zoom {}: tile {x}/{y} in two ranges", z.z);
            }
        }
    }
    assert_eq!(set.len(), z.tiles);
    set
}

#[test]
fn box_plans_match_tiles_covering() {
    let (s, w, n, e) = (-30.7, 27.0, -28.6, 29.5); // Lesotho
    let p = plan(json!({"areas": [{"bbox": [s, w, n, e], "zooms": [8, 9, 10, 11, 12]}]}));
    let (x0, y0) = latlon_to_merc(s, w);
    let (x1, y1) = latlon_to_merc(n, e);
    let b = MercBox { min_x: x0, min_y: y0, max_x: x1, max_y: y1 };
    assert_eq!(p.zooms.len(), 5);
    for z in &p.zooms {
        let want: BTreeSet<(u32, u32)> = tiles_covering(z.z, &b).into_iter().map(|t| (t.x, t.y)).collect();
        assert_eq!(plan_tiles(z), want, "zoom {}", z.z);
        assert_eq!(z.ranges.len(), 1, "a box is one rectangle");
        assert_eq!(z.bytes, (z.tiles as f64 * tile_bytes_typical(z.z)).round());
        assert_eq!(z.bytes_high, (z.tiles as f64 * tile_bytes_high(z.z)).round());
    }
    assert_eq!(p.tiles, p.zooms.iter().map(|z| z.tiles).sum::<usize>());
    assert_eq!(p.tiles, 1150, "docs/design/explorer.md: Lesotho, Standard, 1 150 tiles");
    // Overlapping areas count once.
    let twice = plan(
        json!({"areas": [{"bbox": [s, w, n, e], "zooms": [10]}, {"bbox": [s + 0.5, w + 0.5, n, e], "zooms": [10]}]}),
    );
    assert_eq!(twice.tiles, p.zooms.iter().find(|z| z.z == 10).unwrap().tiles);
}

#[test]
fn plans_across_the_antimeridian() {
    let p = plan(json!({"areas": [{"bbox": [-18.0, 179.0, -16.0, -179.0], "zooms": [8]}]}));
    let set = plan_tiles(&p.zooms[0]);
    let xs: BTreeSet<u32> = set.iter().map(|t| t.0).collect();
    assert_eq!(xs, BTreeSet::from([0, 255]));
    // A circle on the antimeridian takes both sides too.
    let c = plan(json!({"areas": [{"circle": [-17.0, 180.0, 30000.0], "zooms": [10]}]}));
    let xs: BTreeSet<u32> = plan_tiles(&c.zooms[0]).iter().map(|t| t.0).collect();
    assert!(xs.contains(&0) && xs.contains(&1023), "{xs:?}");
}

#[test]
fn circle_plans_take_exactly_the_tiles_the_circle_reaches() {
    let (lat, lon, r): (f64, f64, f64) = (-29.3, 29.4, 20_000.0);
    let p = plan(json!({"areas": [{"circle": [lat, lon, r], "zooms": [12]}]}));
    let got = plan_tiles(&p.zooms[0]);
    // Brute force over the circle's box: the nearest point of each tile to the centre.
    let n = 4096.0;
    let x_of = |lon: f64| ((lon + 180.0) / 360.0 * n) as u32;
    let lat_of = |y: f64| merc_to_latlon(0.0, std::f64::consts::PI * 6_378_137.0 * (1.0 - 2.0 * y / n)).0;
    let dlat = r / 111_000.0 * 1.2;
    let dlon = dlat / lat.to_radians().cos();
    let (_, ytop) = latlon_to_merc(lat + dlat, 0.0);
    let (_, ybot) = latlon_to_merc(lat - dlat, 0.0);
    let ty =
        |my: f64| ((std::f64::consts::PI * 6_378_137.0 - my) / (2.0 * std::f64::consts::PI * 6_378_137.0) * n) as u32;
    let mut uncertain = 0;
    for y in ty(ytop)..=ty(ybot) {
        for x in x_of(lon - dlon)..=x_of(lon + dlon) {
            let (lo_w, lo_e) = (x as f64 / n * 360.0 - 180.0, (x + 1) as f64 / n * 360.0 - 180.0);
            let (la_n, la_s) = (lat_of(y as f64), lat_of(y as f64 + 1.0));
            let near = LatLon::new(lat.clamp(la_s, la_n), lon.clamp(lo_w, lo_e));
            let d = haversine_m(LatLon::new(lat, lon), near);
            if (d - r).abs() < 50.0 {
                uncertain += 1;
                continue;
            }
            assert_eq!(got.contains(&(x, y)), d < r, "tile {x}/{y} at {d:.0} m");
        }
    }
    assert!(uncertain < 4, "{uncertain}");
    assert!(got.len() > 20 && got.len() < 40, "{}", got.len());
}

/// The Grootkloof sheet's corners (TL, TR, BR, BL), from its true georeference.
fn sheet_corners() -> Vec<[f64; 2]> {
    let a = truth();
    [(0.0, 0.0), (3507.0, 0.0), (3507.0, 4960.0), (0.0, 4960.0)]
        .iter()
        .map(|&(x, y)| {
            let (mx, my) = a.apply(x, y);
            let (lat, lon) = merc_to_latlon(mx, my);
            [lat, lon]
        })
        .collect()
}

#[test]
fn trailhead_plan_around_a_sheet() {
    let corners = sheet_corners();
    let p = plan(json!({"trailhead": {"corners": corners}}));
    assert!(p.dropped.is_empty(), "{:?}", p.dropped);
    let zs: Vec<u8> = p.zooms.iter().map(|z| z.z).collect();
    assert_eq!(zs, vec![9, 10, 11, 12, 13]);
    let mb = p.bytes / 1e6;
    eprintln!(
        "trailhead of a 1:25 000 sheet: {} tiles, {mb:.1} MB ({:.1} MB high): {:?}",
        p.tiles,
        p.bytes_high / 1e6,
        p.zooms.iter().map(|z| (z.z, z.tiles)).collect::<Vec<_>>()
    );
    assert!((2.0..15.0).contains(&mb), "{mb:.1} MB");
    // Zoom 13 covers the sheet and 3 km around it; zoom 12 the continuation margin (4.7 km here).
    let z13 = plan_tiles(&p.zooms[4]);
    let z12 = plan_tiles(&p.zooms[3]);
    let k = LAT.to_radians().cos();
    for (zoom, set, margin) in [(13u8, &z13, 3000.0), (12, &z12, continuation_margin_m(9450.0))] {
        for c in &corners {
            let (mx, my) = latlon_to_merc(c[0], c[1]);
            for (dx, dy) in [(-1.0, -1.0), (1.0, 1.0), (-1.0, 1.0), (1.0, -1.0)] {
                let (px, py) = (mx + dx * margin * 0.99 / k / 2f64.sqrt(), my + dy * margin * 0.99 / k / 2f64.sqrt());
                let t = tiles_covering(zoom, &MercBox::around(px, py, 0.0))[0];
                if (px - mx).abs() < 1.0 {
                    continue;
                }
                let outward = {
                    let (cx, cy) = sheet_centre();
                    (px - cx).abs() > (mx - cx).abs() && (py - cy).abs() > (my - cy).abs()
                };
                if outward {
                    assert!(set.contains(&(t.x, t.y)), "zoom {zoom}: {t:?} near corner {c:?} left out");
                }
            }
        }
    }
    // A 1:250 000 sheet (≈ 110 × 75 km): zoom 13 over it would take thousands of tiles.
    let big = plan(json!({"trailhead": {"corners": [[-25.0, 27.0], [-25.0, 28.1], [-25.7, 28.1], [-25.7, 27.0]]}}));
    let d13 = big.dropped.iter().find(|d| d.z == 13).expect("zoom 13 dropped");
    assert!(d13.tiles > TRAILHEAD_MAX_TILES_PER_ZOOM && d13.reason.contains("400"), "{d13:?}");
    assert!(big.zooms.iter().all(|z| z.tiles <= TRAILHEAD_MAX_TILES_PER_ZOOM));
}

fn sheet_centre() -> (f64, f64) {
    let pts: Vec<(f64, f64)> = sheet_corners().iter().map(|c| latlon_to_merc(c[0], c[1])).collect();
    (pts.iter().map(|p| p.0).sum::<f64>() / 4.0, pts.iter().map(|p| p.1).sum::<f64>() / 4.0)
}

#[test]
fn quad_plans_grow_by_their_margin() {
    let corners = sheet_corners();
    let tight = plan(json!({"areas": [{"quad": corners, "margin_m": 0.0, "zooms": [13]}]}));
    let wide = plan(json!({"areas": [{"quad": corners, "margin_m": 5000.0, "zooms": [13]}]}));
    let (t, w) = (plan_tiles(&tight.zooms[0]), plan_tiles(&wide.zooms[0]));
    assert!(t.is_subset(&w) && w.len() > t.len());
    for c in &corners {
        let (mx, my) = latlon_to_merc(c[0], c[1]);
        let id = tiles_covering(13, &MercBox::around(mx, my, 0.0))[0];
        assert!(t.contains(&(id.x, id.y)), "corner tile {id:?}");
    }
    // Every tile of the wide plan is within the margin (plus the corner squares' diagonal) of the sheet.
    let (cx, cy) = sheet_centre();
    let half_diag = 6500.0 / LAT.to_radians().cos();
    for &(x, y) in &w {
        let b = tile_bounds(TileId { z: 13, x, y });
        let d = ((b.min_x + b.max_x) / 2.0 - cx).hypot((b.min_y + b.max_y) / 2.0 - cy);
        assert!(d < half_diag + 5000.0 * 1.5 / LAT.to_radians().cos() + 4000.0, "{x}/{y} at {d:.0}");
    }
}

#[test]
fn plan_requests_are_checked() {
    for req in [
        json!({"areas": [{"zooms": [8]}]}),
        json!({"areas": [{"bbox": [0, 0, 1, 1], "circle": [0, 0, 5], "zooms": [8]}]}),
        json!({"areas": [{"bbox": [1, 0, 0, 1], "zooms": [8]}]}),
        json!({"areas": [{"bbox": [0, 0, 1, 1], "zooms": [16]}]}),
        json!({"areas": [{"circle": [0, 0, -5], "zooms": [8]}]}),
        json!({"areas": [{"quad": [[0, 0], [0, 1], [1, 1], [1, 0]], "margin_m": -1, "zooms": [8]}]}),
    ] {
        assert!(tile_plan(&parse_request(&req.to_string()).unwrap()).is_err(), "{req}");
    }
    let empty = plan(json!({}));
    assert_eq!((empty.tiles, empty.zooms.len()), (0, 0));
}

#[test]
fn a_country_plan_is_quick() {
    // South Africa, zooms 8–13: ~166 000 tiles.
    let req: PlanRequest = parse_request(
        &json!({"areas": [{"bbox": [-35.0, 16.4, -22.1, 33.0], "zooms": [8, 9, 10, 11, 12, 13]}]}).to_string(),
    )
    .unwrap();
    let t = Instant::now();
    let p = tile_plan(&req).unwrap();
    let ms = t.elapsed().as_secs_f64() * 1e3;
    assert!(p.tiles > 150_000, "{}", p.tiles);
    assert!(ms < time_budget_ms(5.0), "{ms:.2} ms");
}
