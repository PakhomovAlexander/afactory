#!/usr/bin/env python3
"""afactory brand assets, derived from the four source SVGs in logo/source/.

The source files are the design (CorelDRAW exports, kept verbatim). Everything
else in brand/ is generated from them by this script, so a change to the design
is a change to logo/source/ followed by `python3 brand/gen.py`.

Derived files:
  logo/wordmark.svg            letters in currentColor, workers in brand colour (inline use)
  logo/wordmark-on-dark.svg    letters in paper, for <img> on an ink ground (README, OG)
  logo/wordmark-on-light.svg   letters in ink, for <img> on a paper ground
  logo/wordmark-mono.svg       letters currentColor, workers grey, eyes ink (one-colour print)
  logo/wordmark-one-ink.svg    everything currentColor, eyes cut out (stamps, etching)
  worker/worker-{pink,green,blue}.svg   one worker, cropped, exact vector
  worker/worker.svg            body currentColor, eyes ink (tintable)
  worker/worker-{pink,green,blue}.png   202 px rasters
  mark/mark-{pink,green,blue}.svg       the 16-cell pixel worker: favicons, app icons
  mark/mark.svg                = mark-pink.svg
  mark/mark-tile.svg           pink worker on an ink tile (opaque icon contexts)
  mark/favicon-{16,32,180,512}.png
  banner/banner-{dark,light}.svg        README / OG banner with the tagline
  banner/social-preview.png             1280x640, GitHub social preview
  banner/og.png                         1200x630, website Open Graph card
  ascii.txt                    the ASCII worker and wordmark for terminals

Needs rsvg-convert (brew install librsvg) and Pillow for the rasters.
"""

from __future__ import annotations

import re
import shutil
import subprocess
from pathlib import Path

HERE = Path(__file__).parent
SRC = HERE / "logo" / "source"

# ---- palette: the seven colours the logo files use ---------------------------
INK = "#0F0F0F"
PAPER = "#F6EDE4"
WHITE = "#FFFFFF"
BLUE = "#5195F5"
PINK = "#EE366A"
GREEN = "#36EEA8"
GREY = "#999999"

WORKERS = {"pink": PINK, "green": GREEN, "blue": BLUE}
TAGLINE = "agent pipelines made fast"
MONO = "'Fira Code', ui-monospace, SFMono-Regular, Menlo, Consolas, monospace"


# ---- reading the source ----------------------------------------------------
def read_source(name: str) -> tuple[str, list[tuple[str, str, str]]]:
    """viewBox and the (tag, fill, geometry) list of one source SVG."""
    text = (SRC / name).read_text()
    viewbox = re.search(r'viewBox="([^"]+)"', text).group(1)
    classes = {
        m.group(1): m.group(2).lower()
        for m in re.finditer(r"\.(fil\d+)\s*\{fill:([^}]+)\}", text)
    }
    elements = []
    for m in re.finditer(r'<(path|polygon) class="(fil\d+)" (?:d|points)="([^"]+)"', text):
        tag, cls, geom = m.groups()
        elements.append((tag, classes[cls], geom.strip()))
    return viewbox, elements


def element(tag: str, geom: str, fill: str, extra: str = "") -> str:
    attr = "d" if tag == "path" else "points"
    return f'<{tag} {attr}="{geom}" fill="{fill}"{extra}/>'


def bbox(tag: str, geom: str) -> tuple[float, float, float, float]:
    """Axis-aligned box of an absolute polygon or an M/m/L/l/z path (the only kinds here)."""
    xs, ys = [], []
    if tag == "polygon":
        for pair in geom.split():
            x, y = pair.split(",")
            xs.append(float(x))
            ys.append(float(y))
        return min(xs), min(ys), max(xs), max(ys)
    x = y = sx = sy = 0.0
    for cmd, args in re.findall(r"([MmLlZz])([^MmLlZz]*)", geom):
        if cmd in "Zz":
            x, y = sx, sy
            continue
        nums = [float(v) for v in args.replace(",", " ").split()]
        for i in range(0, len(nums), 2):
            dx, dy = nums[i], nums[i + 1]
            absolute = cmd in "ML"
            x, y = (dx, dy) if absolute else (x + dx, y + dy)
            if cmd in "Mm" and i == 0:
                sx, sy = x, y
            xs.append(x)
            ys.append(y)
    return min(xs), min(ys), max(xs), max(ys)


