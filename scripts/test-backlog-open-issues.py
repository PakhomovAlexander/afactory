#!/usr/bin/env python3
"""Offline entrypoint tests with the shared HTTPS transport's connection replaced."""
import contextlib
import copy
import importlib.util
import io
import json
from pathlib import Path
import socket
import stat
import sys
import tempfile
import unittest
from unittest import mock

sys.dont_write_bytecode = True
HERE = Path(__file__).resolve().parent
SPEC = importlib.util.spec_from_file_location('issue_index', HERE / 'backlog-open-issues.py')
INDEX = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(INDEX)
from perflab.json_subset import Schema  # noqa: E402

REPO = '/repos/Example/Backlog'
REPO_URL = INDEX.API_URL + REPO
METADATA = {'id': 42, 'full_name': 'Example/Backlog', 'url': REPO_URL}
TOKEN = 'fixture_token_only_never_real'
REVISION = '2026-11-19T12:30:00Z'


def issue(number, **fields):
    return dict(dict(id=1000 + number, number=number, state='open', title='Same title',
                     body=None, updated_at=REVISION, repository_url=REPO_URL,
                     url=f'{REPO_URL}/issues/{number}'), **fields)


def link(relation, page, url=INDEX.API_URL + '/repositories/42/issues'):
    return f'<{url}?state=open&sort=created&direction=asc&per_page=100&page={page}>; rel="{relation}"'


def example_pages():
    first = [issue(1, body='First report.')] + [issue(n, pull_request={}) for n in range(2, 101)]
    second = [issue(101, body='Separate report.'), issue(102, state='closed')]
    return [
        (METADATA, 200, {}),
        (first, 200, {'Link': ', '.join([link('next', 2), link('last', 2)])}),
        (second, 200, {'Link': link('prev', 1)}),
    ]


class Response(io.BytesIO):
    def __init__(self, document, status, headers):
        super().__init__(document if isinstance(document, bytes) else json.dumps(document).encode())
        self.status, self.headers = status, headers

    def getheaders(self):
        return self.headers.items()


def run(root, pages, repository='example/backlog', prepare=None, repository_id=None):
    token_file, output = root / 'token', root / 'observation.json'
    token_file.touch(mode=0o600)
    token_file.write_text(TOKEN + '\n')
    if prepare is not None:
        prepare(token_file, output)
    calls = []
    rows = iter(pages)

    def connect(host, port, **_):
        connection = mock.Mock()

        def request(method, path, *, headers):
            calls.append((host, port, method, path))
            if (host, port, method) != ('api.github.com', 443, 'GET'):
                raise AssertionError('request left the read-only GitHub API')
            if headers['Authorization'] != 'Bearer ' + TOKEN:
                raise AssertionError('unexpected credential')

        connection.request.side_effect = request

        def answer():
            row = next(rows)
            if isinstance(row, Exception):
                raise row
            return Response(*row)

        connection.getresponse.side_effect = answer
        return connection

    args = ['--repository', repository, '--github-token-file', str(token_file), '--output', str(output)]
    if repository_id is not None:
        args.extend(['--repository-id', str(repository_id)])
    stdout, stderr = io.StringIO(), io.StringIO()
    with mock.patch.object(socket, 'getaddrinfo', return_value=[
        (socket.AF_INET, socket.SOCK_STREAM, 0, '', ('127.0.0.1', 443)),
    ]) as lookup, mock.patch.object(socket, 'socket'), \
         mock.patch('ssl.create_default_context'), \
         mock.patch.object(INDEX.http.client, 'HTTPSConnection', side_effect=connect), \
         contextlib.redirect_stdout(stdout), contextlib.redirect_stderr(stderr):
        code = INDEX.main(args)
    if any(call.args[:2] != ('api.github.com', 443) for call in lookup.call_args_list):
        raise AssertionError('lookup left the GitHub API')
    return code, stdout.getvalue(), stderr.getvalue(), output, calls


