"""Collect a run's evidence from GitHub Actions artifacts into an archive (package p4,
ADR-0172).

`archive --fetch ACTIONS_RUN_ID --root DIR --repository OWNER/NAME` is the trusted collector:
it holds the only credential, a token with read access to the repository's contents and Actions
artifacts, named by `--token-env` and never written anywhere. Benchmarks never receive it: no
sample, build or shard environment is built from the collector's.

Every API path is built from the configured repository and validated numeric IDs; nothing an
artifact or evidence names becomes a URL. The token goes only to the configured API origin. An
artifact download answers with a redirect to signed storage; it is followed by this module's own
bounded handling, never by a library's, and the request to any other origin carries no
Authorization, cookie or other credential. Signed locations are never logged or recorded. Pages,
redirect hops, response bytes, every request's time and the whole collection's time are bounded.
Each request's time is one absolute deadline, the earlier of its own bound and the collection's,
over its whole exchange: the connection, TLS, headers and every body byte, however slowly a
server sends them; a request still running then is shut down and refused, and a response that
completes after it is refused too (ADR-0173). Redirect hops share the collection's deadline.

An artifact is transport, never code: its ZIP container is read with bounded decompression
(members, each member's and all members' bytes, and the compression ratio), refusing encrypted,
symlinked, duplicate, absolute or traversing members, and it must declare the explicit
`af.perf-transport/1` format in `af-perf-transport.json`; any other format refuses clearly.
Transported HTML or SVG views are kept only as data. A missing or expired artifact leaves the
run's retained evidence incomplete, which its verdict and the archive's status say.
"""
import http.client
import io
import json
import os
import re
import secrets
import socket
import ssl
import stat
import threading
import time
import zipfile
import zlib
from datetime import datetime
from urllib.parse import urlsplit

from . import admission, archive, bundle, compare, contracts, limits
from .values import ContractError, check_relative_path, require, sha256_bytes

API_URL = "https://api.github.com"
TOKEN_ENV = "AF_PERF_ARCHIVE_TOKEN"
ARTIFACT_PREFIX = "af-perf-"
ARTIFACT_NAME = re.compile(r"^af-perf-[A-Za-z0-9_.-]{1,120}$")
TRANSPORT_FILE = "af-perf-transport.json"
BUNDLE_PREFIX = "bundle/"
VIEWS_PREFIX = "views/"
PAGE_SIZE = 100
PAGES_MAX = 10
PAGE_BYTES_MAX = 4 * limits.MIB
REDIRECTS_MAX = 3
REQUEST_SECONDS = 60
COLLECTION_SECONDS = 30 * 60
# One artifact holds one bundle and its views: at most a shard bundle's bound and the views'.
VIEW_BYTES_MAX = 16 * limits.MIB
ARTIFACT_BYTES_MAX = limits.SHARD_BUNDLE_BYTES_MAX + 2 * VIEW_BYTES_MAX
ZIP_MEMBERS_MAX = 8192
# A member larger than this may not decompress to more than this many times its stored size.
RATIO_FLOOR_BYTES = limits.MIB
RATIO_MAX = 200
CHUNK = 1 << 20
# How long a request's worker is given to end once its deadline shut its connection down.
CLEANUP_SECONDS = 1.0


class TransportError(ContractError):
    """The transport failed; nothing was admitted from it."""


class Response(object):
    def __init__(self, status, headers, body):
        self.status = status
        self.headers = dict((key.lower(), value) for key, value in headers.items())
        self.body = body


def _origin(url, what):
    """(scheme, host, port) of an https URL; `what` names it, for no URL is ever echoed."""
    parts = urlsplit(url)
    require(parts.scheme == "https" and parts.hostname, "%s is not an https URL" % what)
    return (parts.scheme, parts.hostname.lower(), parts.port or 443)


def seconds_text(value):
    """`value` seconds as a diagnostic shows them: exact to the millisecond, never truncated."""
    return ("%.3f" % value).rstrip("0").rstrip(".")


