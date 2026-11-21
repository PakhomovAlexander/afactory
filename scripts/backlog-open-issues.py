#!/usr/bin/env python3
"""Write a private, bounded observation of one GitHub repository's open issues."""
import argparse
import datetime as dt
import http.client
import json
import os
from pathlib import Path
import re
import stat
import sys
import tempfile
import time
from urllib.parse import parse_qs, urlsplit

sys.dont_write_bytecode = True
sys.path.insert(0, str(Path(__file__).resolve().parent / 'performance'))
from perflab.transport import API_URL, HttpsTransport  # noqa: E402
from perflab.values import ContractError, loads_strict  # noqa: E402

SCHEMA = 'af.backlog-open-issues/1'
REPOSITORY = re.compile(r'[A-Za-z0-9][A-Za-z0-9-]{0,38}/[A-Za-z0-9_.-]{1,100}')
REVISION = re.compile(r'[0-9]{4}-[0-9]{2}-[0-9]{2}T[0-9]{2}:[0-9]{2}:[0-9]{2}Z')
PAGE_SIZE, MAX_PAGES, PAGE_BYTES = 100, 20, 16 * 1024 * 1024
OUTPUT_BYTES = 4 * 1024 * 1024
WALL_SECONDS, REQUEST_SECONDS = 120, 15
TEXT_LIMIT = 512
MAX_ID = (1 << 53) - 1


class Refused(Exception):
    """A refusal with a fixed diagnostic, free of remote data and credentials."""


def require(ok, message):
    if not ok:
        raise Refused(message)


def repository_name(value):
    return (isinstance(value, str) and REPOSITORY.fullmatch(value)
            and value.split('/')[1] not in ('.', '..'))


def identity(value):
    return type(value) is int and 0 < value <= MAX_ID


def read_token(path):
    try:
        fd = os.open(path, os.O_RDONLY | os.O_NOFOLLOW | os.O_NONBLOCK)
        with os.fdopen(fd, 'rb') as stream:
            info = os.fstat(stream.fileno())
            require(stat.S_ISREG(info.st_mode) and info.st_uid in (0, os.geteuid())
                    and not info.st_mode & 0o077 and info.st_nlink == 1,
                    'credential file must be a private regular file')
            raw = stream.read(4097)
    except OSError:
        raise Refused('credential file unavailable') from None
    match = re.fullmatch(rb'([A-Za-z0-9_]{20,255})\n?', raw)
    require(match is not None, 'credential file malformed')
    return match[1].decode('ascii')


def get(transport, path, token, deadline):
    remaining = deadline - time.monotonic()
    require(remaining > 0, 'GitHub read budget exhausted')
    try:
        response = transport.request(API_URL + path, {
            'Authorization': 'Bearer ' + token, 'Accept': 'application/vnd.github+json',
            'X-GitHub-Api-Version': '2022-11-28', 'User-Agent': 'af-backlog-open-issues',
            'Cache-Control': 'no-cache',
        }, PAGE_BYTES, min(remaining, REQUEST_SECONDS))
        require(time.monotonic() < deadline, 'GitHub read budget exhausted')
        require(response.status == 200, 'GitHub read failed')
        size = response.headers.get('content-length')
        require(size is None or size == str(len(response.body)), 'GitHub response incomplete')
        return loads_strict(response.body, 'GitHub response', PAGE_BYTES), response.headers
    except (OSError, http.client.HTTPException, ContractError, RecursionError):
        raise Refused('GitHub read failed') from None


def pagination(link, path, named_path, page):
    require(len(link) <= 8192, 'GitHub pagination refused')
    relations = {}
    for item in link.split(',') if link else ():
        match = re.fullmatch(r'\s*<([^>]+)>;\s*rel="(next|prev|first|last)"\s*', item)
        require(match is not None, 'GitHub pagination refused')
        try:
            target = urlsplit(match[1])
            query = parse_qs(target.query, keep_blank_values=True)
        except ValueError:
            raise Refused('GitHub pagination refused') from None
        require(target.scheme == 'https' and target.netloc == 'api.github.com'
                and target.path in (path, named_path)
                and not target.fragment and query.get('per_page') == [str(PAGE_SIZE)]
                and match[2] not in relations, 'GitHub pagination refused')
        numbers = query.get('page', [])
        require(len(numbers) == 1 and re.fullmatch(r'[1-9][0-9]{0,8}', numbers[0]),
                'GitHub pagination refused')
        relations[match[2]] = int(numbers[0])
    require(relations.get('next', page + 1) == page + 1
            and relations.get('prev', page - 1) == page - 1
            and relations.get('first', 1) == 1, 'GitHub pagination refused')
    return 'next' in relations, relations.get('last')


