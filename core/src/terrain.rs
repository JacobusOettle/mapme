//! Terrain for the app's explorer features (docs/design/explorer.md): a thread-safe,
//! memory-bounded store of decoded Terrarium tiles that the app feeds (`TerrainStore`, C ABI
//! `MMTerrain`), and the queries on it: elevations at points, statistics and an elevation profile
//! along a polyline (planned routes, GPX tracks, the recorded trail), height grids over a Web
//! Mercator rectangle (the 3D view's mesh), contour and hillshade rendering in a map's pixel space
//! (terrain beyond the sheet's edge) or in Web Mercator (the world view, the 3D view's texture),
//! and tile plans (offline regions, the terrain cached around a located map).
//!
//! The request and response types below are the JSON contract (docs/ARCHITECTURE.md, "C ABI",
//! Terrain). The query bodies answer `not implemented` until they are written; the store's
//! bookkeeping, the interval rule, the tile-size tables and the trailhead rule are final.
//!
//! The store never downloads: a query that needs tiles it does not hold answers `need_dem` with
//! the tiles to add (`need_tiles`), and still answers from what it holds.

use crate::dem::TileId;
use serde::{Deserialize, Serialize};

/// Message of every query not implemented yet.
pub const NOT_IMPLEMENTED: &str = "not implemented";

/// Default byte budget of decoded elevations (`mm_terrain_new(0)`).
pub const DEFAULT_MAX_BYTES: u64 = 64 << 20;
/// Smallest and largest byte budgets accepted (others are clamped).
pub const MIN_MAX_BYTES: u64 = 4 << 20;
pub const MAX_MAX_BYTES: u64 = 1 << 30;
/// Missing marks kept at most (the oldest go first).
pub const MAX_MISSING_MARKS: usize = 100_000;
/// Finest DEM zoom a query uses by default, and the finest accepted at all.
pub const DEFAULT_MAX_ZOOM: u8 = 13;
pub const MAX_ZOOM: u8 = 15;
/// Zooms below `max_zoom` a query may fall back to by default.
pub const DEFAULT_ZOOM_FALLBACK: u8 = 3;
/// Tiles listed in one `need_tiles` answer by default.
pub const DEFAULT_MAX_TILES: usize = 64;
/// Grid sides accepted by `mm_terrain_grid`.
pub const GRID_SIDE: (usize, usize) = (2, 4096);
/// Contour intervals the automatic rule picks from (metres).
pub const CONTOUR_INTERVALS_M: [f64; 10] = [5.0, 10.0, 20.0, 25.0, 50.0, 100.0, 200.0, 250.0, 500.0, 1000.0];
/// The automatic interval is the smallest of `CONTOUR_INTERVALS_M` at least this many times
/// the ground metres per point.
pub const INTERVAL_POINTS: f64 = 4.0;
/// Index contours every this many intervals by default.
pub const DEFAULT_INDEX_EVERY: u32 = 5;
/// Default climb hysteresis of a profile (metres), as the app's recorded-track ascent.
pub const DEFAULT_HYSTERESIS_M: f64 = 3.0;
/// Default number of profile samples returned.
pub const DEFAULT_MAX_SAMPLES: usize = 1000;

/// Trailhead caching (`"trailhead"` in a tile plan): what the app keeps on the device around
/// every map it locates, so matching nearby maps, profiles, the terrain beyond the sheet and the
/// 3D view work offline there. Zooms 9 and 10 within `TRAILHEAD_COARSE_RADIUS_M` of the map's
/// centre (the coarse bands of a 50 km search), zoom 11 within `TRAILHEAD_Z11_RADIUS_M`, zoom 12
/// over the sheet plus its continuation margin (`continuation_margin_m`), zoom 13 over the sheet
/// plus `TRAILHEAD_Z13_MARGIN_M`; a zoom whose part would exceed `TRAILHEAD_MAX_TILES_PER_ZOOM`
/// tiles is dropped (reported in `"dropped"`).
pub const TRAILHEAD_COARSE_RADIUS_M: f64 = 50_000.0;
pub const TRAILHEAD_Z11_RADIUS_M: f64 = 30_000.0;
pub const TRAILHEAD_Z13_MARGIN_M: f64 = 3_000.0;
pub const TRAILHEAD_MAX_TILES_PER_ZOOM: usize = 400;
/// The continuation margin around a sheet: half its long side (ground metres), within these.
pub const CONTINUATION_MARGIN_M: (f64, f64) = (3_000.0, 15_000.0);

