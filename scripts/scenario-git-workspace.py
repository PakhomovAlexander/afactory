"""Workspace lifecycle extension, embedded with the GitHub transport at installation.

No workflow command, environment or path is executed. The owner-side installer pins both
sources and the exact machine configuration in one isolated Python executable.
"""
import shlex
import subprocess
import shutil
import tempfile
import ctypes

WORKSPACE_MAX_RECORDS = 32
WORKSPACE_CONFIG = "af/git-workspace-config@2"
WORKSPACE_STATE = "af/git-workspace-state@2"
RESERVATION_SLOTS = 4
PREPARED = "af/git-workspace-prepared@1"
PERFORMANCE_OP = "publish_performance_pr_v1"
OPS = {"prepare_candidate_v1", "cleanup_workspace_v1", PERFORMANCE_OP}


def sync_directory(path):
    fd = os.open(path, os.O_RDONLY | os.O_DIRECTORY | os.O_NOFOLLOW)
    try:
        os.fsync(fd)
    finally:
        os.close(fd)


def publish_directory(source, destination):
    # Ordinary rename can replace a foreign empty directory. Require the platform's
    # exclusive atomic rename; unsupported systems fail closed, without a fallback.
    libc = ctypes.CDLL(None, use_errno=True)
    if sys.platform == "darwin":
        rename = libc.renamex_np
        rename.argtypes = [ctypes.c_char_p, ctypes.c_char_p, ctypes.c_uint]
        result = rename(os.fsencode(source), os.fsencode(destination), 4)  # RENAME_EXCL
    else:
        need(sys.platform.startswith("linux") and hasattr(libc, "renameat2"),
             "exclusive directory publication unavailable")
        rename = libc.renameat2
        rename.argtypes = [ctypes.c_int, ctypes.c_char_p, ctypes.c_int, ctypes.c_char_p, ctypes.c_uint]
        result = rename(-100, os.fsencode(source), -100, os.fsencode(destination), 1)  # RENAME_NOREPLACE
    if result != 0:
        raise OSError(ctypes.get_errno(), "exclusive directory publication refused")


def workspace_validate_config(c):
    fields(c, ("schema", "repository", "repository_id", "actor", "token_file", "state_dir",
               "repo", "task_store", "worktree_root", "base", "branch_prefix", "source_commit",
               "source_content_digest", "af", "af_digest", "git", "git_digest", "ssh_config",
               "ssh_config_digest", "author_name", "author_email", "required_checks", "reviewers", "publication_mode", "operations"), ("performance",))
    need(c["schema"] == WORKSPACE_CONFIG, "unsupported workspace machine configuration")
    string(c["repository"], REPO)
    integer(c["repository_id"])
    person(c["actor"])
    string(c["source_commit"], SHA)
    string(c["source_content_digest"], DIGEST)
    branch(c["base"])
    branch(c["branch_prefix"])
    need(len(c["branch_prefix"]) <= 134, "owned branch prefix exceeds GitHub payload bound")
    for key in ("author_name", "author_email"):
        string(c[key], limit=200)
        need("\n" not in c[key] and "\r" not in c[key], "invalid author")
    paths = ("repo", "task_store", "worktree_root", "state_dir", "token_file", "af", "git", "ssh_config")
    for key in paths:
        p = Path(string(c[key]))
        need(p.is_absolute() and p.resolve() == p, "machine paths must be canonical")
    for a, b in (("repo", "task_store"), ("repo", "worktree_root"), ("repo", "state_dir"),
                 ("state_dir", "worktree_root"), ("task_store", "worktree_root"), ("state_dir", "task_store")):
        p, q = Path(c[a]), Path(c[b])
        need(p != q and p not in q.parents and q not in p.parents, "machine roots overlap")
    for key in ("state_dir", "task_store", "worktree_root", "token_file", "ssh_config", "af", "git"):
        outside_checkout(Path(c[key]))
    for key in ("af_digest", "git_digest", "ssh_config_digest"):
        string(c[key], DIGEST)
    need(c["publication_mode"] == "exclusive_owned_writer", "explicit exclusive writer authority required")
    operations = c["operations"]
    need(isinstance(operations, list) and operations and len(set(operations)) == len(operations)
         and set(operations) <= OPS | set(PAYLOAD_SCHEMAS) | {"observe"}, "invalid machine workspace operations")
    performance = c.get("performance")
    need((PERFORMANCE_OP in operations) == (performance is not None), "standing publication requires narrow opt-in lab authority")
    if performance is not None:
        fields(performance, ("executable", "digest", "destination"))
        lab = Path(string(performance["executable"]))
        need(lab.is_absolute() and lab.resolve() == lab, "native evidence adapter must be canonical")
        outside_checkout(lab)
        string(performance["digest"], DIGEST)
        string(performance["destination"])
    # Reuse the established check/person schema and validators without its fixed branch map.
    validate_config({"schema": "af/github-adapter-config@1", "repository": c["repository"],
        "repository_id": c["repository_id"], "actor": c["actor"], "token_file": c["token_file"],
        "state_dir": c["state_dir"], "heads": {"workspace/config-validation": c["base"]},
        "required_checks": c["required_checks"], "reviewers": c["reviewers"], "operations": ["observe"]})
    return c


def file_digest(path):
    fd = os.open(path, os.O_RDONLY | os.O_NOFOLLOW | os.O_NONBLOCK)
    with os.fdopen(fd, "rb") as stream:
        meta = os.fstat(stream.fileno())
        need(stat.S_ISREG(meta.st_mode) and meta.st_size <= 256 * 1024 * 1024 and meta.st_mode & 0o022 == 0,
             "unsafe or oversized pinned executable")
        sha = hashlib.sha256()
        while True:
            data = stream.read(65536)
            if not data:
                break
            sha.update(data)
        return "sha256:" + sha.hexdigest()


def bounded_command(argv, cwd, env, data, timeout, directory, maximum=STATE_LIMIT):
    # Native commands are fixed argv, with no candidate command. Keep captures on disk and
    # read only their admitted bound; diagnostics never reflect subprocess/credential bytes.
    with tempfile.TemporaryFile(dir=directory) as output, tempfile.TemporaryFile(dir=directory) as error:
        result = subprocess.run(argv, cwd=cwd, env=env, input=data, stdout=output, stderr=error, timeout=timeout)
        need(result.returncode == 0 and output.tell() <= maximum and error.tell() <= STATE_LIMIT,
             "bounded native operation failed; retain receipts and reconcile")
        output.seek(0)
        return output.read(maximum + 1)


def repository_settings(c):
    env = {"PATH": "/usr/bin:/bin", "HOME": c["state_dir"] + "/home", "GIT_CONFIG_NOSYSTEM": "1",
           "GIT_CONFIG_GLOBAL": "/dev/null", "GIT_NO_REPLACE_OBJECTS": "1"}
    raw = bounded_command([c["git"], "config", "--local", "--no-includes", "--null", "--list"],
                          c["repo"], env, None, 5, c["state_dir"], LIMIT)
    safe = {"core.repositoryformatversion", "core.filemode", "core.bare", "core.logallrefupdates",
            "core.ignorecase", "core.precomposeunicode", "user.name", "user.email"}
    for entry in raw.split(b"\0"):
        if not entry:
            continue
        key = entry.split(b"\n", 1)[0].decode("ascii").lower()
        need(key in safe or (key.startswith("remote.") and key.rsplit(".", 1)[-1] in ("url", "fetch", "pushurl"))
             or (key.startswith("branch.") and key.rsplit(".", 1)[-1] in ("remote", "merge", "pushremote", "rebase")),
             "local Git configuration contains unpinned executable or URL-rewrite authority")


