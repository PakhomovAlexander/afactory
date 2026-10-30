#!/usr/bin/env python3
"""Exercise `backlog-check-triage.py` (docs/design/issue-triage.md §6.3): the `af/issue-triage@1`
schema, the hard rules, the merge rule, the six issues verified by hand on 2026-10-30, and the
staged triage packages and example under `fixtures/issue-triage/`. Reachability runs real `git`
on a throwaway repository; no network, no GitHub, no Provider."""
import contextlib
import copy
import importlib.util
import io
import json
import os
from pathlib import Path
import re
import shutil
import subprocess
import sys
import tempfile
import unittest

sys.dont_write_bytecode = True

ROOT = Path(__file__).resolve().parent.parent
STAGED = ROOT / 'fixtures' / 'issue-triage'
HAND = json.loads((STAGED / 'hand-triage-2026-10-30.json').read_text(encoding='utf-8'))


def load():
    spec = importlib.util.spec_from_file_location('backlog_check_triage', ROOT / 'scripts' / 'backlog-check-triage.py')
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


triage = load()
SCHEMA = triage.validator()
BODY = 'ab' * 32
MAIN = '26d8a6da' + '0' * 32


def doc(**fields):
    """A valid `af/issue-triage@1`; `resolution` fields merge into the open default."""
    document = {
        'schema': 'af/issue-triage@1', 'issue': 432, 'body_sha256': BODY, 'class': 'bug', 'size': 'M',
        'pipeline': 'default', 'adr': None, 'confidence': 0.86,
        'rationale': 'TaskAttentionV1::of puts every waiting Task in the approval group.',
        'resolution': {'state': 'open', 'evidence': [], 'addressed': [], 'remaining': []},
        'duplicates': [], 'needs_info': [], 'acceptance': 'A Task that needs input is listed under its own group.',
    }
    document['resolution'] = dict(document['resolution'], **fields.pop('resolution', {}))
    document.update(fields)
    return document


def evaluation(outcome='passed', block=None):
    summary = 'crates/review-core/src/task/state.rs:483 says what the report claims.'
    if block is not None:
        summary += '\n\n```json\n' + json.dumps(block, indent=2) + '\n```'
    return {'document_id': 'sha256:' + '1' * 64, 'sources_id': 'sha256:' + '2' * 64,
            'requirements_id': 'sha256:' + '3' * 64, 'check_receipt_id': 'sha256:' + '4' * 64,
            'source_snapshot_id': 'sha256:' + '5' * 64, 'outcome': outcome, 'summary': summary}


def rendered(document, fence='```'):
    """The author's report as the kernel's human renderer writes it: an authored closed fence stays
    a fenced example, its fence longer than any backtick run inside."""
    return '\n\n'.join([
        f'# Triage of issue #{document["issue"]}',
        '**Result: accepted.** The report passed its automated checks and its independent review.',
        '## Triage', f'{fence}json\n{json.dumps(document, indent=2)}\n{fence}',
        '## Rationale', 'The approval group is named for a plan to confirm; `af task plan --fit` is not one.',
        '## Findings', 'The excerpts show the group and its next command.',
        '## Recommendation', 'Take it on the default pipeline.',
        '## Repository citations', '- `crates/review-core/src/task/state.rs:483`',
        '## Review', 'The report passed its automated checks.',
    ]) + '\n'


class Reach:
    """Reachability as the captured main of the hand triage holds it (`captured_main` 26d8a6da):
    the commits and merged pull requests the owner's session verified there."""

    def __init__(self, commits=(), pulls=None):
        self.main, self.commits, self.pulls = MAIN, set(commits), pulls

    def commit(self, sha):
        return any(sha.startswith(known) or known.startswith(sha) for known in self.commits)

    def pull(self, number):
        return self.pulls is not None and number in self.pulls


def run_check(author, verifier_block=None, outcome='passed', labels=(), reach=None, **kwargs):
    return triage.check(author, evaluation(outcome, verifier_block), list(labels), reach or Reach(), **kwargs)


