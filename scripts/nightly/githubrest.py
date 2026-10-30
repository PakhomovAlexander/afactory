"""The nightly train's GitHub REST transport (ADR-0265, ADR-0268), shared by build.py and
publish.py.

The reader asks for the check runs of a commit, anonymously or with the read-only token: one
bounded GET of api.github.com, retried once when it fails or the server answers 5xx. The writer
holds the write token `publish` reads from its `[publish]` table: JSON requests to
api.github.com and asset uploads to the release's `upload_url` on uploads.github.com, each one
bounded (60 s, a bounded response), never redirected and never sent to another origin; only its
GETs are retried. No token is ever printed, logged or passed as an argument.
"""
import http.client
import json
import os
import re
import sys
import urllib.error
import urllib.parse
import urllib.request

sys.path.insert(0, os.path.join(os.path.dirname(os.path.dirname(os.path.abspath(__file__))),
                                "performance"))

from perflab.nightlycontracts import TAG_PATTERN  # noqa: E402
from perflab.values import ContractError, loads_strict  # noqa: E402

API_HOST = "api.github.com"
API = "https://" + API_HOST
UPLOADS = "https://uploads.github.com"
CONNECT_SECONDS, READ_SECONDS = 5, 30
WRITE_SECONDS = 60
RESPONSE_BYTES_MAX = 16 * 1024 * 1024
PER_PAGE, PAGES_MAX = 100, 20


class Refused(Exception):
    """The night cannot be planned, built or published; the message says why."""


def https_get(path, headers):
    """One bounded GET of api.github.com: 5 s to connect, 30 s for each read."""
    connection = http.client.HTTPSConnection(API_HOST, timeout=CONNECT_SECONDS)
    try:
        connection.connect()
        connection.sock.settimeout(READ_SECONDS)
        connection.request("GET", path, headers=headers)
        response = connection.getresponse()
        return response.status, response.read(RESPONSE_BYTES_MAX + 1)
    finally:
        connection.close()


class _NoRedirect(urllib.request.HTTPRedirectHandler):
    """A redirect is answered as it came: a credential never follows one."""

    def redirect_request(self, req, fp, code, msg, headers, newurl):
        return None


_OPENER = urllib.request.build_opener(_NoRedirect)


def https_send(method, url, headers, body):
    """One request through urllib, 60 s, never redirected: (status, at most 16 MiB + 1 of the
    response). An HTTP error status is returned, not raised."""
    request = urllib.request.Request(url, data=body, headers=headers, method=method)
    try:
        with _OPENER.open(request, timeout=WRITE_SECONDS) as response:
            return response.status, response.read(RESPONSE_BYTES_MAX + 1)
    except urllib.error.HTTPError as error:
        try:
            return error.code, error.read(RESPONSE_BYTES_MAX + 1)
        finally:
            error.close()


def _failure(error):
    # urllib's URLError says only its reason; neither it nor anything else here names the URL.
    reason = getattr(error, "reason", None)
    return str(reason if reason is not None else error) or type(error).__name__


def _message(body):
    """GitHub's own `message` of an error answer, bounded, or nothing."""
    try:
        doc = json.loads(body.decode("utf-8"))
    except (ValueError, UnicodeDecodeError, AttributeError):
        return ""
    text = doc.get("message") if isinstance(doc, dict) else None
    return ": " + text[:200] if isinstance(text, str) and text else ""


def _is_int(value):
    return type(value) is int and value > 0


