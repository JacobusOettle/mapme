//! Coarse global search: orientation-field FFT correlation of map contours vs DEM.
//!
//! OWNER: contours agent. Contract (may be extended, not broken):

use crate::dem::{DemMosaic, MercBox};
use crate::geo::Affine;
use crate::image::Mask;

/// Doubled-angle orientation field derived from the contour mask at a
/// reduced working resolution.
#[derive(Clone)]
pub struct MapField {
    /// Field size in cells.
    pub w: usize,
    pub h: usize,
    /// Base-image pixels per field cell.
    pub px_per_cell: f64,
    /// Raw structure-tensor components (Jxx−Jyy, 2Jxy) before smoothing to the template scale.
    pub re: Vec<f32>,
    pub im: Vec<f32>,
}

/// Build the map field from the full-resolution contour mask.
pub fn map_field(_mask: &Mask) -> MapField {
    todo!("contours agent")
}

#[derive(Debug, Clone)]
pub struct SearchParams {
    /// GPS position in Mercator metres.
    pub center_merc: (f64, f64),
    /// Ground-metre radius around the GPS position the map centre may lie in.
    pub search_radius_m: f64,
    /// Ground metres per base-image pixel range to scan.
    pub scale_min: f64,
    pub scale_max: f64,
    /// Map-up bearings to try, degrees (default [0.0]).
    pub rotations_deg: Vec<f64>,
    /// Max number of distinct candidates to return.
    pub top_k: usize,
}

#[derive(Debug, Clone)]
pub struct Candidate {
    /// Pixel → Mercator similarity.
    pub transform: Affine,
    /// Normalised correlation score of the peak.
    pub score: f32,
    /// Peak score / best score at a location ≥ 2 km away (any scale).
    pub peak_ratio: f32,
    /// Ground metres per pixel.
    pub scale_m_per_px: f64,
    pub rotation_deg: f64,
}

/// Mercator box the coarse DEM must cover for these params and image size.
pub fn required_dem_box(_params: &SearchParams, _img_w: usize, _img_h: usize) -> MercBox {
    todo!("contours agent")
}

/// Run the coarse search. `dem` is a coarse mosaic (zoom 11, ~76 m) covering
/// `required_dem_box`. Returns up to `top_k` candidates, best first.
/// `cancel()` is polled between scales; `progress(f)` receives 0..1.
pub fn coarse_search(
    _field: &MapField,
    _dem: &DemMosaic,
    _img_w: usize,
    _img_h: usize,
    _params: &SearchParams,
    _cancel: &(dyn Fn() -> bool + Sync),
    _progress: &(dyn Fn(f32) + Sync),
) -> Vec<Candidate> {
    todo!("contours agent")
}