class HttpsTransport(object):
    """One GET over TLS with certificate and host name verification (`context`, the system's
    trust by default), no proxy and no redirect following: the response status, headers and
    at most `limit` body bytes.

    `timeout` seconds from the call are one absolute deadline for the whole exchange
    (ADR-0173): the name lookup, the connection, the TLS handshake, the request, the headers,
    every body byte and the response's admission. Each blocking call is given only what is left
    of it, and the exchange runs in a worker the caller waits for only until it; then the
    connection is shut down, the worker is given `CLEANUP_SECONDS` to end, and the request is
    refused, however slowly the server was still sending. An abandoned exchange makes no
    further connection or request."""

    def __init__(self, context=None):
        self.context = context if context is not None else ssl.create_default_context()

    def request(self, url, headers, limit, timeout):
        deadline = time.monotonic() + timeout
        exchange = _Exchange(url, headers, limit, deadline, self.context)
        worker = threading.Thread(target=exchange.run, name="af-perf-collector-request")
        worker.daemon = True
        worker.start()
        worker.join(max(0.0, deadline - time.monotonic()))
        if exchange.abort():
            worker.join(CLEANUP_SECONDS)
            raise TransportError("a response did not complete within its %s-second bound%s"
                                 % (seconds_text(timeout), "" if not worker.is_alive()
                                    else "; its exchange was abandoned"))
        if exchange.error is not None:
            raise exchange.error
        if exchange.finished_at > deadline:
            raise TransportError("a response completed after its %s-second bound"
                                 % seconds_text(timeout))
        return exchange.response


class _Aborted(Exception):
    """The caller abandoned the exchange at its deadline."""


class _Exchange(object):
    """One request's calls, in its worker, and what the caller waiting for it can stop."""

    def __init__(self, url, headers, limit, deadline, context):
        self.url, self.headers, self.limit = url, headers, limit
        self.deadline, self.context = deadline, context
        self.lock = threading.Lock()
        self.aborted = self.finished = False
        self.sock = None
        self.response = self.error = None
        self.finished_at = None

    def abort(self):
        """Stop an unfinished exchange: its socket is shut down, so a blocked call returns.
        Whether it was unfinished."""
        with self.lock:
            if self.finished:
                return False
            self.aborted = True
            sock = self.sock
        if sock is not None:
            try:
                # The socket's own shutdown, never the TLS layer's, which another thread holds.
                socket.socket.shutdown(sock, socket.SHUT_RDWR)
            except OSError:
                pass
        return True

    def run(self):
        try:
            self.response = self._exchange()
        except _Aborted:
            pass
        except (TransportError, OSError, http.client.HTTPException) as error:
            self.error = error
        finally:
            with self.lock:
                self.finished = True
                self.finished_at = time.monotonic()
                sock, self.sock = self.sock, None
            if sock is not None:
                sock.close()

    def _hold(self, sock):
        """Make `sock` the one an abort shuts down, unless the exchange was abandoned."""
        with self.lock:
            if self.aborted:
                sock.close()
                raise _Aborted()
            self.sock = sock

    def _left(self, sock=None):
        """What is left of the deadline, as the next blocking call's timeout on `sock`."""
        left = self.deadline - time.monotonic()
        if left <= 0:
            raise TransportError("a response did not complete within its bound")
        if sock is not None:
            sock.settimeout(left)
        return left

    def _exchange(self):
        parts = urlsplit(self.url)
        host, port = parts.hostname, parts.port or 443
        self._left()
        addresses = socket.getaddrinfo(host, port, 0, socket.SOCK_STREAM)
        failure = OSError("no address for the host")
        for family, kind, protocol, _, address in addresses:
            raw = socket.socket(family, kind, protocol)
            self._hold(raw)
            try:
                self._left(raw)
                raw.connect(address)
                break
            except OSError as error:
                failure = error
                with self.lock:
                    self.sock = None
                raw.close()
        else:
            raise failure
        tls = self.context.wrap_socket(raw, server_hostname=host, do_handshake_on_connect=False)
        self._hold(tls)
        self._left(tls)
        tls.do_handshake()
        connection = http.client.HTTPSConnection(host, port, context=self.context)
        # The verified connection above; the library never opens one of its own.
        connection.sock = tls
        response = None
        try:
            path = parts.path + ("?" + parts.query if parts.query else "")
            self._left(tls)
            connection.request("GET", path, headers=self.headers)
            self._left(tls)
            response = connection.getresponse()
            chunks, size = [], 0
            while True:
                self._left(tls)
                chunk = response.read(min(CHUNK, self.limit + 1 - size))
                if not chunk:
                    break
                size += len(chunk)
                if size > self.limit:
                    raise TransportError("a response exceeded its %d-byte bound" % self.limit)
                chunks.append(chunk)
            return Response(response.status, dict(response.getheaders()), b"".join(chunks))
        finally:
            if response is not None:
                response.close()
            connection.close()


