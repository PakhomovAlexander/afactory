"""Telemetry captures of a diagnostic child run's CLI cases (ADR-0199).

`cli.py profile --telemetry spans` (or `content`) adds, beside the child's CPU profiles or phase
traces, one telemetry-recorded execution of each eligible selected CLI case per side: the
side's own diagnostic AF binary runs the case's command at that explicitly selected
`[telemetry] level`, in the case's private HOME, XDG directories, TMPDIR and Store after its
declared setup, and its whole output is judged by the command's oracle. Then, in the same
private namespace and before it is removed, the same binary's own readers report what it
recorded: `af telemetry runs --json`, `show RUN --store ID --json`, `status --json` and
`config RUN --store ID --json`. Each document is a new evidence file bounded while it is
written, admitted as a confined regular file, and read only through the schema the product
commits for it (`perflab.jsonsubset` over `schemas/telemetry-*.json`); the laboratory never
opens a segment, a blob or the index itself.

From those four documents alone `capture` derives what the child records, so the report can
derive it again from the archived documents and refuse a capture that differs: the run and
its executions bound to the case, side, source and binary; the per-span-name phase rows (the
union of each name's ended intervals, never a sum across overlapping spans; the self time the
product computed, summed per name); a bounded timeline; and an evidence state that is
`complete` only when every execution was sealed by its writer with nothing dropped, no span or
execution is open, nothing is unreadable, no problem was recorded and the configuration record
is complete. Anything else is `partial` with every reason named, never silently complete. A
document that is missing, over its bound, of another version or refused by its schema is an
omission with its reason; its bytes stay counted in the evidence and are never shown.

Telemetry never contributes a verdict sample: these are separate diagnostic executions, and
`content` is labelled perturbing. Which cases record telemetry is the supported matrix
(`eligibility`): native library cases do not, because the probe installs no telemetry
subscriber; discovery, startup and state-read commands do not, by design. Their phase
coverage stays the perf-trace recipe's.
"""
import json

from . import diagcontracts, jsonsubset, phasetrace, render
from .values import ContractError, require

LEVELS = ("spans", "content")
# The CLI cases whose command runs a kernel run, and so records one (docs/telemetry.md "What
# this release records"): `af task run --execute`. Every other CLI case of the suite is a
# discovery command, a Task start without execution or a state read.
RECORDING_CASES = ("cli_task_run_check",)
DOCUMENTS = (
    ("runs", "af/telemetry-runs@1", "telemetry-runs-v1.json"),
    ("show", "af/telemetry-show@1", "telemetry-show-v1.json"),
    ("status", "af/telemetry-status@1", "telemetry-status-v1.json"),
    ("config", "af/TelemetryConfig@2", "telemetry-config-v2.json"),
)
DOCUMENT_KINDS = tuple(kind for kind, _, _ in DOCUMENTS)
DOCUMENT_BYTES_MAX = 4 * 1024 * 1024
READ_SECONDS = 60
SPANS_MAX = 65536
EXECUTIONS_MAX = 16
PHASES_MAX = 128
TIMELINE_SPANS_MAX = 512
LABEL_MAX = 128
PERFETTO = "perfetto.json"
# Why a capture is partial: each a fact one of the four documents states.
PARTIAL_REASONS = diagcontracts.TELEMETRY_PARTIAL_REASONS


def eligibility(case):
    """(reason, detail) when a selected case records no telemetry, or None when it does: the
    supported matrix, stated for every case instead of a silent gap."""
    if case["kind"] == "library":
        return ("no_telemetry_subscriber", "%s is a native library case: the perf-probe installs "
                "no telemetry subscriber, so it records no telemetry; its phases are the "
                "perf-trace recipe's spans" % case["instance_id"])
    if case["case_id"] not in RECORDING_CASES:
        return ("records_no_telemetry", "%s's command runs no kernel run, and discovery, Task "
                "start without execution and state reads deliberately record no telemetry and "
                "open no telemetry directory (docs/telemetry.md); its startup or command region "
                "is the perf-trace recipe's" % case["instance_id"])
    return None


