"""Release-performance handoff from completed upstream Release runs (package p6b, ADR-0180).

`.github/workflows/performance-release.yml` runs on every completed run of the repository's
`Release` workflow and on an explicit remeasurement dispatch. Its handoff job is the only job of
the laboratory that holds a token, the workflow's own read-only `GITHUB_TOKEN` (contents and
Actions read); it reads, never writes, through this module, and nothing here downloads, imports
or executes an upstream artifact. The measurement itself is the unchanged continuous workflow,
called with only `contents: read` and no token, and builds the published tag from Git.

**Eligibility** (`verify`). A completed upstream run is eligible only when every identity
agrees:

- the run is of the repository's own `.github/workflows/release.yml`, bound by the workflow ID
  the API gives that path, never by its display name; another workflow named `Release`, or one
  with a job named `sign and publish`, supplies no provenance;
- the run, the event's repository, its head repository and every job listed name this
  repository, the run's ID and head SHA, and each job its own attempt;
- the run is a push to `main` that tagged its commit, or a push of a release tag
  `vX.Y.Z[-rc.N]`; a main push without a release tag on its commit is not eligible;
- the tag names the run's head SHA both in the checkout and through the API, and its release
  is published and not a draft, its prerelease flag the tag's;
- the `sign and publish` job succeeded in some attempt. Every attempt's jobs are read through
  bounded, complete pagination, so a later pin-only retry that failed never hides an earlier
  successful publication. A listing that hits its bound or changes while it is read is
  `incomplete`, never taken as success or as absence.

**Deduplication.** The immutable identity is the upstream run ID, the tag and the SHA, never
the attempt; its canonical serialization's SHA-256 is the claim key. Before any sample the
handoff writes the claim `af.perf-release-claim/1` as the artifact `af-release-claim-<key>`,
then reads the claim history again: only the first valid claim measures. A valid claim was
written by this repository's `performance-release.yml` from `main` and holds exactly this
identity. An artifact of that name from another repository, branch or workflow is recorded and
ignored; one this handoff workflow wrote that is unreadable, oversized or of another identity is
trusted evidence that cannot be read, so the history is unavailable and nothing runs, never
taken as the absence of a claim. A later run of the same
identity (a pin-only rerun, a duplicate race) links the first claim's retained run bundles; a
claim without a result is `incomplete` and is never measured again automatically. History that
cannot be read completely, or that may have expired (an upstream run older than the claims'
retention, or an expired claim), runs nothing. An explicit remeasurement (`remeasure_of`, the
parent claim's key) verifies the same identity and its parent claim, then writes its own
`af-release-remeasure-<key>-<run>` claim and measures anew; the parent claim stays as it is.

The claim names (`af-release-*`) are not the collector's `af-perf-*` transport artifacts, so
the archive's fetch contract is unchanged. Release results stay advisory for publication.
"""
import io
import re
import stat
import time
import zipfile
from datetime import datetime

from . import contracts, transport
from .values import ContractError, require

RELEASE_WORKFLOW_FILE = "release.yml"
RELEASE_WORKFLOW_PATH = contracts.RELEASE_WORKFLOW_PATH
HANDOFF_WORKFLOW_PATH = contracts.RELEASE_HANDOFF_WORKFLOW_PATH
PUBLISH_JOB = "sign and publish"
PIN_JOB = "pin the release in .af/af.lock"
MAIN = "main"
# The tags release.yml publishes: vX.Y.Z and vX.Y.Z-rc.N.
RELEASE_TAG = contracts.RELEASE_TAG.compiled
FULL_SHA = re.compile(r"^[0-9a-f]{40}$")
DIGEST = re.compile(r"^sha256:[0-9a-f]{64}$")
CLAIM_FILE = "claim.json"
HANDOFF_FILE = "handoff.json"
CONFIRM_FILE = "confirm.json"
TOKEN_ENV = "AF_PERF_RELEASE_TOKEN"
# Bounds of every listing: attempts of one upstream run, pages of one attempt's jobs, pages of
# one claim name and the claims considered. Reaching one is incomplete, never a conclusion.
ATTEMPTS_MAX = contracts.RELEASE_ATTEMPTS_MAX
JOB_PAGES_MAX = 3
CLAIM_PAGES_MAX = 2
CLAIMS_MAX = contracts.RELEASE_CLAIMS_MAX
RESULTS_MAX = contracts.RELEASE_RESULTS_MAX
TAG_HOPS_MAX = 2
CLAIM_BYTES_MAX = 64 * 1024
CLAIM_ARTIFACT_BYTES_MAX = 1024 * 1024
# Claims are retained as long as the workflow's other non-PR artifacts; an upstream run older
# than that, less a day's margin, may have had its claims expire, so its history is expired.
CLAIM_RETENTION_DAYS = 90
HISTORY_DAYS = CLAIM_RETENTION_DAYS - 1
DAY_MS = 24 * 60 * 60 * 1000
# Decisions: a claim written and awaiting confirmation; the first valid claim, measured; a
# duplicate linked to the first claim's retained result; and why nothing runs otherwise.
STATUSES = contracts.RELEASE_STATUSES
EXIT_CODES = {"claimed": 0, "measure": 0, "linked": 0, "not_eligible": 0, "incomplete": 4,
              "refused": 4}