class Collector(object):
    """The trusted collector of one repository's Actions artifacts."""

    def __init__(self, transport, repository, token, api_url=API_URL, clock=time.monotonic,
                 seconds=COLLECTION_SECONDS):
        contracts.REPOSITORY.validate(repository, "--repository")
        owner, name = repository.split("/")
        require(owner not in (".", "..") and name not in (".", ".."),
                "--repository: OWNER/NAME")
        require(token and "\n" not in token and "\r" not in token,
                "the collector's token is empty or malformed")
        self.api = api_url.rstrip("/")
        self.origin = _origin(self.api, "--api-url")
        require(urlsplit(self.api).path in ("", "/api/v3") and not urlsplit(self.api).query,
                "--api-url: an API origin, such as https://api.github.com")
        self.transport = transport
        self.repository = repository
        self.token = token
        self.clock = clock
        self.seconds = seconds
        self.deadline = clock() + seconds

    def remaining(self, step):
        """Seconds left of the whole collection's bound; none left refuses `step`."""
        left = self.deadline - self.clock()
        if left <= 0:
            raise TransportError("the collection's %s-second bound ended before %s; nothing "
                                 "more was done" % (seconds_text(self.seconds), step))
        return left

    def _get(self, url, limit, authorized):
        """One request within what is left of the collection's deadline and its own bound; a
        response that arrives after the collection's deadline is refused (ADR-0173)."""
        response = self._request(url, limit, authorized)
        if self.clock() > self.deadline:
            raise TransportError("a response arrived after the collection's %s-second bound; "
                                 "it was not admitted" % seconds_text(self.seconds))
        return response

    def _request(self, url, limit, authorized):
        seconds = self.remaining("the next request")
        headers = {"Accept": "application/vnd.github+json",
                   "X-GitHub-Api-Version": "2022-11-28", "User-Agent": "af-perf-collector"}
        if authorized:
            # Only the configured API origin ever receives the token.
            require(_origin(url, "an API URL") == self.origin, "a credential is never sent off "
                    "the API origin")
            headers["Authorization"] = "Bearer " + self.token
        try:
            return self.transport.request(url, headers, limit, min(REQUEST_SECONDS, seconds))
        except TransportError:
            raise
        except (OSError, http.client.HTTPException) as error:
            raise TransportError("the request to %s failed: %s" % (
                "the API" if authorized else "signed storage", type(error).__name__))

    def _json(self, response, what):
        require(response.status == 200, "%s answered HTTP %d" % (what, response.status))
        try:
            return json.loads(response.body.decode("utf-8"),
                              object_pairs_hook=_unique_pairs)
        except (UnicodeDecodeError, ValueError) as error:
            raise TransportError("%s is not JSON: %s" % (what, error))

    def get_json(self, path, what):
        """One authorized GET of the configured repository's API below `/repos/OWNER/NAME`;
        `path` is built by the caller from validated numeric IDs and constants only."""
        require(path.startswith("/") and "//" not in path and ".." not in path,
                "an API path below the repository")
        url = "%s/repos/%s%s" % (self.api, self.repository, path)
        return self._json(self._get(url, PAGE_BYTES_MAX, True), what)

    def artifacts(self, run_id):
        """Every artifact of Actions run `run_id`, from bounded paginated listing."""
        contracts.ACTIONS_ID.validate(run_id, "ACTIONS_RUN_ID")
        found = []
        total = None
        for page in range(1, PAGES_MAX + 1):
            url = "%s/repos/%s/actions/runs/%d/artifacts?per_page=%d&page=%d" % (
                self.api, self.repository, run_id, PAGE_SIZE, page)
            doc = self._json(self._get(url, PAGE_BYTES_MAX, True), "the artifact listing")
            require(isinstance(doc, dict) and isinstance(doc.get("artifacts"), list)
                    and type(doc.get("total_count")) is int,
                    "the artifact listing has no artifacts and total_count")
            total = doc["total_count"]
            for item in doc["artifacts"]:
                found.append(_artifact(item, run_id))
            if len(found) >= total or not doc["artifacts"]:
                break
        require(total is not None and len(found) >= total,
                "the listing named %s artifacts, more than the %d a collection reads"
                % (total, len(found)))
        ids = [item["id"] for item in found]
        require(len(ids) == len(set(ids)), "the listing names one artifact twice")
        return found

    def download(self, artifact):
        """An artifact's ZIP bytes: the API's redirect to signed storage, followed without any
        credential; None when the artifact expired."""
        url = "%s/repos/%s/actions/artifacts/%d/zip" % (self.api, self.repository,
                                                       artifact["id"])
        require(artifact["size"] <= ARTIFACT_BYTES_MAX, "artifact %d declares %d bytes, more "
                "than %d" % (artifact["id"], artifact["size"], ARTIFACT_BYTES_MAX))
        limit = min(ARTIFACT_BYTES_MAX, artifact["size"] + limits.MIB)
        response = self._get(url, limit, True)
        hops = 0
        while response.status in (301, 302, 303, 307, 308):
            hops += 1
            require(hops <= REDIRECTS_MAX, "the download redirected more than %d times"
                    % REDIRECTS_MAX)
            location = response.headers.get("location", "")
            require(location, "the download redirected nowhere")
            _origin(location, "the download's signed location")
            response = self._get(location, limit, False)
        if response.status in (404, 410):
            return None
        require(response.status == 200, "the download answered HTTP %d" % response.status)
        return response.body


