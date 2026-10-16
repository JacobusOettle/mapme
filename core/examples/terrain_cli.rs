//! Developer tool: the terrain engine (`mm_terrain_*`, `mm_tile_plan`) from the command line, for
//! looking at what the app will draw and measure.
//!
//! ```text
//! cargo run --release --example terrain_cli -- render --dem-dir DIR
//!     (--merc min_x,min_y,max_x,max_y | --latlon s,w,n,e
//!      | --map x0,y0,x1,y1 (--case case.json | --h m0,…,m8 [--warp warp.json]))
//!     [--size WxH] [--style dark|paper] [--hillshade S] [--scale M_PER_PT] [--line-scale S]
//!     [--interval M] [--no-labels] [--transparent] [--tiles N] [--over map.jpg] [--max-zoom Z]
//!     --out out.png
//! cargo run --release --example terrain_cli -- profile --dem-dir DIR --points "lat,lon;lat,lon;…"
//!     [--max-zoom Z] [--out chart.png]
//! cargo run --release --example terrain_cli -- grid --dem-dir DIR --merc … [--size WxH] [--out heights.png]
//! cargo run --release --example terrain_cli -- plan '{"areas":[…],"trailhead":{…}}'
//! ```
//!
//! Runs the app's `need_dem` loop: tiles come from `<dem-dir>/{z}_{x}_{y}.png` (Terrarium PNGs,
//! as fixtures/grootkloof/dem holds them) and are marked missing when absent. Prints the final
//! answer's JSON (profile samples shortened) and the query's time (renders: the best of 10).
//!
//! `render --map` draws in a map's pixel space: `--case` takes the pixel → Mercator affine from a
//! fixture's `truth_points` (fixtures/grootkloof/case.json); `--over map.jpg` then draws the
//! rendered lines in red over the map image in grey, to see that the DEM's contours fall on the
//! printed ones. `--tiles N` renders the rectangle as N × N tiles too and reports how far the
//! stitched tiles differ from the whole (they should not).

use mapme_core::dem::TileId;
use mapme_core::geo::{latlon_to_merc, Affine};
use mapme_core::terrain::*;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::Instant;

const USAGE: &str = "usage: terrain_cli render|profile|grid|plan … (see the source's header)";

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("terrain_cli: {e}\n{USAGE}");
            ExitCode::FAILURE
        }
    }
}

struct Args {
    rest: Vec<String>,
}

impl Args {
    fn take(&mut self, flag: &str) -> Option<String> {
        let i = self.rest.iter().position(|a| a == flag)?;
        self.rest.remove(i);
        (i < self.rest.len()).then(|| self.rest.remove(i))
    }

    fn flag(&mut self, flag: &str) -> bool {
        match self.rest.iter().position(|a| a == flag) {
            Some(i) => {
                self.rest.remove(i);
                true
            }
            None => false,
        }
    }
}

fn numbers(s: &str, n: usize, what: &str) -> Result<Vec<f64>, String> {
    let v: Vec<f64> =
        s.split(',').map(|x| x.trim().parse::<f64>()).collect::<Result<_, _>>().map_err(|e| format!("{what}: {e}"))?;
    if v.len() != n {
        return Err(format!("{what}: expected {n} numbers"));
    }
    Ok(v)
}

fn size(s: Option<String>, default: (usize, usize)) -> Result<(usize, usize), String> {
    let Some(s) = s else { return Ok(default) };
    let (w, h) = s.split_once('x').ok_or("--size WxH")?;
    Ok((w.parse().map_err(|e| format!("--size: {e}"))?, h.parse().map_err(|e| format!("--size: {e}"))?))
}

fn run() -> Result<(), String> {
    let mut args = Args { rest: std::env::args().skip(1).collect() };
    if args.rest.is_empty() {
        return Err("no command".into());
    }
    let cmd = args.rest.remove(0);
    if cmd == "plan" {
        let json = args.rest.first().ok_or("plan JSON")?;
        let t = Instant::now();
        let out = tile_plan_json(json);
        eprintln!("{:.2} ms", t.elapsed().as_secs_f64() * 1e3);
        println!("{out}");
        return Ok(());
    }
    let dem_dir = PathBuf::from(args.take("--dem-dir").ok_or("--dem-dir")?);
    let store = TerrainStore::new(256 << 20);
    match cmd.as_str() {
        "render" => render(&mut args, &store, &dem_dir),
        "profile" => profile(&mut args, &store, &dem_dir),
        "grid" => grid(&mut args, &store, &dem_dir),
        other => Err(format!("unknown command {other}")),
    }
}

