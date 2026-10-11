//! Manual calibration from user-picked control points.
//!
//! OWNER: grid agent. Contract (may be extended, not broken):

use crate::geo::Homography;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct ControlPoint {
    pub x: f64,
    pub y: f64,
    pub lat: f64,
    pub lon: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ControlFit {
    pub transform: Homography,
    /// "similarity" | "affine" | "homography"
    pub model: String,
    pub rms_m: f64,
    pub residuals_m: Vec<f64>,
}

/// `model`: "auto" (2 → similarity, 3+ → affine), "similarity", "affine", "homography" (≥4).
pub fn fit(_points: &[ControlPoint], _model: &str) -> Result<ControlFit, String> {
    todo!("grid agent")
}
