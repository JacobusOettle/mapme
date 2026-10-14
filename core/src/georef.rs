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
//!   first. Otherwise the areas to search are, in order: the hints (request
//!   hints and the map's coordinate labels, `hints.rs`), the near disc around
//!   the GPS position (20 km, then up to 50 km), and beyond 50 km the wide
//!   search (`georef/wide_search.rs`). A hint or the near disc gets the
//!   coarse FFT search on zoom-11 tiles, then its best candidates are refined
//!   on zoom-13 tiles (the best alone first when it clearly stands out) and
//!   the most confident one wins; the wide search works chunk by chunk on
//!   zoom-10/9 tiles with zoom-11 re-checks. Every need_dem answer asks for
//!   at most `max_tiles_per_round` tiles; the response's `search` reports
//!   the progress and `hint_queries` the names worth geocoding.
//!
//! A request with `photo: true` (a photo of a paper map, flattened by its
//! page borders) analyses an illumination- and white-balance-normalised copy
//! of the raster (`contour::normalize_illumination`), traces grid lines
//! (bent where the paper does not lie flat) against their local background,
//! extracts contours as luminance valleys of contour colour
//! (`contour::photo_mask`), and lets the grid fit and the contour refinement
//! end in a homography, which absorbs the perspective and aspect error the
//! flattening left. On top of that homography a photo may get a `warp`
//! (`warp.rs`) for paper that does not lie flat: fitted to the identified
//! grid lines (the grid path then also asks for the zoom-13 DEM under the
//! sheet and adds the contours, when contours were requested) or to the
//! contours, and kept only if the data left out of the fit agree with it.
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
use crate::hints::{
    ground_distance_m, hint_queries, label_band, label_hints, order_hints, HintSource, LabelBand, SearchHint,
    DEFAULT_HINT_RADIUS_M, HINT_RADIUS_RANGE_M,
};
use crate::image::Rgb8;
use crate::labels::{
    classify_pois, height_unit, parse_coordinate, parse_scale, CoordValue, OcrObservation, PoiKind, PoiLabel, FOOT_M,
};
use crate::refine::{
    confidence, evaluate_prepared, evaluate_prepared_homography, fit_confidence, fit_evidence, fit_quality,
    refine_dem_box, refine_prepared_with, select_refined, PreparedDem, RefineOptions, RefineResult,
};
use crate::scalebar::{find_scale_bar, ScaleBar};
use crate::search::{
    coarse_search, map_field, required_dem_box_capped, rotation_sweep, Candidate, MapField, SearchParams,
};
use crate::skeleton::{contour_samples, default_sampling, ContourSamples};
use crate::warp::{fit_warp, ContourInput, GridLines, Warp, WarpFit, WarpInput};
use crate::wide::{default_scale_range, near_zoom, scale_bands, ScaleBand, FINE_MAX_SCALE};
use serde::{Deserialize, Serialize};
use serde_json::{json, Map, Value};
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, OnceLock};
use std::time::Instant;

/// Grid line fits with a larger RMS (ground metres) only seed the contour stage.
pub const GRID_MAX_RMS_M: f64 = 25.0;
/// Photos: paper that does not lie flat (folds, curl) bends the grid, so a
/// homography through the grid intersections of a good fit is off by tens
/// of metres (`fixtures/grootkloof/photo`, folded in four: 25 m RMS).
pub const PHOTO_GRID_MAX_RMS_M: f64 = 60.0;
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
/// A scale bar's ground m/px (`scalebar`) is searched within this factor
/// either way: its label is good to ~0.2 %, its measurement to a few pixels,
/// and a PDF page drawn at another aspect ratio (Gaia: 1.6 %) or a photo's
/// flattening skews it.
const SCALE_BAR_TOLERANCE: f64 = 1.15;
/// Candidates of a coarser scale band (`wide::scale_bands`) are refined only
/// from this peak ratio (`refine::confidence` gives search evidence from 1.15).
const COARSE_BAND_MIN_RATIO: f32 = 1.15;
/// Search evidence credited to a labels-only fit that the contours agree with.
const LABELS_PEAK_RATIO: f32 = 1.6;
/// Fewer contour segments than this: the map has no usable contours.
pub const MIN_SEGMENTS: usize = 10;
/// DEM zoom of the coarse search up to 60° of latitude (coarser beyond:
/// `wide::near_zoom`).
pub const COARSE_ZOOM: u8 = crate::wide::NEAR_ZOOM;
pub const FINE_ZOOM: u8 = 13;
/// One refinement may need at most this many tiles; coarser zooms are used beyond.
const MAX_FINE_TILES: u64 = 36;
/// Scale range (ground m/px) searched without a scale prior: from 1:5 000
/// plans at 300 dpi to large-area exports and 1:250 000 sheets, in bands
/// (`wide::scale_bands`): up to `wide::FINE_MAX_SCALE` on zoom-11 tiles,
/// beyond it on coarser ones.
pub const DEFAULT_SCALE_RANGE: (f64, f64) = (0.4, 50.0);
/// Search radius around the GPS fix without one in the request (ground metres).
pub const DEFAULT_RADIUS_M: f64 = 50_000.0;
/// Allowed search radius (ground metres).
pub const RADIUS_RANGE_M: (f64, f64) = (1_000.0, 2_000_000.0);
/// The near area: the disc around the fix searched at zoom 11 (coarser past
/// 60° of latitude: `wide::near_zoom`; the whole
/// search up to this radius); beyond it the wide search takes over.
pub const NEAR_RADIUS_M: f64 = 50_000.0;
/// The near disc is searched out to this radius first (a third of the
/// coarse search's cost: the map is usually close), then to `NEAR_RADIUS_M`.
pub const NEAR_FIRST_RADIUS_M: f64 = 20_000.0;
/// Most tiles one need_dem answer asks for, without one in the request.
pub const DEFAULT_MAX_TILES_PER_ROUND: usize = 64;
/// Allowed `max_tiles_per_round`.
pub const MAX_TILES_RANGE: (usize, usize) = (1, 4096);
/// Ground scales (m/px) outside this range are not plausible map transforms.
const PLAUSIBLE_SCALE: (f64, f64) = (0.02, 500.0);
/// A mosaic counts as complete when this fraction of it has elevations.
const COMPLETE_DEM: f64 = 0.98;
/// Confidence of a labels-only fit (below 0.5: the app asks to check it).
const LABELS_MAX_CONFIDENCE: f64 = 0.45;
/// Confidence of a grid fit with one axis from a single label that the
/// terrain could not confirm.
const LONE_LABEL_MAX_CONFIDENCE: f64 = 0.45;
/// The terrain rules a one-label grid fit out only with this much to judge
/// by (`refine::fit_evidence`)…
const LONE_MIN_EVIDENCE: f64 = 0.5;
/// …and a fit quality (`refine::fit_quality`) under this.
const LONE_REJECT_QUALITY: f64 = 0.5;
/// A spot height further than this (metres) outside the DEM elevations around
/// its position is a misread.
const SPOT_HEIGHT_DEM_TOLERANCE_M: f64 = 150.0;

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
    /// Candidate areas searched before the area around the GPS fix.
    pub hints: Vec<SearchHint>,
    /// Most tiles one need_dem answer asks for.
    pub max_tiles_per_round: usize,
}

impl Default for GeorefRequest {
    fn default() -> Self {
        GeorefRequest {
            gps: None,
            ocr: Vec::new(),
            dpi: None,
            contour_rgb: None,
            search_radius_m: DEFAULT_RADIUS_M,
            allow_rotation: false,
            methods: vec!["grid".into(), "contours".into()],
            verify: false,
            photo: false,
            hints: Vec::new(),
            max_tiles_per_round: DEFAULT_MAX_TILES_PER_ROUND,
        }
    }
}