/// Run `query` until it is complete, feeding it tiles from `dir` (absent ones marked missing).
fn resolve(store: &TerrainStore, dir: &Path, mut query: impl FnMut() -> serde_json::Value) -> serde_json::Value {
    for round in 0..30 {
        let v = query();
        let need: Vec<TileId> = serde_json::from_value(v["need_tiles"].clone()).unwrap_or_default();
        if v["status"] != "need_dem" || need.is_empty() {
            return v;
        }
        let (mut added, mut missing) = (0, 0);
        for t in need {
            match std::fs::read(dir.join(format!("{}_{}_{}.png", t.z, t.x, t.y))) {
                Ok(png) if store.add_png(t, &png).is_ok() => added += 1,
                _ => {
                    let _ = store.mark_missing(t);
                    missing += 1;
                }
            }
        }
        eprintln!("round {round}: {added} tiles added, {missing} missing");
    }
    query()
}

fn render(args: &mut Args, store: &TerrainStore, dir: &Path) -> Result<(), String> {
    let (w, h) = size(args.take("--size"), (512, 512))?;
    let out = PathBuf::from(args.take("--out").ok_or("--out")?);
    let mut req = serde_json::json!({
        "style": args.take("--style").unwrap_or_else(|| "dark".into()),
        "hillshade": args.take("--hillshade").map_or(Ok(0.0), |s| s.parse::<f64>()).map_err(|e| format!("--hillshade: {e}"))?,
        "line_scale": args.take("--line-scale").map_or(Ok(1.0), |s| s.parse::<f64>()).map_err(|e| format!("--line-scale: {e}"))?,
        "labels": !args.flag("--no-labels"),
    });
    if let Some(s) = args.take("--scale") {
        req["scale_m_per_pt"] = s.parse::<f64>().map_err(|e| format!("--scale: {e}"))?.into();
    }
    if let Some(s) = args.take("--interval") {
        req["interval_m"] = s.parse::<f64>().map_err(|e| format!("--interval: {e}"))?.into();
    }
    if let Some(s) = args.take("--max-zoom") {
        req["max_zoom"] = s.parse::<u8>().map_err(|e| format!("--max-zoom: {e}"))?.into();
    }
    if args.flag("--transparent") {
        req["background"] = serde_json::json!([0, 0, 0, 0]);
    }
    let over = args.take("--over");
    if let Some(r) = args.take("--merc") {
        req["space"] = "mercator".into();
        req["rect"] = numbers(&r, 4, "--merc")?.into();
    } else if let Some(r) = args.take("--latlon") {
        let v = numbers(&r, 4, "--latlon")?;
        let (x0, y0) = latlon_to_merc(v[0], v[1]);
        let (x1, y1) = latlon_to_merc(v[2], v[3]);
        req["space"] = "mercator".into();
        req["rect"] = serde_json::json!([x0, y0, x1, y1]);
    } else if let Some(r) = args.take("--map") {
        req["space"] = "map".into();
        req["rect"] = numbers(&r, 4, "--map")?.into();
        let m: Vec<f64> = if let Some(case) = args.take("--case") {
            let a = case_affine(Path::new(&case))?;
            vec![a.a, a.b, a.c, a.d, a.e, a.f, 0.0, 0.0, 1.0]
        } else {
            numbers(&args.take("--h").ok_or("--case or --h")?, 9, "--h")?
        };
        req["pixel_to_mercator"] = m.into();
        if let Some(wp) = args.take("--warp") {
            let text = std::fs::read_to_string(&wp).map_err(|e| format!("{wp}: {e}"))?;
            req["warp"] = serde_json::from_str(&text).map_err(|e| format!("{wp}: {e}"))?;
        }
    } else {
        return Err("--merc, --latlon or --map".into());
    }
    let tiles_n: usize = args.take("--tiles").map_or(Ok(1), |s| s.parse()).map_err(|e| format!("--tiles: {e}"))?;
    if !args.rest.is_empty() {
        return Err(format!("unknown arguments {:?}", args.rest));
    }
    let parsed: RenderRequest = parse_request(&req.to_string())?;
    let mut rgba = vec![0u8; w * h * 4];
    let v = resolve(store, dir, || value(store.render(&parsed, &mut rgba, w, h, w * 4)));
    // The fastest of 10 (the box may be busy with other work).
    let mut best = f64::INFINITY;
    for _ in 0..10 {
        let t = Instant::now();
        let _ = store.render(&parsed, &mut rgba, w, h, w * 4);
        best = best.min(t.elapsed().as_secs_f64() * 1e3);
    }
    eprintln!("render {w}x{h}: {best:.2} ms");
    println!("{v}");

    if tiles_n > 1 {
        let (tw, th) = (w / tiles_n, h / tiles_n);
        let r = parsed.rect;
        let mut diff = 0u32;
        let mut differing = 0usize;
        for ty in 0..tiles_n {
            for tx in 0..tiles_n {
                let mut sub = parsed.clone();
                let fx = |i: usize| r[0] + (r[2] - r[0]) * i as f64 / w as f64;
                sub.rect = match parsed.space {
                    RenderSpace::Map => {
                        let fy = |j: usize| r[1] + (r[3] - r[1]) * j as f64 / h as f64;
                        [fx(tx * tw), fy(ty * th), fx(tx * tw + tw), fy(ty * th + th)]
                    }
                    RenderSpace::Mercator => {
                        let fy = |j: usize| r[3] - (r[3] - r[1]) * j as f64 / h as f64;
                        [fx(tx * tw), fy(ty * th + th), fx(tx * tw + tw), fy(ty * th)]
                    }
                };
                let mut buf = vec![0u8; tw * th * 4];
                let _ = store.render(&sub, &mut buf, tw, th, tw * 4);
                for j in 0..th {
                    for i in 0..tw {
                        for c in 0..4 {
                            let a = buf[(j * tw + i) * 4 + c];
                            let b = rgba[((ty * th + j) * w + tx * tw + i) * 4 + c];
                            let d = a.abs_diff(b) as u32;
                            diff = diff.max(d);
                            differing += (d > 0) as usize;
                        }
                    }
                }
            }
        }
        eprintln!(
            "{tiles_n}×{tiles_n} tiles against the whole: max difference {diff}, {differing} channel values differ"
        );
    }

    if let Some(map) = over {
        // The map in grey, the rendered lines in red on it (render with a transparent background).
        let img = load_jpeg(Path::new(&map))?;
        let mut req2 = parsed.clone();
        req2.background = Some([0, 0, 0, 0]);
        req2.hillshade = 0.0;
        let mut lines = vec![0u8; w * h * 4];
        let _ = store.render(&req2, &mut lines, w, h, w * 4);
        let r = parsed.rect;
        for j in 0..h {
            for i in 0..w {
                let x = (r[0] + (i as f64 + 0.5) * (r[2] - r[0]) / w as f64) as isize;
                let y = (r[1] + (j as f64 + 0.5) * (r[3] - r[1]) / h as f64) as isize;
                let grey = if x >= 0 && y >= 0 && (x as usize) < img.0 && (y as usize) < img.1 {
                    let p = &img.2[(y as usize * img.0 + x as usize) * 3..][..3];
                    (p[0] as f64 * 0.3 + p[1] as f64 * 0.59 + p[2] as f64 * 0.11) * 0.8 + 40.0
                } else {
                    40.0
                };
                let a = lines[(j * w + i) * 4 + 3] as f64 / 255.0;
                let o = &mut rgba[(j * w + i) * 4..][..4];
                o[0] = (grey * (1.0 - a) + 255.0 * a) as u8;
                o[1] = (grey * (1.0 - a)) as u8;
                o[2] = (grey * (1.0 - a)) as u8;
                o[3] = 255;
            }
        }
    }
    write_png(&out, w, h, &unpremultiply(&rgba))
}

