//! MEASUREMENTS behind the wide search (`docs/ARCHITECTURE.md`, "Wide
//! search"): real Terrarium tiles from the network, cached on disk
//! (`MAPME_DEM_CACHE`, default `$TMPDIR/mapme-demcache`). All `#[ignore]`;
//! run one at a time:
//!
//! ```text
//! MAPME_DEM_CACHE=/path/to/cache cargo test --release --test wide_measure -- --ignored --nocapture --test-threads=1
//! ```

mod common;
use common::*;
use mapme_core::contour::auto_mask;
use mapme_core::dem::{tiles_covering, DemStore, MercBox, TileId};
use mapme_core::geo::{latlon_to_merc, merc_to_latlon};
use mapme_core::search::{coarse_search, map_field, Candidate, MapField, SearchParams};
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Mutex, OnceLock};
use std::time::Instant;

fn cache_dir() -> PathBuf {
    std::env::var_os("MAPME_DEM_CACHE")
        .map(PathBuf::from)
        .unwrap_or_else(|| std::env::temp_dir().join("mapme-demcache"))
}

/// One Terrarium tile (PNG bytes), from the cache or the network (curl).
fn fetch(t: TileId) -> Option<Vec<u8>> {
    let p = cache_dir().join(format!("{}/{}/{}.png", t.z, t.x, t.y));
    if let Ok(b) = std::fs::read(&p) {
        return Some(b);
    }
    std::fs::create_dir_all(p.parent().unwrap()).ok()?;
    let url = format!("https://s3.amazonaws.com/elevation-tiles-prod/terrarium/{}/{}/{}.png", t.z, t.x, t.y);
    let tmp = p.with_extension(format!("part{}", std::process::id()));
    for _ in 0..3 {
        let ok = std::process::Command::new("curl")
            .args(["-sSf", "--max-time", "60", "-o"])
            .arg(&tmp)
            .arg(&url)
            .status()
            .map(|s| s.success())
            .unwrap_or(false);
        if ok && std::fs::rename(&tmp, &p).is_ok() {
            return std::fs::read(&p).ok();
        }
    }
    None
}

/// Fetch tiles on 16 threads; (tiles with bytes, seconds, bytes).
fn fetch_all(tiles: &[TileId]) -> (Vec<(TileId, Vec<u8>)>, f64, usize) {
    let t = Instant::now();
    let next = AtomicUsize::new(0);
    let out = Mutex::new(Vec::new());
    std::thread::scope(|s| {
        for _ in 0..16 {
            s.spawn(|| loop {
                let i = next.fetch_add(1, Ordering::Relaxed);
                if i >= tiles.len() {
                    break;
                }
                if let Some(b) = fetch(tiles[i]) {
                    out.lock().unwrap().push((tiles[i], b));
                }
            });
        }
    });
    let v = out.into_inner().unwrap();
    let bytes = v.iter().map(|(_, b)| b.len()).sum();
    (v, t.elapsed().as_secs_f64(), bytes)
}

/// Peak resident memory of this process (MB) since the last reset.
fn peak_rss_mb() -> f64 {
    let s = std::fs::read_to_string("/proc/self/status").unwrap_or_default();
    s.lines()
        .find(|l| l.starts_with("VmHWM:"))
        .and_then(|l| l.split_whitespace().nth(1)?.parse::<f64>().ok())
        .map_or(f64::NAN, |kb| kb / 1024.0)
}

fn reset_peak_rss() {
    let _ = std::fs::write("/proc/self/clear_refs", "5");
}

struct Map {
    w: usize,
    h: usize,
    field: MapField,
    /// Truth map centre (Mercator) and ground scale (m/px).
    centre: (f64, f64),
    scale: f64,
}

fn grootkloof() -> &'static Map {
    static M: OnceLock<Map> = OnceLock::new();
    M.get_or_init(|| {
        let img = load_map();
        let (mask, _) = auto_mask(&img);
        let field = map_field(&mask);
        let t = truth();
        let centre = t.apply(img.w as f64 / 2.0, img.h as f64 / 2.0);
        let (lat, _) = merc_to_latlon(centre.0, centre.1);
        Map { w: img.w, h: img.h, field, centre, scale: t.mean_scale() * lat.to_radians().cos() }
    })
}

fn ground_km(a: (f64, f64), b: (f64, f64)) -> f64 {
    let (la, _) = merc_to_latlon(0.0, 0.5 * (a.1 + b.1));
    (a.0 - b.0).hypot(a.1 - b.1) * la.to_radians().cos() / 1000.0
}

const PLACES: [(&str, f64, f64); 6] = [
    ("map", -25.8499, 27.4000),
    ("Rustenburg", -25.67, 27.24),
    ("Pretoria", -25.75, 28.19),
    ("Polokwane", -23.90, 29.45),
    ("Bloemfontein", -29.12, 26.21),
    ("Durban", -29.86, 31.03),
];

