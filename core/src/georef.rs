//! Session + orchestrator: request JSON → response JSON.
//!
//! A `Session` owns one map raster and the DEM tiles the app has supplied, and
//! caches everything derived from them (grid lines, contour products, coarse
//! search candidates, refinements), so the app's DEM loop (`need_dem` → add
//! tiles → call again with the same request) never repeats work. The methods
//! of a request run in order:
//!
//! * **grid**: grid lines + coordinate labels (`grid.rs`). A line fit with a
//!   small RMS is the answer. A labels-only fit seeds the contour stage.
//! * **contours**: a labels-only fit is refined against the zoom-13 DEM
//!   first. Otherwise the coarse FFT search runs around the GPS position on
//!   zoom-11 tiles, then the best candidates are refined on zoom-13 tiles
//!   (the best alone first when it clearly stands out) and the most
//!   confident one wins.
//!
//! A request with `photo: true` (a photo of a paper map, flattened by its
//! page borders) analyses an illumination- and white-balance-normalised copy
//! of the raster (`contour::normalize_illumination`), detects grid lines
//! against their local background, extracts contours with hysteresis, and
//! lets the grid fit and the contour refinement end in a homography, which
//! absorbs the perspective and aspect error the flattening left.
//!
//! When a stage needs DEM tiles that are neither present nor marked missing,
//! the call returns `need_dem` and starts computing the contour products on
//! a background thread while the app downloads.
//!
//! `georeference` takes `&self`; `progress` and `cancel` may be called from
//! other threads while it runs. See docs/ARCHITECTURE.md for the JSON.

use crate::contour::{auto_mask, extract_mask, model_from_rgb, normalize_illumination, photo_mask, ContourColorModel};
use crate::control_points::{self, ControlPoint};
use crate::dem::{merc_to_tile, resolution, tiles_covering, DemMosaic, DemStore, MercBox, TileId};
use crate::geo::{latlon_to_merc, merc_to_latlon, Affine, Homography, LatLon};
use crate::grid::{
    detect_lines, detect_lines_photo, georeference_from_grid_with, GridLine, GridMethod, GridOptions, GridResult,
    LineAxis,
};
use crate::image::Rgb8;
use crate::labels::{classify_pois, parse_coordinate, parse_scale, CoordValue, OcrObservation, PoiLabel};
use crate::refine::{
    confidence, evaluate_prepared, evaluate_prepared_homography, fit_confidence, refine_dem_box, refine_prepared_with,
    select_refined, PreparedDem, RefineOptions, RefineResult,
};
use crate::search::{coarse_search, map_field, required_dem_box, rotation_sweep, Candidate, MapField, SearchParams};
use crate::skeleton::{contour_samples, default_sampling, ContourSamples};
use serde::{Deserialize, Serialize};
use serde_json::{json, Map, Value};
use std::collections::{BTreeMap, HashMap};
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, OnceLock};
use std::time::Instant;

/// Grid line fits with a larger RMS (ground metres) only seed the contour stage.
pub const GRID_MAX_RMS_M: f64 = 25.0;
/// Contour matches at least this confident are reported without refining
/// further candidates (`refine::confidence`: ≥ 0.7 is a confident match).
pub const CONFIDENT: f64 = 0.7;
/// Lowest confidence reported as a match; below it the call fails.
pub const ACCEPT: f64 = 0.5;
/// Coarse-search candidates refined at most.
pub const MAX_REFINE_CANDIDATES: usize = 3;
/// Peak ratio from which the best coarse candidate is refined alone first
/// (`refine::confidence` gives full search evidence from 1.6).
const DISTINCT_PEAK_RATIO: f32 = 1.6;
/// Search evidence credited to a labels-only fit that the contours agree with.
const LABELS_PEAK_RATIO: f32 = 1.6;
/// Fewer contour segments than this: the map has no usable contours.
pub const MIN_SEGMENTS: usize = 10;
/// DEM zoom of the coarse search and preferred zoom of refinement.
pub const COARSE_ZOOM: u8 = 11;
pub const FINE_ZOOM: u8 = 13;
/// One refinement may need at most this many tiles; coarser zooms are used beyond.
const MAX_FINE_TILES: u64 = 36;
/// Scale range (ground m/px) searched without a scale prior.
pub const DEFAULT_SCALE_RANGE: (f64, f64) = (0.4, 12.0);
/// Allowed search radius (ground metres).
const RADIUS_RANGE_M: (f64, f64) = (1_000.0, 50_000.0);
/// Ground scales (m/px) outside this range are not plausible map transforms.
const PLAUSIBLE_SCALE: (f64, f64) = (0.02, 500.0);
/// A mosaic counts as complete when this fraction of it has elevations.
const COMPLETE_DEM: f64 = 0.98;

// ---------------------------------------------------------------------------
// Request / response
// ---------------------------------------------------------------------------

/// GPS fix of the device.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct GpsFix {
    pub lat: f64,
    pub lon: f64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub accuracy_m: Option<f64>,
}

/// Georeference request. Every field is optional on the wire (null counts as
/// absent) and unknown fields are ignored; see `Default` for the defaults.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(try_from = "RawRequest")]
pub struct GeorefRequest {
    pub gps: Option<GpsFix>,
    pub ocr: Vec<OcrObservation>,
    /// Pixels per inch of the raster when known (PDF render).
    pub dpi: Option<f64>,
    /// User-picked contour colour; None = automatic.
    pub contour_rgb: Option<[u8; 3]>,
    pub search_radius_m: f64,
    pub allow_rotation: bool,
    /// "grid" and/or "contours", tried in order.
    pub methods: Vec<String>,
    /// Check a grid result against the terrain (adds `diagnostics.contour_check`).
    pub verify: bool,
    /// The raster is a photo of a paper map, flattened by its page borders
    /// (photo mode: lighting normalisation, homography fits).
    pub photo: bool,
}

impl Default for GeorefRequest {
    fn default() -> Self {
        GeorefRequest {
            gps: None,
            ocr: Vec::new(),
            dpi: None,
            contour_rgb: None,
            search_radius_m: 20_000.0,
            allow_rotation: false,
            methods: vec!["grid".into(), "contours".into()],
            verify: false,
            photo: false,
        }
    }
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct RawRequest {
    gps: Option<GpsFix>,
    ocr: Option<Vec<OcrObservation>>,
    dpi: Option<f64>,
    contour_rgb: Option<Vec<f64>>,
    search_radius_m: Option<f64>,
    allow_rotation: Option<bool>,
    methods: Option<Vec<String>>,
    verify: Option<bool>,
    photo: Option<bool>,
}

impl TryFrom<RawRequest> for GeorefRequest {
    type Error = String;