def _unique_pairs(pairs):
    seen = {}
    for key, value in pairs:
        if key in seen:
            raise ValueError("duplicate key %r" % key)
        seen[key] = value
    return seen


def _unix_ms(text):
    if not isinstance(text, str):
        return None
    try:
        moment = datetime.strptime(text, "%Y-%m-%dT%H:%M:%SZ")
    except ValueError:
        return None
    return int((moment - datetime(1970, 1, 1)).total_seconds() * 1000)


def _artifact(item, run_id):
    require(isinstance(item, dict), "an artifact entry is not an object")
    ident, name, size = item.get("id"), item.get("name"), item.get("size_in_bytes")
    require(type(ident) is int and 1 <= ident <= 2 ** 53 and isinstance(name, str)
            and 0 < len(name) <= 256 and type(size) is int and size >= 0
            and type(item.get("expired")) is bool,
            "an artifact entry has no valid id, name, size and expiry")
    workflow = item.get("workflow_run")
    if isinstance(workflow, dict) and "id" in workflow:
        require(workflow["id"] == run_id, "artifact %d belongs to another run" % ident)
    return {"id": ident, "name": name, "size": size, "expired": item["expired"],
            "created_unix_ms": _unix_ms(item.get("created_at")),
            "expires_unix_ms": _unix_ms(item.get("expires_at"))}


# The transport format -----------------------------------------------------------------------

def read_transport(data, remaining=None):
    """(transport document, {bundle path: bytes}, [(view name, bytes)]) of one artifact's ZIP,
    decompressed within every bound and, when given, the collection's time (`remaining`);
    anything but the declared format refuses."""
    require(data[:4] == b"PK\x03\x04", "the artifact is not a ZIP container, so it is not the "
            "af.perf-transport/1 format; nothing was imported")

    def admit(name):
        require(name == TRANSPORT_FILE or name.startswith(BUNDLE_PREFIX)
                or name.startswith(VIEWS_PREFIX),
                "the artifact member %s is not part of the transport format" % name)
    return _from_members(read_zip(data, remaining, admit))