/// Average Terrarium tile size per zoom over 1 000 km around the map
/// (a deterministic sample of tiles, land and the sea off Durban).
#[test]
#[ignore]
fn tile_weights() {
    let c = latlon_to_merc(-26.5, 28.5);
    for z in [8u8, 9, 10, 11, 12, 13] {
        let b = MercBox::around(c.0, c.1, 1_150_000.0);
        let all = tiles_covering(z, &b);
        let step = (all.len() / 60).max(1);
        let sample: Vec<TileId> = all.iter().copied().step_by(step).take(60).collect();
        let (got, secs, bytes) = fetch_all(&sample);
        let mut sizes: Vec<usize> = got.iter().map(|(_, b)| b.len()).collect();
        sizes.sort();
        eprintln!(
            "[tiles] z{z}: {} sampled, mean {:.1} KB, median {:.1} KB, p10 {:.1} KB, p90 {:.1} KB ({:.1} s)",
            got.len(),
            bytes as f64 / got.len().max(1) as f64 / 1024.0,
            sizes[sizes.len() / 2] as f64 / 1024.0,
            sizes[sizes.len() / 10] as f64 / 1024.0,
            sizes[sizes.len() * 9 / 10] as f64 / 1024.0,
            secs
        );
        // Decode time.
        let t = Instant::now();
        let mut s = DemStore::new();
        for (id, b) in &got {
            s.add_png(*id, b).unwrap();
        }
        eprintln!("         decode {:.2} ms/tile", t.elapsed().as_secs_f64() * 1e3 / got.len().max(1) as f64);
    }
}

/// Outcome of one direct (un-chunked) coarse window.
fn window(z: u8, gps: (f64, f64), radius_m: f64, scale: (f64, f64), top_k: usize) {
    let m = grootkloof();
    let c = latlon_to_merc(gps.0, gps.1);
    let k = 1.0 / gps.0.to_radians().cos();
    let half = (radius_m + m.w.max(m.h) as f64 * scale.1 / 2.0 + 1000.0) * k;
    let b = MercBox::around(c.0, c.1, half);
    let tiles = tiles_covering(z, &b);
    let (got, dl_s, bytes) = fetch_all(&tiles);
    reset_peak_rss();
    let rss0 = peak_rss_mb();
    let t = Instant::now();
    let mut s = DemStore::new();
    for (id, png) in &got {
        s.add_png(*id, png).unwrap();
    }
    let dem = s.mosaic(z, &b);
    drop(s);
    let t_mosaic = t.elapsed().as_secs_f64();
    let params = SearchParams {
        center_merc: c,
        search_radius_m: radius_m,
        scale_min: scale.0,
        scale_max: scale.1,
        rotations_deg: vec![0.0],
        top_k,
        region: None,
    };
    let t = Instant::now();
    let cands = coarse_search(&m.field, &dem, m.w, m.h, &params, &|| false, &|_| {});
    let t_search = t.elapsed().as_secs_f64();
    let rss = peak_rss_mb();
    let truth_km = ground_km(c, m.centre);
    let rank = cands.iter().position(|cd| is_truth(cd, m));
    let line = |cd: &Candidate| {
        let p = cd.center_merc(m.w, m.h);
        format!(
            "score {:.3} ratio {:.2} scale {:.2} off {:.1} km",
            cd.score,
            cd.peak_ratio,
            cd.scale_m_per_px,
            ground_km(p, m.centre)
        )
    };
    eprintln!(
        "[window] z{z} gps {:.2},{:.2} (truth {:.0} km) R {:.0} km scale {:.2}-{:.2}: {} tiles {:.1} MB dl {:.1} s | \
         mosaic {}x{} {:.2} s search {:.2} s | peak RSS {:.0} MB (+{:.0}) | truth rank {:?}",
        gps.0,
        gps.1,
        truth_km,
        radius_m / 1000.0,
        scale.0,
        scale.1,
        tiles.len(),
        bytes as f64 / 1e6,
        dl_s,
        dem.w(),
        dem.h(),
        t_mosaic,
        t_search,
        rss,
        rss - rss0,
        rank
    );
    for (i, cd) in cands.iter().take(6).enumerate() {
        eprintln!("    #{i} {}{}", line(cd), if Some(i) == rank { "  <= TRUTH" } else { "" });
    }
    if let Some(r) = rank.filter(|&r| r >= 6) {
        eprintln!("    #{r} {}  <= TRUTH", line(&cands[r]));
    }
}

fn is_truth(c: &Candidate, m: &Map) -> bool {
    ground_km(c.center_merc(m.w, m.h), m.centre) < 2.0 && (c.scale_m_per_px / m.scale - 1.0).abs() < 0.1
}

/// The scale prior from "1:25 000" at 300 dpi (as `georef.rs` derives it).
fn prior_range(lat: f64) -> (f64, f64) {
    let s = 25_000.0 * 0.0254 / 300.0;
    (0.75 * s * lat.to_radians().cos(), 1.25 * s)
}