class Schema(unittest.TestCase):
    def errors(self, document):
        return SCHEMA.errors(document, 'urn:af:schema:issue-triage:1')

    def test_both_schemas_are_2020_12_with_their_ids(self):
        for name, urn in (('issue-triage-v1.json', 'urn:af:schema:issue-triage:1'),
                          ('issue-triage-merged-v1.json', 'urn:af:schema:issue-triage-merged:1')):
            schema = json.loads((ROOT / 'schemas' / name).read_text(encoding='utf-8'))
            self.assertEqual(schema['$schema'], 'https://json-schema.org/draft/2020-12/schema')
            self.assertEqual(schema['$id'], urn)
            self.assertIs(schema['additionalProperties'], False)

    def test_the_design_example_shape_is_accepted(self):
        self.assertEqual(self.errors(doc()), [])
        self.assertEqual(self.errors(doc(adr='amends ADR-0170', size='L', pipeline='design', confidence=1)), [])

    def test_each_violation_is_refused(self):
        cases = {
            'missing field': {k: v for k, v in doc().items() if k != 'acceptance'},
            'unknown field': doc(area='crates'),
            'schema version': doc(schema='af/issue-triage@2'),
            'issue below 1': doc(issue=0),
            'issue not an integer': doc(issue='432'),
            'body digest': doc(body_sha256='AB' * 32),
            'class': doc(**{'class': 'area'}),
            'size': doc(size='XXL'),
            'pipeline': doc(pipeline='fast'),
            'adr type': doc(adr=7),
            'confidence above 1': doc(confidence=1.5),
            'confidence below 0': doc(confidence=-0.1),
            'rationale too long': doc(rationale='x' * 2001),
            'resolution state': doc(resolution={'state': 'fixed'}),
            'resolution field': doc(resolution={'closed': True}),
            'evidence item type': doc(resolution={'evidence': [356]}),
            'duplicates repeated': doc(duplicates=[12, 12]),
            'needs_info blank': doc(needs_info=['  ']),
        }
        for name, document in cases.items():
            with self.subTest(name):
                self.assertNotEqual(self.errors(document), [])
                with self.assertRaises(triage.Refusal) as refused:
                    run_check(document)
                self.assertEqual(refused.exception.rule, 'triage-schema')

    def test_the_validator_refuses_a_keyword_it_does_not_implement(self):
        validator = triage.Validator(ROOT / 'schemas' / 'issue-triage-v1.json')
        validator.schemas['urn:test'] = {'$id': 'urn:test', 'type': 'string', 'format': 'date'}
        with self.assertRaises(ValueError):
            validator.errors('x', 'urn:test')


class Documents(unittest.TestCase):
    def test_the_triage_block_is_read_from_the_rendered_report(self):
        self.assertEqual(triage.author_document(document=rendered(doc())), doc())
        # The renderer lengthens a fence whose content holds backticks.
        tick = doc(rationale='`TaskAttentionV1::of` puts it under ```approval```.')
        self.assertEqual(triage.author_document(document=rendered(tick, fence='````')), tick)

    def test_the_triage_block_is_read_from_the_draft(self):
        draft = {'schema': 'af.document-draft/2', 'title': 'Triage of issue #432', 'citations': [],
                 'sections': [{'heading': 'Triage', 'body': '```json\n' + json.dumps(doc()) + '\n```'},
                              {'heading': 'Rationale', 'body': 'Why.'}]}
        self.assertEqual(triage.author_document(draft=draft), doc())

    def test_a_report_without_one_parsable_block_or_a_rationale_is_refused(self):
        block = '```json\n' + json.dumps(doc()) + '\n```'
        cases = {
            'canonical markdown escapes the fence': rendered(doc()).replace('```', '\\`\\`\\`'),
            'two blocks': rendered(doc()).replace('## Rationale', block + '\n\n## Rationale'),
            'no rationale': rendered(doc()).replace('## Rationale', '## Reasoning'),
            'not json': rendered(doc()).replace('"schema"', 'schema'),
            'unclosed fence': rendered(doc()).replace('}\n```', '}\n'),
        }
        for name, text in cases.items():
            with self.subTest(name), self.assertRaises(triage.Refusal) as refused:
                triage.author_document(document=text)
            self.assertEqual(refused.exception.rule, 'author-draft')

    def test_the_verifier_block_is_one_json_fence_of_the_summary(self):
        self.assertEqual(triage.verifier_document(evaluation('failed', doc(size='L')), SCHEMA),
                         ('failed', doc(size='L')))
        self.assertEqual(triage.verifier_document(evaluation('passed'), SCHEMA), ('passed', None))
        twice = evaluation('failed', doc())
        twice['summary'] += '\n\n```json\n{}\n```'
        with self.assertRaises(triage.Refusal) as refused:
            triage.verifier_document(twice, SCHEMA)
        self.assertEqual(refused.exception.rule, 'verifier-block')
        bad = evaluation('passed')
        bad['outcome'] = 'verified'
        with self.assertRaises(triage.Refusal) as refused:
            triage.verifier_document(bad, SCHEMA)
        self.assertEqual(refused.exception.rule, 'evaluation-schema')


