"""Implementation lifecycle extension, embedded with the GitHub transport at installation.

No workflow command, environment or path is executed. The owner-side installer pins both
sources and the exact machine configuration in one isolated Python executable.
"""
import shlex
import subprocess
import shutil
import tempfile

IMPL_MAX_RECORDS = 32
IMPL_CONFIG = "af/scenario-impl-config@1"
IMPL_STATE = "af/scenario-impl-state@1"
PREPARED = "af/scenario-impl-prepared@1"
OPS = {"impl_prepare_v1", "impl_publish_v1", "impl_cleanup_v1", "observe"}


def validate_config(c):
    fields(c, ("schema", "repository", "repository_id", "actor", "token_file", "state_dir",
               "repo", "task_store", "worktree_root", "base", "branch_prefix", "source_commit",
               "source_content_digest", "af", "af_digest", "git", "git_digest", "ssh_config",
               "ssh_config_digest", "author_name", "author_email", "required_checks", "reviewers"))
    need(c["schema"] == IMPL_CONFIG, "unsupported implementation machine configuration")
    string(c["repository"], REPO)
    integer(c["repository_id"])
    person(c["actor"])
    string(c["source_commit"], SHA)
    string(c["source_content_digest"], DIGEST)
    branch(c["base"])
    branch(c["branch_prefix"])
    for key in ("author_name", "author_email"):
        string(c[key], limit=200)
        need("\n" not in c[key] and "\r" not in c[key], "invalid author")
    paths = ("repo", "task_store", "worktree_root", "state_dir", "token_file", "af", "git", "ssh_config")
    for key in paths:
        p = Path(string(c[key]))
        need(p.is_absolute() and p.resolve() == p, "machine paths must be canonical")
    for a, b in (("repo", "task_store"), ("repo", "worktree_root"), ("repo", "state_dir"),
                 ("state_dir", "worktree_root"), ("task_store", "worktree_root")):
        p, q = Path(c[a]), Path(c[b])
        need(p != q and p not in q.parents and q not in p.parents, "machine roots overlap")
    for key in ("state_dir", "task_store", "worktree_root", "token_file", "ssh_config"):
        outside_checkout(Path(c[key]))
    for key in ("af_digest", "git_digest", "ssh_config_digest"):
        string(c[key], DIGEST)
    # Reuse the established check/person schema and validators without its fixed branch map.
    original_validate_config({"schema": "af/github-adapter-config@1", "repository": c["repository"],
        "repository_id": c["repository_id"], "actor": c["actor"], "token_file": c["token_file"],
        "state_dir": c["state_dir"], "heads": {"impl/config-validation": c["base"]},
        "required_checks": c["required_checks"], "reviewers": c["reviewers"], "operations": ["observe"]})
    need(all(r["id"] != c["actor"]["id"] for r in c["reviewers"]), "a publishing actor cannot approve its own PR; use explicit independent reviewers or an empty approval set")
    return c


# The installer inserts this assignment before overriding the common validator.
# original_validate_config refers to the GitHub transport's configuration validator.


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


def load_config():
    need(CONFIG_PATH is not None and CONFIG_DIGEST is not None, "adapter is not installed")
    raw = read_private(Path(CONFIG_PATH), LIMIT)
    need(digest(raw) == CONFIG_DIGEST, "installed implementation configuration changed")
    c = validate_config(decode(raw))
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
    need(value["type"] == kind, "implementation input type differs")
    return value["value"]


def prepared_subject(p):
    return "impl:" + digest(canonical(p))


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
    string(p["title"], limit=200)
    string(p["body"], limit=48000)
    need("\n" not in p["title"] and "\r" not in p["title"] and "<!-- af-impl:" not in p["body"],
         "invalid publication content")
    # The response wraps this JSON as evidence text. Check its escaped wire size before
    # branch/worktree creation; valid content must fit the existing native output cap.
    need(len(canonical(canonical(p).decode())) <= 60000, "prepared publication exceeds native response bound")
    return p