def read_argv(binary, kind, run=None):
    """The side's own reader of `kind`, run in the recording's private namespace."""
    if kind in ("runs", "status"):
        return [binary, "telemetry", kind, "--json"]
    return [binary, "telemetry", kind, run["run_id"], "--store", run["store_id"], "--json"]


def read(kind, data, where):
    """One admitted document through the product's committed schema, then bounded."""
    name = dict((item[0], item) for item in DOCUMENTS)[kind]
    doc = jsonsubset.parse(data, where, DOCUMENT_BYTES_MAX)
    require(isinstance(doc, dict) and doc.get("schema") == name[1],
            "%s: expected %s, found %s" % (where, name[1],
                                           json.dumps(doc.get("schema") if isinstance(doc, dict)
                                                      else None)[:64]))
    jsonsubset.load(name[2]).validate(doc, where)
    if kind == "show":
        require(len(doc["spans"]) <= SPANS_MAX, "%s: more than %d spans" % (where, SPANS_MAX))
    return doc


def select_run(runs, expected_task):
    """The one run the recorded command worked on: exactly one run, a Task run of the case's
    Task when the command names one."""
    require(len(runs["runs"]) == 1, "the recording holds %d runs, not exactly the one its "
            "command ran" % len(runs["runs"]))
    run = runs["runs"][0]
    require(run.get("kind") == "task", "the recorded run is a %s run, not a Task run"
            % json.dumps(run.get("kind"))[:32])
    if expected_task is not None:
        require(run.get("task_id") == expected_task, "the recorded run is of Task %s, not the "
                "case's %s" % (json.dumps(run.get("task_id"))[:96], expected_task))
    return run


def _label(value, what):
    require(isinstance(value, str) and 0 < len(value) <= LABEL_MAX,
            "%s: a label of 1 to %d characters" % (what, LABEL_MAX))
    return value


def _optional_label(value):
    if not isinstance(value, str) or not value:
        return None
    return value[:LABEL_MAX]


def _config_row(config, key):
    for row in config["rows"]:
        if row.get("key") == key:
            return _optional_label(row.get("value"))
    return None


def _nested(spans):
    """{span_id: (lane, level)}: per segment, a span inside the innermost enclosing ended span
    is nested at that depth, and one that overlaps without nesting goes to an async lane, so
    the Perfetto export is always valid. Open spans (no recorded end) are instants."""
    placed = {}
    by_segment = {}
    for span in spans:
        if span.get("total_us") is None:
            placed[span["span_id"]] = ("instant", 0)
            continue
        by_segment.setdefault(span["segment_id"], []).append(span)
    for members in by_segment.values():
        members.sort(key=lambda span: (span["start_offset_us"], -span["total_us"],
                                       span["span_id"]))
        stack = []
        for span in members:
            end = span["start_offset_us"] + span["total_us"]
            while stack and stack[-1][1] <= span["start_offset_us"]:
                stack.pop()
            if stack and end > stack[-1][1]:
                placed[span["span_id"]] = ("async", 0)
                continue
            placed[span["span_id"]] = ("nested", len(stack))
            stack.append((span["span_id"], end))
    return placed


def _phases(spans):
    rows = {}
    for span in spans:
        row = rows.setdefault(span["name"], {"intervals": [], "spans": 0, "open": 0,
                                              "self_us": 0, "longest_us": 0})
        row["spans"] += 1
        if span.get("total_us") is None:
            row["open"] += 1
            continue
        start = span["start_offset_us"]
        row["intervals"].append((start, start + span["total_us"]))
        row["self_us"] += span.get("self_us") or 0
        row["longest_us"] = max(row["longest_us"], span["total_us"])
    out = []
    for name in sorted(rows):
        row = rows[name]
        out.append({"name": name, "spans": row["spans"], "open": row["open"],
                    "covered_us": phasetrace.union_ns(row["intervals"]),
                    "self_us": row["self_us"], "longest_us": row["longest_us"]})
    return out