def read_zip(data, remaining=None, admit=None, bytes_max=ARTIFACT_BYTES_MAX):
    """{member path: bytes} of one artifact's ZIP container, decompressed within every bound
    (members, each member's and all members' bytes, the compression ratio) and, when given,
    the collection's time (`remaining`), refusing encrypted, linked, special, duplicate,
    absolute or traversing members; `admit(name)` refuses a member its format does not
    hold. Shared by the archive's transport format and the telemetry collector (ADR-0200)."""
    require(data[:4] == b"PK\x03\x04", "the artifact is not a ZIP container; nothing was "
            "imported")
    try:
        container = zipfile.ZipFile(io.BytesIO(data))
        infos = container.infolist()
    except (zipfile.BadZipFile, zipfile.LargeZipFile, ValueError) as error:
        raise TransportError("the artifact is not a readable ZIP container: %s" % error)
    require(len(infos) <= ZIP_MEMBERS_MAX, "the artifact holds more than %d members"
            % ZIP_MEMBERS_MAX)
    declared = sum(info.file_size for info in infos)
    require(declared <= bytes_max, "the artifact declares %d decompressed bytes, more "
            "than %d" % (declared, bytes_max))
    names = set()
    members = {}
    total = 0
    for info in infos:
        name = info.filename
        if name.endswith("/") and info.file_size == 0:
            continue
        check_relative_path(name, "an artifact member")
        require(name not in names, "the artifact holds %s twice" % name)
        names.add(name)
        # Many writers record only permission bits; any file type but a regular file refuses.
        require(stat.S_IFMT(info.external_attr >> 16) in (0, stat.S_IFREG),
                "the artifact member %s is not a regular file" % name)
        require(not info.flag_bits & 0x1, "the artifact member %s is encrypted" % name)
        require(info.compress_type in (zipfile.ZIP_STORED, zipfile.ZIP_DEFLATED),
                "the artifact member %s uses an unsupported compression" % name)
        require(info.file_size <= max(RATIO_FLOOR_BYTES, RATIO_MAX * info.compress_size),
                "the artifact member %s decompresses beyond the %d:1 ratio bound"
                % (name, RATIO_MAX))
        if admit is not None:
            admit(name)
        chunks = []
        read = 0
        try:
            with container.open(info) as stream:
                while True:
                    if remaining is not None:
                        remaining("decompressing the artifact")
                    chunk = stream.read(min(CHUNK, info.file_size - read + 1))
                    if not chunk:
                        break
                    read += len(chunk)
                    total += len(chunk)
                    require(read <= info.file_size and total <= bytes_max,
                            "the artifact member %s decompresses beyond its declared size"
                            % name)
                    chunks.append(chunk)
        except (zipfile.BadZipFile, zlib.error) as error:
            raise TransportError("the artifact member %s is damaged: %s" % (name, error))
        require(read == info.file_size, "the artifact member %s is shorter than declared"
                % name)
        members[name] = b"".join(chunks)
    return members


def _from_members(members):
    """(transport document, bundle files, views) of an artifact's checked members."""
    require(TRANSPORT_FILE in members, "the artifact declares no %s, so it is not the "
            "af.perf-transport/1 format; nothing was imported" % TRANSPORT_FILE)
    try:
        doc = contracts.read_json(members.pop(TRANSPORT_FILE), "transport", TRANSPORT_FILE)
    except ContractError as error:
        raise TransportError("the artifact's transport format is not af.perf-transport/1: %s"
                             % error)
    files = dict((name[len(BUNDLE_PREFIX):], value) for name, value in members.items()
                 if name.startswith(BUNDLE_PREFIX))
    require(doc["content"] == "shard_bundle" or not any(
        name.startswith(compare.SHARD_DIR + "/") for name in files),
        "a run bundle artifact holds a shard's files; each shard travels in its own")
    views = []
    declared_views = set(doc["views"])
    for name, value in sorted(members.items()):
        if name.startswith(VIEWS_PREFIX):
            view = name[len(VIEWS_PREFIX):]
            require(view in declared_views and "/" not in view and len(value) <= VIEW_BYTES_MAX,
                    "the artifact's view %s is not one it declares" % view)
            views.append((view, value))
    require(set(view for view, _ in views) == declared_views, "the artifact lacks a declared "
            "view")
    require(files, "the artifact holds no bundle")
    return doc, files, views


