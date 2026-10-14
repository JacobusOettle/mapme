"""The page of a Gaia GPS PDF export as the app renders it (MapImporter.renderPDF), for the fixtures.

usage: python render_page.py export.pdf native.png map.jpg map_nearest.jpg
       (pip install numpy pillow pikepdf pymupdf)

The page is rendered like MapImporter.renderPDF: 300/72 px per pt (3508 x 2480 for A4
landscape), top page edge pinned to the first row, white background. Vector content and
the logo are rendered by MuPDF (with alpha) in two layers, the map raster (image XObject
/I0, /Interpolate false) is resampled by PIL into its exact placement from the content
stream's cm operator, and the layers are composited in content-stream order:
white, frame stroke, raster, everything after the raster.

map.jpg upsamples the raster with a smooth (bicubic) kernel, which is what CoreGraphics does with
the context's interpolationQuality .high (see README.md); map_nearest.jpg repeats its pixels
(nearest neighbour), what a renderer that honours /Interpolate false would draw. native.png is
the embedded raster itself, pixel for pixel (as pdfimages extracts it). JPEG quality 92, 4:4:4.
"""
import sys, re, io
import numpy as np, pikepdf, pymupdf
from PIL import Image

pdf_path, native_out, map_out, nearest_out = sys.argv[1:5]
SCALE = 300 / 72


def render_layer(content: bytes):
    pdf = pikepdf.open(pdf_path)
    page = pdf.pages[0]
    page.Contents = pdf.make_stream(content)
    buf = io.BytesIO(); pdf.save(buf)
    doc = pymupdf.open(stream=buf.getvalue(), filetype='pdf')
    pix = doc[0].get_pixmap(matrix=pymupdf.Matrix(SCALE, SCALE), alpha=True)
    a = np.frombuffer(pix.samples, np.uint8).reshape(pix.height, pix.width, 4).astype(np.float64) / 255
    return a


pdf = pikepdf.open(pdf_path)
page = pdf.pages[0]
content = page.Contents.read_bytes()
mb = [float(v) for v in page.MediaBox]
pw, ph = mb[2] - mb[0], mb[3] - mb[1]
W, H = int(round(pw * SCALE)), int(round(ph * SCALE))
m = re.search(rb'q\s+([-\d.]+) ([-\d.]+) ([-\d.]+) ([-\d.]+) ([-\d.]+) ([-\d.]+) cm\s+/I0 Do\s+Q', content)
a, b, c, d, e, f = map(float, m.groups())
assert b == 0 and c == 0 and a > 0 and d > 0
img = page.Resources.XObject['/I0']
assert '/Interpolate' not in img or not bool(img.Interpolate)
native = pikepdf.PdfImage(img).as_pil_image().convert('RGB')
native.save(native_out)
# placement in render pixels (edge coordinates, y down from the top of the page)
x0, x1 = e * SCALE, (e + a) * SCALE
y0, y1 = (ph - (f + d)) * SCALE, (ph - f) * SCALE
print(f'page {pw}x{ph} pt -> {W}x{H} px; raster {native.size} placed at pt x {e}..{e + a}, '
      f'y(top-down) {ph - f - d:.4f}..{ph - f:.4f}; render px x {x0:.4f}..{x1:.4f}, y {y0:.4f}..{y1:.4f}')
assert all(abs(v - round(v)) < 1e-3 for v in (x0, x1, y0, y1)), 'placement is not on whole pixels'
x0, x1, y0, y1 = (int(round(v)) for v in (x0, x1, y0, y1))

pre = content[:m.start()]
post = content[m.end():]
layer1 = render_layer(pre)
layer2 = render_layer(re.sub(rb're\s+S', b're\nn', pre) + b'\n' + post)
assert layer1.shape[0] >= H and layer1.shape[1] >= W, layer1.shape


def over(base, layer):
    al = layer[:H, :W, 3:4]
    return base * (1 - al) + layer[:H, :W, :3] * al


for resample, out in ((Image.BICUBIC, map_out), (Image.NEAREST, nearest_out)):
    base = np.ones((H, W, 3))
    base = over(base, layer1)
    up = np.asarray(native.resize((x1 - x0, y1 - y0), resample), np.float64) / 255
    base[y0:y1, x0:x1] = up
    base = over(base, layer2)
    Image.fromarray(np.clip(np.round(base * 255), 0, 255).astype(np.uint8)).save(out, quality=92, subsampling=0)
    print(out, W, H)
