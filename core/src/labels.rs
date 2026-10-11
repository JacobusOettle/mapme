//! OCR text parsing: coordinate labels, map scale, POI names.
//!
//! OWNER: grid agent. Contract (may be extended, not broken):

use serde::{Deserialize, Serialize};

/// One text observation from the platform OCR (Apple Vision / ML Kit).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct OcrObservation {
    pub text: String,
    /// [x, y, w, h] in base-image pixels, top-left origin.
    pub bbox: [f64; 4],
    #[serde(default = "default_conf")]
    pub confidence: f32,
}

fn default_conf() -> f32 {
    0.5
}

impl OcrObservation {
    pub fn center(&self) -> (f64, f64) {
        (self.bbox[0] + self.bbox[2] / 2.0, self.bbox[1] + self.bbox[3] / 2.0)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub enum CoordValue {
    /// Latitude in signed degrees (south negative).
    Lat(f64),
    /// Longitude in signed degrees (west negative).
    Lon(f64),
    /// Ambiguous degrees with no hemisphere letter.
    Degrees(f64),
    /// Projected grid value in metres (easting or northing, unknown which).
    Grid(f64),
}

/// Parse a coordinate label: DMS (25°49'00"S, 25 49 00 S, 25°49′S, OCR
/// variants with *, º, ’, ”), decimal degrees (-25.8167, 25.8167°S) and
/// projected grid values (540000, 540 000mE, abbreviated "40").
pub fn parse_coordinate(_text: &str) -> Option<CoordValue> {
    todo!("grid agent")
}

/// Parse a representative fraction like "1:25 000" / "1 : 50,000" → 25000.
pub fn parse_scale(_text: &str) -> Option<f64> {
    todo!("grid agent")
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PoiKind {
    Place,
    Peak,
    SpotHeight,
    Water,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PoiLabel {
    pub name: String,
    pub kind: PoiKind,
    pub elevation_m: Option<f64>,
    /// Base-image pixel position.
    pub x: f64,
    pub y: f64,
}

/// Turn OCR text into POI candidates (place names, spot heights), skipping
/// coordinate labels, scale text and legend/title boilerplate.
pub fn classify_pois(_obs: &[OcrObservation]) -> Vec<PoiLabel> {
    todo!("grid agent")
}