def validate_impl_request(r, c):
    fields(r, ("schema", "method", "intent_id", "input_digest", "operation", "destination", "inputs"))
    need(r["schema"] == "af/scenario-adapter-request@3", "unsupported native adapter request")
    for key in ("intent_id", "input_digest"):
        string(r[key], DIGEST)
    op = r["operation"]
    need(op in OPS and r["destination"] == "github:" + c["repository"], "unauthorized lifecycle operation/destination")
    need((op == "observe" and r["method"] == "observe") or
         (op != "observe" and r["method"] in ("execute", "reconcile", "recover")), "method cannot disguise effects")
    i = r["inputs"]
    if op == "observe":
        fields(i, ("subject", "revision", "condition"))
        string(datum(i, "subject"), r"impl:sha256:[0-9a-f]{64}")
        string(datum(i, "revision"), SHA)
        need(datum(i, "condition") in ("checks_passed", "review_passed", "merged", "implementation_outcome", "implementation_ready"), "unsupported implementation Wait")
        return None
    expected = ["subject", "revision", "body", "candidate"] if op == "impl_prepare_v1" else ["subject", "revision", "body", "prepared"]
    if op == "impl_publish_v1":
        expected.append("decision")
    if op == "impl_cleanup_v1":
        expected.append("evidence")
    if op == "impl_prepare_v1" and "previous" in i:
        expected += ["previous", "repair"]
    fields(i, expected)
    string(datum(i, "subject"))
    string(datum(i, "revision"))
    p = decode(string(datum(i, "body")).encode())
    if op == "impl_prepare_v1":
        fields(p, ("schema", "title", "body"))
        need(p["schema"] == "af/scenario-impl-publication@1", "unsupported publication content")
        string(p["title"], limit=200)
        string(p["body"], limit=48000)
        t = datum(i, "candidate", "task")
        fields(t, ("task_id", "plan_id", "revision_id", "result_id", "charged_tokens", "result"))
        string(t["task_id"], r"[A-Za-z0-9][A-Za-z0-9_-]{0,127}")
        for key in ("plan_id", "revision_id", "result_id"):
            string(t[key], DIGEST)
        result = t["result"]
        need(result["execution"] == "completed" and result["acceptance"] == "satisfied"
             and t["result_id"] == datum(i, "subject")
             and result["outputs"]["snapshot"]["snapshot_id"] == datum(i, "revision"),
             "preparation is not an accepted exact candidate")
        if "previous" in i:
            previous = decode(datum(i, "previous", "action")["evidence"].encode())
            validate_prepared(previous, c, CONFIG_DIGEST)
            observed = datum(i, "repair", "observation")
            outcome = decode(observed["evidence"].encode())
            need(observed["satisfied"] is True and observed["condition"] == "implementation_outcome"
                 and outcome["schema"] == "af/scenario-impl-outcome@1" and outcome["repair"] is True
                 and outcome["result_id"] == previous["result_id"] and outcome["snapshot_id"] == previous["snapshot_id"]
                 and observed["subject"] == prepared_subject(previous) and observed["revision"] == previous["commit"],
                 "repair preparation lacks exact native failure evidence")
        return p
    validate_prepared(p, c, CONFIG_DIGEST)
    a = datum(i, "prepared", "action")
    fields(a, ("intent_id", "operation", "destination", "subject", "revision", "evidence"))
    need(a["operation"] == "impl_prepare_v1" and a["intent_id"] == p["owner"]
         and a["destination"] == r["destination"] and a["subject"] == p["result_id"]
         and a["revision"] == p["snapshot_id"] and decode(a["evidence"].encode()) == p,
         "preparation Action provenance differs")
    need(datum(i, "subject") == prepared_subject(p) and datum(i, "revision") == p["commit"],
         "action does not bind prepared bytes and exact commit")
    if op == "impl_publish_v1":
        d = datum(i, "decision", "decision")
        need(d["schema"] == "af/scenario-signed-decision@1" and d["signature"], "signed native decision required")
        q = d["request"]
        need(q["approve"] is True and q["subject"] == prepared_subject(p)
             and q["revision"] == p["commit"] and type(q["expires_unix_ms"]) is int,
             "decision is stale or differs from prepared publication")
    else:
        o = datum(i, "evidence", "observation")
        need(o["condition"] == "merged" and o["satisfied"] is True
             and o["subject"] == prepared_subject(p) and o["revision"] == p["commit"], "cleanup needs exact merge evidence")
    return p


