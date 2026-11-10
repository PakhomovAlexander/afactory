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
HOST = ROOT / 'crates/af-runner-pi/src/host.mjs'
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
                    # Exercise production validation in-process (scoped issue537 trial); the
                    # hidden-summary case below keeps representative CLI stderr/exit coverage.
                    with self.assertRaisesRegex(ValueError, 'unsupported or malformed'):
                        VALIDATION.read_claims(self.record)
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


def fixture_sdk(root, state):
    """Install the existing synthetic SDK fixture in a test-private directory."""
    package = root / 'installed/node_modules/@earendil-works/pi-coding-agent'
    (package / 'dist/core').mkdir(parents=True)
    (package / 'dist/index.js').write_text((ROOT / 'fixtures/pi-sdk/index.mjs').read_text().replace('AF_FIXTURE_STATE', json.dumps(str(state))))
    shutil.copyfile(ROOT / 'fixtures/pi-sdk/auth-storage.mjs', package / 'dist/core/auth-storage.js')
    for name in ['pi-coding-agent', 'pi-ai', 'pi-agent-core']:
        target = package.parent / name
        target.mkdir(exist_ok=True)
        (target / 'package.json').write_text(json.dumps({'name': '@earendil-works/'+name, 'version': '1.4.0', 'type': 'module'}))
    (package.parent / 'pi-ai/dist').mkdir()
    (package.parent / 'pi-ai/dist/models.js').write_text("export const getSupportedThinkingLevels=m=>Object.keys(m.thinkingLevelMap ?? (m.reasoning?{high:'high'}:{off:'off'}));")
    return str(package / 'dist/index.js')