def _evidence(runs, runs_run, show, status, config):
    reasons = set()
    executions = show["run"]["executions"]
    open_spans = sum(1 for span in show["spans"] if span["status"] == "open")
    if open_spans:
        reasons.add("open_spans")
    if executions["open"] or any(item["open"] for item in executions["list"]) \
            or runs_run.get("open"):
        reasons.add("open_execution")
    if any(item["segment"] != "sealed" for item in executions["list"]):
        reasons.add("unsealed_execution")
    if not show["run"]["history"].startswith("prefix:") or show["run"]["missing_ordinals"]:
        reasons.add("history_partial")
    if show["unreadable"] or status["unreadable"] or runs["unreadable"]:
        reasons.add("unreadable_files")
    if show["index"]["state"] == "absent" or status["index"]["state"] == "absent":
        reasons.add("index_absent")
    segments = status["segments"]
    for state, reason in (("live", "live_segment"), ("unsealed", "unsealed_segment"),
                          ("corrupt", "corrupt_segment")):
        if segments[state]:
            reasons.add(reason)
    ids = set(item["execution_id"] for item in executions["list"])
    require(len(ids) == len(executions["list"]), "show.json repeats an execution")
    observed = {}
    counts = dict((state, 0) for state in ("live", "unsealed", "sealed", "corrupt"))
    for entry in status["bytes"]["entries"]:
        if entry["kind"] != "segment":
            continue
        require(entry["name"] not in observed, "status.json repeats a segment observation")
        observed[entry["name"]] = entry
        state = entry.get("state")
        if state in counts:
            counts[state] += 1
        else:
            reasons.add("history_partial")
    for state, count in counts.items():
        require(count <= segments[state], "status.json contradicts its %s segment count" % state)
        if count != segments[state]:
            reasons.add("history_partial")
    captures = []
    for execution in executions["list"]:
        entry = observed.get(execution["execution_id"])
        if entry is None:
            # No observation is evidence of unknown history, never a complete capture.
            reasons.add("history_partial")
            captures.append({"execution_id": execution["execution_id"], "state": None,
                             "exit": None, "capture": None})
            continue
        require(entry.get("state") in (None, execution["segment"]),
                "status.json and show.json disagree about an execution's segment state")
        if entry.get("state") is None:
            reasons.add("history_partial")
        capture = entry.get("capture")
        captures.append({"execution_id": entry["name"][:LABEL_MAX],
                         "state": entry.get("state"), "exit": entry.get("exit"),
                         "capture": None if capture is None else capture[:LABEL_MAX]})
        if entry.get("state") == "sealed" and (capture != "complete"
                                               or entry.get("exit") not in ("ok", "error", "signal")):
            reasons.add("dropped_or_recovered_capture")
    if runs_run.get("coverage") != "complete":
        reasons.add("dropped_or_recovered_capture")
    if status["problems"]:
        reasons.add("problems_recorded")
    if config["state"] != "complete":
        reasons.add("config_incomplete")
    require(reasons <= set(PARTIAL_REASONS), "an undeclared partial reason")
    ordered = sorted(reasons)
    return {"state": "partial" if ordered else "complete", "reasons": ordered,
            "index": show["index"]["state"], "history": show["run"]["history"][:64],
            "segments": dict((key, segments[key]) for key in ("live", "unsealed", "sealed",
                                                              "corrupt")),
            "segment_captures": sorted(captures, key=lambda item: item["execution_id"])
            [:EXECUTIONS_MAX],
            "problems": len(status["problems"]),
            "unreadable": (len(show["unreadable"]) + len(status["unreadable"])
                           + len(runs["unreadable"])),
            "open_spans": open_spans, "config": config["state"]}