class ImplJournal(Journal):
    def load(self):
        state = decode(read_private(self.path, STATE_LIMIT), STATE_LIMIT)
        fields(state, ("schema", "config_digest", "records", "checksum"))
        checksum = state.pop("checksum")
        need(checksum == digest(canonical(state)) and state["schema"] == IMPL_STATE
             and state["config_digest"] == self.config_digest, "foreign or corrupt implementation journal")
        need(isinstance(state["records"], dict) and len(state["records"]) <= IMPL_MAX_RECORDS, "journal bound exceeded")
        for key, rec in state["records"].items():
            fields(rec, ("request", "phase", "prepared", "object", "inode", "device", "build", "cleaned", "cleanup_pending", "progress"))
            need(rec["phase"] in ("reserved", "started", "done"), "invalid lifecycle phase")
            need(key == rec["request"]["intent_id"], "journal intent differs")
            validate_impl_request({**rec["request"], "method": "reconcile"}, self.config)
            if rec["prepared"] is not None:
                validate_prepared(rec["prepared"], self.config, self.config_digest)
            need(isinstance(rec["cleaned"], list) and len(rec["cleaned"]) <= 3 and len(set(rec["cleaned"])) == len(rec["cleaned"]), "invalid cleanup ownership ledger")
            for owner in rec["cleaned"]:
                string(owner, DIGEST)
            if rec["cleanup_pending"] is not None:
                string(rec["cleanup_pending"], DIGEST)
            if rec["object"] is not None:
                fields(rec["object"], ("id", "number"))
                integer(rec["object"]["id"])
                integer(rec["object"]["number"])
            for prop in ("inode", "device"):
                if rec[prop] is not None:
                    integer(rec[prop])
            if rec["build"] is not None:
                fields(rec["build"], ("inode", "device"))
                integer(rec["build"]["inode"])
                integer(rec["build"]["device"])
            if rec["request"]["operation"] == "impl_prepare_v1" and rec["phase"] == "done":
                need(rec["prepared"] is not None and rec["inode"] is not None and rec["device"] is not None and rec["build"] is not None, "completed preparation lacks owned artifacts")
            if rec["request"]["operation"] != "impl_cleanup_v1":
                need(not rec["cleaned"] and rec["cleanup_pending"] is None, "non-cleanup intent has cleanup progress")
            need(isinstance(rec["progress"], list) and set(rec["progress"]) <= {"push", "post", "build", "worktree", "branch", "remote"}
                 and len(set(rec["progress"])) == len(rec["progress"]), "invalid lifecycle recovery progress")
        return state

    def record(self, r):
        records = self.state["records"]
        exact = identity(r)
        if r["intent_id"] in records:
            rec = records[r["intent_id"]]
            need(rec["request"] == exact, "intent reused with changed native input")
            return rec
        need(len(records) < IMPL_MAX_RECORDS, "lifecycle receipt budget exhausted")
        for rec in records.values():
            a, b = rec["request"], exact
            need((a["operation"], a["inputs"]["subject"], a["inputs"]["revision"]) !=
                 (b["operation"], b["inputs"]["subject"], b["inputs"]["revision"]), "lifecycle object has another reserved intent")
        rec = dict(request=exact, phase="reserved", prepared=None, object=None, inode=None, device=None, build=None, cleaned=[], cleanup_pending=None, progress=[])
        records[r["intent_id"]] = rec
        need(len(state_bytes(self.state)) + len(records) * 60000 <= STATE_LIMIT, "completion evidence reservation exhausted")
        self.save()
        return rec


