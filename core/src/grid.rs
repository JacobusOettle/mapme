//! Grid / graticule georeferencing from straight grid lines + OCR labels.
//!
//! OWNER: grid agent. Contract (may be extended, not broken):

use crate::geo::{Homography, LatLon};
use crate::image::Rgb8;
use crate::labels::OcrObservation;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LineAxis {
    /// Roughly horizontal in the image (parallels / northings).
    Horizontal,
    /// Roughly vertical in the image (meridians / eastings).
    Vertical,
}

/// A long straight line: points p with p·n = rho, n = (cos θ, sin θ).
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct GridLine {
    pub axis: LineAxis,
    pub theta: f64,
    pub rho: f64,
    /// Fraction of the image span covered by line pixels.
    pub coverage: f64,
}

/// Detect long straight thin lines within ±5° of horizontal/vertical.
pub fn detect_lines(_img: &Rgb8) -> Vec<GridLine> {
    todo!("grid agent")
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Gcp {
    pub x: f64,
    pub y: f64,
    pub lat: f64,
    pub lon: f64,
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GridMethod {
    /// Labels matched to detected grid lines (accurate).
    Lines,
    /// Labels only (no lines found): coarse, should be refined.
    LabelsOnly,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GridResult {
    pub transform: Homography,
    pub method: GridMethod,
    /// RMS residual of the GCP fit in ground metres.
    pub rms_m: f64,
    pub n_lat: usize,
    pub n_lon: usize,
    pub gcps: Vec<Gcp>,
    /// OCR observations (indices) that were used as coordinate labels.
    pub used_labels: Vec<usize>,
}

/// Full grid pipeline. `gps` resolves UTM zone / abbreviated grid labels.
pub fn georeference_from_grid(
    _img: &Rgb8,
    _lines: &[GridLine],
    _obs: &[OcrObservation],
    _gps: Option<LatLon>,
) -> Option<GridResult> {
    todo!("grid agent")
}