def capture(documents, expected_task):
    """What a capture records, derived from the four admitted documents' bytes alone:
    {run, af, executions, evidence, spans, phases, timeline}. Refuses documents that disagree
    about the run, and documents too large to show."""
    docs = dict((kind, read(kind, documents[kind], kind + ".json")) for kind in DOCUMENT_KINDS)
    run = select_run(docs["runs"], expected_task)
    show, status, config = docs["show"], docs["status"], docs["config"]
    for name, doc in (("show", show["run"]), ("config", config)):
        require((doc["store_id"], doc["run_id"]) == (run["store_id"], run["run_id"]),
                "%s.json describes another run than runs.json" % name)
    require(show["run"]["trace_id"] == run["trace_id"], "show.json names another trace")
    listed = show["run"]["executions"]["list"]
    require(1 <= len(listed) <= EXECUTIONS_MAX,
            "the run has %d executions, not 1 to %d" % (len(listed), EXECUTIONS_MAX))
    ids = [item["execution_id"] for item in listed]
    require(config["execution_id"] in ids, "config.json describes an execution show.json does "
            "not list")
    spans = show["spans"]
    seen = set()
    for span in spans:
        require(span["span_id"] not in seen, "show.json repeats span %s" % span["span_id"])
        seen.add(span["span_id"])
        _label(span["name"], "show.json span name")
        require(span["segment_id"] in ids, "show.json: span %s lies in no execution of the run"
                % span["span_id"])
        require((span["status"] == "open") == ("total_us" not in span),
                "show.json: span %s has a total exactly when it ended" % span["span_id"])
        require(("total_us" in span) == ("self_us" in span)
                and span.get("self_us", 0) <= span.get("total_us", 0),
                "show.json: span %s's self time is within its total" % span["span_id"])
    for span in spans:
        parent = span.get("parent_span_id")
        require(parent is None or parent in seen, "show.json: span %s names a parent the tree "
                "does not hold" % span["span_id"])
    phases = _phases(spans)
    require(len(phases) <= PHASES_MAX, "show.json: more than %d span names" % PHASES_MAX)
    placed = _nested(spans)
    timeline = [{"span_id": span["span_id"], "parent": span.get("parent_span_id"),
                 "name": span["name"], "depth": min(span["depth"], 64),
                 "start_us": span["start_offset_us"], "total_us": span.get("total_us"),
                 "self_us": span.get("self_us"), "status": span["status"],
                 "lane": placed[span["span_id"]][0], "level": min(placed[span["span_id"]][1],
                                                                  64)}
                for span in spans[:TIMELINE_SPANS_MAX]]
    executions = [{"execution_id": _label(item["execution_id"], "execution id"),
                   "ordinal": item["ordinal"], "segment": item["segment"],
                   "open": item["open"], "wall_us": item["wall_us"],
                   "outcome": _optional_label(item.get("outcome"))} for item in listed]
    return {
        "run": {"store_id": run["store_id"], "run_id": _label(run["run_id"], "run id"),
                "trace_id": run["trace_id"], "kind": "task",
                "task_id": _optional_label(run.get("task_id"))},
        "af": {"version": _config_row(config, "version.version"),
               "commit": _config_row(config, "version.commit"),
               "target": _config_row(config, "version.target")},
        "executions": executions,
        "evidence": _evidence(docs["runs"], run, show, status, config),
        "spans": len(spans), "phases": phases,
        "timeline": {"spans": timeline, "omitted": len(spans) - len(timeline)},
    }


