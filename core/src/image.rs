//! Minimal raster buffers and filters used by the georeferencing pipeline.

use rayon::prelude::*;

/// Borrowed 8-bit RGBA (alpha ignored) image with an arbitrary row stride.
#[derive(Clone, Copy)]
pub struct RgbaView<'a> {
    pub data: &'a [u8],
    pub width: usize,
    pub height: usize,
    pub stride: usize,
}

impl<'a> RgbaView<'a> {
    pub fn new(data: &'a [u8], width: usize, height: usize, stride: usize) -> Option<Self> {
        if width == 0 || height == 0 || stride < width * 4 || data.len() < stride * (height - 1) + width * 4 {
            return None;
        }
        Some(Self { data, width, height, stride })
    }

    #[inline]
    pub fn rgb(&self, x: usize, y: usize) -> [u8; 3] {
        let i = y * self.stride + x * 4;
        [self.data[i], self.data[i + 1], self.data[i + 2]]
    }
}

/// Owned packed RGB8 image (3 bytes per pixel, no padding).
#[derive(Clone)]
pub struct Rgb8 {
    pub w: usize,
    pub h: usize,
    pub data: Vec<u8>,
}

impl Rgb8 {
    pub fn from_rgba(v: &RgbaView) -> Rgb8 {
        let mut data = vec![0u8; v.width * v.height * 3];
        data.par_chunks_mut(v.width * 3).enumerate().for_each(|(y, row)| {
            let src = &v.data[y * v.stride..y * v.stride + v.width * 4];
            for x in 0..v.width {
                row[x * 3] = src[x * 4];
                row[x * 3 + 1] = src[x * 4 + 1];
                row[x * 3 + 2] = src[x * 4 + 2];
            }
        });
        Rgb8 { w: v.width, h: v.height, data }
    }

    /// From packed RGB bytes (w*h*3).
    pub fn from_rgb(w: usize, h: usize, data: Vec<u8>) -> Option<Rgb8> {
        if data.len() != w * h * 3 {
            return None;
        }
        Some(Rgb8 { w, h, data })
    }

    #[inline]
    pub fn rgb(&self, x: usize, y: usize) -> [u8; 3] {
        let i = (y * self.w + x) * 3;
        [self.data[i], self.data[i + 1], self.data[i + 2]]
    }
}

/// Binary mask (0 or 1 per pixel).
#[derive(Clone)]
pub struct Mask {
    pub w: usize,
    pub h: usize,
    pub data: Vec<u8>,
}

impl Mask {
    pub fn new(w: usize, h: usize) -> Mask {
        Mask { w, h, data: vec![0; w * h] }
    }
    #[inline]
    pub fn get(&self, x: usize, y: usize) -> bool {
        self.data[y * self.w + x] != 0
    }
    #[inline]
    pub fn set(&mut self, x: usize, y: usize, v: bool) {
        self.data[y * self.w + x] = v as u8;
    }
    pub fn count(&self) -> usize {
        self.data.iter().map(|&v| v as usize).sum()
    }
    pub fn fraction(&self) -> f64 {
        self.count() as f64 / (self.w * self.h).max(1) as f64
    }
    /// Area-average downsample to a density image of size (nw, nh).
    pub fn to_density(&self, nw: usize, nh: usize) -> GrayF32 {
        let g = GrayF32 { w: self.w, h: self.h, data: self.data.iter().map(|&v| v as f32).collect() };
        g.resize_area(nw, nh)
    }
}

/// Single-channel f32 image.
#[derive(Clone, Debug)]
pub struct GrayF32 {
    pub w: usize,
    pub h: usize,
    pub data: Vec<f32>,
}

impl GrayF32 {
    pub fn new(w: usize, h: usize) -> GrayF32 {
        GrayF32 { w, h, data: vec![0.0; w * h] }
    }

    pub fn filled(w: usize, h: usize, v: f32) -> GrayF32 {
        GrayF32 { w, h, data: vec![v; w * h] }
    }

    #[inline]
    pub fn get(&self, x: usize, y: usize) -> f32 {
        self.data[y * self.w + x]
    }

    #[inline]
    pub fn set(&mut self, x: usize, y: usize, v: f32) {
        self.data[y * self.w + x] = v;
    }

    #[inline]
    pub fn get_clamped(&self, x: isize, y: isize) -> f32 {
        let x = x.clamp(0, self.w as isize - 1) as usize;
        let y = y.clamp(0, self.h as isize - 1) as usize;
        self.data[y * self.w + x]
    }

    /// Bilinear sample at continuous coords where integer coords are pixel
    /// centres. Returns `None` outside [0, w-1]×[0, h-1].
    #[inline]
    pub fn bilinear(&self, x: f64, y: f64) -> Option<f32> {
        if !(x >= 0.0 && y >= 0.0 && x <= (self.w - 1) as f64 && y <= (self.h - 1) as f64) {
            return None;
        }
        let x0 = (x.floor() as usize).min(self.w.saturating_sub(2));
        let y0 = (y.floor() as usize).min(self.h.saturating_sub(2));
        let fx = (x - x0 as f64) as f32;
        let fy = (y - y0 as f64) as f32;
        let x1 = (x0 + 1).min(self.w - 1);
        let y1 = (y0 + 1).min(self.h - 1);
        let a = self.get(x0, y0);
        let b = self.get(x1, y0);
        let c = self.get(x0, y1);
        let d = self.get(x1, y1);
        Some(a * (1.0 - fx) * (1.0 - fy) + b * fx * (1.0 - fy) + c * (1.0 - fx) * fy + d * fx * fy)
    }