class HardRules(unittest.TestCase):
    def refused(self, rule, author, block=None, **kwargs):
        with self.assertRaises(triage.Refusal) as refused:
            run_check(author, block, **kwargs)
        self.assertEqual(refused.exception.rule, rule, str(refused.exception))
        return str(refused.exception)

    def test_a_named_contract_schema_budget_or_sandbox_change_needs_at_least_L(self):
        for phrase in ('contract change', 'schema-version change', 'budget change', 'sandbox-boundary change'):
            with self.subTest(phrase):
                message = self.refused('size-below-L', doc(rationale=f'This needs a {phrase} in af.'))
                self.assertIn(phrase.split()[0], message)
                record, code = run_check(doc(rationale=f'This needs a {phrase}.', size='L', pipeline='design'))
                self.assertEqual((code, record['size']), (0, 'L'))
        self.refused('size-below-L', doc(), doc(rationale='The fix is a Budget change.'))
        for negated in ('no contract change', 'not a schema-version change', 'without a budget change',
                        'without sandbox-boundary change'):
            with self.subTest(negated):
                self.assertEqual(run_check(doc(rationale=f'An additive group: {negated}.'))[1], 0)

    def test_an_adr_makes_size_S_impossible(self):
        self.refused('adr-with-S', doc(adr='new ADR: attention groups', size='S', pipeline='light'))
        self.assertEqual(run_check(doc(adr='new ADR: attention groups'))[0]['adr'], 'new ADR: attention groups')

    def test_a_fixed_or_partial_claim_needs_reachable_evidence(self):
        for state in ('fixed_candidate', 'partial'):
            with self.subTest(state):
                claim = doc(resolution={'state': state, 'evidence': []})
                self.refused('resolution-evidence', claim, claim)
                prose = doc(resolution={'state': state, 'evidence': ['CHANGELOG.md says it shipped']})
                self.refused('resolution-evidence', prose, prose)
        pr = doc(resolution={'state': 'fixed_candidate', 'evidence': ['PR #356']})
        self.assertIn('no merged-pulls list', self.refused('resolution-evidence', pr, pr))
        self.refused('resolution-evidence', pr, pr, reach=Reach(pulls={355}))
        self.assertEqual(run_check(pr, pr, reach=Reach(pulls={356}))[0]['evidence_check']['pulls'], [356])
        unknown = doc(resolution={'state': 'partial', 'evidence': ['d0b49e57', 'PR #356']})
        self.assertIn('d0b49e57', self.refused('resolution-evidence', unknown, unknown, reach=Reach(pulls={356})))

    def test_a_verifier_document_is_held_to_the_same_rules(self):
        self.refused('adr-with-S', doc(), doc(adr='amends ADR-0170', size='S', pipeline='light'))
        self.refused('issue-identity', doc(), doc(issue=433))

    def test_security_from_either_triager_holds_before_any_other_rule(self):
        for author, block in ((doc(**{'class': 'security'}), doc()), (doc(), doc(**{'class': 'security'})),
                              (doc(**{'class': 'security', 'size': 'S', 'adr': 'new ADR: x'}), None)):
            record, code = run_check(author, block, outcome='failed' if block else 'passed')
            self.assertEqual((code, record['verdict'], record['labels']), (triage.EXIT_HELD, 'held', []))
            self.assertIsNone(record['class'])


