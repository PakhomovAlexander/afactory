"""The laboratory's one trusted offline renderer (package p4, ADR-0172).

Every page is one self-contained HTML document built from validated evidence by this code: no
template, script, style or image comes from evidence. Every evidence string is escaped, every
attribute quoted, and the Content-Security-Policy allows only the one bundled script and style
block by their SHA-256 hashes and local images; nothing is loaded from a network, a CDN or an
evidence URL, and nothing launches a browser. Links name only files the renderer itself placed
below the page's own directory, by validated relative paths.
"""
import base64
from fractions import Fraction
import hashlib
import html
from urllib.parse import quote

from .values import ContractError, check_relative_path

STYLE = """
body { font-family: system-ui, sans-serif; margin: 1.5rem; color: #1b1f24; }
h1 { font-size: 1.4rem; } h2 { font-size: 1.15rem; margin-top: 2rem; }
table { border-collapse: collapse; margin: 0.5rem 0; }
th, td { border: 1px solid #c8ccd1; padding: 0.2rem 0.45rem; text-align: left;
  vertical-align: top; font-size: 0.85rem; }
th { background: #f0f2f4; }
code, .mono { font-family: ui-monospace, monospace; font-size: 0.8rem; word-break: break-all; }
.pass { color: #116329; font-weight: bold; } .fail { color: #a40e26; font-weight: bold; }
.incomplete { color: #7d4e00; font-weight: bold; }
.note { background: #fff8c5; border: 1px solid #d4a72c; padding: 0.4rem 0.6rem; }
.unavailable { color: #57606a; font-style: italic; }
svg { border: 1px solid #c8ccd1; background: #fcfcfd; margin: 0.25rem 0; }
.base { fill: #0969da; } .cand { fill: #cf222e; } .warm { opacity: 0.45; }
.failed { fill: none; stroke: #6e7781; stroke-width: 1.5; }
.missing { fill: none; stroke: #bf8700; stroke-width: 1; }
.axis { stroke: #8c959f; stroke-width: 1; } .label { font-size: 10px; fill: #57606a; }
#search { margin: 0.5rem 0; padding: 0.3rem; width: 24rem; }
.verdict { font-size: 1.05rem; }
details { margin: 0.4rem 0; } summary { cursor: pointer; color: #0969da; }
"""

SCRIPT = """
(function () {
  var input = document.getElementById("search");
  if (!input) { return; }
  input.addEventListener("input", function () {
    var needle = input.value.toLowerCase();
    var rows = document.querySelectorAll("table.searchable tbody tr");
    for (var i = 0; i < rows.length; i++) {
      rows[i].hidden = needle !== "" &&
        rows[i].textContent.toLowerCase().indexOf(needle) < 0;
    }
  });
}());
"""


def source_hash(text):
    """The CSP hash source of an inline block's exact text."""
    return "'sha256-%s'" % base64.b64encode(hashlib.sha256(text.encode("utf-8")).digest()
                                            ).decode("ascii")


CSP = ("default-src 'none'; script-src %s; style-src %s; img-src 'self'; base-uri 'none'; "
       "form-action 'none'" % (source_hash(SCRIPT), source_hash(STYLE)))


def escape(value):
    """Evidence text as HTML text or a quoted attribute value."""
    return html.escape("" if value is None else str(value), quote=True)


def link(path, text):
    """A link to a file the renderer placed at `path` below the page's directory; any other
    path is refused, so no evidence can name a URL, an absolute path or a traversal."""
    check_relative_path(path, "a report link")
    if path.startswith(".") or "/." in path:
        raise ContractError("a report link names a hidden or relative-dot path: " + path)
    return '<a href="%s" download>%s</a>' % (escape(quote(path, safe="/")), escape(text))


def image(path, alt):
    """A local image the renderer placed at `path` below the page's directory (a sanitized
    SVG, ADR-0174); any other path is refused, like a link's."""
    check_relative_path(path, "a report image")
    if path.startswith(".") or "/." in path:
        raise ContractError("a report image names a hidden or relative-dot path: " + path)
    return '<img src="%s" alt="%s">' % (escape(quote(path, safe="/")), escape(alt))


def table(headers, rows, searchable=False):
    """A table of escaped cells; a cell that is a `Markup` is the renderer's own HTML."""
    out = ['<table%s><thead><tr>' % (' class="searchable"' if searchable else "")]
    out.extend("<th>%s</th>" % escape(header) for header in headers)
    out.append("</tr></thead><tbody>")
    for row in rows:
        out.append("<tr>")
        for cell in row:
            out.append("<td>%s</td>" % (cell.text if isinstance(cell, Markup) else escape(cell)))
        out.append("</tr>")
    out.append("</tbody></table>")
    return "".join(out)