def classify(elements):
    """Split a colour source into letters, worker bodies (by colour) and eyes."""
    letters, bodies, eyes = [], {}, []
    for tag, fill, geom in elements:
        f = fill.upper()
        if f in {"BLACK", "#000000"}:
            eyes.append((tag, geom))
        elif f in {BLUE, PINK, GREEN, GREY}:
            bodies[{BLUE: "blue", PINK: "pink", GREEN: "green", GREY: "grey"}[f]] = (tag, geom)
        else:
            letters.append((tag, geom))
    return letters, bodies, eyes


def eyes_of(body_box, eyes):
    """The eye path that sits inside a body's box."""
    x0, y0, x1, y1 = body_box
    for tag, geom in eyes:
        ex0, ey0, ex1, ey1 = bbox(tag, geom)
        if x0 <= ex0 and ex1 <= x1 and y0 <= ey0 and ey1 <= y1:
            return tag, geom
    raise ValueError("no eyes inside body")


def svg(width, height, viewbox, body, title, extra_attrs=""):
    return (
        f'<svg xmlns="http://www.w3.org/2000/svg" width="{width}" height="{height}" '
        f'viewBox="{viewbox}"{extra_attrs}>\n  <title>{title}</title>\n{body}\n</svg>\n'
    )


def num(v: float) -> str:
    return f"{v:.2f}".rstrip("0").rstrip(".")


# ---- wordmarks -------------------------------------------------------------
def wordmarks(out: Path):
    viewbox, elements = read_source("afactory_logo_white_color.svg")
    letters, bodies, eyes = classify(elements)
    vw, vh = (float(v) for v in viewbox.split()[2:])
    ratio = vh / vw
    w = 780
    h = round(w * ratio, 2)

    def letters_svg(fill):
        return "\n".join(f"  {element(t, g, fill)}" for t, g in letters)

    def workers_svg(colour_of):
        parts = []
        for name, (t, g) in bodies.items():
            parts.append(f"  {element(t, g, colour_of(name))}")
        for t, g in eyes:
            parts.append(f"  {element(t, g, INK)}")
        return "\n".join(parts)

    brand = lambda n: WORKERS[n]
    files = {
        "wordmark.svg": (letters_svg("currentColor"), workers_svg(brand)),
        "wordmark-on-dark.svg": (letters_svg(PAPER), workers_svg(brand)),
        "wordmark-on-light.svg": (letters_svg(INK), workers_svg(brand)),
        "wordmark-mono.svg": (letters_svg("currentColor"), workers_svg(lambda n: GREY)),
    }
    for name, (lt, wk) in files.items():
        (out / name).write_text(svg(w, h, viewbox, f"{lt}\n{wk}", "afactory"))

    # one ink: letters and bodies in currentColor, the eyes cut out through a mask
    mask = [f'  <mask id="eyes"><rect x="0" y="0" width="{vw}" height="{vh}" fill="white"/>']
    mask += [f"    {element(t, g, 'black')}" for t, g in eyes]
    mask.append("  </mask>")
    body = "\n".join(mask)
    body += f'\n  <g mask="url(#eyes)">\n{letters_svg("currentColor")}\n'
    body += "\n".join(f"    {element(t, g, 'currentColor')}" for t, g in bodies.values())
    body += "\n  </g>"
    (out / "wordmark-one-ink.svg").write_text(svg(w, h, viewbox, body, "afactory"))
    return viewbox, letters, bodies, eyes


