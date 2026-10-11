#!/usr/bin/env python3
"""Ground truth for printed-map-photo.jpg (a phone photo of the printed Grootkloof
sheet lying on a wooden table; the paper was folded in four, so it is not flat).

Writes, next to this script:
  truth.json  page (sheet) corners, map frame corners, graticule polylines and
              intersections, and a dense set of control points photo pixel ->
              lat/lon, all in photo pixels (top-left origin, y down).
  ocr.json    fixtures/grootkloof/ocr.json moved into photo pixels (each box as
              a quad and its axis-aligned bbox), to drive the grid path in tests
              without Apple Vision.

Method: SIFT matches photo <-> map.jpg; a local RANSAC homography from the
matches within 260 px of each node of a 40 px grid; nodes inconsistent with a
local affine fit of their neighbours (> 6 map px) are dropped; the field is
interpolated linearly (Delaunay) between nodes. The predicted graticule lies on
the printed one to ~1 px across the creases. The paper corners come from a
paper/wood segmentation (low saturation, bright) with a Huber line fit per side.

Needs numpy, scipy, opencv-python. Run from the repo root:
  python fixtures/grootkloof/photo/make_truth.py
"""
import json
import os

import cv2
import numpy as np
from scipy.interpolate import griddata
from scipy.spatial import cKDTree

HERE = os.path.dirname(os.path.abspath(__file__))
ROOT = os.path.abspath(os.path.join(HERE, "..", "..", ".."))
PHOTO = os.path.join(HERE, "printed-map-photo.jpg")
MAP = os.path.join(ROOT, "fixtures", "grootkloof", "map.jpg")
MAP_TRUTH = os.path.join(ROOT, "fixtures", "grootkloof", "truth.json")
MAP_OCR = os.path.join(ROOT, "fixtures", "grootkloof", "ocr.json")

R = 6378137.0


def merc_to_latlon(x, y):
    return np.degrees(2 * np.arctan(np.exp(y / R)) - np.pi / 2), np.degrees(x / R)


def latlon_to_merc(lat, lon):
    return R * np.radians(lon), R * np.log(np.tan(np.pi / 4 + np.radians(lat) / 2))


def map_px_to_merc(truth):
    """Axis-aligned linear fit of map.jpg pixels to Web Mercator from the graticule."""
    ys = np.array([l["y"] for l in truth["lat_lines"]], float)
    my = np.array([latlon_to_merc(l["lat"], 0)[1] for l in truth["lat_lines"]])
    xs = np.array([l["x"] for l in truth["lon_lines"]], float)
    mx = np.array([latlon_to_merc(0, l["lon"])[0] for l in truth["lon_lines"]])
    ay = np.polyfit(ys, my, 1)
    ax = np.polyfit(xs, mx, 1)
    return lambda x, y: (np.polyval(ax, x), np.polyval(ay, y))


def field_nodes(photo, mp):
    sift = cv2.SIFT_create(nfeatures=80000)
    k = 0.5
    mps = cv2.resize(mp, None, fx=k, fy=k, interpolation=cv2.INTER_AREA)
    k1, d1 = sift.detectAndCompute(cv2.cvtColor(photo, cv2.COLOR_BGR2GRAY), None)
    k2, d2 = sift.detectAndCompute(cv2.cvtColor(mps, cv2.COLOR_BGR2GRAY), None)
    m = cv2.BFMatcher().knnMatch(d1, d2, k=2)
    good = [a for a, b in m if a.distance < 0.8 * b.distance]
    p1 = np.float32([k1[a.queryIdx].pt for a in good])
    p2 = np.float32([k2[a.trainIdx].pt for a in good]) / k
    h, w = photo.shape[:2]
    cv2.setRNGSeed(7)
    nodes = []
    for cy in range(150, h - 100, 40):
        for cx in range(80, w - 80, 40):
            d = np.hypot(p1[:, 0] - cx, p1[:, 1] - cy)
            s = d < 260
            if s.sum() < 25:
                continue
            hl, inl = cv2.findHomography(p1[s], p2[s], cv2.RANSAC, 5.0, maxIters=3000)
            if hl is None or inl.sum() < 15:
                continue
            ii = np.where(s)[0][inl.ravel().astype(bool)]
            if (d[ii] < 150).sum() < 6:
                continue
            q = cv2.perspectiveTransform(np.float32([[[cx, cy]]]), hl)[0, 0]
            nodes.append((cx, cy, q[0], q[1]))
    nodes = np.array(nodes, float)
    cx, cy, mx, my = nodes.T
    tree = cKDTree(np.c_[cx, cy])
    keep = np.ones(len(nodes), bool)
    for i in range(len(nodes)):
        nb = [j for j in tree.query_ball_point([cx[i], cy[i]], 90) if j != i]
        if len(nb) < 4:
            keep[i] = False
            continue
        X = np.c_[cx[nb] - cx[i], cy[nb] - cy[i], np.ones(len(nb))]
        ax = np.linalg.lstsq(X, mx[nb], rcond=None)[0]
        ay = np.linalg.lstsq(X, my[nb], rcond=None)[0]
        keep[i] = np.hypot(ax[2] - mx[i], ay[2] - my[i]) < 6
    return nodes[keep]