class ImplAdapter:
    def __init__(self, config, config_digest, service):
        self.config, self.config_digest, self.service = config, config_digest, service
        self.journal = ImplJournal(config, config_digest)
        self.repo = "/repos/" + config["repository"]
        self.common = Adapter.__new__(Adapter)
        self.common.config, self.common.service, self.common.repo = config, service, self.repo

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

    def native_source(self, t, owner, previous=None):
        c = self.config
        path = Path(c["state_dir"]) / (owner.split(":")[1] + ".request.json")
        request = dict(schema="af/scenario-impl-source-request@1", receipt=t,
                       source_commit=previous["commit"] if previous else c["source_commit"], candidate_snapshot_id=t["result"]["outputs"]["snapshot"]["snapshot_id"],
                       predecessor_snapshot_id=previous["snapshot_id"] if previous else None)
        if path.exists():
            need(decode(read_private(path, LIMIT)) == request, "native source request changed")
        else:
            atomic_write(path, canonical(request))
        home = Path(c["state_dir"]) / "home"
        capture = Path(c["state_dir"]) / (owner.split(":")[1] + ".source")
        raw = bounded_command([c["af"], "scenario", "impl-source", "--file", str(path),
            "--repo", c["repo"], "--task-state", c["task_store"], "--into", str(capture), "--json"],
            c["state_dir"], {"PATH": "/usr/bin:/bin", "AF_SELF_OFFLINE": "1", "HOME": str(home), "XDG_CONFIG_HOME": str(home),
                 "XDG_DATA_HOME": str(home), "XDG_STATE_HOME": str(home), "XDG_CACHE_HOME": str(home)},
            None, 18, c["state_dir"], LIMIT)
        source = decode(raw, LIMIT)
        need(source["schema"] == "af/scenario-impl-source@1"
             and source["request_digest"] == digest(canonical(request))
             and source["source_commit"] == request["source_commit"]
             and (source["predecessor_snapshot"] == previous["snapshot_id"] if previous else source["source_content_digest"] == c["source_content_digest"] and source["predecessor_snapshot"] is None)
             and source["candidate_snapshot"] == request["candidate_snapshot_id"]
             and source["repository"] == str(capture / "repository.git"), "native source provenance differs")
        return source

    def prepare(self, rec):
        c, r = self.config, rec["request"]
        i = r["inputs"]
        t = datum(i, "candidate", "task")
        owner = r["intent_id"]
        suffix = owner.split(":")[1]
        worktree = Path(c["worktree_root"]) / suffix
        previous = None
        if "previous" in i:
            previous = decode(datum(i, "previous", "action")["evidence"].encode())
            self.preparation(previous)
            chain = self.lineage(previous)
            need(len(chain) <= 2, "implementation repair allowance exhausted")
            self.publication(prepared_subject(previous), previous["commit"])
        ref = previous["branch"] if previous else c["branch_prefix"] + "/" + suffix
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
                     source["repository"], "refs/heads/af-impl-candidate")
            tree = source["candidate_tree"]
            need(self.git("rev-parse", source["transport_commit"] + "^{tree}").decode() == tree, "native candidate tree changed")
            content = decode(datum(i, "body").encode())
            message = content["title"] + "\n\nIntent: " + owner + "\nTask: " + t["task_id"] + "\nResult: " + t["result_id"] + "\nSnapshot: " + datum(i, "revision") + "\n"
            commit_env = {"GIT_AUTHOR_NAME": c["author_name"], "GIT_AUTHOR_EMAIL": c["author_email"],
                          "GIT_COMMITTER_NAME": c["author_name"], "GIT_COMMITTER_EMAIL": c["author_email"],
                          "GIT_AUTHOR_DATE": "2000-01-01T00:00:00+0000", "GIT_COMMITTER_DATE": "2000-01-01T00:00:00+0000"}
            commit = self.git("commit-tree", tree, "-p", source["source_commit"], data=message.encode(), extra=commit_env).decode()
            p = dict(schema=PREPARED, owner=owner, previous_owner=previous["owner"] if previous else None, config_digest=self.config_digest, repository=c["repository"],
                     repository_id=c["repository_id"], task_id=t["task_id"], result_id=t["result_id"],
                     snapshot_id=datum(i, "revision"), source_snapshot=source["source_snapshot"], source_predecessor=source["predecessor_snapshot"], source_commit=source["source_commit"],
                     branch=ref, base=c["base"], commit=commit, tree=tree, worktree=str(worktree), title=content["title"], body=content["body"])
            validate_prepared(p, c, self.config_digest)
            # Commit bytes and ownership intent are durable before creating any branch/worktree.
            rec["prepared"], rec["phase"] = p, "started"
            self.journal.save()
        p = rec["prepared"]
        refs = self.git("for-each-ref", "--format=%(objectname)", "refs/heads/" + ref)
        expected_parent = previous["commit"] if previous else None
        need((refs.decode() or None) in (expected_parent, p["commit"]), "foreign branch retained")
        if refs.decode() != p["commit"]:
            self.git("update-ref", "refs/heads/" + ref, p["commit"], expected_parent or "0" * 40)
        if not worktree.exists():
            need(rec["inode"] is None, "owned worktree disappeared; never replace it")
            worktree.mkdir(mode=0o700)
            st = worktree.stat()
            rec["inode"], rec["device"] = st.st_ino, st.st_dev
            self.journal.save()
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
            self.git("worktree", "add", "--detach", str(worktree), p["commit"])
        # An interrupted Git checkout can leave partial bytes: preserve rather than reset them.
        self.owned(rec, p)
        build = worktree / "target"
        if rec["build"] is None:
            need(not build.exists() and not build.is_symlink(), "candidate occupies reserved build directory")
            build.mkdir(mode=0o700)
            st = build.stat()
            rec["build"] = {"device": st.st_dev, "inode": st.st_ino}
            self.journal.save()
        rec["phase"] = "done"
        self.journal.save()
        return "done", canonical(p).decode()

    def preparation(self, p):
        rec = self.journal.state["records"].get(p["owner"])
        need(rec is not None and rec["phase"] == "done" and rec["prepared"] == p
             and rec["request"]["operation"] == "impl_prepare_v1", "no local completed preparation provenance")
        return rec

    def lineage(self, p):
        chain, seen = [], set()
        while p is not None:
            need(p["owner"] not in seen and len(chain) < 3, "invalid or excessive repair lineage")
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

    def owned(self, rec, p, check_tip=True):
        path = Path(p["worktree"])
        need(path.resolve() == path and not path.is_symlink(), "foreign workspace path")
        st = path.stat()
        need(st.st_dev == rec["device"] and st.st_ino == rec["inode"], "workspace ownership changed")
        common = Path(self.git("rev-parse", "--git-common-dir", cwd=path).decode())
        common = (path / common).resolve()
        expected = Path(self.git("rev-parse", "--git-common-dir").decode())
        expected = (Path(self.config["repo"]) / expected).resolve()
        need(common == expected and (path / ".git").is_file()
             and self.git("rev-parse", "HEAD", cwd=path).decode() == p["commit"], "workspace identity changed")
        # Detached HEAD is intentional: all repair worktrees retain their exact candidate
        # while the one publication branch advances only under compare-and-swap.
        need(self.git("rev-parse", "--abbrev-ref", "HEAD", cwd=path) == b"HEAD", "foreign attached worktree retained")
        need(not self.git("status", "--porcelain=v1", "--untracked-files=all", cwd=path), "dirty workspace retained")
        ignored = self.git("ls-files", "--others", "--ignored", "--exclude-standard", "-z", cwd=path)
        need(all(item.startswith(b"target/") for item in ignored.split(b"\0") if item), "foreign ignored content retained")
        if rec["build"] is not None:
            build = path / "target"
            if build.exists() or build.is_symlink():
                st = build.lstat()
                need(stat.S_ISDIR(st.st_mode) and st.st_dev == rec["build"]["device"]
                     and st.st_ino == rec["build"]["inode"], "foreign build directory retained")
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

    def marker(self, rec):
        if rec["request"]["operation"] == "impl_publish_v1":
            p = decode(datum(rec["request"]["inputs"], "body").encode())
            if p["previous_owner"] is not None:
                parent = self.journal.state["records"][p["previous_owner"]]["prepared"]
                previous, _ = self.publication(prepared_subject(parent), parent["commit"])
                return self.marker(previous)
        return "\n\n<!-- af-impl:" + rec["request"]["intent_id"] + ":" + digest(canonical(rec["request"])) + " -->"

    def validate_pr(self, rec, p, remote):
        integer(remote.get("id"))
        integer(remote.get("number"))
        if rec["object"] is not None:
            need(rec["object"] == {"id": remote["id"], "number": remote["number"]}, "foreign PR identity")
        need(remote.get("user", {}).get("id") == self.config["actor"]["id"]
             and remote.get("title") == p["title"] and remote.get("body") == p["body"] + self.marker(rec), "PR content or creator changed")
        for side, ref in (("head", p["branch"]), ("base", p["base"])):
            need(remote.get(side, {}).get("ref") == ref
                 and remote[side].get("repo", {}).get("id") == p["repository_id"]
                 and remote[side]["repo"].get("full_name") == p["repository"], "PR repository or branch changed")
        need(remote["head"].get("sha") == p["commit"], "changed head invalidates all checks and merge observations")
        return remote

    def read_pr(self, rec, p):
        if rec["object"] is not None:
            pr = self.service.request("GET", self.repo + "/pulls/" + str(rec["object"]["number"]))
            return self.validate_pr(rec, p, pr)
        rows = self.service.pages(self.repo + "/pulls", params={"state": "all", "head": p["repository"].split("/")[0] + ":" + p["branch"], "base": p["base"]})
        matches = [row for row in rows if row.get("body") == p["body"] + self.marker(rec)]
        need(len(matches) <= 1, "multiple matching publication objects")
        return self.validate_pr(rec, p, matches[0]) if matches else None

    def publish(self, rec, p, execute):
        self.preparation(p)
        self.common.actor()
        chain = self.lineage(p)
        previous = chain[1] if len(chain) > 1 else None
        if previous:
            prior, _ = self.publication(prepared_subject(previous), previous["commit"])
            need(prior["object"] is not None, "repair lacks its original PR identity")
            rec["object"] = prior["object"]
            raw = self.service.request("GET", self.repo + "/pulls/" + str(rec["object"]["number"]))
            if raw.get("head", {}).get("sha") == p["commit"]:
                self.validate_pr(rec, p, raw)
                rec["phase"] = "done"
                self.journal.save()
                return "done", "Owned repaired PR " + str(raw["number"]) + " at " + p["commit"]
            self.validate_pr(prior, previous, raw)
            need(raw.get("state") == "open" and raw.get("merged") is False, "closed PR cannot be repaired")
            pr = None
        else:
            pr = self.read_pr(rec, p)
        if pr is not None:
            rec["object"] = {"id": pr["id"], "number": pr["number"]}
            rec["phase"] = "done"
            self.journal.save()
            return "done", "Owned PR " + str(pr["number"]) + " at " + p["commit"]
        if "post" in rec["progress"]:
            return "pending", "PR creation may have occurred; remote absence never authorizes another POST"
        if not execute:
            return ("absent", "Reserved publication has not dispatched") if rec["phase"] == "reserved" else ("pending", "Resume the same publication intent")
        self.owned(self.preparation(p), p)
        rec["phase"] = "started"
        if "push" not in rec["progress"]:
            rec["progress"].append("push")
            self.journal.save()
        remote = self.remote_ref(p)
        parent_commit = previous["commit"] if previous else None
        need(remote in (parent_commit, p["commit"]), "foreign or changed remote branch retained")
        if remote != p["commit"]:
            # Initial publication compares against absence. Repair compares against the prior
            # owned commit, and the new commit's parent must be exactly that prior commit.
            need(self.git("rev-parse", p["commit"] + "^").decode() == p["source_commit"], "prepared parent changed")
            url = "ssh://git@github.com/" + self.config["repository"] + ".git"
            self.git("push", "--porcelain", "--force-with-lease=refs/heads/" + p["branch"] + ":" + (parent_commit or ""), url,
                     p["commit"] + ":refs/heads/" + p["branch"])
        need(self.remote_ref(p) == p["commit"], "pushed head readback differs")
        if previous:
            self.read_pr(rec, p)
        else:
            rec["progress"].append("post")
            self.journal.save()
            pr = self.service.request("POST", self.repo + "/pulls", {"head": p["branch"], "base": p["base"],
                "title": p["title"], "body": p["body"] + self.marker(rec), "draft": False})
            self.validate_pr(rec, p, pr)
            rec["object"] = {"id": pr["id"], "number": pr["number"]}
        rec["phase"] = "done"
        self.journal.save()
        return "done", "Owned PR " + str(rec["object"]["number"]) + " at " + p["commit"]

    def publication(self, subject_value, revision):
        matches = [r for r in self.journal.state["records"].values()
                   if r["request"]["operation"] == "impl_publish_v1"
                   and datum(r["request"]["inputs"], "subject") == subject_value
                   and datum(r["request"]["inputs"], "revision") == revision and r["phase"] == "done"]
        need(len(matches) == 1, "no completed owned PR receipt")
        rec = matches[0]
        p = decode(datum(rec["request"]["inputs"], "body").encode())
        self.preparation(p)
        return rec, p

    def check_outcome(self, p, pr):
        reasons = []
        green = self.common.green(p["commit"])
        if not green:
            for check in self.config["required_checks"]:
                if check["kind"] == "check":
                    rows = self.service.pages(self.repo + "/commits/" + p["commit"] + "/check-runs", key="check_runs", params={"filter": "all"})
                    matches = [r for r in rows if r.get("name") == check["name"] and r.get("app", {}).get("id") == check["app_id"] and r.get("head_sha") == p["commit"]]
                    if matches:
                        latest = max(matches, key=lambda r: integer(r["id"]))
                        suites = self.service.pages(self.repo + "/commits/" + p["commit"] + "/check-suites", key="check_suites")
                        app_suites = [s for s in suites if s.get("app", {}).get("id") == check["app_id"]]
                        newest = max(app_suites, key=lambda s: integer(s["id"])) if app_suites else None
                        if newest is not None:
                            need(newest.get("head_sha") == p["commit"], "stale failure suite")
                        if (newest is not None and latest.get("check_suite", {}).get("id") == newest["id"]
                                and latest.get("status") == "completed"
                                and latest.get("conclusion") in ("failure", "timed_out", "cancelled", "action_required", "startup_failure", "skipped", "neutral", "stale")):
                            reasons.append("CI " + check["name"] + " failed, run " + str(latest["id"]))
                else:
                    rows = self.service.pages(self.repo + "/commits/" + p["commit"] + "/statuses")
                    matches = [r for r in rows if r.get("context") == check["context"] and r.get("creator", {}).get("id") == check["creator_id"]]
                    if matches and max(matches, key=lambda r: integer(r["id"])).get("state") in ("error", "failure"):
                        reasons.append("CI status " + check["context"] + " failed")
        reviews = self.service.pages(self.repo + "/pulls/" + str(pr["number"]) + "/reviews")
        latest = {}
        for review in reviews:
            if review.get("state") not in ("APPROVED", "CHANGES_REQUESTED", "DISMISSED"):
                continue
            ident = integer(review.get("user", {}).get("id"))
            if ident not in latest or integer(review["id"]) > latest[ident]["id"]:
                latest[ident] = review
        for row in latest.values():
            if row.get("state") == "CHANGES_REQUESTED" and row.get("commit_id") == p["commit"]:
                body = str(row.get("body") or "Changes requested").replace("\x00", "").encode("utf-8")[:512].decode("utf-8", errors="ignore")
                reasons.append("Review " + str(row["id"]) + ": " + body)
        if pr.get("state") == "open" and (pr.get("mergeable") is False or pr.get("mergeable_state") == "dirty"):
            reasons.append("PR conflicts with its current base; repair requires captured base evidence")
        approved = all(
            latest.get(who["id"], {}).get("state") == "APPROVED" and latest[who["id"]].get("commit_id") == p["commit"]
            for who in self.config["reviewers"])
        approved = approved and not any(row.get("state") == "CHANGES_REQUESTED" for row in latest.values())
        reasons = reasons[:32]
        ready = green and approved and not reasons and (pr.get("state") == "open" or pr.get("merged") is True)
        if not reasons and not ready:
            return None
        return dict(schema="af/scenario-impl-outcome@1", subject=prepared_subject(p), revision=p["commit"],
                    result_id=p["result_id"], snapshot_id=p["snapshot_id"], ready=ready, repair=bool(reasons),
                    reasons=reasons, pr_id=pr["id"], pr_number=pr["number"])

    def observe(self, r):
        i = r["inputs"]
        subject_value, revision, condition = (datum(i, k) for k in ("subject", "revision", "condition"))
        self.common.actor()
        rec, p = self.publication(subject_value, revision)
        pr = self.read_pr(rec, p)
        need(pr is not None, "owned PR disappeared")
        if pr.get("state") == "closed" and pr.get("merged") is not True:
            return "failed", "PR closed without merge; workspace and branches retained", False
        if condition in ("implementation_outcome", "implementation_ready"):
            result = self.check_outcome(p, pr)
            after = self.read_pr(rec, p)
            need(after.get("state") == pr.get("state") and after.get("merged") == pr.get("merged"), "PR changed during failure observation")
            if result is None:
                return "pending", "Waiting for exact-head CI and required review evidence", False
            if result["repair"] and pr.get("merged") is True:
                return "failed", "Merged PR has unresolved CI/review evidence; workspace retained", False
            if condition == "implementation_outcome" and result["ready"] and pr.get("merged") is not True:
                return "pending", "Exact-head CI/review passed; waiting for human merge or later failure", False
            if condition == "implementation_ready":
                return ("done", "Exact-head CI/review passed", True) if result["ready"] else ("failed", "Implementation repair allowance exhausted: " + "; ".join(result["reasons"]), False)
            return "done", canonical(result).decode(), True
        if condition == "merged":
            ok = pr.get("state") == "closed" and pr.get("merged") is True and isinstance(pr.get("merged_at"), str) and bool(pr["merged_at"]) and re.fullmatch(SHA, pr.get("merge_commit_sha") or "") is not None
        elif condition == "checks_passed":
            ok = pr.get("state") == "open" and self.common.green(revision)
        else:
            reviews = self.service.pages(self.repo + "/pulls/" + str(pr["number"]) + "/reviews")
            latest = {}
            for review in reviews:
                if review.get("state") not in ("APPROVED", "CHANGES_REQUESTED", "DISMISSED"):
                    continue
                ident = review.get("user", {}).get("id")
                if ident not in latest or review["id"] > latest[ident]["id"]:
                    latest[ident] = review
            ok = all(
                latest.get(who["id"], {}).get("state") == "APPROVED"
                and latest[who["id"]].get("commit_id") == revision for who in self.config["reviewers"])
            ok = ok and not any(row.get("state") == "CHANGES_REQUESTED" for row in latest.values())
        # Re-read PR identity and exact head after all listings. No result survives a head change.
        after = self.read_pr(rec, p)
        need(after.get("state") == pr.get("state") and after.get("merged") == pr.get("merged"), "PR changed during observation")
        return ("done" if ok else "pending"), ("Exact-head " + condition + (" satisfied" if ok else " pending")), bool(ok)

    def cleanup(self, rec, p):
        chain = self.lineage(p)
        check = {**rec["request"], "method": "observe", "operation": "observe", "inputs": {
            "subject": {"type": "text", "value": prepared_subject(p)},
            "revision": {"type": "text", "value": p["commit"]},
            "condition": {"type": "text", "value": "merged"}}}
        outcome, _, merged = self.observe(check)
        need(outcome == "done" and merged, "cleanup requires actual exact-head merge")
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

    def handle(self, r):
        p = validate_impl_request(r, self.config)
        with self.journal.locked():
            if r["operation"] == "observe":
                outcome, evidence, satisfied = self.observe(r)
                obs = dict(subject=datum(r["inputs"], "subject"), revision=datum(r["inputs"], "revision"),
                           condition=datum(r["inputs"], "condition"), satisfied=satisfied, evidence=evidence)
            else:
                rec = self.journal.record(r)
                op = r["operation"]
                need(r["method"] != "recover" or rec["phase"] != "reserved", "recovery requires an already dispatched lifecycle intent")
                if op == "impl_publish_v1":
                    outcome, evidence = self.publish(rec, p, r["method"] in ("execute", "recover"))
                elif rec["phase"] == "started" and r["method"] == "reconcile":
                    outcome, evidence = "pending", "Retained lifecycle intent requires explicit bounded recovery"
                elif rec["phase"] == "reserved" and r["method"] == "reconcile":
                    outcome, evidence = "absent", "Durable local lifecycle intent has not dispatched"
                elif op == "impl_prepare_v1":
                    outcome, evidence = self.prepare(rec)
                else:
                    outcome, evidence = self.cleanup(rec, p)
                obs = None
            need(len(evidence.encode()) <= 65536, "lifecycle output bound exceeded")
            response = dict(schema="af/scenario-adapter-response@1", intent_id=r["intent_id"],
                            input_digest=r["input_digest"], outcome=outcome, evidence=evidence, observation=obs)
            need(len(canonical(response)) <= 65536, "native lifecycle response bound exceeded")
            return response


