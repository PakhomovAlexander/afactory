"""Offline transport inserted into the real observer before its entrypoint."""
from types import SimpleNamespace

fixture_state = Path(__AF_BACKLOG_FIXTURE_STATE__)
fixture_repo = '/repos/Fixture/Backlog'
fixture_issues = fixture_repo + '/issues'
fixture_url = API_URL + fixture_repo


def fixture_issue(number, day, **fields):
    return dict(dict(id=1000 + number, number=number, state='open', title='Boundary issue',
                     body=None, updated_at=f'2026-11-{day}T12:30:00Z', repository_url=fixture_url,
                     url=f'{fixture_url}/issues/{number}'), **fields)


class FixtureTransport:
    def request(self, url, headers, max_bytes, timeout):
        assert headers['Authorization'] == 'Bearer fixture_issues_read_only'
        assert 'AF_BACKLOG_GITHUB_TOKEN' not in os.environ
        assert 0 < timeout <= REQUEST_SECONDS and max_bytes == PAGE_BYTES
        mode = (fixture_state / 'observer-mode').read_text()
        day = '19' if mode == 'first' else '20'
        with (fixture_state / 'observer-requests').open('a') as trace:
            trace.write(url + '\n')
        status, response = 200, {}
        if url == API_URL + '/repos/fixture/backlog':
            payload = dict(id=24 if mode == 'wrong-scope' else 23, full_name='Fixture/Backlog',
                           url=fixture_url)
        else:
            page = f'{fixture_url}/issues?state=open&sort=created&direction=asc&per_page=100&page='
            if url == page + '1':
                payload = [fixture_issue(73, day, body=(
                    'blue-boundary.py rejects the input 3 and accepts 4.\nPrivate index detail.'))]
                payload += [fixture_issue(n, day, pull_request={}) for n in range(100, 199)]
                if mode == 'oversize':
                    payload = [fixture_issue(n, day) for n in range(1, 101)]
                response['link'] = f'<{page}2>; rel="next", <{page}2>; rel="last"'
            else:
                assert url == page + '2'
                payload = [fixture_issue(74, day, body=(
                    'blue-boundary.py rejects a string before comparing the input.'))]
                if mode == 'oversize':
                    payload = [fixture_issue(n, day) for n in range(101, 130)]
                elif mode == 'partial':
                    payload = []
                elif mode == 'unavailable':
                    status = 503
                elif mode == 'second-with-registry-drift':
                    with (fixture_state.parent / 'providers.toml').open('a') as registry:
                        registry.write('\n# receiver registry changed before preparation\n')
        return SimpleNamespace(status=status, headers=response, body=json.dumps(payload).encode())


HttpsTransport = FixtureTransport