/// How far beyond a sheet's edge the app continues it with terrain (off-map contours, the 3D
/// view's surroundings, the trailhead's zoom-12 area): half the sheet's long side, at least 3 km,
/// at most 15 km.
pub fn continuation_margin_m(long_side_m: f64) -> f64 {
    if !long_side_m.is_finite() {
        return CONTINUATION_MARGIN_M.0;
    }
    (long_side_m / 2.0).clamp(CONTINUATION_MARGIN_M.0, CONTINUATION_MARGIN_M.1)
}

/// The automatic contour interval for a ground scale (metres per point).
pub fn auto_interval_m(scale_m_per_pt: f64) -> f64 {
    let want = INTERVAL_POINTS * scale_m_per_pt;
    if !want.is_finite() || want <= 0.0 {
        return CONTOUR_INTERVALS_M[0];
    }
    CONTOUR_INTERVALS_M
        .iter()
        .copied()
        .find(|&i| i >= want)
        .unwrap_or(CONTOUR_INTERVALS_M[CONTOUR_INTERVALS_M.len() - 1])
}

/// Mean Terrarium tile size (bytes) per zoom where the terrain comes from SRTM alone (most of the
/// world outside the US and Europe): HEAD requests on s3 elevation-tiles-prod, October 2026, 9–25
/// tiles per zoom around the Drakensberg, the Magaliesberg, the Khumbu, Tasmania and Patagonia
/// (docs/design/explorer.md, Offline regions).
pub fn tile_bytes_typical(z: u8) -> f64 {
    let kb = match z {
        0..=8 => 82.0,
        9 => 74.0,
        10 => 61.0,
        11 => 53.0,
        12 => 44.0,
        _ => 35.0,
    };
    kb * 1024.0
}

/// Mean Terrarium tile size (bytes) per zoom over high-resolution sources (US 3DEP, European
/// DEMs): the Swiss Alps, the Colorado Rockies, the Scottish Highlands and Kansas, measured as
/// `tile_bytes_typical`. The largest tile seen was 156 KB.
pub fn tile_bytes_high(z: u8) -> f64 {
    let kb = match z {
        0..=8 => 82.0,
        9 => 102.0,
        10 => 94.0,
        11 => 127.0,
        12 => 117.0,
        _ => 103.0,
    };
    kb * 1024.0
}

/// Why a store call was refused.
#[derive(Debug, Clone, PartialEq)]
pub enum TerrainError {
    /// Invalid tile coordinates or arguments.
    Argument(String),
    /// The PNG could not be decoded (the tile is then marked missing).
    Decode(String),
    /// Not in this build yet.
    NotImplemented,
}

/// Answer status of every query.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Status {
    Ok,
    /// Tiles listed in `need_tiles` are needed; the answer holds what could be had.
    NeedDem,
    Failed,
}

/// `mm_terrain_stats`.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct TerrainStats {
    pub status: Status,
    pub tiles: usize,
    pub missing: usize,
    pub bytes: u64,
    pub max_bytes: u64,
    pub evicted: u64,
}

/// `mm_terrain_elevations` request.
#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct ElevationsRequest {
    /// `[lat, lon]` pairs.
    pub points: Vec<[f64; 2]>,
    pub max_zoom: u8,
    pub min_zoom: Option<u8>,
    pub max_tiles: usize,
}

impl Default for ElevationsRequest {
    fn default() -> Self {
        ElevationsRequest {
            points: Vec::new(),
            max_zoom: DEFAULT_MAX_ZOOM,
            min_zoom: None,
            max_tiles: DEFAULT_MAX_TILES,
        }
    }
}

/// `mm_terrain_elevations` answer.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ElevationsResponse {
    pub status: Status,
    pub elevations_m: Vec<Option<f64>>,
    pub zoom: Option<u8>,
    pub need_tiles: Vec<TileId>,
}

/// `mm_terrain_profile` request.
#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct ProfileRequest {
    /// `[lat, lon]` pairs, at least one.
    pub points: Vec<[f64; 2]>,
    pub max_zoom: u8,
    pub min_zoom: Option<u8>,
    /// Sampling step along the line (metres); None: half a DEM cell of the zoom used.
    pub spacing_m: Option<f64>,
    /// Gaussian sigma of the smoothing along the line (metres); None: one DEM cell.
    pub smoothing_m: Option<f64>,
    pub hysteresis_m: f64,
    pub max_samples: usize,
    pub max_tiles: usize,
}