def workspace_load_config():
    need(CONFIG_PATH is not None and CONFIG_DIGEST is not None, "adapter is not installed")
    raw = read_private(Path(CONFIG_PATH), LIMIT)
    need(digest(raw) == CONFIG_DIGEST, "installed workspace configuration changed")
    c = workspace_validate_config(decode(raw))
    for key in ("af", "git"):
        path = Path(c[key])
        need(path.is_file() and file_digest(path) == c[key + "_digest"], "pinned tool changed")
    need(digest(read_private(Path(c["ssh_config"]), LIMIT)) == c["ssh_config_digest"], "SSH configuration changed")
    private_path(Path(CONFIG_PATH).parent, directory=True)
    private_path(Path(c["worktree_root"]), directory=True)
    repository_settings(c)
    return c


def datum(inputs, key, kind="text"):
    value = inputs[key]
    fields(value, ("type", "value"))
    need(value["type"] == kind, "workspace input type differs")
    return value["value"]


def prepared_subject(p):
    return subject("publish_pr", publication_payload(p))


def validate_prepared(p, c, config_digest):
    fields(p, ("schema", "owner", "previous_owner", "config_digest", "repository", "repository_id", "task_id",
               "result_id", "snapshot_id", "source_snapshot", "source_predecessor", "source_commit", "branch", "base",
               "commit", "tree", "worktree", "title", "body"))
    need(p["schema"] == PREPARED and p["config_digest"] == config_digest, "foreign preparation")
    for key in ("owner", "config_digest", "result_id", "snapshot_id", "source_snapshot"):
        string(p[key], DIGEST)
    for key in ("source_commit", "commit", "tree"):
        string(p[key], SHA)
    string(p["task_id"], r"[A-Za-z0-9][A-Za-z0-9_-]{0,127}")
    for key in ("repository", "repository_id", "base"):
        need(p[key] == c[key], "preparation machine scope differs")
    suffix = p["owner"].split(":")[1]
    need((p["source_predecessor"] is None) == (p["previous_owner"] is None), "source lineage differs from repair lineage")
    if p["source_predecessor"] is not None:
        string(p["source_predecessor"], DIGEST)
    if p["previous_owner"] is None:
        need(p["source_commit"] == c["source_commit"] and p["branch"] == c["branch_prefix"] + "/" + suffix, "initial source/branch differs")
    else:
        string(p["previous_owner"], DIGEST)
        string(p["branch"], re.escape(c["branch_prefix"]) + r"/[0-9a-f]{64}")
    need(p["worktree"] == str(Path(c["worktree_root"]) / suffix), "foreign owned artifact path")
    branch(p["branch"])
    need(p["branch"] != p["base"], "workspace head must differ from base")
    string(p["title"], limit=200)
    string(p["body"], limit=48000)
    need("\n" not in p["title"] and "\r" not in p["title"] and "<!-- af-github:" not in p["body"],
         "invalid publication content")
    # The response wraps this JSON as evidence text. Check its escaped wire size before
    # branch/worktree creation; valid content must fit the existing native output cap.
    need(len(canonical(canonical(p).decode())) <= 60000, "prepared publication exceeds native response bound")
    return p


def publication_payload(p):
    return dict(schema="af/github-publish-pr@1", head=p["branch"], base=p["base"], title=p["title"], body=p["body"])


def performance_payload(p):
    return dict(publication_payload(p), schema="af/github-performance-pr@1")


def performance_subject(p):
    return subject(PERFORMANCE_OP, performance_payload(p))


def workspace_validate_request(r, c):
    fields(r, ("schema", "method", "intent_id", "input_digest", "operation", "destination", "inputs"))
    need(r["schema"] == "af/scenario-adapter-request@4", "unsupported workspace request")
    for key in ("intent_id", "input_digest"):
        string(r[key], DIGEST)
    op, i = r["operation"], r["inputs"]
    need(op in OPS
         and r["destination"] == "github:" + c["repository"]
         and r["method"] in ("execute", "reconcile"), "unauthorized workspace operation")
    if op == PERFORMANCE_OP:
        fields(i, ("subject", "revision", "body", "candidate", "verification", "report", "measurement", "machine_scope"))
        scope = decode(string(datum(i, "machine_scope")).encode())
        fields(scope, ("repository", "repository_id", "repo", "task_state", "base", "branch_prefix", "source_commit", "source_content_digest", "evidence_adapter", "evidence_destination"))
        for key in ("repository", "repository_id", "repo", "base", "branch_prefix", "source_commit", "source_content_digest"):
            need(scope[key] == c[key], "performance publication machine scope differs")
        need(scope["task_state"] == c["task_store"] and c.get("performance") is not None
             and scope["evidence_destination"] == c["performance"]["destination"], "performance native evidence scope differs")
        content = decode(string(datum(i, "body")).encode())
        fields(content, ("schema", "title"))
        need(content["schema"] == "af/performance-pr-content@1", "unsupported performance publication content")
        string(content["title"], limit=200)
        need("\n" not in content["title"] and "\r" not in content["title"], "multiline title")
        for key in ("candidate", "verification", "report"):
            t = datum(i, key, "task")
            fields(t, ("task_id", "plan_id", "revision_id", "result_id", "charged_tokens", "result"))
            need(t["result"]["execution"] == "completed" and t["result"]["acceptance"] == "satisfied", "publication requires accepted Tasks")
        t = datum(i, "candidate", "task")
        need(t["result_id"] == datum(i, "subject") and t["result"]["outputs"]["snapshot"]["snapshot_id"] == datum(i, "revision"), "publication candidate selectors differ")
        o = datum(i, "measurement", "observation")
        need(o["satisfied"] is True and o["condition"] == "native_verified" and o["revision"] == datum(i, "revision"), "publication needs exact native measurement")
        return content
    expected = ["subject", "revision", "body", "candidate"] if op == "prepare_candidate_v1" else ["subject", "revision", "body", "prepared", "evidence"]
    fields(i, expected, ["previous"] if op == "prepare_candidate_v1" else [])
    string(datum(i, "subject"))
    string(datum(i, "revision"))
    p = decode(string(datum(i, "body")).encode())
    if op == "prepare_candidate_v1":
        fields(p, ("schema", "title", "body"))
        need(p["schema"] == "af/git-workspace-content@1", "unsupported workspace content")
        string(p["title"], limit=200)
        string(p["body"], limit=48000)
        t = datum(i, "candidate", "task")
        fields(t, ("task_id", "plan_id", "revision_id", "result_id", "charged_tokens", "result"))
        string(t["task_id"], r"[A-Za-z0-9][A-Za-z0-9_-]{0,127}")
        for key in ("plan_id", "revision_id", "result_id"):
            string(t[key], DIGEST)
        need(t["result"]["execution"] == "completed" and t["result"]["acceptance"] == "satisfied"
             and t["result"]["task_revision_id"] == t["revision_id"]
             and t["result_id"] == datum(i, "subject")
             and t["result"]["outputs"]["snapshot"]["snapshot_id"] == datum(i, "revision"),
             "preparation requires the exact accepted candidate")
        if "previous" in i:
            validate_prepared(decode(datum(i, "previous", "action")["evidence"].encode()), c, CONFIG_DIGEST)
        return p
    validate_prepared(p, c, CONFIG_DIGEST)
    a = datum(i, "prepared", "action")
    need(a["operation"] == "prepare_candidate_v1" and a["intent_id"] == p["owner"]
         and a["destination"] == r["destination"] and a["subject"] == p["result_id"]
         and a["revision"] == p["snapshot_id"] and decode(a["evidence"].encode()) == p,
         "preparation Action provenance differs")
    o = datum(i, "evidence", "observation")
    need(datum(i, "subject") == prepared_subject(p) and datum(i, "revision") == p["commit"]
         and o["condition"] == "merged" and o["satisfied"] is True
         and o["subject"] == prepared_subject(p) and o["revision"] == p["commit"],
         "cleanup requires exact native merge evidence")
    return p


