"""Post-run checks of the p8 local workflow: exact reproduction and local provenance links.

Usage: checks.py reproduce ORIGINAL_RUN_JSON RERUN_JSON MAIN_REPORT_DIR REPRO_REPORT_DIR AGGREGATE_VERDICT OUT
       checks.py links REPORT_DIR [HISTORY_DIR] OUT
       checks.py exported EVIDENCE_DIR OUT

`links` checks the workflow's own report and history directories, before any export. `exported`
checks the evidence directory as it is retained: the same links of its `report/` and `history/`,
every bundle manifest's files, and each file an earlier export listed in `omitted.json`, which
must now be present with exactly the listed size and SHA-256.
"""
import hashlib
import html.parser
import json
import os
import sys


def sha256(path):
    with open(path, "rb") as stream:
        return "sha256:" + hashlib.sha256(stream.read()).hexdigest()


def load(path):
    with open(path, "rb") as stream:
        return json.loads(stream.read())


class Links(html.parser.HTMLParser):
    def __init__(self):
        super().__init__()
        self.links = []

    def handle_starttag(self, tag, attrs):
        for name, value in attrs:
            if (tag, name) in (("a", "href"), ("img", "src")):
                self.links.append((tag, value))


def page_links(page):
    parser = Links()
    with open(page, encoding="utf-8") as stream:
        parser.feed(stream.read())
    return parser.links


def check_page(page, root, downloads):
    """Every link of `page` is a relative path to a regular file below `root`, and a listed
    download has its recorded hash."""
    out = []
    base = os.path.dirname(page)
    for tag, value in page_links(page):
        entry = {"page": os.path.relpath(page, root), "tag": tag, "target": value}
        local = not (":" in value or value.startswith(("/", "#")))
        target = os.path.normpath(os.path.join(base, *value.split("/"))) if local else None
        entry["local_relative"] = local
        entry["inside_root"] = bool(local and os.path.commonpath(
            [os.path.realpath(target), os.path.realpath(root)]) == os.path.realpath(root))
        entry["regular_file"] = bool(local and os.path.isfile(target)
                                     and not os.path.islink(target))
        if entry["regular_file"]:
            entry["sha256"] = sha256(target)
            listed = downloads.get(os.path.relpath(target, root))
            if listed is not None:
                entry["listed_sha256_matches"] = listed == entry["sha256"]
        out.append(entry)
    return out


def link_result(report_dir, history_dir):
    report = load(os.path.join(report_dir, "report.json"))
    downloads = dict((item["path"], item["sha256"]) for item in report["downloads"])
    checked = check_page(os.path.join(report_dir, "report.html"), report_dir, downloads)
    for child in report["diagnostics"]:
        page = os.path.join(report_dir, *child["page"].split("/"))
        checked += check_page(page, report_dir, {})
    if history_dir:
        history_page = os.path.join(history_dir, "history.html")
        checked += [dict(item, scope="history") for item in
                    check_page(history_page, history_dir, {})]
        runs = os.path.join(history_dir, "runs")
        for run in sorted(os.listdir(runs)):
            page = os.path.join(runs, run, "report.html")
            if os.path.isfile(page):
                checked += [dict(item, scope="history") for item in
                            check_page(page, history_dir, {})]
    failures = [item for item in checked if not (item["local_relative"] and item["inside_root"]
                                                  and item["regular_file"]
                                                  and item.get("listed_sha256_matches", True))]
    return {"links_checked": len(checked), "failures": failures,
            "pages": sorted(set(item["page"] for item in checked)),
            "downloads_listed": len(downloads),
            "downloads_linked_and_matching": sum(1 for item in checked
                                                 if item.get("listed_sha256_matches"))}


def write(result, output):
    with open(output, "w") as stream:
        json.dump(result, stream, indent=2, sort_keys=True)
        stream.write("\n")


def links(report_dir, history_dir, output):
    result = link_result(report_dir, history_dir)
    write(result, output)
    return 0 if not result["failures"] else 1