def page_quad(photo):
    hsv = cv2.cvtColor(photo, cv2.COLOR_BGR2HSV)
    paper = ((hsv[..., 1] < 60) & (hsv[..., 2] > 150)).astype(np.uint8) * 255
    paper = cv2.morphologyEx(paper, cv2.MORPH_CLOSE, np.ones((25, 25), np.uint8))
    n, lab, st, _ = cv2.connectedComponentsWithStats(paper)
    i = 1 + np.argmax(st[1:, cv2.CC_STAT_AREA])
    m = cv2.morphologyEx((lab == i).astype(np.uint8) * 255, cv2.MORPH_OPEN, np.ones((9, 9), np.uint8))
    cnts, _ = cv2.findContours(m, cv2.RETR_EXTERNAL, cv2.CHAIN_APPROX_NONE)
    c = max(cnts, key=cv2.contourArea)
    hull = cv2.convexHull(c)
    peri = cv2.arcLength(hull, True)
    for eps in [0.01, 0.02, 0.03, 0.04, 0.05]:
        ap = cv2.approxPolyDP(hull, eps * peri, True)
        if len(ap) == 4:
            break
    q = ap.reshape(4, 2).astype(float)
    s, d = q.sum(1), q[:, 0] - q[:, 1]
    o = np.array([q[np.argmin(s)], q[np.argmax(d)], q[np.argmax(s)], q[np.argmin(d)]])
    pts = c.reshape(-1, 2).astype(float)
    lines = []
    for k in range(4):
        a, b = o[k], o[(k + 1) % 4]
        ab = b - a
        L = np.linalg.norm(ab)
        u = ab / L
        nrm = np.array([-u[1], u[0]])
        rel = pts - a
        t, dist = rel @ u / L, rel @ nrm
        sel = (t > 0.1) & (t < 0.9) & (np.abs(dist) < 25)
        vx, vy, x0, y0 = cv2.fitLine(pts[sel].astype(np.float32), cv2.DIST_HUBER, 0, 0.01, 0.01).ravel()
        lines.append((np.array([x0, y0]), np.array([vx, vy])))

    def inter(l1, l2):
        p, r = l1
        q2, s2 = l2
        t = np.linalg.solve(np.array([r, -s2]).T, q2 - p)
        return p + t[0] * r

    return np.array([inter(lines[3], lines[0]), inter(lines[0], lines[1]),
                     inter(lines[1], lines[2]), inter(lines[2], lines[3])])