class HostTests(unittest.TestCase):
    def test_terminal_sdk_callback_sequence_and_sanitization_without_auth_io(self):
        node = shutil.which('node')
        self.assertIsNotNone(node)
        script = r'''
import assert from 'node:assert/strict';
import {pathToFileURL} from 'node:url';
const host = await import(pathToFileURL(process.argv[2]).href);
async function invoke(options = {}) {
  const records = []; let listener; let bound;
  const model = {id:'k3',provider:'kimi-coding',api:'openai-completions',maxTokens:32768};
  const diagnostic = host.diagnosticFilter();
  diagnostic.remember({access:'private-access',refresh:'private-refresh',type:'oauth'});
  const state = {request:{backend:model.provider},diagnostic,verifyContext:()=>{},
    credentials:{read:async()=>{}},checkAuth:async()=>{},effectiveIdentity:()=> 'pinned-model'};
  const message = {role:'assistant',provider:model.provider,model:options.wrongModel ? 'wrong-model' : model.id,
    stopReason:options.stop ?? 'toolUse',content:[{type:'text',text:'FINAL_REPLY'}]};
  const end = msg => listener({type:'message_end',message:msg});
  const session = {subscribe:fn=>{listener=fn;},dispose:()=>{},async prompt() {
    const entry = bound.beginInference(model);
    bound.observe(entry,{usage:options.usage ?? {prompt_tokens:439,completion_tokens:38,
      completion_tokens_details:{reasoning_tokens:24}}},model);
    end(message);
    if (options.late !== false) end({...message,stopReason:options.lateStop ?? 'error',
      errorMessage:options.missingDiagnostic ? undefined : options.diagnostic ?? 'Kimi terminal 764: tool-call continuation rejected'});
    if (options.replyAfterError) end({...message,stopReason:'stop'});
    if (options.continueAfterError) assert.throws(()=>bound.beginInference(model),/unsafe_accounting/);
    if (!options.unsettled) listener({type:'agent_settled',aborted:options.aborted ?? false});
    if (options.promptError) throw Error(options.promptError);
  }};
  bound = host.bindInferenceSession(session,state,model,{model_identity:'pinned-model'},
    new host.InferenceBudget(1000,model.provider),(type,data={})=>records.push({type,...data}));
  await bound.run('PRIVATE_PROMPT','sha256:'+'e'.repeat(64));
  return records;
}
const records = await invoke();
const finalized = records.filter(r=>r.type==='usage' && r.complete);
assert.equal(finalized.length,1);
assert.deepEqual(finalized[0],{type:'usage',inference:1,complete:true,observations:1,
  input:439,output:38,cache_read:null,cache_write:null,reasoning:24});
assert.deepEqual(records.at(-1),{type:'error',code:'inference_failed',
  diagnostic:'Kimi terminal 764: tool-call continuation rejected',failure_kind:'environment',accounting_complete:true});
assert(!records.some(r=>['message','settled'].includes(r.type)));
for (const options of [{replyAfterError:true},{aborted:true},{lateStop:'aborted'},
  {unsettled:true},{wrongModel:true},{usage:{prompt_tokens:-1,completion_tokens:38}},
  {usage:{completion_tokens:38}},{usage:{prompt_tokens:439,completion_tokens:38,cached_tokens:440}}]) {
  const refused = await invoke(options);
  assert.equal(refused.at(-1).type,'error',JSON.stringify(options));
  assert.equal(refused.at(-1).accounting_complete,false,JSON.stringify(options));
  assert(!refused.some(r=>['message','settled'].includes(r.type)));
}
assert.equal((await invoke({continueAfterError:true})).at(-1).type,'error');
const stop = await invoke({stop:'stop',late:false});
assert.equal(stop.at(-1).type,'settled');
assert.equal(stop.at(-2).text,'FINAL_REPLY');
const thrown = await invoke({stop:'stop',late:false,promptError:'Distinctive prompt exception 764'});
assert.equal(thrown.at(-1).diagnostic,'Distinctive prompt exception 764');
assert.equal(thrown.at(-1).accounting_complete,true);
assert(!thrown.some(r=>r.type==='message'));
const missing = await invoke({missingDiagnostic:true,promptError:'Actual prompt error after empty SDK error'});
assert.equal(missing.at(-1).diagnostic,'Actual prompt error after empty SDK error');
const first = await invoke({promptError:'later less useful prompt exception'});
assert.equal(first.at(-1).diagnostic,'Kimi terminal 764: tool-call continuation rejected');
for (const diagnostic of ['private-access','private-refresh','pri\nvate-access','Bearer short','api_key=short',
  'password short','https://user:pass@host/?key=short','{"details":"body"}','[1,2,3]','a'.repeat(5000),
  'safe text '.repeat(30)+'private-refresh']) {
  const terminal = (await invoke({diagnostic})).at(-1);
  assert(terminal.diagnostic.includes('withheld'));
  assert(terminal.diagnostic.length<=240);
  assert(terminal.accounting_complete);
}
const filter = host.diagnosticFilter();
assert.equal(filter.safe('\x1b[31mDistinctive\x1b[0m\nterminal\0 error'),'Distinctive terminal error');
const safeLong = Array(100).fill('safe diagnostic').join(' ');
assert.equal(filter.safe(safeLong).length,240);
assert.equal(filter.safe('{"error":{"message":"Distinctive structured SDK error"},"headers":"PRIVATE_PAYLOAD"}'),
  'Distinctive structured SDK error');
assert.equal(filter.safe('[SDK Error] Distinctive failure'),'[SDK Error] Distinctive failure');
// Refreshed/short literal credentials have no token prefix to guess from.
filter.remember({access:'rotated-small-value'});
assert(filter.safe('error rotated-small-value').includes('withheld'));
filter.remember({headers:{type:'violet aardvark'}});
assert(filter.safe('violet aardvark').includes('withheld'));
filter.remember({headers:{nested:{type:'\x1b[31mshort\x1b[0m\0credential'}}});
assert(filter.safe('short credential').includes('withheld'));
for (const [text, expected] of [
  ['HTTP 429 rate limit exceeded at https://api.example.com','quota'],
  ['\x1b[31mrate\x1b[0m\nlimit reached at https://api.example.com','quota'],
  ['{"error":{"message":"HTTP 429 at https://api.example.com"},"credential":"expired"}','quota'],
  ['HTTP 401 unauthorized: Bearer private-access','auth_rejected'],
  ['network connection failed at https://api.example.com','network'],
  ['refresh token failed: another refresh in progress','auth_refresh_contended'],
  ['credentials revoked','auth_revoked'], ['credentials expired','auth_expired'],
  ['missing credentials','auth_missing'], ['credential refresh failed','auth_refresh_failed'],
  ['model at capacity','capacity'], ['model unavailable','model_unavailable'],
  ['safeguards flagged this message','model_refused'], ['unknown SDK error','environment'],
]) {
  assert.equal(host.failureKind(text),expected);
  assert.equal((await invoke({diagnostic:text})).at(-1).failure_kind,expected);
}
// Structured codes classify neutral messages but never become diagnostic text.
for (const [payload, expected, diagnostic] of [
  [{error:{type:'rate_limit_error',message:'Please slow down'}},'quota','Please slow down'],
  [{error:{code:'model_not_found',message:'Request refused'}},'model_unavailable','Request refused'],
  [{type:'rate_limit_error',message:'Please slow down'},'quota','Please slow down'],
  [{code:'model_not_found',message:'Request refused'},'model_unavailable','Request refused'],
  [{error:{type:'rate_limit_error'}},'quota','[Pi diagnostic withheld: structured payload]'],
  [{error:{code:'model_not_found',message:'private-access'}},'model_unavailable','[Pi diagnostic withheld: sensitive text]'],
]) {
  const categorized = await invoke({diagnostic:JSON.stringify(payload)});
  const terminal = categorized.at(-1);
  assert.equal(terminal.failure_kind,expected);
  assert.equal(terminal.diagnostic,diagnostic);
  assert(terminal.accounting_complete);
  assert.deepEqual(categorized.filter(r=>r.type==='usage' && r.complete),finalized);
  assert(!categorized.some(r=>['message','settled'].includes(r.type)));
}
for (const payload of [
  {error:{type:['rate_limit_error'],message:'Neutral'}},
  {error:{code:{value:'model_not_found'},message:'Neutral'}},
  {error:{code:'x'.repeat(128)+'model_not_found',message:'Neutral'}},
  {error:{code:'model_not_found invalid',message:'Neutral'}},
  {details:{code:'model_not_found'},message:'Neutral'},
]) assert.equal(host.failureKind(JSON.stringify(payload)),'environment');
assert.equal(host.failureKind('x'.repeat(4096)+' HTTP 429'),'environment');
for (const file of ['[default]\naws_secret_access_key = orange-lantern-47\n',
  '{"type":"orange-lantern-47"}', 'orange-lantern-47\n', 'key = "orange-lantern-47"']) {
  const privateFile = host.diagnosticFilter();
  privateFile.rememberFile(Buffer.from(file));
  assert(privateFile.safe('orange-lantern-47').includes('withheld'));
}
// URL selectors hide their query values from public context identity, but a
// provider can echo only the decoded value. Remember every sensitive occurrence.
for (const [url, secrets] of [
  ['https://auth.example/path?token=briefvalue', ['briefvalue']],
  [' \nhttps://auth.example/path?token=briefvalue\n', ['briefvalue']],
  ['ht\ttps://auth.example/path?token=briefvalue', ['briefvalue']],
  ['https://auth.example/path?token=first&token=second&api_key=third', ['first','second','third']],
  ['https://auth.example/path?signature=short%2Dvalue&password=two+words', ['short-value','two words']],
  ['https://auth.example/path?auth=%E9%9B%AA', ['雪']],
  ['https://auth.example/path?secret=%00', ['unrepresentable diagnostic']],
  ['https://auth.example/path?token=%FF', ['unrepresentable diagnostic']],
]) {
  const privateFile = host.diagnosticFilter();
  privateFile.rememberFile(Buffer.from(JSON.stringify({credential_source:{url}})));
  for (const secret of secrets) assert(privateFile.safe('provider rejected '+secret).includes('withheld'),url);
}
const overflow = host.diagnosticFilter();
overflow.remember('x'.repeat(4097));
assert(overflow.safe('short SDK error').includes('withheld'));
'''
        with tempfile.TemporaryDirectory() as directory:
            contract = Path(directory) / 'terminal-callbacks.mjs'
            contract.write_text(script)
            result = subprocess.run([node, str(contract), str(HOST)], capture_output=True,
                                    text=True, timeout=30, check=False)
            self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
            # Refuse the previous protocol before loading an SDK or any credentials.
            old = subprocess.run([node, '--no-addons', str(HOST)],
                                 input=json.dumps({'schema': 'af.pi-host/6', 'type': 'init'}) + '\n',
                                 capture_output=True, text=True, timeout=30,
                                 env={'HOME': directory}, check=False)
            self.assertEqual(old.returncode, 1)
            self.assertEqual(json.loads(old.stdout), {'schema': 'af.pi-host/7', 'type': 'error',
                                                     'code': 'host_refused'})
            self.assertEqual(old.stderr, '')

    def test_terminal_sdk_error_after_complete_tool_use_retains_safe_diagnostic_and_charge(self):
        node = shutil.which('node')
        self.assertIsNotNone(node)
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            state = root / 'state.json'
            sdk = fixture_sdk(root, state)
            auth = root / 'auth'
            auth.mkdir()
            (auth / 'auth.json').write_text(json.dumps({'kimi-coding': {
                'type': 'oauth', 'access': 'PRIVATE_SHORT_ACCESS',
                'refresh': 'PRIVATE_SHORT_REFRESH', 'expires': 9999999999999}}))
            # Exercise the selected profile and captured credential-file closure,
            # including values with neither a recognizable prefix nor a secret label.
            def private(name, text):
                file = root / name
                file.write_text(text)
                file.chmod(0o600)
                return str(file.resolve())
            aws = private('aws-credentials', '[default]\naws_secret_access_key = orange-lantern-47\n')
            token = private('subject-token', 'teal-juniper-73')
            adc = private('adc.json', json.dumps({'private_key': 'silver marigold',
                                                'credential_source': {'file': token,
                                                                      'url': 'https://auth.example/path?token=briefvalue'}}))
            private('auth/models.json', json.dumps({'providers': {'kimi-coding': {
                'headers': {'type': 'violet aardvark'}}}}))
            private('auth/af-context.json', json.dumps({'schema': 'af.pi-context/1', 'environment': {
                'AWS_SHARED_CREDENTIALS_FILE': aws, 'GOOGLE_APPLICATION_CREDENTIALS': adc}}))
            init = {'schema': 'af.pi-host/7', 'type': 'init', 'sdk': sdk, 'backend': 'kimi-coding',
                    'auth_dir': str(auth), 'principal': None,
                    'auth_context_id': 'sha256:' + hashlib.sha256(os.fsencode(auth.resolve())).hexdigest(),
                    'cwd': directory, 'home': directory, 'readable': [directory], 'writable': [], 'probe': True}
            def invoke(inputs):
                result = subprocess.run([node, '--no-addons', str(HOST)],
                                        input=''.join(json.dumps(r) + '\n' for r in inputs),
                                        capture_output=True, text=True, cwd=root, timeout=30,
                                        env={'HOME': directory, 'PI_OFFLINE': '1'}, check=False)
                self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
                return [json.loads(line) for line in result.stdout.splitlines()]
            state.write_text(json.dumps({'backend': 'kimi-coding'}))
            ready = invoke([init])[0]
            prepare = {'schema': 'af.pi-host/7', 'type': 'prepare', 'model': 'k3', 'effort': 'high',
                       'model_identity': ready['models'][0]['identity'], 'remaining_tokens': 1000,
                       'reply_schema': None}
            prompt = {'schema': 'af.pi-host/7', 'type': 'prompt', 'text': 'PRIVATE_WORKER_REQUEST',
                      'prompt_nonce': 'sha256:' + 'e'*64}
            distinctive = 'Kimi terminal 764: tool-call continuation rejected'
            base = {'backend': 'kimi-coding', 'stopReason': 'toolUse',
                    'usage': {'prompt_tokens': 439, 'completion_tokens': 38,
                              'completion_tokens_details': {'reasoning_tokens': 24}},
                    'lateError': distinctive}
            for extra, complete in [({}, True), ({'wrongMessageModel': True}, False),
                                    ({'usage': {'prompt_tokens': -1, 'completion_tokens': 38}}, False)]:
                with self.subTest(extra=extra):
                    state.write_text(json.dumps({**base, **extra}))
                    records = invoke([{**init, 'probe': False}, prepare, prompt])
                    self.assertEqual(records[-1]['type'], 'error', records)
                    # Usage/model refusals can stop before the SDK emits its late error.
                    if 'usage' not in extra:
                        self.assertEqual(records[-1]['diagnostic'], distinctive)
                    self.assertEqual(records[-1]['accounting_complete'], complete)
                    self.assertFalse(any(r['type'] in ['message', 'settled'] for r in records))
                    usage = [r for r in records if r['type'] == 'usage'][-1]
                    self.assertEqual(usage['output'], 38)
                    if 'usage' not in extra:
                        self.assertEqual((usage['input'], usage['reasoning']), (439, 24))
                        self.assertEqual(usage['complete'], 'wrongMessageModel' not in extra)
            # A real final stop continues to succeed; a thrown prompt error after the
            # same accounted stream can never release that final reply.
            for thrown in [None, 'Distinctive prompt rejection 764']:
                control = {**base, 'stopReason': 'stop'}
                del control['lateError']
                if thrown:
                    control['promptError'] = thrown
                state.write_text(json.dumps(control))
                records = invoke([{**init, 'probe': False}, prepare, prompt])
                self.assertEqual(records[-1]['type'], 'error' if thrown else 'settled')
                if thrown:
                    self.assertEqual(records[-1]['diagnostic'], thrown)
                    self.assertTrue(records[-1]['accounting_complete'])
                    self.assertFalse(any(r['type'] == 'message' for r in records))
            # The real credential filter also recognizes short literal credentials.
            for unsafe in ['PRIVATE_SHORT_ACCESS', 'PRIVATE_SHORT_REFRESH', 'violet aardvark',
                           'orange-lantern-47', 'silver marigold', 'teal-juniper-73',
                           'provider rejected briefvalue']:
                state.write_text(json.dumps({**base, 'lateError': unsafe}))
                records = invoke([{**init, 'probe': False}, prepare, prompt])
                diagnostic = records[-1]['diagnostic']
                self.assertIn('withheld', diagnostic)
                self.assertLessEqual(len(diagnostic), 240)
                self.assertTrue(records[-1]['accounting_complete'])
                for secret in [unsafe, 'briefvalue', 'PRIVATE_SHORT_ACCESS', 'PRIVATE_SHORT_REFRESH', 'PRIVATE_WORKER_REQUEST']:
                    self.assertNotIn(secret, json.dumps(records))

    def test_official_catalog_invariant_covers_every_chat_provider_transport_and_native_usage(self):
        sdk = installed_sdk()
        if not sdk:
            print('official full catalog invariant: not run (SDK unavailable; hermetic transport invariant runs)')
        with tempfile.TemporaryDirectory() as directory:
            script = Path(directory) / 'catalog.mjs'
            script.write_text(r'''import assert from 'node:assert/strict';
import {pathToFileURL} from 'node:url';
const host=await import(pathToFileURL(process.argv[2]).href);
globalThis.fetch=async()=>{throw Error('network forbidden during catalog/auth resolution');};
globalThis.WebSocket=class{constructor(){throw Error('websocket forbidden');}};
// Native events, not initialized AssistantMessage counters. Every official wire API is covered.
const cases={
 'openai-completions':[{usage:{prompt_tokens:12,completion_tokens:3}},12,3],
 'mistral-conversations':[{usage:{prompt_tokens:12,completion_tokens:3}},12,3],
 'openai-responses':[{response:{usage:{input_tokens:12,output_tokens:3}}},12,3],
 'openai-codex-responses':[{response:{usage:{input_tokens:12,output_tokens:3}}},12,3],
 'azure-openai-responses':[{response:{usage:{input_tokens:12,output_tokens:3}}},12,3],
 'anthropic-messages':[{message:{usage:{input_tokens:7,cache_read_input_tokens:3,cache_creation_input_tokens:2,output_tokens:3}}},12,3],
 'google-generative-ai':[{usageMetadata:{promptTokenCount:12,candidatesTokenCount:2,thoughtsTokenCount:1,totalTokenCount:15}},12,3],
 'google-vertex':[{usageMetadata:{promptTokenCount:12,candidatesTokenCount:2,thoughtsTokenCount:1,totalTokenCount:15}},12,3],
 'bedrock-converse-stream':[{metadata:{usage:{inputTokens:12,outputTokens:3,cacheReadInputTokens:2}}},12,3],
 'pi-messages':[{usage:{input:7,output:3,cacheRead:3,cacheWrite:2}},12,3],
};
assert.deepEqual(Object.keys(cases).sort(),Object.keys(host.TRANSPORTS).sort());
for(const [api,[event,input,output]] of Object.entries(cases)) {
 const usage=host.rawUsage(api,event);assert.equal(usage.input,input,api);assert.equal(usage.output,output,api);
 assert.equal(host.rawUsage(api,{}),undefined,api);
 const budget=new host.InferenceBudget(20,'arbitrary-provider');
 const first=budget.begin({maxTokens:100});Object.assign(first,{observed:usage,finished:true,complete:true});
 assert.equal(budget.begin({maxTokens:100}).maxTokens,5,api);
}
// Anthropic optional caches default to zero only from native usage; mandatory fields do not.
let invalid=0;
for (const optional of [{},{cache_read_input_tokens:null,cache_creation_input_tokens:null},
                        {cache_read_input_tokens:3},{cache_creation_input_tokens:2}]) {
 const usage=host.rawUsage('anthropic-messages',{message:{usage:{input_tokens:7,output_tokens:3,...optional}}},()=>invalid++);
 assert.deepEqual(usage,{input:7+(optional.cache_read_input_tokens??0)+(optional.cache_creation_input_tokens??0),
  output:3,cache_read:optional.cache_read_input_tokens??0,cache_write:optional.cache_creation_input_tokens??0,reasoning:null});
}
assert.equal(invalid,0);
for (const field of ['input_tokens','output_tokens']) {
 const usage={input_tokens:7,output_tokens:3};delete usage[field];
 const result=host.rawUsage('anthropic-messages',{usage},()=>invalid++);
 assert(field==='input_tokens' ? invalid>0 && result.input===null : result.output===null,
  'mandatory counters cannot become fabricated zeros');
}

const components={};
const anthropic=usage=>host.rawUsage('anthropic-messages',{usage},()=>invalid++,components);
invalid=0;
assert.equal(anthropic({input_tokens:7,cache_read_input_tokens:3,cache_creation_input_tokens:2,output_tokens:0}).input,12);
assert.equal(anthropic({input_tokens:8}).input,13);
assert.deepEqual(anthropic({output_tokens:3,cache_read_input_tokens:null,cache_creation_input_tokens:null}),
 {input:13,output:3,cache_read:3,cache_write:2,reasoning:null});
assert.equal(invalid,0,'partial deltas retain known native components');
assert.equal(anthropic({input_tokens:9,cache_read_input_tokens:2}).input,14);
assert.equal(invalid,1,'component decrease refuses even when total input would increase');
assert.equal(anthropic({cache_creation_input_tokens:'unsafe'}).input,14);
assert.equal(invalid,2,'malformed component retains prior safe floor');
invalid=0;
assert.equal(host.rawUsage('google-generative-ai',{usageMetadata:{promptTokenCount:12,candidatesTokenCount:3}},()=>invalid++).output,3);
assert.equal(invalid,1);
if (process.argv[3]) {
 const sdk=await host.loadSdk(process.argv[3]);
 let selectedProvider,selectedCredential;
 const credentials={read:async id=>id===selectedProvider?selectedCredential:undefined,
  list:async()=>selectedCredential?[{providerId:selectedProvider,type:selectedCredential.type}]:[],
  modify:async(id,fn)=>{assert.equal(id,selectedProvider);const next=await fn(selectedCredential);if(next!==undefined)selectedCredential=next;return selectedCredential;},
  delete:async()=>{throw Error('no mutation');}};
 const runtime=await host.createRuntime(sdk,credentials);
 let models=0;const apis=new Set();
 for(const provider of runtime.getProviders()) for(const model of runtime.getModels(provider.id)) {
  assert.match(model.id,/^(?!\/)(?!.*[.][.])[A-Za-z0-9._/:@+~-]{1,256}$/);
  assert(cases[model.api],`unaccounted SDK transport ${model.api}`);
  assert.match(host.modelIdentity(model),/^sha256:/);
  assert(sdk.getSupportedThinkingLevels(model).every(l=>['off','minimal','low','medium','high','xhigh','max'].includes(l)));
  apis.add(model.api);models++;
 }
 assert(models>1000);assert.deepEqual([...apis].sort(),Object.keys(cases).sort());
 const nonChat=runtime.getAllModels().filter(m=>m.type && m.type!=='chat');
 assert(nonChat.length>0,'non-chat exclusion must exercise actual SDK catalog data');
 for(const model of nonChat) assert.throws(()=>host.modelIdentity(model));
 let oauth=0;
 for(const provider of runtime.getProviders().filter(p=>p.auth.oauth)) {
  selectedProvider=provider.id;
  selectedCredential={type:'oauth',access:'PRIVATE_SYNTHETIC_ACCESS',refresh:provider.id==='openrouter'?'':'PRIVATE_SYNTHETIC_REFRESH',
   expires:provider.id==='openrouter'?Number.MAX_SAFE_INTEGER:9999999999999};
  if(provider.id==='openai-codex') {
   selectedCredential.accountId='synthetic-account';
   selectedCredential.access='e30.'+Buffer.from(JSON.stringify({'https://api.openai.com/auth':{chatgpt_account_id:'synthetic-account'}})).toString('base64url')+'.synthetic';
  }
  const identity=host.principal(selectedCredential,provider.id);
  assert.equal(identity===null,provider.id!=='openai-codex');
  assert((await runtime.getAuth(provider.id,{env:{}}))?.auth,provider.id+' SDK OAuth credential resolution');oauth++;
 }
 console.log(`official catalog: ${runtime.getProviders().length} providers, ${models} chat models, ${apis.size} transports, ${oauth} OAuth resolvers; no model requests`);
}
''')
            result = subprocess.run([shutil.which('node'), str(script), str(HOST), sdk or ''],
                                    capture_output=True, text=True, timeout=30,
                                    env={'HOME': directory, 'PI_OFFLINE': '1'}, check=False)
            self.assertEqual(result.returncode, 0, result.stderr)
            print(result.stdout.strip())

    def test_provider_neutral_official_auth_catalog_dispatch_and_custom_cached_kimi(self):
        sdk = installed_sdk()
        official = bool(sdk)
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory).resolve()
            state = root / 'state.json'
            if not sdk:
                sdk = fixture_sdk(root, state)
            auth = root / 'profile'
            auth.mkdir(mode=0o700)
            home = root / 'host-home'
            home.mkdir()
            wrapper = root / 'streams.mjs'
            wrapper.write_text(r'''import assert from 'node:assert/strict';
import {pathToFileURL} from 'node:url';
let calls=0;
if(process.argv[4]) {
 const {createRequire}=await import('node:module');
 const require=createRequire(process.argv[4]);
 try {
  const {BedrockRuntimeClient}=await import(pathToFileURL(require.resolve('@aws-sdk/client-bedrock-runtime')).href);
  BedrockRuntimeClient.prototype.send=async function(command) {
   assert.equal(++calls,1);assert.equal(command.input.modelId,'amazon.nova-lite-v1:0');
   assert.equal(await this.config.maxAttempts(),1,'native cloud SDK retry chain must stay disabled');
   return {$metadata:{},stream:(async function*(){
    yield {messageStart:{role:'assistant'}};
    yield {contentBlockDelta:{contentBlockIndex:0,delta:{text:'OK'}}};
    yield {contentBlockStop:{contentBlockIndex:0}};
    yield {messageStop:{stopReason:'end_turn'}};
    yield {metadata:{usage:{inputTokens:12,outputTokens:3,totalTokens:15}}};
   })()};
  };
 } catch(error) { if(!String(process.argv[4]).includes('/installed/'))throw error; }
}
globalThis.WebSocket=class{constructor(){throw Error('websocket forbidden');}};
globalThis.fetch=async(url,options)=>{
 assert.equal(++calls,1,'retry or auxiliary inference');
 const target=String(url),body=JSON.parse(String(options.body));
 let events;
 if(target.includes('/messages')) {
  events=[{type:'message_start',message:{id:'m',role:'assistant',model:body.model,content:[],stop_reason:null,usage:{input_tokens:7,output_tokens:0,cache_read_input_tokens:3,cache_creation_input_tokens:2}}},
   {type:'content_block_start',index:0,content_block:{type:'text',text:''}},
   {type:'content_block_delta',index:0,delta:{type:'text_delta',text:'OK'}},
   {type:'content_block_stop',index:0},
   // A native input-only delta must retain previously observed cache components.
   {type:'message_delta',delta:{},usage:{input_tokens:7}},
   {type:'message_delta',delta:{stop_reason:'end_turn',stop_sequence:null},usage:{output_tokens:3}},
   {type:'message_stop'}];
 } else if(target.includes('generativelanguage')) {
  events=[{candidates:[{content:{role:'model',parts:[{text:'OK'}]},finishReason:'STOP',index:0}],
    usageMetadata:{promptTokenCount:12,candidatesTokenCount:2,thoughtsTokenCount:1,totalTokenCount:15}}];
 } else if(target.endsWith('/responses')) {
  const item={type:'message',id:'msg',role:'assistant',status:'completed',content:[{type:'output_text',text:'OK',annotations:[]}]};
  events=[{type:'response.output_item.added',output_index:0,item},{type:'response.output_item.done',output_index:0,item},
    {type:'response.completed',response:{id:'r',status:'completed',output:[item],usage:{input_tokens:12,output_tokens:3,input_tokens_details:{cached_tokens:2}}}}];
 } else {
  assert(target.endsWith('/chat/completions'),target);
  events=[{id:'c',object:'chat.completion.chunk',created:1,model:body.model,choices:[{index:0,delta:{role:'assistant',content:'OK'},finish_reason:null}]},
   {id:'c',object:'chat.completion.chunk',created:1,model:body.model,choices:[{index:0,delta:{},finish_reason:'stop'}],usage:{prompt_tokens:12,completion_tokens:3}}];
 }
 return new Response(events.map(e=>(target.includes('/messages')?'event: '+e.type+'\n':'')+'data: '+JSON.stringify(e)+'\n\n').join(''),
  {headers:{'content-type':'text/event-stream'}});
};
const host=await import(pathToFileURL(process.argv[2]).href);await host.main();
if(process.argv[3]==='invoke')assert.equal(calls,1);
''')
            def private_json(name, value):
                file = auth / name
                file.write_text(json.dumps(value))
                file.chmod(0o600)
            def invoke(records, turn=False):
                return subprocess.run([shutil.which('node'), '--no-addons', str(wrapper), str(HOST),
                                       'invoke' if turn and official else 'probe', sdk],
                                      input=''.join(json.dumps(r)+'\n' for r in records),
                                      capture_output=True, text=True, cwd=root, timeout=30,
                                      env={'HOME': str(home), 'PI_OFFLINE': '1'}, check=False)
            cases = [
                ('anthropic', 'claude-haiku-4-5', 'off', 'stored'),
                ('openai', 'gpt-4o-mini', 'off', 'stored'),
                ('google', 'gemini-2.5-flash', 'off', 'context'),
                ('amazon-bedrock', 'amazon.nova-lite-v1:0', 'off', 'cloud'),
                ('team-compatible', 'team-code', 'off', 'stored'),
                ('team-private', 'arn:aws:bedrock:us-east-1:123456789012:inference-profile/team', 'off', 'private'),
                ('registered-example', 'registered-code', 'off', 'extension'),
                ('kimi-coding', 'k3.1-pro', 'high', 'configured'),
                ('kimi-coding', 'k3.1-pro', 'high', 'cached'),
            ]
            for backend, model, effort, mode in cases:
                with self.subTest(backend=backend, mode=mode):
                    for name in ['models.json', 'models-store.json', 'af-context.json']:
                        (auth / name).unlink(missing_ok=True)
                    credential = {'type': 'api_key', 'key': 'PRIVATE_SYNTHETIC_KEY'}
                    if backend == 'kimi-coding':
                        credential = {'type': 'oauth', 'access': 'PRIVATE_ACCESS',
                                      'refresh': 'PRIVATE_REFRESH', 'expires': 9999999999999}
                    private_json('auth.json', {} if mode in ['context','cloud'] else {backend: credential})
                    if mode == 'cloud':
                        private_json('af-context.json', {'schema': 'af.pi-context/1', 'environment': {
                            'AWS_ACCESS_KEY_ID': 'PRIVATE_SYNTHETIC_KEY', 'AWS_SECRET_ACCESS_KEY': 'PRIVATE_CLOUD_SECRET',
                            'AWS_REGION': 'us-east-1'}})
                    api = {'anthropic': 'anthropic-messages', 'openai': 'openai-responses',
                           'google': 'google-generative-ai', 'amazon-bedrock': 'bedrock-converse-stream'}.get(backend, 'openai-completions')
                    if not official:
                        native = {
                            'anthropic-messages': {'message': {'usage': {'input_tokens': 7, 'output_tokens': 3, 'cache_read_input_tokens': 3, 'cache_creation_input_tokens': 2}}},
                            'openai-responses': {'response': {'usage': {'input_tokens': 12, 'output_tokens': 3}}},
                            'google-generative-ai': {'usageMetadata': {'promptTokenCount': 12, 'candidatesTokenCount': 2, 'thoughtsTokenCount': 1, 'totalTokenCount': 15}},
                            'bedrock-converse-stream': {'metadata': {'usage': {'inputTokens': 12, 'outputTokens': 3}}},
                        }.get(api, {'usage': {'prompt_tokens': 12, 'completion_tokens': 3}})
                        state.write_text(json.dumps({'backend': backend, 'model': model, 'api': api,
                                                    'providerEvents': [native], 'reasoning': effort != 'off'}))
                    if mode == 'context':
                        private_json('af-context.json', {'schema': 'af.pi-context/1',
                                                        'environment': {'GEMINI_API_KEY': 'PRIVATE_SYNTHETIC_KEY'}})
                    definition = {'id': model, 'name': model, 'api': 'openai-completions',
                                  'baseUrl': 'https://custom.invalid/v1' if backend in ['team-compatible','registered-example','team-private'] else 'https://api.kimi.com/coding/v1',
                                  'provider': backend, 'input': ['text'], 'reasoning': effort == 'high',
                                  'thinkingLevelMap': {'low': 'low', 'high': 'high', 'max': 'max'} if effort == 'high' else {'off': 'off'},
                                  'cost': {'input': 0, 'output': 0, 'cacheRead': 0, 'cacheWrite': 0},
                                  'contextWindow': 32768, 'maxTokens': 1024}
                    if mode == 'extension':
                        extension = root / 'registered-provider.mjs'
                        definition.pop('provider')
                        extension.write_text('export function register(api){api.registerProvider("registered-example",'+json.dumps({
                            'api': 'openai-completions', 'baseUrl': definition['baseUrl'], 'models': [definition]})+');}')
                        private_json('af-context.json', {'schema': 'af.pi-context/1', 'extension': {
                            'entrypoint': str(extension), 'sha256': 'sha256:'+hashlib.sha256(extension.read_bytes()).hexdigest()}})
                    if mode == 'cached':
                        private_json('models-store.json', {backend: {'models': [definition],
                                                                    'lastModified': 9999999999999}})
                    elif backend in ['team-compatible','team-private'] or mode == 'configured':
                        # Pi models.json data-only definitions, interpreted by its real composer.
                        custom = {k: v for k, v in definition.items() if k != 'provider'}
                        private_json('models.json', {'providers': {backend: {'api': 'openai-completions',
                                                                           'baseUrl': definition['baseUrl'], 'models': [custom]}}})
                    init = {'schema': 'af.pi-host/7', 'type': 'init', 'sdk': sdk,
                            'backend': backend, 'principal': None, 'auth_dir': str(auth),
                            'auth_context_id': 'sha256:'+hashlib.sha256(os.fsencode(auth.resolve())).hexdigest(),
                            'cwd': str(root), 'home': str(home), 'readable': [str(root)], 'writable': [], 'probe': True}
                    probe = invoke([init])
                    self.assertEqual(probe.returncode, 0, probe.stdout+probe.stderr)
                    ready = json.loads(probe.stdout)
                    self.assertIsNone(ready['principal'])
                    public_model = ('af-private-model/'+hashlib.sha256(('af.pi.private-model/1\0'+backend+'\0'+model).encode()).hexdigest()) if mode == 'private' else model
                    selected = next(m for m in ready['models'] if m['id'] == public_model)
                    if mode == 'private':
                        self.assertNotIn('123456789012', probe.stdout+probe.stderr)
                    self.assertIn(effort, selected['efforts'])
                    prepare = {'schema': 'af.pi-host/7', 'type': 'prepare', 'model': public_model,
                               'effort': effort, 'remaining_tokens': 1000, 'reply_schema': None,
                               'model_identity': selected['identity']}
                    prompt = {'schema': 'af.pi-host/7', 'type': 'prompt', 'text': 'Reply OK',
                              'prompt_nonce': 'sha256:'+'a'*64}
                    result = invoke([{**init, 'probe': False}, prepare, prompt], True)
                    self.assertEqual(result.returncode, 0, result.stdout+result.stderr)
                    records = [json.loads(line) for line in result.stdout.splitlines()]
                    self.assertEqual(records[-1]['type'], 'settled')
                    final = [r for r in records if r['type'] == 'usage' and r['complete']][-1]
                    self.assertEqual((final['input'], final['output']), (12, 3))
                    for secret in ['PRIVATE_SYNTHETIC_KEY', 'PRIVATE_ACCESS', 'PRIVATE_REFRESH', 'PRIVATE_CLOUD_SECRET', '123456789012']:
                        self.assertNotIn(secret, result.stdout+result.stderr)
                    refused = invoke([{**init, 'probe': False}, {**prepare, 'model': 'absent'}, prompt])
                    self.assertEqual(refused.returncode, 1)
                    self.assertNotIn('accepted', refused.stdout)
                    # Content-bound effective configuration, never a silent k3 substitution.
                    private_json('models.json', {'providers': {backend: {'baseUrl': 'https://changed.invalid/v1'}}})
                    drift = invoke([{**init, 'probe': False}, prepare, prompt])
                    self.assertEqual(drift.returncode, 1)
                    self.assertNotIn('accepted', drift.stdout)
            print('API-family synthetic session matrix exercised with '+('official SDK' if official else 'hermetic SDK fixture')+'; see unittest result; no live requests')

    def test_anthropic_optional_native_caches_settle_and_invalid_counters_keep_charge_floors(self):
        """Use real host sessions; optional SDK coverage uses only synthetic HTTP/keys."""
        official = installed_sdk()
        modes = [('hermetic SDK fixture', None)]
        if official:
            modes.append(('official SDK', official))
        else:
            print('Anthropic optional caches official SDK: not run (SDK unavailable)')
        for label, sdk in modes:
            with self.subTest(sdk=label), tempfile.TemporaryDirectory() as directory:
                root = Path(directory).resolve()
                state = root / 'state.json'
                if sdk is None:
                    sdk = fixture_sdk(root, state)
                auth = root / 'profile'
                auth.mkdir(mode=0o700)
                (auth / 'auth.json').write_text(json.dumps({
                    'anthropic': {'type': 'api_key', 'key': 'PRIVATE_SYNTHETIC_KEY'}}))
                (auth / 'auth.json').chmod(0o600)
                home = root / 'home'
                home.mkdir()
                model = 'claude-haiku-4-5'
                def configure(events, inferences=1):
                    state.write_text(json.dumps({'backend': 'anthropic', 'model': model,
                                                 'api': 'anthropic-messages', 'reasoning': False,
                                                 'providerEvents': events, 'inferences': inferences}))
                wrapper = root / 'anthropic.mjs'
                wrapper.write_text(r'''import fs from 'node:fs';
import assert from 'node:assert/strict';
import {pathToFileURL} from 'node:url';
let calls=0;
globalThis.WebSocket=class{constructor(){throw Error('websocket forbidden');}};
globalThis.fetch=async(url,options)=>{
 assert.equal(++calls,1,'retry or auxiliary inference');
 assert(String(url).includes('/messages'));
 assert.equal(JSON.parse(options.body).model,'claude-haiku-4-5');
 const events=JSON.parse(fs.readFileSync(process.argv[3],'utf8')).providerEvents;
 return new Response(events.map(e=>'event: '+e.type+'\ndata: '+JSON.stringify(e)+'\n\n').join(''),
  {headers:{'content-type':'text/event-stream'}});
};
const host=await import(pathToFileURL(process.argv[2]).href);await host.main();
''')
                def invoke(records):
                    return subprocess.run([shutil.which('node'), '--no-addons', str(wrapper),
                                           str(HOST), str(state)],
                                          input=''.join(json.dumps(r)+'\n' for r in records),
                                          capture_output=True, text=True, cwd=root, timeout=30,
                                          env={'HOME': str(home), 'PI_OFFLINE': '1'}, check=False)
                init = {'schema': 'af.pi-host/7', 'type': 'init', 'sdk': sdk,
                        'backend': 'anthropic', 'principal': None, 'auth_dir': str(auth),
                        'auth_context_id': 'sha256:'+hashlib.sha256(os.fsencode(auth)).hexdigest(),
                        'cwd': str(root), 'home': str(home), 'readable': [str(root)], 'writable': [], 'probe': True}
                configure([])
                probe = invoke([init])
                self.assertEqual(probe.returncode, 0, probe.stdout+probe.stderr)
                selected = next(m for m in json.loads(probe.stdout)['models'] if m['id'] == model)
                prepare = {'schema': 'af.pi-host/7', 'type': 'prepare', 'model': model,
                           'effort': 'off', 'remaining_tokens': 1000, 'reply_schema': None,
                           'model_identity': selected['identity']}
                prompt = {'schema': 'af.pi-host/7', 'type': 'prompt', 'text': 'Reply OK',
                          'prompt_nonce': 'sha256:'+'a'*64}
                def events(usage, delta):
                    return [
                        {'type': 'message_start', 'message': {'id': 'm', 'role': 'assistant',
                         'model': model, 'content': [], 'stop_reason': None, 'usage': usage}},
                        {'type': 'content_block_start', 'index': 0, 'content_block': {'type': 'text', 'text': ''}},
                        {'type': 'content_block_delta', 'index': 0, 'delta': {'type': 'text_delta', 'text': 'OK'}},
                        {'type': 'content_block_stop', 'index': 0},
                        {'type': 'message_delta', 'delta': {}, 'usage': delta},
                        {'type': 'message_delta', 'delta': {'stop_reason': 'end_turn', 'stop_sequence': None},
                         'usage': {'output_tokens': 3}},
                        {'type': 'message_stop'}]
                optional_cases = [{}, {'cache_read_input_tokens': None, 'cache_creation_input_tokens': None},
                                  {'cache_read_input_tokens': 3}, {'cache_creation_input_tokens': 2},
                                  {'cache_read_input_tokens': 3, 'cache_creation_input_tokens': 2}]
                for optional in optional_cases:
                    with self.subTest(optional=optional):
                        read = optional.get('cache_read_input_tokens') or 0
                        write = optional.get('cache_creation_input_tokens') or 0
                        # Increase uncached input while null/omitted cache fields retain prior components.
                        configure(events({'input_tokens': 7, 'output_tokens': 0, **optional},
                                         {'input_tokens': 8, 'cache_read_input_tokens': None}),
                                  2 if label == 'hermetic SDK fixture' else 1)
                        native = Path(str(state)+'.native')
                        native.unlink(missing_ok=True)
                        result = invoke([{**init, 'probe': False}, prepare, prompt])
                        self.assertEqual(result.returncode, 0, result.stdout+result.stderr)
                        records = [json.loads(line) for line in result.stdout.splitlines()]
                        self.assertEqual(records[-1]['type'], 'settled', records)
                        finalized = [r for r in records if r['type'] == 'usage' and r['complete']]
                        self.assertEqual(len(finalized), 2 if label == 'hermetic SDK fixture' else 1)
                        for usage in finalized:
                            self.assertEqual((usage['input'], usage['output'], usage['cache_read'], usage['cache_write']),
                                             (8+read+write, 3, read, write))
                            self.assertEqual(usage['input']+usage['output'], 11+read+write)
                        observed = [r for r in records if r['type'] == 'usage' and r['observations']]
                        self.assertTrue(all(r['cache_read'] == read and r['cache_write'] == write for r in observed))
                        if label == 'hermetic SDK fixture':
                            self.assertEqual([json.loads(line)['maxTokens'] for line in native.read_text().splitlines()],
                                             [1000, 1000-(11+read+write)])
                        self.assertNotIn('PRIVATE_SYNTHETIC_KEY', result.stdout+result.stderr)
                # Exercise failures through the same real host observer without the official SDK
                # aborting malformed synthetic wire data before AF can observe it.
                if label == 'hermetic SDK fixture':
                    invalid = []
                    for field in ['input_tokens', 'output_tokens', 'cache_read_input_tokens', 'cache_creation_input_tokens']:
                        for value in [-1, False, '3', 1.5, 9007199254740992]:
                            invalid.append((field, value, [{'input_tokens': 7, 'output_tokens': 3,
                                            'cache_read_input_tokens': 3, 'cache_creation_input_tokens': 2, field: value}]))
                    for field, value in [('input_tokens', 6), ('output_tokens', 2),
                                         ('cache_read_input_tokens', 2), ('cache_creation_input_tokens', 1),
                                         ('input_tokens', 9007199254740991)]:
                        invalid.append((field, value, [None, {field: value}]))
                    for field, value, updates in invalid:
                        with self.subTest(field=field, invalid=value):
                            baseline = {'input_tokens': 7, 'output_tokens': 3,
                                        'cache_read_input_tokens': 3, 'cache_creation_input_tokens': 2}
                            configure([{'usage': baseline if u is None else u} for u in updates], 2)
                            result = invoke([{**init, 'probe': False}, prepare, prompt])
                            records = [json.loads(line) for line in result.stdout.splitlines()]
                            self.assertEqual(records[-1]['type'], 'error', records)
                            self.assertFalse(any(r['type'] == 'settled' for r in records))
                            last = [r for r in records if r['type'] == 'usage'][-1]
                            self.assertFalse(last['complete'])
                            if updates[0] is None:
                                self.assertEqual((last['input'], last['output'], last['cache_read'], last['cache_write']),
                                                 (9007199254740991 if value == 9007199254740991 else 12, 3, 3, 2))
                            else:
                                expected = {'input_tokens': (5, 3, 3, 2),
                                            'output_tokens': (12, None, 3, 2),
                                            'cache_read_input_tokens': (9, 3, 0, 2),
                                            'cache_creation_input_tokens': (10, 3, 3, 0)}[field]
                                self.assertEqual((last['input'], last['output'], last['cache_read'], last['cache_write']),
                                                 expected)
                    for missing, explicit_null in [('input_tokens', False), ('input_tokens', True),
                                                   ('output_tokens', False), ('output_tokens', True), ('all usage', False)]:
                        with self.subTest(missing=missing, explicit_null=explicit_null):
                            usage = {'input_tokens': 7, 'output_tokens': 3}
                            if explicit_null:
                                usage[missing] = None
                            else:
                                usage.pop(missing, None)
                            configure([] if missing == 'all usage' else [{'usage': usage}])
                            result = invoke([{**init, 'probe': False}, prepare, prompt])
                            records = [json.loads(line) for line in result.stdout.splitlines()]
                            self.assertEqual(records[-1]['type'], 'error', records)
                            self.assertFalse(any(r['type'] == 'settled' for r in records))
                            last = [r for r in records if r['type'] == 'usage'][-1]
                            if missing != 'input_tokens':
                                self.assertIsNone(last['output'])
                            if missing != 'output_tokens':
                                self.assertIsNone(last['input'])
                print('Anthropic optional caches, exact totals/charge and partial deltas: '+label+'; no live requests')

    def test_explicit_profile_rechecks_keys_cloud_files_and_pinned_modules_without_candidate_discovery(self):
        with tempfile.TemporaryDirectory() as directory:
            script = Path(directory) / 'context.mjs'
            script.write_text(r'''import assert from 'node:assert/strict';
import fs from 'node:fs';import path from 'node:path';
import {createHash} from 'node:crypto';import {pathToFileURL} from 'node:url';
const host=await import(pathToFileURL(process.argv[2]).href);
const root=fs.realpathSync(process.argv[3]),auth=path.join(root,'auth'),candidate=path.join(root,'candidate');
fs.mkdirSync(auth,{mode:0o700});fs.mkdirSync(candidate);fs.mkdirSync(path.join(candidate,'.git'));
const write=(file,value)=>{fs.writeFileSync(file,JSON.stringify(value),{mode:0o600});fs.chmodSync(file,0o600);};
process.chdir(candidate);
write(path.join(candidate,'models.json'),{providers:{'team-compatible':{apiKey:'!touch ESCAPE'}}});
write(path.join(candidate,'af-context.json'),{schema:'af.pi-context/1',environment:{OPENAI_API_KEY:'UNRELATED_AMBIENT'}});
let profile=host.loadProfile(auth,'team-compatible');assert.equal(profile.config,undefined);assert.deepEqual(profile.env,{});
assert.throws(()=>host.loadProfile(candidate,'team-compatible'),/profile_authority/);
const context=path.join(auth,'af-context.json');
write(context,{schema:'af.pi-context/1',environment:{AZURE_OPENAI_DEPLOYMENT_NAME_MAP:'{"gpt-model":"deployment-one"}'}});
const deployment=host.loadProfile(auth,'azure').identity;
write(context,{schema:'af.pi-context/1',environment:{AZURE_OPENAI_DEPLOYMENT_NAME_MAP:'{"gpt-model":"deployment-two"}'}});
assert.notEqual(host.loadProfile(auth,'azure').identity,deployment);

for(const invalid of [
 {schema:'af.pi-context/0'}, {schema:'af.pi-context/1',tools:['bash']},
 {schema:'af.pi-context/1',environment:{NODE_OPTIONS:'--import=project'}},
 {schema:'af.pi-context/1',environment:{HOME:candidate}},
 {schema:'af.pi-context/1',environment:{AWS_PROFILE:'unselected-default-files'}},
]) {write(context,invalid);assert.throws(()=>host.loadProfile(auth,'team-compatible'));}
write(context,{schema:'af.pi-context/1',environment:{OPENAI_API_KEY:'EXPLICIT_PRIVATE_KEY'}});
profile=host.loadProfile(auth,'team-compatible');assert.equal(profile.env.OPENAI_API_KEY,'EXPLICIT_PRIVATE_KEY');
write(context,{schema:'af.pi-context/1',environment:{OPENAI_API_KEY:'REPLACED_PRIVATE_KEY'}});assert.throws(()=>profile.check());
const cloud=path.join(root,'selected-cloud.json');write(cloud,{private_key:'SYNTHETIC_PRIVATE_FILE'});
write(context,{schema:'af.pi-context/1',environment:{GOOGLE_APPLICATION_CREDENTIALS:cloud,GOOGLE_CLOUD_PROJECT:'synthetic-project'}});
profile=host.loadProfile(auth,'team-compatible');profile.check();write(cloud,{private_key:'REPLACED_PRIVATE_FILE'});assert.throws(()=>profile.check());
const tokenFile=path.join(root,'selected-token');fs.writeFileSync(tokenFile,'FIRST_PRIVATE_TOKEN',{mode:0o600});
write(cloud,{credential_source:{file:tokenFile}});profile=host.loadProfile(auth,'team-compatible');profile.check();
const originalContextId=profile.identity;const secondToken=path.join(root,'second-token');fs.writeFileSync(secondToken,'PRIVATE_SECOND',{mode:0o600});
write(cloud,{credential_source:{file:secondToken}});assert.notEqual(host.loadProfile(auth,'team-compatible').identity,originalContextId);
write(cloud,{credential_source:{file:tokenFile}});
fs.writeFileSync(tokenFile,'REPLACED_PRIVATE_TOKEN');assert.throws(()=>profile.check());
write(cloud,{credential_source:{file:path.join(candidate,'models.json')}});assert.throws(()=>host.loadProfile(auth,'team-compatible'));
write(cloud,{credential_source:{executable:{command:'touch ESCAPE'}}});assert.throws(()=>host.loadProfile(auth,'team-compatible'));
write(context,{schema:'af.pi-context/1',environment:{GOOGLE_APPLICATION_CREDENTIALS:path.join(candidate,'models.json')}});
assert.throws(()=>host.loadProfile(auth,'team-compatible'));
// Standard sectioned AWS INI is data, not a JSON array or executable authority.
const awsCredentials=path.join(root,'aws-credentials'),awsConfig=path.join(root,'aws-config');
const ini=(file,text)=>{fs.writeFileSync(file,text,{mode:0o600});fs.chmodSync(file,0o600);};
ini(awsCredentials,'[selected]\naws_access_key_id = SYNTHETIC_AWS_KEY\naws_secret_access_key = SYNTHETIC_AWS_SECRET\n');
ini(awsConfig,'[profile selected]\nregion = us-east-1\n');
write(context,{schema:'af.pi-context/1',environment:{AWS_PROFILE:'selected',AWS_SHARED_CREDENTIALS_FILE:awsCredentials,AWS_CONFIG_FILE:awsConfig}});
profile=host.loadProfile(auth,'amazon-bedrock');profile.check();
ini(awsCredentials,'[selected]\naws_access_key_id = REPLACED_AWS_KEY\n');assert.throws(()=>profile.check(),/configuration_changed/);
ini(awsConfig,'[profile selected]\ncredential_process = touch ESCAPE\n');
assert.throws(()=>host.loadProfile(auth,'amazon-bedrock'),/helper_authority/);
ini(awsConfig,'[profile selected]\nweb_identity_token_file = '+path.join(candidate,'models.json')+'\n');
assert.throws(()=>host.loadProfile(auth,'amazon-bedrock'),/profile_authority/);
fs.unlinkSync(context);
write(path.join(auth,'models.json'),{providers:{'team-compatible':{apiKey:'!touch ESCAPE'}}});assert.throws(()=>host.loadProfile(auth,'team-compatible'));
fs.unlinkSync(path.join(auth,'models.json'));
const module=path.join(root,'installed-provider.mjs');fs.writeFileSync(module,'export function register(){}');
const hash=file=>'sha256:'+createHash('sha256').update(fs.readFileSync(file)).digest('hex');
write(context,{schema:'af.pi-context/1',extension:{entrypoint:module,sha256:hash(module)}});
profile=host.loadProfile(auth,'team-compatible');profile.check();fs.appendFileSync(module,'// changed');assert.throws(()=>profile.check());
const projectModule=path.join(candidate,'provider.mjs');fs.writeFileSync(projectModule,'throw Error("MUST_NOT_EXECUTE")');
write(context,{schema:'af.pi-context/1',extension:{entrypoint:projectModule,sha256:hash(projectModule)}});assert.throws(()=>host.loadProfile(auth,'team-compatible'));
write(context,{schema:'af.pi-context/1',extension:{entrypoint:module,sha256:'sha256:'+'0'.repeat(64)}});assert.throws(()=>host.loadProfile(auth,'team-compatible'));
// Stored literal-key drift is checked in memory, without producing a credential fingerprint.
let current={type:'api_key',key:'PRIVATE_FIRST'};
const storage={read:async()=>{throw Error('helper-capable read forbidden');},modify:async(id,fn)=>{const next=await fn(current);if(next!==undefined)current=next;return current;}};
const guarded=host.guardedCredentials(storage,null,'arbitrary-provider',()=>{},current);
await guarded.read('arbitrary-provider');current={type:'api_key',key:'PRIVATE_SECOND'};await assert.rejects(guarded.read('arbitrary-provider'));
assert.equal(host.principal(current,'arbitrary-provider'),null);
assert.equal(host.principal({type:'oauth',access:'PRIVATE_ACCESS',refresh:'',expires:Number.MAX_SAFE_INTEGER},'openrouter'),null);
assert.throws(()=>host.principal({type:'oauth',access:'PRIVATE_ACCESS',refresh:'',expires:42},'arbitrary-provider'));
assert.throws(()=>host.transportAdapter('constructor'));
const model={provider:'arbitrary',id:'sdk-code',api:'openai-completions',baseUrl:'https://custom.invalid/v1'};
assert.match(host.modelIdentity({...model,samplingParams:{temperature:0.2,top_p:0.9}}),/^sha256:/);
for(const key of ['model','messages','tools','max_tokens','previous_response_id','session_id','stream_options']) {
 assert.throws(()=>host.modelIdentity({...model,samplingParams:{[key]:'ESCAPE'}}),/unsafe_model_configuration/);
 assert.throws(()=>host.modelIdentity({...model,samplingParamsByThinkingLevel:{high:{extra_body:{[key]:'ESCAPE'}}}}),/unsafe_model_configuration/);
}

assert(!fs.existsSync(path.join(candidate,'ESCAPE')));
''')
            result = subprocess.run([shutil.which('node'), str(script), str(HOST), directory],
                                    capture_output=True, text=True, timeout=10, check=False)
            self.assertEqual(result.returncode, 0, result.stderr)

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
            request = {'schema': 'af.pi-host/7', 'type': 'init', 'backend': 'openai-codex',
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
            prepare = {'schema': 'af.pi-host/7', 'type': 'prepare', 'remaining_tokens': 1000, 'reply_schema': None,
                       'model': selected['id'], 'effort': 'high',
                       'model_identity': selected['identity']}
            prompt = {'schema': 'af.pi-host/7', 'type': 'prompt', 'prompt_nonce': 'sha256:' + 'd'*64,
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
                self.assertEqual(refused_records[-1], {'schema': 'af.pi-host/7',
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
                self.assertEqual(records[-1], {'schema': 'af.pi-host/7', 'type': 'error',
                                               'code': 'inference_failed',
                                               'diagnostic': 'invalid_usage',
                                               'failure_kind': 'environment',
                                               'accounting_complete': False})
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
for (const bad of [{...credential,type:'invalid'}, {...credential,access:''},
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
// IDs, endpoints and API capabilities belong to the admitted SDK catalog, not a Kimi branch.
for(const accepted of [{...model,id:'k3.1-pro'}, {...model,baseUrl:'https://other.invalid'},
                  {...model,api:'anthropic-messages'}, {...model,provider:'other'}]) {
 assert.match(host.modelIdentity(accepted),/^sha256:/);
 assert.notEqual(host.modelIdentity(accepted),host.modelIdentity(model));
}
for(const bad of [{...model,type:'image'}, {...model,baseUrl:'file:///private/secret'},
 {...model,baseUrl:'https://secret:password@other.invalid'}, {...model,baseUrl:'https://other.invalid?api_key=SECRET'}]) assert.throws(()=>host.modelIdentity(bad));
const aggregate={prompt_tokens:439,completion_tokens:38,completion_tokens_details:{reasoning_tokens:24}};
assert.deepEqual(host.rawUsage('openai-completions',{usage:aggregate}),
 {input:439,output:38,cache_read:null,cache_write:null,reasoning:24});
const usage={prompt_tokens:12,completion_tokens:5,cached_tokens:3,completion_tokens_details:{reasoning_tokens:2}};
const expected={input:12,output:5,cache_read:3,cache_write:null,reasoning:2};
assert.deepEqual(host.rawUsage('openai-completions',{usage}),expected);
assert.deepEqual(host.rawUsage('openai-completions',{choices:[{usage}]}),expected);
assert.deepEqual(host.rawUsage('openai-completions',{usage,choices:[{usage}]}),expected);
for(const cache of [{prompt_tokens_details:{cached_tokens:3}},{prompt_cache_hit_tokens:3}]) {
 const u={...usage,...cache};delete u.cached_tokens;
 assert.deepEqual(host.rawUsage('openai-completions',{usage:u}),expected);
}
assert.equal(host.rawUsage('openai-completions',{usage:null,choices:[{}]}),undefined);
assert.deepEqual(host.rawUsage('openai-completions',{usage:{}}),{input:null,output:null,cache_read:null,cache_write:null,reasoning:null});
for(const value of [-1,1.2,'12',NaN,Infinity,Number.MAX_SAFE_INTEGER+1]) {
 assert.throws(()=>host.rawUsage('openai-completions',{usage:{...usage,prompt_tokens:value}}));
}
assert.throws(()=>host.rawUsage('openai-completions',{usage:{...usage,prompt_cache_hit_tokens:2}}));
assert.throws(()=>host.rawUsage('openai-completions',{usage,choices:[{usage:{...usage,completion_tokens:6}}]}));
assert.deepEqual(host.rawUsage('openai-completions',{usage:{prompt_tokens:0,completion_tokens:0,cached_tokens:0}}),
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
            init = {'schema': 'af.pi-host/7', 'type': 'init', 'sdk': sdk, 'backend': 'kimi-coding',
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
            prepare = {'schema': 'af.pi-host/7', 'type': 'prepare', 'remaining_tokens': 1000, 'reply_schema': None, 'model': 'k3',
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
 assert.equal(body.max_tokens ?? body.max_completion_tokens, calls===0 ? 1000 : 985);
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
            prompt = {'schema': 'af.pi-host/7', 'type': 'prompt', 'text': 'Use the confined write tool.',
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

    def test_raw_usage_keeps_independent_fields_when_optional_or_aggregate_counters_conflict(self):
        node = shutil.which('node')
        self.assertIsNotNone(node)
        script = r"""
import assert from 'node:assert/strict';
import {pathToFileURL} from 'node:url';
const {rawUsage} = await import(pathToFileURL(process.argv[2]).href);
const base = {prompt_tokens:12,completion_tokens:3,cached_tokens:2,
  prompt_tokens_details:{cache_write_tokens:1},completion_tokens_details:{reasoning_tokens:1}};
const expected = {input:12,output:3,cache_read:2,cache_write:1,reasoning:1};
function invalid(event, safe) {
  let refused = 0;
  assert.deepEqual(rawUsage('openai-completions',event,()=>refused++),safe);
  assert.equal(refused,1);
  assert.throws(()=>rawUsage('openai-completions',event));
}
for (const value of [-1,0.5,'12',true,9007199254740992,Infinity,NaN,{},[]]) {
  invalid({usage:{...base,cached_tokens:value}},{...expected,cache_read:null});
  invalid({usage:{...base,prompt_cache_hit_tokens:value}},expected);
  invalid({usage:{...base,prompt_tokens_details:{cache_write_tokens:value}}},
    {...expected,cache_write:null});
  invalid({usage:{...base,completion_tokens_details:{reasoning_tokens:value}}},
    {...expected,reasoning:null});
  invalid({usage:{...base,prompt_tokens:value}},{...expected,input:null});
  invalid({usage:{...base,completion_tokens:value}},{...expected,output:null});
}
for (const [field, change] of [
  ['input',{prompt_tokens:13}], ['output',{completion_tokens:4}],
  ['cache_read',{cached_tokens:3}],
  ['cache_write',{prompt_tokens_details:{cache_write_tokens:2}}],
  ['reasoning',{completion_tokens_details:{reasoning_tokens:2}}],
]) {
  const conflicting = {...base,...change};
  // Reordering or repeating sources cannot select a conflicting value.
  for (const sources of [[base,conflicting,base],[conflicting,base,conflicting]]) {
    invalid({usage:sources[0],choices:sources.slice(1).map(usage=>({usage}))},
      {...expected,[field]:null});
  }
}
// Unknown components are absence, not numeric conflicts, in either source order.
const totals = {input:100,output:10,cache_read:null,cache_write:null,reasoning:null};
const aggregate = {prompt_tokens:100,completion_tokens:10};
for (const sources of [[aggregate,{prompt_tokens_details:false}],
                       [{prompt_tokens_details:false},aggregate]]) {
  invalid({usage:sources[0],choices:[{usage:sources[1]}]},totals);
}
for (const change of [
  {prompt_tokens:false}, {completion_tokens:false}, {cached_tokens:false},
  {prompt_tokens_details:{cache_write_tokens:false}},
  {completion_tokens_details:{reasoning_tokens:false}},
]) {
  for (const sources of [[base,change],[change,base]]) {
    invalid({usage:sources[0],choices:[{usage:sources[1]}]},expected);
  }
}
for (const unknown of [undefined,null,-1,'PRIVATE_RAW_BODY']) {
  for (const sources of [[aggregate,{prompt_tokens:unknown}],
                         [{prompt_tokens:unknown},aggregate]]) {
    const event = {usage:sources[0],choices:[{usage:sources[1]}]};
    if (unknown == null) assert.deepEqual(rawUsage('openai-completions',event),totals);
    else invalid(event,totals);
  }
}
for (const sources of [[{prompt_tokens:100},{completion_tokens:10}],
                       [{completion_tokens:10},{prompt_tokens:100}]]) {
  assert.deepEqual(rawUsage('openai-completions',
    {usage:sources[0],choices:[{usage:sources[1]}]}),totals);
}
// A malformed sibling must not hide valid aliases from permanent conflict detection.
const malformedAlias = {...aggregate,cached_tokens:false,prompt_cache_hit_tokens:3};
const otherAlias = {cached_tokens:4};
invalid({usage:malformedAlias},{...totals,cache_read:3});
for (const sources of [[malformedAlias,otherAlias],[otherAlias,malformedAlias],
                       [malformedAlias,otherAlias,{cached_tokens:3}],
                       [otherAlias,malformedAlias,{cached_tokens:4}]]) {
  invalid({usage:sources[0],choices:sources.slice(1).map(usage=>({usage}))},totals);
}
for (const sources of [[{...aggregate,cached_tokens:3,prompt_cache_hit_tokens:4},
                        {cached_tokens:3}],
                       [{cached_tokens:4},
                        {...aggregate,cached_tokens:3,prompt_cache_hit_tokens:4}]]) {
  invalid({usage:sources[0],choices:sources.slice(1).map(usage=>({usage}))},totals);
}
// Contradictory aliases are actual numeric conflicts too; another source cannot restore them.
invalid({usage:{...base,prompt_cache_hit_tokens:3},choices:[{usage:base}]},
  {...expected,cache_read:null});
invalid({usage:{...base,prompt_cache_hit_tokens:3}},{...expected,cache_read:null});
invalid({usage:[],choices:[{usage:base}]},expected);
invalid({usage:[]},undefined);
// A malformed optional counter also refuses OpenAI, retaining its independent raw totals.
let refused = false;
assert.deepEqual(rawUsage('openai-codex-responses',{response:{usage:{input_tokens:12,output_tokens:3,
  input_tokens_details:{cached_tokens:2},output_tokens_details:{reasoning_tokens:-1}}}},
  ()=>{refused=true;}),{input:12,output:3,cache_read:2,cache_write:null,reasoning:null});
assert(refused);
"""
        with tempfile.TemporaryDirectory() as directory:
            contract = Path(directory) / 'usage.mjs'
            contract.write_text(script)
            result = subprocess.run([node, str(contract), str(HOST)],
                                    capture_output=True, text=True, timeout=30, check=False)
            self.assertEqual(result.returncode, 0, result.stdout + result.stderr)

    def test_raw_usage_validates_recognized_native_containers_without_losing_sibling_totals(self):
        node = shutil.which('node')
        self.assertIsNotNone(node)
        script = r"""
import assert from 'node:assert/strict';
import {pathToFileURL} from 'node:url';
const {rawUsage} = await import(pathToFileURL(process.argv[2]).href);
const kimi = {prompt_tokens:100,completion_tokens:10};
const openai = {input_tokens:100,output_tokens:10};
const totals = {input:100,output:10,cache_read:null,cache_write:null,reasoning:null};
function check(backend,event,expected,invalid) {
  let refused = 0;
  assert.deepEqual(rawUsage(backend==='kimi-coding'?'openai-completions':'openai-codex-responses',event,()=>refused++),expected);
  assert.equal(refused,Number(invalid),JSON.stringify({backend,event}));
  if (invalid) assert.throws(()=>rawUsage(backend==='kimi-coding'?'openai-completions':'openai-codex-responses',event),/invalid_usage/);
}
// Every non-null JSON shape outside objects is rejected at object containers;
// choices alone requires an array, and each entry requires a non-null object.
for (const backend of ['kimi-coding','openai-codex']) {
  const wrap = usage => backend==='kimi-coding' ? {usage} : {response:{usage}};
  const usage = backend==='kimi-coding' ? kimi : openai;
  const details = backend==='kimi-coding'
    ? ['prompt_tokens_details','completion_tokens_details']
    : ['input_tokens_details','output_tokens_details'];
  for (const bad of [false,true,0,1,'PRIVATE_RAW_BODY',[],[{}]]) {
    check(backend,bad,undefined,true);
    check(backend,wrap(bad),undefined,true);
    for (const field of details) check(backend,wrap({...usage,[field]:bad}),totals,true);
    if (backend==='openai-codex') check(backend,{response:bad},undefined,true);
    else {
      check(backend,{usage,choices:[bad]},totals,true);
      check(backend,{usage:bad,choices:[{usage}]},totals,true);
      check(backend,{usage,choices:[{usage:bad}]},totals,true);
    }
  }
  check(backend,null,undefined,true);
  check(backend,undefined,undefined,true);
  for (const optional of [undefined,null]) {
    check(backend,{},undefined,false);
    check(backend,wrap(optional),undefined,false);
    for (const field of details) check(backend,wrap({...usage,[field]:optional}),totals,false);
    if (backend==='openai-codex') check(backend,{response:optional},undefined,false);
    else {
      check(backend,{usage,choices:optional},totals,false);
      check(backend,{usage,choices:[{usage:optional}]},totals,false);
    }
  }
}
for (const choices of [{},false,0,'PRIVATE_RAW_BODY',[null]]) {
  check('kimi-coding',{usage:kimi,choices},totals,true);
}
for (const choices of [[],[{}],[{usage:kimi}],[{usage:kimi},{usage:kimi}]]) {
  check('kimi-coding',{usage:kimi,choices},totals,false);
}
check('kimi-coding',{choices:[{usage:kimi}]},totals,false);
check('kimi-coding',{usage:{...kimi,prompt_tokens_details:{cached_tokens:2,cache_write_tokens:1},
  completion_tokens_details:{reasoning_tokens:3}}},
  {...totals,cache_read:2,cache_write:1,reasoning:3},false);
check('openai-codex',{response:{usage:{...openai,input_tokens_details:{cached_tokens:2,cache_write_tokens:1},
  output_tokens_details:{reasoning_tokens:3}}}},
  {...totals,cache_read:2,cache_write:1,reasoning:3},false);
// Unknown unrelated fields remain opaque.
check('kimi-coding',{usage:{...kimi,unrelated:[]},unrelated:false},totals,false);
check('openai-codex',{response:{usage:openai,unrelated:[]},unrelated:false},totals,false);
"""
        with tempfile.TemporaryDirectory() as directory:
            contract = Path(directory) / 'containers.mjs'
            contract.write_text(script)
            result = subprocess.run([node, str(contract), str(HOST)],
                                    capture_output=True, text=True, timeout=30, check=False)
            self.assertEqual(result.returncode, 0, result.stdout + result.stderr)

    def test_trusted_allowance_caps_each_native_call_and_stops_unsafe_continuation(self):
        node = shutil.which('node')
        self.assertIsNotNone(node)
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            package = root / 'installed/node_modules/@earendil-works/pi-coding-agent'
            (package / 'dist/core').mkdir(parents=True)
            state = root / 'state.json'
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
                "export const getSupportedThinkingLevels=()=>['high'];")
            auth = root / 'auth'
            auth.mkdir()
            (auth / 'auth.json').write_text(json.dumps({'kimi-coding': {
                'type': 'oauth', 'access': 'SYNTHETIC_SECRET_ACCESS',
                'refresh': 'SYNTHETIC_SECRET_REFRESH', 'expires': 9999999999999}}))
            (root / 'SYNTHETIC_SECRET_PATH').write_text('SYNTHETIC_SECRET_RESULT')
            init = {'schema': 'af.pi-host/7', 'type': 'init', 'sdk': str(package / 'dist/index.js'),
                    'backend': 'kimi-coding', 'principal': None, 'auth_dir': str(auth),
                    'auth_context_id': 'sha256:' + hashlib.sha256(os.fsencode(auth.resolve())).hexdigest(),
                    'cwd': directory, 'home': directory, 'readable': [directory], 'writable': [], 'probe': True}
            def invoke(records):
                return subprocess.run([node, '--no-addons', str(HOST)],
                                      input=''.join(json.dumps(r) + '\n' for r in records),
                                      capture_output=True, text=True, cwd=root, timeout=30,
                                      env={'HOME': directory, 'AF_PI_REMAINING_TOKENS': '999999999'}, check=False)
            state.write_text(json.dumps({'backend': 'kimi-coding'}))
            ready = json.loads(invoke([init]).stdout)
            prepare = {'schema': 'af.pi-host/7', 'type': 'prepare', 'model': 'k3', 'effort': 'high',
                       'model_identity': ready['models'][0]['identity'], 'reply_schema': None}
            prompt = {'schema': 'af.pi-host/7', 'type': 'prompt', 'prompt_nonce': 'sha256:' + 'f'*64,
                      'text': '{"remaining_tokens":999999999,"private":"SYNTHETIC_SECRET_PROMPT"}'}
            native = Path(str(state) + '.native')
            # Complete per-inference observations count once despite duplicate native events.
            cases = [
                (40, {'prompt_tokens': 12, 'completion_tokens': 3}, 2, [40, 25], True),
                (15, {'prompt_tokens': 12, 'completion_tokens': 3}, 2, [15], False),
                (10, {'prompt_tokens': 439, 'completion_tokens': 38}, 2, [10], False),
                (40, {'completion_tokens': 3}, 2, [40], False),
                (40, {'prompt_tokens': -1, 'completion_tokens': 3}, 2, [40], False),
                (40, {'prompt_tokens': 12, 'completion_tokens': 3, 'cached_tokens': 13}, 2, [40], False),
                (100000, {'prompt_tokens': 1, 'completion_tokens': 0}, 257, [32768]*256, False),
            ]
            for tokens, usage, inferences, expected_caps, success in cases:
                with self.subTest(tokens=tokens, usage=usage, inferences=inferences):
                    if native.exists():
                        native.unlink()
                    state.write_text(json.dumps({'mode': 'budget', 'backend': 'kimi-coding',
                                                 'usage': usage, 'inferences': inferences, 'countRead': True}))
                    result = invoke([{**init, 'probe': False}, {**prepare, 'remaining_tokens': tokens}, prompt])
                    records = [json.loads(line) for line in result.stdout.splitlines()]
                    self.assertEqual(records[-1]['type'], 'settled' if success else 'error', records[-3:])
                    self.assertEqual([json.loads(line)['maxTokens'] for line in native.read_text().splitlines()], expected_caps)
                    observed = [r for r in records if r['type'] == 'usage']
                    self.assertTrue(observed)
                    self.assertLessEqual(len({r['inference'] for r in observed}), 256)
                    if tokens == 10:
                        self.assertEqual((observed[-1]['input'], observed[-1]['output']), (439, 38))
                    if success:
                        self.assertEqual([r['count'] for r in records if r['type'] == 'tool_count'], [1, 2])
                        self.assertEqual([r['observations'] for r in observed if r['complete']], [2, 2])
                    self.assertNotIn('SYNTHETIC_SECRET', result.stdout + result.stderr)
            native.unlink()
            state.write_text(json.dumps({'mode': 'budget', 'backend': 'kimi-coding',
                                         'usage': {'prompt_tokens': 12, 'completion_tokens': 3},
                                         'inferences': 2, 'usageEvents': 128}))
            result = invoke([{**init, 'probe': False}, {**prepare, 'remaining_tokens': 1000}, prompt])
            records = [json.loads(line) for line in result.stdout.splitlines()]
            self.assertEqual(records[-1]['type'], 'error')
            self.assertEqual(len(native.read_text().splitlines()), 1)
            last_usage = [r for r in records if r['type'] == 'usage'][-1]
            self.assertEqual(last_usage['observations'], 128)
            self.assertEqual((last_usage['input'], last_usage['output']), (12, 3))
            self.assertFalse(last_usage['complete'])
            for invalid in [None, 0, -1, '1000', 1.2, 9007199254740992]:
                if native.exists():
                    native.unlink()
                result = invoke([{**init, 'probe': False}, {**prepare, 'remaining_tokens': invalid}, prompt])
                self.assertEqual(result.returncode, 1)
                self.assertFalse(native.exists())
                self.assertNotIn('accepted', result.stdout)

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
assert(options.resourceLoader.getSystemPrompt().includes('"outputs":{"report":[{"summary":"..."}]}'));
assert(options.resourceLoader.getSystemPrompt().includes('no shell'));
assert(options.resourceLoader.getSystemPrompt().includes('Do not claim tests ran'));
const declared = {type:'object',properties:{outputs:{type:'object',properties:{actual:{type:'array',items:{type:'integer'}}}}}};
const resources = host.emptyResources(sdk,declared);
assert(resources.getSystemPrompt().includes(JSON.stringify(declared)));
assert(resources.getSystemPrompt().includes('actual declared ports and schemas'));
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