class Decided(Exception):
    """The handoff's decision when it is not to measure: a status of `STATUSES` and why."""

    def __init__(self, status, reason, detail):
        Exception.__init__(self, detail)
        self.status = status
        self.reason = reason
        self.detail = detail[:4096]


class Incomplete(ContractError):
    """A listing that reached its bound or changed while it was read."""


# The claim key, the SHA-256 of the identity's canonical serialization (never the attempt), and
# the claim artifact names: `af-release-claim-<key>` and `af-release-remeasure-<key>-<run>`.
identity_key = contracts.release_identity_key
claim_artifact = contracts.release_claim_artifact


def lab_run_prefix(actions_run_id, actions_run_attempt):
    """The prefix of every laboratory run ID the claiming Actions run plans (`ci.schedule`)."""
    return "gh%d_a%d_" % (actions_run_id, actions_run_attempt)


def unix_ms(text, what):
    require(isinstance(text, str), "%s has no timestamp" % what)
    try:
        moment = datetime.strptime(text, "%Y-%m-%dT%H:%M:%SZ")
    except ValueError:
        raise ContractError("%s: %r is not a UTC timestamp" % (what, text))
    return int((moment - datetime(1970, 1, 1)).total_seconds() * 1000)


# Reading the API ------------------------------------------------------------------------------

class Api(transport.Collector):
    """Bounded read-only requests to the configured repository's API with the handoff's token,
    every response within the collector's own time, byte and redirect bounds (ADR-0173)."""

    def __init__(self, transport_, repository, token, api_url=transport.API_URL,
                 clock=time.monotonic, seconds=10 * 60):
        transport.Collector.__init__(self, transport_, repository, token, api_url, clock,
                                     seconds)

    def get(self, path, what, missing=False):
        """The JSON object at the repository's API `path`; None for 404 when `missing`."""
        url = "%s/repos/%s%s" % (self.api, self.repository, "/" + path if path else "")
        response = self._get(url, transport.PAGE_BYTES_MAX, True)
        if missing and response.status == 404:
            return None
        if response.status != 200:
            # An answer that is not the document is the API being unavailable, never a fact.
            raise transport.TransportError("%s answered HTTP %d" % (what, response.status))
        doc = self._json(response, what)
        require(isinstance(doc, dict), "%s is not a JSON object" % what)
        return doc

    def listing(self, path, key, what, pages_max):
        """Every item of a paginated listing, read completely within `pages_max` pages: its
        total must hold on every page and be reached exactly, or the listing is incomplete."""
        found, total = [], None
        for page in range(1, pages_max + 1):
            doc = self.get("%s%sper_page=%d&page=%d" % (path, "&" if "?" in path else "?",
                                                        transport.PAGE_SIZE, page), what)
            items, count = doc.get(key), doc.get("total_count")
            require(isinstance(items, list) and type(count) is int and count >= 0,
                    "%s has no %s and total_count" % (what, key))
            if total is None:
                total = count
            if count != total:
                raise Incomplete("%s changed from %d to %d items while it was read"
                                 % (what, total, count))
            found.extend(items)
            if len(found) >= total or not items:
                break
        if len(found) < total:
            raise Incomplete("%s names %d items; %d pages of %d read only %d"
                             % (what, total, pages_max, transport.PAGE_SIZE, len(found)))
        require(len(found) == total, "%s returned %d items for a total of %d"
                % (what, len(found), total))
        return found

    def tag_commit(self, tag):
        """The commit the repository's tag names through the API, an annotated tag peeled; None
        when there is no such tag."""
        ref = self.get("git/ref/tags/%s" % tag, "the tag ref", missing=True)
        if ref is None:
            return None
        require(ref.get("ref") == "refs/tags/" + tag, "the tag ref names another ref")
        target = ref.get("object") or {}
        for _ in range(TAG_HOPS_MAX + 1):
            kind, sha = target.get("type"), target.get("sha")
            require(isinstance(sha, str) and FULL_SHA.match(sha) is not None,
                    "the tag object has no full SHA")
            if kind == "commit":
                return sha
            require(kind == "tag", "the tag names a %r, not a commit" % kind)
            target = (self.get("git/tags/%s" % sha, "the annotated tag") or {}).get(
                "object") or {}
        raise ContractError("the tag %s is nested more than %d deep" % (tag, TAG_HOPS_MAX))


