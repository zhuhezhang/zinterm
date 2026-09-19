# -*- coding: utf-8 -*-
"""Generate the ZinTerm app icon.

Brand mark: charcoal rounded tile + geometric "Z" (accent blue) with a
terminal block cursor (teal). Pure-Pillow, 4× supersampled — same pipeline
as upstream meatshell (https://github.com/yituorou/meatshell), so title-bar
18px downscales stay crisp (no soft Z-glow / heavy Gaussian smear).

Outputs (run from assets/):
  icon.png, icon@512.png, zinterm.ico

The custom title bar uses assets/icon.svg (vector) so 18px stays sharp on
HiDPI; these PNGs cover Window.icon, About, Linux/macOS packaging, and the
embedded Windows .ico.
"""
from PIL import Image, ImageDraw, ImageFilter, ImageChops

SS = 4
BASE = 256
S = BASE * SS  # working canvas 1024

# Palette aligned with ui/shared/theme.slint (dark)
TILE_TOP = (35, 38, 48)
TILE_BOT = (22, 24, 30)
PANEL = (14, 15, 19)
ACCENT = (74, 144, 226)       # Theme.accent dark
ACCENT_HI = (122, 180, 245)
CURSOR = (78, 201, 176)       # Theme.success dark


def lerp(a, b, t):
    return int(round(a + (b - a) * t))


def vgradient(size, top, bot):
    w, h = size
    col = Image.new("RGB", (1, h))
    px = col.load()
    for y in range(h):
        t = y / max(h - 1, 1)
        px[0, y] = (lerp(top[0], bot[0], t),
                    lerp(top[1], bot[1], t),
                    lerp(top[2], bot[2], t))
    return col.resize((w, h))