class WorkspaceJournal(Journal):
    def load(self):
        state = decode(read_private(self.path, STATE_LIMIT), STATE_LIMIT)
        fields(state, ("schema", "config_digest", "records", "checksum"))
        checksum = state.pop("checksum")
        need(checksum == digest(canonical(state)) and state["schema"] == WORKSPACE_STATE
             and state["config_digest"] == self.config_digest, "foreign or corrupt workspace journal")
        need(isinstance(state["records"], dict) and len(state["records"]) <= WORKSPACE_MAX_RECORDS, "journal bound exceeded")
        for key, rec in state["records"].items():
            fields(rec, ("request", "phase", "prepared", "inode", "device", "build", "cleaned", "cleanup_pending", "progress", "reservations"))
            need(rec["phase"] in ("reserved", "started", "done"), "invalid lifecycle phase")
            need(key == rec["request"]["intent_id"], "journal intent differs")
            workspace_validate_request({**rec["request"], "method": "reconcile"}, self.config)
            if rec["prepared"] is not None:
                validate_prepared(rec["prepared"], self.config, self.config_digest)
            need(isinstance(rec["cleaned"], list) and len(rec["cleaned"]) <= WORKSPACE_MAX_RECORDS and len(set(rec["cleaned"])) == len(rec["cleaned"]), "invalid cleanup ownership ledger")
            for owner in rec["cleaned"]:
                string(owner, DIGEST)
            if rec["cleanup_pending"] is not None:
                string(rec["cleanup_pending"], DIGEST)
            for prop in ("inode", "device"):
                if rec[prop] is not None:
                    integer(rec[prop])
            fields(rec["reservations"], ("worktree", "build"))
            for kind, reservation in rec["reservations"].items():
                fields(reservation, ("slot", "published"))
                need(type(reservation["slot"]) is int and 0 <= reservation["slot"] < RESERVATION_SLOTS
                     and type(reservation["published"]) is bool, "invalid directory reservation")
                if reservation["published"]:
                    need(rec["inode"] is not None if kind == "worktree" else rec["build"] is not None,
                         "published directory lacks ownership")
            need((rec["inode"] is None) == (rec["device"] is None), "incomplete workspace identity")
            if rec["build"] is not None:
                fields(rec["build"], ("inode", "device"))
                integer(rec["build"]["inode"])
                integer(rec["build"]["device"])
            if rec["request"]["operation"] in ("prepare_candidate_v1", PERFORMANCE_OP) and rec["phase"] == "done":
                need(rec["prepared"] is not None and rec["inode"] is not None and rec["device"] is not None and rec["build"] is not None
                     and all(r["published"] for r in rec["reservations"].values()), "completed preparation lacks owned artifacts")
            if rec["request"]["operation"] != "cleanup_workspace_v1":
                need(not rec["cleaned"] and rec["cleanup_pending"] is None, "non-cleanup intent has cleanup progress")
            need(isinstance(rec["progress"], list) and set(rec["progress"]) <= {"branch", "remote"}
                 and len(set(rec["progress"])) == len(rec["progress"]), "invalid lifecycle recovery progress")
        return state

    def record(self, r):
        records = self.state["records"]
        exact = identity(r)
        if r["intent_id"] in records:
            rec = records[r["intent_id"]]
            need(rec["request"] == exact, "intent reused with changed native input")
            return rec
        need(len(records) < WORKSPACE_MAX_RECORDS, "lifecycle receipt budget exhausted")
        for rec in records.values():
            a, b = rec["request"], exact
            if b["operation"] == PERFORMANCE_OP and a["operation"] in ("prepare_candidate_v1", PERFORMANCE_OP):
                need(a["inputs"]["revision"] != b["inputs"]["revision"],
                     "candidate Snapshot already reserved; reconcile its original workspace intent")
            need((a["operation"], a["inputs"]["subject"], a["inputs"]["revision"]) !=
                 (b["operation"], b["inputs"]["subject"], b["inputs"]["revision"]), "lifecycle object has another reserved intent")
        rec = dict(request=exact, phase="reserved", prepared=None, inode=None, device=None, build=None, cleaned=[], cleanup_pending=None, progress=[],
                   reservations={kind: dict(slot=0, published=False) for kind in ("worktree", "build")})
        records[r["intent_id"]] = rec
        need(len(state_bytes(self.state)) + len(records) * 60000 <= STATE_LIMIT, "completion evidence reservation exhausted")
        self.save()
        return rec