# Eligibility ----------------------------------------------------------------------------------

def _int(value, what):
    require(type(value) is int and 1 <= value <= 2 ** 53, "%s is not a positive ID" % what)
    return value


def _upstream(payload):
    """The completed upstream run an event names, its fields checked; refused when any is
    missing or malformed."""
    run = payload.get("workflow_run") if isinstance(payload, dict) else None
    try:
        require(isinstance(run, dict), "the event has no workflow_run")
        out = {"run_id": _int(run.get("id"), "workflow_run.id"),
               "run_attempt": _int(run.get("run_attempt"), "workflow_run.run_attempt"),
               "workflow_id": _int(run.get("workflow_id"), "workflow_run.workflow_id")}
        for key in ("path", "head_branch", "event", "status", "head_sha"):
            require(isinstance(run.get(key), str) and run[key],
                    "workflow_run.%s is missing" % key)
        require(FULL_SHA.match(run["head_sha"]) is not None,
                "workflow_run.head_sha is not a full commit SHA")
        for key in ("repository", "head_repository"):
            require(isinstance(run.get(key), dict)
                    and isinstance(run[key].get("full_name"), str),
                    "workflow_run.%s has no full_name" % key)
    except ContractError as error:
        raise Decided("refused", "invalid_event", str(error))
    out.update(workflow_path=run["path"][:256], event=run["event"][:64],
               head_branch=run["head_branch"][:256], head_sha=run["head_sha"],
               status=run["status"][:64], conclusion=(run.get("conclusion") or None),
               repository=run["repository"]["full_name"][:201],
               head_repository=run["head_repository"]["full_name"][:201],
               name=(run.get("name") or "")[:256])
    if isinstance(out["conclusion"], str):
        out["conclusion"] = out["conclusion"][:64]
    else:
        out["conclusion"] = None
    return out


def _same(actual, expected, what):
    if actual != expected:
        raise Decided("refused", "identity_mismatch", "%s: the API says %r, the event %r; "
                      "nothing is concluded from disagreeing identities"
                      % (what, actual, expected))


def _release_tag(git, upstream):
    """The release tag of the run's head SHA: the pushed tag itself, or the one release tag on a
    main push's commit; a main push without one is not eligible."""
    branch, sha = upstream["head_branch"], upstream["head_sha"]
    tags = [(name, commit) for name, commit in git.tags() if RELEASE_TAG.match(name)]
    if RELEASE_TAG.match(branch):
        found = [commit for name, commit in tags if name == branch]
        if not found:
            raise Decided("not_eligible", "no_published_tag", "the pushed tag %s is not in the "
                          "checkout" % branch)
        if found[0] != sha:
            raise Decided("refused", "tag_mismatch", "tag %s names %s, not the run's head %s"
                          % (branch, found[0], sha))
        return branch
    if branch != MAIN:
        raise Decided("not_eligible", "not_release_ref", "the run pushed %s, neither %s nor a "
                      "release tag" % (branch, MAIN))
    on_head = sorted(name for name, commit in tags if commit == sha)
    if not on_head:
        raise Decided("not_eligible", "no_published_tag", "the %s push %s has no release tag; "
                      "only a published release is measured" % (MAIN, sha))
    if len(on_head) > 1:
        raise Decided("refused", "ambiguous_release_tag", "the %s push %s carries the release "
                      "tags %s" % (MAIN, sha, ", ".join(on_head)))
    return on_head[0]


def _publication(api, upstream, latest):
    """Every attempt's jobs, through complete bounded pagination: the attempt whose `sign and
    publish` job succeeded first, and each attempt's publish and pin outcomes."""
    if latest > ATTEMPTS_MAX:
        raise Decided("incomplete", "jobs_incomplete", "the run has %d attempts; at most %d are "
                      "read, so publication is not concluded" % (latest, ATTEMPTS_MAX))
    attempts, published = [], None
    for attempt in range(1, latest + 1):
        try:
            jobs = api.listing("actions/runs/%d/attempts/%d/jobs" % (upstream["run_id"],
                                                                     attempt),
                               "jobs", "attempt %d's jobs" % attempt, JOB_PAGES_MAX)
        except Incomplete as error:
            raise Decided("incomplete", "jobs_incomplete", str(error))
        record = {"attempt": attempt, "jobs": len(jobs), "publish": None, "pin": None}
        ids = set()
        for job in jobs:
            require(isinstance(job, dict), "a job entry is not an object")
            _same(job.get("run_id"), upstream["run_id"], "a job's run ID")
            _same(job.get("run_attempt"), attempt, "a job's attempt")
            _same(job.get("head_sha"), upstream["head_sha"], "a job's head SHA")
            if "workflow_name" in job and upstream["name"]:
                _same(job["workflow_name"], upstream["name"], "a job's workflow name")
            require(type(job.get("id")) is int and job["id"] not in ids,
                    "attempt %d lists a job without an ID or twice" % attempt)
            ids.add(job["id"])
            for name, field in ((PUBLISH_JOB, "publish"), (PIN_JOB, "pin")):
                if job.get("name") != name:
                    continue
                if record[field] is not None:
                    raise Decided("refused", "ambiguous_job", "attempt %d lists two jobs named "
                                  "%r" % (attempt, name))
                conclusion = job.get("conclusion") if job.get("status") == "completed" \
                    else job.get("status")
                record[field] = (conclusion if isinstance(conclusion, str)
                                 else "unknown")[:64]
        if record["publish"] == "success" and published is None:
            published = attempt
        attempts.append(record)
    return {"attempts": attempts, "published_in_attempt": published}