def perfetto(show_data, label):
    """Chrome trace-event JSON of an admitted show document that Perfetto opens offline: a
    process per execution, complete events on its nested track, begin/end pairs on an explicit
    async track for what does not nest, and an instant for an open span. Only identities,
    names, statuses and times; no attribute of the product's is copied."""
    show = read("show", show_data, "show.json")
    ids = [item["execution_id"] for item in show["run"]["executions"]["list"]]
    placed = _nested(show["spans"])
    events = []
    for index, execution in enumerate(ids, 1):
        events.append('{"name":"process_name","ph":"M","pid":%d,"tid":0,"args":{"name":%s}}'
                      % (index, json.dumps("%s execution %s" % (label, execution))))
    for span in show["spans"]:
        pid = ids.index(span["segment_id"]) + 1
        lane, _ = placed[span["span_id"]]
        args = json.dumps({"span_id": span["span_id"], "parent": span.get("parent_span_id"),
                           "status": span["status"], "self_us": span.get("self_us")},
                          sort_keys=True, separators=(",", ":"))
        name = json.dumps(span["name"])
        if lane == "nested":
            events.append('{"name":%s,"cat":"af","ph":"X","ts":%d,"dur":%d,"pid":%d,"tid":1,'
                          '"args":%s}' % (name, span["start_offset_us"], span["total_us"], pid,
                                          args))
        elif lane == "async":
            for phase, at in (("b", span["start_offset_us"]),
                              ("e", span["start_offset_us"] + span["total_us"])):
                events.append('{"name":%s,"cat":"af.async","ph":"%s","id":"0x%s","ts":%d,'
                              '"pid":%d,"tid":1,"args":%s}'
                              % (name, phase, span["span_id"], at, pid, args))
        else:
            events.append('{"name":%s,"cat":"af.open","ph":"i","s":"t","ts":%d,"pid":%d,'
                          '"tid":1,"args":%s}' % (name, span["start_offset_us"], pid, args))
    metadata = json.dumps({"schema": "af/telemetry-show@1", "run_id": show["run"]["run_id"],
                           "first_start_unix_ns": show["run"]["first_start_unix_ns"]},
                          sort_keys=True, separators=(",", ":"))
    return ('{"displayTimeUnit":"ms","metadata":%s,"traceEvents":[\n%s\n]}\n'
            % (metadata, ",\n".join(events))).encode("utf-8")


# The page ----------------------------------------------------------------------------------

