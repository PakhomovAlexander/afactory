"""Bounded real-AF setup regression in the production telemetry executor.

Run explicitly with an actual release binary; never discovered as a synthetic unit test.
The hosted validation job builds that binary in the pinned GNU image. This exercises setup
and public receipts at smoke scale, and makes no performance or hosted rollout claim.
"""
import argparse
import json
import os
from pathlib import Path
import shutil
import sys
import tempfile
import time

sys.dont_write_bytecode = True
ROOT = Path(__file__).resolve().parents[3]
sys.path.insert(0, str(ROOT / 'scripts/performance'))
from perflab import budget, builds, hostprep, runner, telemetryexec, telemetrylab  # noqa: E402
from tests.telemetry_measurement_smoke import IMAGE  # noqa: E402


def exercise(executor, registry, config, af, refuse=False, deadline=None):
    """Use the same argv, private environment, read-only inputs and executor as the lab."""
    deadline = deadline or time.monotonic() + 420
    own = Path(executor.directory)
    own.mkdir(mode=0o700)

    def execute(label, experiment, prepare=None):
        run, capture = own / label, own / (label + '-capture')
        run.mkdir(mode=0o700)
        capture.mkdir(mode=0o700)
        home, env = telemetrylab.private_paths(str(run))
        Path(home).mkdir(mode=0o700)
        script = str(ROOT / registry['experiments'][experiment]['script'])
        if label == 'corpus':
            argv = telemetrylab.corpus_argv(config['read_latency'], config['seed'], script,
                                            str(run / 'corpus'), executor.python)
        else:
            if prepare:
                prepare(run)
            argv = telemetrylab.argv_for(experiment, config[experiment], config['seed'], script,
                af=af, work=str(run / 'bench'), corpus=str(run / 'corpus'), python=executor.python)
            if refuse:
                # Retain the failed Task's public inspection document for the assertions
                # below; this is a diagnostic reproduction, never a timed result.
                argv += ['--keep']
        execution = telemetryexec.Execution(argv, env, str(run), [str(run)],
            telemetrylab._readable(registry, str(ROOT), experiment,
                                   None if label == 'corpus' else af),
            str(capture), label, config['work_bytes'], min(time.monotonic() + 180, deadline - 30),
            telemetrylab.FILE_BYTES_MAX, cleanup_deadline=deadline)
        outcome = executor.run(execution)
        assert outcome.cleanup == 'confirmed', outcome.cleanup_detail
        assert outcome.failure() is None, (label, outcome.failure(), outcome.stderr)
        if refuse:
            assert outcome.code == 1, (outcome.code, outcome.stdout, outcome.stderr)
        else:
            assert outcome.code == 0, (label, outcome.code, outcome.stdout, outcome.stderr)
        print('real AF setup:', label, 'exit', outcome.code, flush=True)
        return run, json.loads(outcome.stdout) if outcome.stdout else None

    if refuse:
        run, _ = execute('overhead', 'overhead')
        document = json.loads((run / 'bench/telemetry-bench-concurrent/00-off/stdout').read_bytes())
        assert document['phase']['kind'] == 'finished'
        assert document['result']['execution'] == 'exhausted'
        assert document['result']['acceptance'] == 'inconclusive'
        assert document['attempts'] == 1
        report = document['run_reports'][0]
        check = next(node for node in report['report']['nodes'] if node['node'] == 'root.nodes.check')
        assert check['outcome']['kind'] == 'failed' and check['outcome']['class'] == 'resources'
        assert report['diagnostics']['root.nodes.check']['message'] == 'Task runtime evidence cannot be empty'
        assert b'finished/exhausted/budget' in outcome_stderr(run)
        # Inspect only public typed receipts, never Worker stdout/stderr or transcripts.
        print('4 GiB reproduces the public check resource failure before any reviewer', flush=True)
    else:
        _, overhead = execute('overhead', 'overhead')
        assert overhead['gate']['verdict'] == 'not_judged'
        assert len(overhead['fixtures']) == 3
        _, invocation = execute('invocation', 'invocation_cost')
        assert invocation['gate']['verdict'] == 'not_judged'
        assert len(invocation['cases']) == 2
        corpus, summary = execute('corpus', 'read_latency')
        assert summary['runs'] == 10 and summary['executions'] == 20 and summary['blobs'] == 200
        _, reads = execute('reads', 'read_latency', lambda run:
            shutil.copytree(corpus / 'corpus', run / 'corpus'))
        assert reads['corpus']['runs'] == 10 and reads['corpus']['executions'] == 20
        assert reads['corpus']['blobs'] == 200 and reads['corpus']['segments'] == 20
        assert {item['name'] for item in reads['measurements']} == set(telemetrylab.READ_MEASUREMENTS)
        for fixture in overhead['fixtures']:
            for pair in fixture['pairs']:
                assert pair['off']['exit_code'] == pair['spans']['exit_code'] == 0
        print('actual AF admitted/completed all three experiment paths; smoke scale only', flush=True)
    # Never remove a possibly live execution's directory. Each run above confirmed teardown.
    owner = getattr(executor, 'owner', None)
    if owner is not None:
        owner.uncertain()
    shutil.rmtree(own)
    if owner is not None:
        owner.filesystem_cleaned()
        owner.work_cleaned()


