"""Phase traces of `perf-trace` diagnostic builds (package p5, ADR-0174).

A trace (`af.perf-trace/1`) is validated, then checked for nesting thread by thread: a span that
lies inside the innermost enclosing span of its thread is drawn on that thread's nested track,
and one that overlaps without nesting is exported on an explicit async track instead, so the
Perfetto-compatible export (Chrome trace-event JSON) is always valid. Phase totals are the union
of each phase's intervals, never a sum: overlapping or nested spans of a phase never count
twice, and phases are never added together into a process total. The offline timeline is part of
the diagnostic page, drawn as SVG by the laboratory's own renderer.
"""
import json

from . import contracts, render

TIMELINE_SPANS_MAX = 4000


def load(data, where="trace.json"):
    return contracts.read_json(data, "trace", where)


def tracks(doc):
    """{span id: ("nested", depth) or ("async", None)} by each thread's interval nesting."""
    placed = {}
    by_thread = {}
    for span in doc["spans"]:
        by_thread.setdefault(span["tid"], []).append(span)
    for spans in by_thread.values():
        spans.sort(key=lambda span: (span["start_ns"], -span["end_ns"], span["id"]))
        stack = []
        for span in spans:
            # Spans that ended by this one's start are siblings or ancestors' siblings.
            while stack and stack[-1]["end_ns"] <= span["start_ns"]:
                stack.pop()
            if stack and span["end_ns"] > stack[-1]["end_ns"]:
                placed[span["id"]] = ("async", None)
                continue
            placed[span["id"]] = ("nested", len(stack))
            stack.append(span)
    return placed


def _micros(ns):
    return "%d.%03d" % (ns // 1000, ns % 1000)


def perfetto(doc, label):
    """Chrome trace-event JSON that Perfetto opens: complete events on each thread's nested
    track and begin/end pairs on an explicit async track for anything that does not nest.
    Timestamps are exact microseconds with nanosecond decimals."""
    placed = tracks(doc)
    events = ['{"name":"process_name","ph":"M","pid":%d,"tid":0,"args":{"name":%s}}'
              % (doc["pid"], json.dumps(label))]
    for span in doc["spans"]:
        kind, _ = placed[span["id"]]
        args = '{"id":%d,"parent":%s}' % (span["id"], "null" if span["parent"] is None
                                          else str(span["parent"]))
        if kind == "nested":
            events.append('{"name":"%s","cat":"af","ph":"X","ts":%s,"dur":%s,"pid":%d,'
                          '"tid":%d,"args":%s}' % (span["phase"], _micros(span["start_ns"]),
                                                   _micros(span["end_ns"] - span["start_ns"]),
                                                   doc["pid"], span["tid"], args))
        else:
            for phase, at in (("b", span["start_ns"]), ("e", span["end_ns"])):
                events.append('{"name":"%s","cat":"af.async","ph":"%s","id":%d,"ts":%s,'
                              '"pid":%d,"tid":%d,"args":%s}'
                              % (span["phase"], phase, span["id"], _micros(at), doc["pid"],
                                 span["tid"], args))
    metadata = ('{"schema":"af.perf-trace/1","dropped":%d,"open":%d,"capacity":%d,'
                '"epoch_unix_ns":"%s"}' % (doc["dropped"], doc["open"], doc["capacity"],
                                           doc["epoch_unix_ns"]))
    return ('{"displayTimeUnit":"ns","metadata":%s,"traceEvents":[\n%s\n]}\n'
            % (metadata, ",\n".join(events))).encode("utf-8")


def union_ns(intervals):
    """The length of the union of `[start, end]` intervals: overlap counts once."""
    total = 0
    current = None
    for start, end in sorted(intervals):
        if current is None or start > current[1]:
            if current is not None:
                total += current[1] - current[0]
            current = [start, end]
        else:
            current[1] = max(current[1], end)
    if current is not None:
        total += current[1] - current[0]
    return total


def phase_summary(doc):
    """Per phase: spans, the union of their intervals and the longest one. No cross-phase sum."""
    phases = {}
    for span in doc["spans"]:
        phases.setdefault(span["phase"], []).append((span["start_ns"], span["end_ns"]))
    rows = []
    for phase in sorted(phases):
        intervals = phases[phase]
        rows.append({"phase": phase, "spans": len(intervals), "covered_ns": union_ns(intervals),
                     "longest_ns": max(end - start for start, end in intervals)})
    return rows


def async_count(doc):
    return sum(1 for kind, _ in tracks(doc).values() if kind == "async")


def timeline_svg(doc, width=960, lane=14):
    """One trace as an SVG timeline: a band per thread, nested spans stacked by depth, async
    spans in their own row. At most TIMELINE_SPANS_MAX spans are drawn, the longest first, and
    the page says how many were left out."""
    spans = doc["spans"]
    if not spans:
        return render.Markup('<p class="unavailable">no span was recorded</p>')
    placed = tracks(doc)
    drawn = sorted(spans, key=lambda span: (-(span["end_ns"] - span["start_ns"]), span["id"]))
    drawn = drawn[:TIMELINE_SPANS_MAX]
    start = min(span["start_ns"] for span in spans)
    end = max(span["end_ns"] for span in spans)
    scale = float(width - 120) / max(end - start, 1)
    threads = sorted(set(span["tid"] for span in spans))
    depth = {}
    for span in spans:
        kind, level = placed[span["id"]]
        row = level if kind == "nested" else -1
        depth[span["tid"]] = max(depth.get(span["tid"], 0), row + 2)
    offsets = {}
    y = 4
    for tid in threads:
        offsets[tid] = y
        y += (depth[tid] + 1) * lane
    parts = ['<svg xmlns="http://www.w3.org/2000/svg" width="%d" height="%d" role="img">'
             % (width, y + 4)]
    for tid in threads:
        parts.append('<text class="label" x="2" y="%d">thread %d</text>'
                     % (offsets[tid] + lane - 3, tid))
    for span in drawn:
        kind, level = placed[span["id"]]
        row = (depth[span["tid"]] - 1) if kind == "async" else level
        x = 116 + (span["start_ns"] - start) * scale
        w = max((span["end_ns"] - span["start_ns"]) * scale, 0.5)
        parts.append('<rect class="%s" x="%.2f" y="%d" width="%.2f" height="%d"><title>%s'
                     '</title></rect>'
                     % ("cand" if kind == "async" else "base", x,
                        offsets[span["tid"]] + row * lane, w, lane - 2,
                        render.escape("%s #%d (%s) %s ns" % (span["phase"], span["id"], kind,
                                                            span["end_ns"] - span["start_ns"]))))
    parts.append("</svg>")
    note = ""
    if len(drawn) < len(spans):
        note = ('<p class="note">%d of %d spans drawn, the longest; every span is in the trace '
                'and its Perfetto export.</p>' % (len(drawn), len(spans)))
    return render.Markup("".join(parts) + note)