def verify(api, git, payload, repository):
    """(upstream, publication, release) of an eligible completed Release run; `Decided` with
    why it is not eligible, refused or incomplete otherwise."""
    upstream = _upstream(payload)
    if upstream["status"] != "completed":
        raise Decided("not_eligible", "not_completed", "the upstream run is %s"
                      % upstream["status"])
    if upstream["repository"] != repository or upstream["head_repository"] != repository:
        raise Decided("not_eligible", "wrong_repository", "the upstream run is of %s from %s, "
                      "not of %s" % (upstream["repository"], upstream["head_repository"],
                                     repository))
    workflow = api.get(
        "actions/workflows/%s" % RELEASE_WORKFLOW_FILE, "the release workflow", missing=True)
    if workflow is None or workflow.get("path") != RELEASE_WORKFLOW_PATH:
        raise Decided("not_eligible", "wrong_workflow", "the repository has no %s workflow"
                      % RELEASE_WORKFLOW_PATH)
    if upstream["workflow_path"] != RELEASE_WORKFLOW_PATH \
            or upstream["workflow_id"] != workflow.get("id"):
        raise Decided("not_eligible", "wrong_workflow", "the upstream run is of workflow %d "
                      "at %s, not the repository's %s (workflow %r); a name supplies no "
                      "provenance" % (upstream["workflow_id"], upstream["workflow_path"],
                                      RELEASE_WORKFLOW_PATH, workflow.get("id")))
    run = api.get("actions/runs/%d" % upstream["run_id"], "the upstream run")
    _same(run.get("id"), upstream["run_id"], "the run ID")
    _same(run.get("workflow_id"), upstream["workflow_id"], "the run's workflow")
    _same(run.get("path"), upstream["workflow_path"], "the run's workflow path")
    _same(run.get("head_sha"), upstream["head_sha"], "the run's head SHA")
    _same(run.get("head_branch"), upstream["head_branch"], "the run's head branch")
    _same(run.get("event"), upstream["event"], "the run's event")
    _same((run.get("repository") or {}).get("full_name"), repository, "the run's repository")
    _same((run.get("head_repository") or {}).get("full_name"), repository,
          "the run's head repository")
    latest = run.get("run_attempt")
    require(type(latest) is int and latest >= upstream["run_attempt"],
            "the run's latest attempt precedes the event's")
    upstream["latest_attempt"] = latest
    upstream["created_unix_ms"] = unix_ms(run.get("created_at"), "the run's created_at")
    if upstream["event"] != "push":
        raise Decided("not_eligible", "not_push", "the upstream run was started by %s, not a "
                      "push" % upstream["event"])
    tag = _release_tag(git, upstream)
    commit = api.tag_commit(tag)
    if commit is None:
        raise Decided("not_eligible", "no_published_tag", "the repository has no tag %s" % tag)
    if commit != upstream["head_sha"]:
        raise Decided("refused", "tag_mismatch", "the repository's tag %s names %s, not the "
                      "run's head %s" % (tag, commit, upstream["head_sha"]))
    found = api.get("releases/tags/%s" % tag, "the release", missing=True)
    if found is None:
        raise Decided("not_eligible", "not_published", "tag %s has no published release" % tag)
    _same(found.get("tag_name"), tag, "the release's tag")
    if found.get("draft") is not False:
        raise Decided("not_eligible", "draft_release", "the release of %s is a draft" % tag)
    published_at = found.get("published_at")
    if not isinstance(published_at, str):
        raise Decided("not_eligible", "not_published", "the release of %s was never "
                      "published" % tag)
    prerelease = RELEASE_TAG.match(tag).group(4) is not None
    if found.get("prerelease") is not prerelease:
        raise Decided("refused", "prerelease_mismatch", "the release of %s says prerelease %r; "
                      "its tag says %r" % (tag, found.get("prerelease"), prerelease))
    publication = _publication(api, upstream, latest)
    if publication["published_in_attempt"] is None:
        raise Decided("not_eligible", "publish_not_successful", "no attempt's %r job succeeded"
                      % PUBLISH_JOB)
    release = {"tag": tag, "sha": upstream["head_sha"], "id": _int(found.get("id"),
                                                                    "the release ID"),
               "prerelease": prerelease, "published_at": published_at[:64]}
    return upstream, publication, release


