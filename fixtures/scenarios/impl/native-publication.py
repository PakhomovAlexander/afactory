"""Controlled Git/GitHub transport for native scoped-publication regressions only.

All acceptance, source export, workspace preparation, publication guards and journals use the
production implementation. Only SSH/GitHub IO is replaced; there are no model calls here.
"""
import contextlib
import io
import types
import importlib.util
import json
import os
from pathlib import Path
import subprocess
import sys

root, config_path, remote_path, service_path = sys.argv[1:]
spec = importlib.util.spec_from_file_location("installer", Path(root) / "scripts/install-scenario-git-workspace.py")
installer = importlib.util.module_from_spec(spec)
spec.loader.exec_module(installer)
c = json.loads(Path(config_path).read_text())
installed = Path(config_path + '.installed')
if not installed.exists():
    with contextlib.redirect_stdout(io.StringIO()):
        installer.install(Path(config_path), installed)
a = types.ModuleType('native_scoped_installed')
exec(compile(installed.read_text(), str(installed), 'exec'), a.__dict__)
a.workspace_load_config()
service_file = Path(service_path)
state = json.loads(service_file.read_text()) if service_file.exists() else dict(prs=[], posts=0, pushes=0, lose_post=True)


def save():
    service_file.write_text(json.dumps(state))


def remote_tip(branch):
    out = subprocess.run([c['git'], '--git-dir', remote_path, 'rev-parse', '--verify', 'refs/heads/' + branch],
                         stdout=subprocess.PIPE, stderr=subprocess.DEVNULL, check=False)
    return out.stdout.decode().strip() if out.returncode == 0 else None


class Service:
    def request(self, method, path, body=None, *, before_send=None):
        if method == 'PATCH':
            assert callable(before_send)
            before_send()
            assert path == '/repos/octo/project/pulls/1'
            state['patches'] = state.get('patches', 0) + 1
            assert state['patches'] == 1
            state['prs'][0]['body'] = body['body']
            save()
            raise RuntimeError('controlled loss after retained PATCH')
        if method == 'POST':
            assert callable(before_send)
            before_send()
            assert path == '/repos/octo/project/pulls'
            state['posts'] += 1
            assert state['posts'] == 1
            state['prs'].append(dict(id=101, number=1, user=dict(id=7, login='owner'), title=body['title'], body=body['body'],
                url='https://api.github.com/repos/octo/project/pulls/1', html_url='https://github.com/octo/project/pull/1',
                head=dict(ref=body['head'], sha=remote_tip(body['head']), repo=dict(id=17, full_name='octo/project')),
                base=dict(ref=body['base'], repo=dict(id=17, full_name='octo/project')), state='open', merged=False))
            save()
            if state['lose_post']:
                state['lose_post'] = False
                save()
                raise RuntimeError('controlled loss after retained POST')
            return state['prs'][0]
        assert method == 'GET'
        if path == '/user': return dict(id=state.get('actor_id', 7), login='owner')
        if path == '/repos/octo/project':
            if state.get('source_after_actor') is not None:
                state['source_actor_reads'] = state.get('source_actor_reads', 0) + 1
                if state['source_actor_reads'] == 3:
                    # The last remote identity observation changes local source after
                    # performance_base's first source check. The final source read must refuse.
                    subprocess.run([c['git'], '-C', c['repo'], 'update-ref', 'refs/heads/main',
                                    state['source_after_actor']], check=True)
            return dict(id=state.get('repository_id', 17), full_name='octo/project')
        if path == '/repos/octo/project/git/ref/heads/main':
            return dict(ref='refs/heads/main', object=dict(type='commit', sha=remote_tip('main')))
        if path == '/repos/octo/project/pulls/1':
            pr = state['prs'][0]
            pr['head']['sha'] = remote_tip(pr['head']['ref'])
            return pr
        raise AssertionError(path)

    def pages(self, path, key=None, params=None):
        if path == '/repos/octo/project/pulls': return state['prs']
        if path.endswith('/reviews'): return []
        revision = path.split('/')[-2]
        conclusion = 'success' if state['pushes'] >= 2 else 'failure'
        if path.endswith('/check-runs'):
            return [dict(id=50, name='CI', app=dict(id=4), head_sha=revision,
                         check_suite=dict(id=40), status='completed', conclusion=conclusion)]
        if path.endswith('/check-suites'):
            return [dict(id=40, app=dict(id=4), head_sha=revision, status='completed', conclusion=conclusion)]
        raise AssertionError(path)


class Transport(a.WorkspaceAdapter):
    def git(self, *args, **kwargs):
        url = 'ssh://git@github.com/octo/project.git'
        if url in args:
            assert args[0] in ('push', 'ls-remote')
            if args[0] == 'push':
                before_spawn = kwargs.get('before_spawn')
                assert callable(before_spawn)
                target_commit = args[-1].split(':', 1)[0]
                def guarded():
                    fault = state.get('faults', []).pop(0) if state.get('faults') else None
                    if fault == 'actor': state['actor_id'] = 99
                    if fault == 'destination': state['repository_id'] = 99
                    if fault == 'base':
                        subprocess.run([c['git'], '--git-dir', remote_path, '-c', 'protocol.file.allow=always',
                                        'fetch', '--quiet', '--no-tags', '--no-write-fetch-head', c['repo'], target_commit], check=True)
                    if fault == 'source':
                        state['source_after_actor'] = target_commit
                        state['source_actor_reads'] = 0
                    if fault == 'base':
                        subprocess.run([c['git'], '--git-dir', remote_path, 'update-ref', 'refs/heads/main', target_commit], check=True)
                    try:
                        before_spawn()
                    except a.Refused:
                        state.setdefault('refusals', []).append(fault)
                        raise
                    else:
                        if fault is not None:
                            state['bad_guard_success'] = fault
                            raise AssertionError('native publication guard accepted a changed boundary')
                        state['pushes'] += 1
                    finally:
                        state.pop('actor_id', None)
                        state.pop('repository_id', None)
                        state.pop('source_after_actor', None)
                        state.pop('source_actor_reads', None)
                        if fault in ('source', 'base'):
                            target = ['-C', c['repo']] if fault == 'source' else ['--git-dir', remote_path]
                            subprocess.run([c['git'], *target, 'update-ref', 'refs/heads/main', c['source_commit']], check=True)
                        save()
                kwargs['before_spawn'] = guarded
            args = tuple(remote_path if arg == url else arg for arg in args)
            # The only file transport is this test's disposable bare remote.
            args = ('-c', 'protocol.file.allow=always', *args)
        return super().git(*args, **kwargs)


os.umask(0o077)
request = json.load(sys.stdin)
a.CONFIG_DIGEST = a.digest(Path(config_path).read_bytes())
adapter = Transport(c, a.CONFIG_DIGEST, Service())
try:
    if request.get('schema') == 'af/implementation-startup-preflight@2':
        response = adapter.startup_preflight(request)
    else:
        response = adapter.handle(request)
except Exception as error:
    state['last_error'] = type(error).__name__ + ': ' + str(error)
    save()
    raise
print(json.dumps(response))