def exported(evidence, output):
    """The retained directory itself: its pages' links, its bundle manifests and the files an
    earlier export omitted."""
    result = link_result(os.path.join(evidence, "report"), os.path.join(evidence, "history"))
    result["scope"] = "the exported evidence directory, after export"
    manifests = []
    for base, dirs, names in os.walk(evidence):
        dirs.sort()
        if "manifest.json" not in names:
            continue
        manifest = load(os.path.join(base, "manifest.json"))
        if manifest.get("schema") != "af.perf-manifest/1":
            continue
        for entry in manifest["entries"]:
            path = os.path.join(base, *entry["path"].split("/"))
            present = os.path.isfile(path) and not os.path.islink(path)
            manifests.append({
                "manifest": os.path.relpath(os.path.join(base, "manifest.json"),
                                            evidence).replace(os.sep, "/"),
                "path": entry["path"],
                "matches": bool(present and os.path.getsize(path) == entry["size"]
                                and sha256(path) == entry["sha256"])})
    result["manifest_entries_checked"] = len(manifests)
    result["manifest_failures"] = [item for item in manifests if not item["matches"]]
    restored = []
    listing = os.path.join(evidence, "omitted.json")
    for entry in load(listing) if os.path.isfile(listing) else []:
        path = os.path.join(evidence, *entry["path"].split("/"))
        present = os.path.isfile(path) and not os.path.islink(path)
        restored.append({"path": entry["path"], "sha256": entry["sha256"], "present": present,
                         "matches": bool(present and os.path.getsize(path) == entry["bytes"]
                                         and sha256(path) == entry["sha256"])})
    result["previously_omitted"] = restored
    write(result, output)
    ok = (not result["failures"] and not result["manifest_failures"]
          and all(item["matches"] for item in restored))
    return 0 if ok else 1


def reproduce(original, rerun, main_report, repro_report, aggregate_verdict, output):
    first, second = load(original), load(rerun)
    differing = sorted(key for key in set(first) | set(second) if first.get(key) != second.get(key))
    allowed = {"run_id", "parent_run_id", "plan_digest"}
    verdicts = {
        "aggregate": sha256(aggregate_verdict),
        "report": sha256(os.path.join(main_report, "verdict.json")),
        "reproduced_report": sha256(os.path.join(repro_report, "verdict.json")),
    }
    a, b = load(os.path.join(main_report, "report.json")), load(os.path.join(repro_report,
                                                                             "report.json"))

    def without_diagnostics(doc):
        doc = json.loads(json.dumps(doc))
        doc["diagnostics"] = []
        doc["downloads"] = [item for item in doc["downloads"]
                            if not item["path"].startswith("diagnostics/")]
        for case in doc["cases"]:
            case.pop("profile", None)
            case.pop("trace", None)
        return doc
    result = {
        "rerun_plan_fields_differing": differing,
        "rerun_plan_differs_only_in_identity": set(differing) <= allowed,
        "rerun_parent_run_id": second.get("parent_run_id"),
        "original_run_id": first.get("run_id"),
        "verdict_sha256": verdicts,
        "verdicts_identical": len(set(verdicts.values())) == 1,
        "report_documents_identical_apart_from_linked_diagnostics":
            without_diagnostics(a) == without_diagnostics(b),
        "summary_identical_before_its_diagnostics_section":
            open(os.path.join(main_report, "summary.md"), "rb").read().split(
                b"#### Diagnostics")[0]
            == open(os.path.join(repro_report, "summary.md"), "rb").read().split(
                b"#### Diagnostics")[0],
    }
    with open(output, "w") as stream:
        json.dump(result, stream, indent=2, sort_keys=True)
        stream.write("\n")
    ok = (result["rerun_plan_differs_only_in_identity"] and result["verdicts_identical"]
          and result["report_documents_identical_apart_from_linked_diagnostics"])
    return 0 if ok else 1


if __name__ == "__main__":
    if sys.argv[1] == "links":
        args = sys.argv[2:]
        sys.exit(links(args[0], args[1] if len(args) == 3 else None, args[-1]))
    if sys.argv[1] == "exported":
        sys.exit(exported(sys.argv[2], sys.argv[3]))
    sys.exit(reproduce(*sys.argv[2:8]))
