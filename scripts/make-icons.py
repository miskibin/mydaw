"""Build WAVE's app icons from the logo source.

Input : scripts/logo-source.webp (1254x1254 RGBA, dark rounded square with a
        white/blue "W" of piano keys, on a white background).
Output: app/icons/{icon.png, source.png, 32x32.png, 128x128.png,
        128x128@2x.png, icon.ico} and ui/public/favicon.png.

Re-runnable: run with the project Python, e.g. `python scripts/make-icons.py`.
"""

from pathlib import Path

from PIL import Image, ImageChops, ImageDraw, ImageFilter

ROOT = Path(__file__).resolve().parent.parent
SOURCE = ROOT / "scripts" / "logo-source.webp"
ICONS = ROOT / "app" / "icons"
FAVICON = ROOT / "ui" / "public" / "favicon.png"

FLOOD_TOLERANCE = 40
MARKER = (255, 0, 255)  # temporary colour painted over the outer background
ICO_SIZES = [(16, 16), (24, 24), (32, 32), (48, 48), (64, 64), (128, 128), (256, 256)]


def outer_background_mask(rgb: Image.Image) -> Image.Image:
    """Return an L mask: 255 = opaque, 0 = outer white background.

    Only the white region connected to the image corners is flood-filled, so
    the white piano keys inside the dark square are never reached.
    """
    work = rgb.copy()
    w, h = work.size
    for xy in [(0, 0), (w - 1, 0), (0, h - 1), (w - 1, h - 1)]:
        if work.getpixel(xy) != MARKER:
            ImageDraw.floodfill(work, xy, MARKER, thresh=FLOOD_TOLERANCE)
    r, g, b = work.split()
    is_r = r.point(lambda v: 255 if v == MARKER[0] else 0)
    is_g = g.point(lambda v: 255 if v == MARKER[1] else 0)
    is_b = b.point(lambda v: 255 if v == MARKER[2] else 0)
    marked = ImageChops.multiply(ImageChops.multiply(is_r, is_g), is_b)
    # marked = 255 on the outer background; opaque = inverse
    return ImageChops.invert(marked)


def build_master(src_path: Path) -> Image.Image:
    img = Image.open(src_path).convert("RGBA")
    rgb = img.convert("RGB")
    opaque = outer_background_mask(rgb)
    # Erode by 1 px so anti-aliased white edge pixels do not leave a fringe,
    # then soften the remaining edge with a light blur.
    opaque = opaque.filter(ImageFilter.MinFilter(3))
    opaque = opaque.filter(ImageFilter.GaussianBlur(radius=0.7))
    out = img.copy()
    out.putalpha(opaque)

    bbox = out.getchannel("A").getbbox()
    out = out.crop(bbox)
    side = max(out.size)
    square = Image.new("RGBA", (side, side), (0, 0, 0, 0))
    square.paste(out, ((side - out.width) // 2, (side - out.height) // 2))
    return square.resize((1024, 1024), Image.LANCZOS)


def main() -> None:
    ICONS.mkdir(parents=True, exist_ok=True)
    FAVICON.parent.mkdir(parents=True, exist_ok=True)

    master = build_master(SOURCE)
    master.save(ICONS / "icon.png")
    master.save(ICONS / "source.png")

    def scaled(size: int) -> Image.Image:
        return master.resize((size, size), Image.LANCZOS)

    scaled(32).save(ICONS / "32x32.png")
    scaled(128).save(ICONS / "128x128.png")
    scaled(256).save(ICONS / "128x128@2x.png")
    master.save(ICONS / "icon.ico", format="ICO", sizes=ICO_SIZES)
    scaled(64).save(FAVICON)
    print("icons written to", ICONS, "and", FAVICON)


if __name__ == "__main__":
    main()