fn profile(args: &mut Args, store: &TerrainStore, dir: &Path) -> Result<(), String> {
    let pts: Vec<[f64; 2]> = args
        .take("--points")
        .ok_or("--points")?
        .split(';')
        .map(|p| numbers(p, 2, "--points").map(|v| [v[0], v[1]]))
        .collect::<Result<_, _>>()?;
    let mut req = serde_json::json!({ "points": pts });
    if let Some(s) = args.take("--max-zoom") {
        req["max_zoom"] = s.parse::<u8>().map_err(|e| format!("--max-zoom: {e}"))?.into();
    }
    let out = args.take("--out");
    let parsed: ProfileRequest = parse_request(&req.to_string())?;
    let v = resolve(store, dir, || value(store.profile(&parsed)));
    let t = Instant::now();
    let _ = store.profile(&parsed);
    eprintln!("profile: {:.2} ms", t.elapsed().as_secs_f64() * 1e3);
    let mut answer = v;
    let samples: Vec<(f64, Option<f64>)> = serde_json::from_value(answer["samples"].clone()).unwrap_or_default();
    if let Some(arr) = answer["samples"].as_array_mut() {
        let n = arr.len();
        if n > 8 {
            arr.drain(4..n - 4);
        }
    }
    println!("{answer}");
    if let Some(out) = out {
        let (w, h) = (900usize, 300usize);
        let mut img = vec![0u8; w * h * 4];
        for p in img.chunks_exact_mut(4) {
            p.copy_from_slice(&[24, 24, 28, 255]);
        }
        let es: Vec<f64> = samples.iter().filter_map(|s| s.1).collect();
        let (lo, hi) = es.iter().fold((f64::INFINITY, f64::NEG_INFINITY), |a, &e| (a.0.min(e), a.1.max(e)));
        let dist = samples.last().map_or(1.0, |s| s.0).max(1.0);
        for s in &samples {
            if let Some(e) = s.1 {
                let x = ((s.0 / dist) * (w - 1) as f64) as usize;
                let y = ((1.0 - (e - lo) / (hi - lo).max(1.0)) * (h - 20) as f64) as usize + 10;
                for yy in y..h {
                    let p = &mut img[(yy * w + x) * 4..][..4];
                    p.copy_from_slice(if yy < y + 3 { &[255, 120, 140, 255] } else { &[80, 60, 70, 255] });
                }
            }
        }
        write_png(Path::new(&out), w, h, &img)?;
    }
    Ok(())
}