    /// Box-filter (area-average) resize; best for downsampling.
    pub fn resize_area(&self, nw: usize, nh: usize) -> GrayF32 {
        let nw = nw.max(1);
        let nh = nh.max(1);
        let sx = self.w as f64 / nw as f64;
        let sy = self.h as f64 / nh as f64;
        if sx < 1.0 || sy < 1.0 {
            return self.resize_bilinear(nw, nh);
        }
        // Horizontal pass.
        let mut tmp = vec![0.0f32; nw * self.h];
        tmp.par_chunks_mut(nw).enumerate().for_each(|(y, row)| {
            let src = &self.data[y * self.w..(y + 1) * self.w];
            for (ox, out) in row.iter_mut().enumerate() {
                let a = ox as f64 * sx;
                let b = a + sx;
                *out = (box_sum(src, a, b) / sx) as f32;
            }
        });
        // Vertical pass.
        let mut out = vec![0.0f32; nw * nh];
        out.par_chunks_mut(nw).enumerate().for_each(|(oy, row)| {
            let a = oy as f64 * sy;
            let b = a + sy;
            let i0 = a.floor() as usize;
            let i1 = (b.ceil() as usize).min(self.h);
            for i in i0..i1 {
                let lo = (i as f64).max(a);
                let hi = ((i + 1) as f64).min(b);
                let wgt = ((hi - lo).max(0.0) / sy) as f32;
                if wgt > 0.0 {
                    let src = &tmp[i * nw..(i + 1) * nw];
                    for (o, s) in row.iter_mut().zip(src) {
                        *o += wgt * s;
                    }
                }
            }
        });
        GrayF32 { w: nw, h: nh, data: out }
    }

    /// Bilinear resize (pixel-centre aligned).
    pub fn resize_bilinear(&self, nw: usize, nh: usize) -> GrayF32 {
        let sx = self.w as f64 / nw as f64;
        let sy = self.h as f64 / nh as f64;
        let mut out = vec![0.0f32; nw * nh];
        out.par_chunks_mut(nw).enumerate().for_each(|(oy, row)| {
            let y = ((oy as f64 + 0.5) * sy - 0.5).clamp(0.0, (self.h - 1) as f64);
            for (ox, o) in row.iter_mut().enumerate() {
                let x = ((ox as f64 + 0.5) * sx - 0.5).clamp(0.0, (self.w - 1) as f64);
                *o = self.bilinear(x, y).unwrap_or(0.0);
            }
        });
        GrayF32 { w: nw, h: nh, data: out }
    }

    /// Separable Gaussian blur with clamped borders. sigma ≤ 0 returns a copy.
    pub fn gaussian_blur(&self, sigma: f64) -> GrayF32 {
        if sigma <= 0.0 {
            return self.clone();
        }
        let k = gaussian_kernel(sigma);
        let r = (k.len() / 2) as isize;
        let (w, h) = (self.w, self.h);
        let mut tmp = vec![0.0f32; w * h];
        tmp.par_chunks_mut(w).enumerate().for_each(|(y, row)| {
            let src = &self.data[y * w..(y + 1) * w];
            for (x, o) in row.iter_mut().enumerate() {
                let mut s = 0.0f32;
                for (i, kv) in k.iter().enumerate() {
                    let xx = (x as isize + i as isize - r).clamp(0, w as isize - 1) as usize;
                    s += kv * src[xx];
                }
                *o = s;
            }
        });
        let mut out = vec![0.0f32; w * h];
        out.par_chunks_mut(w).enumerate().for_each(|(y, row)| {
            for (i, kv) in k.iter().enumerate() {
                let yy = (y as isize + i as isize - r).clamp(0, h as isize - 1) as usize;
                let src = &tmp[yy * w..(yy + 1) * w];
                for (o, s) in row.iter_mut().zip(src) {
                    *o += kv * s;
                }
            }
        });
        GrayF32 { w, h, data: out }
    }

    /// 3×3 Sobel gradients (gx, gy), clamped borders. gy is positive downwards.
    pub fn sobel(&self) -> (GrayF32, GrayF32) {
        let (w, h) = (self.w, self.h);
        let mut gx = vec![0.0f32; w * h];
        let mut gy = vec![0.0f32; w * h];
        gx.par_chunks_mut(w).zip(gy.par_chunks_mut(w)).enumerate().for_each(|(y, (rx, ry))| {
            let yi = y as isize;
            for x in 0..w {
                let xi = x as isize;
                let p = |dx: isize, dy: isize| self.get_clamped(xi + dx, yi + dy);
                rx[x] = (p(1, -1) + 2.0 * p(1, 0) + p(1, 1)) - (p(-1, -1) + 2.0 * p(-1, 0) + p(-1, 1));
                ry[x] = (p(-1, 1) + 2.0 * p(0, 1) + p(1, 1)) - (p(-1, -1) + 2.0 * p(0, -1) + p(1, -1));
            }
        });
        (GrayF32 { w, h, data: gx }, GrayF32 { w, h, data: gy })
    }

