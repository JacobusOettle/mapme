"""Perturbed copies of the real fixtures, for calibrating the reported accuracy (the app's ±).

usage: python perturb_real.py OUT_DIR DEM_DIR [--only NAME,...]

Sources: the two Gaia exports (native raster and the page as the app renders it) and the three
MCSA sheets. Perturbations: JPEG quality 25 and 15, crops to 30-60 % of each side (centre, a
corner, a side), contours hidden on 40-70 % of the map (the region median-filtered: thin lines
vanish, tints and hillshade stay), contrast 0.5, blur, and combinations. 119 cases.

Each case is written in the common case format (fixtures/tools/bench_contours.py) with the truth
on a 7 x 7 grid over the map content left in the new image (the source's map frame clipped to
it, 2-98 %), from the source's pixel -> Web Mercator affine (least squares on its truth points:
all the real fixtures are north-up Web Mercator, residuals < 1 m). DEM_DIR is where the cases
look for (and bench_contours.py --fetch puts) Terrarium tiles: seed it with the fixtures' dem/
tiles. The GPS of a case is its map centre.

    python perturb_real.py /tmp/pert /tmp/dem && cp -n ../*/dem/*.png /tmp/dem/
    python bench_contours.py /tmp/pert --variants centre --fetch --no-dump --out /tmp/pert-out

bench_contours.py's summary then reports the reported ± against the true max error.
"""
import argparse
import io
import json
import math
import os

import numpy as np
from PIL import Image, ImageEnhance, ImageFilter

Image.MAX_IMAGE_PIXELS = None
R = 6378137.0
FIX = os.path.normpath(os.path.join(os.path.dirname(os.path.abspath(__file__)), ".."))
SOURCES = [
    ("gaiaA-native", "gaia-langalibalele", "case_native.json"),
    ("gaiaA-page", "gaia-langalibalele", "case.json"),
    ("gaiaB-native", "gaia-giants-castle", "case_native.json"),
    ("gaiaB-page", "gaia-giants-castle", "case.json"),
    ("grootkloof", "grootkloof", "case.json"),
    ("castle-gorge", "castle-gorge", "case.json"),
    ("mhlabatini", "mhlabatini", "case.json"),
]


def merc(lat, lon):
    return R * math.radians(lon), R * math.log(math.tan(math.pi / 4 + math.radians(lat) / 2))


def unmerc(x, y):
    return math.degrees(2 * math.atan(math.exp(y / R)) - math.pi / 2), math.degrees(x / R)


def fit_affine(tp):
    A = np.array([[p["x"], p["y"], 1.0] for p in tp])
    M = np.array([merc(p["lat"], p["lon"]) for p in tp])
    cx = np.linalg.lstsq(A, M[:, 0], rcond=None)[0]
    cy = np.linalg.lstsq(A, M[:, 1], rcond=None)[0]
    res = A @ np.stack([cx, cy], 1) - M
    return cx, cy, float(np.abs(res).max())


def jpeg(img, q):
    b = io.BytesIO()
    img.save(b, "JPEG", quality=q)
    b.seek(0)
    return Image.open(b).convert("RGB")


