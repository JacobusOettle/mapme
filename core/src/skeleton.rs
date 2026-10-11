//! Thinning and contour segment extraction.
//!
//! OWNER: contours agent. Contract (may be extended, not broken):

use crate::image::Mask;

/// Points sampled along contour segments, in base-image pixel coordinates.
/// `seg[i]` is the segment id of `pts[i]`; ids are 0..n_segs.
#[derive(Debug, Clone, Default)]
pub struct ContourSamples {
    pub pts: Vec<[f32; 2]>,
    pub seg: Vec<u32>,
    pub n_segs: usize,
}

/// Skeletonise the mask, break it at junctions, keep segments with at least
/// `min_len` pixels and sample every `step`-th pixel.
pub fn contour_samples(_mask: &Mask, _min_len: usize, _step: usize) -> ContourSamples {
    todo!("contours agent")
}