fn grid(args: &mut Args, store: &TerrainStore, dir: &Path) -> Result<(), String> {
    let (w, h) = size(args.take("--size"), (384, 384))?;
    let rect = numbers(&args.take("--merc").ok_or("--merc")?, 4, "--merc")?;
    let out = args.take("--out");
    let parsed: GridRequest = parse_request(&serde_json::json!({ "rect": rect }).to_string())?;
    let mut heights = vec![0f32; w * h];
    let v = resolve(store, dir, || value(store.grid(&parsed, &mut heights, w, h)));
    let t = Instant::now();
    let _ = store.grid(&parsed, &mut heights, w, h);
    eprintln!("grid {w}x{h}: {:.2} ms", t.elapsed().as_secs_f64() * 1e3);
    println!("{v}");
    if let Some(out) = out {
        let (lo, hi) = heights
            .iter()
            .filter(|h| h.is_finite())
            .fold((f32::INFINITY, f32::NEG_INFINITY), |a, &e| (a.0.min(e), a.1.max(e)));
        let mut img = vec![0u8; w * h * 4];
        for (p, &e) in img.chunks_exact_mut(4).zip(&heights) {
            let g = if e.is_finite() { ((e - lo) / (hi - lo).max(1.0) * 255.0) as u8 } else { 0 };
            p.copy_from_slice(&[g, g, if e.is_finite() { g } else { 128 }, 255]);
        }
        write_png(Path::new(&out), w, h, &img)?;
    }
    Ok(())
}

/// Pixel → Mercator affine fitted to a fixture's `truth_points`.
fn case_affine(path: &Path) -> Result<Affine, String> {
    let v: serde_json::Value =
        serde_json::from_slice(&std::fs::read(path).map_err(|e| format!("{}: {e}", path.display()))?)
            .map_err(|e| format!("{}: {e}", path.display()))?;
    let pts = v["truth_points"].as_array().ok_or("case has no truth_points")?;
    let (mut src, mut dst) = (Vec::new(), Vec::new());
    for p in pts {
        let f = |k: &str| p[k].as_f64().ok_or(format!("truth point without {k}"));
        src.push((f("x")?, f("y")?));
        dst.push(latlon_to_merc(f("lat")?, f("lon")?));
    }
    Affine::fit(&src, &dst).ok_or_else(|| "could not fit the truth points".into())
}

fn load_jpeg(path: &Path) -> Result<(usize, usize, Vec<u8>), String> {
    let bytes = std::fs::read(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let mut dec = jpeg_decoder::Decoder::new(std::io::Cursor::new(bytes));
    let px = dec.decode().map_err(|e| e.to_string())?;
    let info = dec.info().ok_or("no JPEG info")?;
    if info.pixel_format != jpeg_decoder::PixelFormat::RGB24 {
        return Err("only RGB JPEGs".into());
    }
    Ok((info.width as usize, info.height as usize, px))
}

/// A query's answer as the app sees it (the JSON the C ABI returns).
fn value<T: serde::Serialize>(answer: Result<T, String>) -> serde_json::Value {
    serde_json::from_str(&answer_json(answer)).unwrap_or_default()
}

/// Premultiplied RGBA (the core's) → straight alpha (PNG's).
fn unpremultiply(rgba: &[u8]) -> Vec<u8> {
    let mut out = rgba.to_vec();
    for p in out.chunks_exact_mut(4) {
        let a = p[3] as u32;
        if a > 0 && a < 255 {
            for c in &mut p[..3] {
                *c = ((*c as u32 * 255 + a / 2) / a).min(255) as u8;
            }
        }
    }
    out
}

fn write_png(path: &Path, w: usize, h: usize, rgba: &[u8]) -> Result<(), String> {
    let file = std::fs::File::create(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let mut enc = png::Encoder::new(std::io::BufWriter::new(file), w as u32, h as u32);
    enc.set_color(png::ColorType::Rgba);
    enc.set_depth(png::BitDepth::Eight);
    let mut writer = enc.write_header().map_err(|e| e.to_string())?;
    writer.write_image_data(rgba).map_err(|e| e.to_string())?;
    eprintln!("wrote {}", path.display());
    Ok(())
}