/// Coarse windows of growing radius around places at growing distance, at
/// zoom 11, 10 and 9, with the default scale range and with the scale prior.
#[test]
#[ignore]
fn coarse_windows() {
    let _ = grootkloof();
    let which = std::env::var("WIDE_ZOOMS").unwrap_or_else(|_| "11,10,9".into());
    let zooms: Vec<u8> = which.split(',').filter_map(|s| s.trim().parse().ok()).collect();
    let places: Vec<&str> = std::env::var("WIDE_PLACES")
        .map(|s| s.split(',').map(|p| Box::leak(p.trim().to_string().into_boxed_str()) as &str).collect())
        .unwrap_or_else(|_| PLACES.iter().map(|p| p.0).collect());
    for &z in &zooms {
        // Largest window per zoom (~4 500 px a side).
        let max_r = match z {
            11 => 150_000.0,
            10 => 300_000.0,
            _ => 650_000.0,
        };
        for (name, lat, lon) in PLACES.iter().filter(|p| places.contains(&p.0)) {
            let m = grootkloof();
            let d = ground_km(latlon_to_merc(*lat, *lon), m.centre) * 1000.0;
            let r = (d + 30_000.0).max(50_000.0);
            if r > max_r {
                eprintln!("[window] z{z} {name}: R {:.0} km too large for a direct window", r / 1000.0);
                continue;
            }
            eprintln!("--- z{z} {name} ---");
            window(z, (*lat, *lon), r, (0.4, 12.0), 20);
            window(z, (*lat, *lon), r, prior_range(*lat), 20);
        }
    }
}

// ---------------------------------------------------------------------------
// Candidate verification: coarse (z10 / z9) → z11 re-check → z13 refinement
// ---------------------------------------------------------------------------

use mapme_core::contour::normalize_illumination;
use mapme_core::geo::Affine;
use mapme_core::georef::ContourProducts;
use mapme_core::image::Rgb8;
use mapme_core::refine::{fit_confidence, refine_dem_box, refine_prepared_with, PreparedDem, RefineOptions};

/// A map to measure with: its products and where it truly is.
struct Case {
    name: String,
    w: usize,
    h: usize,
    prod: ContourProducts,
    centre: (f64, f64),
    scale: f64,
    photo: bool,
}

fn jpeg(path: &std::path::Path) -> Rgb8 {
    let bytes = std::fs::read(path).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
    let mut dec = jpeg_decoder::Decoder::new(std::io::Cursor::new(bytes));
    let px = dec.decode().expect("decode jpeg");
    let info = dec.info().unwrap();
    Rgb8::from_rgb(info.width as usize, info.height as usize, px).unwrap()
}

fn case_from(name: &str, img: &Rgb8, photo: bool, truth: &Affine) -> Case {
    let src = if photo { normalize_illumination(img) } else { img.clone() };
    let prod = ContourProducts::compute_with(&src, None, photo);
    let centre = truth.apply(img.w as f64 / 2.0, img.h as f64 / 2.0);
    let (lat, _) = merc_to_latlon(centre.0, centre.1);
    Case {
        name: name.into(),
        w: img.w,
        h: img.h,
        prod,
        centre,
        scale: truth.mean_scale() * lat.to_radians().cos(),
        photo,
    }
}

/// Cases: the scan, a synthetic photo of it (2.5 % corner error) and, when
/// `WIDE_SHEETS` names a directory holding `castle-1.jpg` / `castle_grid.json`
/// and `mhlabatini-1.jpg` / `mhlabatini_grid.json`, the two new sheets.
fn cases() -> Vec<Case> {
    let want = std::env::var("WIDE_CASES").unwrap_or_else(|_| "scan,photo,castle,mhlabatini".into());
    let mut out = Vec::new();
    if want.contains("scan") {
        out.push(case_from("grootkloof scan", &load_map(), false, &truth()));
    }
    if want.contains("photo") {
        let map = load_map();
        let cam = photo_camera(&map);
        let photo = synth_photo(&map, &cam, 7);
        let pc = photo_case(&map, &photo, &cam, 0.025);
        let t = pc.truth();
        // The flattened photo's truth near its centre (local affine).
        let a = t.local_affine(pc.image.w as f64 / 2.0, pc.image.h as f64 / 2.0);
        out.push(case_from("grootkloof synthetic photo", &pc.image, true, &a));
    }
    if want.contains("realphoto") {
        // The real phone photo, flattened by its page corners; its truth is
        // the contour fit next to the map (fixture DEM), as a local affine.
        let photo = jpeg(&fixture_dir().join("photo/printed-map-photo.jpg"));
        let tj: serde_json::Value =
            serde_json::from_slice(&std::fs::read(fixture_dir().join("photo/truth.json")).unwrap()).unwrap();
        let q: Vec<(f64, f64)> = tj["page_quad"]
            .as_array()
            .unwrap()
            .iter()
            .map(|p| (p[0].as_f64().unwrap(), p[1].as_f64().unwrap()))
            .collect();
        let (img, _) = rectify(&photo, &[q[0], q[1], q[2], q[3]]);
        let s = mapme_core::georef::Session::new(img.clone());
        for (t, png) in dem_tiles() {
            s.add_dem_tile(t, &png).unwrap();
        }
        let req: mapme_core::georef::GeorefRequest = serde_json::from_value(serde_json::json!({
            "gps": {"lat": CENTER.0, "lon": CENTER.1}, "photo": true, "methods": ["contours"]
        }))
        .unwrap();
        let r = s.georeference(&req);
        let h = mapme_core::geo::Homography { m: r.pixel_to_mercator.expect("real photo located near the map") };
        let a = h.local_affine(img.w as f64 / 2.0, img.h as f64 / 2.0);
        out.push(case_from("grootkloof real photo", &img, true, &a));
    }
    if let Some(dir) = std::env::var_os("WIDE_SHEETS").map(PathBuf::from) {
        for (key, jpg, grid) in
            [("castle", "castle-1.jpg", "castle_grid.json"), ("mhlabatini", "mhlabatini-1.jpg", "mhlabatini_grid.json")]
        {
            if !want.contains(key) || !dir.join(jpg).exists() {
                continue;
            }
            let g: serde_json::Value = serde_json::from_slice(&std::fs::read(dir.join(grid)).unwrap()).unwrap();
            let m: Vec<f64> = g["pixel_to_mercator"].as_array().unwrap().iter().map(|v| v.as_f64().unwrap()).collect();
            let a = Affine { a: m[0], b: m[1], c: m[2], d: m[3], e: m[4], f: m[5] };
            out.push(case_from(key, &jpeg(&dir.join(jpg)), false, &a));
        }
    }
    out
}