# Claims and their history ---------------------------------------------------------------------

def read_claim_zip(data):
    """The claim document of one downloaded claim artifact: a ZIP holding exactly `claim.json`,
    a regular, unencrypted member within its bound."""
    require(len(data) <= CLAIM_ARTIFACT_BYTES_MAX and data[:4] == b"PK\x03\x04",
            "the claim artifact is not a bounded ZIP container")
    try:
        container = zipfile.ZipFile(io.BytesIO(data))
        infos = container.infolist()
        require([info.filename for info in infos] == [CLAIM_FILE],
                "the claim artifact holds %s, not exactly %s"
                % (sorted(info.filename for info in infos)[:4], CLAIM_FILE))
        info = infos[0]
        require(stat.S_IFMT(info.external_attr >> 16) in (0, stat.S_IFREG)
                and not info.flag_bits & 0x1 and info.file_size <= CLAIM_BYTES_MAX,
                "the claim member is not a bounded regular unencrypted file")
        with container.open(info) as stream:
            body = stream.read(CLAIM_BYTES_MAX + 1)
    except (zipfile.BadZipFile, zipfile.LargeZipFile, ValueError, OSError) as error:
        raise ContractError("the claim artifact is not readable: %s" % error)
    require(len(body) == info.file_size, "the claim member is not its declared size")
    return contracts.read_json(body, "release-claim", CLAIM_FILE)


class History(object):
    """The claims of one name, each checked: (state, detail, entries, the valid claim docs in
    claim order)."""

    def __init__(self, state, detail, entries, valid):
        self.state = state
        self.detail = detail[:4096]
        self.entries = entries
        self.valid = valid

    def first(self):
        return self.valid[0] if self.valid else None


def read_history(api, name, key, identity, repository_id, kind="release"):
    """Every artifact named `name` in the repository, each a valid claim only when this
    repository's handoff workflow wrote it from `main` and it holds exactly `key`, `identity`
    and claim `kind`. An artifact of another repository, branch or workflow is foreign and
    ignored once its producer is established. Unreadable or incomplete history is
    `unavailable`, and so is history holding a trusted producer's artifact of this name that is
    oversized, unreadable or of another identity: it may be a claim, so it never proves that
    none exists. A trusted claim that expired makes it `expired`."""
    entries, valid, ambiguous = [], [], []
    try:
        listed = api.listing("actions/artifacts?name=%s" % name, "artifacts",
                             "the claim history", CLAIM_PAGES_MAX)
        if len(listed) > CLAIMS_MAX:
            raise Incomplete("the claim history holds %d claims; at most %d are read"
                             % (len(listed), CLAIMS_MAX))
        expired = False
        for item in listed:
            require(isinstance(item, dict), "a claim entry is not an object")
            run = item.get("workflow_run") or {}
            entry = {"artifact_id": _int(item.get("id"), "the claim's artifact ID"),
                     "actions_run_id": _int(run.get("id"), "the claim's run ID"),
                     "created_unix_ms": unix_ms(item.get("created_at"), "the claim's created_at"),
                     "expired": item.get("expired") is True, "valid": False, "reason": None}
            entries.append(entry)
            if item.get("name") != name:
                entry["reason"] = "other_name"
                continue
            if run.get("repository_id") != repository_id \
                    or run.get("head_repository_id") != repository_id \
                    or run.get("head_branch") != MAIN:
                entry["reason"] = "foreign_run"
                continue
            producer = api.get("actions/runs/%d" % entry["actions_run_id"], "a claim's run")
            if producer.get("path") != HANDOFF_WORKFLOW_PATH \
                    or (producer.get("repository") or {}).get("full_name") != api.repository \
                    or producer.get("head_branch") != MAIN:
                entry["reason"] = "foreign_workflow"
                continue
            if entry["expired"]:
                entry["reason"] = "expired"
                expired = True
                continue
            if _size(item) > CLAIM_ARTIFACT_BYTES_MAX:
                entry["reason"] = "oversized"
                ambiguous.append(entry)
                continue
            data = api.download({"id": entry["artifact_id"], "size": _size(item)})
            if data is None:
                entry.update(expired=True, reason="expired")
                expired = True
                continue
            try:
                claim = read_claim_zip(data)
            except ContractError:
                entry["reason"] = "unreadable"
                ambiguous.append(entry)
                continue
            if claim["key"] != key or claim["identity"] != identity or claim["kind"] != kind \
                    or claim["claimant"]["actions_run_id"] != entry["actions_run_id"]:
                entry["reason"] = "other_identity"
                ambiguous.append(entry)
                continue
            entry["valid"] = True
            valid.append((entry["created_unix_ms"], entry["artifact_id"], entry, claim))
    except (Incomplete, ContractError) as error:
        return History("unavailable", str(error), entries, [])
    valid.sort(key=lambda item: (item[0], item[1]))
    claims = [(entry, claim) for _, _, entry, claim in valid]
    if ambiguous:
        return History("unavailable", "%d artifact(s) named %s from this repository's handoff "
                       "workflow on main are %s; whether they claimed this identity cannot be "
                       "read" % (len(ambiguous), name, ", ".join(sorted(set(
                           entry["reason"] for entry in ambiguous)))), entries, claims)
    if expired:
        return History("expired", "a claim of this identity expired; whether it was measured "
                       "can no longer be read", entries, claims)
    return History("complete", "%d of %d claims named %s are valid" % (len(claims),
                                                                       len(entries), name),
                   entries, claims)


