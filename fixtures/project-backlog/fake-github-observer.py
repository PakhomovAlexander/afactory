"""Offline transport inserted into the real observer before its entrypoint."""
from types import SimpleNamespace

fixture_state = Path(__AF_BACKLOG_FIXTURE_STATE__)
fixture_repo = '/repos/Fixture/Backlog'
fixture_url = API_URL + fixture_repo


def fixture_issue(number, day, **fields):
    return dict(dict(id=1000 + number, number=number, state='open', title='Boundary issue',
                     body=None, updated_at=f'2026-11-{day}T12:30:00Z', repository_url=fixture_url,
                     url=f'{fixture_url}/issues/{number}'), **fields)


class FixtureTransport:
    def __init__(self):
        self.walks = 0

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
            page = API_URL + '/repositories/23/issues?state=open&sort=created&direction=asc&per_page=100&page='
            if url == page + '1':
                self.walks += 1
            if mode == 'offset-shift':
                number = int(url.removeprefix(page))
                # Close issue 1 after page 1 of the first walk. Both walks still have three pages.
                start = 1 if self.walks == 1 and number == 1 else 2
                offset = 100 * (number - 1)
                payload = [fixture_issue(n, day) if n == 101 else fixture_issue(n, day, pull_request={})
                           for n in range(start + offset, min(start + offset + 100, 251))]
                if number < 3:
                    response['link'] = f'<{page}{number + 1}>; rel="next", <{page}3>; rel="last"'
                return SimpleNamespace(status=status, headers=response, body=json.dumps(payload).encode())
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
                elif mode == 'second-with-registry-drift' and self.walks == 2:
                    with (fixture_state.parent / 'providers.toml').open('a') as registry:
                        registry.write('\n# receiver registry changed before preparation\n')
        return SimpleNamespace(status=status, headers=response, body=json.dumps(payload).encode())


HttpsTransport = FixtureTransport
