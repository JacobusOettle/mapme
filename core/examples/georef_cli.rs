//! Developer tool: georeference a map image from the command line.
//!
//! ```text
//! cargo run --release --example georef_cli -- <image.jpg|png> [--gps lat,lon] [--ocr ocr.json]
//!     [--dem-dir dir] [--dpi N] [--methods grid,contours] [--radius-m M] [--contour-rgb r,g,b]
//!     [--rotate] [--verify] [--photo] [--hint lat,lon[,radius_m][,name]]... [--max-tiles N]
//!     [--dump-mask out.png] [--missing-out tiles.txt] [--fit-check a,b,c,d,e,f] [--map-area]
//! ```
//!
//! Runs the same request / `need_dem` loop as the app: tiles are read from
//! `<dem-dir>/{z}_{x}_{y}.png` (Terrarium PNGs) and marked missing when
//! absent. Prints the final response JSON on stdout and a per-round log on
//! stderr. `--ocr` takes a JSON array of observations or an object with an
//! `observations` array (fixtures/grootkloof/ocr.json).
//!
//! `--dump-mask` writes the contour extraction of the request as a grey PNG of
//! the image's size: 255 at the skeleton samples the session matches (computed
//! by `ContourProducts::compute_in` on the map's rectangle, as the session does), 128 elsewhere on
//! the colour mask (the same choice of model, recomputed here), 0 off it.
//! `--missing-out` appends every tile the core asked for that is not in
//! `--dem-dir` (`z x y` per line) to a file, so a benchmark can fetch them.
//!
//! `--map-area` prints the map's content area on the image (`content::map_content`:
//! the rectangle the contour path works on) and exits.
//!
//! `--fit-check a,b,c,d,e,f` (a pixel → Mercator affine, e.g. a case's
//! truth) georeferences nothing: it prints the fit of that transform to the
//! contours on the fine DEM the session would use (the refinement's metrics
//! and confidence factors), then the refinement from it and from it shifted
//! by 300 m, 1 km and 2.8 km, with how far each moved the image corners.

use mapme_core::content::{map_content, MapRect};
use mapme_core::contour::normalize_illumination;
use mapme_core::contour_lines::contour_mask;
use mapme_core::dem::{tiles_covering, DemStore, TileId};
use mapme_core::geo::{merc_to_latlon, Affine};
use mapme_core::georef::{ContourProducts, GeorefRequest, GpsFix, Session, Status};
use mapme_core::hints::{HintSource, SearchHint, DEFAULT_HINT_RADIUS_M};
use mapme_core::image::Rgb8;
use mapme_core::labels::OcrObservation;
use mapme_core::refine::{
    evaluate_prepared, fit_confidence, fit_evidence, fit_quality, refine_dem_box, refine_prepared_with, PreparedDem,
    RefineOptions, RefineResult,
};
use mapme_core::skeleton::default_sampling;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::Instant;

const USAGE: &str = "usage: georef_cli <image.jpg|png> [--gps lat,lon] [--ocr ocr.json] [--dem-dir dir] [--dpi N] \
                     [--methods grid,contours] [--radius-m M] [--contour-rgb r,g,b] [--rotate] [--verify] [--photo] \
                     [--hint lat,lon[,radius_m][,name]]... [--max-tiles N] [--dump-mask out.png] [--missing-out tiles.txt] \
                     [--fit-check a,b,c,d,e,f] [--map-area]";

/// Most `need_dem` rounds before giving up (a 2 000 km wide search takes ~100).
const MAX_ROUNDS: usize = 300;

fn main() -> ExitCode {
    match run() {
        Ok(ok) => {
            if ok {
                ExitCode::SUCCESS
            } else {
                ExitCode::from(1)
            }
        }
        Err(e) => {
            eprintln!("error: {e}\n{USAGE}");
            ExitCode::from(2)
        }
    }
}

fn numbers(s: &str, n: usize, what: &str) -> Result<Vec<f64>, String> {
    let v: Vec<f64> =
        s.split(',').map(|p| p.trim().parse::<f64>()).collect::<Result<_, _>>().map_err(|e| format!("{what}: {e}"))?;
    if v.len() != n {
        return Err(format!("{what}: expected {n} comma-separated numbers"));
    }
    Ok(v)
}

