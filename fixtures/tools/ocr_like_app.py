"""Emulate ios OCRService.plan with RapidOCR: upright 1600px tiles (15% overlap) + cw/ccw passes
over the left/right 18% strips. Output {"observations":[{text,bbox:[x,y,w,h],confidence}]} in base px.

RapidOCR stands in for Apple Vision (which only runs on Apple platforms): it reads the same
image regions the app's OCR plan reads, but its errors differ from Vision's (different
garbling, splitting and merging of labels), so the output is a stand-in, not a recording.

usage: python ocr_like_app.py map.jpg ocr.json   (pip install rapidocr_onnxruntime opencv-python numpy)
Used for fixtures/castle-gorge/ocr.json and fixtures/mhlabatini/ocr.json.
"""
import sys, json, math, numpy as np, cv2
from rapidocr_onnxruntime import RapidOCR
ocr = RapidOCR()
def spans(length, tile=1600, overlap=0.15):
    if length <= tile: return [(0, length)]
    n = math.ceil((length - tile*overlap) / (tile*(1-overlap)))
    n = max(n, 2)
    step = (length - tile) / (n - 1)
    return [(int(round(i*step)), tile) for i in range(n)]
def run(img):
    res, _ = ocr(img)
    return res or []
def main(path, out):
    im = cv2.imread(path); H, W = im.shape[:2]
    obs = []
    def add(box_pts, text, conf):
        xs = [p[0] for p in box_pts]; ys = [p[1] for p in box_pts]
        obs.append({"text": text, "bbox": [min(xs), min(ys), max(xs)-min(xs), max(ys)-min(ys)], "confidence": float(conf)})
    for (y0, th) in spans(H):
        for (x0, tw) in spans(W):
            crop = im[y0:y0+th, x0:x0+tw]
            for box, text, conf in run(crop):
                add([(p[0]+x0, p[1]+y0) for p in box], text, conf)
    mx = int(round(W*0.18))
    for sx in (0, W-mx):
        for (y0, th) in spans(H):
            crop = im[y0:y0+th, sx:sx+mx]
            h, w = crop.shape[:2]
            # clockwise rotation: rotated (u,v) -> original (x = v, y = h-1-u)
            cw = cv2.rotate(crop, cv2.ROTATE_90_CLOCKWISE)
            for box, text, conf in run(cw):
                add([(sx + p[1], y0 + (h - 1 - p[0])) for p in box], text, conf)
            ccw = cv2.rotate(crop, cv2.ROTATE_90_COUNTERCLOCKWISE)
            for box, text, conf in run(ccw):
                add([(sx + (w - 1 - p[1]), y0 + p[0]) for p in box], text, conf)
    # light dedupe: same text, centres within 20 px
    out_obs = []
    for o in sorted(obs, key=lambda o: -o["confidence"]):
        cx, cy = o["bbox"][0]+o["bbox"][2]/2, o["bbox"][1]+o["bbox"][3]/2
        if any(q["text"] == o["text"] and abs(q["bbox"][0]+q["bbox"][2]/2-cx) < 20 and abs(q["bbox"][1]+q["bbox"][3]/2-cy) < 20 for q in out_obs):
            continue
        out_obs.append(o)
    desc = ("Stand-in for Apple Vision: RapidOCR (rapidocr_onnxruntime) run by fixtures/tools/ocr_like_app.py "
            "on map.jpg with the app's OCR plan (ios OCRService: upright 1600 px tiles overlapping 15 %, plus "
            "clockwise and counter-clockwise passes over the left and right 18 % margin strips; boxes mapped "
            "back to map.jpg pixels, bbox = [x, y, w, h], top-left origin; same text within 20 px deduplicated). "
            "Unedited OCR output: labels can be garbled, split or merged, as Vision's can, but not identically.")
    json.dump({"description": desc, "observations": out_obs}, open(out, "w"), indent=1)
    print(path, len(out_obs), "observations")
if __name__ == "__main__":
    main(sys.argv[1], sys.argv[2])