class Markup(object):
    """HTML the renderer built itself from escaped parts."""

    def __init__(self, text):
        self.text = text


def code(value):
    return Markup('<code>%s</code>' % escape(value))


def outcome(result, text=None):
    css = result if result in ("pass", "fail", "incomplete") else "unavailable"
    return Markup('<span class="%s">%s</span>' % (css, escape(text or result)))


def page(title, sections, search=True):
    """One self-contained page: the hash-authorized style and script and the sections, each
    already rendered by this module."""
    parts = ['<!DOCTYPE html>\n<html lang="en"><head><meta charset="utf-8">',
             '<meta http-equiv="Content-Security-Policy" content="%s">' % escape(CSP),
             '<meta name="referrer" content="no-referrer">',
             "<title>%s</title>" % escape(title),
             "<style>%s</style></head><body>" % STYLE,
             "<h1>%s</h1>" % escape(title)]
    if search:
        parts.append('<input id="search" type="search" placeholder="Filter table rows" '
                     'aria-label="Filter table rows">')
    parts.extend(sections)
    parts.append("<script>%s</script></body></html>\n" % SCRIPT)
    return "".join(parts).encode("utf-8")


def section(heading, *body):
    return "<section><h2>%s</h2>%s</section>" % (escape(heading), "".join(body))


def paragraph(text, css=None):
    return "<p%s>%s</p>" % (' class="%s"' % css if css else "", escape(text))


WIDTH = 640
HEIGHT = 150
MARGIN = 28


def plot(points, label):
    """An inline SVG of a sequence's raw points in planned order, warmups and failures
    included: each ok point at its value, a failed one as a ring on the axis and a missing one
    as a ring at the top. Coordinates come from validated decimals or rationals; nothing else
    is drawn."""
    values = [float(Fraction(point["value"])) for point in points if point["status"] == "ok"]
    low, high = (min(values), max(values)) if values else (0.0, 1.0)
    if high <= low:
        high = low + 1.0
    count = max(len(points) - 1, 1)
    parts = ['<svg role="img" width="%d" height="%d" viewBox="0 0 %d %d" '
             'xmlns="http://www.w3.org/2000/svg"><title>%s</title>'
             % (WIDTH, HEIGHT, WIDTH, HEIGHT, escape(label)),
             '<line class="axis" x1="%d" y1="%d" x2="%d" y2="%d"/>'
             % (MARGIN, HEIGHT - MARGIN, WIDTH - 4, HEIGHT - MARGIN),
             '<text class="label" x="2" y="12">%s</text>' % escape("%.6g ms" % high),
             '<text class="label" x="2" y="%d">%s</text>' % (HEIGHT - MARGIN,
                                                             escape("%.6g ms" % low))]
    for index, point in enumerate(points):
        x = MARGIN + (WIDTH - MARGIN - 8) * index / count
        side = "base" if point["side"] == "baseline" else "cand"
        warm = " warm" if point["phase"] in ("warmup",) else ""
        shown = "%s %s pair %d.%d %s: %s" % (point["phase"], point["side"], point["pair"],
                                             point["position"], point["status"],
                                             point["value"] or point["failure"] or "no record")
        if point["status"] == "ok":
            y = (HEIGHT - MARGIN) - (HEIGHT - MARGIN - 16) * (
                float(Fraction(point["value"])) - low) / (high - low)
            parts.append('<circle class="%s%s" cx="%.1f" cy="%.1f" r="2.2"><title>%s</title>'
                         '</circle>' % (side, warm, x, y, escape(shown)))
        else:
            css = "failed" if point["status"] == "failed" else "missing"
            y = HEIGHT - MARGIN if css == "failed" else 18
            parts.append('<circle class="%s" cx="%.1f" cy="%.1f" r="3"><title>%s</title>'
                         '</circle>' % (css, x, y, escape(shown)))
    parts.append("</svg>")
    return "".join(parts)


def markdown_cell(value):
    """A GitHub job-summary table cell: no markup, no table or line break survives."""
    text = "" if value is None else str(value)
    text = " ".join(text.split())
    for raw, entity in (("&", "&amp;"), ("<", "&lt;"), (">", "&gt;"), ("|", "&#124;"),
                        ("`", "&#96;"), ("[", "&#91;"), ("]", "&#93;"), ("*", "&#42;"),
                        ("_", "&#95;"), ("\\", "&#92;")):
        text = text.replace(raw, entity)
    return text