impl Default for ProfileRequest {
    fn default() -> Self {
        ProfileRequest {
            points: Vec::new(),
            max_zoom: DEFAULT_MAX_ZOOM,
            min_zoom: None,
            spacing_m: None,
            smoothing_m: None,
            hysteresis_m: DEFAULT_HYSTERESIS_M,
            max_samples: DEFAULT_MAX_SAMPLES,
            max_tiles: DEFAULT_MAX_TILES,
        }
    }
}

/// `mm_terrain_profile` answer.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ProfileResponse {
    pub status: Status,
    pub distance_m: f64,
    pub ascent_m: Option<f64>,
    pub descent_m: Option<f64>,
    pub min_m: Option<f64>,
    pub max_m: Option<f64>,
    /// `(distance along the line, elevation)`, serialised as `[d, e|null]`.
    pub samples: Vec<(f64, Option<f64>)>,
    pub vertex_distance_m: Vec<f64>,
    pub vertex_elevation_m: Vec<Option<f64>>,
    pub zoom: Option<u8>,
    pub missing_fraction: f64,
    pub need_tiles: Vec<TileId>,
}

/// `mm_terrain_grid` request.
#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct GridRequest {
    /// `[min_x, min_y, max_x, max_y]`, Web Mercator metres.
    pub rect: [f64; 4],
    pub max_zoom: u8,
    pub min_zoom: Option<u8>,
    pub max_tiles: usize,
}

impl Default for GridRequest {
    fn default() -> Self {
        GridRequest { rect: [0.0; 4], max_zoom: DEFAULT_MAX_ZOOM, min_zoom: None, max_tiles: DEFAULT_MAX_TILES }
    }
}

/// `mm_terrain_grid` answer (the heights go into the caller's buffer).
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct GridResponse {
    pub status: Status,
    pub zoom: Option<u8>,
    pub min_m: Option<f64>,
    pub max_m: Option<f64>,
    pub missing_fraction: f64,
    pub need_tiles: Vec<TileId>,
}

/// Where a render's rectangle lives.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RenderSpace {
    /// A map's base-image pixels, through its georeference (`pixel_to_mercator` + `warp`).
    Map,
    /// Web Mercator metres.
    Mercator,
}

/// Colours of a render: the app's dark map or paper.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RenderStyle {
    Dark,
    Paper,
}

/// `mm_terrain_render` request.
#[derive(Debug, Clone, Deserialize)]
pub struct RenderRequest {
    pub space: RenderSpace,
    /// Map: `[x0, y0, x1, y1]` base-image pixels; mercator: `[min_x, min_y, max_x, max_y]`.
    pub rect: [f64; 4],
    /// Space `map`: the georeference's 3×3 row-major homography.
    #[serde(default)]
    pub pixel_to_mercator: Option<[f64; 9]>,
    /// Space `map`: the georeference's warp, exactly as the georeference response has it.
    #[serde(default)]
    pub warp: Option<serde_json::Value>,
    pub style: RenderStyle,
    #[serde(default)]
    pub interval_m: Option<f64>,
    #[serde(default)]
    pub scale_m_per_pt: Option<f64>,
    #[serde(default = "default_index_every")]
    pub index_every: u32,
    #[serde(default)]
    pub hillshade: f64,
    #[serde(default = "default_line_scale")]
    pub line_scale: f64,
    /// RGBA 0–255, straight alpha; None: the style's paper.
    #[serde(default)]
    pub background: Option<[u8; 4]>,
    #[serde(default = "default_max_zoom")]
    pub max_zoom: u8,
    #[serde(default)]
    pub min_zoom: Option<u8>,
    #[serde(default = "default_max_tiles")]
    pub max_tiles: usize,
}

fn default_index_every() -> u32 {
    DEFAULT_INDEX_EVERY
}

fn default_line_scale() -> f64 {
    1.0
}

fn default_max_zoom() -> u8 {
    DEFAULT_MAX_ZOOM
}

fn default_max_tiles() -> usize {
    DEFAULT_MAX_TILES
}

/// `mm_terrain_render` answer.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct RenderResponse {
    pub status: Status,
    pub interval_m: f64,
    pub index_interval_m: f64,
    pub zoom: Option<u8>,
    pub missing_fraction: f64,
    pub need_tiles: Vec<TileId>,
}

