//! 2-D complex FFT helpers built on rustfft.

use rayon::prelude::*;
use rustfft::num_complex::Complex32;
use rustfft::{Fft, FftPlanner};
use std::sync::Arc;

pub use rustfft::num_complex::Complex32 as C32;

/// Smallest n' ≥ n whose prime factors are only 2, 3 and 5 (fast FFT sizes).
pub fn next_fast_len(n: usize) -> usize {
    let mut m = n.max(1);
    loop {
        let mut k = m;
        for p in [2, 3, 5] {
            while k.is_multiple_of(p) {
                k /= p;
            }
        }
        if k == 1 {
            return m;
        }
        m += 1;
    }
}

/// Planned 2-D FFT of a fixed size (row-major, `w` columns × `h` rows).
pub struct Fft2 {
    pub w: usize,
    pub h: usize,
    fw: Arc<dyn Fft<f32>>,
    fh: Arc<dyn Fft<f32>>,
    iw: Arc<dyn Fft<f32>>,
    ih: Arc<dyn Fft<f32>>,
}

impl Fft2 {
    pub fn new(w: usize, h: usize) -> Fft2 {
        let mut p = FftPlanner::<f32>::new();
        Fft2 {
            w,
            h,
            fw: p.plan_fft_forward(w),
            fh: p.plan_fft_forward(h),
            iw: p.plan_fft_inverse(w),
            ih: p.plan_fft_inverse(h),
        }
    }

    /// In-place forward transform (unnormalised).
    pub fn forward(&self, data: &mut [Complex32]) {
        self.run(data, &self.fw, &self.fh);
    }

    /// In-place inverse transform, normalised by 1/(w·h).
    pub fn inverse(&self, data: &mut [Complex32]) {
        self.run(data, &self.iw, &self.ih);
        let s = 1.0 / (self.w * self.h) as f32;
        data.par_iter_mut().for_each(|v| *v *= s);
    }

    fn run(&self, data: &mut [Complex32], fw: &Arc<dyn Fft<f32>>, fh: &Arc<dyn Fft<f32>>) {
        assert_eq!(data.len(), self.w * self.h);
        let (w, h) = (self.w, self.h);
        data.par_chunks_mut(w).for_each(|row| fw.process(row));
        let mut t = transpose(data, w, h);
        t.par_chunks_mut(h).for_each(|col| fh.process(col));
        let back = transpose(&t, h, w);
        data.copy_from_slice(&back);
    }
}

/// Transpose a row-major `w`×`h` matrix into `h`×`w`.
pub fn transpose(src: &[Complex32], w: usize, h: usize) -> Vec<Complex32> {
    let mut dst = vec![Complex32::new(0.0, 0.0); w * h];
    const B: usize = 32;
    dst.par_chunks_mut(h * B).enumerate().for_each(|(bi, chunk)| {
        let x0 = bi * B;
        let xs = chunk.len() / h;
        for y0 in (0..h).step_by(B) {
            for dx in 0..xs {
                let x = x0 + dx;
                for y in y0..(y0 + B).min(h) {
                    chunk[dx * h + y] = src[y * w + x];
                }
            }
        }
    });
    dst
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fast_len() {
        assert_eq!(next_fast_len(1), 1);
        assert_eq!(next_fast_len(7), 8);
        assert_eq!(next_fast_len(97), 100);
        assert_eq!(next_fast_len(513), 540);
    }

    #[test]
    fn roundtrip_and_correlation_peak() {
        let (w, h) = (48usize, 30usize);
        let f = Fft2::new(w, h);
        let mut a: Vec<Complex32> =
            (0..w * h).map(|i| Complex32::new(((i * 7919) % 13) as f32, ((i * 104729) % 5) as f32)).collect();
        let orig = a.clone();
        f.forward(&mut a);
        f.inverse(&mut a);
        for (x, y) in a.iter().zip(orig.iter()) {
            assert!((x - y).norm() < 1e-3);
        }
        // Cross-correlation of a signal with a shifted copy peaks at the shift.
        let (sx, sy) = (5usize, 3usize);
        let mut b = vec![Complex32::new(0.0, 0.0); w * h];
        for y in 0..h {
            for x in 0..w {
                b[((y + sy) % h) * w + (x + sx) % w] = orig[y * w + x];
            }
        }
        let mut fa = orig.clone();
        let mut fb = b.clone();
        f.forward(&mut fa);
        f.forward(&mut fb);
        let mut c: Vec<Complex32> = fb.iter().zip(fa.iter()).map(|(p, q)| p * q.conj()).collect();
        f.inverse(&mut c);
        let (imax, _) = c.iter().enumerate().max_by(|x, y| x.1.re.partial_cmp(&y.1.re).unwrap()).unwrap();
        assert_eq!((imax % w, imax / w), (sx, sy));
    }
}