    /// Central-difference gradient (per pixel), clamped borders.
    pub fn gradient(&self) -> (GrayF32, GrayF32) {
        let (w, h) = (self.w, self.h);
        let mut gx = vec![0.0f32; w * h];
        let mut gy = vec![0.0f32; w * h];
        gx.par_chunks_mut(w).zip(gy.par_chunks_mut(w)).enumerate().for_each(|(y, (rx, ry))| {
            let yi = y as isize;
            for x in 0..w {
                let xi = x as isize;
                rx[x] = 0.5 * (self.get_clamped(xi + 1, yi) - self.get_clamped(xi - 1, yi));
                ry[x] = 0.5 * (self.get_clamped(xi, yi + 1) - self.get_clamped(xi, yi - 1));
            }
        });
        (GrayF32 { w, h, data: gx }, GrayF32 { w, h, data: gy })
    }

    pub fn percentile(&self, p: f64, positive_only: bool) -> f32 {
        let mut v: Vec<f32> =
            self.data.iter().copied().filter(|x| x.is_finite() && (!positive_only || *x > 0.0)).collect();
        if v.is_empty() {
            return 0.0;
        }
        let k = ((p / 100.0) * (v.len() - 1) as f64).round() as usize;
        let (_, kth, _) = v.select_nth_unstable_by(k, |a, b| a.partial_cmp(b).unwrap());
        *kth
    }
}

fn box_sum(src: &[f32], a: f64, b: f64) -> f64 {
    let i0 = a.floor() as usize;
    let i1 = (b.ceil() as usize).min(src.len());
    let mut s = 0.0f64;
    for (i, &v) in src.iter().enumerate().take(i1).skip(i0) {
        let lo = (i as f64).max(a);
        let hi = ((i + 1) as f64).min(b);
        s += (hi - lo).max(0.0) * v as f64;
    }
    s
}

pub fn gaussian_kernel(sigma: f64) -> Vec<f32> {
    let r = (3.0 * sigma).ceil().max(1.0) as isize;
    let mut k: Vec<f32> = (-r..=r).map(|i| (-(i * i) as f64 / (2.0 * sigma * sigma)).exp() as f32).collect();
    let s: f32 = k.iter().sum();
    for v in k.iter_mut() {
        *v /= s;
    }
    k
}

/// RGB (0-255) → HSV with h in degrees [0,360), s and v in [0,1].
#[inline]
pub fn rgb_to_hsv(r: u8, g: u8, b: u8) -> (f32, f32, f32) {
    let (r, g, b) = (r as f32 / 255.0, g as f32 / 255.0, b as f32 / 255.0);
    let mx = r.max(g).max(b);
    let mn = r.min(g).min(b);
    let d = mx - mn;
    let h = if d <= 1e-6 {
        0.0
    } else if mx == r {
        60.0 * (((g - b) / d) % 6.0)
    } else if mx == g {
        60.0 * ((b - r) / d + 2.0)
    } else {
        60.0 * ((r - g) / d + 4.0)
    };
    let h = if h < 0.0 { h + 360.0 } else { h };
    let s = if mx <= 1e-6 { 0.0 } else { d / mx };
    (h, s, mx)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resize_area_preserves_mean() {
        let mut g = GrayF32::new(100, 60);
        for (i, v) in g.data.iter_mut().enumerate() {
            *v = (i % 7) as f32;
        }
        let mean: f32 = g.data.iter().sum::<f32>() / g.data.len() as f32;
        let r = g.resize_area(33, 17);
        let m2: f32 = r.data.iter().sum::<f32>() / r.data.len() as f32;
        assert!((mean - m2).abs() < 0.05, "{mean} {m2}");
    }

    #[test]
    fn blur_keeps_constant() {
        let g = GrayF32::filled(20, 10, 3.0);
        let b = g.gaussian_blur(2.0);
        assert!(b.data.iter().all(|v| (v - 3.0).abs() < 1e-4));
    }

    #[test]
    fn sobel_on_ramp() {
        let mut g = GrayF32::new(10, 10);
        for y in 0..10 {
            for x in 0..10 {
                g.set(x, y, x as f32 * 2.0 + y as f32);
            }
        }
        let (gx, gy) = g.sobel();
        assert!((gx.get(5, 5) - 16.0).abs() < 1e-4); // 2 * 8
        assert!((gy.get(5, 5) - 8.0).abs() < 1e-4);
    }

    #[test]
    fn hsv_brown() {
        let (h, s, v) = rgb_to_hsv(168, 152, 136);
        assert!((h - 30.0).abs() < 1.0 && (s - 0.19).abs() < 0.01 && (v - 0.659).abs() < 0.01);
    }
}