class Merge(unittest.TestCase):
    def merged(self, author, verifier, labels=(), **kwargs):
        return triage.merge(author, verifier, list(labels), **kwargs)

    def test_agreement_sets_class_size_and_the_pipeline_the_size_names(self):
        for size, pipeline in (('S', 'light'), ('M', 'default'), ('L', 'design'), ('XL', 'design')):
            record = self.merged(doc(size=size, pipeline='hand'), doc(size=size))
            self.assertEqual((record['verdict'], record['pipeline']), ('agreed', pipeline))
            self.assertEqual(record['labels'], ['class/bug', f'size/{size}', f'pipeline/{pipeline}'])
        # A passed evaluation without a block stands for the author's document.
        self.assertEqual(self.merged(doc(), None)['verdict'], 'agreed')

    def test_a_difference_in_class_size_resolution_or_duplicates_is_a_dispute(self):
        cases = {
            'class': doc(**{'class': 'aix'}),
            'size': doc(size='L', pipeline='design'),
            'resolution': doc(resolution={'state': 'partial', 'evidence': ['PR #429']}),
            'duplicates': doc(duplicates=[376]),
        }
        for field, verifier in cases.items():
            with self.subTest(field):
                record = self.merged(doc(), verifier, outcome='failed')
                self.assertEqual((record['verdict'], record['labels']), ('disputed', ['triage/disputed']))
                self.assertEqual((record['class'], record['size'], record['pipeline']), (None, None, None))
                self.assertTrue(record['reasons'][0].startswith(field), record['reasons'])
        failed = self.merged(doc(), doc(), outcome='failed')
        self.assertEqual(failed['verdict'], 'disputed')
        with self.assertRaises(triage.Refusal) as refused:
            self.merged(doc(), None, outcome='failed')
        self.assertEqual(refused.exception.rule, 'verifier-block')

    def test_questions_low_confidence_or_an_inconclusive_evaluation_ask_for_information(self):
        cases = [(doc(needs_info=['Which group?']), doc(), 'passed'),
                 (doc(), doc(needs_info=['Is the group additive?']), 'failed'),
                 (doc(confidence=0.59), doc(), 'passed'),
                 (doc(), None, 'inconclusive')]
        for author, verifier, outcome in cases:
            with self.subTest(outcome=outcome, author=author['needs_info'], confidence=author['confidence']):
                record = self.merged(author, verifier, outcome=outcome)
                self.assertEqual((record['verdict'], record['labels']), ('needs-info', ['triage/needs-info']))
                self.assertIsNone(record['class'])
        self.assertEqual(self.merged(doc(confidence=0.6), doc())['verdict'], 'agreed')
        # Needs-info comes before a dispute.
        self.assertEqual(self.merged(doc(needs_info=['?']), doc(size='XL'), outcome='failed')['verdict'],
                         'needs-info')

    def test_a_hold_label_or_security_holds_before_anything_else(self):
        for labels, author, verifier in (
                (['class/security'], doc(), doc()), (['triage/held', 'bug'], doc(needs_info=['?']), doc()),
                ([], doc(**{'class': 'security'}), doc()), ([], doc(), doc(**{'class': 'security'}))):
            with self.subTest(labels=labels):
                record = self.merged(author, verifier, labels, outcome='failed')
                self.assertEqual((record['verdict'], record['labels']), ('held', []))
        record, code = run_check(doc(), doc(), labels=['triage/held'])
        self.assertEqual((code, record['verdict']), (triage.EXIT_HELD, 'held'))

    def test_an_agreed_duplicate_sets_triage_duplicate_instead_of_class_labels(self):
        record = self.merged(doc(duplicates=[376]), doc(duplicates=[376]))
        self.assertEqual((record['verdict'], record['labels'], record['duplicates']),
                         ('duplicate', ['triage/duplicate'], [376]))

    def test_an_owner_override_is_kept_in_the_labels_and_the_record(self):
        override = triage.owner_overrides(['pipeline/hand'])
        record, code = run_check(doc(), doc(), owner_labels=['pipeline/hand'])
        self.assertEqual((code, record['pipeline'], record['owner_override']), (0, 'hand', override))
        self.assertEqual(record['labels'], ['class/bug', 'size/M', 'pipeline/hand'])
        # An owner label on the disputed namespace resolves the dispute.
        record = self.merged(doc(), doc(**{'class': 'aix'}), outcome='failed',
                             owner_override=triage.owner_overrides(['class/uix']))
        self.assertEqual((record['verdict'], record['class'], record['labels'][0]), ('agreed', 'uix', 'class/uix'))
        # The override stays the effective value when no class is agreed.
        record = self.merged(doc(needs_info=['?']), doc(), owner_override=triage.owner_overrides(['size/S']))
        self.assertEqual((record['verdict'], record['size'], record['labels']),
                         ('needs-info', 'S', ['size/S', 'triage/needs-info']))
        for labels in (['size/S', 'size/M'], ['area/tui']):
            with self.subTest(labels), self.assertRaises(triage.Refusal) as refused:
                triage.owner_overrides(labels)
            self.assertEqual(refused.exception.rule, 'owner-override')