def draw_z(draw, box, fill, width):
    """Thick geometric Z that stays legible at 16×16."""
    x0, y0, x1, y1 = box
    w = x1 - x0
    t = max(width, int(w * 0.20))
    r = max(2, t // 3)
    draw.rounded_rectangle([x0, y0, x1, y0 + t], radius=r, fill=fill)
    draw.rounded_rectangle([x0, y1 - t, x1, y1], radius=r, fill=fill)
    half = t * 0.55
    pts = [
        (x1, y0 + t * 0.35),
        (x1, y0 + t * 0.35 + half * 2),
        (x0, y1 - t * 0.35),
        (x0, y1 - t * 0.35 - half * 2),
    ]
    draw.polygon(pts, fill=fill)


img = Image.new("RGBA", (S, S), (0, 0, 0, 0))

# ---------------------------------------------------------------- tile (same approach as meatshell)
tile_mask = Image.new("L", (S, S), 0)
ImageDraw.Draw(tile_mask).rounded_rectangle(
    [0, 0, S - 1, S - 1], radius=int(S * 0.22), fill=255)

grad = vgradient((S, S), TILE_TOP, TILE_BOT).convert("RGBA")
img.paste(grad, (0, 0), tile_mask)

# subtle top sheen (opacity matched to meatshell)
sheen = Image.new("RGBA", (S, S), (0, 0, 0, 0))
ImageDraw.Draw(sheen).ellipse(
    [int(-S * 0.3), int(-S * 0.55), int(S * 1.3), int(S * 0.35)],
    fill=(255, 255, 255, 22))
sheen.putalpha(ImageChops.multiply(sheen.getchannel("A"), tile_mask))
img = Image.alpha_composite(img, sheen)

# ---------------------------------------------------------------- inner terminal panel
px0, py0 = int(S * 0.14), int(S * 0.14)
px1, py1 = int(S * 0.86), int(S * 0.86)
pr = int(S * 0.10)

# drop shadow — meatshell-scale blur (≈2.5% of canvas), not a soft halo
shadow = Image.new("RGBA", (S, S), (0, 0, 0, 0))
ImageDraw.Draw(shadow).rounded_rectangle(
    [px0, py0 + int(S * 0.02), px1, py1 + int(S * 0.03)],
    radius=pr, fill=(0, 0, 0, 150))
shadow = shadow.filter(ImageFilter.GaussianBlur(S * 0.025))
img = Image.alpha_composite(img, shadow)

panel = Image.new("RGBA", (S, S), (0, 0, 0, 0))
ImageDraw.Draw(panel).rounded_rectangle(
    [px0, py0, px1, py1], radius=pr, fill=PANEL + (255,))
# thin rim so the panel edge stays defined after 18px downscale
ImageDraw.Draw(panel).rounded_rectangle(
    [px0, py0, px1, py1], radius=pr,
    outline=(58, 62, 72, 200), width=max(2, int(S * 0.006)))
img = Image.alpha_composite(img, panel)

# window chrome dots — fully opaque (semi-transparent fill softens at 18px)
dot_y = py0 + int((py1 - py0) * 0.11)
dot_r = max(3, int(S * 0.020))
dots = Image.new("RGBA", (S, S), (0, 0, 0, 0))
dd = ImageDraw.Draw(dots)
colors = [(232, 92, 92, 255), (226, 168, 74, 255), (78, 201, 176, 255)]
gap = int(S * 0.055)
x = px0 + int(S * 0.055)
for c in colors:
    dd.ellipse([x - dot_r, dot_y - dot_r, x + dot_r, dot_y + dot_r], fill=c)
    x += gap
img = Image.alpha_composite(img, dots)

# ---------------------------------------------------------------- Z mark (no soft glow — that caused mushy edges at title-bar size)
zx0 = px0 + int(S * 0.16)
zy0 = py0 + int(S * 0.22)
zx1 = px1 - int(S * 0.16)
zy1 = py1 - int(S * 0.20)

# dark contact shadow under Z for contrast on dark panel (tiny blur like meatshell prompt)
z_shadow = Image.new("RGBA", (S, S), (0, 0, 0, 0))
draw_z(ImageDraw.Draw(z_shadow),
       (zx0 + int(S * 0.012), zy0 + int(S * 0.012),
        zx1 + int(S * 0.012), zy1 + int(S * 0.012)),
       (0, 0, 0, 160), int((zx1 - zx0) * 0.20))
z_shadow = z_shadow.filter(ImageFilter.GaussianBlur(1.2))
img = Image.alpha_composite(img, z_shadow)

zlayer = Image.new("RGBA", (S, S), (0, 0, 0, 0))
zd = ImageDraw.Draw(zlayer)
draw_z(zd, (zx0, zy0, zx1, zy1), ACCENT + (255,), int((zx1 - zx0) * 0.20))
# solid highlight stripe (opaque-ish) instead of a soft translucent wash
hi_t = max(2, int((zx1 - zx0) * 0.05))
zd.rounded_rectangle(
    [zx0 + int(S * 0.01), zy0 + int(S * 0.008),
     zx1 - int(S * 0.01), zy0 + hi_t],
    radius=max(1, hi_t // 2), fill=ACCENT_HI + (160,))
img = Image.alpha_composite(img, zlayer)

# ---------------------------------------------------------------- block cursor (solid, meatshell-style contrast)
cw = int(S * 0.085)
ch = int(S * 0.11)
cx1 = min(zx1 + int(S * 0.02), px1 - int(S * 0.06))
cy1 = min(zy1 - int(S * 0.01), py1 - int(S * 0.08))
cx0 = cx1 - cw
cy0 = cy1 - ch

cursor = Image.new("RGBA", (S, S), (0, 0, 0, 0))
cd = ImageDraw.Draw(cursor)
cd.rounded_rectangle([cx0, cy0, cx1, cy1], radius=max(2, S // 140),
                     fill=CURSOR + (255,))
img = Image.alpha_composite(img, cursor)

# clip to rounded tile
img.putalpha(ImageChops.multiply(img.getchannel("A"), tile_mask))

# ---------------------------------------------------------------- export
img256 = img.resize((BASE, BASE), Image.LANCZOS)
img512 = img.resize((512, 512), Image.LANCZOS)
img256.save("icon.png")
img512.save("icon@512.png")
img256.save("zinterm.ico", format="ICO",
            sizes=[(256, 256), (128, 128), (64, 64),
                   (48, 48), (32, 32), (16, 16)])
print("OK: icon.png, icon@512.png, zinterm.ico written")