def _ms(micros):
    return "%d.%03d" % (micros // 1000, micros % 1000)


def timeline_svg(timeline, width=960, lane=14):
    """A capture's bounded timeline as SVG: nested spans by depth, async spans and open spans
    on rows of their own below. Every name is escaped into a title."""
    spans = timeline["spans"]
    if not spans:
        return render.Markup('<p class="unavailable">no span was recorded</p>')
    ended = [span for span in spans if span["total_us"] is not None]
    end = max([span["start_us"] + span["total_us"] for span in ended]
              + [span["start_us"] for span in spans])
    start = min(span["start_us"] for span in spans)
    scale = float(width - 20) / max(end - start, 1)
    nested = max([span["level"] for span in spans if span["lane"] == "nested"] + [0]) + 1
    rows = {"nested": 0, "async": nested, "instant": nested + 1}
    height = (nested + 2) * lane + 8
    parts = ['<svg xmlns="http://www.w3.org/2000/svg" width="%d" height="%d" role="img">'
             % (width, height)]
    for span in spans:
        row = rows[span["lane"]] + (span["level"] if span["lane"] == "nested" else 0)
        x = 10 + (span["start_us"] - start) * scale
        if span["total_us"] is None:
            parts.append('<rect class="missing" x="%.2f" y="%d" width="2" height="%d"><title>%s'
                         '</title></rect>' % (x, 4 + row * lane, lane - 2, render.escape(
                             "%s %s open at +%s ms" % (span["name"], span["span_id"],
                                                       _ms(span["start_us"])))))
            continue
        w = max(span["total_us"] * scale, 0.5)
        parts.append('<rect class="%s" x="%.2f" y="%d" width="%.2f" height="%d"><title>%s'
                     '</title></rect>' % (
                         "cand" if span["lane"] == "async" else "base", x, 4 + row * lane, w,
                         lane - 2, render.escape("%s %s (%s) at +%s ms: total %s ms, self %s ms"
                                                 % (span["name"], span["span_id"], span["status"],
                                                    _ms(span["start_us"]),
                                                    _ms(span["total_us"]),
                                                    _ms(span["self_us"] or 0)))))
    parts.append("</svg>")
    note = ""
    if timeline["omitted"]:
        note = ('<p class="note">%d further spans are not drawn; every span is in show.json and '
                'its Perfetto export.</p>' % timeline["omitted"])
    return render.Markup("".join(parts) + note)


def section(telemetry):
    """The child page's telemetry section, drawn from profiles.json alone."""
    if telemetry["level"] is None:
        return render.section("Telemetry", render.paragraph(
            "No telemetry was selected for this child run (profile --telemetry spans|content).",
            "unavailable"))
    intro = render.paragraph(
        "Each capture is a separate telemetry-recorded execution of the case's command by the "
        "side's diagnostic AF binary at level %s, read back through that binary's own af "
        "telemetry runs, show, status and config documents. Telemetry never contributes a "
        "verdict sample%s. Phase rows give each span name's covered time (the union of its "
        "spans' intervals) and the self time the product computed, summed over the name's "
        "spans, so concurrent spans each count." % (
            telemetry["level"], "; content is perturbing and diagnostic only"
            if telemetry["perturbing"] else ""), "note")
    rows = []
    details = []
    for item in telemetry["captures"]:
        evidence = item["evidence"]
        state = ("omitted: %s" % item["omission"]["reason"] if item["omission"] is not None
                 else evidence["state"] + ("" if not evidence["reasons"]
                                           else " (" + ", ".join(evidence["reasons"]) + ")"))
        rows.append((item["instance_id"], item["side"], item["level"], state,
                     item["command_wall_ms"] or "",
                     item["run"]["run_id"] if item["run"] else "",
                     len(item["executions"]), item["spans"],
                     (item["af"] or {}).get("version") or "", item["binary_sha256"]))
        if item["omission"] is not None:
            details.append("<h3>%s %s</h3>%s" % (
                render.escape(item["instance_id"]), render.escape(item["side"]),
                render.paragraph("omitted: %s: %s" % (item["omission"]["reason"],
                                                      item["omission"]["detail"]),
                                 "unavailable")))
            continue
        details.append("<h3>%s %s</h3>%s%s%s%s" % (
            render.escape(item["instance_id"]), render.escape(item["side"]),
            render.paragraph("run %s, trace %s, store %s; index %s, history %s; segments %d "
                             "sealed, %d unsealed, %d live, %d corrupt; %d problems, %d "
                             "unreadable, %d open spans; configuration record %s" % (
                                 item["run"]["run_id"], item["run"]["trace_id"],
                                 item["run"]["store_id"], evidence["index"],
                                 evidence["history"], evidence["segments"]["sealed"],
                                 evidence["segments"]["unsealed"],
                                 evidence["segments"]["live"],
                                 evidence["segments"]["corrupt"], evidence["problems"],
                                 evidence["unreadable"], evidence["open_spans"],
                                 evidence["config"])),
            render.table(("execution", "ordinal", "segment", "open", "wall ms", "outcome",
                          "capture"),
                         [(execution["execution_id"], execution["ordinal"] or "",
                           execution["segment"], execution["open"],
                           _ms(execution["wall_us"]), execution["outcome"] or "",
                           "; ".join("%s, exit %s, capture %s" % (
                               capture["state"] or "unknown", capture["exit"] or "unknown",
                               capture["capture"] or "unknown")
                               for capture in evidence["segment_captures"]
                               if capture["execution_id"] == execution["execution_id"])
                           or "not listed by status")
                          for execution in item["executions"]]),
            render.table(("span name", "spans", "open", "covered ms (union)", "self ms (sum)",
                          "longest ms"),
                         [(row["name"], row["spans"], row["open"], _ms(row["covered_us"]),
                           _ms(row["self_us"]), _ms(row["longest_us"]))
                          for row in item["phases"]], searchable=True),
            timeline_svg(item["timeline"]).text))
        details.append(render.paragraph("archived documents (not linked): " + "; ".join(
            "%s %s" % (artifact["path"], artifact["sha256"]) for artifact in item["artifacts"])))
    table = render.table(("case", "side", "level", "evidence", "command ms", "run", "executions",
                          "spans", "af", "binary"), rows, searchable=True)
    return render.section("Telemetry", intro, table, "".join(details) or render.paragraph(
        "No telemetry was captured; see the omissions.", "unavailable"))


def check_capture(entry, derived):
    """Refuse a recorded capture whose derived fields are not what the documents give."""
    for name in ("run", "af", "executions", "evidence", "spans", "phases", "timeline"):
        if entry[name] != derived[name]:
            raise ContractError("its profiles claim another %s than its telemetry documents "
                                "give" % name)
