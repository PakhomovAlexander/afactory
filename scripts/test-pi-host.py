#!/usr/bin/env python3
"""Token-free tests of AF's embedded Pi host; official SDK is optional in CI."""
import os
import json
import shlex
import base64
from pathlib import Path
import shutil
import subprocess
import tempfile
import unittest
import hashlib
import importlib.util
import sys
from unittest.mock import patch

ROOT = Path(__file__).resolve().parent.parent
HOST = ROOT / 'crates/review-runner-pi/src/host.mjs'
SDK_PACKAGE = Path('node_modules/@earendil-works/pi-coding-agent/dist/index.js')

# -I does not itself disable bytecode writes by importlib. Load the helper without creating
# source-tree caches, including when this suite is invoked without -B by the existing gate.
sys.dont_write_bytecode = True
_spec = importlib.util.spec_from_file_location('pi_validation', ROOT / 'scripts/pi-validation.py')
VALIDATION = importlib.util.module_from_spec(_spec)
_spec.loader.exec_module(VALIDATION)


class ValidationTests(unittest.TestCase):
    def setUp(self):
        directory = tempfile.TemporaryDirectory()
        self.addCleanup(directory.cleanup)
        self.root = Path(directory.name)
        (self.root / 'docs/design').mkdir(parents=True)
        (self.root / 'source').write_text('original source')
        self.record = self.root / VALIDATION.RECORD
        self.claim = {
            'schema': 'af/pi-validation-claim@1', **VALIDATION.projection(self.root),
            'command': 'make perf-kernel-measure', 'filter': VALIDATION.FILTER,
            'exit_code': 0,
            'summary': 'Summary [   1.234s] 1 test run: 1 passed (1 slow), 10 skipped',
            'log_sha256': 'sha256:' + 'a' * 64}
        self.write_claim()

    def write_claim(self, suffix=''):
        self.record.write_text(VALIDATION.BEGIN + '\n```json\n' +
                               json.dumps(self.claim) + '\n```\n' + VALIDATION.END + suffix)

    def cli(self, *args):
        return subprocess.run([sys.executable, '-I', '-B', str(ROOT / 'scripts/pi-validation.py'),
                               '--root', str(self.root), '--record', str(self.record),
                               '--claim-source', self.claim.get('source_sha256', 'sha256:' + 'f' * 64),
                               '--check', *args],
                              capture_output=True, text=True, timeout=10, check=False)

    def test_future_checkout_needs_no_new_measurement_and_no_implicit_source_is_verified(self):
        # Normal functional CI parses retained evidence, never compares it to today's tree.
        claims = VALIDATION.read_claims(ROOT / VALIDATION.RECORD)
        self.assertGreaterEqual(len(claims), 6)
        before = self.record.read_bytes()
        (self.root / 'unrelated-future-change').write_text('new release content')
        self.assertEqual(VALIDATION.read_claims(self.record)[self.claim['source_sha256']],
                         self.claim)
        self.assertEqual(self.record.read_bytes(), before)
        self.assertIn('does not match selected source', self.cli().stderr)
        for args in [[], ['--claim-source', self.claim['source_sha256']],
                     ['--root', str(self.root)]]:
            result = subprocess.run([sys.executable, '-I', '-B',
                                     str(ROOT / 'scripts/pi-validation.py'), '--check', *args],
                                    capture_output=True, text=True, timeout=10, check=False)
            self.assertNotEqual(result.returncode, 0)
            self.assertIn('requires explicit --root and --claim-source', result.stderr)

    def test_explicit_selection_keeps_distinct_historical_claims_and_rejects_unknown_source(self):
        earlier = self.record.read_text()
        original_source = self.claim['source_sha256']
        (self.root / 'source').write_text('later measured fixture source')
        self.claim.update(VALIDATION.projection(self.root))
        self.write_claim()
        later = self.record.read_text()
        self.record.write_text('Historical af-pi-validation-claim prose.\n' + earlier +
                               '\n<!-- unrelated historical comment -->\n' + later + '\nEnd of record.\n')
        self.assertEqual(len(VALIDATION.read_claims(self.record)), 2)
        self.assertEqual(self.cli().returncode, 0)
        wrong = self.cli('--claim-source', original_source)
        self.assertNotEqual(wrong.returncode, 0)
        self.assertIn('does not match selected source', wrong.stderr)
        (self.root / 'source').write_text('original source')
        self.assertEqual(self.cli('--claim-source', original_source).returncode, 0)
        unknown = self.cli('--claim-source', 'sha256:' + '0' * 64)
        self.assertNotEqual(unknown.returncode, 0)
        self.assertIn('selected historical claim not found', unknown.stderr)
        # Even an unselected malformed or duplicate record must fail closed.
        self.record.write_text(earlier + '\n' + later + '\n' + later)
        self.assertIn('duplicate historical source claim', self.cli().stderr)
        self.record.write_text(earlier + '\n' + later.replace(':v1 -->', ':v2 -->'))
        self.assertIn('unsupported or malformed', self.cli().stderr)

    def test_catalog_adapter_rc4_and_premerge_rc5_records_keep_original_values(self):
        claims = VALIDATION.read_claims(ROOT / VALIDATION.RECORD)
        for digest, entries, files, log, summary in [
                ('7f9261ba019aa397fb84cae3673d0e04d3e3ca0e3d11afc75e1575dcce16d8a3',
                 3859, 3172, 'c264cfb6ccd88b5f55310bcff46c331769a9afe1442e60af995d6fd61e02790c',
                 'Summary [ 912.403s] 1 test run: 1 passed (1 slow), 4010 skipped'),
                ('aa0f0012b53e0b8ad6448b7213cd4bfd97c10f38bffed193ff64356991f22144',
                 3883, 3195, 'b9cc4fa3bd5c082f6e5a31adfbda4a1840670abc9e8c0758ac156f9c5fc43f5e',
                 'Summary [ 792.681s] 1 test run: 1 passed (1 slow), 4014 skipped'),
                ('2339691c0b34404684686e27422b3d90f7f7db4fdda2ecc3c59dae137cf7ae14',
                 3799, 3112, '790aedf44dd4a37b8db3d4a61e11ebe0d0ab69b40fb1f0fb5694282c02185796',
                 'Summary [1323.393s] 1 test run: 1 passed (1 slow), 4010 skipped'),
                ('41cf48452e806ca51167b55b05165fd22f197c286f662cba97cac2119084e955',
                 3799, 3112, '369a647a1c971b6756d25afdddd23ce9bd7f5d43086f59b66db67c83783f12a8',
                 'Summary [ 817.770s] 1 test run: 1 passed (1 slow), 4010 skipped')]:
            with self.subTest(source=digest):
                claim = claims['sha256:' + digest]
                self.assertEqual((claim['entries'], claim['files'], claim['log_sha256'],
                                  claim['summary']), (entries, files, 'sha256:' + log, summary))

    def test_source_mutation_and_stale_counts_refuse_without_rewriting_record(self):
        self.assertEqual(self.cli().returncode, 0)
        before = self.record.read_bytes()
        (self.root / 'source').write_text('mutated source')
        result = self.cli()
        self.assertNotEqual(result.returncode, 0)
        self.assertIn('does not match selected source', result.stderr)
        self.assertEqual(self.record.read_bytes(), before)
        (self.root / 'source').write_text('original source')
        for field in ['entries', 'files', 'source_sha256']:
            with self.subTest(field=field):
                original = self.claim[field]
                self.claim[field] = original + 1 if isinstance(original, int) else 'sha256:' + '0'*64
                self.write_claim()
                self.assertNotEqual(self.cli().returncode, 0)
                self.claim[field] = original

    def test_exact_three_exclusions_allow_record_edits_but_include_nested_and_hidden_paths(self):
        self.write_claim('\nHistorical prose may be corrected without a new measurement.\n')
        for name in ['.git', 'target']:
            (self.root / name).mkdir()
            (self.root / name / 'scratch').write_text('excluded')
        self.assertEqual(self.cli().returncode, 0)
        for name in ['docs/.git', 'docs/target', '.af', '.config', 'docs/other.md']:
            with self.subTest(path=name):
                path = self.root / name
                path.write_text('included')
                self.assertNotEqual(self.cli().returncode, 0)
                path.unlink()
        self.assertEqual(self.cli().returncode, 0)

    def test_projection_matches_independent_rows_and_tracks_symlinks_and_executable_bits(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            (root / 'd').mkdir()
            (root / 'd/\u2028.txt').write_bytes(b'content')
            (root / 'link').symlink_to('d/\u2028.txt')
            rows = [['d', 'directory', False, None],
                    ['d/\u2028.txt', 'file', False, hashlib.sha256(b'content').hexdigest()],
                    ['link', 'symlink', False,
                     hashlib.sha256(os.fsencode('d/\u2028.txt')).hexdigest()]]
            wire = b'af.pi.validation-source/1\0' + b''.join(
                (json.dumps(row, ensure_ascii=True, separators=(',', ':')) + '\n').encode('ascii')
                for row in rows)
            expected = {'source_sha256': 'sha256:' + hashlib.sha256(wire).hexdigest(),
                        'entries': 3, 'files': 1}
            self.assertEqual(VALIDATION.projection(root), expected)
            (root / 'd/\u2028.txt').chmod(0o700)
            self.assertNotEqual(VALIDATION.projection(root), expected)
            (root / 'd/\u2028.txt').chmod(0o600)
            self.assertEqual(VALIDATION.projection(root), expected)
            (root / 'link').unlink()
            (root / 'link').symlink_to('missing')
            self.assertNotEqual(VALIDATION.projection(root), expected)

    def test_claim_is_closed_versioned_unique_and_never_selects_exclusions(self):
        for field, value in [('schema', 'af/pi-validation-claim@0'), ('exclusions', ['source']),
                             ('exit_code', 1), ('entries', True), ('files', -1),
                             ('command', 'fake measurement'), ('log_sha256', 'invalid'),
                             ('filter', 'test(/anything/)'), ('summary', 'passed')]:
            with self.subTest(field=field):
                original = dict(self.claim)
                self.claim[field] = value
                self.write_claim()
                self.assertNotEqual(self.cli().returncode, 0)
                self.claim = original
        for field in list(self.claim):
            with self.subTest(missing=field):
                original = dict(self.claim)
                del self.claim[field]
                self.write_claim()
                self.assertNotEqual(self.cli().returncode, 0)
                self.claim = original
        self.write_claim()
        valid = self.record.read_text()
        for malformed in [valid + valid, valid.replace(':v1 -->', ':v0 -->'),
                          valid.replace(VALIDATION.END, ''),
                          valid.replace('af-pi-validation-claim', 'af-pi-validation-current'),
                          valid.replace('"schema":', '"entries": 0, "schema":')]:
            self.record.write_text(malformed)
            self.assertNotEqual(self.cli().returncode, 0)

    def test_unconsumed_opening_and_closing_claim_family_markers_fail_closed(self):
        valid = self.record.read_text()
        for marker in [
                VALIDATION.BEGIN, VALIDATION.END,
                '<!-- af-pi-validation-claim:v2 -->',
                '<!-- /af-pi-validation-claim:v2 -->',
                '<!-- /af-pi-validation-claim:v1 -->',
                '<!-- af-pi-validation-claim -->',
                '<!-- af-pi-validation-claim: -->',
                '<!-- af-pi-validation-claim_v2 -->',
                '<!-- // af-pi-validation-claim:v2 -->',
                '<!-- /af-pi-validation-claim: -->',
                '<!-- af-pi-validation-claim:v1',
                '<!-- /af-pi-validation-claim',
                '<!--af-pi-validation-claim:v1-->',
                '<!--/af-pi-validation-claim-->',
                '<!-- \t / af-pi-validation-claim:v2 -->',
                '<!-- /af-pi-validation-current:v2 -->',
                '<!-- /af-pi-validation-future:v2 -->',
                '<!-- af-pi-validation -->',
                '<!-- /af-pi-validation -->',
                '<!-- af-pi-validation-unknown:v99 -->',
                '<!--- af-pi-validation-claim:v1 --->',
                '<!---- /af-pi-validation-current --->',
                '<!-- note: af-pi-validation-future:v2 is reserved -->',
                '<!-- ordinary prefix\nthen af-pi-validation\nrest -->',
                '<!-- prefix <!-- af-pi-validation-future -->',
                '<!-- prefix af-pi-validation-future',
                '<!-- /af-pi-validation-current:v2 -->' * 2]:
            for position in ['before', 'after', 'replacement']:
                with self.subTest(marker=marker, position=position):
                    if position == 'replacement' and marker == VALIDATION.END:
                        continue
                    malformed = {'before': marker + '\n' + valid,
                                 'after': valid + '\n' + marker,
                                 'replacement': valid.replace(VALIDATION.END, marker)}[position]
                    self.record.write_text(malformed)
                    result = self.cli()
                    self.assertNotEqual(result.returncode, 0, result.stdout)
                    self.assertIn('unsupported or malformed', result.stderr)
        # Marker counts alone cannot detect a marker hidden inside a consumed block.
        self.claim['summary'] += ' <!-- /af-pi-validation-claim:v2 -->'
        self.write_claim()
        self.assertIn('unsupported or malformed', self.cli().stderr)

    def test_original_comment_boundaries_refuse_wrapped_and_nested_claim_delimiters(self):
        valid = self.record.read_text()
        for malformed in [
                '<!-- ordinary wrapper\n' + valid + '\n-->',
                '<!-- ordinary prefix ' + valid,
                valid.replace(VALIDATION.BEGIN,
                              '<!-- ordinary prefix ' + VALIDATION.BEGIN + ' -->'),
                valid.replace(VALIDATION.END,
                              '<!-- ordinary prefix ' + VALIDATION.END + ' -->'),
                valid.replace(VALIDATION.END, '<!-- ordinary prefix ' + VALIDATION.END),
                valid.replace(VALIDATION.BEGIN, VALIDATION.BEGIN.replace('-->', '--->')),
                valid.replace(VALIDATION.END, VALIDATION.END.replace('-->', '--->'))]:
            with self.subTest(record=malformed):
                self.record.write_text(malformed)
                with self.assertRaisesRegex(ValueError, 'unsupported or malformed'):
                    VALIDATION.read_claims(self.record)
                result = self.cli()
                self.assertNotEqual(result.returncode, 0, result.stdout)
                self.assertIn('unsupported or malformed', result.stderr)

    def test_reserved_comment_namespace_is_ascii_case_sensitive_and_prose_is_allowed(self):
        self.write_claim('\nHistorical af-pi-validation prose remains ordinary text.\n'
                         '<!-- ordinary historical comment -->\n'
                         '<!-- AF-PI-VALIDATION-claim:v2 -->\n'
                         '<!-- af-pi-Validation-current -->\n'
                         '<!-- af-pi-valid\u0430tion uses a Cyrillic a -->\n'
                         '<!-- unrelated dangling comment')
        # Complete delimiters may share a line with prose; no Markdown rendering rule applies.
        self.record.write_text('Ordinary prefix ' + self.record.read_text().replace(
            VALIDATION.END, VALIDATION.END + ' ordinary suffix'))
        self.assertEqual(VALIDATION.read_claims(self.record)[self.claim['source_sha256']],
                         self.claim)
        result = self.cli()
        self.assertEqual(result.returncode, 0, result.stderr)

    def test_supplied_log_checks_hash_source_filter_and_outcome(self):
        # Synthetic log checks the verifier, not a claimed execution of the measurement.
        log = self.root / 'target/fixture.log'
        log.parent.mkdir()
        lines = [VALIDATION.source_line({key: self.claim[key] for key in
                                        ['source_sha256', 'entries', 'files']}),
                 'command: make perf-kernel-measure', 'filter: ' + VALIDATION.FILTER,
                 '  ' + self.claim['summary'], 'make exit: 0']

        def bind_log(text):
            log.write_text(text)
            self.claim['log_sha256'] = 'sha256:' + hashlib.sha256(log.read_bytes()).hexdigest()
            self.write_claim()

        bind_log('\n'.join(lines) + '\n')
        verified = self.cli('--measurement-log', str(log))
        self.assertEqual(verified.returncode, 0, verified.stderr)
        self.assertIn('historical claim matches selected source; measurement log verified',
                      verified.stdout)
        log.write_text(log.read_text() + 'unbound change\n')
        self.assertIn('SHA256 differs', self.cli('--measurement-log', str(log)).stderr)
        for index, replacement in [(0, lines[0].replace(self.claim['source_sha256'], 'stale')),
                                   (1, 'command: fake'), (2, 'filter: fake'),
                                   (3, 'Summary [1s] 1 test run: 0 passed, 1 failed'),
                                   (4, 'make exit: 1')]:
            bad = list(lines)
            bad[index] = replacement
            bind_log('\n'.join(bad) + '\n')
            self.assertNotEqual(self.cli('--measurement-log', str(log)).returncode, 0)
        for duplicate in lines:
            bind_log('\n'.join(lines + [duplicate]) + '\n')
            self.assertNotEqual(self.cli('--measurement-log', str(log)).returncode, 0)
        log.unlink()
        self.assertNotEqual(self.cli('--measurement-log', str(log)).returncode, 0)
        source_only = self.cli()
        self.assertEqual(source_only.returncode, 0)
        self.assertIn('measurement log not supplied (not read)', source_only.stdout)


def installed_sdk():
    """Find a trusted external installation without executing a launcher or npm.

    An explicit test SDK is authoritative: a bad path fails instead of silently
    substituting fixture coverage. The host checks package identity and version.
    """
    if os.environ.get('AF_PI_TEST_HERMETIC') == '1':
        return None
    explicit = os.environ.get('AF_PI_TEST_SDK')
    if explicit:
        candidates = [Path(explicit).expanduser()]
        if not candidates[0].is_absolute():
            raise ValueError('AF_PI_TEST_SDK must be an absolute installed SDK entrypoint')
    else:
        candidates = [Path.home() / '.pi/agent/install/releases/1.4.0' / SDK_PACKAGE]
        launcher = shutil.which('pi')
        if launcher:
            resolved = Path(launcher).resolve()
            candidates += [resolved.parent.parent / 'index.js',
                           resolved.parent.parent / 'install/releases/1.4.0' / SDK_PACKAGE]
    for candidate in candidates:
        entry = candidate.resolve()
        repository_owned = (entry.is_relative_to(ROOT) or any(
            (parent / '.git').exists() or (parent / '.af').is_dir()
            for parent in entry.parents))
        if entry.is_file() and not repository_owned:
            return str(entry)
        if explicit:
            raise ValueError('AF_PI_TEST_SDK must name an existing SDK outside repositories')
    return None


class HostTests(unittest.TestCase):
    def test_sdk_discovery_is_portable_explicit_and_never_executes_a_launcher(self):
        with tempfile.TemporaryDirectory() as directory:
            home = Path(directory)
            entry = home / '.pi/agent/install/releases/1.4.0' / SDK_PACKAGE
            entry.parent.mkdir(parents=True)
            entry.write_text('not executed by discovery')
            launcher = home / '.pi/agent/bin/pi'
            launcher.parent.mkdir(parents=True)
            launcher.write_text('not executed by discovery')
            with patch.dict(os.environ, {}, clear=True), patch.object(Path, 'home', return_value=home), \
                    patch.object(shutil, 'which', return_value=None):
                self.assertEqual(installed_sdk(), str(entry.resolve()))
                with patch.object(Path, 'home', return_value=home / 'empty'), \
                        patch.object(shutil, 'which', return_value=str(launcher)):
                    self.assertEqual(installed_sdk(), str(entry.resolve()))
                bundled_cli = entry.parent / 'bundle/cli.js'
                bundled_cli.parent.mkdir()
                bundled_cli.write_text('not executed by discovery')
                with patch.object(Path, 'home', return_value=home / 'empty'), \
                        patch.object(shutil, 'which', return_value=str(bundled_cli)):
                    self.assertEqual(installed_sdk(), str(entry.resolve()))
                with patch.dict(os.environ, {'AF_PI_TEST_SDK': str(entry)}):
                    self.assertEqual(installed_sdk(), str(entry.resolve()))
                for invalid in ['relative/index.js', str(home / 'missing'), str(HOST)]:
                    with patch.dict(os.environ, {'AF_PI_TEST_SDK': invalid}):
                        with self.assertRaises(ValueError):
                            installed_sdk()
                with patch.dict(os.environ, {'AF_PI_TEST_HERMETIC': '1'}):
                    self.assertIsNone(installed_sdk())
                entry.unlink()
                self.assertIsNone(installed_sdk())

    def test_host_entrypoint_probe_and_safe_auth_refusal(self):
        node = shutil.which('node')
        self.assertIsNotNone(node)
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            sdk = installed_sdk()
            official = bool(sdk)
            if not sdk:
                package = root / 'installed/node_modules/@earendil-works/pi-coding-agent'
                (package / 'dist/core').mkdir(parents=True)
                (package / 'package.json').write_text(json.dumps({
                    'name': '@earendil-works/pi-coding-agent', 'version': '1.4.0',
                    'type': 'module'}))
                state = root / 'state.json'
                state.write_text('{"mode":"review"}')
                source = (ROOT / 'fixtures/pi-sdk/index.mjs').read_text()
                (package / 'dist/index.js').write_text(source.replace(
                    'AF_FIXTURE_STATE', json.dumps(str(state))))
                shutil.copyfile(ROOT / 'fixtures/pi-sdk/auth-storage.mjs',
                                package / 'dist/core/auth-storage.js')
                for dependency in ('pi-ai', 'pi-agent-core'):
                    target = package.parent / dependency
                    (target / 'dist').mkdir(parents=True)
                    (target / 'package.json').write_text(json.dumps({
                        'name': '@earendil-works/' + dependency, 'version': '1.4.0',
                        'type': 'module'}))
                (package.parent / 'pi-ai/dist/models.js').write_text(
                    "export const getSupportedThinkingLevels=()=>['high'];")
                sdk = str(package / 'dist/index.js')
            auth = root / 'auth'
            auth.mkdir()
            request = {'schema': 'af.pi-host/3', 'type': 'init', 'backend': 'openai-codex',
                       'sdk': sdk, 'auth_dir': str(auth),
                       'auth_context_id': 'sha256:' + hashlib.sha256(os.fsencode(auth.resolve())).hexdigest(),
                       'principal': None,
                       'cwd': directory, 'home': directory, 'readable': [],
                       'writable': [], 'probe': True}
            claims = json.dumps({'https://api.openai.com/auth': {'chatgpt_account_id': 'SYNTHETIC_ACCOUNT'}}).encode()
            access = 'e30.' + base64.urlsafe_b64encode(claims).decode().rstrip('=') + '.synthetic'
            credential = {'type': 'oauth', 'accountId': 'SYNTHETIC_ACCOUNT',
                          'access': access, 'refresh': 'PRIVATE_REFRESH',
                          'expires': 9999999999999}
            for value, expected in [(credential, 'ready'),
                                    ({'type': 'api_key', 'key': '!touch ' + shlex.quote(str(root / 'helper-ran'))}, 'error'),
                                    ({**credential, 'accountId': ''}, 'error')]:
                original = json.dumps({'openai-codex': value})
                (auth / 'auth.json').write_text(original)
                result = subprocess.run([node, '--no-addons', str(HOST)],
                                        input=json.dumps(request) + '\n', text=True,
                                        capture_output=True, cwd=root,
                                        env={'HOME': directory, 'PI_OFFLINE': '1',
                                             'PI_TELEMETRY': 'off'}, timeout=30, check=False)
                self.assertFalse((root / 'helper-ran').exists(), 'credential helper executed')
                self.assertEqual((auth / 'auth.json').read_text(), original, 'probe edited auth')
                records = [json.loads(line) for line in result.stdout.splitlines()]
                self.assertEqual(len(records), 1, result.stderr)
                self.assertEqual(records[0]['type'], expected)
                if expected == 'ready':
                    ready = records[0]
                self.assertEqual(result.returncode, 0 if expected == 'ready' else 1)
                if expected == 'error':
                    self.assertEqual(records[0]['code'], 'unsupported_auth')
                for secret in [access, 'PRIVATE_REFRESH', 'SYNTHETIC_ACCOUNT',
                               '!credential-helper']:
                    self.assertNotIn(secret, result.stdout + result.stderr)

            # Exercise the real SDK's inference/event/tool loop with an in-process HTTP
            # response, never a service call. CI drives the same host via its SDK fixture.
            wrapper = root / 'invoke.mjs'
            wrapper.write_text(r'''
import assert from 'node:assert/strict';
import fs from 'node:fs';
import {pathToFileURL} from 'node:url';
import {zstdDecompressSync} from 'node:zlib';
let calls=0; let websockets=0;
const reply = {schema:'af.worker-reply/1',outputs:{report:[{summary:'SDK_RESULT'}]}};
globalThis.WebSocket = class { constructor() { websockets++; throw Error('WebSocket forbidden'); } };
globalThis.fetch = async (url, options) => {
  assert.equal(String(url),'https://chatgpt.com/backend-api/codex/responses');
  const headers = new Headers(options.headers);
  assert.equal(headers.get('chatgpt-account-id'),'SYNTHETIC_ACCOUNT');
  assert(headers.get('authorization').startsWith('Bearer e30.'));
  assert(++calls <= 2, 'hidden inference or HTTP retry');
  const body = headers.get('content-encoding')==='zstd'
    ? zstdDecompressSync(options.body).toString('utf8') : String(options.body);
  if (calls===2) {
    assert(body.includes('CONFINED_SDK_TOOL_RESULT'), 'read result missing from SDK turn');
    assert.equal(fs.readFileSync('turn-created/nested/file','utf8'),'SDK_NESTED_WRITE');
    const outputs = JSON.parse(body).input.filter(item=>item.type==='function_call_output');
    assert.equal(outputs.length,2, 'SDK must dispatch both registered tool schemas');
    assert(outputs.some(item=>JSON.stringify(item.output).includes('written')), 'write result missing from SDK turn');
  }
  const items = calls===1
    ? [{type:'function_call',id:'fc_1',call_id:'call_1',name:'read',arguments:'{"path":"inside"}'},
       {type:'function_call',id:'fc_2',call_id:'call_2',name:'write',arguments:'{"path":"turn-created/nested/file","content":"SDK_NESTED_WRITE"}'}]
    : [{type:'message',id:'msg_2',role:'assistant',status:'completed',content:[{type:'output_text',text:JSON.stringify(reply),annotations:[]}]}];
  const events = [
    ...items.flatMap((item,output_index)=>[
      {type:'response.output_item.added',output_index,item},
      {type:'response.output_item.done',output_index,item}]),
    {type:'response.completed',response:{id:'resp_'+calls,status:'completed',output:items,usage:{input_tokens:12,output_tokens:3,input_tokens_details:{cached_tokens:2},output_tokens_details:{reasoning_tokens:1}}}},
  ];
  if (process.argv[4]==='invalid' && calls===2) events.at(-1).response.usage.input_tokens=-1;
  return new Response(events.map(e=>'data: '+JSON.stringify(e)+'\n\n').join(''),
    {status:200,headers:{'content-type':'text/event-stream'}});
};
const host = await import(pathToFileURL(process.argv[2]).href);
await host.main();
assert.equal(websockets,0);
if (process.argv[3]==='official') assert.equal(calls,2);
''')
            (auth / 'auth.json').write_text(json.dumps({'openai-codex': credential}))
            (root / 'inside').write_text('CONFINED_SDK_TOOL_RESULT')
            selected = next(model for model in ready['models'] if 'high' in model['efforts'])
            invocation = {**request, 'probe': False, 'principal': ready['principal'],
                          'readable': [directory], 'writable': [directory]}
            prepare = {'schema': 'af.pi-host/3', 'type': 'prepare',
                       'model': selected['id'], 'effort': 'high',
                       'model_identity': selected['identity']}
            prompt = {'schema': 'af.pi-host/3', 'type': 'prompt', 'prompt_nonce': 'sha256:' + 'd'*64,
                      'text': 'Read inside, write turn-created/nested/file, then return the typed reply.'}
            result = subprocess.run([node, '--no-addons', str(wrapper), str(HOST),
                                     'official' if official else 'fixture'],
                                    input=''.join(json.dumps(record) + '\n' for record in
                                                  [invocation, prepare, prompt]),
                                    text=True, capture_output=True, cwd=root,
                                    env={'HOME': directory, 'PI_OFFLINE': '1',
                                         'PI_TELEMETRY': 'off'}, timeout=30, check=False)
            self.assertEqual(result.returncode, 0, result.stderr)
            records = [json.loads(line) for line in result.stdout.splitlines()]
            self.assertEqual(records[-1]['type'], 'settled', records)
            self.assertEqual(next(r for r in records if r['type'] == 'accepted')['prompt_nonce'],
                             prompt['prompt_nonce'])
            self.assertFalse(any(record['type'] == 'error' for record in records), records)
            finalized = [record for record in records if record['type'] == 'usage'
                         and record['complete']]
            self.assertEqual([record['inference'] for record in finalized],
                             [1, 2] if official else [1])
            if official:
                self.assertEqual(json.loads(records[-2]['text']), {
                    'schema': 'af.worker-reply/1', 'outputs': {'report': [{'summary': 'SDK_RESULT'}]}})
                self.assertEqual((root / 'turn-created/nested/file').read_text(), 'SDK_NESTED_WRITE')
                self.assertTrue(all(record['input'] == 12 and record['cache_read'] == 2
                                    and record['output'] == 3 and record['reasoning'] == 1
                                    and record['cache_write'] is None for record in finalized))
                self.assertEqual(sum(record['input'] - record['cache_read'] + record['output']
                                     for record in finalized), 26)
            # Malformed/missing challenges refuse at the real embedded-host entrypoint,
            # before accepted/inference; no upstream transport is reached.
            for challenge in [None, 'invalid']:
                bad_prompt = {**prompt, 'prompt_nonce': challenge}
                if challenge is None:
                    del bad_prompt['prompt_nonce']
                refused = subprocess.run([node, '--no-addons', str(HOST)],
                                         input=''.join(json.dumps(r) + '\n' for r in
                                                       [invocation, prepare, bad_prompt]),
                                         text=True, capture_output=True, cwd=root,
                                         env={'HOME': directory, 'PI_OFFLINE': '1',
                                              'PI_TELEMETRY': 'off'}, timeout=30, check=False)
                self.assertEqual(refused.returncode, 1, refused.stderr)
                refused_records = [json.loads(line) for line in refused.stdout.splitlines()]
                self.assertEqual(refused_records[-1], {'schema': 'af.pi-host/3',
                                                       'type': 'error', 'code': 'host_refused'})
                self.assertFalse(any(r['type'] in ['accepted', 'usage', 'message', 'settled']
                                     for r in refused_records))
                for secret in [access, 'PRIVATE_REFRESH', 'SYNTHETIC_ACCOUNT']:
                    self.assertNotIn(secret, refused.stdout + refused.stderr)
            print('official Pi SDK agent turn/read/nested write/typed reply/native usage: ' +
                  ('passed (mocked transport; no paid call)' if official else
                   'not run (SDK absent or hermetic mode; fixture protocol passed)'))
            for secret in [access, 'PRIVATE_REFRESH', 'SYNTHETIC_ACCOUNT']:
                self.assertNotIn(secret, result.stdout + result.stderr)
            if official:
                invalid = subprocess.run([node, '--no-addons', str(wrapper), str(HOST),
                                          'official', 'invalid'],
                                         input=''.join(json.dumps(record) + '\n' for record in
                                                       [invocation, prepare, prompt]),
                                         text=True, capture_output=True, cwd=root,
                                         env={'HOME': directory, 'PI_OFFLINE': '1',
                                              'PI_TELEMETRY': 'off'}, timeout=30, check=False)
                self.assertEqual(invalid.returncode, 0, invalid.stderr)
                records = [json.loads(line) for line in invalid.stdout.splitlines()]
                self.assertEqual(records[-1], {'schema': 'af.pi-host/3', 'type': 'error',
                                               'code': 'inference_failed'})
                self.assertFalse(any(record['type'] == 'settled' for record in records))
                observed = [record for record in records if record['type'] == 'usage']
                self.assertEqual(observed[-1]['output'], 3)
                self.assertIsNone(observed[-1]['input'])
                self.assertFalse(observed[-1]['complete'])
                for secret in [access, 'PRIVATE_REFRESH', 'SYNTHETIC_ACCOUNT']:
                    self.assertNotIn(secret, invalid.stdout + invalid.stderr)

    def test_kimi_oauth_identity_model_and_raw_usage_contract(self):
        node = shutil.which('node')
        self.assertIsNotNone(node)
        with tempfile.TemporaryDirectory() as directory:
            script = Path(directory) / 'contract.mjs'
            script.write_text(r'''
import assert from 'node:assert/strict';
import {pathToFileURL} from 'node:url';
const host = await import(pathToFileURL(process.argv[2]).href);
globalThis.fetch=async()=>{throw Error('Network forbidden during credential validation/refresh');};
globalThis.WebSocket=class{constructor(){throw Error('WebSocket forbidden');}};
const credential = {type:'oauth',access:'PRIVATE_ACCESS',refresh:'PRIVATE_REFRESH',expires:9999999999999};
assert.equal(host.principal(credential,'kimi-coding'),null);
for (const field of ['type','access','refresh','expires']) {
 const missing={...credential};delete missing[field];
 assert.throws(()=>host.principal(missing,'kimi-coding'));
}
for (const bad of [{...credential,type:'api_key'}, {...credential,access:''},
                   {...credential,refresh:''}, {...credential,access:42},
                   {...credential,refresh:null}, {...credential,expires:'later'},
                   {...credential,expires:NaN}, {...credential,expires:Infinity},
                   {type:'api_key',key:'!echo HELPER_MUST_NOT_RUN'},
                   {type:'command',command:'echo HELPER_MUST_NOT_RUN'}]) assert.throws(()=>host.principal(bad,'kimi-coding'));
// Opaque SDK-owned fields carry no AF authority, even when their names resemble it.
const metadata = {accountId:'NOT_AN_ACCOUNT_PRINCIPAL', key:'!echo HELPER_MUST_NOT_RUN',
  endpoint:'https://untrusted.invalid', settings:{thinking:'medium'}, helper:'DO_NOT_EXECUTE',
  nested:{secret:'PRIVATE_METADATA', values:[1,null,true]}, flags:['opaque'],
  generation:10, optional:null, label:'SDK_METADATA'};
const extended = {...credential,...metadata};
assert.equal(host.principal(extended,'kimi-coding'),null);
let current = extended;
const storage = {read:async()=>{throw Error('helper-capable read forbidden');}, async modify(id,fn) { assert.equal(id,'kimi-coding'); const next=await fn(current); if(next!==undefined)current=next;return current; }};
let sameContext = true;
const guarded = host.guardedCredentials(storage,null,'kimi-coding',()=>{if(!sameContext)throw Error('context_changed');});
await guarded.read('kimi-coding');
await guarded.modify('kimi-coding',async c=>({...c,access:'rotated'}));
const rotated = await guarded.read('kimi-coding');
assert.equal(rotated.access,'rotated');
for (const [key,value] of Object.entries(metadata)) assert.deepEqual(rotated[key],value);
await guarded.modify('kimi-coding',async()=>undefined);
assert.deepEqual(await guarded.read('kimi-coding'),rotated);
await assert.rejects(guarded.modify('kimi-coding',async c=>({...c,refresh:''})));
assert.deepEqual(current,rotated,'invalid refresh must not write');
sameContext=false;await assert.rejects(guarded.read('kimi-coding'));
await assert.rejects(guarded.modify('kimi-coding',async c=>c));sameContext=true;
await assert.rejects(guarded.modify('kimi-coding',async c=>{sameContext=false;return c;}));sameContext=true;
await assert.rejects(guarded.modify('kimi-coding',async c=>({...c,type:'api_key'})));
current={...credential,type:'api_key'};
await assert.rejects(guarded.read('kimi-coding'));
const model={provider:'kimi-coding',id:'k3',api:'openai-completions',baseUrl:'https://api.kimi.com/coding/v1'};
assert.match(host.modelIdentity(model),/^sha256:/);
for(const bad of [{...model,id:'kimi-for-coding'}, {...model,baseUrl:'http://api.kimi.com/coding/v1'},
                  {...model,baseUrl:'https://other.invalid'}, {...model,api:'anthropic-messages'},
                  {...model,provider:'other'}]) assert.throws(()=>host.modelIdentity(bad));
const aggregate={prompt_tokens:439,completion_tokens:38,completion_tokens_details:{reasoning_tokens:24}};
assert.deepEqual(host.rawUsage('kimi-coding',{usage:aggregate}),
 {input:439,output:38,cache_read:null,cache_write:null,reasoning:24});
const usage={prompt_tokens:12,completion_tokens:5,cached_tokens:3,completion_tokens_details:{reasoning_tokens:2}};
const expected={input:12,output:5,cache_read:3,cache_write:null,reasoning:2};
assert.deepEqual(host.rawUsage('kimi-coding',{usage}),expected);
assert.deepEqual(host.rawUsage('kimi-coding',{choices:[{usage}]}),expected);
assert.deepEqual(host.rawUsage('kimi-coding',{usage,choices:[{usage}]}),expected);
for(const cache of [{prompt_tokens_details:{cached_tokens:3}},{prompt_cache_hit_tokens:3}]) {
 const u={...usage,...cache};delete u.cached_tokens;
 assert.deepEqual(host.rawUsage('kimi-coding',{usage:u}),expected);
}
assert.equal(host.rawUsage('kimi-coding',{usage:null,choices:[{}]}),undefined);
assert.deepEqual(host.rawUsage('kimi-coding',{usage:{}}),{input:null,output:null,cache_read:null,cache_write:null,reasoning:null});
for(const value of [-1,1.2,'12',NaN,Infinity,Number.MAX_SAFE_INTEGER+1]) {
 assert.throws(()=>host.rawUsage('kimi-coding',{usage:{...usage,prompt_tokens:value}}));
}
assert.throws(()=>host.rawUsage('kimi-coding',{usage:{...usage,prompt_cache_hit_tokens:2}}));
assert.throws(()=>host.rawUsage('kimi-coding',{usage,choices:[{usage:{...usage,completion_tokens:6}}]}));
assert.deepEqual(host.rawUsage('kimi-coding',{usage:{prompt_tokens:0,completion_tokens:0,cached_tokens:0}}),
 {input:0,output:0,cache_read:0,cache_write:null,reasoning:null});
// Exercise the pinned SDK's locked storage/refresh interface with synthetic in-memory data.
if (process.argv[3]) {
 const sdk=await host.loadSdk(process.argv[3]);
 const actual=sdk.AuthStorage.inMemory({'kimi-coding':extended});
 const secured=host.guardedCredentials(actual,null,'kimi-coding');
 assert.deepEqual(await secured.read('kimi-coding'),extended);
 await secured.modify('kimi-coding',async c=>({...c,access:'rotated',expires:c.expires+1}));
 const saved=await secured.read('kimi-coding');
 for (const [key,value] of Object.entries(metadata)) assert.deepEqual(saved[key],value);
 assert.equal(saved.access,'rotated');
 await assert.rejects(secured.modify('kimi-coding',async c=>({...c,type:'api_key'})));
 assert.deepEqual(await secured.read('kimi-coding'),saved);
}
console.log('Kimi contract passed (synthetic credentials only)');
''')
            sdk = installed_sdk()
            result = subprocess.run([node, str(script), str(HOST)] + ([sdk] if sdk else []),
                                    capture_output=True, text=True, env={'HOME': directory},
                                    timeout=30, check=False)
            self.assertEqual(result.returncode, 0, result.stderr)
            for secret in ['PRIVATE_ACCESS', 'PRIVATE_REFRESH', 'PRIVATE_METADATA', 'NOT_AN_ACCOUNT_PRINCIPAL']:
                self.assertNotIn(secret, result.stdout + result.stderr)

    def test_kimi_host_observed_usage_tools_and_exact_thinking_with_mock_transport(self):
        node = shutil.which('node')
        self.assertIsNotNone(node)
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            sdk = installed_sdk()
            official = bool(sdk)
            if not sdk:
                package = root / 'installed/node_modules/@earendil-works/pi-coding-agent'
                (package / 'dist/core').mkdir(parents=True)
                state = root / 'state.json'
                state.write_text(json.dumps({'mode': 'kimi-tool', 'backend': 'kimi-coding'}))
                (package / 'dist/index.js').write_text(
                    (ROOT / 'fixtures/pi-sdk/index.mjs').read_text().replace(
                        'AF_FIXTURE_STATE', json.dumps(str(state))))
                shutil.copyfile(ROOT / 'fixtures/pi-sdk/auth-storage.mjs', package / 'dist/core/auth-storage.js')
                for name in ['pi-coding-agent', 'pi-ai', 'pi-agent-core']:
                    target = package.parent / name
                    target.mkdir(exist_ok=True)
                    (target / 'package.json').write_text(json.dumps({
                        'name': '@earendil-works/' + name, 'version': '1.4.0', 'type': 'module'}))
                (package.parent / 'pi-ai/dist').mkdir()
                (package.parent / 'pi-ai/dist/models.js').write_text(
                    "export const getSupportedThinkingLevels=()=>['low','high','max'];")
                sdk = str(package / 'dist/index.js')
            auth = root / 'auth'
            auth.mkdir()
            credential = {'type': 'oauth', 'access': 'KIMI_PRIVATE_ACCESS',
                          'refresh': 'KIMI_PRIVATE_REFRESH', 'expires': 9999999999999,
                          'sdkMetadata': {'opaque': 'KIMI_PRIVATE_METADATA'},
                          'endpoint': 'https://untrusted.invalid',
                          'settings': {'thinking': 'medium'},
                          'helper': '!echo HELPER_MUST_NOT_RUN',
                          'accountId': 'NOT_AN_ACCOUNT_PRINCIPAL'}
            (auth / 'auth.json').write_text(json.dumps({'kimi-coding': credential}))
            init = {'schema': 'af.pi-host/3', 'type': 'init', 'sdk': sdk, 'backend': 'kimi-coding',
                    'auth_dir': str(auth),
                    'auth_context_id': 'sha256:' + hashlib.sha256(os.fsencode(auth.resolve())).hexdigest(),
                    'principal': None, 'cwd': directory, 'home': directory,
                    'readable': [directory], 'writable': [directory], 'probe': True}
            def invoke(records, wrapper=None):
                return subprocess.run([node, '--no-addons', str(wrapper or HOST)],
                                      input=''.join(json.dumps(r) + '\n' for r in records),
                                      capture_output=True, text=True, cwd=root, timeout=30,
                                      env={'HOME': directory, 'PI_OFFLINE': '1', 'PI_TELEMETRY': 'off'},
                                      check=False)
            refused_context = invoke([{**init, 'auth_context_id': 'sha256:' + '0' * 64}])
            self.assertEqual(refused_context.returncode, 1)
            self.assertEqual(json.loads(refused_context.stdout)['code'], 'identity_changed')
            result = invoke([init])
            self.assertEqual(result.returncode, 0, result.stderr)
            ready = json.loads(result.stdout)
            self.assertIsNone(ready['principal'])
            self.assertEqual(ready['models'][0]['id'], 'k3')
            self.assertEqual(ready['models'][0]['efforts'], ['low', 'high', 'max'])
            prepare = {'schema': 'af.pi-host/3', 'type': 'prepare', 'model': 'k3',
                       'effort': 'high', 'model_identity': ready['models'][0]['identity']}
            for effort in ['off', 'medium', 'xhigh']:
                refused = invoke([{**init, 'probe': False}, {**prepare, 'effort': effort}])
                self.assertEqual(refused.returncode, 1)
                self.assertNotIn('accepted', refused.stdout)
            wrapper = root / 'invoke.mjs'
            wrapper.write_text(r'''
import assert from 'node:assert/strict';
import fs from 'node:fs';
import {pathToFileURL} from 'node:url';
let calls=0;
globalThis.WebSocket=class{constructor(){throw Error('WebSocket forbidden');}};
globalThis.fetch=async(url,options)=>{
 assert.equal(String(url),'https://api.kimi.com/coding/v1/chat/completions');
 assert.equal(new Headers(options.headers).get('authorization'),'Bearer KIMI_PRIVATE_ACCESS');
 const body=JSON.parse(String(options.body));assert.equal(body.model,'k3');assert.equal(body.reasoning_effort,'high');
 assert(++calls<=2,'hidden inference or retry');
 if(calls===2){assert(body.messages.some(m=>m.role==='tool'));assert.equal(fs.readFileSync('kimi-edited','utf8'),'confined Kimi edit');}
 const delta=calls===1 ? {role:'assistant',tool_calls:[{index:0,id:'tool-1',type:'function',function:{name:'write',arguments:'{"path":"kimi-edited","content":"confined Kimi edit"}'}}]}
 : {role:'assistant',content:'KIMI_FIXTURE_RESULT'};
 const chunk={id:'chat-'+calls,object:'chat.completion.chunk',created:1,model:'k3',choices:[{index:0,delta,finish_reason:null}]};
 const usage={prompt_tokens:12,completion_tokens:3,cached_tokens:2,completion_tokens_details:{reasoning_tokens:1}};
 const final={...chunk,choices:[{index:0,delta:{},finish_reason:calls===1?'tool_calls':'stop',...(calls===2?{usage}:{})}],...(calls===1?{usage}:{})};
 return new Response([chunk,final].map(c=>'data: '+JSON.stringify(c)+'\n\n').join('')+'data: [DONE]\n\n',
 {headers:{'content-type':'text/event-stream'}});
};
const host=await import(pathToFileURL(HOST_PATH).href);await host.main();
'''.replace('HOST_PATH', json.dumps(str(HOST))))
            prompt = {'schema': 'af.pi-host/3', 'type': 'prompt', 'text': 'Use the confined write tool.',
                      'prompt_nonce': 'sha256:' + 'e' * 64}
            result = invoke([{**init, 'probe': False}, prepare, prompt], wrapper)
            self.assertEqual(result.returncode, 0, result.stderr)
            records = [json.loads(line) for line in result.stdout.splitlines()]
            self.assertEqual(records[-1]['type'], 'settled', records)
            finalized = [r for r in records if r['type'] == 'usage' and r['complete']]
            self.assertEqual(len(finalized), 2 if official else 1)
            self.assertTrue(all((r['input'], r['output'], r['cache_read'], r['reasoning']) ==
                                (12, 3, 2, 1) for r in finalized))
            self.assertEqual((root / 'kimi-edited').read_text(), 'confined Kimi edit')
            for secret in (credential['access'], credential['refresh'], 'KIMI_PRIVATE_METADATA',
                           'NOT_AN_ACCOUNT_PRINCIPAL', 'HELPER_MUST_NOT_RUN'):
                self.assertNotIn(secret, result.stdout + result.stderr)
            print('Kimi SDK tools and raw usage: passed (' +
                  ('official SDK, mocked HTTP; no live call)' if official else 'hermetic SDK fixture)'))

    def test_confined_tools_identity_rotation_and_resource_contract(self):
        node = shutil.which('node')
        self.assertIsNotNone(node, 'host fixture requires Node, also used by markdownlint')
        with tempfile.TemporaryDirectory() as directory:
            script = Path(directory) / 'test.mjs'
            script.write_text(r'''
import assert from 'node:assert/strict';
import fs from 'node:fs';
import path from 'node:path';
import {pathToFileURL} from 'node:url';
globalThis.fetch = async () => { throw Error('Network forbidden during SDK construction/tools'); };
globalThis.WebSocket = class { constructor() { throw Error('WebSocket forbidden'); } };
const host = await import(pathToFileURL(process.argv[2]).href);
const root = path.dirname(process.argv[1]);
const work = path.join(root, 'work'); fs.mkdirSync(work);
const base = path.join(root, 'base'); fs.mkdirSync(base);
fs.writeFileSync(path.join(root,'outside'),'SECRET_MARKER');
fs.writeFileSync(path.join(base,'before'),'base');
fs.symlinkSync(path.join(root,'outside'),path.join(work,'link'));
fs.symlinkSync(root,path.join(work,'dirlink'));
fs.writeFileSync(path.join(work,'inside'),'safe');
const tools = host.confinedTools(work,[work,base],[work]);
const run = (name,args) => tools.find(t=>t.name===name).execute('test',args);
await assert.rejects(run('write',{path:'.',content:'bad'}));
for (const target of [path.join(root,'outside'),'../outside','link','dirlink/outside']) {
  for (const name of ['read','write','edit','grep','find','ls']) {
    await assert.rejects(run(name,{path:target,content:'bad',oldText:'SECRET',newText:'bad',pattern:'SECRET'}));
  }
}
assert.equal(fs.readFileSync(path.join(root,'outside'),'utf8'),'SECRET_MARKER');
assert.equal((await run('read',{path:'../base/before'})).content[0].text,'base');
assert.equal((await run('read',{path:path.join(work,'inside')})).content[0].text,'safe');
await assert.rejects(run('write',{path:'../base/before',content:'bad'}));
await run('write',{path:'new',content:'first'});
await run('write',{path:'new-directory/nested/file',content:'created inside grant'});
assert.equal(fs.readFileSync(path.join(work,'new-directory/nested/file'),'utf8'),'created inside grant');
await run('edit',{path:'new',oldText:'first',newText:'next'});
assert.equal((await run('read',{path:'new'})).content[0].text,'next');
assert((await run('grep',{path:'.',pattern:'safe'})).content[0].text.includes('inside'));
assert(!(await run('ls',{path:'.'})).content[0].text.includes('link'));
assert.deepEqual(host.confinedTools(work,[work],[]).map(t=>t.name).sort(),['find','grep','ls','read']);
assert(!tools.some(t=>['bash','shell','codemode','tool_search'].includes(t.name)));
const controller = new AbortController(); controller.abort();
await assert.rejects(tools[0].execute('test',{path:'inside'},controller.signal));
const token = (account,nonce='original') => 'e30.' + Buffer.from(JSON.stringify({'https://api.openai.com/auth':{chatgpt_account_id:account},nonce})).toString('base64url') + '.synthetic';
const credential = {type:'oauth',accountId:'synthetic-account',access:token('synthetic-account'),refresh:'PRIVATE_REFRESH',expires:100};
const id = host.principal(credential);
assert(!id.includes('PRIVATE')); assert.equal(id,host.principal({...credential,access:token('synthetic-account','rotation')}));
assert.notEqual(id,host.principal({...credential,accountId:'replacement',access:token('replacement')}));
assert.throws(()=>host.principal({...credential,access:token('replacement')}));
for (const bad of [undefined,{type:'api_key',key:'!echo secret'},{...credential,accountId:''}]) assert.throws(()=>host.principal(bad));
let current = credential;
const storage = {read:async()=>{throw Error('helper-capable read forbidden');}, modify:async(_id,fn)=>{const next=await fn(current); if(next!==undefined)current=next; return current;}};
const guarded = host.guardedCredentials(storage,id);
await guarded.read('openai-codex');
await guarded.modify('openai-codex',async(c)=>({...c,access:token('synthetic-account','rotation')}));
assert.equal(current.access,token('synthetic-account','rotation'));
await guarded.modify('openai-codex',async()=>undefined);
assert.equal(current.access,token('synthetic-account','rotation'));
await assert.rejects(guarded.modify('openai-codex',async(c)=>({...c,accountId:'replacement'})));
current={...credential,accountId:'replacement',access:token('replacement')};
await assert.rejects(guarded.read('openai-codex'));
assert.equal(await guarded.read('anthropic'),undefined);
let options;
const model = {id:'fixture',provider:'openai-codex',api:'openai-codex-responses',baseUrl:'https://chatgpt.com/backend-api'};
const sdk = {
  createExtensionRuntime:()=>({}),
  SettingsManager:{inMemory:(settings,opts)=>({settings,opts})},
  SessionManager:{inMemory:(cwd)=>({cwd})},
  createAgentSession:async(o)=>{
    options=o;
    return {session:{model,thinkingLevel:'high',getActiveToolNames:()=>o.tools,
      setAutoRetryEnabled:()=>{},setAutoCompactionEnabled:()=>{},dispose:()=>{}}};
  },
};
await host.createSession(sdk,{},model,'high',work,[work],[],root);
assert.equal(options.noTools,'all');
assert.equal(options.settingsManager.settings.retry.provider.maxRetries,0);
assert.equal(options.settingsManager.settings.retry.enabled,false);
assert.equal(options.settingsManager.settings.compaction.enabled,false);
assert.equal(options.settingsManager.settings.cacheWarming,'off');
assert.equal(options.settingsManager.opts.projectTrusted,false);
assert.deepEqual(options.resourceLoader.getExtensions().extensions,[]);
assert.deepEqual(options.resourceLoader.getAgentsFiles().agentsFiles,[]);
assert.deepEqual(options.resourceLoader.getSkills().skills,[]);
assert.deepEqual(options.resourceLoader.getPrompts().prompts,[]);
assert.deepEqual(options.resourceLoader.getThemes().themes,[]);
assert.throws(()=>options.resourceLoader.extendResources({}));
let disposed = false;
const changedSdk = {...sdk, createAgentSession:async(o)=>{
  const result = await sdk.createAgentSession(o);
  result.session.model = {...model,baseUrl:'https://refused.invalid'};
  result.session.dispose = ()=>{disposed=true;};
  return result;
}};
await assert.rejects(host.createSession(changedSdk,{},model,'high',work,[work],[],root));
assert(disposed, 'SDK endpoint drift must dispose before private input');
// Real SDK creation exercises the actual runtime and wrapped tool registry with no auth or call.
if (process.argv[3]) {
  const real = await host.loadSdk(process.argv[3]);
  const runtime = await host.createRuntime(real,real.AuthStorage.inMemory());
  const selected = runtime.getModels('openai-codex')[0];
  const result = await host.createSession(real,runtime,selected,'high',work,[work,base],[work],root);
  assert.equal(result.settings.getProviderRetrySettings().maxRetries,0);
  assert.equal(result.settings.getCompactionEnabled(),false);
  assert.equal(result.settings.getRetryEnabled(),false);
  assert.equal(result.settings.getCacheWarmingMode(),'off');
  assert.deepEqual(result.session.resourceLoader.getAgentsFiles().agentsFiles,[]);
  const active = result.session.agent.state.tools;
  assert.deepEqual(active.map(t=>t.name).sort(),['edit','find','grep','ls','read','write']);
  const read = active.find(t=>t.name==='read');
  assert.equal((await read.execute('test',{path:'inside'})).content[0].text,'safe');
  assert.equal((await read.execute('test',{path:'../base/before'})).content[0].text,'base');
  await assert.rejects(read.execute('test',{path:'../outside'}));
  await assert.rejects(read.execute('test',{path:'link'}));
  const write = active.find(t=>t.name==='write');
  await assert.rejects(write.execute('test',{path:'../outside',content:'bad'}));
  await assert.rejects(write.execute('test',{path:'../base/new/nested/file',content:'bad'}));
  await assert.rejects(write.execute('test',{path:'dirlink/new/nested/file',content:'bad'}));
  await write.execute('test',{path:'sdk-created/nested/file',content:'confined'});
  assert.equal(fs.readFileSync(path.join(work,'sdk-created/nested/file'),'utf8'),'confined');
  result.session.dispose();
  const readonly = await host.createSession(real,runtime,selected,'high',work,[work,base],[],root);
  assert.deepEqual(readonly.session.getActiveToolNames().sort(),['find','grep','ls','read']);
  readonly.session.dispose();
}
''')
            sdk = installed_sdk()
            env = {'HOME': directory, 'PI_OFFLINE': '1', 'PI_TELEMETRY': 'off'}
            # Synthetic ambient customizations must remain unobserved by session construction.
            agent = Path(directory) / '.pi/agent'
            agent.mkdir(parents=True)
            (agent / 'settings.json').write_text('{"extensions":["./sentinel.mjs"],'
                                               '"mcpServers":{"bad":{"command":"false"}}}')
            (agent / 'sentinel.mjs').write_text('throw new Error("AMBIENT_EXTENSION");')
            (agent / 'models.json').write_text('INVALID_AMBIENT_MODEL_OVERRIDE')
            (agent / 'auth.json').write_text('{"openai-codex":{"type":"api_key",'
                                            '"key":"!echo AMBIENT_HELPER"}}')
            (Path(directory) / 'AGENTS.md').write_text('AMBIENT_CONTEXT')
            result = subprocess.run([node, '--no-addons', str(script), str(HOST)] +
                                    ([sdk] if sdk else []), env=env, capture_output=True,
                                    text=True, timeout=30, check=False)
            self.assertEqual(result.returncode, 0, result.stderr)
            self.assertNotIn('PRIVATE_', result.stdout + result.stderr)
            print('official Pi SDK creation/tools: ' + ('passed (token-free)' if sdk else
                                                      'not run (SDK absent or hermetic mode; hermetic contract passed)'))


if __name__ == '__main__':
    unittest.main()
