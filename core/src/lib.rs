//! mapme-core: georeferencing engine for MapMe.
//!
//! Given a raster map, OCR text and an optional GPS fix, work out the
//! pixel → Web Mercator transform, by reading grid labels or by matching the
//! map's contour lines against a digital elevation model. See
//! `docs/ARCHITECTURE.md`.

pub mod contour;
pub mod control_points;
pub mod dem;
pub mod ffi;
pub mod fft;
pub mod geo;
pub mod georef;
pub mod grid;
pub mod image;
pub mod labels;
pub mod page;
pub mod refine;
pub mod search;
pub mod skeleton;
pub mod warp;

pub const VERSION: &str = env!("CARGO_PKG_VERSION");
