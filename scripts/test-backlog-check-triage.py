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
from unittest import mock

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
DOCUMENT_ID = 'sha256:' + '1' * 64
# Every identity a publishable record needs, by key (`triage.IDENTITIES`).
IDENTITY = {'task_id': 'triage-432', 'document_id': DOCUMENT_ID, 'evaluation_id': 'sha256:' + '6' * 64,
            'bundle_sha256': '7' * 64, 'main': MAIN, 'policy_version': 'triage-runner/1',
            'event_id': 'issues-opened-1'}


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
    return {'document_id': DOCUMENT_ID, 'sources_id': 'sha256:' + '2' * 64,
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


AS_AUTHOR = object()


def run_check(author, verifier_block=AS_AUTHOR, outcome='passed', labels=(), reach=None, **kwargs):
    """`check` on an evaluation whose summary ends with `verifier_block` (by default the verifier's
    own copy of the author's document; None for none), with every identity unless overridden."""
    if verifier_block is AS_AUTHOR:
        verifier_block = copy.deepcopy(author)
    kwargs.setdefault('identity', IDENTITY)
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

    def test_the_canonical_escapes_are_read_back(self):
        # The escapes of `markdown_text` (crates/review-pipeline/src/task/document.rs); the Rust
        # test task_triage feeds the real renderer's bytes through the script.
        for written, authored in (('\\`\\`\\`json', '```json'), ('\\~\\~\\~', '~~~'),
                                  ('"schema": `"af/issue-triage@1",`', '"schema": "af/issue-triage@1",'),
                                  ('\\`\\``a@b`\\`', '``a@b`'), ('`a\\`\\``@`', 'a\\`@'),
                                  ('"x\\\\"y"', '"x\\"y"'), ('\\*x\\_ \\&amp; 1\\. \\#', '*x_ &amp; 1. #'),
                                  ('a\\b', 'a\\b'), ('` unclosed', '` unclosed')):
            with self.subTest(written):
                self.assertEqual(triage.canonical_text(written), authored)
        # A Triage section without a fence is the canonical bytes: its escaped fence is read back.
        escaped = rendered(doc()).replace('```', '\\`\\`\\`')
        self.assertEqual(triage.author_document(document=escaped), doc())

    def test_a_report_without_one_parsable_block_or_a_rationale_is_refused(self):
        block = '```json\n' + json.dumps(doc()) + '\n```'
        cases = {
            'canonical bytes with two blocks': rendered(doc()).replace('```', '\\`\\`\\`').replace(
                '## Rationale', block.replace('```', '\\`\\`\\`') + '\n\n## Rationale'),
            'canonical bytes not json': rendered(doc()).replace('```', '\\`\\`\\`').replace('"schema"', 'schema'),
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
        # The summary of any outcome may lack the block here; `check` refuses it (Identities).
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
    def refused(self, rule, author, block=AS_AUTHOR, **kwargs):
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
        for author, block, who in ((doc(**{'class': 'security'}), doc(), 'author'),
                                   (doc(), doc(**{'class': 'security'}), 'verifier'),
                                   (doc(**{'class': 'security', 'size': 'S', 'adr': 'new ADR: x'}), None, 'author')):
            record, code = run_check(author, block, outcome='failed' if block else 'passed', identity={})
            self.assertEqual((code, record['schema'], record['security_triagers']), (triage.EXIT_HELD, triage.HOLD, [who]))
            self.assertEqual(SCHEMA.errors(record, 'urn:af:schema:issue-triage-hold:1'), [])

    def test_a_marked_commit_reference_counts_whatever_its_digit_and_letter_mix(self):
        cases = {
            'commit 1234567': ['1234567'], 'see Commit abcdefa on main': ['abcdefa'], 'fixed by `7654321`': ['7654321'],
            '`deadbee`': ['deadbee'], ' 1234567 ': ['1234567'], 'facade0': ['facade0'], 'abcdefab': ['abcdefab'],
            'fixed in d0b49e57 on main': ['d0b49e57'],
            # Unmarked in prose, an all-digit run is a number and an all-letter one a word.
            'the gate took 1200000 ms': [], 'a defaced page': [], 'PR #356': [],
        }
        for entry, commits in cases.items():
            with self.subTest(entry):
                self.assertEqual(triage.references([entry])[0], commits)
        for sha in ('1234567', 'abcdefa'):
            with self.subTest(sha):
                claim = doc(resolution={'state': 'fixed_candidate', 'evidence': [f'commit {sha}']})
                record, code = run_check(claim, reach=Reach(commits={sha}))
                self.assertEqual((code, record['evidence_check']['commits']), (0, [sha]))
                self.assertIn(sha, self.refused('resolution-evidence', claim))


class Merge(unittest.TestCase):
    def merged(self, author, verifier, **kwargs):
        return triage.merge(author, verifier, **kwargs)

    def test_agreement_sets_class_size_and_the_pipeline_the_size_names(self):
        for size, pipeline in (('S', 'light'), ('M', 'default'), ('L', 'design'), ('XL', 'design')):
            record = self.merged(doc(size=size, pipeline='hand'), doc(size=size))
            self.assertEqual((record['verdict'], record['pipeline']), ('agreed', pipeline))
            self.assertEqual(record['labels'], ['class/bug', f'size/{size}', f'pipeline/{pipeline}'])
        # No outcome stands for the verifier's document: the author's is never substituted.
        for outcome in ('passed', 'failed', 'inconclusive'):
            with self.subTest(outcome), self.assertRaises(triage.Refusal) as refused:
                self.merged(doc(), None, outcome=outcome)
            self.assertEqual(refused.exception.rule, 'verifier-block')

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
                 (doc(), doc(), 'inconclusive')]
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
        class NoGit:
            """Reachability that fails the test when the check consults it."""

            @property
            def main(self):
                raise AssertionError('a hold resolved the captured main')

            def commit(self, sha):
                raise AssertionError('a hold checked a commit')

        invalid = doc(size='S', adr='new ADR: x', resolution={'state': 'fixed_candidate'})
        cases = [
            (['class/security'], (), doc(), ['class/security'], []),
            (['triage/held', 'bug'], (), doc(needs_info=['?']), ['triage/held'], []),
            # An owner's hold label holds without a duplicate --label.
            ([], ['class/security'], invalid, ['class/security'], []),
            ([], ['triage/held'], invalid, ['triage/held'], []),
            (['triage/held'], ['class/security', 'size/S'], invalid, ['triage/held', 'class/security'], []),
            ([], (), doc(**{'class': 'security'}), [], ['author']),
        ]
        for labels, owner_labels, author, held, triagers in cases:
            with self.subTest(labels=labels, owner_labels=owner_labels):
                record, code = triage.check(author, evaluation('failed', None), labels, NoGit(),
                                            owner_labels=owner_labels)
                self.assertEqual(code, triage.EXIT_HELD)
                self.assertEqual(record, {'schema': 'af/issue-triage-hold@1', 'issue': 432,
                                          'reasons': record['reasons'], 'held_labels': held,
                                          'security_triagers': triagers, 'generation': None})
                self.assertEqual(SCHEMA.errors(record, 'urn:af:schema:issue-triage-hold:1'), [])
        # Unreadable documents are held by a label alone; the generation is kept as given.
        record, code = triage.check(None, None, [], NoGit(), owner_labels=['triage/held'],
                                    identity={'main': 'main', 'event_id': 'opened-1'})
        self.assertEqual((code, record['issue'], record['generation']),
                         (triage.EXIT_HELD, None, {'bundle_sha256': None, 'main': 'main', 'policy_version': None,
                                                   'event_id': 'opened-1'}))
        self.assertEqual(SCHEMA.errors(record, 'urn:af:schema:issue-triage-hold:1'), [])
        # A held merged record is no longer a shape: the merged schema refuses the verdict.
        merged = run_check(doc(), doc())[0]
        self.assertNotEqual(SCHEMA.errors(dict(merged, verdict='held'), 'urn:af:schema:issue-triage-merged:1'), [])

    def test_an_agreed_duplicate_sets_triage_duplicate_instead_of_class_labels(self):
        record = self.merged(doc(duplicates=[376]), doc(duplicates=[376]))
        self.assertEqual((record['verdict'], record['labels'], record['duplicates']),
                         ('duplicate', ['triage/duplicate'], [376]))

    def test_an_owner_override_is_kept_in_the_labels_and_the_record(self):
        override = triage.owner_overrides(['pipeline/hand'])
        record, code = run_check(doc(), doc(), owner_labels=['pipeline/hand'])
        self.assertEqual((code, record['pipeline'], record['owner_override']), (0, 'hand', override))
        self.assertEqual(record['labels'], ['class/bug', 'size/M', 'pipeline/hand'])
        # Owner values remain effective without establishing agreement between the documents.
        for fields, labels in (({'class': 'aix'}, ['class/uix']),
                               ({'size': 'L', 'pipeline': 'design'}, ['size/S']),
                               ({'class': 'aix', 'size': 'L', 'pipeline': 'design'},
                                ['class/uix', 'size/S', 'pipeline/hand'])):
            for outcome in ('passed', 'failed'):
                with self.subTest(fields=fields, outcome=outcome):
                    record, code = run_check(doc(), doc(**fields), outcome, owner_labels=labels)
                    self.assertEqual((code, record['verdict']), (0, 'disputed'))
                    self.assertEqual(record['labels'], labels + ['triage/disputed'])
                    self.assertEqual(record['owner_override'], triage.owner_overrides(labels))
                    for label in labels:
                        namespace, value = label.split('/')
                        self.assertEqual(record[namespace], value)
                    for field in ('class', 'size'):
                        if field in fields:
                            self.assertNotEqual(record['author'][field], record['verifier'][field])
                            self.assertTrue(any(reason.startswith(field + ':') for reason in record['reasons']))
        # The override stays the effective value when no class is agreed.
        record = self.merged(doc(needs_info=['?']), doc(), owner_override=triage.owner_overrides(['size/S']))
        self.assertEqual((record['verdict'], record['size'], record['labels']),
                         ('needs-info', 'S', ['size/S', 'triage/needs-info']))
        for labels in (['size/S', 'size/M'], ['area/tui']):
            with self.subTest(labels), self.assertRaises(triage.Refusal) as refused:
                triage.owner_overrides(labels)
            self.assertEqual(refused.exception.rule, 'owner-override')


class Identities(unittest.TestCase):
    """What a publishable record must carry: the verifier's own document and every identity."""

    def test_every_outcome_needs_the_verifiers_own_block(self):
        for outcome in ('passed', 'failed', 'inconclusive'):
            with self.subTest(outcome), self.assertRaises(triage.Refusal) as refused:
                run_check(doc(), None, outcome)
            self.assertEqual(refused.exception.rule, 'verifier-block')

    def test_agreement_compares_two_independently_supplied_documents(self):
        # A passed evaluation whose own document sizes the issue differently is no agreement.
        record, code = run_check(doc(), doc(size='L', pipeline='design'), 'passed')
        self.assertEqual((code, record['verdict'], record['labels']), (0, 'disputed', ['triage/disputed']))
        self.assertEqual((record['author']['size'], record['verifier']['size']), ('M', 'L'))
        record, _ = run_check(doc(), doc(rationale='The verifier read state.rs:483 itself.'), 'passed')
        self.assertEqual((record['verdict'], record['verifier']['rationale']),
                         ('agreed', 'The verifier read state.rs:483 itself.'))

    def test_each_missing_identity_is_a_usage_error(self):
        for flag, key in triage.IDENTITIES:
            for value in (None, ''):
                with self.subTest(flag, value=value), self.assertRaises(triage.Usage) as usage:
                    run_check(doc(), identity=dict(IDENTITY, **{key: value}))
                self.assertIn(flag, str(usage.exception))

    def test_the_merged_schema_refuses_a_null_identity_or_verifier(self):
        record, code = run_check(doc())
        self.assertEqual((code, SCHEMA.errors(record, 'urn:af:schema:issue-triage-merged:1')), (0, []))
        for section, key in (('generation', 'bundle_sha256'), ('generation', 'policy_version'),
                             ('generation', 'event_id'), ('sources', 'task_id'), ('sources', 'document_id'),
                             ('sources', 'evaluation_id')):
            with self.subTest(key):
                broken = copy.deepcopy(record)
                broken[section][key] = None
                self.assertNotEqual(SCHEMA.errors(broken, 'urn:af:schema:issue-triage-merged:1'), [])
        self.assertNotEqual(SCHEMA.errors(dict(record, verifier=None), 'urn:af:schema:issue-triage-merged:1'), [])

    def test_the_document_id_is_the_one_the_evaluation_judged(self):
        with self.assertRaises(triage.Refusal) as refused:
            run_check(doc(), identity=dict(IDENTITY, document_id='sha256:' + '9' * 64))
        self.assertEqual(refused.exception.rule, 'document-identity')


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
    # Fixed identity and dates, so each commit has the id the tests name.
    env = dict(os.environ, GIT_CONFIG_GLOBAL=os.devnull, GIT_CONFIG_NOSYSTEM='1',
               GIT_AUTHOR_DATE='2026-10-30T12:00:00+00:00', GIT_COMMITTER_DATE='2026-10-30T12:00:00+00:00')
    return subprocess.run(['git', '-C', str(repo), '-c', 'user.name=Fixture', '-c', 'user.email=fixture@example.invalid',
                           *args], capture_output=True, text=True, check=True, env=env).stdout.strip()


# The commits of the fixture repository: `fix` and `main` on main, `unmerged` on a side branch.
FIX = '374f34b5b3e3591cea278166e9c9f741f7681f68'
SIDE = '8edf07b76ad7df4ae9255a71e07c617bee3889d0'
FIXTURE_MAIN = '3290981fad7181523316de8de191c5686a670ceb'


class Cli(unittest.TestCase):
    """The script as the runner calls it, against a real Git repository."""

    @classmethod
    def setUpClass(cls):
        cls.tmp = tempfile.TemporaryDirectory()
        cls.dir = Path(cls.tmp.name)
        cls.repo = cls.dir / 'repo'
        cls.repo.mkdir()
        git(cls.repo, 'init', '-q', '-b', 'main', '--object-format=sha1')
        git(cls.repo, 'commit', '-q', '--allow-empty', '-m', 'fix')
        git(cls.repo, 'checkout', '-q', '-b', 'side')
        git(cls.repo, 'commit', '-q', '--allow-empty', '-m', 'unmerged')
        git(cls.repo, 'checkout', '-q', 'main')
        git(cls.repo, 'commit', '-q', '--allow-empty', '-m', 'main')
        (cls.dir / 'pulls.json').write_text('[356, 438]')

    @classmethod
    def tearDownClass(cls):
        cls.tmp.cleanup()

    def test_the_fixture_commits_have_the_ids_the_tests_name(self):
        self.assertEqual([git(self.repo, 'rev-parse', ref) for ref in ('main~1', 'side', 'main')],
                         [FIX, SIDE, FIXTURE_MAIN])

    def run_cli(self, author, verifier_block=AS_AUTHOR, outcome='passed', *extra, draft=False, omit=(),
                sections=None):
        """The script's exit code, stdout and stderr, with every identity flag but those in `omit`."""
        if verifier_block is AS_AUTHOR:
            verifier_block = copy.deepcopy(author)
        path = self.dir / ('author.json' if draft else 'report.md')
        if draft:
            path.write_text(json.dumps({'schema': 'af.document-draft/2', 'title': 't', 'citations': [],
                                       'sections': sections if sections is not None else [
                {'heading': 'Triage', 'body': '```json\n' + json.dumps(author) + '\n```'},
                {'heading': 'Rationale', 'body': 'Why.'}]}))
        else:
            path.write_text(rendered(author))
        (self.dir / 'evaluation.json').write_text(json.dumps(evaluation(outcome, verifier_block)))
        identities = [arg for flag, key in triage.IDENTITIES if flag not in omit
                      for arg in (flag, 'main' if key == 'main' else IDENTITY[key])]
        out, err = io.StringIO(), io.StringIO()
        with contextlib.redirect_stdout(out), contextlib.redirect_stderr(err):
            code = triage.main(['--draft' if draft else '--document', str(path), '--evaluation',
                                str(self.dir / 'evaluation.json'), '--repo', str(self.repo), *identities, *extra])
        return code, out.getvalue(), err.getvalue()

    def test_a_reachable_commit_merges_and_the_record_validates(self):
        claim = doc(resolution={'state': 'fixed_candidate', 'evidence': [f'commit {FIX[:10]}']})
        code, out, _ = self.run_cli(claim)
        self.assertEqual(code, 0)
        record = json.loads(out)
        self.assertEqual(SCHEMA.errors(record, 'urn:af:schema:issue-triage-merged:1'), [])
        self.assertEqual((record['verdict'], record['labels'][-1]), ('agreed', 'triage/maybe-fixed'))
        self.assertEqual(record['evidence_check']['commits'], [FIX[:10]])
        self.assertEqual(record['generation'], {'issue': 432, 'bundle_sha256': IDENTITY['bundle_sha256'],
                                                'main_commit': FIXTURE_MAIN,
                                                'policy_version': IDENTITY['policy_version'],
                                                'event_id': IDENTITY['event_id']})
        self.assertEqual(record['sources'], {key: IDENTITY[key] for key in ('task_id', 'document_id', 'evaluation_id')})

    def test_an_unreachable_or_unknown_commit_is_refused_with_exit_1(self):
        for sha in (SIDE[:12], 'deadbeef1'):
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

    def test_a_missing_identity_is_a_usage_error_before_anything_is_printed(self):
        for flag, _ in triage.IDENTITIES:
            with self.subTest(flag):
                code, out, err = self.run_cli(doc(), omit=(flag,))
                self.assertEqual((code, out), (2, ''))
                self.assertIn('usage:', err)
                self.assertIn(f'needs {flag}', err)

    def test_a_passed_evaluation_without_the_verifier_block_is_refused(self):
        code, out, err = self.run_cli(doc(), None, 'passed')
        self.assertEqual((code, out), (1, ''))
        self.assertIn('refused: verifier-block:', err)

    def test_a_security_hold_exits_3_and_prints_only_the_private_hold(self):
        def held(*args, **kwargs):
            code, out, err = self.run_cli(*args, **kwargs)
            self.assertEqual(code, 3, err)
            self.assertIn('held:', err)
            record = json.loads(out)
            self.assertEqual((record['schema'], SCHEMA.errors(record, 'urn:af:schema:issue-triage-hold:1')),
                             (triage.HOLD, []))
            return record

        every = tuple(flag for flag, _ in triage.IDENTITIES)
        record = held(doc(**{'class': 'security'}), None, 'passed', '--owner-label', 'size/S', omit=every)
        self.assertEqual((record['security_triagers'], record['generation']), (['author'], None))
        self.assertEqual(held(doc(), doc(), 'passed', '--label', 'class/security')['held_labels'], ['class/security'])
        for label in ('class/security', 'triage/held'):
            with self.subTest(label):
                record = held(doc(), doc(), 'passed', '--owner-label', label, omit=every)
                self.assertEqual(record['held_labels'], [label])
        # An issue already held stays held even when its documents cannot be read.
        record = held(doc(), doc(), 'passed', '--label', 'triage/held', '--evaluation', str(self.dir / 'absent.json'))
        self.assertEqual(record['generation']['main'], 'main')

    def test_label_holds_never_read_any_input_and_need_no_publication_identity(self):
        for flag in ('--label', '--owner-label'):
            for label in triage.HOLD_LABELS:
                for author_flag in ('--document', '--draft'):
                    for generation in ([], ['--main', 'main', '--event-id', 'opened-1',
                                             '--bundle-sha256', 'given-bundle', '--policy-version', 'given-policy']):
                        with self.subTest(flag=flag, label=label, author=author_flag, generation=generation):
                            out, err = io.StringIO(), io.StringIO()
                            with contextlib.ExitStack() as stack:
                                author = stack.enter_context(mock.patch.object(
                                    triage, '_read_author', side_effect=AssertionError('author read')))
                                read = stack.enter_context(mock.patch.object(
                                    triage, '_load_json', side_effect=AssertionError('JSON read')))
                                stack.enter_context(contextlib.redirect_stdout(out))
                                stack.enter_context(contextlib.redirect_stderr(err))
                                code = triage.main([author_flag, str(self.dir / 'absent-author'),
                                                    '--evaluation', str(self.dir / 'absent-evaluation'),
                                                    '--pulls', str(self.dir / 'absent-pulls'),
                                                    '--hand-triage', str(self.dir / 'absent-hand'),
                                                    flag, label, *generation])
                            author.assert_not_called()
                            read.assert_not_called()
                            self.assertEqual(code, 3, err.getvalue())
                            record = json.loads(out.getvalue())
                            self.assertEqual(SCHEMA.errors(record, 'urn:af:schema:issue-triage-hold:1'), [])
                            self.assertEqual(record, {
                                'schema': triage.HOLD, 'issue': None, 'reasons': [f'the issue carries {label}'],
                                'held_labels': [label], 'security_triagers': [],
                                'generation': {'bundle_sha256': 'given-bundle', 'main': 'main',
                                               'policy_version': 'given-policy', 'event_id': 'opened-1'}
                                if generation else None,
                            })
                            self.assertIn('held:', err.getvalue())
                            self.assertNotIn('Traceback', err.getvalue())

    def security_hold_without_later_reads(self, who):
        read_text = Path.read_text
        every = tuple(flag for flag, _ in triage.IDENTITIES)
        for draft in (False, True):
            # A directory cannot be read as a file, even when tests run as root.
            for unreadable in (self.dir / 'absent-auxiliary', self.dir):
                with self.subTest(who=who, draft=draft, unreadable=unreadable):
                    def guarded_read(path, *args, **kwargs):
                        if path == unreadable:
                            raise AssertionError(f'hold tried to read {path}')
                        return read_text(path, *args, **kwargs)

                    extra = ['--pulls', str(unreadable), '--hand-triage', str(unreadable),
                             '--owner-label', 'size/S', '--owner-label', 'size/L']
                    if who == 'author':
                        extra += ['--evaluation', str(unreadable)]
                    author = doc(size='S', adr='new ADR: x', resolution={'state': 'fixed_candidate'})
                    verifier = doc(**{'class': 'security'})
                    if who == 'author':
                        author['class'] = 'security'
                    with contextlib.ExitStack() as stack:
                        read = stack.enter_context(mock.patch.object(
                            Path, 'read_text', autospec=True, side_effect=guarded_read))
                        for name in ('merge', 'check_rules', 'owner_overrides'):
                            stack.enter_context(mock.patch.object(
                                triage, name, side_effect=AssertionError(f'hold called {name}')))
                        stack.enter_context(mock.patch.object(
                            triage.GitReach, '_git', side_effect=AssertionError('hold ran Git')))
                        code, out, err = self.run_cli(author, verifier, 'failed', *extra, draft=draft, omit=every)
                    self.assertNotIn(unreadable, [call.args[0] for call in read.call_args_list])
                    self.assertEqual(code, 3, err)
                    self.assertEqual(err, 'backlog-check-triage: held: security hold; '
                                         'publish nothing about this issue\n')
                    record = json.loads(out)
                    self.assertEqual(SCHEMA.errors(record, 'urn:af:schema:issue-triage-hold:1'), [])
                    self.assertEqual(record, {
                        'schema': triage.HOLD, 'issue': 432, 'reasons': [f'the {who} classified security'],
                        'held_labels': [], 'security_triagers': [who], 'generation': None,
                    })

    def test_author_security_never_reads_evaluation_or_auxiliary_inputs(self):
        self.security_hold_without_later_reads('author')

    def test_verifier_security_never_reads_pulls_or_hand_triage_or_processes_publication(self):
        self.security_hold_without_later_reads('verifier')

    def test_non_string_relevant_draft_bodies_are_refused_and_label_holds_still_win(self):
        for heading in ('Triage', 'Rationale'):
            for body in (42, ['nonempty'], False, {}, []):
                sections = [{'heading': 'Triage', 'body': '```json\n' + json.dumps(doc()) + '\n```'},
                            {'heading': 'Rationale', 'body': 'Why.'}]
                next(section for section in sections if section['heading'] == heading)['body'] = body
                for flag in (None, '--label', '--owner-label'):
                    with self.subTest(heading=heading, body=body, flag=flag):
                        extra = [flag, 'triage/held'] if flag else []
                        code, out, err = self.run_cli(doc(), AS_AUTHOR, 'passed', *extra, draft=True, sections=sections)
                        self.assertEqual(len(err.splitlines()), 1, err)
                        self.assertNotIn('Traceback', err)
                        if flag:
                            self.assertEqual(code, 3, err)
                            self.assertEqual(SCHEMA.errors(json.loads(out), 'urn:af:schema:issue-triage-hold:1'), [])
                        else:
                            self.assertEqual((code, out), (1, ''))
                            self.assertEqual(err, f'backlog-check-triage: refused: author-draft: '
                                                 f'the draft {heading} section body must be a string\n')

    def test_missing_or_null_relevant_draft_bodies_keep_the_blank_section_refusal(self):
        for heading in ('Triage', 'Rationale'):
            for missing in (False, True):
                with self.subTest(heading=heading, missing=missing):
                    sections = [{'heading': 'Triage', 'body': '```json\n' + json.dumps(doc()) + '\n```'},
                                {'heading': 'Rationale', 'body': 'Why.'}]
                    section = next(section for section in sections if section['heading'] == heading)
                    section['body'] = None
                    if missing:
                        del section['body']
                    code, out, err = self.run_cli(doc(), draft=True, sections=sections)
                    self.assertEqual((code, out), (1, ''))
                    self.assertEqual(err, f'backlog-check-triage: refused: author-draft: '
                                         f'the report has no nonblank {heading} section\n')

    def test_malformed_draft_sections_hold_for_every_label_source_or_are_normally_refused(self):
        # Include the evaluator's exact sections: 42 reproduction and other non-array shapes.
        for sections in (42, None, False, '', 'Triage', {}, {'heading': 'Triage'}):
            path = self.dir / 'malformed-draft.json'
            path.write_text(json.dumps({'schema': 'af.document-draft/2', 'sections': sections}))
            for label in (None, *triage.HOLD_LABELS):
                for owner_label in (None, *triage.HOLD_LABELS):
                    with self.subTest(sections=sections, label=label, owner_label=owner_label):
                        labels = (['--label', label] if label else [])
                        labels += ['--owner-label', owner_label] if owner_label else []
                        out, err = io.StringIO(), io.StringIO()
                        with contextlib.redirect_stdout(out), contextlib.redirect_stderr(err):
                            code = triage.main(['--draft', str(path), '--evaluation',
                                                str(self.dir / 'absent-evaluation'), *labels])
                        self.assertNotIn('Traceback', err.getvalue())
                        if labels:
                            self.assertEqual(code, 3, err.getvalue())
                            record = json.loads(out.getvalue())
                            self.assertEqual(SCHEMA.errors(record, 'urn:af:schema:issue-triage-hold:1'), [])
                            self.assertEqual(record['held_labels'], list(dict.fromkeys(
                                value for value in (label, owner_label) if value)))
                            self.assertIsNone(record['issue'])
                            self.assertIsNone(record['generation'])
                            self.assertEqual(record['security_triagers'], [])
                            self.assertIn('held:', err.getvalue())
                        else:
                            self.assertEqual((code, out.getvalue()), (1, ''))
                            self.assertIn('refused: author-draft:', err.getvalue())

    def test_an_unknown_main_is_refused(self):
        code, _, err = self.run_cli(doc(), doc(), 'passed', '--main', 'nonexistent')
        self.assertEqual(code, 1)
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
        # §6.1's budget, unraised: 150k tokens, 20 minutes, the report pipeline's 8 Attempts.
        self.assertEqual({key: task['limits'][key] for key in ('tokens', 'wall_ms', 'max_attempts')},
                         {'tokens': 150_000, 'wall_ms': 1_200_000, 'max_attempts': 8})

    def test_the_worker_allowances_fit_the_task_budget(self):
        contracts = json.loads((STAGED / 'contracts.json').read_text(encoding='utf-8'))
        for worker in ('triage-author', 'triage-verifier'):
            with self.subTest(worker):
                manifest = (STAGED / 'task-packages' / 'backlog' / worker / 'worker.toml').read_text(encoding='utf-8')
                # Only these integer allowance tables are needed, not a general TOML parser.
                tables = re.findall(r'(?ms)^\[signature\.attempt\][ \t]*\n(.*?)(?=^\[|\Z)', manifest)
                self.assertEqual(len(tables), 1)
                attempt = {}
                for line in tables[0].splitlines():
                    line = line.split('#', 1)[0].strip()
                    if not line:
                        continue
                    field = re.fullmatch(r'(\w+)\s*=\s*([0-9]+)', line)
                    self.assertIsNotNone(field, line)
                    key, value = field.groups()
                    self.assertNotIn(key, attempt)
                    attempt[key] = int(value)
                self.assertEqual(attempt, {'tokens': 40_000, 'wall_ms': 500_000})
                self.assertEqual(contracts['workers'][f'backlog/{worker}']['attempt'],
                                 {'tokens': 40_000, 'wall_ms': 500_000})

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