class HandTriage(unittest.TestCase):
    """The six issues of `hand-triage-2026-10-30.json` reach their recorded state with the recorded
    evidence, as the captured main of that triage holds it."""

    def reach(self, issue):
        commits, pulls = triage.references(issue['evidence'])
        merged = {int(n) for entry in issue['evidence'] for n in re.findall(r'\bPR #([0-9]+)', entry)}
        self.assertTrue(commits or merged, issue['title'])
        return Reach(commits=commits, pulls=merged)

    def claim(self, number, **resolution):
        issue = HAND['issues'][number]
        fields = {'state': issue['state'], 'evidence': issue['evidence'], 'addressed': issue['addressed'],
                  'remaining': issue['remaining']}
        fields.update(resolution)
        return issue, doc(issue=int(number), resolution=fields)

    def test_the_three_fixed_issues_merge_to_maybe_fixed_and_without_evidence_are_refused(self):
        for number in ('196', '357', '436'):
            with self.subTest(number):
                issue, claim = self.claim(number)
                self.assertEqual(issue['state'], 'fixed_candidate')
                record, code = run_check(claim, claim, reach=self.reach(issue), hand_triage=HAND)
                self.assertEqual((code, record['verdict']), (0, 'agreed'))
                self.assertEqual(record['resolution']['state'], 'fixed_candidate')
                self.assertEqual(record['labels'], ['class/bug', 'size/M', 'pipeline/default', 'triage/maybe-fixed'])
                _, empty = self.claim(number, evidence=[])
                with self.assertRaises(triage.Refusal) as refused:
                    run_check(empty, empty, reach=self.reach(issue))
                self.assertEqual(refused.exception.rule, 'resolution-evidence')

    def test_the_three_partly_fixed_issues_merge_to_partial_and_keep_what_remains(self):
        for number in ('425', '393', '284'):
            with self.subTest(number):
                issue, claim = self.claim(number)
                self.assertEqual(issue['state'], 'partial')
                record, code = run_check(claim, claim, reach=self.reach(issue), hand_triage=HAND)
                self.assertEqual((code, record['verdict']), (0, 'agreed'))
                self.assertEqual(record['resolution']['state'], 'partial')
                self.assertEqual(record['resolution']['remaining'], issue['remaining'])
                self.assertNotIn('triage/maybe-fixed', record['labels'])

    def test_a_state_the_hand_triage_contradicts_is_refused(self):
        issue, claim = self.claim('425', state='fixed_candidate', remaining=[])
        with self.assertRaises(triage.Refusal) as refused:
            run_check(claim, claim, reach=self.reach(issue), hand_triage=HAND)
        self.assertEqual(refused.exception.rule, 'hand-triage')


def git(repo, *args):
    env = dict(os.environ, GIT_CONFIG_GLOBAL=os.devnull, GIT_CONFIG_NOSYSTEM='1')
    return subprocess.run(['git', '-C', str(repo), '-c', 'user.name=Fixture', '-c', 'user.email=fixture@example.invalid',
                           *args], capture_output=True, text=True, check=True, env=env).stdout.strip()