# ---- workers ---------------------------------------------------------------
def workers(out: Path, bodies, eyes):
    boxes = {}
    for name, (t, g) in bodies.items():
        x0, y0, x1, y1 = bbox(t, g)
        et, eg = eyes_of((x0, y0, x1, y1), eyes)
        w, h = x1 - x0, y1 - y0
        pad = w * 0.02
        viewbox = f"{num(x0 - pad)} {num(y0 - pad)} {num(w + 2 * pad)} {num(h + 2 * pad)}"
        boxes[name] = (x0, y0, x1, y1, et, eg)
        px_w = 202
        px_h = round(px_w * (h + 2 * pad) / (w + 2 * pad), 2)
        body = f"  {element(t, g, WORKERS[name])}\n  {element(et, eg, INK)}"
        (out / f"worker-{name}.svg").write_text(svg(px_w, px_h, viewbox, body, f"afactory worker, {name}"))
        if name == "pink":
            tint = f"  {element(t, g, 'currentColor')}\n  {element(et, eg, INK)}"
            (out / "worker.svg").write_text(svg(px_w, px_h, viewbox, tint, "afactory worker"))
    return boxes


# ---- the pixel mark (16-cell grid) -----------------------------------------
# Proportions from the vector worker: base 1.30x body, chimney 1/3 body, eyes 1/4 body,
# body 1.28:1, eye rows top 34% / eye 51% / bottom 15%.
MARK_ROWS = [
    "................",
    "................",
    "......XXXX......",
    "......XXXX......",
    "..XXXXXXXXXXXX..",
    "..XXXXXXXXXXXX..",
    "..XXXXXXXXXXXX..",
    "..XEEEXXXXEEEX..",
    "..XEEEXXXXEEEX..",
    "..XEEEXXXXEEEX..",
    "..XEEEXXXXEEEX..",
    "..XEEEXXXXEEEX..",
    "..XXXXXXXXXXXX..",
    "XXXXXXXXXXXXXXXX",
    "XXXXXXXXXXXXXXXX",
    "................",
]


def rects(rows, colours, ox=0, oy=0):
    """Horizontal runs of one colour become one rect each."""
    out = []
    for r, row in enumerate(rows):
        c = 0
        while c < len(row):
            ch = row[c]
            if ch not in colours:
                c += 1
                continue
            start = c
            while c < len(row) and row[c] == ch:
                c += 1
            out.append(
                f'  <rect x="{ox + start}" y="{oy + r}" width="{c - start}" height="1" fill="{colours[ch]}"/>'
            )
    return "\n".join(out)


def marks(out: Path):
    crisp = ' shape-rendering="crispEdges"'
    for name, colour in WORKERS.items():
        body = rects(MARK_ROWS, {"X": colour, "E": INK})
        (out / f"mark-{name}.svg").write_text(svg(16, 16, "0 0 16 16", body, "afactory", crisp))
    shutil.copy(out / "mark-pink.svg", out / "mark.svg")
    tile = (
        f'  <rect x="0" y="0" width="16" height="16" rx="3" fill="{INK}"/>\n'
        + rects(MARK_ROWS, {"X": PINK, "E": INK})
    )
    (out / "mark-tile.svg").write_text(svg(16, 16, "0 0 16 16", tile, "afactory", crisp))


# ---- banner ------------------------------------------------------------------
def banners(out: Path, viewbox, letters, bodies, eyes):
    vw, vh = (float(v) for v in viewbox.split()[2:])
    W, H = 1200, 320
    mark_w = 840
    scale = mark_w / vw
    mark_h = vh * scale
    x = (W - mark_w) / 2
    y = 88
    for theme, ground, ink, muted in (
        ("dark", INK, PAPER, "#9E9993"),
        ("light", PAPER, INK, "#676360"),
    ):
        parts = [f'  <rect width="{W}" height="{H}" fill="{ground}"/>']
        parts.append(f'  <g transform="translate({num(x)} {num(y)}) scale({scale:.6f})">')
        parts += [f"    {element(t, g, ink)}" for t, g in letters]
        parts += [f"    {element(t, g, WORKERS[n])}" for n, (t, g) in bodies.items()]
        parts += [f"    {element(t, g, INK)}" for t, g in eyes]
        parts.append("  </g>")
        ty = y + mark_h + 62
        parts.append(
            f'  <text x="{W / 2}" y="{num(ty)}" text-anchor="middle" font-family="{MONO}" '
            f'font-size="26" fill="{muted}">{TAGLINE}</text>'
        )
        (out / f"banner-{theme}.svg").write_text(
            svg(W, H, f"0 0 {W} {H}", "\n".join(parts), f"afactory: {TAGLINE}")
        )


