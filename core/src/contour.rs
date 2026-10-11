//! Contour-line pixel classification.
//!
//! OWNER: contours agent. Contract (may be extended, not broken):

use crate::image::{Mask, Rgb8};
use serde::{Deserialize, Serialize};

/// Colour model deciding which pixels belong to contour lines.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ContourColorModel {
    /// Hue range in degrees (may wrap if min > max).
    pub hue_min: f32,
    pub hue_max: f32,
    pub sat_min: f32,
    pub sat_max: f32,
    pub val_min: f32,
    pub val_max: f32,
}

/// Default model: brown / tan / orange contours (most topographic maps).
pub fn default_model() -> ContourColorModel {
    todo!("contours agent")
}

/// Model centred on a user-picked contour colour.
pub fn model_from_rgb(_rgb: [u8; 3]) -> ContourColorModel {
    todo!("contours agent")
}

/// Classify every pixel; 1 = contour.
pub fn extract_mask(_img: &Rgb8, _model: &ContourColorModel) -> Mask {
    todo!("contours agent")
}

/// Try a few built-in models and keep the one whose mask looks most like
/// contour lines. Returns the mask and the model used.
pub fn auto_mask(_img: &Rgb8) -> (Mask, ContourColorModel) {
    todo!("contours agent")
}