class Cli(unittest.TestCase):
    """The script as the runner calls it, against a real Git repository."""

    @classmethod
    def setUpClass(cls):
        cls.tmp = tempfile.TemporaryDirectory()
        cls.dir = Path(cls.tmp.name)
        cls.repo = cls.dir / 'repo'
        cls.repo.mkdir()
        git(cls.repo, 'init', '-q', '-b', 'main')
        git(cls.repo, 'commit', '-q', '--allow-empty', '-m', 'fix')
        cls.fix = git(cls.repo, 'rev-parse', 'HEAD')
        git(cls.repo, 'checkout', '-q', '-b', 'side')
        git(cls.repo, 'commit', '-q', '--allow-empty', '-m', 'unmerged')
        cls.side = git(cls.repo, 'rev-parse', 'HEAD')
        git(cls.repo, 'checkout', '-q', 'main')
        git(cls.repo, 'commit', '-q', '--allow-empty', '-m', 'main')
        cls.main = git(cls.repo, 'rev-parse', 'HEAD')
        (cls.dir / 'pulls.json').write_text('[356, 438]')

    @classmethod
    def tearDownClass(cls):
        cls.tmp.cleanup()

    def run_cli(self, author, verifier_block=None, outcome='passed', *extra, draft=False):
        path = self.dir / ('author.json' if draft else 'report.md')
        if draft:
            path.write_text(json.dumps({'schema': 'af.document-draft/2', 'title': 't', 'citations': [], 'sections': [
                {'heading': 'Triage', 'body': '```json\n' + json.dumps(author) + '\n```'},
                {'heading': 'Rationale', 'body': 'Why.'}]}))
        else:
            path.write_text(rendered(author))
        (self.dir / 'evaluation.json').write_text(json.dumps(evaluation(outcome, verifier_block)))
        out, err = io.StringIO(), io.StringIO()
        with contextlib.redirect_stdout(out), contextlib.redirect_stderr(err):
            code = triage.main(['--draft' if draft else '--document', str(path), '--evaluation',
                                str(self.dir / 'evaluation.json'), '--repo', str(self.repo), '--main', 'main', *extra])
        return code, out.getvalue(), err.getvalue()

    def test_a_reachable_commit_merges_and_the_record_validates(self):
        claim = doc(resolution={'state': 'fixed_candidate', 'evidence': [f'commit {self.fix[:10]}']})
        code, out, _ = self.run_cli(claim, claim, 'passed', '--task-id', 'triage-432', '--event-id', 'opened-1')
        self.assertEqual(code, 0)
        record = json.loads(out)
        self.assertEqual(SCHEMA.errors(record, 'urn:af:schema:issue-triage-merged:1'), [])
        self.assertEqual((record['verdict'], record['labels'][-1]), ('agreed', 'triage/maybe-fixed'))
        self.assertEqual(record['evidence_check']['commits'], [self.fix[:10]])
        self.assertEqual(record['generation']['main_commit'], self.main)
        self.assertEqual(record['sources']['task_id'], 'triage-432')
        self.assertEqual(record['sources']['document_id'], 'sha256:' + '1' * 64)

    def test_an_unreachable_or_unknown_commit_is_refused_with_exit_1(self):
        for sha in (self.side[:12], 'deadbeef1'):
            with self.subTest(sha):
                claim = doc(resolution={'state': 'partial', 'evidence': [sha], 'remaining': ['the rest']})
                code, out, err = self.run_cli(claim, claim, 'passed', '--pulls', str(self.dir / 'pulls.json'))
                self.assertEqual((code, out), (1, ''))
                self.assertIn('refused: resolution-evidence:', err)
                self.assertIn(sha, err)

    def test_pull_request_evidence_needs_the_runner_list(self):
        issue = HAND['issues']['357']
        claim = doc(issue=357, resolution={'state': 'fixed_candidate', 'evidence': issue['evidence'],
                                           'addressed': issue['addressed']})
        code, _, err = self.run_cli(claim, claim)
        self.assertEqual(code, 1)
        self.assertIn('no merged-pulls list was given', err)
        code, out, _ = self.run_cli(claim, claim, 'passed', '--pulls', str(self.dir / 'pulls.json'),
                                    '--hand-triage', str(STAGED / 'hand-triage-2026-10-30.json'), draft=True)
        self.assertEqual(code, 0)
        self.assertEqual(json.loads(out)['evidence_check'], {'commits': [], 'pulls': [356], 'unverified': []})

    def test_a_dispute_prints_both_positions_and_exits_0(self):
        code, out, _ = self.run_cli(doc(), doc(**{'class': 'aix'}), 'failed')
        record = json.loads(out)
        self.assertEqual((code, record['verdict'], record['labels']), (0, 'disputed', ['triage/disputed']))
        self.assertEqual((record['author']['class'], record['verifier']['class']), ('bug', 'aix'))

    def test_a_security_hold_exits_3_and_sets_no_label(self):
        code, out, err = self.run_cli(doc(**{'class': 'security'}), None, 'passed', '--owner-label', 'size/S')
        record = json.loads(out)
        self.assertEqual((code, record['verdict'], record['labels']), (3, 'held', []))
        self.assertIn('held:', err)
        code, out, _ = self.run_cli(doc(), doc(), 'passed', '--label', 'class/security')
        self.assertEqual((code, json.loads(out)['verdict']), (3, 'held'))
        # An issue already held stays held even when its documents cannot be read.
        code, out, _ = self.run_cli(doc(), doc(), 'passed', '--label', 'triage/held',
                                    '--evaluation', str(self.dir / 'absent.json'))
        self.assertEqual((code, json.loads(out)['labels']), (3, []))

    def test_an_unknown_main_is_refused(self):
        _, _, err = self.run_cli(doc(), doc(), 'passed', '--main', 'nonexistent')
        self.assertIn('refused: main:', err)