def _materialize(target, files):
    for relative, data in sorted(files.items()):
        check_relative_path(relative, "a transported bundle member")
        path = os.path.join(target, *relative.split("/"))
        os.makedirs(os.path.dirname(path), 0o700, exist_ok=True)
        fd = os.open(path, os.O_WRONLY | os.O_CREAT | os.O_EXCL | os.O_NOFOLLOW, 0o600)
        with os.fdopen(fd, "wb") as stream:
            stream.write(data)


def fetch(store, collector, run_id, lab_run=None):
    """Collect Actions run `run_id`'s `af-perf-*` transport artifacts into the open archive
    `store`: (status, archived run ID). Expired or missing artifacts are recorded with their
    states, and the run is admitted with whatever complete bundles remain. One Actions run of
    the continuous workflow can hold several laboratory runs, each named in its artifacts'
    names (`af-perf-<run>-...`, ADR-0177); `lab_run` collects only that one."""
    started = int(store.wall() * 1000)
    source = {"kind": "actions", "repository": collector.repository, "actions_run_id": run_id,
              "artifacts": []}
    if lab_run is not None:
        require(re.match(r"^[a-z][a-z0-9_]{0,63}$", lab_run) is not None,
                "--lab-run: a laboratory run ID")
    prefix = None if lab_run is None else "%s%s-" % (ARTIFACT_PREFIX, lab_run)
    try:
        listed = [item for item in collector.artifacts(run_id)
                  if ARTIFACT_NAME.match(item["name"])
                  and (prefix is None or item["name"].startswith(prefix))]
        transports = []
        for item in listed:
            source["artifacts"].append({
                "id": item["id"], "name": item["name"], "size": item["size"],
                "created_unix_ms": item["created_unix_ms"],
                "expires_unix_ms": item["expires_unix_ms"], "state": "not_collected",
                "sha256": None})
        for item, record in zip(listed, source["artifacts"]):
            if item["expired"]:
                record["state"] = "expired"
                continue
            data = collector.download(item)
            if data is None:
                record["state"] = "expired"
                continue
            record["sha256"] = sha256_bytes(data)
            try:
                transports.append(read_transport(data, collector.remaining) + (record,))
            except ContractError:
                record["state"] = "refused"
                raise
            record["state"] = "downloaded"
        require(transports, "Actions run %d holds no unexpired %s* transport artifact"
                % (run_id, ARTIFACT_PREFIX))
        run_ids = set(doc["run_id"] for doc, _, _, _ in transports)
        require(len(run_ids) == 1, "the run's artifacts name more than one laboratory run: %s"
                % ", ".join(sorted(run_ids)))
        runs = [item for item in transports if item[0]["content"] == "run_bundle"]
        require(len(runs) == 1, "the run's artifacts hold %d run bundles, not one" % len(runs))
        shards = [item[0]["shard"] for item in transports if item[0]["content"]
                  == "shard_bundle"]
        require(len(shards) == len(set(shards)), "the run's artifacts hold one shard twice")
    except ContractError as error:
        detail = "the transport was refused: %s" % error
        store.record_failure(started, source, "transport_failed", detail)
        raise archive.Refused("transport_failed", detail)
    staging = os.path.join(store.root, archive.TMP, "fetch-" + secrets.token_hex(12))
    os.mkdir(staging, 0o700)
    run_dir = os.path.join(staging, "run")
    try:
        _materialize(run_dir, runs[0][1])
        views = list(runs[0][2])
        for doc, files, shard_views, _ in transports:
            if doc["content"] != "shard_bundle":
                continue
            _materialize(os.path.join(run_dir, compare.SHARD_DIR, str(doc["shard"])), files)
            views.extend(shard_views)
        names = [name for name, _ in views]
        require(len(names) == len(set(names)), "two artifacts carry one view name")
        return store.import_run(run_dir, source, views)
    except (ContractError, OSError) as error:
        if isinstance(error, archive.Refused):
            raise
        detail = "the transported run was refused: %s" % error
        store.record_failure(started, source, "refused", detail)
        raise archive.Refused("refused", detail)
    finally:
        admission.remove_tree(staging)