/// One area of a tile plan: exactly one of `bbox`, `circle`, `quad`.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct PlanArea {
    /// `[south, west, north, east]` degrees; west > east crosses the antimeridian.
    pub bbox: Option<[f64; 4]>,
    /// `[lat, lon, radius_m]`.
    pub circle: Option<[f64; 3]>,
    /// Four `[lat, lon]` corners (a map's TL, TR, BR, BL), grown by `margin_m`.
    pub quad: Option<[[f64; 2]; 4]>,
    pub margin_m: f64,
    pub zooms: Vec<u8>,
}

/// The trailhead preset of a tile plan: a located map's corners.
#[derive(Debug, Clone, Deserialize)]
pub struct PlanTrailhead {
    /// `[lat, lon]` TL, TR, BR, BL.
    pub corners: [[f64; 2]; 4],
}

/// `mm_tile_plan` request.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct PlanRequest {
    pub areas: Vec<PlanArea>,
    pub trailhead: Option<PlanTrailhead>,
}

/// The tiles of one zoom in a plan.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct PlanZoom {
    pub z: u8,
    pub tiles: usize,
    pub bytes: f64,
    pub bytes_high: f64,
    /// Disjoint inclusive tile rectangles `[x0, x1, y0, y1]` covering exactly the zoom's tiles.
    pub ranges: Vec<[u32; 4]>,
}

/// A zoom the trailhead preset left out.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct PlanDropped {
    pub z: u8,
    pub tiles: usize,
    pub reason: String,
}

/// `mm_tile_plan` answer.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct PlanResponse {
    pub status: Status,
    pub tiles: usize,
    pub bytes: f64,
    pub bytes_high: f64,
    pub zooms: Vec<PlanZoom>,
    pub dropped: Vec<PlanDropped>,
}

/// The store behind `MMTerrain`. Every method takes `&self` and is safe to call from many
/// threads at once (the implementation locks internally).
#[derive(Debug)]
pub struct TerrainStore {
    max_bytes: u64,
}

impl TerrainStore {
    /// A store holding at most `max_bytes` of decoded elevations (0: `DEFAULT_MAX_BYTES`; others
    /// clamped to `MIN_MAX_BYTES ..= MAX_MAX_BYTES`).
    pub fn new(max_bytes: u64) -> TerrainStore {
        let max_bytes = if max_bytes == 0 { DEFAULT_MAX_BYTES } else { max_bytes.clamp(MIN_MAX_BYTES, MAX_MAX_BYTES) };
        TerrainStore { max_bytes }
    }

    pub fn max_bytes(&self) -> u64 {
        self.max_bytes
    }

    /// Add a Terrarium PNG tile (replaces one held, clears a missing mark).
    pub fn add_png(&self, t: TileId, png: &[u8]) -> Result<(), TerrainError> {
        check_tile(t)?;
        if png.is_empty() {
            return Err(TerrainError::Argument("empty PNG".into()));
        }
        Err(TerrainError::NotImplemented)
    }

    /// Record that a tile is unavailable.
    pub fn mark_missing(&self, t: TileId) -> Result<(), TerrainError> {
        check_tile(t)?;
        Err(TerrainError::NotImplemented)
    }

    /// Forget every missing mark.
    pub fn clear_missing(&self) {}

    pub fn stats(&self) -> TerrainStats {
        TerrainStats { status: Status::Ok, tiles: 0, missing: 0, bytes: 0, max_bytes: self.max_bytes, evicted: 0 }
    }

    pub fn elevations(&self, _req: &ElevationsRequest) -> Result<ElevationsResponse, String> {
        Err(format!("{NOT_IMPLEMENTED}: mm_terrain_elevations"))
    }

    pub fn profile(&self, _req: &ProfileRequest) -> Result<ProfileResponse, String> {
        Err(format!("{NOT_IMPLEMENTED}: mm_terrain_profile"))
    }

    /// Fill `heights` (`cols × rows`, row 0 north) over the request's rectangle.
    pub fn grid(
        &self,
        _req: &GridRequest,
        _heights: &mut [f32],
        _cols: usize,
        _rows: usize,
    ) -> Result<GridResponse, String> {
        Err(format!("{NOT_IMPLEMENTED}: mm_terrain_grid"))
    }

    /// Render into `rgba` (`w × h` pixels, rows `stride` bytes apart).
    pub fn render(
        &self,
        _req: &RenderRequest,
        _rgba: &mut [u8],
        _w: usize,
        _h: usize,
        _stride: usize,
    ) -> Result<RenderResponse, String> {
        Err(format!("{NOT_IMPLEMENTED}: mm_terrain_render"))
    }
}