class WorkspaceAdapter:
    def __init__(self, config, config_digest, service):
        self.config, self.config_digest, self.service = config, config_digest, service
        self.journal = WorkspaceJournal(config, config_digest)
        self.repo = "/repos/" + config["repository"]
        self.common = None
        self.performance_checked = {}

    def git(self, *args, cwd=None, data=None, extra=None):
        c = self.config
        home = Path(c["state_dir"]) / "home"
        private_path(home, directory=True)
        env = {"PATH": "/usr/bin:/bin", "HOME": str(home), "XDG_CONFIG_HOME": str(home),
               "GIT_CONFIG_NOSYSTEM": "1", "GIT_CONFIG_GLOBAL": "/dev/null", "GIT_NO_REPLACE_OBJECTS": "1", "GIT_TERMINAL_PROMPT": "0",
               "GIT_SSH_COMMAND": "/usr/bin/ssh -F " + shlex.quote(c["ssh_config"]) + " -oBatchMode=yes -oStrictHostKeyChecking=yes",
               **(extra or {})}
        command = [c["git"], "-c", "core.hooksPath=/dev/null", "-c", "core.fsmonitor=false",
                   "-c", "commit.gpgsign=false", "-c", "protocol.file.allow=never",
                   "-c", "protocol.ext.allow=never", "-c", "core.attributesFile=/dev/null", *args]
        return bounded_command(command, cwd or c["repo"], env, data, 15, c["state_dir"]).rstrip(b"\n")

    def performance_base(self):
        c = self.config
        need(self.git("rev-parse", "refs/heads/" + c["base"]).decode() == c["source_commit"], "local publication base drifted")
        remote = self.service.request("GET", self.repo + "/git/ref/heads/" + quote(c["base"], safe=""))
        need(remote.get("ref") == "refs/heads/" + c["base"]
             and remote.get("object", {}).get("type") == "commit"
             and remote["object"].get("sha") == c["source_commit"], "remote publication base drifted")
        self.common.actor()  # Revalidate the exact numeric repository and installed actor.

    def performance_content(self, request):
        c, inputs = self.config, request["inputs"]
        need(PERFORMANCE_OP in c["operations"] and c.get("performance") is not None,
             "standing publication is not installed")
        content = workspace_validate_request(dict(request, method="reconcile"), c)
        key = digest(canonical(identity(request)))
        if key in self.performance_checked:
            self.performance_base()
            return self.performance_checked[key]
        lab = c["performance"]
        need(file_digest(lab["executable"]) == lab["digest"], "pinned native evidence adapter changed")
        observation = datum(inputs, "measurement", "observation")
        native_request = dict(schema="af/performance-pr-proof-request@1",
            candidate=datum(inputs, "candidate", "task"), verification=datum(inputs, "verification", "task"),
            report=datum(inputs, "report", "task"), evidence=observation,
            source_commit=c["source_commit"], source_content_digest=c["source_content_digest"])
        # This exact request is retained before any commit, branch, push or PR effect.
        path = Path(c["state_dir"]) / (request["intent_id"].split(":")[1] + ".performance.json")
        if path.exists() or path.is_symlink():
            need(decode(read_private(path, LIMIT)) == native_request, "performance proof intent changed")
        else:
            atomic_write(path, canonical(native_request))
        home = str(Path(c["state_dir"]) / "home")
        env = {"PATH": "/usr/bin:/bin", "AF_SELF_OFFLINE": "1", "HOME": home, "XDG_CONFIG_HOME": home,
               "XDG_DATA_HOME": home, "XDG_STATE_HOME": home, "XDG_CACHE_HOME": home, "TMPDIR": c["state_dir"]}
        proof = decode(bounded_command([c["af"], "scenario", "performance-proof", "--file", str(path),
            "--repo", c["repo"], "--task-state", c["task_store"], "--json"], c["state_dir"], env, None,
            18, c["state_dir"], LIMIT), LIMIT)
        fields(proof, ("schema", "request_digest", "source_snapshot", "candidate_snapshot", "body"))
        need(proof["schema"] == "af/performance-pr-proof@1" and proof["request_digest"] == digest(canonical(native_request))
             and proof["candidate_snapshot"] == datum(inputs, "revision"), "native publication proof differs")
        lab_request = dict(schema="af/scenario-adapter-request@4", method="observe", intent_id=request["intent_id"],
            input_digest=request["input_digest"], operation="observe", destination=lab["destination"],
            inputs={key: dict(type="text", value=value) for key, value in
                dict(subject=observation["subject"], revision=observation["revision"], condition="native_verified").items()})
        observed = decode(bounded_command([lab["executable"]], c["state_dir"], env, canonical(lab_request),
            18, c["state_dir"], 65536), 65536)
        fields(observed, ("schema", "intent_id", "input_digest", "outcome", "evidence", "observation"))
        need(observed["schema"] == "af/scenario-adapter-response@1" and observed["intent_id"] == request["intent_id"]
             and observed["input_digest"] == request["input_digest"] and observed["outcome"] == "done"
             and observed["observation"] == observation, "measurement differs from retained native lab evidence")
        completion = decode(observation["evidence"].encode())
        need(completion["baseline_snapshot_id"] == proof["source_snapshot"]
             and completion["candidate_snapshot_id"] == proof["candidate_snapshot"], "measured sources differ from accepted native sources")
        self.performance_base()
        content = dict(schema="af/git-workspace-content@1", title=content["title"], body=proof["body"])
        self.performance_checked[key] = content
        return content

    def native_source(self, t, owner, previous=None):
        c = self.config
        path = Path(c["state_dir"]) / (owner.split(":")[1] + ".request.json")
        request = dict(schema="af/git-candidate-source-request@1", receipt=t,
                       source_commit=previous["commit"] if previous else c["source_commit"], candidate_snapshot_id=t["result"]["outputs"]["snapshot"]["snapshot_id"],
                       predecessor_snapshot_id=previous["snapshot_id"] if previous else None)
        if path.exists():
            need(decode(read_private(path, LIMIT)) == request, "native source request changed")
        else:
            atomic_write(path, canonical(request))
        home = Path(c["state_dir"]) / "home"
        capture = Path(c["state_dir"]) / (owner.split(":")[1] + ".source")
        raw = bounded_command([c["af"], "scenario", "candidate-source", "--file", str(path),
            "--repo", c["repo"], "--task-state", c["task_store"], "--into", str(capture), "--json"],
            c["state_dir"], {"PATH": "/usr/bin:/bin", "AF_SELF_OFFLINE": "1", "HOME": str(home), "XDG_CONFIG_HOME": str(home),
                 "XDG_DATA_HOME": str(home), "XDG_STATE_HOME": str(home), "XDG_CACHE_HOME": str(home), "TMPDIR": c["state_dir"]},
            None, 18, c["state_dir"], LIMIT)
        source = decode(raw, LIMIT)
        need(source["schema"] == "af/git-candidate-source@1"
             and source["request_digest"] == digest(canonical(request))
             and source["source_commit"] == request["source_commit"]
             and (source["predecessor_snapshot"] == previous["snapshot_id"] if previous else source["source_content_digest"] == c["source_content_digest"] and source["predecessor_snapshot"] is None)
             and source["candidate_snapshot"] == request["candidate_snapshot_id"]
             and source["repository"] == str(capture / "repository.git"), "native source provenance differs")
        return source

    def populate(self, p):
        # Only the just-created --no-checkout worktree may enter this path. Never reset
        # an interrupted/foreign checkout on reconciliation. Native materialization bypasses
        # attributes/filters and reads back exact Snapshot bytes, modes and symlinks.
        c = self.config
        path = Path(p["worktree"])
        need({entry.name for entry in path.iterdir()} == {".git"}, "workspace is not a fresh empty checkout")
        home = str(Path(c["state_dir"]) / "home")
        raw = bounded_command([c["af"], "task", "materialize", p["task_id"], "--snapshot", p["snapshot_id"],
            "--into", str(path), "--force", "--repo", c["repo"], "--state", c["task_store"], "--json"],
            c["state_dir"], {"PATH": "/usr/bin:/bin", "AF_SELF_OFFLINE": "1", "HOME": home, "XDG_CONFIG_HOME": home,
                 "XDG_DATA_HOME": home, "XDG_STATE_HOME": home, "XDG_CACHE_HOME": home, "TMPDIR": c["state_dir"]}, None, 18, c["state_dir"], LIMIT)
        receipt = decode(raw, LIMIT)
        need(receipt["schema"] == "af/task-materialized@1" and receipt["task_id"] == p["task_id"]
             and receipt["snapshot_id"] == p["snapshot_id"] and receipt["verified"] is True,
             "native workspace materialization differs from accepted preparation")
        self.git("read-tree", p["commit"], cwd=path)

    def prepare(self, rec):
        c, r = self.config, rec["request"]
        i = r["inputs"]
        performance_content = self.performance_content(r) if r["operation"] == PERFORMANCE_OP else None
        t = datum(i, "candidate", "task")
        owner = r["intent_id"]
        suffix = owner.split(":")[1]
        worktree = Path(c["worktree_root"]) / suffix
        previous = None
        if "previous" in i:
            previous = decode(datum(i, "previous", "action")["evidence"].encode())
            self.preparation(previous)
            chain = self.lineage(previous)
            need(len(chain) < WORKSPACE_MAX_RECORDS, "workspace lineage bound exceeded")
            self.publication(prepared_subject(previous), previous["commit"])
        ref = previous["branch"] if previous else c["branch_prefix"] + "/" + suffix
        if performance_content is not None and rec["prepared"] is not None:
            need((rec["prepared"]["title"], rec["prepared"]["body"]) == (performance_content["title"], performance_content["body"]),
                 "native performance publication report changed; retain the original intent")
        if rec["phase"] == "done":
            return "done", canonical(rec["prepared"]).decode()
        if rec["prepared"] is None:
            need(not worktree.exists() and not worktree.is_symlink(), "worktree path already exists")
            refs = self.git("for-each-ref", "--format=%(objectname)", "refs/heads/" + ref)
            if previous:
                self.owned(self.preparation(previous), previous)
                need(refs.decode() == previous["commit"], "repair branch changed")
            else:
                need(not refs, "branch already exists")
            source = self.native_source(t, owner, previous)
            # This fixed local fetch imports only objects from the just-validated native capture.
            self.git("-c", "protocol.file.allow=always", "fetch", "--quiet", "--no-tags", "--no-write-fetch-head",
                     source["repository"], "refs/heads/af-candidate")
            tree = source["candidate_tree"]
            need(self.git("rev-parse", source["transport_commit"] + "^{tree}").decode() == tree, "native candidate tree changed")
            content = performance_content or decode(datum(i, "body").encode())
            need(previous is None or (content["title"], content["body"]) == (previous["title"], previous["body"]), "continuation changed publication identity")
            message = content["title"] + "\n\nIntent: " + owner + "\nTask: " + t["task_id"] + "\nResult: " + t["result_id"] + "\nSnapshot: " + datum(i, "revision") + "\n"
            commit_env = {"GIT_AUTHOR_NAME": c["author_name"], "GIT_AUTHOR_EMAIL": c["author_email"],
                          "GIT_COMMITTER_NAME": c["author_name"], "GIT_COMMITTER_EMAIL": c["author_email"],
                          "GIT_AUTHOR_DATE": "2000-01-01T00:00:00+0000", "GIT_COMMITTER_DATE": "2000-01-01T00:00:00+0000"}
            commit = "0" * 40
            p = dict(schema=PREPARED, owner=owner, previous_owner=previous["owner"] if previous else None, config_digest=self.config_digest, repository=c["repository"],
                     repository_id=c["repository_id"], task_id=t["task_id"], result_id=t["result_id"],
                     snapshot_id=datum(i, "revision"), source_snapshot=source["source_snapshot"], source_predecessor=source["predecessor_snapshot"], source_commit=source["source_commit"],
                     branch=ref, base=c["base"], commit=commit, tree=tree, worktree=str(worktree), title=content["title"], body=content["body"])
            validate_prepared(p, c, self.config_digest)
            if r["operation"] == PERFORMANCE_OP:
                self.performance_base()
            p["commit"] = self.git("commit-tree", tree, "-p", source["source_commit"], data=message.encode(), extra=commit_env).decode()
            # Commit bytes and ownership intent are durable before creating any branch/worktree.
            rec["prepared"], rec["phase"] = p, "started"
            self.journal.save()
        p = rec["prepared"]
        refs = self.git("for-each-ref", "--format=%(objectname)", "refs/heads/" + ref)
        expected_parent = previous["commit"] if previous else None
        need((refs.decode() or None) in (expected_parent, p["commit"]), "foreign branch retained")
        if refs.decode() != p["commit"]:
            self.git("update-ref", "refs/heads/" + ref, p["commit"], expected_parent or "0" * 40)
        self.reserve_directory(rec, "worktree", worktree)
        # The saved inode reserves this empty directory before Git creates its worktree.
        # Resume that exact reservation, but never retry a partially registered checkout.
        need(worktree.resolve() == worktree and not worktree.is_symlink(), "foreign workspace path")
        st = worktree.lstat()
        need(stat.S_ISDIR(st.st_mode) and st.st_dev == rec["device"] and st.st_ino == rec["inode"],
             "workspace ownership changed")
        if not any(worktree.iterdir()):
            need(stat.S_IMODE(st.st_mode) == 0o700, "empty workspace permissions changed")
            # No --force: Git refuses an existing registration before changing it,
            # including a partial checkout whose directory is now empty (Git 2.30+).
            self.git("worktree", "add", "--no-checkout", "--detach", str(worktree), p["commit"])
            self.populate(p)
        # An interrupted Git checkout can leave partial bytes: preserve rather than reset them.
        self.owned(rec, p)
        build = worktree / "target"
        self.reserve_directory(rec, "build", build)
        rec["phase"] = "done"
        self.journal.save()
        return "done", canonical(p).decode()

    def reserve_directory(self, rec, kind, destination):
        reservation = rec["reservations"][kind]
        root = Path(self.config["worktree_root"])
        private_path(root, directory=True)
        while True:
            stage = root / (".reserve-" + rec["request"]["intent_id"].split(":")[1]
                            + "-" + kind + "-" + str(reservation["slot"]))
            identity = ({"device": rec["device"], "inode": rec["inode"]}
                        if kind == "worktree" and rec["inode"] is not None else
                        rec["build"] if kind == "build" else None)
            if identity is None:
                need(not destination.exists() and not destination.is_symlink(),
                     "candidate or foreign directory occupies reservation")
                if stage.exists() or stage.is_symlink():
                    # An unjournaled mkdir cannot establish ownership. Retain it,
                    # never adopt/delete it. At most three uncertain slots per kind
                    # may be bypassed; exhaustion refuses without growing artifacts.
                    st = stage.lstat()
                    need(stat.S_ISDIR(st.st_mode) and stat.S_IMODE(st.st_mode) == 0o700
                         and not any(stage.iterdir()), "uncertain staging content retained")
                    need(reservation["slot"] + 1 < RESERVATION_SLOTS, "directory reservation budget exhausted")
                    reservation["slot"] += 1
                    self.journal.save()
                    continue
                stage.mkdir(mode=0o700)
                st = stage.lstat()
                sync_directory(stage)
                sync_directory(root)
                identity = dict(device=st.st_dev, inode=st.st_ino)
                if kind == "worktree":
                    rec["device"], rec["inode"] = st.st_dev, st.st_ino
                else:
                    rec["build"] = identity
                self.journal.save()  # Identity is durable BEFORE final-path publication.
            if reservation["published"]:
                need(destination.exists() or destination.is_symlink(), "owned directory disappeared; never replace it")
                st = destination.lstat()
                need(stat.S_ISDIR(st.st_mode) and st.st_dev == identity["device"]
                     and st.st_ino == identity["inode"], "published directory ownership changed")
                if kind == "build":
                    need(stat.S_IMODE(st.st_mode) == 0o700 and not any(destination.iterdir()),
                         "unfinished build reservation changed")
                return
            if stage.exists() or stage.is_symlink():
                st = stage.lstat()
                need(stat.S_ISDIR(st.st_mode) and st.st_dev == identity["device"]
                     and st.st_ino == identity["inode"] and stat.S_IMODE(st.st_mode) == 0o700
                     and not any(stage.iterdir()), "staging ownership or content changed")
                publish_directory(stage, destination)
            # A crash after rename resumes from this exact inode, never from emptiness.
            need(destination.exists() or destination.is_symlink(), "reserved directory disappeared; never replace it")
            st = destination.lstat()
            need(stat.S_ISDIR(st.st_mode) and st.st_dev == identity["device"]
                 and st.st_ino == identity["inode"] and stat.S_IMODE(st.st_mode) == 0o700
                 and not any(destination.iterdir()), "directory publication ownership changed")
            sync_directory(destination.parent)
            sync_directory(root)
            reservation["published"] = True
            self.journal.save()
            return

    def preparation(self, p):
        rec = self.journal.state["records"].get(p["owner"])
        need(rec is not None and rec["phase"] == "done" and rec["prepared"] == p
             and rec["request"]["operation"] in ("prepare_candidate_v1", PERFORMANCE_OP), "no local completed preparation provenance")
        return rec

    def lineage(self, p):
        chain, seen = [], set()
        while p is not None:
            need(p["owner"] not in seen and len(chain) < WORKSPACE_MAX_RECORDS, "invalid or excessive repair lineage")
            seen.add(p["owner"])
            chain.append(p)
            owner = p["previous_owner"]
            parent = self.journal.state["records"].get(owner) if owner else None
            if owner:
                need(parent is not None and parent["prepared"] is not None and parent["phase"] == "done", "missing repair predecessor")
                q = parent["prepared"]
                need(p["branch"] == q["branch"] and p["source_commit"] == q["commit"]
                     and p["source_predecessor"] == q["snapshot_id"] and p["title"] == q["title"] and p["body"] == q["body"], "repair lineage changed publication identity")
                p = q
            else:
                p = None
        return chain

    def exact_tree(self, p):
        # Git status is insufficient: attributes, assume-unchanged and core.filemode can
        # hide changes to accepted bytes/modes. Compare raw tracked blobs to the owned commit.
        root = Path(p["worktree"])
        pending, expected, argument_bytes = [], [], 0
        def flush():
            nonlocal argument_bytes
            if pending:
                actual = self.git("hash-object", "--no-filters", "--", *pending, cwd=root).splitlines()
                need(actual == expected, "changed raw workspace bytes retained")
                pending.clear()
                expected.clear()
                argument_bytes = 0
        for row in self.git("ls-tree", "-r", "-l", "-z", p["commit"]).split(b"\0"):
            if not row:
                continue
            metadata, name = row.split(b"\t", 1)
            mode, kind, oid, size = metadata.split()
            path = root / os.fsdecode(name)
            info = path.lstat()
            need(kind == b"blob" and info.st_size == int(size), "changed workspace entry retained")
            if mode == b"120000":
                need(stat.S_ISLNK(info.st_mode), "changed workspace link retained")
                raw = os.fsencode(os.readlink(path))
                need(self.git("hash-object", "--no-filters", "--stdin", data=raw) == oid,
                     "changed workspace link bytes retained")
            else:
                need(mode in (b"100644", b"100755") and stat.S_ISREG(info.st_mode)
                     and bool(info.st_mode & 0o100) == (mode == b"100755"),
                     "changed workspace type or mode retained")
                pending.append(os.fsdecode(name))
                expected.append(oid)
                argument_bytes += len(name) + 1
                if len(pending) >= 256 or argument_bytes >= 32768:
                    flush()
        flush()

    def owned(self, rec, p, check_tip=True):
        path = Path(p["worktree"])
        need(path.resolve() == path and not path.is_symlink(), "foreign workspace path")
        st = path.stat()
        need(st.st_dev == rec["device"] and st.st_ino == rec["inode"], "workspace ownership changed")
        need(not (path / ".git").is_symlink() and (path / ".git").is_file(), "foreign worktree metadata retained")
        common = Path(self.git("rev-parse", "--git-common-dir", cwd=path).decode())
        common = (path / common).resolve()
        expected = Path(self.git("rev-parse", "--git-common-dir").decode())
        expected = (Path(self.config["repo"]) / expected).resolve()
        need(common == expected
             and self.git("rev-parse", "HEAD", cwd=path).decode() == p["commit"], "workspace identity changed")
        # Detached HEAD is intentional: all repair worktrees retain their exact candidate
        # while the one publication branch advances only under compare-and-swap.
        need(self.git("rev-parse", "--abbrev-ref", "HEAD", cwd=path) == b"HEAD", "foreign attached worktree retained")
        if rec["build"] is not None:
            build = path / "target"
            if build.exists() or build.is_symlink():
                st = build.lstat()
                need(stat.S_ISDIR(st.st_mode) and st.st_dev == rec["build"]["device"]
                     and st.st_ino == rec["build"]["inode"], "foreign build directory retained")
        need(not self.git("status", "--porcelain=v1", "--untracked-files=no", cwd=path), "dirty workspace retained")
        # The exact leased build directory is owned whether or not a project ignores it.
        # Every other untracked/ignored path remains foreign; tracked changes always refuse.
        for flags in (("--exclude-standard",), ("--ignored", "--exclude-standard")):
            paths = self.git("ls-files", "--others", "--directory", *flags, "-z", cwd=path)
            need(all(rec["build"] is not None and item.startswith(b"target/")
                     for item in paths.split(b"\0") if item), "foreign workspace content retained")
        self.exact_tree(p)
        if check_tip:
            need(self.git("rev-parse", "refs/heads/" + p["branch"]).decode() == p["commit"], "owned branch changed")

    def remote_ref(self, p):
        url = "ssh://git@github.com/" + self.config["repository"] + ".git"
        raw = self.git("ls-remote", "--refs", url, "refs/heads/" + p["branch"])
        if not raw:
            return None
        parts = raw.decode().split("\t")
        need(len(parts) == 2 and parts[1] == "refs/heads/" + p["branch"], "unexpected remote ref listing")
        return string(parts[0], SHA)

    def cleanup(self, rec, p):
        chain = self.lineage(p)
        check = {**rec["request"], "method": "observe", "operation": "observe", "inputs": {
            "subject": {"type": "text", "value": prepared_subject(p)},
            "revision": {"type": "text", "value": p["commit"]},
            "condition": {"type": "text", "value": "merged"}}}
        texts, _ = validate_request(check, self.common.config)
        self.common.actor()
        outcome, _, observed = self.common.observe(check, texts)
        need(outcome == "done" and observed is not None and observed["satisfied"], "cleanup requires actual exact-head merge")
        if rec["phase"] == "done":
            need(all(not Path(item["worktree"]).exists() and not Path(item["worktree"]).is_symlink() for item in chain), "completed cleanup path was replaced; foreign content retained")
            need(not self.git("for-each-ref", "--format=%(objectname)", "refs/heads/" + p["branch"])
                 and self.remote_ref(p) is None, "completed cleanup branch was recreated; foreign ref retained")
            return "done", "Owned lineage worktrees/builds and branches removed after human merge"
        # Preflight the whole explicit lineage before deleting any member. A changed remote,
        # branch, dirty worktree, replacement inode or unowned ignored artifact preserves all.
        refs = self.git("for-each-ref", "--format=%(objectname)", "refs/heads/" + p["branch"])
        need((refs.decode() == p["commit"]) or (not refs and "branch" in rec["progress"]), "changed local branch retained")
        remote = self.remote_ref(p)
        need(remote is None or remote == p["commit"], "changed remote branch retained")
        # A persisted deletion intent may have dispatched, even if no reply was saved.
        # Commit equality is not ownership: another actor can recreate the same SHA.
        need(not refs or "branch" not in rec["progress"], "uncertain local deletion; present ref retained")
        need(remote is None or "remote" not in rec["progress"], "uncertain remote deletion; present ref retained")
        worktrees = self.git("worktree", "list", "--porcelain")
        need(("branch refs/heads/" + p["branch"]).encode() not in worktrees.splitlines(), "foreign attached branch retained")
        for item in chain:
            source = self.preparation(item)
            path = Path(item["worktree"])
            if item["owner"] in rec["cleaned"]:
                need(not path.exists() and not path.is_symlink(), "removed workspace path was replaced; retained")
                continue
            if not path.exists() and not path.is_symlink():
                need(rec["cleanup_pending"] == item["owner"], "owned workspace disappeared before cleanup")
            else:
                self.owned(source, item, check_tip=False)
        for item in chain:
            if item["owner"] in rec["cleaned"]:
                continue
            source = self.preparation(item)
            path = Path(item["worktree"])
            rec["phase"], rec["cleanup_pending"] = "started", item["owner"]
            self.journal.save()
            if path.exists() or path.is_symlink():
                self.owned(source, item, check_tip=False)
                build = path / "target"
                if build.exists() or build.is_symlink():
                    need(source["build"] is not None and shutil.rmtree.avoids_symlink_attacks, "safe owned build removal unavailable")
                    shutil.rmtree(build)
                self.git("worktree", "remove", str(path))
            rec["cleaned"].append(item["owner"])
            rec["cleanup_pending"] = None
            self.journal.save()
        if refs:
            if "branch" not in rec["progress"]:
                rec["progress"].append("branch")
                self.journal.save()
            self.git("update-ref", "-d", "refs/heads/" + p["branch"], p["commit"])
        if remote is not None:
            if "remote" not in rec["progress"]:
                rec["progress"].append("remote")
                self.journal.save()
            url = "ssh://git@github.com/" + self.config["repository"] + ".git"
            self.git("push", "--porcelain", "--force-with-lease=refs/heads/" + p["branch"] + ":" + p["commit"],
                     url, ":refs/heads/" + p["branch"])
            need(self.remote_ref(p) is None, "remote cleanup readback differs")
        rec["phase"] = "done"
        self.journal.save()
        return "done", "Owned lineage worktrees/builds and branches removed after human merge"

    def configure_github(self):
        c = self.config
        heads = {r["prepared"]["branch"]: c["base"] for r in self.journal.state["records"].values() if r["prepared"] is not None}
        common_config = {key: c[key] for key in ("repository", "repository_id", "actor", "token_file", "state_dir", "required_checks", "reviewers")}
        common_config.update(schema=CONFIG_SCHEMA, heads=heads, operations=[op for op in c["operations"] if op in PAYLOAD_SCHEMAS or op == "observe"])
        self.common = WorkspaceGitHub(common_config, self.config_digest, self.service, self)
        self.common.journal.path = Path(c["state_dir"]) / "github.json"
        self.common.journal.state = self.common.journal.load()

    def publication(self, subject_value, revision):
        return self.common.publication(subject_value, revision)

    def handle(self, r):
        self.performance_checked = {}  # Native acceptance/evidence reopens on every call, including replay.
        need(r.get("operation") in self.config["operations"], "operation outside installed machine scope")
        with self.journal.locked():
            self.configure_github()
            if (r["operation"] in PAYLOAD_SCHEMAS and r["operation"] != PERFORMANCE_OP) or r["operation"] == "observe":
                texts, _ = validate_request(r, self.common.config)
                self.common.actor()
                if r["method"] == "observe":
                    outcome, evidence, obs = self.common.observe(r, texts)
                else:
                    rec = self.common.journal.record(r)
                    outcome, evidence = (self.common.execute(rec) if r["method"] == "execute" else self.common.reconcile(rec))
                    obs = None
            else:
                p = workspace_validate_request(r, self.config)
                rec = self.journal.record(r)  # Bound all retained proof files by the existing intent budget.
                if r["operation"] == PERFORMANCE_OP:
                    self.performance_content(r)
                if rec["phase"] == "reserved" and r["method"] == "reconcile":
                    outcome, evidence = "absent", "Local intent has never dispatched"
                elif r["operation"] in ("prepare_candidate_v1", PERFORMANCE_OP):
                    # Local operations are idempotent and resume from exact journaled leases.
                    outcome, evidence = self.prepare(rec)
                    if r["operation"] == PERFORMANCE_OP:
                        p = rec["prepared"]
                        self.configure_github()
                        self.common.actor()
                        publication = dict(schema="af/scenario-adapter-request@4", method=r["method"],
                            intent_id=r["intent_id"], input_digest=r["input_digest"], operation=PERFORMANCE_OP,
                            destination=r["destination"], inputs={key: dict(type="text", value=value) for key, value in
                                dict(subject=performance_subject(p), revision=p["commit"], body=canonical(performance_payload(p)).decode()).items()})
                        validate_request(publication, self.common.config)
                        held = self.common.journal.record(publication)
                        outcome, evidence = self.common.reconcile(held)
                        if outcome == "absent" and r["method"] == "execute":
                            outcome, evidence = self.common.execute(held)
                else:
                    outcome, evidence = self.cleanup(rec, p)
                obs = None
            response = dict(schema="af/scenario-adapter-response@1", intent_id=r["intent_id"],
                            input_digest=r["input_digest"], outcome=outcome, evidence=evidence, observation=obs)
            need(len(canonical(response)) <= 65536, "workspace response bound exceeded")
            return response


