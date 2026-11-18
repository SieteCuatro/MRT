"""Draws MRT's icon: the brand mark from the sidebar (a rounded lavender square with
a dark `›_` prompt), as the files the build and the Linux package use.

    python scripts/icons/make_icon.py

Needs Pillow. Writes:
  crates/mrt-app/assets/mrt.ico      Windows executable icon (16-256 px), embedded by build.rs
  crates/mrt-app/assets/mrt-256.png  window and taskbar icon, loaded by main.rs
  packaging/linux/mrt.png            512 px icon for the desktop entry (Icon=mrt)
"""
import os
from PIL import Image, ImageDraw

ROOT = os.path.dirname(os.path.dirname(os.path.dirname(os.path.abspath(__file__))))
ACCENT = (142, 162, 255, 255)     # Palette::ACCENT (dark theme)
ON_ACCENT = (15, 18, 34, 255)     # Palette::ON_ACCENT
SUPER = 4                         # draw large, then downscale for smooth edges


def draw(size: int) -> Image.Image:
    s = size * SUPER
    img = Image.new("RGBA", (s, s), (0, 0, 0, 0))
    d = ImageDraw.Draw(img)
    # Small sizes use more of the square so the mark stays readable.
    margin = round(s * (0.04 if size <= 32 else 0.06))
    radius = round(s * 0.22)
    d.rounded_rectangle([margin, margin, s - margin - 1, s - margin - 1], radius=radius, fill=ACCENT)

    stroke = max(round(s * (0.11 if size <= 24 else 0.085)), 1)
    # The `›` chevron.
    cx, cy = s * 0.33, s * 0.5
    arm = s * 0.15
    points = [(cx - arm * 0.55, cy - arm), (cx + arm * 0.55, cy), (cx - arm * 0.55, cy + arm)]
    d.line(points, fill=ON_ACCENT, width=stroke, joint="curve")
    for x, y in (points[0], points[2]):
        r = stroke / 2
        d.ellipse([x - r, y - r, x + r, y + r], fill=ON_ACCENT)
    # The `_` cursor.
    x0, x1 = s * 0.50, s * 0.74
    y = cy + arm - stroke / 2
    d.rounded_rectangle([x0, y, x1, y + stroke], radius=stroke / 2, fill=ON_ACCENT)
    return img.resize((size, size), Image.LANCZOS)


def main() -> None:
    assets = os.path.join(ROOT, "crates", "mrt-app", "assets")
    os.makedirs(assets, exist_ok=True)
    sizes = [16, 20, 24, 32, 40, 48, 64, 128, 256]
    images = [draw(n) for n in sizes]
    images[-1].save(os.path.join(assets, "mrt.ico"), format="ICO", sizes=[(n, n) for n in sizes], append_images=images[:-1])
    images[-1].save(os.path.join(assets, "mrt-256.png"), optimize=True)
    draw(512).save(os.path.join(ROOT, "packaging", "linux", "mrt.png"), optimize=True)
    print("icons written")


if __name__ == "__main__":
    main()