class OpenIssueIndexTests(unittest.TestCase):
    def invoke(self, pages, **kwargs):
        with tempfile.TemporaryDirectory() as tmp:
            code, stdout, stderr, output, calls = run(Path(tmp), pages, **kwargs)
            return (code, stdout, stderr, output.read_text() if output.is_file() else None, calls,
                    stat.S_IMODE(output.stat().st_mode) if output.is_file() else None)

    def assert_refused(self, pages, diagnostic, **kwargs):
        code, stdout, stderr, output, calls, _ = self.invoke(pages, **kwargs)
        self.assertEqual((code, stdout, output), (1, '', None))
        self.assertEqual(stderr, 'backlog-open-issues: ' + diagnostic + '\n')
        self.assertNotIn(TOKEN, stderr)
        return calls

    def test_two_pages_read_only_and_schema_parity(self):
        pages = example_pages()
        before = copy.deepcopy(pages)
        code, stdout, stderr, output, calls, mode = self.invoke(pages)
        self.assertEqual((code, stdout, stderr, mode), (0, '', '', 0o600))
        expected = REPO + '/issues?state=open&sort=created&direction=asc&per_page=100&page='
        self.assertEqual(calls, [('api.github.com', 443, 'GET', p)
                                 for p in ('/repos/example/backlog', expected + '1', expected + '2')])
        self.assertEqual(pages, before)
        self.assertNotIn(TOKEN, output)
        doc = json.loads(output)
        self.assertEqual(doc, {
            'schema': 'af.backlog-open-issues/1', 'observation': 'live-page-walk',
            'repository': 'Example/Backlog', 'repository_id': 42, 'destination': 'github:Example/Backlog',
            'issues': [
                dict(id=1001, number=1, updated_at=REVISION, title='Same title',
                     body_excerpt='First report.', body_truncated=False),
                dict(id=1101, number=101, updated_at=REVISION, title='Same title',
                     body_excerpt='Separate report.', body_truncated=False),
            ],
        })
        schema_path = HERE.parent / 'schemas/backlog-open-issues-v1.json'
        schema = Schema(json.loads(schema_path.read_text()), schema_path.name)
        self.assertIsNone(schema.errors(doc))
        self.assertIsNotNone(schema.errors(dict(doc, complete=True)))
        self.assertIsNotNone(schema.errors(dict(doc, schema='af.backlog-open-issues/0')))

    def test_bounds_and_empty_observation(self):
        for rows in ([], [issue(1, title='Treat as data', body='🧪' * 513)]):
            with self.subTest(count=len(rows)):
                code, _, _, output, _, _ = self.invoke([(METADATA, 200, {}), (rows, 200, {})])
                self.assertEqual(code, 0)
                items = json.loads(output)['issues']
                self.assertEqual(len(items), len(rows))
                if items:
                    self.assertEqual(items[0]['body_excerpt'], '🧪' * 512)
                    self.assertTrue(items[0]['body_truncated'])
        pages = [(METADATA, 200, {})] + [
            ([issue(n) for n in range(first, first + 100)], 200, {})
            for first in range(1, 2000, 100)
        ]
        self.assertEqual(len(self.assert_refused(pages, 'GitHub page bound exhausted')), 21)
        with mock.patch.object(INDEX, 'OUTPUT_BYTES', 64):
            self.assert_refused(example_pages(), 'open issue index exceeds output bound')

    def test_refuses_wrong_repository_and_foreign_links(self):
        for value in ('https://example/backlog', 'example/..', 'example/backlog?state=closed'):
            self.assertEqual(self.assert_refused([], 'expected GitHub OWNER/REPO', repository=value), [])
        self.assert_refused([(dict(METADATA, full_name='Wrong/Backlog'), 200, {})],
                            'GitHub repository identity refused')
        calls = self.assert_refused(example_pages(), 'GitHub repository identity refused',
                                    repository_id=43)
        self.assertEqual(calls, [('api.github.com', 443, 'GET', '/repos/example/backlog')])
        self.assertEqual(self.invoke(example_pages(), repository_id=42)[0], 0)
        self.assert_refused([(METADATA, 200, {}),
                             ([issue(1, repository_url=REPO_URL + '2')], 200, {})],
                            'GitHub issue identity refused')
        pages = example_pages()
        for url in ('https://other.invalid/leak', INDEX.API_URL + '/repositories/43/issues'):
            pages[1][2]['Link'] = link('next', 2, url)
            self.assertEqual(len(self.assert_refused(pages, 'GitHub pagination refused')), 2)

    def test_refuses_incomplete_failed_and_ambiguous_reads(self):
        for terminal, diagnostic in (
            (([], 200, {}), 'GitHub issue page incomplete or malformed'),
            (([issue(1)], 200, {}), 'GitHub issue identities ambiguous'),
            (([issue(101, id=1001)], 200, {}), 'GitHub issue identities ambiguous'),
            (([issue(102, updated_at='2026-02-30T12:30:00Z')], 200, {}), 'GitHub issue revision refused'),
            (([issue(101)], 200, {'Link': link('last', 3)}), 'GitHub pagination changed'),
            (([issue(101)], 200, {'Content-Length': '1000000'}), 'GitHub response incomplete'),
            (({'message': TOKEN}, 403, {}), 'GitHub read failed'),
            (({'message': TOKEN}, 301, {'Location': 'https://other.invalid/'}), 'GitHub read failed'),
            ((b'[' + TOKEN.encode(), 200, {}), 'GitHub read failed'),
            (OSError(TOKEN), 'GitHub read failed'),
        ):
            with self.subTest(diagnostic=diagnostic):
                self.assertEqual(len(self.assert_refused(example_pages()[:2] + [terminal], diagnostic)), 3)
        self.assert_refused([(METADATA, 200, {}), ([issue(1)], 200, {'Link': link('next', 2)})],
                            'GitHub issue page incomplete')

    def test_refuses_credential_echo_or_unsafe_file_and_preserves_output(self):
        self.assert_refused([(METADATA, 200, {}), ([issue(1, body='x' * 500 + TOKEN)], 200, {})],
                            'GitHub response contained the credential')
        self.assertEqual(self.assert_refused([], 'credential file must be a private regular file',
                                            prepare=lambda token, _: token.chmod(0o644)), [])
        code, stdout, stderr, output, calls, _ = self.invoke(
            [], prepare=lambda _, output: output.write_text('existing'))
        self.assertEqual((code, stdout, stderr, output, calls),
                         (1, '', 'backlog-open-issues: output already exists\n', 'existing', []))