fn store_of(z: u8, b: &MercBox) -> (DemStore, usize, f64) {
    let tiles = tiles_covering(z, b);
    let (got, secs, _) = fetch_all(&tiles);
    let mut s = DemStore::new();
    for (id, png) in &got {
        s.add_png(*id, png).unwrap();
    }
    (s, tiles.len(), secs)
}

/// Re-check a coarse candidate with a small zoom-11 search around it.
fn recheck_z11(c: &Case, cand: &Candidate, radius_m: f64) -> (Option<Candidate>, usize, f64) {
    let centre = cand.center_merc(c.w, c.h);
    let params = SearchParams {
        center_merc: centre,
        search_radius_m: radius_m,
        scale_min: cand.scale_m_per_px / 1.1,
        scale_max: cand.scale_m_per_px * 1.1,
        rotations_deg: vec![0.0],
        top_k: 2,
        region: None,
    };
    let b = mapme_core::search::required_dem_box(&params, c.w, c.h);
    let (s, n, _) = store_of(11, &b);
    let dem = s.mosaic(11, &b);
    let t = Instant::now();
    let r = coarse_search(&c.prod.field, &dem, c.w, c.h, &params, &|| false, &|_| {});
    (r.into_iter().next(), n, t.elapsed().as_secs_f64())
}

fn refine_z13(c: &Case, t: &Affine) -> (f64, usize, f64) {
    let b = refine_dem_box(t, c.w, c.h);
    let (s, n, _) = store_of(13, &b);
    let dem = s.mosaic(13, &b);
    let tm = Instant::now();
    let prep = PreparedDem::new_cropped(&dem, &b);
    let r = refine_prepared_with(&c.prod.samples, &prep, t, &RefineOptions { photo: c.photo });
    (fit_confidence(&r), n, tm.elapsed().as_secs_f64())
}

fn km(c: &Case, cand: &Candidate) -> f64 {
    ground_km(cand.center_merc(c.w, c.h), c.centre)
}

