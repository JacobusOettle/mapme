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

    /// Forward transform of a row-major `w`×`h` array into a column-major
    /// spectrum (`x · h + y`): one transpose instead of two.
    pub fn forward_to_cols(&self, data: &[Complex32]) -> Vec<Complex32> {
        assert_eq!(data.len(), self.w * self.h);
        let (w, h) = (self.w, self.h);
        let mut rows = data.to_vec();
        rows.par_chunks_mut(w * batch(w)).for_each(|c| self.fw.process(c));
        let mut cols = transpose(&rows, w, h);
        cols.par_chunks_mut(h * batch(h)).for_each(|c| self.fh.process(c));
        cols
    }

    /// Column-major spectrum (`x · h + y`) of an `sw`×`sh` row-major block
    /// zero-padded to `w`×`h` (the block at the origin), written to `out`:
    /// only the block's rows are transformed along x, and no transpose of
    /// the padded array is made.
    pub fn forward_padded_to_cols(&self, src: &[Complex32], sw: usize, sh: usize, out: &mut Vec<Complex32>) {
        let (w, h) = (self.w, self.h);
        assert!(sw <= w && sh <= h && src.len() == sw * sh);
        let mut rows = vec![Complex32::new(0.0, 0.0); w * sh];
        rows.par_chunks_mut(w).zip(src.par_chunks(sw.max(1))).for_each(|(r, s)| r[..sw].copy_from_slice(s));
        rows.par_chunks_mut(w * batch(w)).for_each(|c| self.fw.process(c));
        out.clear();
        out.resize(w * h, Complex32::new(0.0, 0.0));
        let nb = batch(h);
        out.par_chunks_mut(h * nb).enumerate().for_each(|(bi, chunk)| {
            for (j, col) in chunk.chunks_mut(h).enumerate() {
                let x = bi * nb + j;
                for (y, v) in col[..sh].iter_mut().enumerate() {
                    *v = rows[y * w + x];
                }
            }
            self.fh.process(chunk);
        });
    }

    /// Inverse transform of a column-major spectrum (`x · h + y`, transformed
    /// in place along y), returning rows `0..rows` of the result row-major
    /// (`rows` × `w`), unnormalised (times w·h).
    pub fn inverse_cols_to_rows(&self, spec: &mut [Complex32], rows: usize, out: &mut Vec<Complex32>) {
        assert_eq!(spec.len(), self.w * self.h);
        let (w, h) = (self.w, self.h);
        let rows = rows.min(h);
        spec.par_chunks_mut(h * batch(h)).for_each(|c| self.ih.process(c));
        out.clear();
        out.resize(rows * w, Complex32::new(0.0, 0.0));
        const B: usize = 32;
        let spec = &*spec;
        out.par_chunks_mut(w * B).enumerate().for_each(|(bi, chunk)| {
            let y0 = bi * B;
            let ny = chunk.len() / w;
            for x0 in (0..w).step_by(B) {
                for x in x0..(x0 + B).min(w) {
                    let col = &spec[x * h + y0..x * h + y0 + ny];
                    for (dy, &v) in col.iter().enumerate() {
                        chunk[dy * w + x] = v;
                    }
                }
            }
            self.iw.process(chunk);
        });
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

/// Transforms of length `n` per `process` call in the parallel passes (one
/// scratch buffer per call).
fn batch(n: usize) -> usize {
    (16_384 / n.max(1)).clamp(1, 64)
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