/// A hint as sent: everything but `lat` and `lon` may be missing or null.
#[derive(Deserialize, Default)]
#[serde(default)]
struct RawHint {
    lat: Option<f64>,
    lon: Option<f64>,
    radius_m: Option<f64>,
    source: Option<String>,
    name: Option<String>,
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
    hints: Option<Vec<RawHint>>,
    max_tiles_per_round: Option<f64>,
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
        let mut hints = Vec::new();
        for (i, h) in r.hints.unwrap_or_default().into_iter().enumerate() {
            let (Some(lat), Some(lon)) = (h.lat, h.lon) else {
                return Err(format!("hints[{i}] needs lat and lon"));
            };
            hints.push(SearchHint {
                lat,
                lon,
                radius_m: h.radius_m.filter(|r| r.is_finite()).unwrap_or(DEFAULT_HINT_RADIUS_M),
                source: h.source.as_deref().map_or(HintSource::Place, HintSource::parse),
                name: h.name.filter(|n| !n.trim().is_empty()),
            });
        }
        let max_tiles = match r.max_tiles_per_round {
            Some(v) if v.is_finite() => v.round().clamp(MAX_TILES_RANGE.0 as f64, MAX_TILES_RANGE.1 as f64) as usize,
            _ => d.max_tiles_per_round,
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
            hints,
            max_tiles_per_round: max_tiles,
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
    /// Photo mode: smooth correction for paper that does not lie flat, on
    /// top of `pixel_to_mercator` (absent otherwise; see `warp.rs`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub warp: Option<Warp>,
    /// Names on the map worth geocoding into `hints` (every response to a
    /// request with OCR text; see `hints::hint_queries`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hint_queries: Option<Vec<String>>,
    /// Where the contour search stands (whenever it ran).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub search: Option<SearchProgress>,
}

/// Stage of the contour search.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SearchStage {
    /// Searching the hint areas.
    Hints,
    /// Searching the disc around the GPS fix at zoom 11.
    Near,
    /// Searching the rest of the radius chunk by chunk.
    Wide,
    /// Checking candidates (zoom-11 re-check, zoom-13 refinement).
    Refine,
    /// Finished (located or not).
    Done,
}