def main():
    os.environ.clear()
    os.umask(0o077)
    need(sys.flags.isolated and sys.flags.no_site and len(sys.argv) == 1, "isolated stdin-only adapter required")
    signal.signal(signal.SIGALRM, deadline_expired)
    signal.alarm(WALL_SECONDS)
    try:
        r = decode(sys.stdin.buffer.read(LIMIT + 1))
        c = load_config()
        response = ImplAdapter(c, CONFIG_DIGEST, GitHub(c) if r.get("operation") != "impl_prepare_v1" else None).handle(r)
        sys.stdout.buffer.write(canonical(response) + b"\n")
        sys.stdout.buffer.flush()
    finally:
        signal.alarm(0)



def impl_schema_documents():
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
    def doc(name, properties):
        schemas[name + "-v1.json"] = {"$schema": "https://json-schema.org/draft/2020-12/schema",
            "$id": "urn:af:schema:" + name + ":1", "title": name + " v1", **obj(dict(schema={"const": "af/" + name + "@1"}, **properties))}
    prepared = dict(owner=dig, previous_owner=dict(anyOf=[dict(type="null"), dig]), config_digest=dig, repository=text(REPO), repository_id=positive,
        task_id=text(r"[A-Za-z0-9][A-Za-z0-9_-]{0,127}"), result_id=dig, snapshot_id=dig, source_snapshot=dig, source_predecessor=dict(anyOf=[dict(type="null"), dig]),
        source_commit=sha, branch=text(), base=text(), commit=sha, tree=sha, worktree=path,
        title=text(maximum=200), body=text(maximum=48000))
    doc("scenario-impl-prepared", prepared)
    doc("scenario-impl-outcome", dict(subject=text(r"impl:sha256:[0-9a-f]{64}"), revision=sha, result_id=dig,
        snapshot_id=dig, ready=dict(type="boolean"), repair=dict(type="boolean"), reasons=array(text(), 32), pr_id=positive, pr_number=positive))
    doc("scenario-impl-publication", dict(title=text(maximum=200), body=text(maximum=48000)))
    doc("scenario-impl-source-request", dict(receipt={"$ref": "urn:af:schema:scenario:4#/$defs/TaskReceipt"},
        source_commit=sha, candidate_snapshot_id=dig, predecessor_snapshot_id=dict(anyOf=[dict(type="null"), dig])))
    doc("scenario-impl-source", dict(request_digest=dig, repository_id=text(maximum=4096), source_snapshot=dig,
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
        required_checks=required_checks, reviewers=array(who, 15))
    doc("scenario-impl-config", config)
    request = obj(dict(schema={"const": "af/scenario-adapter-request@3"}, intent_id=dig, input_digest=dig,
        operation={"enum": sorted(OPS - {"observe"})}, destination=text(), inputs={"type": "object",
            "additionalProperties": {"$ref": "urn:af:schema:scenario:4#/$defs/Datum"}}))
    rec = obj(dict(request=request, phase={"enum": ["reserved", "started", "done"]},
        prepared=dict(anyOf=[dict(type="null"), obj(dict(schema={"const": PREPARED}, **prepared))]),
        object=dict(anyOf=[dict(type="null"), obj(dict(id=positive, number=positive))]),
        inode=nullable_int, device=nullable_int,
        build=dict(anyOf=[dict(type="null"), obj(dict(device=positive, inode=positive))]),
        cleaned=array(dig, 3), cleanup_pending=dict(anyOf=[dict(type="null"), dig]),
        progress=array({"enum": ["push", "post", "build", "worktree", "branch", "remote"]}, 6)))
    doc("scenario-impl-state", dict(config_digest=dig, checksum=dig, records={"type": "object", "maxProperties": IMPL_MAX_RECORDS,
        "propertyNames": dig, "additionalProperties": rec}))
    return schemas

if __name__ == "__main__":
    try:
        main()
    except (Refused, OSError, ValueError, TypeError, KeyError, RecursionError, subprocess.SubprocessError):
        sys.stderr.write("Implementation adapter refused input or machine state; retain receipts and owned workspace\n")
        sys.exit(1)
