//! Fine alignment: make every contour segment have constant DEM elevation.
//!
//! OWNER: contours agent. Contract (may be extended, not broken):

use crate::dem::DemMosaic;
use crate::geo::Affine;
use crate::skeleton::ContourSamples;

#[derive(Debug, Clone)]
pub struct RefineResult {
    /// Pixel → Mercator transform after refinement.
    pub transform: Affine,
    /// Robust mean absolute within-segment elevation residual (metres).
    pub cost_m: f64,
    /// Median over segments of the elevation standard deviation (metres).
    pub median_seg_std_m: f64,
    /// Fraction of samples with |residual| < 2.5 m (or similar threshold).
    pub inlier_fraction: f64,
    /// Fraction of samples that landed on valid DEM.
    pub dem_coverage: f64,
    pub iterations: usize,
    pub converged: bool,
}

/// Levenberg–Marquardt on similarity then affine parameters, starting at `init`.
/// `dem` is a fine mosaic (zoom 13) covering the map footprint plus margin.
pub fn refine(_samples: &ContourSamples, _dem: &DemMosaic, _init: &Affine) -> RefineResult {
    todo!("contours agent")
}

/// Evaluate a transform without optimising (used to verify grid results).
pub fn evaluate(_samples: &ContourSamples, _dem: &DemMosaic, _t: &Affine) -> RefineResult {
    todo!("contours agent")
}