class WorkspaceGitHub(Adapter):
    """Reuse production publication, exact-head checks, merge observation and uncertain POST recovery.

    This installation explicitly trusts one exclusive remote writer. The stable workspace lock
    covers pushes and publication; Git ref changes additionally use exact remote leases. Unlike
    locked_head mode, GitHub cannot enforce a POST head precondition in this mode. Administrators
    must prevent all writers outside this installation, including automation and human pushes.
    """
    def __init__(self, config, config_digest, service, workspace):
        super().__init__(config, config_digest, service)
        self.workspace = workspace

    def prepared(self, record):
        payload, revision = self.payload(record), datum(record["request"]["inputs"], "revision")
        matches = [r["prepared"] for r in self.workspace.journal.state["records"].values()
                   if r["request"]["operation"] in ("prepare_candidate_v1", PERFORMANCE_OP) and r["phase"] == "done"
                   and r["prepared"]["commit"] == revision and (performance_payload(r["prepared"]) if record["request"]["operation"] == PERFORMANCE_OP else publication_payload(r["prepared"])) == payload]
        need(len(matches) == 1, "publication requires one exact local accepted-candidate preparation")
        if record["request"]["operation"] == PERFORMANCE_OP:
            need(matches[0]["owner"] == record["request"]["intent_id"]
                 and self.workspace.preparation(matches[0])["request"]["operation"] == PERFORMANCE_OP,
                 "performance publication lacks its exact native preparation intent")
        return matches[0]

    def locked_head(self, payload, revision):
        if payload.get("schema") == "af/github-performance-pr@1":
            self.workspace.performance_base()
        # An explicit alternative to production locked_head, never a fallback after refusal.
        matches = [r["prepared"] for r in self.workspace.journal.state["records"].values()
                   if r["prepared"] is not None and (performance_payload(r["prepared"]) if payload.get("schema") == "af/github-performance-pr@1" else publication_payload(r["prepared"])) == payload
                   and r["prepared"]["commit"] == revision and r["phase"] == "done"]
        need(len(matches) == 1 and self.workspace.remote_ref(matches[0]) == revision,
             "exclusive owned publication head changed")

    def prior(self, p):
        if p["previous_owner"] is None:
            return None
        parent = self.workspace.journal.state["records"][p["previous_owner"]]["prepared"]
        return self.journal.publication(prepared_subject(parent), parent["commit"])

    def marker(self, record):
        if record["request"]["operation"] == "publish_pr":
            previous = self.prior(self.prepared(record))
            if previous is not None:
                return self.marker(previous)
        return super().marker(record)

    def push(self, p):
        w = self.workspace
        w.owned(w.preparation(p), p)
        remote = w.remote_ref(p)
        parent = None if p["previous_owner"] is None else p["source_commit"]
        need(remote in (parent, p["commit"]), "foreign remote branch retained")
        need(w.git("rev-parse", p["commit"] + "^").decode() == p["source_commit"], "prepared parent changed")
        if remote != p["commit"]:
            url = "ssh://git@github.com/" + w.config["repository"] + ".git"
            if w.preparation(p)["request"]["operation"] == PERFORMANCE_OP:
                w.performance_content(w.preparation(p)["request"])
                need(parent is None and remote is None, "performance publication only creates new scoped branches")
                # A push of a pre-existing ref could fast-forward it. Receive-pack's exact
                # absence lease is mandatory; it cannot update an existing branch.
                w.git("push", "--porcelain", "--force-with-lease=refs/heads/" + p["branch"] + ":",
                      url, p["commit"] + ":refs/heads/" + p["branch"])
            else:
                w.git("push", "--porcelain", "--force-with-lease=refs/heads/" + p["branch"] + ":" + (parent or ""),
                      url, p["commit"] + ":refs/heads/" + p["branch"])
        self.locked_head(performance_payload(p) if w.preparation(p)["request"]["operation"] == PERFORMANCE_OP else publication_payload(p), p["commit"])

    def execute(self, record):
        if record["request"]["operation"] not in PR_OPERATIONS:
            return super().execute(record)
        p = self.prepared(record)
        if record["request"]["operation"] == PERFORMANCE_OP:
            self.workspace.performance_content(self.workspace.preparation(p)["request"])
            self.workspace.performance_base()
        self.workspace.lineage(p)
        previous = self.prior(p)
        if previous is None:
            if record["phase"] == "prepared":
                self.push(p)
            return super().execute(record)
        # Repair changes only the exact owned head of the same open PR; no second POST.
        if record["phase"] == "done":
            return super().reconcile(record)
        need(previous["object"] is not None, "continuation lacks an owned PR")
        remote = self.service.request("GET", self.repo + "/pulls/" + str(previous["object"]["number"]))
        need(remote.get("state") == "open" and remote.get("merged") is False, "closed PR cannot advance")
        if remote.get("head", {}).get("sha") == p["source_commit"]:
            self.validate_remote(previous, remote)
        else:
            self.validate_remote(record, remote)
        record.update(phase="dispatched", object=previous["object"])
        self.journal.save()  # Authority is durable before push, including lost-push recovery.
        self.push(p)
        self.read_object(record)
        return self.finish(record, previous["object"])

    def reconcile(self, record):
        if record["request"]["operation"] == "publish_pr" and record["phase"] == "dispatched":
            p = self.prepared(record)
            if p["previous_owner"] is not None:
                return self.execute(record)
        return super().reconcile(record)