def walk(name, repo_id, token, transport, deadline):
    path = f'/repositories/{repo_id}/issues'
    named_path = '/repos/' + name + '/issues'
    repo_url = API_URL + '/repos/' + name
    ids, numbers, rows, issues = set(), set(), [], []
    more, last = False, None
    for page in range(1, MAX_PAGES + 1):
        batch, headers = get(transport, path + f'?state=open&sort=created&direction=asc'
                             f'&per_page={PAGE_SIZE}&page={page}', token, deadline)
        require(isinstance(batch, list) and len(batch) <= PAGE_SIZE and (batch or not more),
                'GitHub issue page incomplete or malformed')
        more, advertised = pagination(headers.get('link', ''), path, named_path, page)
        require(advertised is None or last in (None, advertised), 'GitHub pagination changed')
        last = advertised if advertised is not None else last
        require(last is None or page <= last <= MAX_PAGES, 'GitHub page bound or changed listing')
        full = len(batch) == PAGE_SIZE
        require((full or not more) and (last is None or more == (page < last)),
                'GitHub issue page incomplete')
        for row in batch:
            require(isinstance(row, dict), 'GitHub issue row malformed')
            issue_id, number = row.get('id'), row.get('number')
            require(identity(issue_id) and identity(number) and row.get('repository_url') == repo_url
                    and row.get('url') == f'{repo_url}/issues/{number}',
                    'GitHub issue identity refused')
            require(issue_id not in ids and number not in numbers, 'GitHub issue identities ambiguous')
            ids.add(issue_id)
            numbers.add(number)
            state = row.get('state')
            require(state in ('open', 'closed'), 'GitHub issue state refused')
            require('pull_request' not in row or isinstance(row['pull_request'], dict),
                    'GitHub pull request marker refused')
            pull_request = 'pull_request' in row
            rows.append((issue_id, number, state, pull_request))
            if pull_request or state == 'closed':
                continue
            title, body, revision = row.get('title'), row.get('body'), row.get('updated_at')
            require(isinstance(title, str) and 0 < len(title) <= TEXT_LIMIT
                    and 'body' in row and (body is None or isinstance(body, str))
                    and isinstance(revision, str) and REVISION.fullmatch(revision),
                    'GitHub issue text or revision refused')
            try:
                dt.datetime.fromisoformat(revision[:-1] + '+00:00')
            except ValueError:
                raise Refused('GitHub issue revision refused') from None
            body = body or ''
            require(token not in title and token not in body, 'GitHub response contained the credential')
            issues.append(dict(id=issue_id, number=number, updated_at=revision, title=title,
                               body_excerpt=body[:TEXT_LIMIT], body_truncated=len(body) > TEXT_LIMIT))
        if not full or (not more and page == last):
            return rows, issues
    raise Refused('GitHub page bound exhausted')


def repository_identity(repository, token, transport, deadline):
    repo, _ = get(transport, '/repos/' + repository, token, deadline)
    require(isinstance(repo, dict), 'GitHub repository identity refused')
    name, repo_id = repo.get('full_name'), repo.get('id')
    require(repository_name(name) and name.lower() == repository.lower() and identity(repo_id)
            and repo.get('url') == API_URL + '/repos/' + name, 'GitHub repository identity refused')
    return name, repo_id


def observe(repository, token, transport, expected_repository_id=None):
    deadline = time.monotonic() + WALL_SECONDS
    name, repo_id = repository_identity(repository, token, transport, deadline)
    require(expected_repository_id is None or repo_id == expected_repository_id,
            'GitHub repository identity refused')
    first = walk(name, repo_id, token, transport, deadline)
    require(walk(name, repo_id, token, transport, deadline) == first,
            'GitHub listing changed between page walks')
    require(repository_identity(repository, token, transport, deadline) == (name, repo_id),
            'GitHub repository identity refused')
    return dict(schema=SCHEMA, observation='live-page-walk', repository=name,
                destination='github:' + name, repository_id=repo_id,
                issues=sorted(first[1], key=lambda item: item['number']))


def write_private(path, data):
    # Publish only finished bytes and keep an existing path, including a symlink, untouched.
    with tempfile.NamedTemporaryFile(dir=path.parent, prefix='.issue-index-') as stream:
        os.fchmod(stream.fileno(), 0o600)
        stream.write(data)
        stream.flush()
        os.fsync(stream.fileno())
        os.link(stream.name, path)


class Parser(argparse.ArgumentParser):
    def error(self, _message):
        self.exit(2, 'backlog-open-issues: invalid arguments; see --help\n')


def main(argv=None):
    parser = Parser(description=__doc__, allow_abbrev=False)
    parser.add_argument('--repository', required=True, help='GitHub OWNER/REPO')
    parser.add_argument('--repository-id', type=int, help='expected stable GitHub repository ID')
    parser.add_argument('--github-token-file', required=True, type=Path,
                        help='private token file with Issues read permission')
    parser.add_argument('--output', required=True, type=Path, help='new private JSON file')
    args = parser.parse_args(argv)
    try:
        require(repository_name(args.repository), 'expected GitHub OWNER/REPO')
        require(args.repository_id is None or identity(args.repository_id),
                'expected a valid stable GitHub repository ID')
        require(not os.path.lexists(args.output), 'output already exists')
        token = read_token(args.github_token_file)
        result = observe(args.repository, token, HttpsTransport(), args.repository_id)
        data = (json.dumps(result, ensure_ascii=False, separators=(',', ':')) + '\n').encode('utf-8')
        require(token.encode('ascii') not in data, 'GitHub response contained the credential')
        require(len(data) <= OUTPUT_BYTES, 'open issue index exceeds output bound')
        write_private(args.output, data)
    except Refused as error:
        print(f'backlog-open-issues: {error}', file=sys.stderr)
        return 1
    except (OSError, UnicodeError):
        print('backlog-open-issues: output or local I/O refused', file=sys.stderr)
        return 1
    return 0


if __name__ == '__main__':
    sys.exit(main())