def token_from(environment, name=TOKEN_ENV):
    """The collector's token from its environment variable; never stored or echoed."""
    require(re.match(r"^[A-Z_][A-Z0-9_]{0,63}$", name), "--token-env: an environment "
            "variable name")
    token = environment.get(name)
    require(token, "the collector's token is not in %s" % name)
    return token


def read_directory(path, remaining=None):
    """(transport document, bundle files, views) of one downloaded artifact directory, the
    form `download-artifact` writes: its members admitted as confined, bounded regular files
    (no symlink, device or FIFO) under the same rules as the ZIP the collector reads."""
    bounds = admission.Bounds(ZIP_MEMBERS_MAX, 64, ARTIFACT_BYTES_MAX, ARTIFACT_BYTES_MAX)
    entries = admission.scan(path, bounds, label="the artifact")
    members = {}
    root_fd = admission.open_root(path, "the artifact")
    try:
        for entry in entries:
            if entry.kind != "file":
                continue
            check_relative_path(entry.path, "an artifact member")
            require(entry.path == TRANSPORT_FILE or entry.path.startswith(BUNDLE_PREFIX)
                    or entry.path.startswith(VIEWS_PREFIX),
                    "the artifact member %s is not part of the transport format" % entry.path)
            if remaining is not None:
                remaining("reading the artifact")
            fd = admission.open_entry(root_fd, entry, "the artifact")
            chunks = []
            try:
                admission.read_bounded(fd, entry.size, None, entry.path, "reading the artifact",
                                       sink=chunks.append)
            finally:
                os.close(fd)
            members[entry.path] = b"".join(chunks)
    finally:
        os.close(root_fd)
    return _from_members(members)


def _members(run_dir, content, shard=None, views=()):
    """(transport document, [(member name, bytes)]) of one bundle of a run directory."""
    if content == "run_bundle":
        root = run_dir
        _, hashes = bundle.verify(root, limits.MAX_JSON_BYTES * 2, skip=(compare.SHARD_DIR,),
                                  label="run")
    else:
        root = os.path.join(run_dir, compare.SHARD_DIR, str(shard))
        _, hashes = bundle.verify(root, limits.SHARD_BUNDLE_BYTES_MAX, label="shard")
    plan = contracts.read_file(os.path.join(run_dir, "run.json"), "run")
    doc = {"schema": contracts.TRANSPORT_SCHEMA, "run_id": plan["run_id"], "content": content,
           "shard": shard, "views": sorted(name for name, _ in views)}
    members = [(TRANSPORT_FILE, contracts.write_bytes(doc, "transport"))]
    for relative in sorted(list(hashes) + [bundle.MANIFEST]):
        with open(os.path.join(root, relative), "rb") as stream:
            members.append((BUNDLE_PREFIX + relative, stream.read()))
    for name, data in views:
        check_relative_path(name, "a transported view")
        require("/" not in name, "a transported view is one file name")
        members.append((VIEWS_PREFIX + name, data))
    return doc, members


def write_transport(path, run_dir, content, shard=None, views=()):
    """Package one bundle of a run directory as an af.perf-transport/1 ZIP at `path`: the
    format the collector reads (tests and staging demonstrations)."""
    _, members = _members(run_dir, content, shard, views)
    with zipfile.ZipFile(path, "w", zipfile.ZIP_DEFLATED) as container:
        for name, data in members:
            container.writestr(name, data)
    return path


def stage(target, run_dir, content, shard=None, views=()):
    """Write one bundle of a run directory as the new directory `target` holding exactly the
    af.perf-transport/1 members, which a workflow uploads as one `af-perf-*` artifact: Actions
    zips the directory's files at its root, so the artifact the collector downloads is that ZIP
    (ADR-0177). The format is unchanged; only who writes the container differs."""
    doc, members = _members(run_dir, content, shard, views)
    os.makedirs(target)
    for name, data in members:
        path = os.path.join(target, *name.split("/"))
        os.makedirs(os.path.dirname(path), 0o755, exist_ok=True)
        fd = os.open(path, os.O_WRONLY | os.O_CREAT | os.O_EXCL | os.O_NOFOLLOW, 0o644)
        with os.fdopen(fd, "wb") as stream:
            stream.write(data)
    return doc