/// The tiles covering a plan's areas (stateless).
pub fn tile_plan(_req: &PlanRequest) -> Result<PlanResponse, String> {
    Err(format!("{NOT_IMPLEMENTED}: mm_tile_plan"))
}

/// Tile coordinates within zoom `MAX_ZOOM` and the zoom's range.
pub fn check_tile(t: TileId) -> Result<(), TerrainError> {
    if t.z > MAX_ZOOM {
        return Err(TerrainError::Argument(format!("zoom {} above {MAX_ZOOM}", t.z)));
    }
    let n = 1u64 << t.z;
    if t.x as u64 >= n || t.y as u64 >= n {
        return Err(TerrainError::Argument(format!("tile {}/{}/{} outside the zoom's range", t.z, t.x, t.y)));
    }
    Ok(())
}

/// Serialise an answer, or a failed response for an error.
pub fn answer_json<T: Serialize>(answer: Result<T, String>) -> String {
    match answer {
        Ok(a) => serde_json::to_string(&a)
            .unwrap_or_else(|e| crate::georef::failed_json(&format!("Could not encode the response: {e}"))),
        Err(message) => crate::georef::failed_json(&message),
    }
}

/// Parse a request, or the failed response to give for it.
pub fn parse_request<'a, T: Deserialize<'a>>(json: &'a str) -> Result<T, String> {
    serde_json::from_str::<T>(json).map_err(|e| format!("Invalid request: {e}"))
}

/// `mm_tile_plan` on JSON.
pub fn tile_plan_json(json: &str) -> String {
    answer_json(parse_request::<PlanRequest>(json).and_then(|req| tile_plan(&req)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn interval_rule() {
        assert_eq!(auto_interval_m(1.0), 5.0);
        assert_eq!(auto_interval_m(2.5), 10.0);
        assert_eq!(auto_interval_m(5.0), 20.0);
        assert_eq!(auto_interval_m(6.0), 25.0);
        assert_eq!(auto_interval_m(10.0), 50.0);
        assert_eq!(auto_interval_m(25.0), 100.0);
        assert_eq!(auto_interval_m(1e6), 1000.0);
        assert_eq!(auto_interval_m(f64::NAN), 5.0);
    }

    #[test]
    fn continuation_margin_rule() {
        assert_eq!(continuation_margin_m(2_000.0), 3_000.0);
        assert_eq!(continuation_margin_m(10_000.0), 5_000.0);
        assert_eq!(continuation_margin_m(150_000.0), 15_000.0);
        assert_eq!(continuation_margin_m(f64::INFINITY), 3_000.0);
    }

    #[test]
    fn byte_budget_is_clamped() {
        assert_eq!(TerrainStore::new(0).max_bytes(), DEFAULT_MAX_BYTES);
        assert_eq!(TerrainStore::new(1).max_bytes(), MIN_MAX_BYTES);
        assert_eq!(TerrainStore::new(u64::MAX).max_bytes(), MAX_MAX_BYTES);
    }

    #[test]
    fn tile_checks() {
        assert!(check_tile(TileId { z: 13, x: 8191, y: 0 }).is_ok());
        assert!(check_tile(TileId { z: 13, x: 8192, y: 0 }).is_err());
        assert!(check_tile(TileId { z: 16, x: 0, y: 0 }).is_err());
    }

    #[test]
    fn requests_take_defaults() {
        let p: ProfileRequest = parse_request(r#"{"points":[[-25.8,27.4],[-25.81,27.41]]}"#).unwrap();
        assert_eq!(p.points.len(), 2);
        assert_eq!(p.max_zoom, DEFAULT_MAX_ZOOM);
        assert_eq!(p.hysteresis_m, DEFAULT_HYSTERESIS_M);
        assert_eq!(p.max_samples, DEFAULT_MAX_SAMPLES);
        let r: RenderRequest = parse_request(r#"{"space":"map","rect":[0,0,512,512],"style":"dark"}"#).unwrap();
        assert_eq!(r.space, RenderSpace::Map);
        assert_eq!(r.index_every, DEFAULT_INDEX_EVERY);
        assert_eq!(r.line_scale, 1.0);
        assert!(parse_request::<RenderRequest>(r#"{"space":"globe","rect":[0,0,1,1],"style":"dark"}"#).is_err());
        let plan: PlanRequest =
            parse_request(r#"{"areas":[{"bbox":[-30.7,27.0,-28.6,29.5],"zooms":[8,9,10,11,12]}]}"#).unwrap();
        assert_eq!(plan.areas[0].zooms, vec![8, 9, 10, 11, 12]);
    }
}