/// Progress of the contour search, for the app's progress UI ("Searched
/// 320 of 1 000 km").
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SearchProgress {
    /// The search radius (ground metres, after clamping).
    pub radius_m: f64,
    /// Radius around the fix searched completely so far.
    pub searched_radius_m: f64,
    /// DEM zoom of the current (or last) coarse search.
    pub coarse_zoom: u8,
    /// Coarse terrain tiles the whole search plans to use (hints, near
    /// area, wide chunks; candidate checks come on top).
    pub tiles_planned: usize,
    /// Of those, tiles supplied (or marked missing) so far.
    pub tiles_fetched: usize,
    /// Hint areas searched so far, and planned.
    pub hints_tried: usize,
    pub hints_planned: usize,
    pub stage: SearchStage,
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
            warp: None,
            hint_queries: None,
            search: None,
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
    /// (`normalize_illumination`) and the mask is `photo_mask`'s.
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

/// Refinement starts this close (Mercator metres at every image corner, or
/// two map pixels) converge to the same fit: the refinement scans ±450 m
/// around either on its coarsest level.
const REFINE_REUSE_M: f64 = 100.0;

/// Whether refining from `b` would repeat the refinement from `a`.
fn starts_agree(a: &Affine, b: &Affine, w: usize, h: usize) -> bool {
    let tol = REFINE_REUSE_M.max(2.0 * b.mean_scale());
    [(0.0, 0.0), (w as f64, 0.0), (w as f64, h as f64), (0.0, h as f64)].iter().all(|&(x, y)| {
        let (p, q) = (a.apply(x, y), b.apply(x, y));
        (p.0 - q.0).hypot(p.1 - q.1) <= tol
    })
}

fn affine_from_bits(m: &[u64; 6]) -> Affine {
    let [a, b, c, d, e, f] = m.map(f64::from_bits);
    Affine { a, b, c, d, e, f }
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
    /// The wide search's plan and the candidates of its searched chunks.
    wide: Mutex<Option<wide_search::WideCache>>,
    /// Zoom-11 re-checks of wide-search candidates.
    rechecks: Mutex<HashMap<wide_search::RecheckKey, Option<Candidate>>>,
    /// Zoom-13 refinements of re-checked wide candidates (by the re-checked
    /// transform) with their DEM zoom: rejected ones release their tiles.
    wide_refined: Mutex<HashMap<wide_search::RecheckKey, Option<(RefineResult, u8)>>>,
    cancel: AtomicBool,
    /// f32 bits of the progress of the running call.
    progress: AtomicU32,
}

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

/// Searches kept in the cache (hint areas and the near area).
const SEARCH_CACHE: usize = 32;

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
            wide: Mutex::new(None),
            rechecks: Mutex::new(HashMap::new()),
            wide_refined: Mutex::new(HashMap::new()),
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

    /// DEM tiles the session holds elevations for (256 KB each): the wide
    /// search releases its tiles as it goes, so this stays bounded.
    pub fn dem_tiles_held(&self) -> usize {
        lock(&self.dem).present_count()
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

/// A refined candidate: the refinement, its DEM zoom and the DEM box it used.
type Refined = (Candidate, RefineResult, u8, MercBox);

/// Outcome of the contour stage.
enum Found {
    Located(Box<GeorefResponse>),
    NotFound(String),
}

/// `Err` stops the call with that response (need_dem, cancelled).
type Step<T> = Result<T, Box<GeorefResponse>>;

/// Hint areas searched at most.
pub(crate) const MAX_HINTS: usize = 8;

/// An area searched at the near resolution.
#[derive(Clone, Copy)]
enum Area<'h> {
    Hint(&'h SearchHint),
    Near,
}

/// Outcome of one area search.
enum AreaResult {
    /// Located, with its confidence.
    Located(Box<GeorefResponse>, f64),
    NotFound,
    /// The map has no usable contours (no area will do better).
    NoContours,
}

struct Run<'a> {
    s: &'a Session,
    req: &'a GeorefRequest,
    gps: Option<LatLon>,
    radius_m: f64,
    /// Most tiles one need_dem answer asks for.
    max_tiles: usize,
    timings: BTreeMap<String, f64>,
    diag: Map<String, Value>,
    used_labels: Vec<usize>,
    t0: Instant,
    /// Hint areas to search, in order, and how many were searched.
    hints: Vec<SearchHint>,
    hints_tried: usize,
    /// Zoom-11 coarse tiles of the hint areas and the near area (never released).
    pinned: HashSet<TileId>,
    /// Zoom-13 tiles of the hint and near areas' refinements and of the checks
    /// run before the search (labels-only refinement, one-label check): never
    /// released.
    kept: HashSet<TileId>,
    /// Present and missing coarse tiles of the areas and chunks searched.
    searched_tiles: HashSet<TileId>,
    missing_tiles: HashSet<TileId>,
    /// Progress of the contour search (the response's `search`).
    progress_info: Option<SearchProgress>,
    /// Coarse tiles of the wide plan: planned, and supplied so far.
    wide_tiles: (usize, usize),
    /// Diagnostics of each area searched (`diagnostics.search_areas`).
    areas: Vec<Value>,
    /// Where coordinate labels of one axis put the map centre.
    band: Option<LabelBand>,
    /// The radius the wide search covers when the map is too small to be
    /// searched out to `radius_m` (`wide::wide_reach`; 50 km: no wide search).
    wide_limit: Option<f64>,
    /// The map's scale bar, once looked for (`scale_bar`).
    scale_bar: Option<Option<ScaleBar>>,
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
            DEFAULT_RADIUS_M
        };
        Run {
            s,
            req,
            gps,
            radius_m,
            max_tiles: req.max_tiles_per_round.clamp(MAX_TILES_RANGE.0, MAX_TILES_RANGE.1),
            timings: BTreeMap::new(),
            diag,
            used_labels: Vec::new(),
            t0: Instant::now(),
            hints: Vec::new(),
            hints_tried: 0,
            pinned: HashSet::new(),
            kept: HashSet::new(),
            searched_tiles: HashSet::new(),
            missing_tiles: HashSet::new(),
            progress_info: None,
            wide_tiles: (0, 0),
            areas: Vec::new(),
            band: None,
            wide_limit: None,
            scale_bar: None,
        }
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
        if !self.req.ocr.is_empty() {
            resp.hint_queries = Some(hint_queries(&self.req.ocr));
        }
        if let Some(mut p) = self.progress_info.take() {
            if resp.status != Status::NeedDem && !resp.is_cancelled() {
                p.stage = SearchStage::Done;
            }
            p.hints_tried = self.hints_tried;
            resp.search = Some(p);
        }
        if !self.areas.is_empty() {
            self.diag.insert("search_areas".into(), Value::Array(std::mem::take(&mut self.areas)));
        }
        if resp.search.is_some() {
            self.diag.insert("dem_tiles_held".into(), json!(self.s.dem_tiles_held()));
        }
        let mut diag = std::mem::take(&mut self.diag);
        if let Value::Object(extra) = std::mem::take(&mut resp.diagnostics) {
            diag.extend(extra);
        }
        resp.diagnostics = Value::Object(diag);
        if resp.status != Status::NeedDem && !resp.is_cancelled() {
            resp.pois = self.pois();
            if let (Status::Ok, Some(m)) = (resp.status, resp.pixel_to_mercator) {
                let dropped = self.check_heights_against_dem(&mut resp.pois, &Homography { m });
                if let (Some(n), Value::Object(d)) = (dropped, &mut resp.diagnostics) {
                    d.insert("spot_heights_off_dem".into(), json!(n));
                }
            }
        }
    }

    /// Spot heights the terrain contradicts are misreads ("478" for 1478,
    /// "1814" for 1314): where the session holds DEM tiles under the located
    /// map (the contour stage or a check fetched them), a spot height more
    /// than `SPOT_HEIGHT_DEM_TOLERANCE_M` outside the elevations around its
    /// position is dropped, and a peak's height likewise forgotten. Returns
    /// how many were dropped, None without terrain data.
    fn check_heights_against_dem(&self, pois: &mut Vec<PoiLabel>, t: &Homography) -> Option<usize> {
        if !pois.iter().any(|p| p.elevation_m.is_some()) {
            return None;
        }
        let (w, h) = self.size();
        let plan = fine_plan(&t.local_affine(w as f64 / 2.0, h as f64 / 2.0), w, h)?;
        let (dem, zoom) = self.s.fine_dem(&plan)?;
        if zoom < FINE_ZOOM - 1 {
            // Coarse tiles smooth cliffs and summits by more than the tolerance.
            return None;
        }
        // Elevation range within one DEM pixel of a point (None where unknown).
        let range = |x: f64, y: f64| -> Option<(f64, f64)> {
            let (mx, my) = t.apply(x, y);
            let r = dem.res;
            let mut lo = f64::INFINITY;
            let mut hi = f64::NEG_INFINITY;
            for (dx, dy) in [(0.0, 0.0), (-r, 0.0), (r, 0.0), (0.0, -r), (0.0, r), (-r, -r), (r, r), (-r, r), (r, -r)] {
                let v = dem.sample(mx + dx, my + dy) as f64;
                if !v.is_finite() {
                    return None;
                }
                lo = lo.min(v);
                hi = hi.max(v);
            }
            Some((lo, hi))
        };
        let fits = |e: f64, (lo, hi): (f64, f64)| {
            e >= lo - SPOT_HEIGHT_DEM_TOLERANCE_M && e <= hi + SPOT_HEIGHT_DEM_TOLERANCE_M
        };
        // Heights in feet on a map that does not say so ("6316" on a Gaia
        // raster): read as metres they are off the terrain, as feet they fit.
        if height_unit(&self.req.ocr).is_none() {
            let spots: Vec<(f64, (f64, f64))> = pois
                .iter()
                .filter(|p| p.kind == PoiKind::SpotHeight)
                .filter_map(|p| Some((p.elevation_m?, range(p.x, p.y)?)))
                .collect();
            let agree = |k: f64| spots.iter().filter(|(e, r)| fits(e * k, *r)).count();
            let (metres, feet) = (agree(1.0), agree(FOOT_M));
            if feet >= 2 && feet > 2 * metres {
                for p in pois.iter_mut() {
                    if let Some(e) = p.elevation_m.as_mut() {
                        *e = (*e * FOOT_M * 10.0).round() / 10.0;
                    }
                }
            }
        }
        let off = |p: &PoiLabel| match (p.elevation_m, range(p.x, p.y)) {
            (Some(e), Some(r)) => !fits(e, r),
            _ => false,
        };
        let before = pois.len();
        pois.retain(|p| p.kind != PoiKind::SpotHeight || !off(p));
        for p in pois.iter_mut().filter(|p| p.kind == PoiKind::Peak) {
            if off(p) {
                p.elevation_m = None;
            }
        }
        Some(before - pois.len())
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
        let mut lone: Option<GridResult> = None;
        let mut reason: Option<String> = None;
        for &m in &methods {
            match m {
                MethodReq::Grid => {
                    if let Some(g) = self.grid()? {
                        // A fit no second label backs up (one label on an axis, or label
                        // centres alone) must have the printed scale when the request tells
                        // it (dpi and an OCR'd 1:n): a misread label stretches the map.
                        if (g.lone_label || g.method == GridMethod::LabelsOnly) && !self.scale_agrees(&g) {
                            if let Some(Value::Object(d)) = self.diag.get_mut("grid") {
                                d.insert("rejected".into(), json!("scale"));
                            }
                            continue;
                        }
                        let max_rms = if self.req.photo { PHOTO_GRID_MAX_RMS_M } else { GRID_MAX_RMS_M };
                        if g.method == GridMethod::Lines && g.rms_m < max_rms {
                            if !g.lone_label {
                                return self.grid_ok(&g, None);
                            }
                            // One axis from a single label, which nothing outvotes: the
                            // terrain under the map confirms it or rules it out.
                            match self.lone_check(&g)? {
                                Some(true) => return self.grid_ok(&g, Some(true)),
                                Some(false) => {
                                    let axis = if g.n_lat <= 1 { self.label_axes(&g).0 } else { self.label_axes(&g).1 };
                                    reason = Some(format!(
                                        "Only one {axis} label could be read, and the terrain under the map doesn't \
                                         match it. Try again near the map, or calibrate manually."
                                    ));
                                }
                                None => lone = Some(g),
                            }
                            continue;
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
        if let Some(g) = &lone {
            return self.grid_ok(g, Some(false));
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
        let mut d = match &g {
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
                "lone_label": g.lone_label,
            }),
        };
        if let Some(g) = g.as_ref().filter(|_| self.req.photo) {
            // How far the identified grid lines bend: paper that does not lie flat.
            let bend = g.lines.iter().filter_map(|l| l.line.bend).map(|b| b.max_abs()).fold(0.0, f64::max);
            d["max_bend_px"] = json!(round_dp(bend, 1));
        }
        self.diag.insert("grid".into(), d);
        if let Some(g) = &g {
            self.used_labels = g.used_labels.clone();
        }
        Ok(g)
    }

    /// Grid labels read: ("latitude", "longitude") or ("northing", "easting").
    fn label_axes(&self, g: &GridResult) -> (&'static str, &'static str) {
        if g.utm_zone.is_none() {
            ("latitude", "longitude")
        } else {
            ("northing", "easting")
        }
    }

    /// A grid answer. `lone`: one axis was fitted from a single label value
    /// (`GridResult::lone_label`), and the terrain confirmed it (`Some(true)`)
    /// or could not be checked (`Some(false)`: flagged for checking).
    fn grid_ok(&mut self, g: &GridResult, lone: Option<bool>) -> Step<GeorefResponse> {
        let (w, h) = self.size();
        if self.req.verify && lone.is_none() {
            self.verify(&g.transform)?;
        }
        let (scale, _) = scale_and_rotation(&g.transform, w as f64 / 2.0, h as f64 / 2.0);
        // Paper that does not lie flat: a warp from the grid lines (and the
        // contours, when they may be used).
        let warp = if self.req.photo {
            let contours = self.warp_contours(&g.transform)?;
            self.photo_warp(&g.transform, Some(g), contours.as_ref().map(|(p, z, b)| (&**p, *z, b)))
        } else {
            None
        };
        // Line positions are good to a fraction of a pixel; the GCP fit RMS
        // alone is optimistic. With a warp, the lines' distance from where
        // the warp fitted without them puts them.
        let fit_rms = match &warp {
            Some(f) if f.warp.is_some() => f.diag.holdout_rms_m.unwrap_or(g.rms_m),
            _ => g.rms_m,
        };
        let rms = (fit_rms.powi(2) + (0.75 * scale).powi(2)).sqrt();
        let n = g.n_lat + g.n_lon;
        let label_factor = match n {
            0..=3 => 0.93,
            4 => 0.97,
            _ => 1.0,
        };
        let rms_ramp =
            if self.req.photo { ramp(g.rms_m, 10.0, PHOTO_GRID_MAX_RMS_M) } else { ramp(g.rms_m, 5.0, GRID_MAX_RMS_M) };
        let mut conf = 0.97 * label_factor * (1.0 - 0.3 * rms_ramp);
        let (ya, xa) = self.label_axes(g);
        let labels = format!("{} {ya} and {} {xa} grid label{}", g.n_lat, g.n_lon, if g.n_lon == 1 { "" } else { "s" });
        let msg = match lone {
            None => format!("Located from {labels} ({}).", plus_minus(rms)),
            Some(true) => format!("Located from {labels}, confirmed by the terrain ({}).", plus_minus(rms)),
            Some(false) => {
                conf = conf.min(LONE_LABEL_MAX_CONFIDENCE);
                let axis = if g.n_lat <= 1 { ya } else { xa };
                format!(
                    "Located from {labels} ({}). Only one {axis} label could be read: check the dot against your \
                     surroundings.",
                    plus_minus(rms)
                )
            }
        };
        let warp = warp.and_then(|f| f.warp);
        Ok(self.located(Method::Grid, &g.transform, warp, conf, rms, msg))
    }

    /// Contours were requested.
    fn wants_contours(&self) -> bool {
        self.req.methods.iter().any(|m| matches!(m.trim().to_ascii_lowercase().as_str(), "contours" | "contour"))
    }

    /// Photo grid fits: the contour products and the fine DEM (zoom, box) a
    /// warp is fitted on besides the grid lines, when contours were
    /// requested (answers need_dem first). None without them.
    fn warp_contours(&mut self, t: &Homography) -> Step<Option<(Arc<ContourProducts>, u8, MercBox)>> {
        if !self.wants_contours() {
            return Ok(None);
        }
        let (w, h) = self.size();
        let Some(plan) = fine_plan(&t.local_affine(w as f64 / 2.0, h as f64 / 2.0), w, h) else {
            return Ok(None);
        };
        let key = self.contour_key();
        self.need(&plan.tiles, "warp", key)?;
        // Offline (tiles marked missing): the grid lines alone.
        let Some((_, zoom)) = self.s.fine_dem(&plan) else {
            return Ok(None);
        };
        let prod = self.products(key)?;
        Ok((prod.samples.n_segs >= MIN_SEGMENTS).then_some((prod, zoom, plan.bbox)))
    }

    fn labels_ok(&mut self, g: &GridResult, contours_tried: bool) -> GeorefResponse {
        let (w, h) = self.size();
        let (scale, _) = scale_and_rotation(&g.transform, w as f64 / 2.0, h as f64 / 2.0);
        let rms = g.rms_m.max(0.75 * scale);
        // Label centres alone are a rough fit no line confirms: always flagged for checking.
        let mut conf = LABELS_MAX_CONFIDENCE * (1.0 - 0.5 * ramp(g.rms_m, 30.0, 150.0));
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
        self.located(Method::Labels, &g.transform, None, conf, rms, msg)
    }

    /// Check a transform against the terrain: `diagnostics.contour_check`.
    fn verify(&mut self, t: &Homography) -> Step<()> {
        self.contour_check(t).map(|_| ())
    }

    /// A grid fit with one axis from a single label (`GridResult::lone_label`),
    /// checked against the terrain when contours were requested
    /// (`diagnostics.contour_check`): Some(true) when the contours fit there
    /// (`ACCEPT`), Some(false) when enough of them landed on the DEM
    /// (`refine::fit_evidence`) and fit badly (`refine::fit_quality`), else
    /// None: nothing to judge it by (no contours requested, no terrain data,
    /// no contours on the map, flat ground) or no clear verdict.
    fn lone_check(&mut self, g: &GridResult) -> Step<Option<bool>> {
        if !self.wants_contours() {
            return Ok(None);
        }
        let Some(r) = self.contour_check(&g.transform)? else { return Ok(None) };
        Ok(if fit_confidence(&r) >= ACCEPT {
            Some(true)
        } else if fit_evidence(&r) >= LONE_MIN_EVIDENCE && fit_quality(&r) < LONE_REJECT_QUALITY {
            Some(false)
        } else {
            None
        })
    }

    /// `verify`: the fit of `t` on the terrain, None without terrain data.
    fn contour_check(&mut self, t: &Homography) -> Step<Option<RefineResult>> {
        let (w, h) = self.size();
        let key = self.contour_key();
        let approx = if t.is_affine() { t.to_affine() } else { t.local_affine(w as f64 / 2.0, h as f64 / 2.0) };
        let Some(plan) = fine_plan(&approx, w, h) else {
            return Ok(None);
        };
        // A one-label check runs again on every call before the contour
        // search: the wide search must not release its tiles (the app
        // answers a tile asked for twice as missing).
        self.kept.extend(plan.tiles.iter().copied());
        self.need(&plan.tiles, "verify", key)?;
        let prod = self.products(key)?;
        let checked = if t.is_affine() {
            self.refine_one(&prod, key, &plan, &approx, true)
        } else {
            self.evaluate_homography(&prod, &plan, t)
        };
        let fit = checked.as_ref().map(|(r, _)| r.clone());
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
                    "gradient_alignment": round_dp(r.gradient_alignment, 3),
                    "contour_interval_m": r.contour_interval_m,
                    "dem_coverage": r.dem_coverage,
                    "n_segments": r.n_segments,
                    "dem_zoom": zoom,
                })
            }
            None => json!({"error": "no terrain data"}),
        };
        self.diag.insert("contour_check".into(), check);
        Ok(fit)
    }

    // --- DEM, contour products, search, refinement ----------------------------

    /// Stop with need_dem if any of `tiles` is unknown (and start computing
    /// the contour products meanwhile). At most `max_tiles_per_round` tiles
    /// are asked for; the next call asks for the rest.
    fn need(&mut self, tiles: &[TileId], stage: &str, key: ContourKey) -> Step<()> {
        self.need_more(tiles, &[], stage, key)
    }

    /// `need`, filling the answer up to `max_tiles_per_round` with unknown
    /// tiles of `lookahead` (the next wide chunk) when `tiles` leave room,
    /// so one round fetches a chunk's candidate checks and the next chunk.
    fn need_more(&mut self, tiles: &[TileId], lookahead: &[TileId], stage: &str, key: ContourKey) -> Step<()> {
        let (mut unknown, mut ahead) = {
            let store = lock(&self.s.dem);
            (store.unknown(tiles), store.unknown(lookahead))
        };
        if unknown.is_empty() {
            return Ok(());
        }
        unknown.sort();
        unknown.dedup();
        let total = unknown.len();
        unknown.truncate(self.max_tiles);
        ahead.sort();
        ahead.dedup();
        ahead.retain(|t| !unknown.contains(t));
        let n_ahead = ahead.len().min(self.max_tiles - unknown.len());
        unknown.extend(ahead.into_iter().take(n_ahead));
        self.s.prefetch_contours(key);
        let zooms: Vec<u8> = unknown.iter().map(|t| t.z).collect::<BTreeSet<u8>>().into_iter().collect();
        let mut r = GeorefResponse::empty(
            Status::NeedDem,
            format!(
                "Need {} terrain tile{} (zoom {}).",
                unknown.len(),
                if unknown.len() == 1 { "" } else { "s" },
                zooms.iter().map(|z| z.to_string()).collect::<Vec<_>>().join(", ")
            ),
        );
        r.diagnostics = json!({
            "stage": stage,
            "tiles_wanted": tiles.len(),
            "tiles_unknown": total,
            "tiles_requested": unknown.len(),
            "lookahead": n_ahead,
        });
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

    /// A grid fit's ground scale is within the range the contour search would
    /// try for the printed scale (`scale_prior`); true without one.
    fn scale_agrees(&self, g: &GridResult) -> bool {
        let Some((_, prior)) = self.scale_prior() else { return true };
        let (w, h) = self.size();
        let (cx, cy) = (w as f64 / 2.0, h as f64 / 2.0);
        let (scale, _) = scale_and_rotation(&g.transform, cx, cy);
        let lat = merc_to_latlon(0.0, g.transform.apply(cx, cy).1).0;
        (0.75 * prior * lat.to_radians().cos()..=1.25 * prior).contains(&scale)
    }

    /// Ground scale range (m/px) to search at latitude `lat`: from an OCR'd
    /// scale and the dpi, a scale bar, a labels-only fit, or the default range.
    fn scale_range(&mut self, lat: f64, init: Option<&GridResult>) -> (f64, f64) {
        self.scale_range_source(lat, init).0
    }

    /// The map's scale bar (`scalebar::find_scale_bar`: a distance label
    /// read and its bar measured), found once per call.
    fn scale_bar(&mut self) -> Option<ScaleBar> {
        if self.scale_bar.is_none() {
            let found = if self.req.ocr.is_empty() { None } else { find_scale_bar(&self.s.image, &self.req.ocr) };
            if let Some(b) = &found {
                self.diag.insert("scale_bar".into(), serde_json::to_value(b).unwrap_or(Value::Null));
            }
            self.scale_bar = Some(found);
        }
        self.scale_bar.clone().flatten()
    }

    /// `scale_range` with its source: "scale_label", "scale_bar", "labels" or
    /// "default".
    fn scale_range_source(&mut self, lat: f64, init: Option<&GridResult>) -> ((f64, f64), &'static str) {
        let (w, h) = self.size();
        let cos_lat = lat.to_radians().cos();
        let default = default_scale_range(w, h);
        // Maps drawn in Web Mercator print at the nominal scale in Mercator
        // metres, so the ground scale can be up to cos(lat) smaller.
        let (lo, hi, source) = if let Some((denom, prior)) = self.scale_prior() {
            self.diag.insert("scale_denominator".into(), json!(denom));
            (0.75 * prior * cos_lat, 1.25 * prior, "scale_label")
        } else if let Some(b) = self.scale_bar() {
            // A bar is drawn for the ground at the map's latitude.
            (b.m_per_px / SCALE_BAR_TOLERANCE, b.m_per_px * SCALE_BAR_TOLERANCE, "scale_bar")
        } else if let Some(g) = init {
            let (s, _) = scale_and_rotation(&g.transform, w as f64 / 2.0, h as f64 / 2.0);
            if s.is_finite() && s > 0.0 {
                (s / 1.25, s * 1.25, "labels")
            } else {
                (default.0, default.1, "default")
            }
        } else {
            (default.0, default.1, "default")
        };
        let (lo, hi) = (lo.clamp(PLAUSIBLE_SCALE.0, PLAUSIBLE_SCALE.1), hi.clamp(PLAUSIBLE_SCALE.0, PLAUSIBLE_SCALE.1));
        self.diag
            .insert("scale_range".into(), json!({"min": round_dp(lo, 3), "max": round_dp(hi, 3), "source": source}));
        ((lo, hi), source)
    }

    /// The scale range at latitude `lat` as bands searched in turn, finest
    /// first (`wide::scale_bands`): the wide search's.
    fn bands(&mut self, lat: f64, init: Option<&GridResult>) -> Vec<ScaleBand> {
        let (w, h) = self.size();
        let (lo, hi) = self.scale_range(lat, init);
        scale_bands(lo, hi, w, h, lat)
    }

    /// The bands of a hint or near area: `bands`, and after a scale bar's
    /// range (a prior, not a constraint: a bar misread or mismeasured must
    /// not hide the map) the default range's.
    fn area_bands(&mut self, lat: f64, init: Option<&GridResult>) -> Vec<ScaleBand> {
        let (w, h) = self.size();
        let ((lo, hi), source) = self.scale_range_source(lat, init);
        let mut bands = scale_bands(lo, hi, w, h, lat);
        if source == "scale_bar" {
            let (dlo, dhi) = default_scale_range(w, h);
            bands.extend(scale_bands(dlo, dhi, w, h, lat));
        }
        bands
    }

    /// Coarse search parameters for the area of `radius_m` around `centre`
    /// in one scale band.
    fn search_params(&self, centre: LatLon, radius_m: f64, band: &ScaleBand) -> SearchParams {
        SearchParams {
            center_merc: latlon_to_merc(centre.lat, centre.lon),
            search_radius_m: radius_m,
            scale_min: band.lo,
            scale_max: band.hi,
            rotations_deg: if self.req.allow_rotation { rotation_sweep(10.0) } else { vec![0.0] },
            top_k: MAX_REFINE_CANDIDATES,
            region: None,
        }
    }

    /// The coarse DEM box and its tiles for an area search in `band`.
    fn area_box(&self, params: &SearchParams, band: &ScaleBand) -> (MercBox, Vec<TileId>) {
        let (w, h) = self.size();
        let (lat, _) = merc_to_latlon(params.center_merc.0, params.center_merc.1);
        let b = required_dem_box_capped(params, w, h, band.max_box_half_m(lat));
        (b, tiles_covering(band.zoom, &b))
    }

    fn search(
        &mut self,
        prod: &ContourProducts,
        key: ContourKey,
        params: &SearchParams,
        (zoom, coarse_box): (u8, &MercBox),
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
        let dem = lock(&self.s.dem).mosaic(zoom, coarse_box);
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
        // The same peak found again (by the whole near disc after its inner
        // disc, or by the default scale range after a scale bar's): its
        // start next to one refined before converges to the same fit.
        if !evaluate_only {
            let (w, h) = self.size();
            let again = lock(&self.s.refinements)
                .iter()
                .find(|(k, _)| {
                    k.contour == key
                        && k.zoom == zoom
                        && k.dem_valid == rkey.dem_valid
                        && !k.evaluate_only
                        && starts_agree(&affine_from_bits(&k.transform), t, w, h)
                })
                .map(|(_, r)| r.clone());
            if let Some(r) = again {
                return Some((r, zoom));
            }
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
    ) -> Step<Vec<Refined>> {
        let (w, h) = self.size();
        let plans: Vec<(Candidate, FinePlan)> =
            cands.iter().filter_map(|c| fine_plan(&c.transform, w, h).map(|p| (c.clone(), p))).collect();
        let mut tiles: Vec<TileId> = plans.iter().flat_map(|(_, p)| p.tiles.iter().copied()).collect();
        tiles.sort();
        tiles.dedup();
        // Hint and near areas keep their refinement tiles (the wide search
        // releases its own).
        self.kept.extend(tiles.iter().copied());
        self.need(&tiles, "refine", key)?;
        let mut out = Vec::new();
        let n = plans.len().max(1) as f32;
        for (i, (c, plan)) in plans.iter().enumerate() {
            self.check_cancel()?;
            if let Some((r, zoom)) = self.refine_one(prod, key, plan, &c.transform, false) {
                out.push((c.clone(), r, zoom, plan.bbox));
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
        let mut labelled: Option<Refined> = None;

        // 1. A labels-only fit: refine it directly (no GPS or search needed).
        if let Some((t, plan, rms)) = init
            .filter(|g| g.transform.is_affine())
            .and_then(|g| fine_plan(&g.transform.to_affine(), w, h).map(|p| (g.transform.to_affine(), p, g.rms_m)))
        {
            // Refined again on every call: kept from the wide search's releases.
            self.kept.extend(plan.tiles.iter().copied());
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
                    return Ok(Found::Located(Box::new(self.contour_ok(&r, conf, (zoom, &plan.bbox), true, &prod))));
                }
                if agree {
                    labelled = Some((labels_candidate(&t, w, h), r, zoom, plan.bbox));
                }
            }
        }

        // 2. Where to search: hint areas, the near disc around the fix, the
        //    wide area beyond it.
        self.plan_search(init);
        if self.gps.is_none() && self.hints.is_empty() {
            return Ok(Found::NotFound(
                "No coordinate labels found on this map, and contour matching needs your location or a place to \
                 search. Turn on location services or calibrate manually."
                    .into(),
            ));
        }
        // Sub-confident matches wait while other areas may still give a
        // confident one; the most confident is reported at the end.
        let mut fallback: Option<(f64, Box<GeorefResponse>)> = None;
        let keep = |fallback: &mut Option<(f64, Box<GeorefResponse>)>, conf: f64, r: Box<GeorefResponse>| {
            if fallback.as_ref().is_none_or(|f| conf > f.0) {
                *fallback = Some((conf, r));
            }
        };

        // 3. Hint areas, at the near resolution: each scale band in turn
        //    (the near zoom's first, then coarser tiles for small-scale maps).
        for i in 0..self.hints.len() {
            let hint = self.hints[i].clone();
            let r = hint.radius_m.clamp(HINT_RADIUS_RANGE_M.0, HINT_RADIUS_RANGE_M.1);
            for band in self.area_bands(hint.lat, init) {
                self.set_progress(SearchStage::Hints, band.zoom, 0.0);
                let area = Area::Hint(&hint);
                let outcome = self.area_search(area, hint.latlon(), r, labelled.as_ref(), &band)?;
                match outcome {
                    AreaResult::Located(resp, conf) if conf >= CONFIDENT => {
                        self.hints_tried = i + 1;
                        return Ok(Found::Located(resp));
                    }
                    AreaResult::Located(resp, conf) => keep(&mut fallback, conf, resp),
                    AreaResult::NoContours => return Ok(Found::NotFound(no_contours.into())),
                    AreaResult::NotFound => {}
                }
            }
            self.hints_tried = i + 1;
        }

        // 4. The near disc around the fix (the whole search up to 50 km).
        if let Some(gps) = self.gps {
            let near = self.radius_m.min(NEAR_RADIUS_M);
            let wide = self.radius_m > NEAR_RADIUS_M;
            let nz = near_zoom(gps.lat);
            self.set_progress(SearchStage::Near, nz, 0.0);
            // The near disc is searched even when coordinate labels of one
            // axis put the map elsewhere (their band): one misread label
            // must not hide a map lying next to the user. The inner disc
            // first (where the map usually is: as fast as a 20 km search),
            // then the whole near disc; then the coarser scale bands over
            // the whole near disc at once (on coarse tiles: cheap). After a
            // scale bar's band, the default range (the fallback for a
            // misread bar) searches the whole disc at once.
            let mut steps: Vec<(f64, ScaleBand)> = Vec::new();
            for (i, band) in self.area_bands(gps.lat, init).into_iter().enumerate() {
                if i == 0 && band.is_fine() && near > NEAR_FIRST_RADIUS_M {
                    steps.push((NEAR_FIRST_RADIUS_M, band));
                }
                steps.push((near, band));
            }
            for (r, band) in steps {
                self.set_progress(SearchStage::Near, band.zoom, 0.0);
                match self.area_search(Area::Near, gps, r, labelled.as_ref(), &band)? {
                    AreaResult::Located(resp, conf) => keep(&mut fallback, conf, resp),
                    AreaResult::NoContours => return Ok(Found::NotFound(no_contours.into())),
                    AreaResult::NotFound => {}
                }
                if fallback.as_ref().is_some_and(|f| f.0 >= CONFIDENT) {
                    break;
                }
                if r < near {
                    self.set_progress(SearchStage::Near, band.zoom, r);
                }
            }
            self.set_progress(SearchStage::Near, nz, near);
            if let Some((conf, _)) = &fallback {
                if *conf >= CONFIDENT || !wide {
                    return Ok(Found::Located(fallback.take().unwrap().1));
                }
            }
            // 5. The wide area, chunk by chunk, nearest first.
            if wide {
                let prod = self.products(self.contour_key())?;
                let bands = self.bands(gps.lat, init);
                if let Some(r) = self.wide_search(&prod, gps, &bands)? {
                    return Ok(Found::Located(r));
                }
            }
        }
        if let Some((_, r)) = fallback {
            return Ok(Found::Located(r));
        }
        Ok(Found::NotFound(self.not_found_message()))
    }

    /// Plan the hint areas (request hints and the map's coordinate labels,
    /// those within the radius of the fix) and the progress counters.
    fn plan_search(&mut self, init: Option<&GridResult>) {
        let (w, h) = self.size();
        let mut hints: Vec<SearchHint> = self.req.hints.clone();
        // Coordinate labels the grid fit could not use.
        if init.is_none() && !self.req.ocr.is_empty() {
            let lat = self
                .req
                .ocr
                .iter()
                .find_map(|o| match parse_coordinate(&o.text) {
                    Some(CoordValue::Lat(v)) => Some(v),
                    _ => None,
                })
                .or(self.gps.map(|g| g.lat))
                .unwrap_or(0.0);
            // Coordinate labels are printed on sheets: without a printed
            // scale the label hint assumes the near zoom's band (a disc
            // around the offset at 50 m/px would reach far past 50 km).
            let (scale, source) = self.scale_range_source(lat, None);
            let scale = if source == "default" { (scale.0, scale.1.min(FINE_MAX_SCALE)) } else { scale };
            let labels = label_hints(&self.req.ocr, w, h, scale);
            if !labels.is_empty() {
                self.diag.insert("label_hints".into(), serde_json::to_value(&labels).unwrap_or(Value::Null));
            }
            hints.extend(labels);
            // Labels of one axis only: a band the wide search tries first.
            if self.gps.is_some() {
                self.band = label_band(&self.req.ocr, w, h, scale, lat);
                if let Some(b) = &self.band {
                    self.diag.insert("label_band".into(), serde_json::to_value(b).unwrap_or(Value::Null));
                }
            }
        }
        let n_in = hints.len();
        let gps = self.gps;
        let radius = self.radius_m;
        hints.retain(|h| h.is_valid() && gps.is_none_or(|g| ground_distance_m(g, h.latlon()) <= radius));
        // Searched at most 50 km around: a larger radius must not count as
        // covering (and hide) the hints after it.
        for h in &mut hints {
            h.radius_m = h.radius_m.clamp(HINT_RADIUS_RANGE_M.0, HINT_RADIUS_RANGE_M.1);
        }
        let mut hints = order_hints(hints);
        let ignored = n_in - hints.len().min(n_in);
        hints.truncate(MAX_HINTS);
        if ignored > 0 {
            self.diag.insert("hints_ignored".into(), json!(ignored));
        }
        self.hints = hints;
        // Coarse tiles of the hint and near areas.
        let mut planned: HashSet<TileId> = HashSet::new();
        let areas: Vec<(LatLon, f64)> = self
            .hints
            .iter()
            .map(|h| (h.latlon(), h.radius_m.clamp(HINT_RADIUS_RANGE_M.0, HINT_RADIUS_RANGE_M.1)))
            .chain(self.gps.map(|g| (g, self.radius_m.min(NEAR_RADIUS_M))))
            .collect();
        for (c, r) in areas {
            for band in self.area_bands(c.lat, init) {
                let p = self.search_params(c, r, &band);
                planned.extend(self.area_box(&p, &band).1);
            }
        }
        self.pinned = planned;
        if let Some(g) = self.gps.filter(|_| self.radius_m > NEAR_RADIUS_M) {
            let bands = self.bands(g.lat, init);
            self.plan_wide_progress(g, &bands);
        }
        let first_lat = self.hints.first().map(|h| h.lat).or(self.gps.map(|g| g.lat)).unwrap_or(0.0);
        self.progress_info = Some(SearchProgress {
            radius_m: if self.gps.is_some() { self.radius_m } else { 0.0 },
            searched_radius_m: 0.0,
            coarse_zoom: near_zoom(first_lat),
            tiles_planned: self.pinned.len(),
            tiles_fetched: 0,
            hints_tried: 0,
            hints_planned: self.hints.len(),
            stage: if self.hints.is_empty() { SearchStage::Near } else { SearchStage::Hints },
        });
        self.update_fetched();
    }

    /// The labels' band as a Mercator box (unbounded along the free axis).
    fn band_merc(&self) -> Option<MercBox> {
        let b = self.band.as_ref()?;
        let half = std::f64::consts::PI * crate::geo::EARTH_RADIUS;
        let mut m = MercBox { min_x: -half, min_y: -half, max_x: half, max_y: half };
        if let Some((s, n)) = b.lat {
            m.min_y = latlon_to_merc(s, 0.0).1;
            m.max_y = latlon_to_merc(n, 0.0).1;
        }
        if let Some((wl, el)) = b.lon {
            m.min_x = latlon_to_merc(0.0, wl).0;
            m.max_x = latlon_to_merc(0.0, el).0;
        }
        Some(m)
    }

    /// Update the progress object: stage, coarse zoom and (when larger) the
    /// radius searched completely.
    fn set_progress(&mut self, stage: SearchStage, zoom: u8, searched_m: f64) {
        let tried = self.hints_tried;
        if let Some(p) = &mut self.progress_info {
            p.stage = stage;
            p.coarse_zoom = zoom;
            p.searched_radius_m = p.searched_radius_m.max(searched_m.min(p.radius_m));
            p.hints_tried = tried;
        }
        self.update_fetched();
    }

    /// Recount the planned coarse tiles supplied so far: the hint and near
    /// areas' and the wide chunks' (`wide_tiles`, kept by the wide search:
    /// tiles of searched chunks count even once released).
    fn update_fetched(&mut self) {
        let known = {
            let store = lock(&self.s.dem);
            self.pinned.iter().filter(|t| store.known(t)).count()
        };
        let (planned, fetched) = self.wide_tiles;
        let tried = self.hints_tried;
        if let Some(p) = &mut self.progress_info {
            p.tiles_planned = self.pinned.len() + planned;
            p.tiles_fetched = known + fetched.min(planned);
            p.hints_tried = tried;
        }
    }

    /// Search one area at the near resolution: the zoom-11 coarse search
    /// within `radius_m` of `centre`, then the zoom-13 refinement of its best
    /// candidates (the best alone first when it clearly stands out), with
    /// the labels-only fit when there is one. Accepts at `ACCEPT`.
    fn area_search(
        &mut self,
        area: Area,
        centre: LatLon,
        radius_m: f64,
        labelled: Option<&Refined>,
        band: &ScaleBand,
    ) -> Step<AreaResult> {
        let (w, h) = self.size();
        let key = self.contour_key();
        let stage = match area {
            Area::Hint(_) => "hint",
            Area::Near => "coarse",
        };
        let params = self.search_params(centre, radius_m, band);
        let (coarse_box, coarse_tiles) = self.area_box(&params, band);
        let zoom = band.zoom;
        self.need(&coarse_tiles, stage, key)?;
        let present: Vec<TileId> = {
            let store = lock(&self.s.dem);
            coarse_tiles.iter().filter(|t| store.present(t)).copied().collect()
        };
        self.searched_tiles.extend(present.iter().copied());
        self.missing_tiles.extend(coarse_tiles.iter().filter(|t| !present.contains(t)).copied());
        let mut record = json!({
            "area": match area { Area::Hint(_) => "hint", Area::Near => "near" },
            "center": [round_dp(centre.lat, 5), round_dp(centre.lon, 5)],
            "radius_km": round_dp(radius_m / 1000.0, 1),
            "tiles": coarse_tiles.len(),
            "tiles_present": present.len(),
            "band": {"zoom": band.zoom, "min": round_dp(band.lo, 3), "max": round_dp(band.hi, 3)},
        });
        if let Area::Hint(hint) = area {
            record["source"] = json!(hint.source);
            record["name"] = json!(hint.name);
        }
        if present.is_empty() {
            record["outcome"] = json!("no terrain");
            self.areas.push(record);
            return Ok(AreaResult::NotFound);
        }
        let prod = self.products(key)?;
        if prod.samples.n_segs < MIN_SEGMENTS {
            return Ok(AreaResult::NoContours);
        }
        let cands = self.search(&prod, key, &params, (zoom, &coarse_box), present.len())?;
        let cdiag: Vec<Value> = cands
            .iter()
            .map(|c| {
                let (mx, my) = c.center_merc(w, h);
                let (lat, lon) = merc_to_latlon(mx, my);
                let km = ground_distance_m(centre, LatLon::new(lat, lon)) / 1000.0;
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
        record["candidate_list"] = Value::Array(cdiag.clone());
        self.diag.insert("candidates".into(), Value::Array(cdiag));
        record["candidates"] = json!(cands.len());

        // Refinement: the best candidate alone when it stands out, else the top few.
        // A coarser band's candidates must stand out a little (small-scale maps
        // found there peak at 3–8 its runner-up): refining three wrong ones costs
        // ~2 s in every search the map is not in.
        let n = if band.is_fine() {
            cands.len().min(MAX_REFINE_CANDIDATES)
        } else {
            cands.iter().take(MAX_REFINE_CANDIDATES).take_while(|c| c.peak_ratio >= COARSE_BAND_MIN_RATIO).count()
        };
        if n == 0 && labelled.is_none() {
            record["outcome"] = json!("no candidates");
            self.areas.push(record);
            return Ok(AreaResult::NotFound);
        }
        if n > 0 && cands[0].peak_ratio >= DISTINCT_PEAK_RATIO {
            let first = self.refine_candidates(&prod, key, &cands[..1])?;
            if let Some((c, r, zoom, bbox)) = first.first() {
                let conf = confidence(c, r);
                if conf >= CONFIDENT {
                    let sel = json!({"candidate": 0, "refined": 1, "confidence": round_dp(conf, 3)});
                    record["outcome"] = json!("located");
                    record["selected"] = sel.clone();
                    self.areas.push(record);
                    let resp = self.contour_ok(r, conf, (*zoom, bbox), false, &prod);
                    return Ok(AreaResult::Located(self.own_diag(resp, sel), conf));
                }
            }
        }
        let mut results = self.refine_candidates(&prod, key, &cands[..n])?;
        let n_search = results.len();
        if let Some(l) = labelled {
            results.push(l.clone());
        }
        let pairs: Vec<(Candidate, RefineResult)> = results.iter().map(|(c, r, _, _)| (c.clone(), r.clone())).collect();
        let Some((best, mut conf)) = select_refined(&pairs) else {
            record["outcome"] = json!("nothing refined");
            self.areas.push(record);
            return Ok(AreaResult::NotFound);
        };
        // The fit gap among the refined candidates counts as search evidence
        // (a right match in repetitive terrain), but three refinements are
        // weak evidence: on real terrain 140 km from the sheet the phone photo
        // fitted wrong places at 0.97 and 0.999 (coarse peak ratios ~1, scale
        // 4× and 2.4× off, turned 5–10°) and was accepted at 0.82 and 0.999.
        // A match lifted that way must also stand out locally, like a wide
        // match: its zoom-11 re-check (4 km, ±10 % scale) peaks at ≥ 2.2 its
        // runner-up (true matches 2.56–5.39, wrong ones ≤ 1.87; `wide_search`).
        let own = confidence(&pairs[best].0, &pairs[best].1);
        let mut recheck = Value::Null;
        if best < n_search && conf >= ACCEPT && own < CONFIDENT && conf > own {
            let r = self.recheck_one(&prod, &pairs[best].0)?;
            let (ratio, ncc) = r.map_or((0.0, 0.0), |c| (c.peak_ratio, c.score));
            recheck = json!({"ratio": round_dp(ratio as f64, 2), "ncc": round_dp(ncc as f64, 3)});
            if ratio < wide_search::WIDE_MIN_RATIO || ncc < wide_search::WIDE_MIN_NCC {
                conf = own;
            }
        }
        let fits: Vec<f64> = pairs.iter().map(|(_, r)| round_dp(fit_confidence(r), 3)).collect();
        record["refined_list"] = pairs
            .iter()
            .map(|(_, r)| {
                json!({
                    "fit": round_dp(fit_confidence(r), 3),
                    "seg_std_m": round_dp(r.median_seg_std_m, 2),
                    "interval_m": round_dp(r.contour_interval_m, 1),
                    "snr": round_dp(r.interval_snr, 2),
                    "explained": round_dp(r.explained_variance, 4),
                    "relief_m": round_dp(r.relief_m, 0),
                    "segments": r.n_segments,
                    "coverage": round_dp(r.dem_coverage, 2),
                    "m_per_px": round_dp(r.px_ground_m, 3),
                    "alignment": round_dp(r.gradient_alignment, 3),
                    "affine": r.affine,
                })
            })
            .collect();
        let sel = json!({"candidate": best, "refined": results.len(), "fit_confidences": fits,
                         "confidence": round_dp(conf, 3), "recheck": recheck});
        record["selected"] = sel.clone();
        if conf < ACCEPT {
            self.diag.insert("best_confidence".into(), json!(round_dp(conf, 3)));
            record["outcome"] = json!("not confident");
            self.areas.push(record);
            return Ok(AreaResult::NotFound);
        }
        record["outcome"] = json!("located");
        self.areas.push(record);
        let (_, r, zoom, bbox) = &results[best];
        let resp = self.contour_ok(r, conf, (*zoom, bbox), best >= n_search, &prod);
        Ok(AreaResult::Located(self.own_diag(resp, sel), conf))
    }

    /// Move the diagnostics of a located match (`refined`, `warp`) into its
    /// response with `selected`, so a match kept while other areas are
    /// searched reports its own.
    fn own_diag(&mut self, mut resp: GeorefResponse, selected: Value) -> Box<GeorefResponse> {
        let mut d = Map::new();
        for k in ["refined", "warp"] {
            if let Some(v) = self.diag.remove(k) {
                d.insert(k.into(), v);
            }
        }
        d.insert("selected".into(), selected);
        resp.diagnostics = Value::Object(d);
        Box::new(resp)
    }

    /// Why nothing was found, saying what was searched.
    fn not_found_message(&self) -> String {
        let present = self.searched_tiles.len();
        let missing = self.missing_tiles.iter().filter(|t| !self.searched_tiles.contains(t)).count();
        if present == 0 && missing > 0 {
            return "Terrain data for the search area couldn't be downloaded, so contour lines can't be matched. Try \
                    again with a connection, or calibrate manually."
                .into();
        }
        if missing > 0 && missing >= present {
            // Most of the area went unsearched: say so, not "no match within".
            return format!(
                "Terrain for much of the search area couldn't be downloaded ({} of {} tiles), and the rest holds no \
                 match. Try again with a connection, or calibrate manually.",
                group_thousands(missing as f64),
                group_thousands((missing + present) as f64)
            );
        }
        let mut what = vec![format!("searched {} terrain tile{}", group_thousands(present as f64), plural(present))];
        if missing > 0 {
            what.push(format!("{} couldn't be downloaded", group_thousands(missing as f64)));
        }
        let tried = &self.hints[..self.hints_tried.min(self.hints.len())];
        let n = |s: HintSource| tried.iter().filter(|h| h.source == s).count();
        let (places, labels, user) = (n(HintSource::Place), n(HintSource::Label), n(HintSource::User));
        if places > 0 {
            what.push(format!("tried {places} place{} named on the map", plural(places)));
        }
        if labels > 0 {
            what.push("tried the position its coordinate labels give".into());
        }
        if user > 0 {
            what.push(format!("tried the area{} you picked", plural(user)));
        }
        if let Some(b) = self.band.as_ref().filter(|_| self.gps.is_some() && self.radius_m > NEAR_RADIUS_M) {
            what.push(format!("first along {}, printed on the map", b.name));
        }
        // A map too small for the coarse zooms is searched out to its limit.
        let limit = self.wide_limit.filter(|l| *l < self.radius_m && self.gps.is_some());
        let km = |m: f64| group_thousands((m / 1000.0).round());
        let head = match self.gps {
            Some(_) => format!("No match within {} km of you", km(limit.unwrap_or(self.radius_m))),
            None => "No match in the areas tried".into(),
        };
        let too_small = limit
            .map(|l| format!(" This map shows too small an area to be searched farther than {} km away.", km(l)))
            .unwrap_or_default();
        let advice = if self.gps.is_some() && self.radius_m < RADIUS_RANGE_M.1 && limit.is_none() {
            "Search a wider area, pick the contour colour, or calibrate manually."
        } else {
            "Pick the contour colour, or calibrate manually."
        };
        format!("{head} ({}).{too_small} {advice}", what.join("; "))
    }

    /// A located contour match; `dem` is the DEM zoom and box of its
    /// refinement (photos: the warp is fitted on the same DEM).
    fn contour_ok(
        &mut self,
        r: &RefineResult,
        conf: f64,
        dem: (u8, &MercBox),
        from_labels: bool,
        prod: &ContourProducts,
    ) -> GeorefResponse {
        let (w, h) = self.size();
        let zoom = dem.0;
        let t = r.homography.unwrap_or_else(|| r.transform.to_homography());
        let (_, cy) = t.apply(w as f64 / 2.0, h as f64 / 2.0);
        let (lat, _) = merc_to_latlon(0.0, cy);
        let mut rms = contour_rms(r, zoom, lat);
        // Paper that does not lie flat: a warp from the contours.
        let warp = if self.req.photo { self.photo_warp(&t, None, Some((prod, dem.0, dem.1))) } else { None };
        let warp = warp.and_then(|f| {
            let warped = RefineResult { median_seg_std_m: f.diag.median_seg_std_m?, ..r.clone() };
            f.warp.inspect(|_| rms = contour_rms(&warped, zoom, lat))
        });
        self.diag.insert(
            "refined".into(),
            json!({
                "fit_confidence": round_dp(fit_confidence(r), 3),
                "cost_m": r.cost_m,
                "median_seg_std_m": r.median_seg_std_m,
                "inlier_fraction": r.inlier_fraction,
                "explained_variance": r.explained_variance,
                "interval_snr": r.interval_snr,
                "gradient_alignment": round_dp(r.gradient_alignment, 3),
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
        self.located(Method::Contours, &t, warp, conf, rms, msg)
    }

    /// Photo mode: fit a warp for paper that does not lie flat on top of `t`
    /// (`warp.rs`) from the identified grid lines and/or the contours on the
    /// DEM of `contours.1`/`.2` (zoom, box); records `diagnostics.warp`.
    fn photo_warp(
        &mut self,
        t: &Homography,
        grid: Option<&GridResult>,
        contours: Option<(&ContourProducts, u8, &MercBox)>,
    ) -> Option<WarpFit> {
        let (w, h) = self.size();
        // Every other long straight line printed on the sheet (the frame,
        // unlabelled grid lines) must stay straight.
        let valued: Vec<GridLine> = grid.map(|g| g.lines.iter().map(|l| l.line).collect()).unwrap_or_default();
        let straight: Vec<GridLine> = self.lines().iter().filter(|l| !valued.contains(l)).copied().collect();
        let tm = Instant::now();
        let dem = contours.and_then(|(_, zoom, bbox)| {
            let plan = FinePlan { zoom, bbox: *bbox, tiles: Vec::new() };
            let (m, z) = self.s.fine_dem(&plan)?;
            (z == zoom).then(|| PreparedDem::new_cropped(&m, bbox))
        });
        let img = self.analysed();
        let input = WarpInput {
            w,
            h,
            transform: *t,
            image: Some(&img),
            grid: grid.filter(|g| !g.lines.is_empty()).map(|g| GridLines { lines: &g.lines, utm_zone: g.utm_zone }),
            straight: &straight,
            contours: contours.zip(dem.as_ref()).map(|((p, _, _), d)| ContourInput { samples: &p.samples, dem: d }),
        };
        if input.grid.is_none() && input.contours.is_none() {
            return None;
        }
        let fit = fit_warp(&input);
        self.time("warp", tm);
        self.diag.insert("warp".into(), serde_json::to_value(&fit.diag).unwrap_or(Value::Null));
        Some(fit)
    }

    /// A located response for the pixel → Mercator homography `t`, plus a
    /// warp in photo mode (the full model `t + warp` gives the corners, the
    /// scale and the rotation).
    fn located(
        &mut self,
        method: Method,
        t: &Homography,
        warp: Option<Warp>,
        conf: f64,
        rms: f64,
        message: String,
    ) -> GeorefResponse {
        let (w, h) = self.size();
        let (cx, cy) = (w as f64 / 2.0, h as f64 / 2.0);
        let (scale, rotation) = match &warp {
            Some(wp) => affine_scale_rotation(&wp.local_affine(t, cx, cy), wp.apply(t, cx, cy).1),
            None => scale_and_rotation(t, cx, cy),
        };
        let corners = [(0.0, 0.0), (w as f64, 0.0), (w as f64, h as f64), (0.0, h as f64)].map(|(x, y)| {
            let (mx, my) = warp.as_ref().map_or_else(|| t.apply(x, y), |wp| wp.apply(t, x, y));
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
        r.warp = warp;
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
    affine_scale_rotation(&local_affine(t, x, y), t.apply(x, y).1)
}

/// Ground scale (m/px) and map-up bearing (degrees) of a local pixel →
/// Mercator affine at Mercator y `my`.
fn affine_scale_rotation(a: &Affine, my: f64) -> (f64, f64) {
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

/// "1 000"-style grouping of a whole number (no-break space between groups).
fn group_thousands(v: f64) -> String {
    let digits = format!("{:.0}", v.max(0.0));
    let mut out = String::new();
    for (i, c) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i) % 3 == 0 {
            out.push('\u{a0}');
        }
        out.push(c);
    }
    out
}

fn plural(n: usize) -> &'static str {
    if n == 1 {
        ""
    } else {
        "s"
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

mod wide_search;

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