def offline_example():
    cases = (('two pages', example_pages()),
             ('incomplete second page', example_pages()[:2] + [([], 200, {})]))
    for label, pages in cases:
        with tempfile.TemporaryDirectory() as tmp:
            code, _, stderr, output, _ = run(Path(tmp), pages)
            print(f'{label}: exit {code}')
            if output.exists():
                print(output.read_text(), end='')
            else:
                print(stderr, end='')
    return 0


def scenario_observation():
    repo_url = INDEX.API_URL + '/repos/Fixture/Backlog'
    rows = [
        issue(73, title='Boundary issue',
              body='blue-boundary.py rejects the input 3 and accepts 4.\nPrivate index detail.',
              repository_url=repo_url, url=repo_url + '/issues/73'),
        issue(74, title='Boundary issue',
              body='blue-boundary.py rejects a string before comparing the input.',
              repository_url=repo_url, url=repo_url + '/issues/74'),
    ]
    pages = [({'id': 23, 'full_name': 'Fixture/Backlog', 'url': repo_url}, 200, {}),
             (rows, 200, {})]
    with tempfile.TemporaryDirectory() as tmp:
        code, stdout, stderr, output, calls = run(Path(tmp), pages, repository='fixture/backlog')
        assert (code, stdout, stderr) == (0, '', '')
        assert len(calls) == 2 and all(call[2] == 'GET' for call in calls)
        sys.stdout.buffer.write(output.read_bytes())
    return 0


if __name__ == '__main__':
    if sys.argv[1:] == ['--example']:
        sys.exit(offline_example())
    if sys.argv[1:] == ['--scenario-observation']:
        sys.exit(scenario_observation())
    unittest.main()