/// Returns Ok(true) when the map was located.
fn run() -> Result<bool, String> {
    let mut args = std::env::args().skip(1);
    let mut image: Option<PathBuf> = None;
    let mut dem_dir: Option<PathBuf> = None;
    let mut dump_mask: Option<PathBuf> = None;
    let mut map_area = false;
    let mut missing_out: Option<PathBuf> = None;
    let mut fit_check: Option<Vec<f64>> = None;
    let mut req = GeorefRequest::default();
    while let Some(a) = args.next() {
        let mut value = |name: &str| args.next().ok_or_else(|| format!("{name} needs a value"));
        match a.as_str() {
            "--gps" => {
                let v = numbers(&value("--gps")?, 2, "--gps")?;
                req.gps = Some(GpsFix { lat: v[0], lon: v[1], accuracy_m: None });
            }
            "--ocr" => req.ocr = read_ocr(Path::new(&value("--ocr")?))?,
            "--dem-dir" => dem_dir = Some(PathBuf::from(value("--dem-dir")?)),
            "--dpi" => req.dpi = Some(value("--dpi")?.parse().map_err(|e| format!("--dpi: {e}"))?),
            "--methods" => req.methods = value("--methods")?.split(',').map(|s| s.trim().to_string()).collect(),
            "--radius-m" => {
                req.search_radius_m = value("--radius-m")?.parse().map_err(|e| format!("--radius-m: {e}"))?
            }
            "--contour-rgb" => {
                let v = numbers(&value("--contour-rgb")?, 3, "--contour-rgb")?;
                req.contour_rgb = Some([v[0], v[1], v[2]].map(|c| c.clamp(0.0, 255.0) as u8));
            }
            "--rotate" => req.allow_rotation = true,
            "--verify" => req.verify = true,
            "--photo" => req.photo = true,
            "--hint" => {
                // lat,lon[,radius_m][,name]: with a name, a place named on the map that the app
                // geocoded (source "place"); without one, an area the user picked.
                let s = value("--hint")?;
                let parts: Vec<&str> = s.splitn(4, ',').map(str::trim).collect();
                let (nums, name): (Vec<&str>, Option<String>) = match parts.as_slice() {
                    [a, b] => (vec![a, b], None),
                    [a, b, c] if c.parse::<f64>().is_ok() => (vec![a, b, c], None),
                    [a, b, c] => (vec![a, b], Some(c.to_string())),
                    [a, b, c, d] if c.parse::<f64>().is_ok() => (vec![a, b, c], Some(d.to_string())),
                    [a, b, c, d] => (vec![a, b], Some(format!("{c},{d}"))),
                    _ => return Err("--hint: expected lat,lon[,radius_m][,name]".into()),
                };
                let v = numbers(&nums.join(","), nums.len(), "--hint")?;
                req.hints.push(SearchHint {
                    lat: v[0],
                    lon: v[1],
                    radius_m: v.get(2).copied().unwrap_or(DEFAULT_HINT_RADIUS_M),
                    source: if name.is_some() { HintSource::Place } else { HintSource::User },
                    name,
                });
            }
            "--max-tiles" => {
                req.max_tiles_per_round = value("--max-tiles")?.parse().map_err(|e| format!("--max-tiles: {e}"))?
            }
            "--dump-mask" => dump_mask = Some(PathBuf::from(value("--dump-mask")?)),
            "--map-area" => map_area = true,
            "--missing-out" => missing_out = Some(PathBuf::from(value("--missing-out")?)),
            "--fit-check" => fit_check = Some(numbers(&value("--fit-check")?, 6, "--fit-check")?),
            "-h" | "--help" => {
                println!("{USAGE}");
                return Ok(true);
            }
            s if s.starts_with("--") => return Err(format!("unknown option {s}")),
            s => {
                if image.replace(PathBuf::from(s)).is_some() {
                    return Err("only one image".into());
                }
            }
        }
    }
    let image = image.ok_or("no image given")?;

    let t = Instant::now();
    let img = load_image(&image)?;
    eprintln!("loaded {} ({}x{}) in {:.0} ms", image.display(), img.w, img.h, t.elapsed().as_secs_f64() * 1e3);
    if map_area {
        let t = Instant::now();
        let c = map_content(&img);
        println!("{}", serde_json::to_string(&c).map_err(|e| e.to_string())?);
        eprintln!("map content area in {:.0} ms", t.elapsed().as_secs_f64() * 1e3);
        return Ok(true);
    }
    if let Some(path) = &dump_mask {
        dump_contours(&img, &req, path)?;
    }
    if let Some(m) = fit_check {
        let t = Affine { a: m[0], b: m[1], c: m[2], d: m[3], e: m[4], f: m[5] };
        return check_fit(&img, &req, dem_dir.as_deref(), &t);
    }
    let session = Session::new(img);
    let mut missing_tiles: Vec<TileId> = Vec::new();

    let mut core_ms = 0.0;
    for round in 1..=MAX_ROUNDS {
        let t = Instant::now();
        let resp = session.georeference(&req);
        let ms = t.elapsed().as_secs_f64() * 1e3;
        core_ms += ms;
        eprintln!("round {round}: {:?} in {ms:.0} ms: {}", resp.status, resp.message);
        if let Some(p) = &resp.search {
            eprintln!(
                "  search: {:?} z{}, {:.0} of {:.0} km, {} of {} tiles, {} of {} hints",
                p.stage,
                p.coarse_zoom,
                p.searched_radius_m / 1000.0,
                p.radius_m / 1000.0,
                p.tiles_fetched,
                p.tiles_planned,
                p.hints_tried,
                p.hints_planned
            );
        }
        if resp.status != Status::NeedDem {
            eprintln!("core time {core_ms:.0} ms over {round} call(s)");
            if let Some(path) = &missing_out {
                write_missing(path, &missing_tiles)?;
            }
            println!("{}", serde_json::to_string_pretty(&resp).map_err(|e| e.to_string())?);
            return Ok(resp.status == Status::Ok);
        }
        let (mut added, mut missing) = (0, 0);
        for &t in &resp.need_tiles {
            match dem_dir.as_ref().and_then(|d| std::fs::read(tile_path(d, t)).ok()) {
                Some(png) => match session.add_dem_tile(t, &png) {
                    Ok(()) => added += 1,
                    Err(e) => {
                        eprintln!("  tile {}/{}/{}: {e}", t.z, t.x, t.y);
                        missing += 1;
                    }
                },
                None => {
                    session.mark_dem_tile_missing(t)?;
                    missing_tiles.push(t);
                    missing += 1;
                }
            }
        }
        eprintln!("  added {added} tile(s), {missing} missing");
    }
    Err(format!("still need DEM tiles after {MAX_ROUNDS} rounds"))
}

