"""Materialize the original run's history copy from the retained bundles, verified, into the
evidence directory.

Usage: restore_history.py REPO EVIDENCE_DIR WORK

The first export listed the ten files of `history/runs/p8_local_aa/` in `omitted.json` instead
of copying them, so the retained `history/history.html` linked to a page it did not hold. This
imports the two retained run bundles, `report/evidence/` and
`history/runs/p8_local_aa_rerun/evidence/`, into a new archive below the new directory WORK with
REPO's `cli.py`, draws the history, and requires every retained history file to be regenerated
byte for byte and every listed file to have its listed size and SHA-256. Only then does it copy
the ten files; nothing retained is rewritten, and it refuses a differing file already in place.
"""
import hashlib
import json
import os
import shutil
import subprocess
import sys

ORIGINAL = "p8_local_aa"
RERUN = "p8_local_aa_rerun"


def sha256(path):
    with open(path, "rb") as stream:
        return "sha256:" + hashlib.sha256(stream.read()).hexdigest()


def files(root):
    out = []
    for base, dirs, names in os.walk(root):
        dirs.sort()
        out += [os.path.relpath(os.path.join(base, name), root).replace(os.sep, "/")
                for name in sorted(names)]
    return out


def main(repo, evidence, work):
    os.makedirs(work)
    env = dict(os.environ, HOME=os.path.join(work, "home"), TMPDIR=os.path.join(work, "tmp"),
               PYTHONDONTWRITEBYTECODE="1")
    os.makedirs(env["HOME"])
    os.makedirs(env["TMPDIR"])
    shutil.copytree(os.path.join(evidence, "report", "evidence"), os.path.join(work, "run"))
    shutil.copytree(os.path.join(evidence, "history", "runs", RERUN, "evidence"),
                    os.path.join(work, "rerun"))
    archive, history = os.path.join(work, "archive"), os.path.join(work, "history")
    lab = [sys.executable, "-B", os.path.join(repo, "scripts", "performance", "cli.py")]
    for argv in (["archive", "--root", archive, "--init", "--capacity-bytes", "2147483648"],
                 ["archive", "--root", archive, "--import", os.path.join(work, "run")],
                 ["archive", "--root", archive, "--import", os.path.join(work, "rerun")],
                 ["history", "--inputs", archive, "--output", history]):
        subprocess.run(lab + argv, env=env, check=True)
    regenerated = files(history)
    original = "runs/%s/" % ORIGINAL
    retained = dict((path, sha256(os.path.join(history, *path.split("/"))))
                    for path in regenerated if not path.startswith(original))
    differing = sorted(path for path, digest in retained.items()
                       if not os.path.isfile(os.path.join(evidence, "history", *path.split("/")))
                       or sha256(os.path.join(evidence, "history", *path.split("/"))) != digest)
    if differing or sorted(retained) != sorted(
            path for path in files(os.path.join(evidence, "history"))
            if not path.startswith(original)):
        sys.exit("the regenerated history is not the retained one: %s" % differing)
    with open(os.path.join(evidence, "omitted.json")) as stream:
        listed = dict((entry["path"], entry) for entry in json.load(stream))
    produced = sorted("history/" + path for path in regenerated if path.startswith(original))
    if produced != sorted(listed):
        sys.exit("the regenerated original run is not the listed one")
    for path in produced:
        source = os.path.join(history, *path[len("history/"):].split("/"))
        entry = listed[path]
        if os.path.getsize(source) != entry["bytes"] or sha256(source) != entry["sha256"]:
            sys.exit("%s: not the listed size and SHA-256" % path)
        target = os.path.join(evidence, *path.split("/"))
        if os.path.exists(target) and sha256(target) != entry["sha256"]:
            sys.exit("%s: a different file is already in place" % path)
    for path in produced:
        target = os.path.join(evidence, *path.split("/"))
        os.makedirs(os.path.dirname(target), exist_ok=True)
        shutil.copyfile(os.path.join(history, *path[len("history/"):].split("/")), target)
    print(json.dumps({"restored": produced, "retained_history_regenerated": len(retained)},
                     indent=1, sort_keys=True))


if __name__ == "__main__":
    main(*sys.argv[1:4])