    fn try_from(r: RawRequest) -> Result<Self, String> {
        let d = GeorefRequest::default();
        let contour_rgb = match r.contour_rgb {
            None => None,
            Some(v) if v.len() == 3 && v.iter().all(|c| c.is_finite()) => {
                Some([0, 1, 2].map(|i| v[i].round().clamp(0.0, 255.0) as u8))
            }
            Some(_) => return Err("contour_rgb must be [r, g, b] with values 0-255".into()),
        };
        Ok(GeorefRequest {
            gps: r.gps,
            ocr: r.ocr.unwrap_or_default(),
            dpi: r.dpi,
            contour_rgb,
            search_radius_m: r.search_radius_m.unwrap_or(d.search_radius_m),
            allow_rotation: r.allow_rotation.unwrap_or(d.allow_rotation),
            methods: r.methods.unwrap_or(d.methods),
            verify: r.verify.unwrap_or(d.verify),
            photo: r.photo.unwrap_or(d.photo),
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Status {
    Ok,
    NeedDem,
    Failed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Method {
    Grid,
    Labels,
    Contours,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GeorefResponse {
    pub status: Status,
    pub method: Option<Method>,
    /// Row-major 3×3 pixel → Web Mercator homography.
    pub pixel_to_mercator: Option<[f64; 9]>,
    /// 0..1.
    pub confidence: Option<f64>,
    /// Estimated position error, ground metres.
    pub rms_m: Option<f64>,
    /// Ground metres per pixel at the image centre.
    pub scale_m_per_px: Option<f64>,
    /// Clockwise angle of map-up from true north, degrees in [0, 360).
    pub rotation_deg: Option<f64>,
    /// Image corners TL, TR, BR, BL as [lat, lon].
    pub corners: Option<[[f64; 2]; 4]>,
    #[serde(default)]
    pub need_tiles: Vec<TileId>,
    #[serde(default)]
    pub pois: Vec<PoiLabel>,
    #[serde(default)]
    pub message: String,
    #[serde(default)]
    pub diagnostics: Value,
    #[serde(default)]
    pub timings_ms: BTreeMap<String, f64>,
}

/// Message of a call stopped by `Session::cancel`.
pub const CANCELLED: &str = "cancelled";

impl GeorefResponse {
    fn empty(status: Status, message: String) -> GeorefResponse {
        GeorefResponse {
            status,
            method: None,
            pixel_to_mercator: None,
            confidence: None,
            rms_m: None,
            scale_m_per_px: None,
            rotation_deg: None,
            corners: None,
            need_tiles: Vec::new(),
            pois: Vec::new(),
            message,
            diagnostics: Value::Object(Map::new()),
            timings_ms: BTreeMap::new(),
        }
    }

    pub fn failed(message: impl Into<String>) -> GeorefResponse {
        GeorefResponse::empty(Status::Failed, message.into())
    }

    pub fn is_cancelled(&self) -> bool {
        self.status == Status::Failed && self.message == CANCELLED
    }
}

// ---------------------------------------------------------------------------
// Control points (stateless)
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ControlPointRequest {
    pub points: Vec<ControlPoint>,
    /// "auto" | "similarity" | "affine" | "homography"
    #[serde(default = "auto_model", deserialize_with = "null_as_auto")]
    pub model: String,
}

fn auto_model() -> String {
    "auto".into()
}

fn null_as_auto<'de, D: serde::Deserializer<'de>>(d: D) -> Result<String, D::Error> {
    Ok(Option::<String>::deserialize(d)?.unwrap_or_else(auto_model))
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ControlPointResponse {
    /// "ok" | "failed"
    pub status: Status,
    pub pixel_to_mercator: Option<[f64; 9]>,
    pub rms_m: Option<f64>,
    #[serde(default)]
    pub residuals_m: Vec<f64>,
    pub model: Option<String>,
    pub scale_m_per_px: Option<f64>,
    pub rotation_deg: Option<f64>,
    #[serde(default)]
    pub message: String,
}

/// Fit a pixel → Mercator transform to manual control points.
pub fn fit_control_points(req: &ControlPointRequest) -> ControlPointResponse {
    match control_points::fit(&req.points, &req.model.to_ascii_lowercase()) {
        Ok(f) => {
            // Describe at the control points' centroid (the image size is unknown here).
            let n = req.points.len().max(1) as f64;
            let cx = req.points.iter().map(|p| p.x).sum::<f64>() / n;
            let cy = req.points.iter().map(|p| p.y).sum::<f64>() / n;
            let (scale, rotation) = scale_and_rotation(&f.transform, cx, cy);
            let rms = round_dp(f.rms_m, 2);
            ControlPointResponse {
                status: Status::Ok,
                pixel_to_mercator: Some(f.transform.m),
                rms_m: Some(rms),
                residuals_m: f.residuals_m.iter().map(|r| round_dp(*r, 2)).collect(),
                message: format!(
                    "Fitted a{} {} to {} points ({}).",
                    if f.model.starts_with('a') { "n" } else { "" },
                    f.model,
                    req.points.len(),
                    plus_minus(f.rms_m)
                ),
                model: Some(f.model),
                scale_m_per_px: Some(round_dp(scale, 4)),
                rotation_deg: Some(round_dp(rotation, 2) % 360.0),
            }
        }
        Err(e) => ControlPointResponse {
            status: Status::Failed,
            pixel_to_mercator: None,
            rms_m: None,
            residuals_m: Vec::new(),
            model: None,
            scale_m_per_px: None,
            rotation_deg: None,
            message: e,
        },
    }
}

/// `fit_control_points` on JSON; never fails (errors become a failed response).
pub fn fit_control_points_json(json: &str) -> String {
    let resp = match serde_json::from_str::<ControlPointRequest>(json) {
        Ok(req) => fit_control_points(&req),
        Err(e) => ControlPointResponse {
            status: Status::Failed,
            pixel_to_mercator: None,
            rms_m: None,
            residuals_m: Vec::new(),
            model: None,
            scale_m_per_px: None,
            rotation_deg: None,
            message: format!("Invalid request: {e}"),
        },
    };
    serde_json::to_string(&resp).unwrap_or_else(|e| failed_json(&format!("Could not encode the response: {e}")))
}

/// `{"status":"failed","message":...}` as a JSON string.
pub fn failed_json(message: &str) -> String {
    json!({"status": "failed", "message": message}).to_string()
}

// ---------------------------------------------------------------------------
// Session
// ---------------------------------------------------------------------------

/// What contour products are derived from: the colour choice (None =
/// automatic model) and whether the raster is analysed as a photo.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
struct ContourKey {
    rgb: Option<[u8; 3]>,
    photo: bool,
}

/// Contour data derived from the raster for one colour choice.
pub struct ContourProducts {
    pub model: ContourColorModel,
    pub mask_fraction: f64,
    pub samples: ContourSamples,
    pub field: MapField,
    /// Milliseconds spent on the mask, the skeleton samples and the field.
    pub timings_ms: [f64; 3],
}

impl ContourProducts {
    pub fn compute(img: &Rgb8, contour_rgb: Option<[u8; 3]>) -> ContourProducts {
        ContourProducts::compute_with(img, contour_rgb, false)
    }

    /// `compute`; with `photo`, `img` is the normalised photo
    /// (`normalize_illumination`) and the mask is grown by hysteresis.
    pub fn compute_with(img: &Rgb8, contour_rgb: Option<[u8; 3]>, photo: bool) -> ContourProducts {
        let t = Instant::now();
        let (mask, model) = match (contour_rgb, photo) {
            (Some(rgb), false) => {
                let m = model_from_rgb(rgb);
                (extract_mask(img, &m), m)
            }
            (None, false) => auto_mask(img),
            (Some(rgb), true) => photo_mask(img, Some(&model_from_rgb(rgb))),
            (None, true) => photo_mask(img, None),
        };
        let t_mask = ms_since(t);
        let (min_len, step) = default_sampling(img.w, img.h);
        let ((samples, t_skel), (field, t_field)) = rayon::join(
            || {
                let t = Instant::now();
                (contour_samples(&mask, min_len, step), ms_since(t))
            },
            || {
                let t = Instant::now();
                (map_field(&mask), ms_since(t))
            },
        );
        ContourProducts { model, mask_fraction: mask.fraction(), samples, field, timings_ms: [t_mask, t_skel, t_field] }
    }
}

#[derive(Clone, PartialEq, Eq, Hash)]
struct SearchKey {
    contour: ContourKey,
    /// GPS position (Mercator metres, rounded), radius (m) and scale range bits.
    center: [i64; 2],
    radius_m: i64,
    scale: [u64; 2],
    rotate: bool,
    /// Present coarse tiles: the result depends on what DEM was there.
    tiles_present: usize,
}

#[derive(Clone, PartialEq, Eq, Hash)]
struct RefineKey {
    contour: ContourKey,
    transform: [u64; 6],
    zoom: u8,
    /// Valid fraction of the mosaic used (bits): identifies the DEM content.
    dem_valid: u64,
    evaluate_only: bool,
}

/// Georeferencing session bound to one map raster.
pub struct Session {
    image: Arc<Rgb8>,
    /// Illumination-normalised copy for photo mode (computed on first use).
    photo_image: Arc<OnceLock<Arc<Rgb8>>>,
    dem: Mutex<DemStore>,
    lines: OnceLock<Vec<GridLine>>,
    photo_lines: OnceLock<Vec<GridLine>>,
    contours: Mutex<HashMap<ContourKey, Arc<OnceLock<Arc<ContourProducts>>>>>,
    searches: Mutex<Vec<(SearchKey, Arc<Vec<Candidate>>)>>,
    refinements: Mutex<HashMap<RefineKey, RefineResult>>,
    cancel: AtomicBool,
    /// f32 bits of the progress of the running call.
    progress: AtomicU32,
}

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

/// Searches kept in the cache.
const SEARCH_CACHE: usize = 4;

impl Session {
    pub fn new(image: Rgb8) -> Session {
        Session {
            image: Arc::new(image),
            photo_image: Arc::new(OnceLock::new()),
            dem: Mutex::new(DemStore::new()),
            lines: OnceLock::new(),
            photo_lines: OnceLock::new(),
            contours: Mutex::new(HashMap::new()),
            searches: Mutex::new(Vec::new()),
            refinements: Mutex::new(HashMap::new()),
            cancel: AtomicBool::new(false),
            progress: AtomicU32::new(0f32.to_bits()),
        }
    }

    pub fn image(&self) -> &Rgb8 {
        &self.image
    }

    /// Add a Terrarium PNG tile. A tile that fails to decode is marked
    /// missing (so it is not requested again) and the error returned.
    pub fn add_dem_tile(&self, t: TileId, png: &[u8]) -> Result<(), String> {
        check_tile(t)?;
        let mut store = lock(&self.dem);
        store.add_png(t, png).inspect_err(|_| store.mark_missing(t))
    }

    /// Record that a tile is unavailable so it is not requested again.
    pub fn mark_dem_tile_missing(&self, t: TileId) -> Result<(), String> {
        check_tile(t)?;
        lock(&self.dem).mark_missing(t);
        Ok(())
    }

    /// Progress of the running call, 0..1.
    pub fn progress(&self) -> f32 {
        let p = f32::from_bits(self.progress.load(Ordering::Relaxed));
        if p.is_finite() {
            p.clamp(0.0, 1.0)
        } else {
            0.0
        }
    }

    /// Stop the running call at its next checkpoint (it returns failed
    /// "cancelled"). Requested while no call runs, it cancels the next one.
    /// The request is consumed when a call returns ok or failed.
    pub fn cancel(&self) {
        self.cancel.store(true, Ordering::Relaxed);
    }

    fn cancelled(&self) -> bool {
        self.cancel.load(Ordering::Relaxed)
    }

    /// Raise the progress to `p` (never lowers it within a call).
    fn bump_progress(&self, p: f32) {
        let p = p.clamp(0.0, 1.0);
        // Non-negative f32 bit patterns order like the values.
        self.progress.fetch_max(p.to_bits(), Ordering::Relaxed);
    }

    fn contour_cell(&self, key: ContourKey) -> Arc<OnceLock<Arc<ContourProducts>>> {
        lock(&self.contours).entry(key).or_default().clone()
    }

    /// The raster as analysed: the original, or for photos its normalised copy.
    fn analysed(&self, photo: bool) -> Arc<Rgb8> {
        if photo {
            normalized(&self.photo_image, &self.image)
        } else {
            Arc::clone(&self.image)
        }
    }

    /// Compute the contour products on a background thread (while the app downloads DEM tiles).
    fn prefetch_contours(&self, key: ContourKey) {
        let cell = self.contour_cell(key);
        if cell.get().is_some() || self.cancelled() {
            return;
        }
        let img = Arc::clone(&self.image);
        let norm = Arc::clone(&self.photo_image);
        let _ = std::thread::Builder::new().name("mapme-contours".into()).spawn(move || {
            let src = if key.photo { normalized(&norm, &img) } else { img };
            let _ = cell.get_or_init(|| Arc::new(ContourProducts::compute_with(&src, key.rgb, key.photo)));
        });
    }

    /// Best available DEM over `plan.bbox`: its zoom if complete, else the
    /// most complete of the two next coarser zooms (tiles already present,
    /// e.g. the coarse-search tiles when offline). None without any DEM.
    fn fine_dem(&self, plan: &FinePlan) -> Option<(DemMosaic, u8)> {
        let store = lock(&self.dem);
        let mut best: Option<(DemMosaic, u8, f64)> = None;
        for z in (plan.zoom.saturating_sub(2).max(1)..=plan.zoom).rev() {
            if tile_count(z, &plan.bbox) > 4 * MAX_FINE_TILES {
                continue;
            }
            let m = store.mosaic(z, &plan.bbox);
            let vf = m.valid_fraction();
            if vf >= COMPLETE_DEM {
                return Some((m, z));
            }
            if vf > best.as_ref().map_or(0.0, |b| b.2 + 0.05) {
                best = Some((m, z, vf));
            }
        }
        best.filter(|b| b.2 >= 0.3).map(|b| (b.0, b.1))
    }

    /// Run georeferencing. Never panics on bad input; errors are failed responses.
    pub fn georeference(&self, req: &GeorefRequest) -> GeorefResponse {
        self.progress.store(0f32.to_bits(), Ordering::Relaxed);
        let mut run = Run::new(self, req);
        let mut resp = match run.execute() {
            Ok(r) => r,
            Err(r) => *r,
        };
        run.finish(&mut resp);
        if resp.status != Status::NeedDem {
            self.progress.store(1f32.to_bits(), Ordering::Relaxed);
            self.cancel.store(false, Ordering::Relaxed);
        }
        resp
    }

    /// `georeference` on JSON; never fails (errors become a failed response).
    pub fn georeference_json(&self, json: &str) -> String {
        let resp = match serde_json::from_str::<GeorefRequest>(json) {
            Ok(req) => self.georeference(&req),
            Err(e) => GeorefResponse::failed(format!("Invalid request: {e}")),
        };
        serde_json::to_string(&resp).unwrap_or_else(|e| failed_json(&format!("Could not encode the response: {e}")))
    }
}

/// The normalised photo in `cell`, computed from `img` on first use.
fn normalized(cell: &OnceLock<Arc<Rgb8>>, img: &Rgb8) -> Arc<Rgb8> {
    Arc::clone(cell.get_or_init(|| Arc::new(normalize_illumination(img))))
}

fn check_tile(t: TileId) -> Result<(), String> {
    if t.z > 24 || t.x >= 1u32 << t.z || t.y >= 1u32 << t.z {
        return Err(format!("invalid tile {}/{}/{}", t.z, t.x, t.y));
    }
    Ok(())
}

/// Number of tiles of zoom z covering `b` (without listing them).
fn tile_count(z: u8, b: &MercBox) -> u64 {
    let n = 1u64 << z;
    let (fx0, fy0) = merc_to_tile(z, b.min_x, b.max_y);
    let (fx1, fy1) = merc_to_tile(z, b.max_x, b.min_y);
    let c = |v: f64| (v.floor().max(0.0) as u64).min(n - 1);
    (c(fx1) - c(fx0) + 1) * (c(fy1) - c(fy0) + 1)
}

/// DEM needed to refine or check one transform.
struct FinePlan {
    zoom: u8,
    bbox: MercBox,
    tiles: Vec<TileId>,
}

/// Zoom-13 tiles around the transform's footprint (`refine_dem_box`), or a
/// coarser zoom for small-scale maps whose footprint would need too many.
/// None for implausible transforms.
fn fine_plan(t: &Affine, w: usize, h: usize) -> Option<FinePlan> {
    let (scale, _) = scale_and_rotation(&t.to_homography(), w as f64 / 2.0, h as f64 / 2.0);
    if !(scale.is_finite() && (PLAUSIBLE_SCALE.0..=PLAUSIBLE_SCALE.1).contains(&scale)) {
        return None;
    }
    let bbox = refine_dem_box(t, w, h);
    if !(bbox.min_x.is_finite() && bbox.max_x.is_finite() && bbox.min_y.is_finite() && bbox.max_y.is_finite()) {
        return None;
    }
    let mut zoom = FINE_ZOOM;
    while zoom > 6 && tile_count(zoom, &bbox) > MAX_FINE_TILES {
        zoom -= 1;
    }
    Some(FinePlan { zoom, tiles: tiles_covering(zoom, &bbox), bbox })
}

// ---------------------------------------------------------------------------
// One georeference call
// ---------------------------------------------------------------------------

#[derive(Clone, Copy, PartialEq, Eq)]
enum MethodReq {
    Grid,
    Contours,
}

/// Outcome of the contour stage.
enum Found {
    Located(Box<GeorefResponse>),
    NotFound(String),
}

/// `Err` stops the call with that response (need_dem, cancelled).
type Step<T> = Result<T, Box<GeorefResponse>>;

struct Run<'a> {
    s: &'a Session,
    req: &'a GeorefRequest,
    gps: Option<LatLon>,
    radius_m: f64,
    timings: BTreeMap<String, f64>,
    diag: Map<String, Value>,
    used_labels: Vec<usize>,
    t0: Instant,
}

impl<'a> Run<'a> {
    fn new(s: &'a Session, req: &'a GeorefRequest) -> Run<'a> {
        let mut diag = Map::new();
        let gps = req
            .gps
            .filter(|g| g.lat.is_finite() && g.lon.is_finite() && g.lat.abs() <= 85.0 && g.lon.abs() <= 180.0)
            .map(|g| LatLon::new(g.lat, g.lon));
        if req.gps.is_some() && gps.is_none() {
            diag.insert("gps_ignored".into(), json!("invalid coordinates"));
        }
        if req.photo {
            diag.insert("photo".into(), json!(true));
        }
        let radius_m = if req.search_radius_m.is_finite() {
            req.search_radius_m.clamp(RADIUS_RANGE_M.0, RADIUS_RANGE_M.1)
        } else {
            GeorefRequest::default().search_radius_m
        };
        Run { s, req, gps, radius_m, timings: BTreeMap::new(), diag, used_labels: Vec::new(), t0: Instant::now() }
    }

    fn size(&self) -> (usize, usize) {
        (self.s.image.w, self.s.image.h)
    }

    fn time(&mut self, stage: &str, since: Instant) {
        *self.timings.entry(stage.to_string()).or_insert(0.0) += ms_since(since);
    }

    fn check_cancel(&self) -> Step<()> {
        if self.s.cancelled() {
            let mut r = GeorefResponse::failed(CANCELLED);
            r.diagnostics = json!({"cancelled": true});
            return Err(Box::new(r));
        }
        Ok(())
    }

    fn finish(&mut self, resp: &mut GeorefResponse) {
        self.timings.insert("total".into(), ms_since(self.t0));
        resp.timings_ms = std::mem::take(&mut self.timings).into_iter().map(|(k, v)| (k, round_dp(v, 1))).collect();
        let mut diag = std::mem::take(&mut self.diag);
        if let Value::Object(extra) = std::mem::take(&mut resp.diagnostics) {
            diag.extend(extra);
        }
        resp.diagnostics = Value::Object(diag);
        if resp.status != Status::NeedDem && !resp.is_cancelled() {
            resp.pois = self.pois();
        }
    }

    /// Map labels (place names, peaks, spot heights), excluding the labels
    /// used as coordinates.
    fn pois(&self) -> Vec<PoiLabel> {
        let obs: Vec<OcrObservation> = self
            .req
            .ocr
            .iter()
            .enumerate()
            .filter(|(i, _)| !self.used_labels.contains(i))
            .map(|(_, o)| o.clone())
            .collect();
        classify_pois(&obs)
    }

    fn execute(&mut self) -> Step<GeorefResponse> {
        self.check_cancel()?;
        let mut methods = Vec::new();
        for m in &self.req.methods {
            let m = match m.trim().to_ascii_lowercase().as_str() {
                "grid" | "labels" => MethodReq::Grid,
                "contours" | "contour" => MethodReq::Contours,
                _ => continue,
            };
            if !methods.contains(&m) {
                methods.push(m);
            }
        }
        if methods.is_empty() {
            return Ok(GeorefResponse::failed("No known method requested: use \"grid\" and/or \"contours\"."));
        }
        let mut weak: Option<GridResult> = None;
        let mut reason: Option<String> = None;
        for &m in &methods {
            match m {
                MethodReq::Grid => {
                    if let Some(g) = self.grid()? {
                        if g.method == GridMethod::Lines && g.rms_m < GRID_MAX_RMS_M {
                            return self.grid_ok(&g);
                        }
                        weak = Some(g);
                    }
                }
                MethodReq::Contours => match self.contours(weak.as_ref())? {
                    Found::Located(r) => return Ok(*r),
                    Found::NotFound(msg) => reason = reason.or(Some(msg)),
                },
            }
        }
        if let Some(g) = &weak {
            return Ok(self.labels_ok(g, methods.contains(&MethodReq::Contours)));
        }
        let msg = reason.unwrap_or_else(|| {
            if self.req.ocr.is_empty() {
                "No text was read from this map, so there are no grid labels to use. Try contour matching or \
                 calibrate manually."
                    .into()
            } else {
                "Couldn't find coordinate grid labels on this map. Try contour matching or calibrate manually.".into()
            }
        });
        Ok(GeorefResponse::failed(msg))
    }

    // --- grid ---------------------------------------------------------------

    fn lines(&mut self) -> &'a [GridLine] {
        let s: &'a Session = self.s;
        let photo = self.req.photo;
        let cell = if photo { &s.photo_lines } else { &s.lines };
        if cell.get().is_none() {
            let img = self.analysed();
            let t = Instant::now();
            let _ = cell.get_or_init(|| if photo { detect_lines_photo(&img) } else { detect_lines(&img) });
            self.time("lines", t);
        }
        cell.get().map(|v| v.as_slice()).unwrap_or(&[])
    }

    /// The raster to analyse (timing the photo normalisation when it runs).
    fn analysed(&mut self) -> Arc<Rgb8> {
        if self.req.photo && self.s.photo_image.get().is_none() {
            let t = Instant::now();
            let img = self.s.analysed(true);
            self.time("normalize", t);
            return img;
        }
        self.s.analysed(self.req.photo)
    }

    fn contour_key(&self) -> ContourKey {
        ContourKey { rgb: self.req.contour_rgb, photo: self.req.photo }
    }

    fn grid(&mut self) -> Step<Option<GridResult>> {
        if self.req.ocr.is_empty() {
            self.diag.insert("grid".into(), json!({"skipped": "no OCR text"}));
            return Ok(None);
        }
        let lines = self.lines();
        self.s.bump_progress(0.1);
        self.check_cancel()?;
        let t = Instant::now();
        let opts = GridOptions { photo: self.req.photo };
        let g = georeference_from_grid_with(&self.s.image, lines, &self.req.ocr, self.gps, &opts);
        self.time("grid", t);
        self.s.bump_progress(0.15);
        let lines_h = lines.iter().filter(|l| l.axis == LineAxis::Horizontal).count();
        let lines_v = lines.len() - lines_h;
        let d = match &g {
            None => json!({"found": false, "lines_h": lines_h, "lines_v": lines_v}),
            Some(g) => json!({
                "found": true,
                "method": g.method,
                "model": if g.projective { "homography" } else { "affine" },
                "rms_m": g.rms_m,
                "n_lat": g.n_lat,
                "n_lon": g.n_lon,
                "n_gcps": g.gcps.len(),
                "lines_h": lines_h,
                "lines_v": lines_v,
            }),
        };
        self.diag.insert("grid".into(), d);
        if let Some(g) = &g {
            self.used_labels = g.used_labels.clone();
        }
        Ok(g)
    }

    /// Grid labels read: ("latitude", "longitude") or ("northing", "easting").
    fn label_axes(&self, g: &GridResult) -> (&'static str, &'static str) {
        let geographic = g.used_labels.iter().filter_map(|&i| self.req.ocr.get(i)).any(|o| {
            matches!(parse_coordinate(&o.text), Some(CoordValue::Lat(_) | CoordValue::Lon(_) | CoordValue::Degrees(_)))
        });
        if geographic || g.used_labels.is_empty() {
            ("latitude", "longitude")
        } else {
            ("northing", "easting")
        }
    }

    fn grid_ok(&mut self, g: &GridResult) -> Step<GeorefResponse> {
        let (w, h) = self.size();
        if self.req.verify {
            self.verify(&g.transform)?;
        }
        let (scale, _) = scale_and_rotation(&g.transform, w as f64 / 2.0, h as f64 / 2.0);
        // Line positions are good to a fraction of a pixel; the GCP fit RMS alone is optimistic.
        let rms = (g.rms_m.powi(2) + (0.75 * scale).powi(2)).sqrt();
        let n = g.n_lat + g.n_lon;
        let label_factor = match n {
            0..=3 => 0.93,
            4 => 0.97,
            _ => 1.0,
        };
        let conf = 0.97 * label_factor * (1.0 - 0.3 * ramp(g.rms_m, 5.0, GRID_MAX_RMS_M));
        let (ya, xa) = self.label_axes(g);
        let msg = format!(
            "Located from {} {ya} and {} {xa} grid label{} ({}).",
            g.n_lat,
            g.n_lon,
            if g.n_lon == 1 { "" } else { "s" },
            plus_minus(rms)
        );
        Ok(self.located(Method::Grid, &g.transform, conf, rms, msg))
    }

    fn labels_ok(&mut self, g: &GridResult, contours_tried: bool) -> GeorefResponse {
        let (w, h) = self.size();
        let (scale, _) = scale_and_rotation(&g.transform, w as f64 / 2.0, h as f64 / 2.0);
        let rms = g.rms_m.max(0.75 * scale);
        let mut conf = 0.5 * (1.0 - 0.5 * ramp(g.rms_m, 30.0, 150.0));
        let mut msg = format!(
            "Roughly located from {} coordinate label{} ({}).",
            g.n_lat + g.n_lon,
            if g.n_lat + g.n_lon == 1 { "" } else { "s" },
            plus_minus(rms)
        );
        if contours_tried {
            conf *= 0.8;
            msg.push_str(" Contour lines couldn't confirm it; check the dot against your surroundings.");
        }
        self.located(Method::Labels, &g.transform, conf, rms, msg)
    }

    /// Check a transform against the terrain: `diagnostics.contour_check`.
    fn verify(&mut self, t: &Homography) -> Step<()> {
        let (w, h) = self.size();
        let key = self.contour_key();
        let approx = if t.is_affine() { t.to_affine() } else { t.local_affine(w as f64 / 2.0, h as f64 / 2.0) };
        let Some(plan) = fine_plan(&approx, w, h) else {
            return Ok(());
        };
        self.need(&plan.tiles, "verify", key)?;
        let prod = self.products(key)?;
        let checked = if t.is_affine() {
            self.refine_one(&prod, key, &plan, &approx, true)
        } else {
            self.evaluate_homography(&prod, &plan, t)
        };
        let check = match checked {
            Some((r, zoom)) => {
                let fit = fit_confidence(&r);
                json!({
                    "fit_confidence": round_dp(fit, 3),
                    "agrees": fit >= ACCEPT,
                    "median_seg_std_m": r.median_seg_std_m,
                    "inlier_fraction": r.inlier_fraction,
                    "explained_variance": r.explained_variance,
                    "interval_snr": r.interval_snr,
                    "contour_interval_m": r.contour_interval_m,
                    "dem_coverage": r.dem_coverage,
                    "n_segments": r.n_segments,
                    "dem_zoom": zoom,
                })
            }
            None => json!({"error": "no terrain data"}),
        };
        self.diag.insert("contour_check".into(), check);
        Ok(())
    }

    // --- DEM, contour products, search, refinement ----------------------------

    /// Stop with need_dem if any of `tiles` is unknown (and start computing
    /// the contour products meanwhile).
    fn need(&mut self, tiles: &[TileId], stage: &str, key: ContourKey) -> Step<()> {
        let mut unknown = lock(&self.s.dem).unknown(tiles);
        if unknown.is_empty() {
            return Ok(());
        }
        unknown.sort();
        unknown.dedup();
        self.s.prefetch_contours(key);
        let zooms: Vec<u8> = {
            let mut z: Vec<u8> = unknown.iter().map(|t| t.z).collect();
            z.dedup();
            z
        };
        let mut r = GeorefResponse::empty(
            Status::NeedDem,
            format!(
                "Need {} terrain tile{} (zoom {}).",
                unknown.len(),
                if unknown.len() == 1 { "" } else { "s" },
                zooms.iter().map(|z| z.to_string()).collect::<Vec<_>>().join(", ")
            ),
        );
        r.diagnostics = json!({"stage": stage, "tiles_wanted": tiles.len(), "tiles_unknown": unknown.len()});
        r.need_tiles = unknown;
        Err(Box::new(r))
    }

    fn products(&mut self, key: ContourKey) -> Step<Arc<ContourProducts>> {
        self.check_cancel()?;
        let cell = self.s.contour_cell(key);
        let fresh = cell.get().is_none();
        let img = if fresh { Some(self.analysed()) } else { None };
        let t = Instant::now();
        let p = cell
            .get_or_init(|| {
                let img = img.unwrap_or_else(|| self.s.analysed(key.photo));
                Arc::new(ContourProducts::compute_with(&img, key.rgb, key.photo))
            })
            .clone();
        if fresh {
            // Includes waiting for a background computation started by need_dem.
            self.time("contour_prep", t);
        }
        self.diag.insert(
            "contours".into(),
            json!({
                "model": p.model,
                "user_colour": key.rgb,
                "mask_fraction": round_dp(p.mask_fraction, 5),
                "n_segments": p.samples.n_segs,
                "n_samples": p.samples.len(),
                "field_cells": [p.field.w, p.field.h],
                "prep_ms": {"mask": round_dp(p.timings_ms[0], 1), "skeleton": round_dp(p.timings_ms[1], 1),
                            "field": round_dp(p.timings_ms[2], 1)},
            }),
        );
        self.s.bump_progress(0.35);
        self.check_cancel()?;
        Ok(p)
    }

    /// Ground metres per pixel suggested by an OCR'd representative fraction
    /// ("1:25 000") and the raster dpi.
    fn scale_prior(&self) -> Option<(f64, f64)> {
        let dpi = self.req.dpi.filter(|d| d.is_finite() && (10.0..=4800.0).contains(d))?;
        let mut counts: Vec<(f64, usize)> = Vec::new();
        for o in &self.req.ocr {
            if let Some(s) = parse_scale(&o.text) {
                match counts.iter_mut().find(|c| c.0 == s) {
                    Some(c) => c.1 += 1,
                    None => counts.push((s, 1)),
                }
            }
        }
        let denom = counts.iter().fold(None::<(f64, usize)>, |b, &c| match b {
            Some(b) if b.1 >= c.1 => Some(b),
            _ => Some(c),
        })?;
        Some((denom.0, denom.0 * 0.0254 / dpi))
    }

    fn search_params(&mut self, gps: LatLon, init: Option<&GridResult>) -> SearchParams {
        let (w, h) = self.size();
        let cos_lat = gps.lat.to_radians().cos();
        // Maps drawn in Web Mercator print at the nominal scale in Mercator
        // metres, so the ground scale can be up to cos(lat) smaller.
        let (lo, hi, source) = if let Some((denom, prior)) = self.scale_prior() {
            self.diag.insert("scale_denominator".into(), json!(denom));
            (0.75 * prior * cos_lat, 1.25 * prior, "scale_label")
        } else if let Some(g) = init {
            let (s, _) = scale_and_rotation(&g.transform, w as f64 / 2.0, h as f64 / 2.0);
            if s.is_finite() && s > 0.0 {
                (s / 1.25, s * 1.25, "labels")
            } else {
                (DEFAULT_SCALE_RANGE.0, DEFAULT_SCALE_RANGE.1, "default")
            }
        } else {
            (DEFAULT_SCALE_RANGE.0, DEFAULT_SCALE_RANGE.1, "default")
        };
        let (lo, hi) = (lo.clamp(PLAUSIBLE_SCALE.0, PLAUSIBLE_SCALE.1), hi.clamp(PLAUSIBLE_SCALE.0, PLAUSIBLE_SCALE.1));
        self.diag
            .insert("scale_range".into(), json!({"min": round_dp(lo, 3), "max": round_dp(hi, 3), "source": source}));
        SearchParams {
            center_merc: latlon_to_merc(gps.lat, gps.lon),
            search_radius_m: self.radius_m,
            scale_min: lo,
            scale_max: hi,
            rotations_deg: if self.req.allow_rotation { rotation_sweep(10.0) } else { vec![0.0] },
            top_k: MAX_REFINE_CANDIDATES,
        }
    }

    fn search(
        &mut self,
        prod: &ContourProducts,
        key: ContourKey,
        params: &SearchParams,
        coarse_box: &MercBox,
        tiles_present: usize,
    ) -> Step<Arc<Vec<Candidate>>> {
        let (w, h) = self.size();
        let skey = SearchKey {
            contour: key,
            center: [params.center_merc.0.round() as i64, params.center_merc.1.round() as i64],
            radius_m: params.search_radius_m.round() as i64,
            scale: [params.scale_min.to_bits(), params.scale_max.to_bits()],
            rotate: params.rotations_deg.len() > 1,
            tiles_present,
        };
        let cached = lock(&self.s.searches).iter().find(|(k, _)| *k == skey).map(|(_, c)| c.clone());
        if let Some(c) = cached {
            self.s.bump_progress(0.8);
            return Ok(c);
        }
        let t = Instant::now();
        let dem = lock(&self.s.dem).mosaic(COARSE_ZOOM, coarse_box);
        self.time("coarse_dem", t);
        let t = Instant::now();
        let s = self.s;
        let cands =
            coarse_search(&prod.field, &dem, w, h, params, &|| s.cancelled(), &|f| s.bump_progress(0.35 + 0.45 * f));
        self.time("coarse_search", t);
        // A cancelled search returns partial results: never cache them.
        self.check_cancel()?;
        let cands = Arc::new(cands);
        let mut cache = lock(&self.s.searches);
        cache.push((skey, cands.clone()));
        if cache.len() > SEARCH_CACHE {
            cache.remove(0);
        }
        Ok(cands)
    }

    /// Refine (or only evaluate) `t` on the best DEM for `plan`; cached.
    fn refine_one(
        &mut self,
        prod: &ContourProducts,
        key: ContourKey,
        plan: &FinePlan,
        t: &Affine,
        evaluate_only: bool,
    ) -> Option<(RefineResult, u8)> {
        let tm = Instant::now();
        let (dem, zoom) = self.s.fine_dem(plan)?;
        let rkey = RefineKey {
            contour: key,
            transform: [t.a, t.b, t.c, t.d, t.e, t.f].map(f64::to_bits),
            zoom,
            dem_valid: dem.valid_fraction().to_bits(),
            evaluate_only,
        };
        if let Some(r) = lock(&self.s.refinements).get(&rkey).cloned() {
            return Some((r, zoom));
        }
        let prep = PreparedDem::new_cropped(&dem, &plan.bbox);
        self.time("fine_dem", tm);
        let tm = Instant::now();
        let r = if evaluate_only {
            evaluate_prepared(&prod.samples, &prep, t)
        } else {
            refine_prepared_with(&prod.samples, &prep, t, &RefineOptions { photo: key.photo })
        };
        self.time(if evaluate_only { "verify" } else { "refine" }, tm);
        lock(&self.s.refinements).insert(rkey, r.clone());
        Some((r, zoom))
    }

    /// Evaluate a homography (photo-mode grid fit) on the best DEM for `plan`.
    fn evaluate_homography(
        &mut self,
        prod: &ContourProducts,
        plan: &FinePlan,
        t: &Homography,
    ) -> Option<(RefineResult, u8)> {
        let tm = Instant::now();
        let (dem, zoom) = self.s.fine_dem(plan)?;
        let prep = PreparedDem::new_cropped(&dem, &plan.bbox);
        let r = evaluate_prepared_homography(&prod.samples, &prep, t);
        self.time("verify", tm);
        Some((r, zoom))
    }

    /// Refine every candidate on its own fine DEM (one need_dem for all tiles).
    fn refine_candidates(
        &mut self,
        prod: &ContourProducts,
        key: ContourKey,
        cands: &[Candidate],
    ) -> Step<Vec<(Candidate, RefineResult, u8)>> {
        let (w, h) = self.size();
        let plans: Vec<(Candidate, FinePlan)> =
            cands.iter().filter_map(|c| fine_plan(&c.transform, w, h).map(|p| (c.clone(), p))).collect();
        let mut tiles: Vec<TileId> = plans.iter().flat_map(|(_, p)| p.tiles.iter().copied()).collect();
        tiles.sort();
        tiles.dedup();
        self.need(&tiles, "refine", key)?;
        let mut out = Vec::new();
        let n = plans.len().max(1) as f32;
        for (i, (c, plan)) in plans.iter().enumerate() {
            self.check_cancel()?;
            if let Some((r, zoom)) = self.refine_one(prod, key, plan, &c.transform, false) {
                out.push((c.clone(), r, zoom));
            }
            self.s.bump_progress(0.8 + 0.2 * (i + 1) as f32 / n);
        }
        Ok(out)
    }

    // --- contours -------------------------------------------------------------

    fn contours(&mut self, init: Option<&GridResult>) -> Step<Found> {
        let (w, h) = self.size();
        let key = self.contour_key();
        let no_contours = "No contour lines found on this map. Pick the contour colour, or calibrate manually.";
        let mut labelled: Option<(Candidate, RefineResult, u8)> = None;

        // 1. A labels-only fit: refine it directly (no GPS or search needed).
        if let Some((t, plan, rms)) = init
            .filter(|g| g.transform.is_affine())
            .and_then(|g| fine_plan(&g.transform.to_affine(), w, h).map(|p| (g.transform.to_affine(), p, g.rms_m)))
        {
            self.need(&plan.tiles, "labels", key)?;
            let prod = self.products(key)?;
            if prod.samples.n_segs < MIN_SEGMENTS {
                return Ok(Found::NotFound(no_contours.into()));
            }
            if let Some((r, zoom)) = self.refine_one(&prod, key, &plan, &t, false) {
                let fit = fit_confidence(&r);
                let agree = transforms_agree(&t, &r.transform, w, h, rms);
                let conf = if agree { fit } else { 0.5 * fit };
                self.diag.insert(
                    "labels_refine".into(),
                    json!({"fit_confidence": round_dp(fit, 3), "agrees_with_labels": agree}),
                );
                if conf >= CONFIDENT {
                    return Ok(Found::Located(Box::new(self.contour_ok(&r, conf, zoom, true))));
                }
                if agree {
                    labelled = Some((labels_candidate(&t, w, h), r, zoom));
                }
            }
        }

        let Some(gps) = self.gps else {
            return Ok(Found::NotFound(
                "No coordinate labels found on this map, and contour matching needs your location. Turn on \
                 location services or calibrate manually."
                    .into(),
            ));
        };

        // 2. Coarse search around the GPS position.
        let params = self.search_params(gps, init);
        let coarse_box = required_dem_box(&params, w, h);
        let coarse_tiles = tiles_covering(COARSE_ZOOM, &coarse_box);
        self.need(&coarse_tiles, "coarse", key)?;
        let present = {
            let store = lock(&self.s.dem);
            coarse_tiles.iter().filter(|t| store.present(t)).count()
        };
        if present == 0 {
            return Ok(Found::NotFound(
                "Terrain data for your area couldn't be downloaded, so contour lines can't be matched. Try again \
                 with a connection, or calibrate manually."
                    .into(),
            ));
        }
        let prod = self.products(key)?;
        if prod.samples.n_segs < MIN_SEGMENTS {
            return Ok(Found::NotFound(no_contours.into()));
        }
        let cands = self.search(&prod, key, &params, &coarse_box, present)?;
        let gps_merc = latlon_to_merc(gps.lat, gps.lon);
        let cdiag: Vec<Value> = cands
            .iter()
            .map(|c| {
                let (mx, my) = c.center_merc(w, h);
                let (lat, lon) = merc_to_latlon(mx, my);
                let km = ((mx - gps_merc.0).hypot(my - gps_merc.1) * gps.lat.to_radians().cos()) / 1000.0;
                json!({
                    "score": c.score,
                    "peak_ratio": c.peak_ratio,
                    "scale_m_per_px": round_dp(c.scale_m_per_px, 4),
                    "rotation_deg": round_dp(c.rotation_deg, 2),
                    "center": [lat, lon],
                    "distance_km": round_dp(km, 2),
                })
            })
            .collect();
        self.diag.insert("candidates".into(), Value::Array(cdiag));
        let not_found = format!(
            "Couldn't match this map's contours to terrain within {:.0} km of you. Try calibrating manually or \
             pick the contour colour.",
            self.radius_m / 1000.0
        );

        // 3. Refinement: the best candidate alone when it stands out, else the top few.
        let n = cands.len().min(MAX_REFINE_CANDIDATES);
        if n == 0 && labelled.is_none() {
            return Ok(Found::NotFound(not_found));
        }
        if n > 0 && cands[0].peak_ratio >= DISTINCT_PEAK_RATIO {
            let first = self.refine_candidates(&prod, key, &cands[..1])?;
            if let Some((c, r, zoom)) = first.first() {
                let conf = confidence(c, r);
                if conf >= CONFIDENT {
                    self.diag.insert("selected".into(), json!({"candidate": 0, "refined": 1}));
                    return Ok(Found::Located(Box::new(self.contour_ok(r, conf, *zoom, false))));
                }
            }
        }
        let mut results = self.refine_candidates(&prod, key, &cands[..n])?;
        let n_search = results.len();
        if let Some(l) = labelled {
            results.push(l);
        }
        let pairs: Vec<(Candidate, RefineResult)> = results.iter().map(|(c, r, _)| (c.clone(), r.clone())).collect();
        let Some((best, conf)) = select_refined(&pairs) else {
            return Ok(Found::NotFound(not_found));
        };
        let fits: Vec<f64> = pairs.iter().map(|(_, r)| round_dp(fit_confidence(r), 3)).collect();
        self.diag.insert(
            "selected".into(),
            json!({"candidate": best, "refined": results.len(), "fit_confidences": fits,
                   "confidence": round_dp(conf, 3)}),
        );
        if conf < ACCEPT {
            self.diag.insert("best_confidence".into(), json!(round_dp(conf, 3)));
            return Ok(Found::NotFound(not_found));
        }
        let (_, r, zoom) = &results[best];
        Ok(Found::Located(Box::new(self.contour_ok(r, conf, *zoom, best >= n_search))))
    }

    fn contour_ok(&mut self, r: &RefineResult, conf: f64, zoom: u8, from_labels: bool) -> GeorefResponse {
        let (w, h) = self.size();
        let t = r.homography.unwrap_or_else(|| r.transform.to_homography());
        let (_, cy) = t.apply(w as f64 / 2.0, h as f64 / 2.0);
        let (lat, _) = merc_to_latlon(0.0, cy);
        let rms = contour_rms(r, zoom, lat);
        self.diag.insert(
            "refined".into(),
            json!({
                "fit_confidence": round_dp(fit_confidence(r), 3),
                "cost_m": r.cost_m,
                "median_seg_std_m": r.median_seg_std_m,
                "inlier_fraction": r.inlier_fraction,
                "explained_variance": r.explained_variance,
                "interval_snr": r.interval_snr,
                "contour_interval_m": r.contour_interval_m,
                "relief_m": r.relief_m,
                "dem_coverage": r.dem_coverage,
                "n_segments": r.n_segments,
                "n_samples": r.n_samples,
                "affine": r.affine,
                "projective": r.homography.is_some(),
                "iterations": r.iterations,
                "converged": r.converged,
                "dem_zoom": zoom,
                "source": if from_labels { "labels" } else { "search" },
            }),
        );
        let mut msg = if from_labels {
            format!("Located from coordinate labels and matched contour lines to terrain ({}).", plus_minus(rms))
        } else {
            format!("Matched contour lines to terrain ({}).", plus_minus(rms))
        };
        if conf < CONFIDENT {
            msg.push_str(" Low confidence: check the dot against your surroundings.");
        }
        self.located(Method::Contours, &t, conf, rms, msg)
    }

    fn located(&mut self, method: Method, t: &Homography, conf: f64, rms: f64, message: String) -> GeorefResponse {
        let (w, h) = self.size();
        let (scale, rotation) = scale_and_rotation(t, w as f64 / 2.0, h as f64 / 2.0);
        let corners = [(0.0, 0.0), (w as f64, 0.0), (w as f64, h as f64), (0.0, h as f64)].map(|(x, y)| {
            let (mx, my) = t.apply(x, y);
            let (lat, lon) = merc_to_latlon(mx, my);
            [lat, lon]
        });
        let mut r = GeorefResponse::empty(Status::Ok, message);
        r.method = Some(method);
        r.pixel_to_mercator = Some(t.m);
        r.confidence = Some(round_dp(conf.clamp(0.0, 1.0), 3));
        r.rms_m = Some(round_dp(rms, 1));
        r.scale_m_per_px = Some(round_dp(scale, 4));
        r.rotation_deg = Some(round_dp(rotation, 2) % 360.0);
        r.corners = Some(corners);
        r
    }
}

/// Candidate standing for a labels-only fit in `select_refined`.
fn labels_candidate(t: &Affine, w: usize, h: usize) -> Candidate {
    let (scale, rotation) = scale_and_rotation(&t.to_homography(), w as f64 / 2.0, h as f64 / 2.0);
    Candidate {
        transform: *t,
        score: 0.0,
        peak_ratio: LABELS_PEAK_RATIO,
        scale_m_per_px: scale,
        rotation_deg: rotation,
    }
}

/// Do two transforms place the image corners within the labels' uncertainty
/// (plus 2 % of the map diagonal) of each other?
fn transforms_agree(a: &Affine, b: &Affine, w: usize, h: usize, rms_m: f64) -> bool {
    let (w, h) = (w as f64, h as f64);
    let (_, cy) = a.apply(w / 2.0, h / 2.0);
    let cos_lat = merc_to_latlon(0.0, cy).0.to_radians().cos();
    let diag_m = (w.hypot(h)) * a.mean_scale() * cos_lat;
    let tol = (3.0 * rms_m).max(100.0) + 0.02 * diag_m;
    [(0.0, 0.0), (w, 0.0), (w, h), (0.0, h)].iter().all(|&(x, y)| {
        let (p, q) = (a.apply(x, y), b.apply(x, y));
        (p.0 - q.0).hypot(p.1 - q.1) * cos_lat <= tol
    })
}

/// Honest position-error estimate (ground metres) of a contour fit: half a
/// DEM cell plus twice the within-segment elevation noise, inflated when the
/// fit is uncertain. Grootkloof on zoom 13: ~13 m (measured max error 11.6 m).
fn contour_rms(r: &RefineResult, zoom: u8, lat: f64) -> f64 {
    let cell_m = resolution(zoom) * lat.to_radians().cos();
    let seg = if r.median_seg_std_m.is_finite() { r.median_seg_std_m } else { 50.0 };
    let base = (0.5 * cell_m + 2.0 * seg).clamp(10.0, 200.0);
    (base * (1.0 + 2.0 * (1.0 - fit_confidence(r)))).min(250.0)
}

/// Local ground scale (m/px) and map-up bearing (degrees) of a pixel →
/// Mercator homography at pixel (x, y).
pub fn scale_and_rotation(t: &Homography, x: f64, y: f64) -> (f64, f64) {
    let a = local_affine(t, x, y);
    let (_, my) = t.apply(x, y);
    let (lat, _) = merc_to_latlon(0.0, my);
    (a.mean_scale() * lat.to_radians().cos(), a.map_up_bearing_deg())
}

/// First-order (affine) approximation of a homography at pixel (x, y).
pub fn local_affine(t: &Homography, x: f64, y: f64) -> Affine {
    t.local_affine(x, y)
}

/// Smooth ramp 0 → 1 between `lo` and `hi`.
fn ramp(x: f64, lo: f64, hi: f64) -> f64 {
    let t = ((x - lo) / (hi - lo)).clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

/// Round to `decimals` places (dividing by a power of ten keeps the JSON
/// short: 11.7, not 11.700000000000001).
fn round_dp(v: f64, decimals: i32) -> f64 {
    let k = 10f64.powi(decimals);
    if v.is_finite() {
        (v * k).round() / k
    } else {
        v
    }
}

/// "±2 m" style uncertainty text.
fn plus_minus(m: f64) -> String {
    if m.is_finite() {
        format!("±{:.0} m", m.max(1.0))
    } else {
        "± unknown".into()
    }
}

fn ms_since(t: Instant) -> f64 {
    t.elapsed().as_secs_f64() * 1e3
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn request_defaults_nulls_and_unknown_fields() {
        let r: GeorefRequest = serde_json::from_str("{}").unwrap();
        assert_eq!(r, GeorefRequest::default());
        let r: GeorefRequest = serde_json::from_str(
            r#"{"gps": null, "ocr": null, "search_radius_m": null, "methods": null, "verify": null,
                "allow_rotation": null, "contour_rgb": null, "dpi": null, "photo": null, "future_field": [1, 2]}"#,
        )
        .unwrap();
        assert_eq!(r, GeorefRequest::default());
        let r: GeorefRequest = serde_json::from_str(
            r#"{"gps": {"lat": -25.8, "lon": 27.4, "accuracy_m": 8}, "dpi": 300, "contour_rgb": [168, 152.4, 300],
                "search_radius_m": 15000, "allow_rotation": true, "methods": ["contours"], "verify": true,
                "photo": true, "ocr": [{"text": "1:25 000", "bbox": [1, 2, 3, 4]}]}"#,
        )
        .unwrap();
        assert!(r.photo);
        assert_eq!(r.gps.unwrap().accuracy_m, Some(8.0));
        assert_eq!(r.contour_rgb, Some([168, 152, 255]));
        assert_eq!(r.ocr[0].confidence, 0.5);
        assert!(r.allow_rotation && r.verify && r.methods == vec!["contours".to_string()]);
        // Round trip.
        let back: GeorefRequest = serde_json::from_str(&serde_json::to_string(&r).unwrap()).unwrap();
        assert_eq!(back, r);
        assert!(serde_json::from_str::<GeorefRequest>(r#"{"contour_rgb": [1, 2]}"#).is_err());
        assert!(serde_json::from_str::<GeorefRequest>(r#"{"gps": {"lat": 1}}"#).is_err());
    }

    #[test]
    fn response_serialises_snake_case() {
        let mut r = GeorefResponse::empty(Status::NeedDem, "x".into());
        r.need_tiles = vec![TileId { z: 11, x: 1179, y: 1175 }];
        r.method = Some(Method::Contours);
        r.confidence = Some(f64::NAN);
        let v: Value = serde_json::to_value(&r).unwrap();
        assert_eq!(v["status"], "need_dem");
        assert_eq!(v["method"], "contours");
        assert_eq!(v["need_tiles"][0]["x"], 1179);
        assert!(v["confidence"].is_null());
        assert!(v["pixel_to_mercator"].is_null());
    }

    #[test]
    fn local_affine_of_affine_is_exact() {
        let t = Affine::pixel_to_merc_similarity(2.1, 0.1, 100.0, 200.0, 3.0e6, -2.9e6);
        let a = local_affine(&t.to_homography(), 37.0, 911.0);
        for (p, q) in [(a.a, t.a), (a.b, t.b), (a.c, t.c), (a.d, t.d), (a.e, t.e), (a.f, t.f)] {
            assert!((p - q).abs() < 1e-6 * q.abs().max(1.0), "{p} vs {q}");
        }
        let (s, rot) = scale_and_rotation(&t.to_homography(), 100.0, 200.0);
        let (lat, _) = merc_to_latlon(3.0e6, -2.9e6);
        assert!((s - 2.1 * lat.to_radians().cos()).abs() < 1e-9);
        assert!((rot - 0.1f64.to_degrees()).abs() < 1e-9);
    }

    #[test]
    fn tile_count_matches_listing() {
        let b = MercBox::around(3.05e6, -2.98e6, 12_000.0);
        for z in [8u8, 11, 13] {
            assert_eq!(tile_count(z, &b), tiles_covering(z, &b).len() as u64);
        }
    }

    #[test]
    fn fine_plan_limits_tiles() {
        // 1:25 000 sheet at 300 dpi: zoom 13.
        let t = Affine::pixel_to_merc_similarity(2.117, 0.0, 1753.0, 2480.0, 3.05e6, -2.98e6);
        let p = fine_plan(&t, 3507, 4960).unwrap();
        assert_eq!(p.zoom, 13);
        assert!(p.tiles.len() as u64 <= MAX_FINE_TILES);
        // 1:250 000: a coarser zoom.
        let t = Affine::pixel_to_merc_similarity(21.17, 0.0, 1753.0, 2480.0, 3.05e6, -2.98e6);
        let p = fine_plan(&t, 3507, 4960).unwrap();
        assert!(p.zoom < 13 && p.tiles.len() as u64 <= MAX_FINE_TILES);
        // Nonsense.
        let t = Affine::pixel_to_merc_similarity(1e5, 0.0, 0.0, 0.0, 0.0, 0.0);
        assert!(fine_plan(&t, 100, 100).is_none());
    }

    #[test]
    fn invalid_requests_fail_softly() {
        let s = Session::new(Rgb8::from_rgb(8, 8, vec![255; 8 * 8 * 3]).unwrap());
        let v: Value = serde_json::from_str(&s.georeference_json("{not json")).unwrap();
        assert_eq!(v["status"], "failed");
        assert!(v["message"].as_str().unwrap().starts_with("Invalid request"));
        let v: Value = serde_json::from_str(&s.georeference_json(r#"{"methods": ["magic"]}"#)).unwrap();
        assert_eq!(v["status"], "failed");
        assert!(s.add_dem_tile(TileId { z: 2, x: 4, y: 0 }, &[]).is_err());
        assert!(s.add_dem_tile(TileId { z: 2, x: 1, y: 0 }, b"not a png").is_err());
        assert!(lock(&s.dem).known(&TileId { z: 2, x: 1, y: 0 }));
    }

    #[test]
    fn control_points_json() {
        let v: Value = serde_json::from_str(&fit_control_points_json(
            r#"{"points": [{"x": 0, "y": 0, "lat": -25.8, "lon": 27.3},
                           {"x": 1000, "y": 1000, "lat": -25.82, "lon": 27.32}], "model": null}"#,
        ))
        .unwrap();
        assert_eq!(v["status"], "ok", "{v}");
        assert_eq!(v["model"], "similarity");
        assert_eq!(v["pixel_to_mercator"].as_array().unwrap().len(), 9);
        let v: Value = serde_json::from_str(&fit_control_points_json(r#"{"points": []}"#)).unwrap();
        assert_eq!(v["status"], "failed");
        let v: Value = serde_json::from_str(&fit_control_points_json("[]")).unwrap();
        assert_eq!(v["status"], "failed");
    }
}