def perturb(tag, src, cf, out, dem, only):
    case = json.load(open(os.path.join(src, cf)))
    cx, cy, res = fit_affine(case["truth_points"])
    im = Image.open(os.path.join(src, case["image"])).convert("RGB")
    W, H = im.size
    frame = case.get("map_frame_px") or [0, 0, W, H]
    level = 3 if any(t.startswith("upsample-x3") for t in case.get("tags", [])) else 1
    wipe = 5 if level == 1 else 11  # median size that wipes a contour line
    n = 0

    def save(name, img, off=(0.0, 0.0), keep_ocr=False):
        nonlocal n
        if only and name not in only:
            return
        d = os.path.join(out, tag + "-" + name)
        os.makedirs(d, exist_ok=True)
        img.save(os.path.join(d, "map.jpg"), quality=95)
        w, h = img.size
        fx0, fy0 = max(0.0, frame[0] - off[0]), max(0.0, frame[1] - off[1])
        fx1, fy1 = min(float(w), frame[2] - off[0]), min(float(h), frame[3] - off[1])
        tps = []
        for j in range(7):
            for i in range(7):
                x = fx0 + (fx1 - fx0) * (0.02 + 0.96 * i / 6)
                y = fy0 + (fy1 - fy0) * (0.02 + 0.96 * j / 6)
                sx, sy = x + off[0], y + off[1]
                lat, lon = unmerc(cx[0] * sx + cx[1] * sy + cx[2], cy[0] * sx + cy[1] * sy + cy[2])
                tps.append(dict(x=round(x, 2), y=round(y, 2), lat=round(lat, 8), lon=round(lon, 8)))
        c = dict(name=tag + "-" + name, image="map.jpg", gps=[tps[24]["lat"], tps[24]["lon"]],
                 dem_dir=os.path.relpath(dem, d), dpi=case.get("dpi"), m_per_px=case["m_per_px"],
                 contour_interval_m=case.get("contour_interval_m"), style=case.get("style"),
                 tags=list(case.get("tags", [])) + ["pert", "pert-" + name.rstrip("0123456789").split("-")[0]],
                 region=case.get("region"), truth_points=tps, map_frame_px=[fx0, fy0, fx1, fy1], image_size=[w, h],
                 source=tag, ocr=(os.path.relpath(os.path.join(src, case["ocr"]), d)
                                  if keep_ocr and case.get("ocr") else None))
        json.dump(c, open(os.path.join(d, "case.json"), "w"), indent=1)
        n += 1

    def masked(img, box):
        fx0, fy0, fx1, fy1 = frame
        bx = (int(fx0 + box[0] * (fx1 - fx0)), int(fy0 + box[1] * (fy1 - fy0)),
              int(fx0 + box[2] * (fx1 - fx0)), int(fy0 + box[3] * (fy1 - fy0)))
        o = img.copy()
        o.paste(img.crop(bx).filter(ImageFilter.MedianFilter(wipe)), bx[:2])
        return o

    def crop(fw, fh, ax, ay, name):
        fx0, fy0, fx1, fy1 = frame
        cw, ch = (fx1 - fx0) * fw, (fy1 - fy0) * fh
        x0, y0 = int(fx0 + ax * (fx1 - fx0 - cw)), int(fy0 + ay * (fy1 - fy0 - ch))
        save(name, im.crop((x0, y0, int(x0 + cw), int(y0 + ch))), off=(x0, y0))

    save("q25", jpeg(im, 25), keep_ocr=True)
    save("q15", jpeg(im, 15), keep_ocr=True)
    save("contrast", ImageEnhance.Contrast(im).enhance(0.5), keep_ocr=True)
    save("blur", im.filter(ImageFilter.GaussianBlur(1.2 * level)), keep_ocr=True)
    save("q25contrast", jpeg(ImageEnhance.Contrast(im).enhance(0.6), 25), keep_ocr=True)
    crop(0.5, 0.5, 0.5, 0.5, "crop50c")
    crop(0.6, 0.6, 0.0, 0.0, "crop60nw")
    crop(0.45, 0.45, 1.0, 1.0, "crop45se")
    crop(0.4, 1.0, 0.0, 0.5, "crop40w")
    crop(1.0, 0.35, 0.5, 1.0, "crop35s")
    crop(0.3, 0.3, 0.3, 0.6, "crop30")
    save("mask60e", masked(im, (0.4, 0.0, 1.0, 1.0)), keep_ocr=True)
    save("mask50n", masked(im, (0.0, 0.0, 1.0, 0.5)), keep_ocr=True)
    save("mask70se", masked(im, (0.0, 0.3, 1.0, 1.0)), keep_ocr=True)
    save("mask45sw", masked(im, (0.0, 0.33, 0.67, 1.0)), keep_ocr=True)
    save("q25mask60e", jpeg(masked(im, (0.4, 0.0, 1.0, 1.0)), 25), keep_ocr=True)
    save("blurmask50w", masked(im.filter(ImageFilter.GaussianBlur(1.0 * level)), (0.0, 0.0, 0.5, 1.0)),
         keep_ocr=True)
    print("%s: %d cases (truth affine residual %.2f m)" % (tag, n, res))
    return n


def main():
    ap = argparse.ArgumentParser(description=__doc__.split("\n\n")[0])
    ap.add_argument("out")
    ap.add_argument("dem")
    ap.add_argument("--only", default="", help="perturbation names, comma-separated (default: all)")
    a = ap.parse_args()
    only = set(a.only.split(",")) - {""}
    out, dem = os.path.abspath(a.out), os.path.abspath(a.dem)
    os.makedirs(out, exist_ok=True)
    os.makedirs(dem, exist_ok=True)
    total = sum(perturb(tag, os.path.join(FIX, d), cf, out, dem, only) for tag, d, cf in SOURCES)
    print("%d cases in %s" % (total, out))


if __name__ == "__main__":
    main()
