"""The nightly scripts' one GitHub reader (ADR-0265, ADR-0267).

`build.py` reads the check runs that decide which main commit is green; `report.py` reads the
backlog, the merged pull requests and the head's check runs for the daily report. Both ask
api.github.com through `Client`, which opens each GET with `urllib.request`: 5 s to connect
and 30 s for each read, at most 16 MiB of response, no proxy and no redirect followed,
retried once when it fails or the server answers 5xx, anonymous unless a read-only token file is
configured. Nothing here writes to GitHub.
"""
import http.client
import re
import urllib.error
import urllib.request

from perflab.values import ContractError, loads_strict

API_ROOT = "https://api.github.com"
CONNECT_SECONDS = 5
READ_SECONDS = 30
RESPONSE_BYTES_MAX = 16 * 1024 * 1024
PER_PAGE, PAGES_MAX = 100, 20


class Failed(Exception):
    """GitHub could not be read; the message says why."""


class NoRedirect(urllib.request.HTTPRedirectHandler):
    """A redirect is answered as its own status, never followed: a request and its token go
    to api.github.com and nowhere else."""

    def redirect_request(self, req, fp, code, msg, headers, newurl):
        return None


class BoundedConnection(http.client.HTTPSConnection):
    """urllib's HTTPS connection with the connect bounded apart from the reads: the socket
    connects within CONNECT_SECONDS and then waits at most the opener's timeout (READ_SECONDS)
    for each read."""

    def connect(self):
        read_timeout, self.timeout = self.timeout, CONNECT_SECONDS
        try:
            super().connect()
        finally:
            self.timeout = read_timeout
        self.sock.settimeout(read_timeout)


class BoundedHTTPSHandler(urllib.request.HTTPSHandler):
    def https_open(self, req):
        return self.do_open(BoundedConnection, req, context=self._context)


def opener():
    """The opener every GET goes through: no proxy from the environment, no redirect, the
    connect bounded by CONNECT_SECONDS."""
    return urllib.request.build_opener(urllib.request.ProxyHandler({}), NoRedirect(),
                                       BoundedHTTPSHandler())


def read_token(path, error=Failed):
    """The token in the file `path`, or None to ask anonymously; `error` when it is unreadable
    or holds no token."""
    if path is None:
        return None
    try:
        with open(path, "r", encoding="utf-8") as stream:
            token = stream.read(4096).strip()
    except OSError as failure:
        raise error("github_token_file %s: %s" % (path, failure))
    if not token or re.match(r"^[A-Za-z0-9_.-]+$", token) is None:
        raise error("github_token_file %s holds no token" % path)
    return token


def https_get(open_url, path, headers):
    """(status, at most RESPONSE_BYTES_MAX + 1 body bytes) of one GET of api.github.com
    through `open_url` (an opener's `open`), READ_SECONDS for each read (and, through
    `opener()`, CONNECT_SECONDS to connect). An HTTP error status is an answer, not a failure."""
    request = urllib.request.Request(API_ROOT + path, headers=headers, method="GET")
    try:
        response = open_url(request, timeout=READ_SECONDS)
    except urllib.error.HTTPError as answer:
        with answer:
            return answer.code, answer.read(RESPONSE_BYTES_MAX + 1)
    with response:
        return response.status, response.read(RESPONSE_BYTES_MAX + 1)


class Client:
    """Bounded GETs of one repository's API through `opener` (`opener()` by default). A
    subclass names the exception a failure raises (`error`) and its User-Agent (`agent`)."""

    error = Failed
    agent = "af-nightly"

    def __init__(self, repository, token=None, opener=None):
        self.repository = repository
        self.token = token
        self.opener = opener

    def request(self, path):
        headers = {"Accept": "application/vnd.github+json", "User-Agent": self.agent,
                   "X-GitHub-Api-Version": "2022-11-28"}
        if self.token:
            headers["Authorization"] = "Bearer " + self.token
        if self.opener is None:
            self.opener = opener()
        failure = None
        for _ in range(2):
            try:
                status, body = https_get(self.opener.open, path, headers)
            # A transport failure: OSError covers URLError and a timeout, ValueError a
            # malformed answer; any other exception of the HTTP layer is a failed read too.
            except Exception as caught:
                failure = str(caught) or type(caught).__name__
                continue
            if status >= 500:
                failure = "HTTP %d" % status
                continue
            if status != 200:
                raise self.error("GitHub answered HTTP %d for %s" % (status, path))
            try:
                return loads_strict(body, "GitHub " + path, RESPONSE_BYTES_MAX)
            except ContractError as caught:
                raise self.error(str(caught))
        raise self.error("GitHub %s failed twice: %s" % (path, failure))

    def check_runs(self, sha):
        """[{name, status, conclusion}] of every check run of `sha`, all pages."""
        runs = []
        for page in range(1, PAGES_MAX + 1):
            doc = self.request("/repos/%s/commits/%s/check-runs?filter=latest&per_page=%d"
                               "&page=%d" % (self.repository, sha, PER_PAGE, page))
            batch = doc.get("check_runs") if isinstance(doc, dict) else None
            total = doc.get("total_count") if isinstance(doc, dict) else None
            if not isinstance(batch, list) or type(total) is not int:
                raise self.error("GitHub check runs of %s: not a check-runs page" % sha)
            for item in batch:
                if not (isinstance(item, dict) and isinstance(item.get("name"), str)
                        and isinstance(item.get("status"), str)
                        and (item.get("conclusion") is None
                             or isinstance(item.get("conclusion"), str))):
                    raise self.error("GitHub check runs of %s: a malformed check run" % sha)
                runs.append({"name": item["name"], "status": item["status"],
                             "conclusion": item["conclusion"]})
            if not batch or len(runs) >= total:
                return runs
        raise self.error("%s has more than %d check runs" % (sha, PER_PAGE * PAGES_MAX))

    def pages(self, path, until=None):
        """Every item of the list endpoint `path` (which holds no `page` or `per_page`), page by
        page, at most PAGES_MAX pages; `until(item)` true ends the walk at that item. More
        items than the pages hold is a failure, never a silently shortened list."""
        items = []
        separator = "&" if "?" in path else "?"
        for page in range(1, PAGES_MAX + 1):
            batch = self.request("%s%sper_page=%d&page=%d" % (path, separator, PER_PAGE, page))
            if not isinstance(batch, list) or not all(isinstance(item, dict) for item in batch):
                raise self.error("GitHub %s: not a list page" % path)
            for item in batch:
                if until is not None and until(item):
                    return items
                items.append(item)
            if len(batch) < PER_PAGE:
                return items
        raise self.error("GitHub %s holds more than %d items" % (path, PER_PAGE * PAGES_MAX))
