"""Copy the reviewable public outputs of the p8 local workflow into the evidence directory.

Usage: export.py WORK DEST REAL_HOME TOOLS_DIR

Bundles, reports and history pages are copied byte for byte: their manifests and the report's
download list hash them. Every page the history links is kept, the original run's materialized
copy too, although its bundle duplicates `report/evidence/`: an export that left it out (the
first one did, listing it in `omitted.json`) published a history whose link did not resolve.
Step records and console output are this driver's own; in them every absolute path of the host
is replaced by `$WORK`, `$WORK/repo` or `$REAL_HOME`. After copying, `checks.py exported` checks
the exported directory itself and writes `checks/links-exported.json`; the export fails unless
every link resolves and every manifest matches. `checks/links.json` stays the workflow's own
check of its directories before export.
"""
import json
import os
import shutil
import sys

WORK, DEST, REAL_HOME, TOOLS = (os.path.realpath(sys.argv[1]), sys.argv[2],
                                os.path.realpath(sys.argv[3]), sys.argv[4])


def scrub(data):
    text = data.decode("utf-8")
    for real, placeholder in ((WORK + "/repo", "$WORK/repo"), (WORK, "$WORK"),
                              (REAL_HOME, "$REAL_HOME")):
        text = text.replace(real, placeholder)
    return text.encode("utf-8")


def copy(source, target, rewrite=False):
    target = os.path.join(DEST, target)
    os.makedirs(os.path.dirname(target), exist_ok=True)
    with open(source, "rb") as stream:
        data = stream.read()
    with open(target, "wb") as stream:
        stream.write(scrub(data) if rewrite else data)


def tree(source, target):
    for base, dirs, names in os.walk(source):
        dirs.sort()
        for name in sorted(names):
            path = os.path.join(base, name)
            copy(path, os.path.join(target, os.path.relpath(path, source)))


if os.path.exists(DEST):
    shutil.rmtree(DEST)
os.makedirs(DEST)
# The measured report: page, documents, the run bundle below evidence/ and the linked
# diagnostic child bundle below diagnostics/.
tree(os.path.join(WORK, "report"), "report")
# History of both runs, materialized from the archive, with every run page it links.
tree(os.path.join(WORK, "history-2"), "history")
# Step records and console output, paths replaced.
copy(os.path.join(WORK, "steps.jsonl"), "steps.jsonl", rewrite=True)
copy(os.path.join(WORK, "fetch-from-first-attempt.err"), "console/fetch.err", rewrite=True)
# The real receipts the native kernel integration exported on the earlier pre-sealing source.
receipts = sorted(os.listdir(os.path.join(os.path.dirname(WORK), "receipts")))
copy(os.path.join(os.path.dirname(WORK), "receipts", receipts[0]), "kernel-receipts.json")
for base, _, names in os.walk(os.path.join(WORK, "logs")):
    for name in sorted(names):
        path = os.path.join(base, name)
        if os.path.getsize(path):
            copy(path, os.path.join("console", os.path.relpath(path, os.path.join(WORK, "logs"))),
                 rewrite=True)
for name in ("reproduce.json", "links.json", "browser.json", "sources-digest-timing.json",
             "load-before-run.txt", "load-after-run.txt", "load-before-rerun.txt",
             "load-after-rerun.txt", "reproduce.argv", "native-kernel-test.txt"):
    copy(os.path.join(WORK, name), os.path.join("checks", name), rewrite=True)
for name in sorted(os.listdir(TOOLS)):
    copy(os.path.join(TOOLS, name), os.path.join("tools", name))
# The exported directory as it is retained, not the work directory it came from.
sys.dont_write_bytecode = True
sys.path.insert(0, TOOLS)
import checks  # noqa: E402

status = checks.exported(DEST, os.path.join(DEST, "checks", "links-exported.json"))
sizes = []
for base, _, names in os.walk(DEST):
    for name in names:
        sizes.append(os.path.getsize(os.path.join(base, name)))
print(json.dumps({"files": len(sizes), "bytes": sum(sizes), "exported_check": status}))
sys.exit(status)