def _size(item):
    size = item.get("size_in_bytes")
    require(type(size) is int and size >= 0, "a claim entry has no size")
    return size


def results(api, claim):
    """The first claim's retained run bundles, `af-perf-<its runs>-run`: [(artifact)] and
    whether any of them expired."""
    prefix = claim["claimant"]["lab_run_prefix"]
    pattern = re.compile(r"^af-perf-%s[a-z0-9_]{1,40}-run$" % re.escape(prefix))
    listed = api.artifacts(claim["claimant"]["actions_run_id"])
    found = [{"artifact_id": item["id"], "name": item["name"], "expired": item["expired"]}
             for item in listed if pattern.match(item["name"])]
    return sorted(found, key=lambda item: item["artifact_id"])[:RESULTS_MAX]


# The handoff ----------------------------------------------------------------------------------

class Actions(object):
    """The downstream run: repository, run ID and attempt, event and SHA."""

    def __init__(self, repository, run_id, run_attempt, event, sha):
        contracts.REPOSITORY.validate(repository, "--repository")
        self.repository = repository
        self.run_id = _int(run_id, "--run-id")
        self.run_attempt = _int(run_attempt, "--run-attempt")
        require(event in ("workflow_run", "workflow_dispatch"),
                "the handoff runs for a workflow_run or a remeasurement dispatch, not %s"
                % event)
        self.event = event
        require(isinstance(sha, str) and FULL_SHA.match(sha) is not None,
                "--sha: a full commit SHA")
        self.sha = sha

    def record(self):
        return {"repository": self.repository, "run_id": self.run_id,
                "run_attempt": self.run_attempt, "event": self.event, "sha": self.sha}


def _document(actions, stage, mode):
    return {"schema": contracts.RELEASE_HANDOFF_SCHEMA, "stage": stage, "mode": mode,
            "actions": actions.record(), "upstream": None, "publication": None,
            "release": None, "identity": None, "key": None,
            "history": {"state": "not_read", "detail": "", "claims": []},
            "status": "incomplete", "reason": None, "detail": "", "claim_artifact": None,
            "first_claim": None, "results": [], "enforcement": "advisory"}


def _decide(doc, status, reason, detail):
    doc.update(status=status, reason=reason, detail=detail[:4096])
    return doc


def _upstream_record(upstream):
    keys = ("run_id", "run_attempt", "latest_attempt", "workflow_id", "workflow_path", "event",
            "head_branch", "head_sha", "conclusion", "created_unix_ms")
    return dict((key, upstream.get(key)) for key in keys)


def _first_claim(entry, claim):
    return {"artifact_id": entry["artifact_id"],
            "actions_run_id": claim["claimant"]["actions_run_id"],
            "actions_run_attempt": claim["claimant"]["actions_run_attempt"],
            "lab_run_prefix": claim["claimant"]["lab_run_prefix"]}


def _record_history(doc, history):
    doc["history"] = {"state": history.state, "detail": history.detail,
                      "claims": history.entries[:CLAIMS_MAX]}


def _duplicate(api, doc, entry, claim, why):
    """Link a duplicate to the first claim's retained result, or say why it cannot be."""
    doc["first_claim"] = _first_claim(entry, claim)
    try:
        found = results(api, claim)
    except ContractError as error:
        return _decide(doc, "incomplete", "history_unavailable", "%s; the first claim's results "
                       "cannot be read: %s" % (why, error))
    doc["results"] = found
    live = [item for item in found if not item["expired"]]
    if live:
        return _decide(doc, "linked", None, "%s; it links run bundle %s of Actions run %d"
                       % (why, ", ".join(item["name"] for item in live),
                          claim["claimant"]["actions_run_id"]))
    if found:
        return _decide(doc, "incomplete", "result_expired", "%s; the first claim's results "
                       "expired" % why)
    return _decide(doc, "incomplete", "claim_without_result", "%s; the first claim, by Actions "
                   "run %d attempt %d, has no retained result. A claim without a result is "
                   "never measured again automatically; an explicit remeasure_of is"
                   % (why, claim["claimant"]["actions_run_id"],
                      claim["claimant"]["actions_run_attempt"]))


