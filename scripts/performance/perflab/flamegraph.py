"""Collapsed stacks and flamegraphs through the pinned inferno tools (package p5, ADR-0174, ADR-0175).

The laboratory lays out no flamegraph itself. inferno 0.12.8 collapses `perf script` output,
computes normalized differentials and renders deterministic SVG:

- **One side**: `inferno-flamegraph --deterministic --no-javascript`, colored by the tool's
  deterministic palette, widths in samples.
- **Both directions of a difference**: `inferno-diff-folded --normalize BASE CAND` shows the
  candidate's stacks colored by the change in their share of CPU samples; the reverse,
  `--normalize CAND BASE` rendered with `--negate`, shows the baseline's stacks with the same
  color meaning, so a stack that disappeared in the candidate stays visible. Color is a change in
  sample share, never an absolute time saved.

Each tool runs under the authority supervisor (`toolrun`) and writes its output as a new evidence
file bounded by the run's byte room; that file is admitted as a confined regular file before it
is read, and refused when malformed (ADR-0175). Every rendered SVG is kept as the tool wrote it
and sanitized (`svgsafe`) for embedding; both are hashed, and the two identities stay distinct.
Unknown frames are kept as inferno writes them; nothing is filtered.
"""
import re

from . import svgsafe
from .values import ContractError, sha256_bytes

TOOLS = ("inferno_collapse_perf", "inferno_diff_folded", "inferno_flamegraph")
_FOLDED = re.compile(r"^(.+) ([0-9]+)$")
_DIFF = re.compile(r"^(.+) ([0-9]+) ([0-9]+)$")


class RenderError(Exception):
    def __init__(self, reason, detail):
        Exception.__init__(self, detail)
        self.reason = reason
        self.detail = detail


def check_folded(data, differential=False):
    """Validate collapsed stacks, `STACK COUNT` (or `STACK BEFORE AFTER`) per line; returns the
    total samples (after, for a difference)."""
    try:
        text = data.decode("utf-8")
    except UnicodeDecodeError:
        raise RenderError("malformed_folded", "collapsed stacks are not UTF-8")
    total = 0
    pattern = _DIFF if differential else _FOLDED
    for number, line in enumerate(text.splitlines(), 1):
        if not line:
            continue
        match = pattern.match(line)
        if match is None or not match.group(1).strip():
            raise RenderError("malformed_folded", "collapsed line %d is not a stack and its "
                              "count%s: %r" % (number, "s" if differential else "", line[:200]))
        total += int(match.group(match.lastindex))
    return total


def merge_folded(parts):
    """Sum collapsed stacks of several repetitions, sorted by stack: deterministic."""
    counts = {}
    for data in parts:
        check_folded(data)
        for line in data.decode("utf-8").splitlines():
            if line:
                stack, count = line.rsplit(" ", 1)
                counts[stack] = counts.get(stack, 0) + int(count)
    return "".join("%s %d\n" % (stack, counts[stack]) for stack in sorted(counts)).encode("utf-8")


def render_case(renderer, relative, instance_id, folded):
    """Render one case's flamegraphs into the evidence directory `relative`: each side present
    in `folded` ({side: relative path of its admitted collapsed stacks}) and, with both sides,
    the difference in both directions. Every tool output is a new evidence file bounded by the
    room left and admitted before it is read (`renderer`, ADR-0175). Returns the render
    records, each with its original and sanitized SVG."""
    renders = []
    data = {}
    paths = {}
    for side in ("baseline", "candidate"):
        if side not in folded:
            continue
        data[side] = renderer.read(folded[side])
        check_folded(data[side])
        paths[side] = relative + "/%s.folded" % side
        renderer.write(paths[side], data[side])
    jobs = []
    for side in ("baseline", "candidate"):
        if side in data:
            jobs.append((side, paths[side], [sha256_bytes(data[side])],
                         "%s %s: CPU samples" % (instance_id, side), False))
    if len(paths) == 2:
        inputs = [sha256_bytes(data["baseline"]), sha256_bytes(data["candidate"])]
        for kind, before, after in (("difference_forward", "baseline", "candidate"),
                                    ("difference_reverse", "candidate", "baseline")):
            target = relative + "/%s.folded" % kind
            diff = renderer.run("inferno_diff_folded", [
                "--normalize", renderer.relative_to(paths[before], target),
                renderer.relative_to(paths[after], target)], target)
            check_folded(diff, differential=True)
        jobs.append(("difference_forward", relative + "/difference_forward.folded", inputs,
                     "%s: candidate stacks, color = change in CPU sample share" % instance_id,
                     False))
        jobs.append(("difference_reverse", relative + "/difference_reverse.folded", inputs[::-1],
                     "%s: baseline stacks, including those gone from the candidate; color = "
                     "change in CPU sample share" % instance_id, True))
    for kind, source, inputs, title, negate in jobs:
        target = relative + "/" + kind + ".original.svg"
        argv = ["--deterministic", "--no-javascript", "--countname", "samples",
                "--title", title]
        if negate:
            argv.append("--negate")
        original = renderer.run("inferno_flamegraph",
                                argv + [renderer.relative_to(source, target)], target)
        if not original.lstrip().startswith(b"<?xml") and not original.lstrip().startswith(
                b"<svg"):
            raise RenderError("malformed_svg", "inferno-flamegraph wrote no SVG")
        try:
            sanitized = svgsafe.sanitize(original)
        except ContractError as error:
            raise RenderError("unsafe_svg", str(error))
        sanitized_path = relative + "/" + kind + ".svg"
        sanitized_record = renderer.write(sanitized_path, sanitized.data)
        renders.append({
            "instance_id": instance_id, "kind": kind,
            "color": "sample_share_change" if kind.startswith("difference") else "sample_count",
            "inputs": inputs,
            "original": {"bytes": len(original), "sha256": sha256_bytes(original),
                         "path": target},
            "sanitized": dict(sanitized_record, path=sanitized_path),
            "removed": {"elements": sanitized.removed_elements,
                        "attributes": sanitized.removed_attributes}})
    return renders
