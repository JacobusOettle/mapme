//! Developer tool: georeference a map image from the command line.
//!
//! ```text
//! cargo run --release --example georef_cli -- <image.jpg|png> [--gps lat,lon] [--ocr ocr.json]
//!     [--dem-dir dir] [--dpi N] [--methods grid,contours] [--radius-m M] [--contour-rgb r,g,b]
//!     [--rotate] [--verify] [--photo] [--hint lat,lon[,radius_m]]... [--max-tiles N]
//!     [--dump-mask out.png] [--missing-out tiles.txt]
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
//! by `ContourProducts::compute_with`, as the session does), 128 elsewhere on
//! the colour mask (the same choice of model, recomputed here), 0 off it.
//! `--missing-out` appends every tile the core asked for that is not in
//! `--dem-dir` (`z x y` per line) to a file, so a benchmark can fetch them.

use mapme_core::contour::{auto_mask, extract_mask, model_from_rgb, normalize_illumination, photo_mask};
use mapme_core::dem::TileId;
use mapme_core::georef::{ContourProducts, GeorefRequest, GpsFix, Session, Status};
use mapme_core::hints::{HintSource, SearchHint, DEFAULT_HINT_RADIUS_M};
use mapme_core::image::Rgb8;
use mapme_core::labels::OcrObservation;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::Instant;

const USAGE: &str = "usage: georef_cli <image.jpg|png> [--gps lat,lon] [--ocr ocr.json] [--dem-dir dir] [--dpi N] \
                     [--methods grid,contours] [--radius-m M] [--contour-rgb r,g,b] [--rotate] [--verify] [--photo] \
                     [--hint lat,lon[,radius_m]]... [--max-tiles N] [--dump-mask out.png] [--missing-out tiles.txt]";

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
    let mut missing_out: Option<PathBuf> = None;
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
                let s = value("--hint")?;
                let n = s.split(',').count();
                let v = numbers(&s, n.clamp(2, 3), "--hint")?;
                req.hints.push(SearchHint {
                    lat: v[0],
                    lon: v[1],
                    radius_m: v.get(2).copied().unwrap_or(DEFAULT_HINT_RADIUS_M),
                    source: HintSource::User,
                    name: None,
                });
            }
            "--max-tiles" => {
                req.max_tiles_per_round = value("--max-tiles")?.parse().map_err(|e| format!("--max-tiles: {e}"))?
            }
            "--dump-mask" => dump_mask = Some(PathBuf::from(value("--dump-mask")?)),
            "--missing-out" => missing_out = Some(PathBuf::from(value("--missing-out")?)),
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
    if let Some(path) = &dump_mask {
        dump_contours(&img, &req, path)?;
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

/// The contour extraction of `req` on `img` as a grey PNG: 255 at the skeleton samples the
/// session matches, 128 elsewhere on the colour mask, 0 off it.
fn dump_contours(img: &Rgb8, req: &GeorefRequest, path: &Path) -> Result<(), String> {
    let t = Instant::now();
    let src = if req.photo { normalize_illumination(img) } else { img.clone() };
    let mask = match (req.contour_rgb, req.photo) {
        (Some(rgb), false) => extract_mask(&src, &model_from_rgb(rgb)),
        (None, false) => auto_mask(&src).0,
        (rgb, true) => photo_mask(&src, rgb.map(model_from_rgb).as_ref()).0,
    };
    let prod = ContourProducts::compute_with(&src, req.contour_rgb, req.photo);
    let mut px: Vec<u8> = mask.data.iter().map(|&m| if m != 0 { 128 } else { 0 }).collect();
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
        "dumped contour mask to {} (mask fraction {:.4}, {} samples in {} segments, {:.0} ms)",
        path.display(),
        prod.mask_fraction,
        prod.samples.len(),
        prod.samples.n_segs,
        t.elapsed().as_secs_f64() * 1e3
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
