#!/usr/bin/env python3
"""Private, token-free interval/output fixture and bounded resource measurements.

Runs no production Ant, touches no existing Store, and inherits no user HOME/XDG state.
This is an executable example/measurement recipe, not machine service installation.
"""
import argparse
import copy
import ctypes
import hashlib
import json
import os
from pathlib import Path
import signal
import subprocess
import tempfile
import time

ROOT = Path(__file__).resolve().parent.parent


def wakeups(pid):
    """Unprivileged counters for our own unreaped Mac child, per SDK resource.h v2.

    This samples the process before exit: it does not count the parent's sleep or claim
    system-wide physical wakeups. Other platforms/permission failures remain unavailable.
    """
    if os.uname().sysname != 'Darwin':
        return {'available': False, 'reason': 'proc_pid_rusage is macOS-specific'}
    class Usage(ctypes.Structure):
        _fields_ = [('uuid', ctypes.c_uint8 * 16)] + [(name, ctypes.c_uint64) for name in [
            'user_time', 'system_time', 'pkg_idle_wkups', 'interrupt_wkups', 'pageins',
            'wired_size', 'resident_size', 'phys_footprint', 'proc_start_abstime',
            'proc_exit_abstime', 'child_user_time', 'child_system_time',
            'child_pkg_idle_wkups', 'child_interrupt_wkups', 'child_pageins',
            'child_elapsed_abstime', 'diskio_bytesread', 'diskio_byteswritten']]
    library = ctypes.CDLL('/usr/lib/libproc.dylib', use_errno=True)
    library.proc_pid_rusage.argtypes = [ctypes.c_int, ctypes.c_int, ctypes.c_void_p]
    library.proc_pid_rusage.restype = ctypes.c_int
    value = Usage()
    if library.proc_pid_rusage(pid, 2, ctypes.byref(value)) != 0:
        return {'available': False, 'errno': ctypes.get_errno()}
    return {'available': True, 'package_idle_wakeups': value.pkg_idle_wkups,
            'interrupt_wakeups': value.interrupt_wkups,
            'basis': 'owned process proc_pid_rusage v2 sample before exit, not host-wide wakeups'}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--af', type=Path, required=True)
    parser.add_argument('--seconds', type=int, default=10)
    args = parser.parse_args()
    if not 4 <= args.seconds <= 60:
        parser.error('--seconds must be 4 through 60 (bounded private measurement)')
    af = args.af.resolve(strict=True)
    with tempfile.TemporaryDirectory(prefix='af-interval-measure-') as temporary:
        private = Path(temporary).resolve()
        private.chmod(0o700)
        env = {k: v for k, v in os.environ.items() if not k.startswith(('AF_', 'XDG_'))}
        env['HOME'] = str(private / 'home')
        for key, leaf in [('XDG_CONFIG_HOME', 'config'), ('XDG_STATE_HOME', 'state'),
                          ('XDG_CACHE_HOME', 'cache'), ('XDG_DATA_HOME', 'data'),
                          ('XDG_BIN_HOME', 'bin'), ('TMPDIR', 'tmp')]:
            env[key] = str(private / leaf)
        for key in ['HOME', 'XDG_CONFIG_HOME', 'XDG_STATE_HOME', 'XDG_CACHE_HOME',
                    'XDG_DATA_HOME', 'XDG_BIN_HOME', 'TMPDIR']:
            Path(env[key]).mkdir(mode=0o700)
        env['AF_SELF_OFFLINE'] = '1'
        store = private / 'scenario-state'
        policy = private / 'policy.json'
        policy.write_bytes((ROOT / 'fixtures/scenarios/results/policy.json').read_bytes())
        policy.chmod(0o600)
        commands = []

        def run(*words, json_result=True):
            command = [str(af), *words, '--state', str(store)]
            if json_result:
                command.append('--json')
            result = subprocess.run(command, cwd=private, env=env, capture_output=True,
                                    text=True, timeout=30, check=False)
            commands.append({'argv': list(words), 'exit_code': result.returncode})
            if result.returncode:
                raise RuntimeError(f'{words}: {result.returncode}\n{result.stdout}\n{result.stderr}')
            return json.loads(result.stdout) if json_result else result.stdout

        ant_paths = []
        for label in ['weather', 'library']:
            bundle = json.loads((ROOT / f'fixtures/scenarios/results/{label}.json').read_text())
            ant = {'schema': 'af/ant@7', 'id': label, 'scenario': bundle, 'inputs': {},
                   'schedule': {'kind': 'interval', 'seconds': 1,
                                'start_unix_ms': time.time_ns() // 1_000_000},
                   'owner': 'fixture', 'notification': 'local:fixture',
                   'execution': {'overlap': 'skip', 'missed': 'skip', 'lateness_ms': 999,
                                 'per_run': copy.deepcopy(bundle['definitions'][label]['limits']),
                                 'rolling_tokens': 0, 'rolling_window_ms': 10000}}
            path = private / f'{label}.json'
            path.write_text(json.dumps(ant))
            pin = run('ant', 'validate', '--file', str(path), '--authority', str(policy))
            run('ant', 'add', '--file', str(path), '--authority', str(policy),
                '--confirm-pin', pin['revision'])
            ant_paths.append(label)

        def sizes():
            files = [p for p in (store / 'runs').iterdir() if p.is_file()]
            return {'retained_runtime_bytes': sum(p.stat().st_size for p in files),
                    'journal_bytes': (store / 'runs/scenarios.sqlite').stat().st_size}

        def measure(command, seconds):
            before = sizes()
            with tempfile.TemporaryFile() as out, tempfile.TemporaryFile() as err:
                started = time.monotonic_ns()
                child = subprocess.Popen([str(af), *command, '--state', str(store)],
                                         cwd=private, env=env, stdout=out, stderr=err)
                # Sample the owned process while it is live, then obtain whole-process CPU
                # from wait4. The sampling window deliberately excludes the final 250 ms.
                time.sleep(max(0, (args.seconds if seconds is None else seconds) - 0.25))
                sampled_wakeups = wakeups(child.pid)
                sampled_wakeups['sample_elapsed_seconds'] = (time.monotonic_ns() - started) / 1e9
                if seconds is not None:
                    # Only the owned watch child is interrupted; no service or PID discovery.
                    time.sleep(0.25)
                    child.send_signal(signal.SIGINT)
                _, status, usage = os.wait4(child.pid, 0)
                child.returncode = os.waitstatus_to_exitcode(status)
                elapsed = (time.monotonic_ns() - started) / 1e9
                err.seek(0)
                detail = err.read().decode()
                expected = (0,) if seconds is None else (-signal.SIGINT, 130)
                if child.returncode not in expected:
                    raise RuntimeError(f'{command}: {child.returncode}: {detail}')
                after = sizes()
                return {'argv': command, 'exit_code': child.returncode,
                        'elapsed_seconds': elapsed, 'user_seconds': usage.ru_utime,
                        'system_seconds': usage.ru_stime,
                        'cpu_percent_one_core': 100 * (usage.ru_utime + usage.ru_stime) / elapsed,
                        'voluntary_context_switches': usage.ru_nvcsw,
                        'involuntary_context_switches': usage.ru_nivcsw,
                        'wakeups': sampled_wakeups,
                        'before': before, 'after': after,
                        'journal_growth_bytes': after['journal_bytes'] - before['journal_bytes']}

        # Disabled bindings establish an existing Store without activating production work.
        idle = measure(['ant', 'scheduler', 'run', '--for-seconds', str(args.seconds)], None)
        for label in ant_paths:
            run('ant', 'enable', label)
        active = measure(['ant', 'scheduler', 'run', '--for-seconds', str(args.seconds)], None)
        receipts = {}
        for label in ant_paths:
            shown = run('ant', 'show', label)
            starts = [o for o in shown['occurrences'] if o['outcome']['kind'] == 'started']
            if len(starts) < 3 or len({o['id'] for o in starts}) != len(starts):
                raise RuntimeError('Insufficient or duplicate repeated occurrences')
            for occurrence in starts:
                result = run('scenario', 'show', occurrence['outcome']['run_id'])
                if result['status'] != 'succeeded' or result['dispatches'] != 0:
                    raise RuntimeError('Inert Scenario did not settle without dispatch')
            for name in ['report', 'empty']:
                document = run('ant', 'output', label, '--name', name, json_result=False)
                if '\x1b' in document or not document.endswith('-->\n'):
                    raise RuntimeError('Invalid Markdown stream')
            receipts[label] = {'started': len(starts), 'occurrences': shown['occurrences']}
        # Producer/scheduler is stopped while polling continues at an independent 1s cadence.
        latest = private / 'latest.md'
        watch = measure(['ant', 'output', 'library', '--name', 'report', '--watch',
                         '--interval-seconds', '1', '--output', str(latest)], args.seconds)
        markdown = latest.read_text()
        if not markdown.startswith('<!-- af-result-snapshot begin') or '\x1b' in markdown:
            raise RuntimeError('Invalid atomic Markdown file')
        print(json.dumps({'schema': 'af-private-interval-measurement/1', 'platform': os.uname().sysname,
                          'binary_sha256': hashlib.sha256(af.read_bytes()).hexdigest(),
                          'seconds_requested': args.seconds, 'basis': 'monotonic wall time and wait4 per owned child; debug build unless caller supplies release binary; two inert Ants; no models or adapters',
                          'idle': idle, 'one_second': active, 'watch_one_second': watch,
                          'receipts': receipts, 'commands': commands}, indent=2))


if __name__ == '__main__':
    main()