def _claim(actions, mode, upstream, publication, release, identity, key, wall, parent=None):
    doc = {"schema": contracts.RELEASE_CLAIM_SCHEMA, "kind": mode, "key": key,
           "identity": identity,
           "upstream": {"workflow_id": upstream["workflow_id"],
                        "workflow_path": RELEASE_WORKFLOW_PATH,
                        "run_attempt": upstream["run_attempt"],
                        "published_in_attempt": publication["published_in_attempt"]},
           "release": {"id": release["id"], "prerelease": release["prerelease"],
                       "published_at": release["published_at"]},
           "claimant": {"repository": actions.repository,
                        "workflow_path": HANDOFF_WORKFLOW_PATH,
                        "actions_run_id": actions.run_id,
                        "actions_run_attempt": actions.run_attempt,
                        "lab_run_prefix": lab_run_prefix(actions.run_id, actions.run_attempt)},
           "remeasure_of": parent, "claimed_unix_ms": int(wall() * 1000)}
    contracts.validate_release_claim(doc)
    return doc


def handoff(api, git, actions, payload=None, remeasure=None, wall=time.time):
    """(handoff document, claim document or None) of one downstream run, before any sample.
    `payload` is a workflow_run event's; `remeasure` a dispatch's (upstream run ID, parent claim
    key). A claim is returned only when this run must write it and then confirm it."""
    mode = "remeasure" if remeasure is not None else "release"
    doc = _document(actions, "handoff", mode)
    try:
        if mode == "remeasure":
            run_id, parent_key = remeasure
            require(actions.event == "workflow_dispatch", "a remeasurement is dispatched")
            if not (isinstance(parent_key, str) and DIGEST.match(parent_key) is not None):
                raise Decided("refused", "invalid_dispatch", "remeasure_of: the parent claim's "
                              "key, sha256:<64 hex>")
            if not (isinstance(run_id, int) and run_id > 0):
                raise Decided("refused", "invalid_dispatch", "release_run: the upstream Release "
                              "run ID")
            payload = {"workflow_run": api.get("actions/runs/%d" % run_id, "the upstream run")}
        else:
            require(actions.event == "workflow_run", "a release handoff is a workflow_run")
        repository_doc = api.get("", "the repository")
        repository_id = _int(repository_doc.get("id"), "the repository ID")
        _same(repository_doc.get("full_name"), actions.repository, "the repository")
        upstream, publication, release = verify(api, git, payload, actions.repository)
        doc.update(upstream=_upstream_record(upstream), publication=publication,
                   release=release)
        identity = {"repository": actions.repository, "upstream_run_id": upstream["run_id"],
                    "tag": release["tag"], "sha": release["sha"]}
        key = identity_key(identity)
        doc.update(identity=identity, key=key)
        if mode == "remeasure" and key != parent_key:
            raise Decided("refused", "remeasure_mismatch", "remeasure_of %s is not the claim "
                          "key %s of this upstream run, tag and SHA" % (parent_key, key))
        now = int(wall() * 1000)
        if now - upstream["created_unix_ms"] > HISTORY_DAYS * DAY_MS:
            doc["history"]["detail"] = "not read: older than the claims' retention"
            raise Decided("incomplete", "history_expired", "the upstream run is older than "
                          "the %d days its claims are retained; whether it was measured cannot "
                          "be read, so nothing runs" % CLAIM_RETENTION_DAYS)
        history = read_history(api, claim_artifact(key), key, identity, repository_id)
        _record_history(doc, history)
        if history.state == "unavailable":
            raise Decided("incomplete", "history_unavailable", "the claim history cannot be "
                          "read completely, so nothing runs: " + history.detail)
        if history.state == "expired":
            raise Decided("incomplete", "history_expired", history.detail + ", so nothing runs")
        first = history.first()
        parent = None
        if mode == "release" and first is not None:
            return _duplicate(api, doc, first[0], first[1], "this release was already claimed"
                              ), None
        if mode == "remeasure":
            if first is None:
                raise Decided("refused", "parent_claim_unavailable", "no valid claim %s "
                              "exists to remeasure" % parent_key)
            entry, claim = first
            doc["first_claim"] = _first_claim(entry, claim)
            parent = {"key": key, "claim_artifact_id": entry["artifact_id"],
                      "actions_run_id": claim["claimant"]["actions_run_id"],
                      "lab_run_id": claim["claimant"]["lab_run_prefix"] + "primary"}
        claim = _claim(actions, mode, upstream, publication, release, identity, key, wall,
                       parent)
        doc["claim_artifact"] = claim_artifact(key, mode, actions.run_id)
        _decide(doc, "claimed", None, "%s %s (%s) of upstream run %d is claimed as %s; it is "
                "measured only once the claim is confirmed first"
                % ("an explicit remeasurement of" if parent else "the published release",
                   release["tag"], release["sha"], upstream["run_id"], doc["claim_artifact"]))
        return contracts.validate_release_handoff(doc), claim
    except Decided as decided:
        return contracts.validate_release_handoff(
            _decide(doc, decided.status, decided.reason, decided.detail)), None
    except transport.TransportError as error:
        return contracts.validate_release_handoff(
            _decide(doc, "incomplete", "api_unavailable", "the API could not be read: %s"
                    % error)), None
    except ContractError as error:
        return contracts.validate_release_handoff(
            _decide(doc, "refused", "invalid_response", str(error))), None