class Staged(unittest.TestCase):
    """The packages and the example the owner installs (fixtures/issue-triage/README.md)."""

    def test_the_example_sources_fit_the_document_sources_shape_and_the_report_bounds(self):
        path = STAGED / 'example' / 'sources.json'
        raw = path.read_bytes()
        sources = json.loads(raw)
        validator = triage.Validator(ROOT / 'schemas' / 'document-sources-v1.json')
        self.assertEqual(validator.errors(sources, 'urn:af:schema:document-sources:1'), [])
        entries = sources['sources']
        self.assertEqual(sorted(entries), ['citations', 'excerpts', 'issue', 'neighbours', 'pulls', 'rubric'])
        texts = [entry['text'].encode('utf-8') for entry in entries.values()]
        self.assertLessEqual(max(map(len, texts)), 256 * 1024)
        self.assertLessEqual(sum(map(len, texts)), 512 * 1024)
        self.assertLess(len(raw), 64 * 1024)
        issue = entries['issue']['text']
        digest = re.search(r'^body_sha256: ([0-9a-f]{64})$', issue, re.M).group(1)
        self.assertEqual(entries['issue']['revision'], f'body_sha256 {digest}')

    def test_the_example_task_is_a_triage_task_reading_those_sources(self):
        task = json.loads((STAGED / 'example' / 'task.json').read_text(encoding='utf-8'))
        self.assertEqual((task['schema'], task['kind'], task['pipeline']),
                         ('af.task-file/1', 'triage', {'name': 'backlog/triage', 'fallback': 'refuse'}))
        self.assertEqual(task['report_sources'], 'fixtures/issue-triage/example/sources.json')
        self.assertEqual(task['limits']['max_attempts'], 8)

    def test_both_triagers_quote_the_rubric_and_the_precedence_verbatim(self):
        design = (ROOT / 'docs' / 'design' / 'issue-triage.md').read_text(encoding='utf-8')
        precedence = design[design.index('Class definitions:'):design.index('`triage/` states:')].strip()
        rubric = design[design.index('The rubric is what makes'):design.index('## 6. The triage pipeline')].strip()
        for worker in ('triage-author', 'triage-verifier'):
            text = (STAGED / 'task-packages' / 'backlog' / worker / 'instructions.md').read_text(encoding='utf-8')
            with self.subTest(worker):
                self.assertIn(precedence, text)
                self.assertIn(rubric, text)
                self.assertIn('untrusted data', text)

    def test_neither_triager_writes_source_or_runs_checks(self):
        for worker in ('triage-author', 'triage-verifier'):
            manifest = (STAGED / 'task-packages' / 'backlog' / worker / 'worker.toml').read_text(encoding='utf-8')
            self.assertRegex(manifest, r'(?m)^effects = \["read-source"\]$')

    def test_the_staged_pins_are_the_digests_of_the_staged_packages(self):
        af = shutil.which('af')
        if af is None:
            self.skipTest('af is not on PATH; the Rust test task_triage checks these digests')
        catalog = (STAGED / 'catalog.toml').read_text(encoding='utf-8')
        pins = re.findall(r'\[packages\."([^"]+)"\]\ndigest = "([^"]+)"\npath = "([^"]+)"', catalog)
        self.assertEqual([name for name, _, _ in pins],
                         ['backlog/triage', 'backlog/triage-author', 'backlog/triage-kind', 'backlog/triage-verifier'])
        with tempfile.TemporaryDirectory() as home:
            # A private home, and no release source: `af` must not reach the developer's real state.
            env = dict(os.environ, HOME=home, XDG_CONFIG_HOME=f'{home}/config', XDG_CACHE_HOME=f'{home}/cache',
                       XDG_DATA_HOME=f'{home}/data', XDG_STATE_HOME=f'{home}/state', AF_SELF_OFFLINE='1')
            for name, digest, path in pins:
                with self.subTest(name):
                    printed = subprocess.run([af, 'catalog', 'digest', str(ROOT / path)], capture_output=True,
                                             text=True, cwd=home, env=env)
                    self.assertEqual(printed.returncode, 0, printed.stderr)
                    self.assertEqual(printed.stdout.strip(), digest)


if __name__ == '__main__':
    unittest.main()
