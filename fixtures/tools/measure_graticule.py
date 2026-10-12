"""Measure the graticule lines of a scanned/rendered map sheet (truth.json line positions).

usage: python measure_graticule.py map.jpg            (pip install numpy opencv-python)

1. Candidates: rows (columns) where a thin dark line runs across most of the sheet: the
   pixel is darker by > 10 than the mean of the pixels 3 px above and below (left and right)
   for at least 25 % of the row, keeping the strongest within 15 px.
2. Position: for every 300 px segment along a candidate, the darkness-weighted centroid
   across an 11 px window, using only positions where the paper on both sides is blank
   (> 200), so contours, text and hatching crossing the line do not pull it; the line's
   position is the median over the segments (pixel-index coordinates, as in
   fixtures/grootkloof/truth.json).

It prints every candidate with its coverage, its position and the spread (10th-90th
percentile) of the segment positions; straight lines across the sheet (coverage >= 0.5,
spread < 0.25 px) are marked "line", hatching and other clutter "other". The frame lines
(x/y 106 and 118, x 3388/3400, y 4841/4853 on the MCSA Magaliesberg sheets) are printed
too, except a line within 15 px of a stronger one (the inner frame line at 118). Which
line is which minute comes from the printed labels and the spacing, read by eye.
"""
import sys
import numpy as np
import cv2


def candidates(g, axis):
    a, b = (np.roll(g, 3, axis), np.roll(g, -3, axis))
    r = 0.5 * (a + b) - g
    frac = (r > 10).mean(axis=1 - axis)
    picked = []
    for i in np.argsort(-frac):
        if frac[i] < 0.25:
            break
        if all(abs(i - p) > 15 for p in picked):
            picked.append(int(i))
    return sorted(picked), frac


def segment_positions(g, axis, pos, seg=300, margin=120):
    length = g.shape[0] if axis == 1 else g.shape[1]
    offs = np.arange(-5, 6)[:, None]
    out = []
    for s in range(margin, length - margin, seg):
        p = g[pos - 5:pos + 6, s:s + seg] if axis == 0 else g[s:s + seg, pos - 5:pos + 6].T
        base = np.median(np.concatenate([p[:3], p[-3:]]), axis=0)
        dark = np.clip(base[None] - p, 0, None)
        tot = dark.sum(0)
        ok = (tot > 60) & (base > 200) & (p[[0, 1, 2, -3, -2, -1]].min(0) > 200)
        if ok.sum() >= 20:
            out.append(pos + float(np.median((dark * offs).sum(0)[ok] / tot[ok])))
    return out


def main(path):
    g = cv2.cvtColor(cv2.imread(path), cv2.COLOR_BGR2GRAY).astype(np.float32)
    print(f"{path}: {g.shape[1]}x{g.shape[0]}")
    for axis, name in ((0, "horizontal (y)"), (1, "vertical (x)")):
        picked, frac = candidates(g, axis)
        print(name)
        for p in picked:
            segs = segment_positions(g, axis, p)
            if not segs:
                print(f"  {p:5d}  coverage {frac[p]:.2f}  no clean segment")
                continue
            spread = np.percentile(segs, 90) - np.percentile(segs, 10)
            kind = "line" if frac[p] >= 0.5 and spread < 0.25 else "other (hatching, contours, text)"
            print(f"  {p:5d}  coverage {frac[p]:.2f}  position {np.median(segs):8.2f}  "
                  f"segments {len(segs):2d}  spread {spread:.2f} px  {kind}")


if __name__ == "__main__":
    main(sys.argv[1])
