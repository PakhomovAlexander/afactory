"""Real native CLI restart fixture. Only the external heavy worker is a stand-in."""
import importlib.util
import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile

sys.dont_write_bytecode = True
ROOT = Path(__file__).resolve().parents[3]
sys.path[:0] = [str(ROOT / 'scripts/performance'), str(ROOT / 'scripts/nightly')]
from tests.test_nightly_scenario import ServiceFixture, trusted_mounts  # noqa: E402
import scenario as runtime  # noqa: E402
from perflab.values import document_bytes, sha256_bytes  # noqa: E402

spec = importlib.util.spec_from_file_location('nightly_install', ROOT / 'scripts/nightly/install.py')
install = importlib.util.module_from_spec(spec)
spec.loader.exec_module(install)


def main(af, comparison_proof=None):
    fixture = ServiceFixture()
    fixture.setUp()
    try:
        root = fixture.root
        state = root / 'state'
        repo = root / 'repo'
        repo.mkdir()
        # The adapter wrapper models root-owned mount ancestors in this sandbox only.
        adapter = root / 'adapter'
        adapter.write_text('#!/usr/bin/python3 -IB\nimport sys\nsys.path[:0]='
            + repr([str(ROOT / 'scripts/performance'), str(ROOT / 'scripts/nightly')])
            + '\nfrom tests.test_nightly_scenario import trusted_mounts\nimport scenario\n'
            + 'sys.argv=["scenario.py","adapter","--service",' + repr(str(fixture.file)) + ']\n'
            + 'with trusted_mounts(): raise SystemExit(scenario.main())\n')
        adapter.chmod(0o700)
        policy = dict(schema='af/scenario-policy@6', adapters=dict(nightly=dict(executable=str(adapter),
            digest=sha256_bytes(adapter.read_bytes()), operations=[*runtime.OPS, 'observe'], destinations=['github:owner/af'],
            max_wall_ms=30000, max_output_bytes=65536)), task_plans=[], task_templates=[], decision_keys={}, file_roots=[],
            performance_publications={}, nightly_releases=dict(nightly=dict(repository='owner/af', configuration_digest=fixture.service.digest)))
        bundle = install.bundle(fixture.service.digest, 'owner/af')
        for name, value in [('policy', policy), ('scenario', bundle), ('inputs', dict(nightly_tag=dict(type='nightly_tag', value='nightly-2026.11.16')))]:
            path = root / (name + '.json')
            path.write_bytes(document_bytes(value))
            path.chmod(0o600)

        def cli(*args, success=True):
            # HOME and all XDG selectors come from crate::common::af's isolated environment.
            result = subprocess.run([af, *args, '--state', str(state), '--json'], cwd=repo,
                stdout=subprocess.PIPE, stderr=subprocess.PIPE, timeout=60)
            if success:
                assert result.returncode == 0, (args, result.stdout, result.stderr)
                return json.loads(result.stdout)
            assert result.returncode != 0, (args, result.stdout)
            return result

        pin = cli('scenario', 'validate', '--file', str(root / 'scenario.json'), '--authority', str(root / 'policy.json'))
        common = ['--file', str(root / 'scenario.json'), '--authority', str(root / 'policy.json'),
                  '--inputs', str(root / 'inputs.json'), '--confirm-pin', pin['revision']]
        waiting = cli('scenario', 'start', 'explicit-night', *common, '--execute')
        assert waiting['status'] == 'waiting', waiting
        calls = []
        class Heavy:
            def run(self, job, op, resumed):
                calls.append(op)
                artifacts = {'build': sha256_bytes(op.encode())}
                if comparison_proof is not None:
                    manifest, comparison = comparison_proof()
                    assert comparison['status'] == 'complete' and comparison['verification'] == 'verified'
                    artifacts.update(manifest=runtime.canonical_digest(manifest), comparison=runtime.canonical_digest(comparison))
                return artifacts, 'external heavy fixture receipt'
        for index, op in enumerate(runtime.OPS):
            assert runtime.Service(fixture.file).worker(Heavy()) == 'done'
            assert runtime.Service(fixture.file).worker(Heavy()) == 'idle'
            run = cli('scenario', 'resume', 'explicit-night')
            assert run['status'] == ('succeeded' if index == 3 else 'waiting'), run
        assert calls == list(runtime.OPS)
        assert cli('scenario', 'resume', 'explicit-night')['status'] == 'succeeded'
        assert len(list(fixture.jobs.glob('nightly-????.??.??.json'))) == 1
        # A malformed calendar date and an altered machine baseline/config pin fail closed.
        (root / 'inputs.json').write_bytes(document_bytes(dict(nightly_tag=dict(type='nightly_tag', value='nightly-2026.02.29'))))
        cli('scenario', 'start', 'bad-night', *common, '--execute', success=False)
        (root / 'nightly.toml').write_bytes(b'baseline_ref = "v9.9.9"\n')
        try:
            runtime.Service(fixture.file)
        except runtime.ContractError:
            pass
        else:
            raise AssertionError('changed baseline configuration admitted')
        print('four async phases, restart/idempotency, invalid date and changed baseline: passed')
    finally:
        fixture.doCleanups()


if __name__ == '__main__':
    if len(sys.argv) == 3 and sys.argv[2] == '--native-family':
        from tests.test_release_comparison import Comparison
        # The Rust real-CLI test requires this path: a real native comparison/export,
        # with only computation/diagnostic external children supplied by the fixture.
        os.environ['AF_NIGHTLY_CLI_BIN'] = sys.argv[1]
        case = Comparison('test_complete_seed_free_comparison_reports_exports_and_verifies_copied_natively')
        case.setUp()
        try:
            case.test_complete_seed_free_comparison_reports_exports_and_verifies_copied_natively()
        finally:
            case.doCleanups()
    else:
        main(sys.argv[1])