/// Wide windows at zoom 10 / 9: the top coarse candidates by NCC, each
/// re-checked at zoom 11 (4 km around it, scale ±10 %) and the best few
/// refined at zoom 13. Truth vs false candidates.
#[test]
#[ignore]
fn wide_candidates() {
    let windows: Vec<(u8, &str, f64)> = match std::env::var("WIDE_WINDOWS") {
        Ok(s) => s
            .split(';')
            .filter_map(|w| {
                let p: Vec<&str> = w.split(',').collect();
                Some((p[0].parse().ok()?, Box::leak(p[1].to_string().into_boxed_str()) as &str, p[2].parse().ok()?))
            })
            .collect(),
        Err(_) => vec![(10, "Pretoria", 110_000.0), (9, "Bloemfontein", 413_000.0), (9, "Durban", 602_000.0)],
    };
    for c in cases() {
        eprintln!(
            "===== {} ({}x{}, {} segments, truth scale {:.2}) =====",
            c.name, c.w, c.h, c.prod.samples.n_segs, c.scale
        );
        for &(z, place, r) in &windows {
            let (_, lat, lon) = *PLACES.iter().find(|p| p.0 == place).unwrap();
            let gc = latlon_to_merc(lat, lon);
            let k = 1.0 / lat.to_radians().cos();
            let half = (r + c.w.max(c.h) as f64 * 12.0 / 2.0 + 1000.0) * k;
            let b = MercBox::around(gc.0, gc.1, half);
            let (s, n, dl) = store_of(z, &b);
            let dem = s.mosaic(z, &b);
            drop(s);
            let params = SearchParams {
                center_merc: gc,
                search_radius_m: r,
                scale_min: 0.4,
                scale_max: 12.0,
                rotations_deg: vec![0.0],
                top_k: 40,
                region: None,
            };
            let t = Instant::now();
            let mut cands = coarse_search(&c.prod.field, &dem, c.w, c.h, &params, &|| false, &|_| {});
            let ts = t.elapsed().as_secs_f64();
            drop(dem);
            let zrank = cands.iter().position(|cd| km(&c, cd) < 2.0);
            cands.sort_by(|a, b| b.score.total_cmp(&a.score));
            let nrank = cands.iter().position(|cd| km(&c, cd) < 2.0);
            eprintln!(
                "--- z{z} {place} R {:.0} km: {n} tiles (dl {dl:.1} s), search {ts:.1} s; truth rank by z {zrank:?}, by \
                 NCC {nrank:?}",
                r / 1000.0
            );
            let mut rows = Vec::new();
            for (i, cd) in cands.iter().enumerate().take(10) {
                let (re, n11, t11) = recheck_z11(&c, cd, 4000.0);
                rows.push((i, cd.clone(), re, n11, t11));
            }
            if let Some(nr) = nrank.filter(|&r| r >= 10) {
                let (re, n11, t11) = recheck_z11(&c, &cands[nr], 4000.0);
                rows.push((nr, cands[nr].clone(), re, n11, t11));
            }
            // Refine the five best by z11 NCC (and the truth).
            let mut order: Vec<usize> = (0..rows.len()).collect();
            order.sort_by(|&a, &b| {
                let s = |i: usize| rows[i].2.as_ref().map_or(-1.0, |c| c.score);
                s(b).total_cmp(&s(a))
            });
            for (j, &ri) in order.iter().enumerate() {
                let (i, cd, re, n11, t11) = &rows[ri];
                let truth = km(&c, cd) < 2.0;
                let fit = if j < 5 || truth { re.as_ref().map(|x| refine_z13(&c, &x.transform)) } else { None };
                eprintln!(
                    "  #{i:2} coarse ncc {:.3} ratio {:.2} scale {:.2} off {:7.1} km | z11 ncc {} ratio {} ({n11} tiles, \
                     {:.2} s) | z13 fit {}{}",
                    cd.score,
                    cd.peak_ratio,
                    cd.scale_m_per_px,
                    km(&c, cd),
                    re.as_ref().map_or("-".into(), |x| format!("{:.3}", x.score)),
                    re.as_ref().map_or("-".into(), |x| format!("{:.2}", x.peak_ratio)),
                    t11,
                    fit.map_or("-".into(), |(f, n13, t13)| format!("{f:.3} ({n13} tiles, {t13:.2} s)")),
                    if truth { "  <= TRUTH" } else { "" }
                );
            }
        }
    }
}

// ---------------------------------------------------------------------------
// The implemented search end to end (Session API, network tiles)
// ---------------------------------------------------------------------------

/// The app's loop against the network (cached): every requested tile is
/// fetched or marked missing. Returns the responses, core seconds, fetch
/// seconds, tiles by zoom and bytes.
fn network_loop(
    s: &mapme_core::georef::Session,
    req: &serde_json::Value,
) -> (Vec<serde_json::Value>, f64, f64, std::collections::BTreeMap<u8, usize>, usize) {
    let mut rs = Vec::new();
    let (mut core_s, mut fetch_s, mut bytes) = (0.0, 0.0, 0usize);
    let mut by_zoom = std::collections::BTreeMap::new();
    for _ in 0..400 {
        let t = Instant::now();
        let r: serde_json::Value = serde_json::from_str(&s.georeference_json(&req.to_string())).unwrap();
        core_s += t.elapsed().as_secs_f64();
        let need = r["status"] == "need_dem";
        if need {
            let tiles: Vec<TileId> = r["need_tiles"]
                .as_array()
                .unwrap()
                .iter()
                .map(|t| TileId {
                    z: t["z"].as_u64().unwrap() as u8,
                    x: t["x"].as_u64().unwrap() as u32,
                    y: t["y"].as_u64().unwrap() as u32,
                })
                .collect();
            let (got, secs, b) = fetch_all(&tiles);
            fetch_s += secs;
            bytes += b;
            let have: std::collections::HashSet<TileId> = got.iter().map(|g| g.0).collect();
            // `WIDE_TILES_DIR`: keep a copy of every tile served (fixture naming).
            let keep = std::env::var_os("WIDE_TILES_DIR").map(PathBuf::from);
            for (t, png) in &got {
                let _ = s.add_dem_tile(*t, png);
                *by_zoom.entry(t.z).or_insert(0) += 1;
                if let Some(dir) = &keep {
                    let _ = std::fs::create_dir_all(dir);
                    let _ = std::fs::write(dir.join(format!("{}_{}_{}.png", t.z, t.x, t.y)), png);
                }
            }
            for t in tiles.iter().filter(|t| !have.contains(t)) {
                let _ = s.mark_dem_tile_missing(*t);
            }
        }
        rs.push(r);
        if !need {
            break;
        }
    }
    (rs, core_s, fetch_s, by_zoom, bytes)
}

/// The confidence of each area searched (hint and near areas' best
/// refinement, wide candidates refined), for the false-positive margin.
fn area_confidences(r: &serde_json::Value) -> String {
    let mut out: Vec<String> = Vec::new();
    for a in r["diagnostics"]["search_areas"].as_array().into_iter().flatten() {
        out.push(format!(
            "{} {} km: {} ({})",
            a["area"].as_str().unwrap_or("?"),
            a["radius_km"],
            a["selected"]["confidence"],
            a["outcome"].as_str().unwrap_or("?")
        ));
    }
    for v in r["diagnostics"]["wide"]["refined"].as_array().into_iter().flatten() {
        out.push(format!("wide chunk {}: {} (ratio {})", v["chunk"], v["confidence"], v["recheck_ratio"]));
    }
    out.join("; ")
}