def outcome_stderr(run):
    return (run / 'bench/telemetry-bench-concurrent/00-off/stderr').read_bytes()


def contained(root, af, registry, config, capacity, refuse, deadline):
    backing, mount, control = root / 'scratch.img', root / 'scratch', root / 'owner'
    hostprep.prepare(str(backing), str(mount), capacity)
    with hostprep.TelemetryOwner(str(control), str(mount), create=True,
                                image=str(backing)) as owner:
        owner.prepare_work()
        try:
            # The low-capacity diagnostic uses the former PR capacity. The successful
            # regression uses the exact current PR registry, without environment overrides.
            environment = telemetrylab.contained_environment(registry,
                dict(config, work_bytes=capacity))
            timing = budget.Budget(deadline, time.monotonic, 30)
            scratch, cpus, runtime = runner.contained_host('docker', IMAGE, str(mount),
                                                          environment, timing, time.monotonic)
            executor = telemetryexec.Contained('docker', builds.BuildImage('docker', IMAGE),
                environment, scratch, os.getuid(), os.getgid(), cpus, 'real-af', runtime,
                owner=owner)
            info = os.statvfs(mount)
            free = info.f_bavail * info.f_frsize
            assert info.f_blocks * info.f_frsize <= capacity
            assert (free < 10 << 30) if refuse else (free >= config['work_bytes'] - (2 << 30))
            print('bounded scratch: gross', capacity, 'available', free, flush=True)
            exercise(executor, registry, config, af, refuse, deadline)
        except BaseException:
            print('regression failed; retained owned recovery state:', root, flush=True)
            raise
        problems = owner.cleanup(str(backing))
        assert not problems, problems
    assert not backing.exists() and not mount.exists()
    shutil.rmtree(control)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--af', required=True)
    parser.add_argument('--native', action='store_true',
                        help='explicit local diagnostic only; no containment claim')
    parser.add_argument('--work-parent', default=str(ROOT / 'target'))
    args = parser.parse_args()
    af = str(Path(args.af).resolve(strict=True))
    assert os.access(af, os.X_OK)
    Path(args.work_parent).mkdir(parents=True, exist_ok=True)
    root = Path(tempfile.mkdtemp(prefix='af-real-telemetry-', dir=args.work_parent))
    registry, _ = telemetrylab.load_registry()
    config = telemetrylab.configuration(registry, 'pr', rounds=1, pairs=1, scale='smoke', corpus_runs=10)
    if args.native:
        executor = telemetryexec.TrustedLocal(sys.executable)
        executor.directory = str(root / 'native')
        exercise(executor, registry, config, af)
        print('native diagnostic only; Docker was not used')
    else:
        assert shutil.which('docker'), 'Docker unavailable: real contained AF regression was not run'
        assert os.getuid() != 0, 'requires a real non-root runner-owned checkout'
        deadline = time.monotonic() + 480
        contained(root, af, registry, config, 4 << 30, True, deadline)
        contained(root, af, registry, config, config['work_bytes'], False, deadline)
        print('real contained AF setup regression passed; hosted rollout proof remains separate')
    shutil.rmtree(root)


if __name__ == '__main__':
    main()