def card(out: Path, name: str, W: int, H: int, banner: Path, inner_w: int):
    """A W x H raster of the dark banner centred on ink (social preview, OG card)."""
    text = banner.read_text()
    bw = int(re.search(r'width="(\d+)"', text).group(1))
    bh = int(re.search(r'height="(\d+)"', text).group(1))
    inner = re.sub(r"^<svg[^>]*>\s*<title>[^<]*</title>", "", text.strip())
    inner = inner[: inner.rfind("</svg>")]
    # drop the banner's own ground; the card paints its own
    inner = re.sub(r'<rect width="\d+" height="\d+" fill="[^"]+"/>', "", inner, count=1)
    s = inner_w / bw
    x, y = (W - bw * s) / 2, (H - bh * s) / 2
    tmp = out / f".{name}.svg"
    tmp.write_text(
        f'<svg xmlns="http://www.w3.org/2000/svg" width="{W}" height="{H}" viewBox="0 0 {W} {H}">'
        f'<rect width="{W}" height="{H}" fill="{INK}"/>'
        f'<g transform="translate({x:.2f} {y:.2f}) scale({s:.5f})">{inner}</g></svg>'
    )
    subprocess.run(["rsvg-convert", "-w", str(W), "-h", str(H), str(tmp), "-o", str(out / name)], check=True)
    tmp.unlink()


# ---- rasters -----------------------------------------------------------------
def rasterise(mark_dir: Path, worker_dir: Path):
    from PIL import Image

    src = mark_dir / "mark.svg"
    for size, scale in ((16, 1), (32, 2), (512, 32)):
        subprocess.run(
            ["rsvg-convert", "-w", str(16 * scale), "-h", str(16 * scale), str(src), "-o", str(mark_dir / f"favicon-{size}.png")],
            check=True,
        )
    # 180: an integer 11x render (176) padded to 180 keeps the pixels square.
    tmp = mark_dir / ".176.png"
    subprocess.run(["rsvg-convert", "-w", "176", "-h", "176", str(mark_dir / "mark-tile.svg"), "-o", str(tmp)], check=True)
    im = Image.open(tmp).convert("RGBA")
    pad = Image.new("RGBA", (180, 180), INK)
    pad.paste(im, (2, 2), im)
    pad.save(mark_dir / "favicon-180.png")
    tmp.unlink()
    for name in WORKERS:
        subprocess.run(
            ["rsvg-convert", "-w", "202", str(worker_dir / f"worker-{name}.svg"), "-o", str(worker_dir / f"worker-{name}.png")],
            check=True,
        )


# ---- ascii -------------------------------------------------------------------
ASCII = """\
afactory in printable ASCII, for terminals (the TUI paints only 0x20..0x7e).

The worker, 11 x 6:

     ###
  #########
  #  ###  #
  #  ###  #
  #########
 ###########

The wordmark, 5 rows, block capitals (never a figlet face):

 ####  ##### ####  ####  ##### ####  ####  #   #
 #  #  #     #  #  #       #   #  #  #  #  #   #
 ####  ####  ####  #       #   #  #  ####   ###
 #  #  #     #  #  #       #   #  #  # #     #
 #  #  #     #  #  ####    #   ####  #  #    #

`af` alone, 3 rows, for a status line or a version banner:

 ####  ####
 #  #  ##
 #  #  #
"""


def main():
    logo, worker, mark, banner = (HERE / d for d in ("logo", "worker", "mark", "banner"))
    for d in (logo, worker, mark, banner):
        d.mkdir(parents=True, exist_ok=True)
    viewbox, letters, bodies, eyes = wordmarks(logo)
    workers(worker, bodies, eyes)
    marks(mark)
    banners(banner, viewbox, letters, bodies, eyes)
    card(banner, "social-preview.png", 1280, 640, banner / "banner-dark.svg", 1080)
    card(banner, "og.png", 1200, 630, banner / "banner-dark.svg", 1040)
    rasterise(mark, worker)
    (HERE / "ascii.txt").write_text(ASCII)
    print("brand assets regenerated under", HERE)


if __name__ == "__main__":
    main()
