"""Find credentials where a credential-free job must have none (ADR-0177).

A read-only GitHub token is still a credential. Checking `.git/config` alone does not establish
its absence: actions/checkout v6 and later persists its token in a separate file under
RUNNER_TEMP and includes it from `.git/config`, and `persist-credentials: false` opts out of
both. So the checks cover a repository's configuration and every file it includes, an
environment (by variable name and by token-shaped value), and the files of a directory such as a
build pack or an evidence bundle. A finding names where and what kind; a credential's value is
never echoed, recorded or returned.
"""
import os
import re
import stat

from . import admission

# Variables that carry an Actions or GitHub credential whenever they are set.
CREDENTIAL_VARIABLES = ("GITHUB_TOKEN", "GH_TOKEN", "GH_ENTERPRISE_TOKEN", "ACTIONS_RUNTIME_TOKEN",
                        "ACTIONS_ID_TOKEN_REQUEST_TOKEN", "ACTIONS_ID_TOKEN_REQUEST_URL",
                        "ACTIONS_RESULTS_URL", "ACTIONS_CACHE_URL", "AF_PERF_ARCHIVE_TOKEN",
                        "CARGO_REGISTRY_TOKEN")
# Token shapes GitHub issues, and an HTTP authorization header with a value.
TOKEN = re.compile(rb"(?:gh[pousr]_[A-Za-z0-9]{30,}|github_pat_[A-Za-z0-9_]{40,}|"
                   rb"(?i:authorization:\s*(?:basic|bearer|token)\s+[A-Za-z0-9+/=._-]{8,}))")
CONFIG_KEYS = re.compile(r"^\s*(extraheader|helper|insteadof|pushinsteadof|path)\s*=",
                         re.IGNORECASE)
CONFIG_SECTIONS = re.compile(r"^\s*\[\s*(credential|include|includeif|http|url)\b",
                             re.IGNORECASE)
USERINFO_URL = re.compile(r"[a-z][a-z0-9+.-]*://[^/@\s]+@", re.IGNORECASE)
CONFIG_BYTES_MAX = 1024 * 1024
SCAN_FILE_BYTES_MAX = 512 * 1024 * 1024
SCAN_ENTRIES_MAX = 100000


def git_config(git_dir):
    """Findings in a repository's configuration: a credential helper, an HTTP header, a URL
    with user information, a URL rewrite, or an include that could bring any of them in."""
    path = os.path.join(git_dir, "config")
    if not os.path.lexists(path):
        return []
    info = os.lstat(path)
    if not stat.S_ISREG(info.st_mode):
        return ["%s is not a regular file" % path]
    with open(path, "rb") as stream:
        data = stream.read(CONFIG_BYTES_MAX + 1)
    findings = []
    if len(data) > CONFIG_BYTES_MAX:
        findings.append("%s is larger than %d bytes" % (path, CONFIG_BYTES_MAX))
    for number, line in enumerate(data.decode("utf-8", "replace").splitlines(), 1):
        section = CONFIG_SECTIONS.match(line)
        if section:
            findings.append("%s:%d: a [%s] section" % (path, number, section.group(1).lower()))
        key = CONFIG_KEYS.match(line)
        if key:
            findings.append("%s:%d: a %s setting" % (path, number, key.group(1).lower()))
        if USERINFO_URL.search(line):
            findings.append("%s:%d: a URL with user information" % (path, number))
    if TOKEN.search(data):
        findings.append("%s: a token-shaped value" % path)
    return findings


def environment(env):
    """Findings in an environment: a credential variable that is set, or any variable whose
    value has a token's shape. Only names are reported."""
    findings = []
    for name in sorted(env):
        value = env[name]
        if name in CREDENTIAL_VARIABLES and value:
            findings.append("the environment sets %s" % name)
        elif TOKEN.search(value.encode("utf-8", "replace")):
            findings.append("the environment variable %s holds a token-shaped value" % name)
    return findings


def tree(root, label=None):
    """Findings in every regular file below `root`, read as confined bounded entries: a
    token-shaped value or an authorization header. Anything but a directory or regular file
    refuses the scan itself."""
    label = label or root
    bounds = admission.Bounds(SCAN_ENTRIES_MAX, 64, SCAN_FILE_BYTES_MAX, 8 * SCAN_FILE_BYTES_MAX)
    entries = admission.scan(root, bounds, label=label)
    findings = []
    root_fd = admission.open_root(root, label)
    try:
        for entry in entries:
            if entry.kind != "file":
                continue
            fd = admission.open_entry(root_fd, entry, label)
            tail = [b""]
            found = []

            def sink(chunk):
                # A token split across two reads is still found: each chunk is searched with
                # the end of the one before.
                window = tail[0] + chunk
                if not found and TOKEN.search(window):
                    found.append(True)
                tail[0] = window[-256:]

            try:
                admission.read_bounded(fd, entry.size, None, entry.path, "scanning " + label,
                                       sink=sink)
            finally:
                os.close(fd)
            if found:
                findings.append("%s/%s: a token-shaped value" % (label, entry.path))
    finally:
        os.close(root_fd)
    return findings