def confirm(api, handoff_doc, claim):
    """The confirmation after the claim was written: the first valid claim of its name must be
    this run's own, or this run measures nothing and links or reports the first claim."""
    require(handoff_doc["status"] == "claimed", "only a claimed handoff is confirmed")
    actions = handoff_doc["actions"]
    require(claim["claimant"]["actions_run_id"] == actions["run_id"]
            and claim["claimant"]["actions_run_attempt"] == actions["run_attempt"]
            and claim["key"] == handoff_doc["key"],
            "the claim is not this run's own claim of this identity")
    doc = dict(handoff_doc, stage="confirm", history={"state": "not_read", "detail": "",
                                                       "claims": []}, results=[],
               first_claim=handoff_doc["first_claim"])
    try:
        repository_id = _int(api.get("", "the repository").get("id"), "the repository ID")
        history = read_history(api, handoff_doc["claim_artifact"], claim["key"],
                               claim["identity"], repository_id, claim["kind"])
        _record_history(doc, history)
        if history.state != "complete":
            raise Decided("incomplete", "history_%s" % history.state, "the claim was written, "
                          "but its history cannot be confirmed (%s), so nothing runs; the "
                          "claim stays without a result" % history.detail)
        first = history.first()
        own = first is not None and first[1]["claimant"]["actions_run_id"] == actions[
            "run_id"] and first[1]["claimant"]["actions_run_attempt"] == actions["run_attempt"]
        if first is not None and not own:
            return contracts.validate_release_handoff(_duplicate(
                api, doc, first[0], first[1], "another run claimed this release first"))
        if not own:
            raise Decided("incomplete", "claim_unconfirmed", "this run's claim %s is not in its "
                          "history yet, so nothing runs; the claim stays without a result"
                          % handoff_doc["claim_artifact"])
        if claim["kind"] == "release":
            doc["first_claim"] = _first_claim(first[0], first[1])
        return contracts.validate_release_handoff(_decide(
            doc, "measure", None, "this run's claim %s is the first valid claim; it measures "
            "%s advisory, never gating publication" % (handoff_doc["claim_artifact"],
                                                       handoff_doc["release"]["tag"])))
    except Decided as decided:
        return contracts.validate_release_handoff(
            _decide(doc, decided.status, decided.reason, decided.detail))
    except transport.TransportError as error:
        return contracts.validate_release_handoff(
            _decide(doc, "incomplete", "api_unavailable", "the API could not be read: %s"
                    % error))


def outputs(doc):
    """The job outputs the workflow reads: whether to write a claim, its artifact name, and
    whether to measure."""
    return {"claim": "true" if doc["status"] == "claimed" else "false",
            "claim_artifact": doc["claim_artifact"] or "",
            "measure": "true" if doc["status"] == "measure" else "false"}


def summary_markdown(doc):
    from . import render
    upstream = doc["upstream"] or {}
    release = doc["release"] or {}
    lines = ["### AF release performance handoff (%s): %s" % (
        render.markdown_cell(doc["stage"]), render.markdown_cell(
            doc["status"] if doc["reason"] is None else "%s (%s)" % (doc["status"],
                                                                    doc["reason"]))),
        "", render.markdown_cell(doc["detail"]), "",
        "| Upstream run | Tag | SHA | Published in attempt | Claim | History |",
        "| --- | --- | --- | --- | --- | --- |",
        "| %s |" % " | ".join(render.markdown_cell(str(cell)) for cell in (
            upstream.get("run_id", "-"), release.get("tag", "-"), release.get("sha", "-"),
            (doc["publication"] or {}).get("published_in_attempt", "-"),
            doc["claim_artifact"] or "-", doc["history"]["state"])), "",
        render.markdown_cell("Release performance is advisory: it never gates publication."),
        ""]
    return "\n".join(lines)