def workspace_main():
    os.environ.clear()
    os.umask(0o077)
    need(sys.flags.isolated and sys.flags.no_site and len(sys.argv) == 1, "isolated stdin adapter required")
    signal.signal(signal.SIGALRM, deadline_expired)
    signal.alarm(WALL_SECONDS)
    try:
        r = decode(sys.stdin.buffer.read(LIMIT + 1))
        c = workspace_load_config()
        # Initial preparation is offline. A continuation must read back its prior
        # publication before admitting the next local commit, so it needs transport too.
        offline = r.get("operation") == "prepare_candidate_v1" and "previous" not in r.get("inputs", {})
        response = WorkspaceAdapter(c, CONFIG_DIGEST, None if offline else GitHub(c)).handle(r)
        sys.stdout.buffer.write(canonical(response) + b"\n")
        sys.stdout.buffer.flush()
    finally:
        signal.alarm(0)


def workspace_schema_documents():
    def obj(properties):
        return dict(type="object", additionalProperties=False, required=list(properties), properties=properties)
    def text(pattern=None, maximum=65536):
        result = dict(type="string", minLength=1, maxLength=maximum)
        if pattern:
            result["pattern"] = "^" + pattern + "$"
        return result
    def array(value, maximum=32):
        return dict(type="array", items=value, maxItems=maximum, uniqueItems=True)
    dig = text(DIGEST)
    sha = text(SHA)
    path = text(r"/[^\x00]+", 4096)
    positive = dict(type="integer", minimum=1, maximum=9007199254740991)
    nullable_int = dict(anyOf=[dict(type="null"), positive])
    who = obj(dict(id=positive, login=text(LOGIN)))
    schemas = {}
    def doc(name, properties, version=1):
        schemas[name + "-v" + str(version) + ".json"] = {"$schema": "https://json-schema.org/draft/2020-12/schema",
            "$id": "urn:af:schema:" + name + ":" + str(version), "title": name + " v" + str(version), **obj(dict(schema={"const": "af/" + name + "@" + str(version)}, **properties))}
    prepared = dict(owner=dig, previous_owner=dict(anyOf=[dict(type="null"), dig]), config_digest=dig, repository=text(REPO), repository_id=positive,
        task_id=text(r"[A-Za-z0-9][A-Za-z0-9_-]{0,127}"), result_id=dig, snapshot_id=dig, source_snapshot=dig, source_predecessor=dict(anyOf=[dict(type="null"), dig]),
        source_commit=sha, branch=text(), base=text(), commit=sha, tree=sha, worktree=path,
        title=text(maximum=200), body=text(maximum=48000))
    doc("git-workspace-prepared", prepared)
    doc("git-workspace-content", dict(title=text(maximum=200), body=text(maximum=48000)))
    doc("performance-pr-content", dict(title=text(maximum=200)))
    doc("git-candidate-source-request", dict(receipt={"$ref": "urn:af:schema:scenario:5#/$defs/TaskReceipt"},
        source_commit=sha, candidate_snapshot_id=dig, predecessor_snapshot_id=dict(anyOf=[dict(type="null"), dig])))
    doc("git-candidate-source", dict(request_digest=dig, repository_id=text(maximum=4096), source_snapshot=dig,
        candidate_snapshot=dig, predecessor_snapshot=dict(anyOf=[dict(type="null"), dig]), source_content_digest=dig, source_commit=sha, candidate_tree=sha,
        transport_commit=sha, repository=path))
    check = obj(dict(kind={"const": "check"}, name=text(maximum=200), app_id=positive))
    status = obj(dict(kind={"const": "status"}, context=text(maximum=200), creator_id=positive))
    required_checks = array(dict(oneOf=[check, status]))
    required_checks["minItems"] = 1
    config = dict(repository=text(REPO), repository_id=positive, actor=who, token_file=path, state_dir=path,
        repo=path, task_store=path, worktree_root=path, base=text(), branch_prefix=text(), source_commit=sha,
        source_content_digest=dig, af=path, af_digest=dig, git=path, git_digest=dig, ssh_config=path,
        ssh_config_digest=dig, author_name=text(maximum=200), author_email=text(maximum=200),
        required_checks=required_checks, reviewers=array(who, 15), publication_mode={"const": "exclusive_owned_writer"},
        operations={"type":"array", "items":{"enum":sorted(OPS | set(PAYLOAD_SCHEMAS) | {"observe"})}, "minItems":1, "maxItems":6, "uniqueItems":True})
    config["operations"]["maxItems"] = 7
    config["performance"] = obj(dict(executable=path, digest=dig, destination=text()))
    doc("git-workspace-config", config, version=2)
    schemas["git-workspace-config-v2.json"]["required"].remove("performance")
    request = obj(dict(schema={"const": "af/scenario-adapter-request@4"}, intent_id=dig, input_digest=dig,
        operation={"enum": sorted(OPS - {"observe"})}, destination=text(), inputs={"type": "object",
            "additionalProperties": {"$ref": "urn:af:schema:scenario:5#/$defs/Datum"}}))
    rec = obj(dict(request=request, phase={"enum": ["reserved", "started", "done"]},
        prepared=dict(anyOf=[dict(type="null"), obj(dict(schema={"const": PREPARED}, **prepared))]),
        inode=nullable_int, device=nullable_int,
        build=dict(anyOf=[dict(type="null"), obj(dict(device=positive, inode=positive))]),
        reservations=obj({kind: obj(dict(slot=dict(type="integer", minimum=0, maximum=RESERVATION_SLOTS - 1),
                                       published=dict(type="boolean"))) for kind in ("worktree", "build")}),
        cleaned=array(dig, WORKSPACE_MAX_RECORDS), cleanup_pending=dict(anyOf=[dict(type="null"), dig]),
        progress=array({"enum": ["branch", "remote"]}, 2)))
    doc("git-workspace-state", dict(config_digest=dig, checksum=dig, records={"type": "object", "maxProperties": WORKSPACE_MAX_RECORDS,
        "propertyNames": dig, "additionalProperties": rec}), version=2)
    return schemas