/// Wide searches through the session as the app runs them: the Grootkloof
/// scan (with and without its OCR'd "1:25 000"), from places at growing
/// distance, and a 1 000 km search that cannot succeed (the sheet is
/// ~1 060 km from Windhoek): rounds, tiles, megabytes, core time, peak RSS.
#[test]
#[ignore]
fn wide_session() {
    use serde_json::json;
    let img = load_map();
    let ocr = ocr_fixture();
    // The labels would locate the sheet by its grid: keep only the scale.
    let scale_only: Vec<_> = ocr.iter().filter(|o| o.text.contains("1:25")).cloned().collect();
    let runs: Vec<(&str, f64, f64, f64, bool)> = match std::env::var("WIDE_RUNS") {
        Ok(s) => s
            .split(';')
            .filter_map(|r| {
                let p: Vec<&str> = r.split(',').collect();
                Some((
                    Box::leak(p[0].to_string().into_boxed_str()) as &str,
                    p[1].parse().ok()?,
                    p[2].parse().ok()?,
                    p[3].parse().ok()?,
                    p.get(4).is_some_and(|v| *v == "prior"),
                ))
            })
            .collect(),
        Err(_) => vec![
            ("Pretoria", -25.75, 28.19, 120_000.0, true),
            ("Polokwane", -23.90, 29.45, 350_000.0, true),
            ("Bloemfontein", -29.12, 26.21, 450_000.0, true),
            ("Durban", -29.86, 31.03, 650_000.0, true),
            ("Durban", -29.86, 31.03, 650_000.0, false),
            ("Windhoek", -22.56, 17.08, 1_000_000.0, true),
        ],
    };
    for (name, lat, lon, radius, prior) in runs {
        let s = mapme_core::georef::Session::new(img.clone());
        let mut req = json!({"gps": {"lat": lat, "lon": lon}, "dpi": 300.0, "methods": ["contours"],
                             "search_radius_m": radius});
        if prior {
            req["ocr"] = serde_json::to_value(&scale_only).unwrap();
        }
        reset_peak_rss();
        let t = Instant::now();
        let (rs, core_s, fetch_s, by_zoom, bytes) = network_loop(&s, &req);
        let r = rs.last().unwrap();
        let err = r["pixel_to_mercator"].as_array().map(|m| {
            let m: Vec<f64> = m.iter().map(|v| v.as_f64().unwrap()).collect();
            max_error_m(&mapme_core::geo::Homography { m: m.try_into().unwrap() })
        });
        let est: serde_json::Value = serde_json::from_str(&mapme_core::wide::search_estimate_json(
            &json!({"radius_m": radius, "gps": {"lat": lat, "lon": lon}}).to_string(),
        ))
        .unwrap();
        eprintln!(
            "[wide_session] {name} R {:.0} km {}: {} in {} rounds, {:.1} s ({:.1} s core, {:.1} s tiles), tiles {:?} \
             ({:.1} MB), peak RSS {:.0} MB, err {}\n    message: {}\n    search: {}\n    wide: {}\n    timings (last \
             call): {}\n    estimate: {}\n    areas: {}",
            radius / 1000.0,
            if prior { "1:25 000" } else { "no scale" },
            r["status"],
            rs.len(),
            t.elapsed().as_secs_f64(),
            core_s,
            fetch_s,
            by_zoom,
            bytes as f64 / 1e6,
            peak_rss_mb(),
            err.map_or("-".into(), |e| format!("{e:.1} m")),
            r["message"],
            r["search"],
            r["diagnostics"]["wide"],
            r["timings_ms"],
            est,
            area_confidences(r)
        );
    }
}

/// `mm_search_estimate` for a range of radii around the Grootkloof sheet.
#[test]
#[ignore]
fn estimate_table() {
    // `ESTIMATE_AT=lat,lon` for another place (default Pretoria).
    let at: Vec<f64> = std::env::var("ESTIMATE_AT")
        .unwrap_or_else(|_| "-25.75,28.19".into())
        .split(',')
        .filter_map(|v| v.trim().parse().ok())
        .collect();
    for known in [false, true] {
        for r in [20_000.0, 50_000.0, 100_000.0, 250_000.0, 300_000.0, 500_000.0, 1_000_000.0, 2_000_000.0] {
            let v: serde_json::Value = serde_json::from_str(&mapme_core::wide::search_estimate_json(
                &serde_json::json!({"radius_m": r, "gps": {"lat": at[0], "lon": at[1]}, "scale_known": known})
                    .to_string(),
            ))
            .unwrap();
            eprintln!(
                "[estimate] R {:>5.0} km scale {}: z{} {:>5} tiles {:>6.1} MB {:>3} rounds {:>2} chunks ~{:>4} s ({} s \
                 compute + {} s download) {}",
                r / 1000.0,
                if known { "known  " } else { "unknown" },
                v["coarse_zoom"],
                v["tiles"],
                v["approx_megabytes"].as_f64().unwrap(),
                v["rounds"],
                v["chunks"],
                v["approx_seconds"],
                v["approx_compute_seconds"],
                v["approx_download_seconds"],
                v["tiles_by_zoom"]
            );
        }
    }
}