class GitHub:
    """The repository's REST API: the check runs of a commit through `get` (`https_get` by
    default), and, with a token, its releases, assets and tags through `send` (`https_send`)."""

    def __init__(self, repository, token=None, get=https_get, send=https_send):
        self.repository = repository
        self.token = token
        self.get = get
        self.send = send

    def headers(self, user_agent="af-nightly-build"):
        headers = {"Accept": "application/vnd.github+json", "User-Agent": user_agent,
                   "X-GitHub-Api-Version": "2022-11-28"}
        if self.token:
            headers["Authorization"] = "Bearer " + self.token
        return headers

    def request(self, path):
        headers = self.headers()
        failure = None
        for _ in range(2):
            try:
                status, body = self.get(path, headers)
            except (OSError, http.client.HTTPException) as error:
                failure = str(error) or type(error).__name__
                continue
            if status >= 500:
                failure = "HTTP %d" % status
                continue
            if status != 200:
                raise Refused("GitHub answered HTTP %d for %s" % (status, path))
            try:
                return loads_strict(body, "GitHub " + path, RESPONSE_BYTES_MAX)
            except ContractError as error:
                raise Refused(str(error))
        raise Refused("GitHub %s failed twice: %s" % (path, failure))

    def check_runs(self, sha):
        """[{name, status, conclusion}] of every check run of `sha`, all pages."""
        runs = []
        for page in range(1, PAGES_MAX + 1):
            doc = self.request("/repos/%s/commits/%s/check-runs?filter=latest&per_page=%d"
                               "&page=%d" % (self.repository, sha, PER_PAGE, page))
            batch = doc.get("check_runs") if isinstance(doc, dict) else None
            total = doc.get("total_count") if isinstance(doc, dict) else None
            if not isinstance(batch, list) or type(total) is not int:
                raise Refused("GitHub check runs of %s: not a check-runs page" % sha)
            for item in batch:
                if not (isinstance(item, dict) and isinstance(item.get("name"), str)
                        and isinstance(item.get("status"), str)
                        and (item.get("conclusion") is None
                             or isinstance(item.get("conclusion"), str))):
                    raise Refused("GitHub check runs of %s: a malformed check run" % sha)
                runs.append({"name": item["name"], "status": item["status"],
                             "conclusion": item["conclusion"]})
            if not batch or len(runs) >= total:
                return runs
        raise Refused("%s has more than %d check runs" % (sha, PER_PAGE * PAGES_MAX))

    # The writer ----------------------------------------------------------------------------

    def call(self, method, target, payload=None, data=None, content_type=None, ok=(200,),
             absent=()):
        """(status, parsed JSON or None) of one authenticated request. `target` is an API path
        or an uploads.github.com URL of this repository; a status in `absent` answers None, any
        status outside `ok` and `absent` refuses. Only a GET is retried, once."""
        if self.token is None:
            raise Refused("publishing needs the write token of [publish] github_token_file")
        if target.startswith("/"):
            url, where = API + target, target
        else:
            require_upload(target, self.repository)
            url, where = target, urllib.parse.urlsplit(target).path
        headers = self.headers("af-nightly-publish")
        if payload is not None:
            data = json.dumps(payload, sort_keys=True).encode("utf-8")
            content_type = "application/json"
        if data is not None:
            headers["Content-Type"] = content_type
        failure = None
        for _ in range(2 if method == "GET" else 1):
            try:
                status, body = self.send(method, url, headers, data)
            except (OSError, http.client.HTTPException) as error:
                failure = _failure(error)
                continue
            if status >= 500 and method == "GET":
                failure = "HTTP %d" % status
                continue
            if status in absent:
                return status, None
            if status not in ok:
                raise Refused("GitHub answered HTTP %d for %s %s%s" % (status, method, where,
                                                                       _message(body)))
            if status == 204:
                return status, None
            try:
                return status, loads_strict(body, "GitHub %s %s" % (method, where),
                                            RESPONSE_BYTES_MAX)
            except ContractError as error:
                raise Refused(str(error))
        raise Refused("GitHub %s %s failed%s: %s" % (method, where,
                                                     " twice" if method == "GET" else "",
                                                     failure))

    def release_by_tag(self, tag):
        """The release of `tag`, or None."""
        _, doc = self.call("GET", "/repos/%s/releases/tags/%s" % (self.repository,
                                                                  _tag(tag)), absent=(404,))
        return None if doc is None else release(doc, self.repository)

    def tag_commit(self, tag):
        """The commit the lightweight tag ref `tag` names, None without the ref; a tag that
        names anything else refuses."""
        _, doc = self.call("GET", "/repos/%s/git/ref/tags/%s" % (self.repository, _tag(tag)),
                           absent=(404,))
        if doc is None:
            return None
        target = doc.get("object") if isinstance(doc, dict) else None
        if not (isinstance(doc, dict) and doc.get("ref") == "refs/tags/" + tag
                and isinstance(target, dict) and target.get("type") == "commit"
                and isinstance(target.get("sha"), str)):
            raise Refused("GitHub's tag %s does not name a commit" % tag)
        return target["sha"]

    def create_release(self, tag, sha, name, notes):
        _, doc = self.call("POST", "/repos/%s/releases" % self.repository, payload={
            "tag_name": _tag(tag), "target_commitish": sha, "name": name, "body": notes,
            "draft": False, "prerelease": True, "make_latest": "false"}, ok=(201,))
        return release(doc, self.repository)

    def update_notes(self, release_id, notes):
        _, doc = self.call("PATCH", "/repos/%s/releases/%d" % (self.repository, release_id),
                           payload={"body": notes})
        return release(doc, self.repository)

    def upload(self, upload_url, name, data, content_type):
        """The asset GitHub made of `data`, uploaded as `name` through the release's
        `upload_url`."""
        url = upload_url.split("{", 1)[0] + "?" + urllib.parse.urlencode({"name": name})
        _, doc = self.call("POST", url, data=data, content_type=content_type, ok=(201,))
        return asset(doc)

    def delete_asset(self, asset_id):
        self.call("DELETE", "/repos/%s/releases/assets/%d" % (self.repository, asset_id),
                  ok=(204,))

    def releases(self):
        """[{id, tag_name, prerelease, draft}] of every release of the repository, all pages."""
        out = []
        for page in range(1, PAGES_MAX + 1):
            _, batch = self.call("GET", "/repos/%s/releases?per_page=%d&page=%d"
                                 % (self.repository, PER_PAGE, page))
            if not isinstance(batch, list):
                raise Refused("GitHub releases of %s: not a page of releases" % self.repository)
            for item in batch:
                if not (isinstance(item, dict) and _is_int(item.get("id"))
                        and isinstance(item.get("tag_name"), str)
                        and type(item.get("prerelease")) is bool
                        and type(item.get("draft")) is bool):
                    raise Refused("GitHub releases of %s: a malformed release" % self.repository)
                out.append(dict((key, item[key]) for key in ("id", "tag_name", "prerelease",
                                                              "draft")))
            if len(batch) < PER_PAGE:
                return out
        raise Refused("%s has more than %d releases" % (self.repository, PER_PAGE * PAGES_MAX))

    def delete_release(self, release_id):
        self.call("DELETE", "/repos/%s/releases/%d" % (self.repository, release_id), ok=(204,))

    def delete_tag(self, tag):
        """Delete the tag ref `tag`; True when it was there, False when GitHub holds no such
        ref (404). Any other answer, 422 included, is refused: the ref may still be there."""
        status, _ = self.call("DELETE", "/repos/%s/git/refs/tags/%s" % (self.repository,
                                                                         _tag(tag)),
                              ok=(204,), absent=(404,))
        return status == 204


