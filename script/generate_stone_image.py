# SPDX-License-Identifier: AGPL-3.0-only
"""Generates app/assets/bundled/jpg/themes/stone.jpg, the background of the Stone theme.

Mnemos IDE: the image is TwinCurrent ApS's own work, under the same license as the rest of
Mnemos, and this script is its source. It draws a pale sandstone texture (layered fractal noise,
pale veins, fine grain and sparse light and dark specks) and three hazy mountain ridges in the
lower right, then fades the ridges out toward the left so the text area stays calm.

Usage (needs Pillow, `pip install pillow`, and ImageMagick for the JPEG step):
    python3 script/generate_stone_image.py stone.png
    magick stone.png -strip -quality 86 app/assets/bundled/jpg/themes/stone.jpg

The defaults are the values the shipped image was made with. Optional positional arguments
after the output path override them, in this order:
    seed (7)    seed of the ridge outlines
    texture (0.63)  weight of the large-scale mottling and veins; lower is calmer
    grain (0.10)    weight of the per-pixel grain
    grain2 (0.06)   weight of the slightly coarser grain
    near (0.97)     brightness of the nearest ridge; lower is darker

Pillow's noise comes from the C library's rand(), which this script does not seed, so the
texture is the same on every run on one platform but may differ between platforms. With Pillow
12.3 and ImageMagick 7.1.1 on macOS (arm64) the commands above reproduce the shipped stone.jpg
byte for byte. Image size 2560x1600; keep the result under 700 KB.
"""

import random
import sys

from PIL import Image, ImageChops, ImageDraw, ImageFilter, ImageMath

W, H = 2560, 1600

out = sys.argv[1]
params = [float(v) for v in sys.argv[2:]]
SEED, TEXW, GRAIN, GRAIN2, NEAR = params + [7, 0.63, 0.10, 0.06, 0.97][len(params) :]
random.seed(int(SEED))


def noise(sx, sy, sigma=64):
    return (
        Image.effect_noise((max(2, sx), max(2, sy)), sigma)
        .resize((W, H), Image.BICUBIC)
        .convert("F")
    )


def fbm(bx, by, octs, gain=0.5):
    """Fractal noise: `octs` octaves from `bx` x `by` cells up, centred on 0."""
    res = None
    amp = 1
    tot = 0
    for i in range(octs):
        f = noise(bx * 2**i, by * 2**i).point(lambda v, a=amp: (v - 128) * a)
        res = f if res is None else ImageMath.lambda_eval(lambda d: d["x"] + d["y"], x=res, y=f)
        tot += amp
        amp *= gain
    return res.point(lambda v: v / tot)


def add(*imgs_w):
    res = None
    for img, w in imgs_w:
        f = img.point(lambda v, w=w: v * w)
        res = f if res is None else ImageMath.lambda_eval(lambda d: d["x"] + d["y"], x=res, y=f)
    return res


def colourise(F, lo, hi, mid=150, k=1.0):
    L = F.point(lambda v: mid + v * k).convert("L")
    return (
        Image.merge(
            "RGB", [L.point(lambda v, i=i: int(lo[i] + (hi[i] - lo[i]) * v / 255)) for i in range(3)]
        ),
        L,
    )


def ridge(n, base, amp, rough, peaks, tilt):
    """A ridge outline by midpoint displacement, with smooth peaks added at `peaks`."""
    pts = [0.0] * n
    step = n - 1
    scale = 1.0
    while step > 1:
        h = step // 2
        for i in range(h, n, step):
            pts[i] = (pts[i - h] + pts[i + h]) / 2 + random.uniform(-1, 1) * scale
        step = h
        scale *= rough
    res = []
    for i in range(n):
        x = i / (n - 1)
        y = base - amp * pts[i] - tilt * x
        for px, ph, pw in peaks:
            t = max(0, 1 - abs(x - px) / pw)
            y -= ph * (t * t * (3 - 2 * t) * 0.5 + t * 0.5)
        res.append((x * W, y))
    return res


# The noise below is drawn in a fixed order; `low`, `mid` and `grain` are not used in the image
# but consume noise, so removing them would change the texture.
low = fbm(3, 2, 3)
mid = fbm(24, 15, 5, 0.55)
grain = Image.effect_noise((W, H), 64).convert("F").point(lambda v: v - 128)
spk = Image.effect_noise((W, H), 90).convert("L")
dark_spk = spk.point(lambda v: 255 if v < 18 else 0).filter(ImageFilter.GaussianBlur(0.8))
light_spk = spk.point(lambda v: 255 if v > 242 else 0).filter(ImageFilter.GaussianBlur(0.8))

# Texture.
lo, hi = (206, 194, 176), (244, 239, 230)
tex = fbm(5, 3, 9, 0.78)
veins = fbm(10, 6, 6, 0.6)
vabs = ImageMath.lambda_eval(lambda d: abs(d["v"]), v=veins)
fine = Image.effect_noise((W, H), 64).convert("F").point(lambda v: v - 128)
fine2 = (
    Image.effect_noise((W // 2, H // 2), 64)
    .resize((W, H), Image.BICUBIC)
    .convert("F")
    .point(lambda v: v - 128)
)
F = add((tex, TEXW), (vabs, -0.5 * TEXW), (fine, GRAIN), (fine2, GRAIN2))
img, L = colourise(F, lo, hi, 178, 1.45)
img = Image.composite(Image.new("RGB", (W, H), (176, 162, 142)), img, dark_spk.point(lambda v: v * 0.30))
img = Image.composite(Image.new("RGB", (W, H), (251, 248, 242)), img, light_spk.point(lambda v: v * 0.35))
base_img = img.copy()

# Ridges, far to near: (base y, roughness amplitude, roughness, peaks (x, height, width), tilt,
# RGB multiplier, opacity).
layers = [
    (1120, 70, 0.62, [(0.80, 440, 0.17), (0.95, 330, 0.10), (0.64, 210, 0.11)], 200, (0.955, 0.945, 0.93), 0.8),
    (1290, 55, 0.6, [(0.88, 280, 0.13), (0.72, 170, 0.11)], 180, (0.925, 0.91, 0.89), 0.88),
    (1450, 40, 0.6, [(0.97, 160, 0.15), (0.80, 80, 0.14)], 150, (NEAR * 0.87, NEAR * 0.85, NEAR * 0.825), 0.95),
]
for base, amp, rough, peaks, tilt, mul, alpha in layers:
    p = ridge(2049, base, amp, rough, peaks, tilt)
    m = Image.new("L", (W, H), 0)
    ImageDraw.Draw(m).polygon(p + [(W, H), (0, H)], fill=255)
    m = m.filter(ImageFilter.GaussianBlur(0.9))
    # Haze: each ridge fades toward its top.
    top = int(min(y for _, y in p))
    hgt = H - top
    haze = Image.linear_gradient("L").resize((W, max(1, hgt))).point(lambda v: int(255 - v * 0.6))
    hz = Image.new("L", (W, H), 255)
    hz.paste(haze, (0, top))
    m = ImageChops.multiply(m, hz).point(lambda v, a=alpha: int(v * a))
    r, g, b = base_img.split()
    tint = Image.merge(
        "RGB",
        [r.point(lambda v: v * mul[0]), g.point(lambda v: v * mul[1]), b.point(lambda v: v * mul[2])],
    )
    img = Image.composite(tint, img, m)

# Fade the ridges out toward the left.
gx = (
    Image.linear_gradient("L")
    .rotate(90)
    .resize((W, H))
    .point(lambda v: int(max(0, min(255, (255 - v - 90) * 2.0))))
)
img = Image.composite(base_img, img, gx)
img.save(out)