/// The real phone photo of the sheet (flattened by its page corners, photo
/// mode, no OCR) searched from far away through the session: the hardest
/// case for the wide acceptance (photo refinement fits false candidates up
/// to 0.635). `WIDE_PHOTO_RUNS` as for `wide_session`.
#[test]
#[ignore]
fn wide_session_real_photo() {
    use serde_json::json;
    let photo = jpeg(&fixture_dir().join("photo/printed-map-photo.jpg"));
    let tj: serde_json::Value =
        serde_json::from_slice(&std::fs::read(fixture_dir().join("photo/truth.json")).unwrap()).unwrap();
    let q: Vec<(f64, f64)> =
        tj["page_quad"].as_array().unwrap().iter().map(|p| (p[0].as_f64().unwrap(), p[1].as_f64().unwrap())).collect();
    let (img, _) = rectify(&photo, &[q[0], q[1], q[2], q[3]]);
    let runs: Vec<(&str, f64, f64, f64)> = match std::env::var("WIDE_PHOTO_RUNS") {
        Ok(s) => s
            .split(';')
            .filter_map(|r| {
                let p: Vec<&str> = r.split(',').collect();
                Some((
                    Box::leak(p[0].to_string().into_boxed_str()) as &str,
                    p[1].parse().ok()?,
                    p[2].parse().ok()?,
                    p[3].parse().ok()?,
                ))
            })
            .collect(),
        Err(_) => vec![("Bloemfontein", -29.12, 26.21, 450_000.0), ("Durban", -29.86, 31.03, 650_000.0)],
    };
    for (name, lat, lon, radius) in runs {
        let s = mapme_core::georef::Session::new(img.clone());
        let req = json!({"gps": {"lat": lat, "lon": lon}, "photo": true, "methods": ["contours"],
                         "search_radius_m": radius});
        reset_peak_rss();
        let t = Instant::now();
        let (rs, core_s, fetch_s, by_zoom, bytes) = network_loop(&s, &req);
        let r = rs.last().unwrap();
        if let Some(dir) = std::env::var_os("WIDE_DUMP") {
            let p = PathBuf::from(dir).join(format!("photo-{name}-{:.0}.json", radius / 1000.0));
            std::fs::write(&p, serde_json::to_string_pretty(r).unwrap()).unwrap();
        }
        // Distance of the map centre from the sheet's centre.
        let off = r["pixel_to_mercator"].as_array().map(|m| {
            let m: Vec<f64> = m.iter().map(|v| v.as_f64().unwrap()).collect();
            let h = mapme_core::geo::Homography { m: m.try_into().unwrap() };
            let (x, y) = h.apply(img.w as f64 / 2.0, img.h as f64 / 2.0);
            ground_km((x, y), latlon_to_merc(CENTER.0, CENTER.1)) * 1000.0
        });
        eprintln!(
            "[wide_session photo] {name} R {:.0} km: {} conf {} in {} rounds, {:.1} s ({:.1} s core, {:.1} s tiles), \
             tiles {:?} ({:.1} MB), peak RSS {:.0} MB, centre {} from the sheet's\n    message: {}\n    search: {}\n    \
             wide: {}\n    areas: {}",
            radius / 1000.0,
            r["status"],
            r["confidence"],
            rs.len(),
            t.elapsed().as_secs_f64(),
            core_s,
            fetch_s,
            by_zoom,
            bytes as f64 / 1e6,
            peak_rss_mb(),
            off.map_or("-".into(), |o| format!("{o:.0} m")),
            r["message"],
            r["search"],
            r["diagnostics"]["wide"],
            area_confidences(r),
        );
    }
}

/// Process CPU seconds (user + system) so far.
fn cpu_seconds() -> f64 {
    let s = std::fs::read_to_string("/proc/self/stat").unwrap_or_default();
    let rest = s.rfind(')').map_or("", |i| &s[i + 2..]);
    let f: Vec<&str> = rest.split_whitespace().collect();
    // utime and stime: fields 14 and 15 of the line (11 and 12 after the state).
    let tick = |i: usize| f.get(i).and_then(|v| v.parse::<f64>().ok());
    tick(11).zip(tick(12)).map_or(f64::NAN, |(u, s)| (u + s) / 100.0)
}

/// A JPEG or PNG file as packed RGB.
fn load_image_file(path: &std::path::Path) -> Rgb8 {
    if path.extension().is_some_and(|e| e.eq_ignore_ascii_case("png")) {
        let bytes = std::fs::read(path).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
        let mut r = png::Decoder::new(std::io::Cursor::new(bytes)).read_info().expect("png");
        let mut buf = vec![0; r.output_buffer_size()];
        let info = r.next_frame(&mut buf).expect("png frame");
        let ch = info.color_type.samples();
        let px: Vec<u8> =
            buf[..info.buffer_size()].chunks(ch).flat_map(|p| [p[0], p[1.min(ch - 1)], p[2.min(ch - 1)]]).collect();
        Rgb8::from_rgb(info.width as usize, info.height as usize, px).unwrap()
    } else {
        jpeg(path)
    }
}