/// `--fit-check`: the fit of `t` on the fine DEM (zoom 13, coarser while the
/// map would need more than 36 tiles, as the session plans it), and the
/// refinements from `t` and from `t` shifted.
fn check_fit(img: &Rgb8, req: &GeorefRequest, dem_dir: Option<&Path>, t: &Affine) -> Result<bool, String> {
    let dir = dem_dir.ok_or("--fit-check needs --dem-dir")?;
    let src = if req.photo { normalize_illumination(img) } else { img.clone() };
    let rect = map_rect(&src, req.photo);
    let prod = ContourProducts::compute_in(&src, req.contour_rgb, req.photo, rect);
    let (w, h) = (img.w, img.h);
    let bbox = refine_dem_box(t, w, h);
    let mut zoom = 13u8;
    while zoom > 6 && tiles_covering(zoom, &bbox).len() > 36 {
        zoom -= 1;
    }
    let mut store = DemStore::new();
    let tiles = tiles_covering(zoom, &bbox);
    let mut missing = 0;
    for &tile in &tiles {
        match std::fs::read(tile_path(dir, tile)) {
            Ok(png) => store.add_png(tile, &png)?,
            Err(_) => missing += 1,
        }
    }
    let dem = store.mosaic(zoom, &bbox);
    let prep = PreparedDem::new_cropped(&dem, &bbox);
    let (_, cy) = t.apply(w as f64 / 2.0, h as f64 / 2.0);
    let cos = merc_to_latlon(0.0, cy).0.to_radians().cos();
    eprintln!(
        "fit check: {} segments, {} samples, mask {:.4}; zoom {zoom}, {} tiles ({missing} missing); {:.3} m/px",
        prod.samples.n_segs,
        prod.samples.len(),
        prod.mask_fraction,
        tiles.len(),
        t.mean_scale() * cos
    );
    let show = |what: &str, r: &RefineResult| {
        // Largest corner displacement from `t` (ground metres).
        let corners = [(0.0, 0.0), (w as f64, 0.0), (w as f64, h as f64), (0.0, h as f64)];
        let off = corners
            .iter()
            .map(|&(x, y)| {
                let (p, q) = (t.apply(x, y), r.transform.apply(x, y));
                (p.0 - q.0).hypot(p.1 - q.1) * cos
            })
            .fold(0.0, f64::max);
        println!(
            "{what:>16}: fit {:.3} (quality {:.3}, evidence {:.3}) seg std {:.2} m, cost {:.2}, inliers {:.2}, \
             expl {:.4}, interval {:.1} m snr {:.2}, align {:.3}, relief {:.0} m, cover {:.2}, {} segs, affine {}, \
             corners {:.0} m",
            fit_confidence(r),
            fit_quality(r),
            fit_evidence(r),
            r.median_seg_std_m,
            r.cost_m,
            r.inlier_fraction,
            r.explained_variance,
            r.contour_interval_m,
            r.interval_snr,
            r.gradient_alignment,
            r.relief_m,
            r.dem_coverage,
            r.n_segments,
            r.affine,
            off
        );
    };
    show("given", &evaluate_prepared(&prod.samples, &prep, t));
    let opts = RefineOptions { photo: req.photo, ..Default::default() };
    for (dx, dy) in [(0.0, 0.0), (300.0, 0.0), (0.0, -1000.0), (2000.0, 2000.0)] {
        let k = 1.0 / cos;
        let start = Affine { c: t.c + dx * k, f: t.f + dy * k, ..*t };
        let r = refine_prepared_with(&prod.samples, &prep, &start, &opts);
        show(&format!("from {dx:+.0},{dy:+.0} m"), &r);
    }
    Ok(true)
}