def main():
    photo = cv2.imread(PHOTO)
    mp = cv2.imread(MAP)
    truth = json.load(open(MAP_TRUTH))
    to_merc = map_px_to_merc(truth)
    nodes = field_nodes(photo, mp)
    cx, cy, mx, my = nodes.T

    src_tree, dst_tree = cKDTree(np.c_[mx, my]), cKDTree(np.c_[cx, cy])

    def interp(pts, src, dst, tree):
        """Linear interpolation inside the nodes' hull; outside it (a few map
        corners), the homography of the 40 nearest nodes."""
        pts = np.asarray(pts, float)
        out = np.c_[griddata(src, dst[:, 0], pts, method="linear"),
                    griddata(src, dst[:, 1], pts, method="linear")]
        for i in np.where(np.isnan(out).any(1))[0]:
            _, nb = tree.query(pts[i], 40)
            hl, _ = cv2.findHomography(src[nb], dst[nb], 0)
            out[i] = cv2.perspectiveTransform(pts[i][None, None], hl)[0, 0]
        return out

    def m2p(pts):
        return interp(pts, np.c_[mx, my], np.c_[cx, cy], src_tree)

    def p2m(pts):
        return interp(pts, np.c_[cx, cy], np.c_[mx, my], dst_tree)

    def latlon_of_map(p):
        x, y = to_merc(p[:, 0], p[:, 1])
        return merc_to_latlon(x, y)

    h, w = photo.shape[:2]
    fx0, fy0, fx1, fy1 = truth["map_frame_px"]
    frame = m2p([[fx0, fy0], [fx1, fy0], [fx1, fy1], [fx0, fy1]])
    page = page_quad(photo)

    lat_lines, lon_lines, inter = [], [], []
    for L in truth["lat_lines"]:
        xs = np.linspace(fx0, fx1, 61)
        p = m2p(np.c_[xs, np.full_like(xs, L["y"])])
        p = p[~np.isnan(p).any(1)]
        lat_lines.append({"lat": L["lat"], "points": np.round(p, 2).tolist()})
    for L in truth["lon_lines"]:
        ys = np.linspace(fy0, fy1, 81)
        p = m2p(np.c_[np.full_like(ys, L["x"]), ys])
        p = p[~np.isnan(p).any(1)]
        lon_lines.append({"lon": L["lon"], "points": np.round(p, 2).tolist()})
    for la in truth["lat_lines"]:
        for lo in truth["lon_lines"]:
            q = m2p([[lo["x"], la["y"]]])[0]
            inter.append({"lat": la["lat"], "lon": lo["lon"], "x": round(q[0], 2), "y": round(q[1], 2)})

    # Control points on a 64 px grid of photo pixels inside the map frame.
    gx, gy = np.meshgrid(np.arange(0, w, 64.0), np.arange(0, h, 64.0))
    g = np.c_[gx.ravel(), gy.ravel()]
    inside = cv2.pointPolygonTest
    fr = frame.astype(np.float32).reshape(-1, 1, 2)
    g = g[[inside(fr, (float(x), float(y)), True) > 8 for x, y in g]]
    mpts = np.c_[griddata(np.c_[cx, cy], mx, g, method="linear"),
                 griddata(np.c_[cx, cy], my, g, method="linear")]
    ok = ~np.isnan(mpts).any(1)
    g, mpts = g[ok], mpts[ok]
    lat, lon = latlon_of_map(mpts)
    cps = [{"x": float(a), "y": float(b), "lat": round(float(c), 7), "lon": round(float(d), 7),
            "map_x": round(float(e), 2), "map_y": round(float(f), 2)}
           for a, b, c, d, e, f in zip(g[:, 0], g[:, 1], lat, lon, mpts[:, 0], mpts[:, 1])]

    # How far one homography (or affine) over the frame is from the truth.
    P = g.astype(np.float32)
    mx_, my_ = to_merc(mpts[:, 0], mpts[:, 1])
    M = np.c_[mx_, my_]
    cosl = np.cos(np.radians(np.mean(lat)))
    Hm, _ = cv2.findHomography(P.astype(np.float64), M, 0)
    e = np.linalg.norm(cv2.perspectiveTransform(P[None].astype(np.float64), Hm)[0] - M, axis=1) * cosl

    out = {
        "description": (
            "Ground truth for printed-map-photo.jpg: a phone photo (1932x2576) of the printed "
            "Grootkloof sheet on a wooden table, folded in four (creases near x=938 and y=1278), "
            "so no single homography fits it. Photo pixels, top-left origin. control_points map "
            "photo pixels to lat/lon (and map.jpg pixels) through a dense SIFT field; see "
            "make_truth.py. homography_floor_m is the error of the best single homography over "
            "the control points, the floor for any flat model."),
        "image": "printed-map-photo.jpg",
        "image_width": int(w),
        "image_height": int(h),
        "page_quad": np.round(page, 1).tolist(),
        "frame_quad": np.round(frame, 1).tolist(),
        "creases": {"x": [938], "y": [1278]},
        "graticule": {"lat_lines": lat_lines, "lon_lines": lon_lines, "intersections": inter},
        "homography_floor_m": {"rms": round(float(np.sqrt((e ** 2).mean())), 1),
                               "median": round(float(np.median(e)), 1),
                               "p90": round(float(np.percentile(e, 90)), 1),
                               "max": round(float(e.max()), 1)},
        "control_points": cps,
    }
    with open(os.path.join(HERE, "truth.json"), "w") as f:
        json.dump(out, f, indent=1)

    ocr = json.load(open(MAP_OCR))
    obs = []
    for o in ocr["observations"]:
        x, y, bw, bh = o["bbox"]
        q = m2p([[x, y], [x + bw, y], [x + bw, y + bh], [x, y + bh]])
        x0, y0 = q.min(0)
        x1, y1 = q.max(0)
        obs.append({"text": o["text"], "quad": np.round(q, 1).tolist(),
                    "bbox": [round(float(x0), 1), round(float(y0), 1),
                             round(float(x1 - x0), 1), round(float(y1 - y0), 1)],
                    "confidence": o["confidence"]})
    with open(os.path.join(HERE, "ocr.json"), "w") as f:
        json.dump({"description": "fixtures/grootkloof/ocr.json moved into printed-map-photo.jpg "
                   "pixels through the truth field (quad = the box's corners TL, TR, BR, BL; "
                   "bbox = their axis-aligned [x, y, w, h]). Made by make_truth.py.",
                   "observations": obs}, f, indent=1)
    print("nodes", len(nodes), "control points", len(cps), "ocr", len(obs),
          "floor", out["homography_floor_m"], "page", out["page_quad"], "frame", out["frame_quad"])


if __name__ == "__main__":
    main()