/// `mm_search_estimate` against searches the session runs to the end (the
/// map is not within the radius, so every area and chunk of the plan is
/// searched): `ESTIMATE_RUNS` = `label|image|ocr.json or -|lat,lon|radius km[|dpi]`
/// separated by `;`. Prints, and appends to `ESTIMATE_OUT` as JSON lines,
/// the rounds, tiles by zoom, megabytes, core (wall) and CPU seconds of the
/// search next to the estimate for the same image size and scale prior.
#[test]
#[ignore]
fn estimate_check() {
    use serde_json::json;
    let runs = std::env::var("ESTIMATE_RUNS").expect("ESTIMATE_RUNS");
    for run in runs.split(';').filter(|r| !r.trim().is_empty()) {
        let p: Vec<&str> = run.split('|').map(str::trim).collect();
        let (label, image, ocr, at, radius_km) = (p[0], p[1], p[2], p[3], p[4].parse::<f64>().unwrap());
        let ll: Vec<f64> = at.split(',').map(|v| v.trim().parse().unwrap()).collect();
        let img = load_image_file(std::path::Path::new(image));
        let mut req = json!({"gps": {"lat": ll[0], "lon": ll[1]}, "methods": ["contours"],
                             "search_radius_m": radius_km * 1000.0});
        if ocr != "-" {
            let v: serde_json::Value = serde_json::from_slice(&std::fs::read(ocr).unwrap()).unwrap();
            req["ocr"] = if v.is_array() { v } else { v["observations"].clone() };
        }
        if let Some(dpi) = p.get(5).and_then(|d| d.parse::<f64>().ok()) {
            req["dpi"] = json!(dpi);
        }
        let s = mapme_core::georef::Session::new(img.clone());
        let cpu0 = cpu_seconds();
        let t = Instant::now();
        let (rs, core_s, fetch_s, by_zoom, bytes) = network_loop(&s, &req);
        let cpu = cpu_seconds() - cpu0;
        let r = rs.last().unwrap();
        let sr = &r["diagnostics"]["scale_range"];
        let mut ereq = json!({"radius_m": radius_km * 1000.0, "gps": {"lat": ll[0], "lon": ll[1]},
                              "image_size": [img.w, img.h]});
        match sr["source"].as_str() {
            Some("scale_label") => ereq["scale_m_per_px"] = json!([sr["min"], sr["max"]]),
            Some("scale_bar") => ereq["scale_bar_m_per_px"] = json!([sr["min"], sr["max"]]),
            _ => {}
        }
        let est: serde_json::Value =
            serde_json::from_str(&mapme_core::wide::search_estimate_json(&ereq.to_string())).unwrap();
        let tiles: usize = by_zoom.values().sum();
        // Stage times over every call, and the refinements made.
        let mut stages: std::collections::BTreeMap<String, f64> = std::collections::BTreeMap::new();
        for x in &rs {
            for (k, v) in x["timings_ms"].as_object().into_iter().flatten() {
                *stages.entry(k.clone()).or_insert(0.0) += v.as_f64().unwrap_or(0.0);
            }
        }
        let refined: u64 = r["diagnostics"]["search_areas"]
            .as_array()
            .into_iter()
            .flatten()
            .map(|a| a["selected"]["refined"].as_u64().unwrap_or(0))
            .sum::<u64>()
            + r["diagnostics"]["wide"]["refined"].as_array().map_or(0, |v| v.len() as u64);
        let line = json!({
            "stages_ms": stages, "refined": refined,
            "label": label, "image": [img.w, img.h], "radius_km": radius_km, "status": r["status"],
            "scale_source": sr["source"], "rounds": rs.len(), "tiles": tiles, "tiles_by_zoom": by_zoom,
            "mb": bytes as f64 / 1e6, "core_s": core_s, "cpu_s": cpu, "fetch_s": fetch_s,
            "wall_s": t.elapsed().as_secs_f64(), "search": r["search"], "estimate": est, "estimate_request": ereq,
            "areas": r["diagnostics"]["search_areas"].as_array().map_or(0, |a| a.len()),
            "wide": r["diagnostics"]["wide"], "timings_ms": r["timings_ms"], "message": r["message"],
        });
        eprintln!(
            "[estimate_check] {label} {}x{} R {radius_km} km ({}): {} in {} rounds, {tiles} tiles {:.1} MB, core \
             {core_s:.1} s (cpu {cpu:.1} s) | estimate: {} tiles {} MB {} rounds, {} s compute (iPhone) + {} s download",
            img.w,
            img.h,
            sr["source"],
            r["status"],
            rs.len(),
            bytes as f64 / 1e6,
            est["tiles"],
            est["approx_megabytes"],
            est["rounds"],
            est["approx_compute_seconds"],
            est["approx_download_seconds"]
        );
        if let Some(out) = std::env::var_os("ESTIMATE_OUT") {
            use std::io::Write;
            let mut f = std::fs::OpenOptions::new().create(true).append(true).open(out).unwrap();
            writeln!(f, "{line}").unwrap();
        }
    }
}
