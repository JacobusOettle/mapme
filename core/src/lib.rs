//! mapme-core: georeferencing engine for MapMe.
//!
//! Given a raster map, OCR text and an optional GPS fix, work out the
//! pixel → Web Mercator transform, by reading grid labels or by matching the
//! map's contour lines against a digital elevation model. See
//! `docs/ARCHITECTURE.md`.

pub mod content;
pub mod contour;
pub mod contour_lines;
pub mod control_points;
pub mod dark;
pub mod dem;
pub mod ffi;
pub mod fft;
pub mod geo;
pub mod georef;
pub mod grid;
pub mod hints;
pub mod image;
pub mod labels;
pub mod page;
pub mod refine;
pub mod scalebar;
pub mod search;
pub mod skeleton;
pub mod terrain;
pub mod warp;
pub mod wide;

pub const VERSION: &str = env!("CARGO_PKG_VERSION");