/// Appends `z x y` lines for the tiles the core asked for that were not in the DEM directory.
fn write_missing(path: &Path, tiles: &[TileId]) -> Result<(), String> {
    use std::io::Write;
    let mut f = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .map_err(|e| format!("{}: {e}", path.display()))?;
    for t in tiles {
        writeln!(f, "{} {} {}", t.z, t.x, t.y).map_err(|e| format!("{}: {e}", path.display()))?;
    }
    Ok(())
}

/// The map's rectangle of the raster, as the session finds it (photos: the whole raster).
fn map_rect(img: &Rgb8, photo: bool) -> MapRect {
    if photo {
        MapRect::whole(img.w, img.h)
    } else {
        map_content(img).rect
    }
}

/// The contour extraction of `req` on `img` as a grey PNG: 255 at the skeleton samples the
/// session matches, 128 elsewhere on the colour mask, 0 off it.
fn dump_contours(img: &Rgb8, req: &GeorefRequest, path: &Path) -> Result<(), String> {
    let t = Instant::now();
    let src = if req.photo { normalize_illumination(img) } else { img.clone() };
    // The session works on the map's rectangle of the raster (`content.rs`).
    let rect = map_rect(&src, req.photo);
    let map = rect.crop(&src);
    let (min_len, _) = default_sampling(map.w, map.h);
    let lm = contour_mask(&map, req.contour_rgb, req.photo, min_len);
    let prod = ContourProducts::compute_in(&src, req.contour_rgb, req.photo, rect);
    // The mask at its analysis level, each pixel over the base pixels it covers.
    let f = lm.level.max(1);
    let mut px = vec![0u8; img.w * img.h];
    for (y, row) in px.chunks_exact_mut(img.w).enumerate() {
        for (x, o) in row.iter_mut().enumerate() {
            if x < rect.x0 || y < rect.y0 {
                continue;
            }
            let (mx, my) = ((x - rect.x0) / f, (y - rect.y0) / f);
            if mx < lm.mask.w && my < lm.mask.h && lm.mask.get(mx, my) {
                *o = 128;
            }
        }
    }
    for p in &prod.samples.pts {
        let (x, y) = (p[0].floor() as usize, p[1].floor() as usize);
        if x < img.w && y < img.h {
            px[y * img.w + x] = 255;
        }
    }
    let file = std::fs::File::create(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let mut enc = png::Encoder::new(std::io::BufWriter::new(file), img.w as u32, img.h as u32);
    enc.set_color(png::ColorType::Grayscale);
    enc.set_depth(png::BitDepth::Eight);
    let mut wr = enc.write_header().map_err(|e| e.to_string())?;
    wr.write_image_data(&px).map_err(|e| e.to_string())?;
    eprintln!(
        "dumped contour mask to {} (level {}, mask fraction {:.4}, {} samples in {} segments, {:.0} ms; \
         products: mask {:.0} ms, skeleton {:.0} ms, field {:.0} ms)\n  extraction {}",
        path.display(),
        lm.level,
        prod.mask_fraction,
        prod.samples.len(),
        prod.samples.n_segs,
        t.elapsed().as_secs_f64() * 1e3,
        prod.timings_ms[0],
        prod.timings_ms[1],
        prod.timings_ms[2],
        prod.extraction
    );
    Ok(())
}

fn tile_path(dir: &Path, t: TileId) -> PathBuf {
    dir.join(format!("{}_{}_{}.png", t.z, t.x, t.y))
}

fn read_ocr(path: &Path) -> Result<Vec<OcrObservation>, String> {
    let bytes = std::fs::read(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let v: serde_json::Value = serde_json::from_slice(&bytes).map_err(|e| format!("{}: {e}", path.display()))?;
    let obs = if v.is_array() { v } else { v["observations"].clone() };
    serde_json::from_value(obs).map_err(|e| format!("{}: {e}", path.display()))
}

fn load_image(path: &Path) -> Result<Rgb8, String> {
    let bytes = std::fs::read(path).map_err(|e| format!("{}: {e}", path.display()))?;
    if bytes.starts_with(&[0x89, b'P', b'N', b'G']) { load_png(&bytes) } else { load_jpeg(&bytes) }
        .map_err(|e| format!("{}: {e}", path.display()))
}

fn load_jpeg(bytes: &[u8]) -> Result<Rgb8, String> {
    let mut dec = jpeg_decoder::Decoder::new(std::io::Cursor::new(bytes));
    let px = dec.decode().map_err(|e| e.to_string())?;
    let info = dec.info().ok_or("no JPEG info")?;
    let (w, h) = (info.width as usize, info.height as usize);
    let rgb = match info.pixel_format {
        jpeg_decoder::PixelFormat::RGB24 => px,
        jpeg_decoder::PixelFormat::L8 => px.iter().flat_map(|&g| [g, g, g]).collect(),
        jpeg_decoder::PixelFormat::L16 => px.chunks_exact(2).flat_map(|c| [c[0], c[0], c[0]]).collect(),
        jpeg_decoder::PixelFormat::CMYK32 => px
            .chunks_exact(4)
            .flat_map(|c| {
                let k = 255 - c[3] as u32;
                [c[0], c[1], c[2]].map(|v| ((255 - v as u32) * k / 255) as u8)
            })
            .collect(),
    };
    Rgb8::from_rgb(w, h, rgb).ok_or_else(|| "unexpected JPEG buffer size".into())
}

fn load_png(bytes: &[u8]) -> Result<Rgb8, String> {
    let mut dec = png::Decoder::new(bytes);
    dec.set_transformations(png::Transformations::EXPAND | png::Transformations::STRIP_16);
    let mut reader = dec.read_info().map_err(|e| e.to_string())?;
    let mut buf = vec![0u8; reader.output_buffer_size()];
    let info = reader.next_frame(&mut buf).map_err(|e| e.to_string())?;
    let (w, h) = (info.width as usize, info.height as usize);
    let px = &buf[..info.buffer_size()];
    let channels = match info.color_type {
        png::ColorType::Grayscale => 1,
        png::ColorType::GrayscaleAlpha => 2,
        png::ColorType::Rgb => 3,
        png::ColorType::Rgba => 4,
        png::ColorType::Indexed => return Err("indexed PNG was not expanded".into()),
    };
    let mut rgb = Vec::with_capacity(w * h * 3);
    for y in 0..h {
        let row = &px[y * info.line_size..y * info.line_size + w * channels];
        for p in row.chunks_exact(channels) {
            match channels {
                1 | 2 => rgb.extend_from_slice(&[p[0], p[0], p[0]]),
                _ => rgb.extend_from_slice(&p[..3]),
            }
        }
    }
    Rgb8::from_rgb(w, h, rgb).ok_or_else(|| "unexpected PNG buffer size".into())
}