def _tag(tag):
    # Only a nightly tag is ever written or deleted, and it is safe in a URL path as it is.
    if not isinstance(tag, str) or re.match(TAG_PATTERN, tag) is None:
        raise Refused("%r is not a nightly tag" % (tag,))
    return tag


def require_upload(url, repository):
    """An upload goes only to uploads.github.com under this repository's releases."""
    prefix = "%s/repos/%s/releases/" % (UPLOADS, repository)
    parts = urllib.parse.urlsplit(url)
    if not (url.startswith(prefix) and parts.scheme == "https"
            and parts.netloc == "uploads.github.com"
            and re.match(r"^/repos/[^/]+/[^/]+/releases/[0-9]+/assets$", parts.path)):
        raise Refused("GitHub's upload_url is not an asset upload of %s" % repository)


def asset(doc):
    """{id, name, size, digest, state} of an asset GitHub reports; its digest may be null."""
    if not (isinstance(doc, dict) and _is_int(doc.get("id")) and isinstance(doc.get("name"), str)
            and type(doc.get("size")) is int and doc["size"] >= 0
            and (doc.get("digest") is None or isinstance(doc.get("digest"), str))
            and isinstance(doc.get("state"), str)):
        raise Refused("GitHub reported a malformed release asset")
    return dict((key, doc.get(key)) for key in ("id", "name", "size", "digest", "state"))


def release(doc, repository):
    """The fields of a release GitHub reports that publishing reads, checked."""
    if not (isinstance(doc, dict) and _is_int(doc.get("id"))
            and isinstance(doc.get("tag_name"), str) and isinstance(doc.get("html_url"), str)
            and isinstance(doc.get("upload_url"), str)
            and isinstance(doc.get("target_commitish"), str)
            and type(doc.get("prerelease")) is bool and type(doc.get("draft")) is bool
            and isinstance(doc.get("assets"), list)):
        raise Refused("GitHub reported a malformed release")
    out = dict((key, doc[key]) for key in ("id", "tag_name", "html_url", "upload_url",
                                           "target_commitish", "prerelease", "draft"))
    require_upload(out["upload_url"].split("{", 1)[0], repository)
    out["assets"] = [asset(item) for item in doc["assets"]]
    return out
