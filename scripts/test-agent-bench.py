#!/usr/bin/env python3
"""Synthetic offline conformance; these fixtures are never navigation measurements."""
import copy
import importlib.util
import json
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest

sys.dont_write_bytecode = True
SCRIPT = Path(__file__).with_name('analyze-agent-bench.py')
SPEC = importlib.util.spec_from_file_location('agent_bench', SCRIPT)
B = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(B)


SEMANTIC_SCRIPT = SCRIPT.parent.parent / 'crates/af-sandbox/src/navigation/semantic.py'
SEMANTIC_SPEC = importlib.util.spec_from_file_location('semantic_driver', SEMANTIC_SCRIPT)
SEMANTIC = importlib.util.module_from_spec(SEMANTIC_SPEC)
SEMANTIC_SPEC.loader.exec_module(SEMANTIC)


class AnalyzerShutdown(unittest.TestCase):
    def finish_peer(self, code):
        with subprocess.Popen([sys.executable, '-I', '-c', code], stdin=subprocess.PIPE,
                              stdout=subprocess.PIPE) as server:
            try:
                SEMANTIC.send(server, {'method': 'exit', 'params': None})
                SEMANTIC.finish(server)
            finally:
                if server.poll() is None:
                    server.kill()

    def test_drains_final_notifications_before_reaping(self):
        self.finish_peer(
            "import json, sys; sys.stdin.buffer.read(); "
            "body=json.dumps({'method':'$/progress','params':'x'*65536}).encode(); "
            "sys.stdout.buffer.write((b'Content-Length: %d\\r\\n\\r\\n'%len(body)+body)*3)"
        )

    def test_nonzero_exit_and_excessive_output_fail(self):
        with self.assertRaisesRegex(ValueError, 'did not exit cleanly'):
            self.finish_peer("import sys; sys.stdin.buffer.read(); sys.exit(7)")
        with self.assertRaisesRegex(ValueError, 'output exceeds limit'):
            self.finish_peer("import sys; sys.stdin.buffer.read(); sys.stdout.buffer.write(b'x'*3000000)")

    def test_stalled_exit_has_a_deadline(self):
        with self.assertRaisesRegex(ValueError, 'exit timed out'):
            self.finish_peer("import sys, time; sys.stdin.buffer.read(); time.sleep(30)")


def fixture(mode='smoke'):
    objects = {}

    def add(value):
        key = B.identity(value)
        objects[key] = value
        return key

    schedule = dict(schema='af.agent-bench-schedule/2', mode=mode,
                    **{key: add({'fixture': key}) for key in
                       ('protocol', 'oracles', 'source_base', 'source_head', 'model', 'collector')},
                    cases=add(dict(schema='af.agent-bench-cases/1', cases=[
                        dict(id='fixture_case', family='navigation', prompt='Find the fixture symbol.')])),
                    slots=[dict(id='a', pair='p1', family='navigation', side='base',
                                case='fixture_case', repetition=1),
                           dict(id='b', pair='p1', family='navigation', side='head',
                                case='fixture_case', repetition=1)])
    if mode == 'tool_only':
        names = ['af_list_paths', 'af_read_range', 'af_search']
        schedule.update(schema='af.agent-bench-tool-schedule/1', mode='exposed_ab',
                        configuration=add({'fixture': 'shared runtime configuration'}),
                        tools_base=add(names),
                        tools_head=add(names + ['af_definition', 'af_references', 'af_implementation']))
    if mode in ('aa', 'tool_only'):
        del objects[schedule['source_head']]
        schedule['source_head'] = schedule['source_base']
    if mode != 'smoke':
        objects.pop(schedule['cases'])
        cases = {f'fixture_{index}': dict(id=f'fixture_{index}', family='navigation',
                                         prompt='Find the fixture symbol.') for index in range(12)}
        schedule['cases'] = add(dict(schema='af.agent-bench-cases/1', cases=list(cases.values())))
        schedule['slots'] = B.frozen_slots(cases)
    schedule_id = add(schedule)
    receipts, grades = [], []
    for scheduled in schedule['slots']:
        slot = scheduled['id']
        input_tokens, elapsed = (100, 1000) if scheduled['side'] == 'base' else (80, 900)
        answer = add({'answer': slot})
        receipts.append(add(dict(schema='af.agent-bench-receipt/1', schedule=schedule_id,
                                 slot=slot, status='ok', wall_ns=elapsed, timing_complete=True,
                                 trace_complete=False, answer=answer,
                                 invocations=[dict(id='call_' + slot, input=input_tokens, output=10,
                                                   cached_input=20, reasoning_output=5,
                                                   chargeable=input_tokens - 20 + 10, usage_complete=True)])))
        grades.append(add(dict(schema='af.agent-bench-grade/1', schedule=schedule_id,
                               slot=slot, answer=answer, oracle=schedule['oracles'],
                               verdict='correct', evidence=add({'grade_fixture': slot}))))
    return dict(schema=B.VERSION, schedule=schedule_id, objects=objects,
                receipts=receipts, grades=grades)


def replace(bundle, ref, mutate):
    """Rehash deliberate mutations, so invariant tests get past integrity validation."""
    value = bundle['objects'].pop(ref)
    mutate(value)
    new_ref = B.identity(value)
    bundle['objects'][new_ref] = value
    for key in ('receipts', 'grades'):
        bundle[key] = [new_ref if item == ref else item for item in bundle[key]]


class AgentBenchTests(unittest.TestCase):
    def test_retained_goldens_match_executable_fixture(self):
        root = SCRIPT.parent.parent / 'fixtures' / 'agent-bench'
        bundle = B.read(root / 'evidence.json')
        self.assertEqual(bundle, fixture())
        self.assertEqual(B.analyze(bundle), B.read(root / 'analysis.json'))

    def test_paired_golden_cached_and_reasoning_are_subsets(self):
        result = B.analyze(fixture())
        self.assertEqual(result['pairs'], [dict(pair='p1', family='navigation', case='fixture_case',
                                               repetition=1, eligible=True, correct_eligible=True,
                                               native_tokens=-20, wall_ns=-100)])
        self.assertEqual(result['totals']['base']['native_tokens'], 110)
        self.assertEqual(result['totals']['head']['native_tokens'], 90)
        self.assertEqual(result['totals']['base']['chargeable'], 90)
        self.assertEqual(result['limits'], ['navigation_trace_incomplete'])
        self.assertEqual(result['qualification'], 'structural_only')
        self.assertEqual(result['conclusion'], 'no_improvement_claim')

    def test_aa_never_claims_equivalence_or_improvement(self):
        report = B.analyze(fixture('aa'))
        self.assertEqual(report['mode'], 'aa')
        self.assertEqual(report['conclusion'], 'no_improvement_claim')

    def test_exposed_ab_requires_distinct_sources_and_retains_frozen_order(self):
        aa, ab = fixture('aa'), fixture('exposed_ab')
        schedule = ab['objects'][ab['schedule']]
        self.assertEqual(schedule['slots'], aa['objects'][aa['schedule']]['slots'])
        self.assertEqual(B.analyze(ab)['mode'], 'exposed_ab')
        old = ab['schedule']
        ab['objects'].pop(old)
        ab['objects'].pop(schedule['source_head'])
        schedule['source_head'] = schedule['source_base']
        ab['schedule'] = B.identity(schedule)
        ab['objects'][ab['schedule']] = schedule
        with self.assertRaisesRegex(ValueError, 'A/B must bind different sources'):
            B.analyze(ab)

    def test_tool_only_ab_has_matched_sources_and_shared_model_and_configuration(self):
        bundle = fixture('tool_only')
        report = B.analyze(bundle)
        self.assertEqual(report['mode'], 'exposed_ab')
        self.assertEqual(report['resources'], B.analyze(fixture('exposed_ab'))['resources'])
        for mutate, reason in (
            (lambda schedule: schedule.update(source_head=schedule['configuration']), 'same source'),
            (lambda schedule: schedule.update(tools_head=schedule['tools_base']), 'exact navigation catalogs'),
            (lambda schedule: schedule.update(model_head=schedule['model']), 'unknown or missing fields'),
            (lambda schedule: schedule.update(config_head=schedule['configuration']), 'unknown or missing fields'),
        ):
            with self.subTest(reason=reason), self.assertRaisesRegex(ValueError, reason):
                changed = copy.deepcopy(bundle)
                old = changed['schedule']
                schedule = changed['objects'].pop(old)
                mutate(schedule)
                changed['schedule'] = B.identity(schedule)
                changed['objects'][changed['schedule']] = schedule
                B.analyze(changed)

    def test_unknown_first_known_later_retry_is_incomplete(self):
        bundle = fixture()
        def change(value):
            first = dict(value['invocations'][0], id='failed', input=None, output=None,
                         cached_input=None, reasoning_output=None, chargeable=None, usage_complete=False)
            value['invocations'].insert(0, first)
        replace(bundle, bundle['receipts'][0], change)
        report = B.analyze(bundle)
        base = report['totals']['base']
        self.assertIsNone(base['native_tokens'])
        self.assertIsNone(base['chargeable'])
        self.assertEqual(base['known_native_tokens'], 110)
        self.assertFalse(report['pairs'][0]['correct_eligible'])
        self.assertFalse(report['pairs'][0]['eligible'])
        self.assertIsNone(report['pairs'][0]['wall_ns'])

    def test_failures_wrong_and_ambiguous_stay_in_denominator(self):
        for status in ('error', 'timeout', 'cancelled', 'lost'):
            with self.subTest(status=status):
                bundle = fixture()
                replace(bundle, bundle['receipts'][0], lambda r: r.update(status=status))
                report = B.analyze(bundle)
                self.assertEqual(report['totals']['base']['slots'], 1)
                self.assertEqual(report['totals']['base']['failures'], 1)
                self.assertEqual(report['totals']['base']['correct'], 0)
                self.assertFalse(report['pairs'][0]['correct_eligible'])
                self.assertTrue(report['pairs'][0]['eligible'])
                self.assertEqual(report['pairs'][0]['native_tokens'], -20)
                self.assertEqual(report['pairs'][0]['wall_ns'], -100)
        for verdict in ('wrong', 'ambiguous'):
            bundle = fixture()
            replace(bundle, bundle['grades'][0], lambda g: g.update(verdict=verdict))
            report = B.analyze(bundle)
            self.assertEqual(report['totals']['base']['correct'], 0)
            self.assertFalse(report['pairs'][0]['correct_eligible'])

    def test_wrong_answers_keep_all_outcome_resource_deltas(self):
        bundle = fixture()
        replace(bundle, bundle['grades'][0], lambda g: g.update(verdict='wrong'))
        pair = B.analyze(bundle)['pairs'][0]
        self.assertTrue(pair['eligible'])
        self.assertFalse(pair['correct_eligible'])
        self.assertEqual((pair['native_tokens'], pair['wall_ns']), (-20, -100))

    def test_unstarted_suffix_retains_unknown_resources(self):
        bundle = fixture()
        grade = bundle['objects'].pop(bundle['grades'].pop())
        bundle['objects'].pop(grade['evidence'])
        bundle['objects'].pop(grade['answer'])
        replace(bundle, bundle['receipts'][1], lambda r: r.update(
            status='not_started', answer=None, invocations=[], wall_ns=None,
            timing_complete=False, trace_complete=False))
        report = B.analyze(bundle)
        self.assertEqual(report['totals']['head']['failures'], 1)
        self.assertIsNone(report['totals']['head']['native_tokens'])
        self.assertFalse(report['pairs'][0]['eligible'])
        for activity in (dict(wall_ns=0), dict(timing_complete=True, wall_ns=0),
                         dict(trace_complete=True)):
            bad = copy.deepcopy(bundle)
            replace(bad, bad['receipts'][1], lambda r: r.update(activity))
            with self.assertRaisesRegex(ValueError, 'not_started'):
                B.analyze(bad)

    def test_weighting_averages_repetitions_cases_then_equal_families(self):
        pairs, measured, slots = [], {}, {}
        # The many repetitions of one case and the many cases of one family must not
        # outweigh a small family. Means: large family (1 + 0)/2, small family -1.
        for case, family, repetitions, delta in (
            ('c1', 'large', 3, 10), ('c2', 'large', 1, 0), ('c3', 'small', 1, -10)
        ):
            for repetition in range(repetitions):
                key = case + str(repetition)
                pairs.append(dict(pair=key, family=family, case=case, eligible=True,
                                  native_tokens=delta, wall_ns=delta))
                slots[key] = {'base': {'id': key}}
                measured[key] = (True, True, 10, 10)
        result = B.weighted_resources(pairs, measured, slots)
        for endpoint in result.values():
            self.assertEqual(endpoint['relative_difference'], dict(numerator=-1, denominator=4))
            self.assertEqual(endpoint['screen'], 'outside_bounds')
        measured['c10'] = (True, True, 0, 0)
        self.assertEqual(B.weighted_resources(pairs, measured, slots)['native_tokens']
                         ['zero_baseline_pairs'], ['c10'])
        pairs[0]['eligible'] = False
        self.assertIsNone(B.weighted_resources(pairs, measured, slots)['native_tokens']
                          ['relative_difference'])

    def test_twenty_percent_screen_boundary_uses_exact_signed_mean(self):
        slots = {'pair': {'base': {'id': 'slot'}}}
        measured = {'slot': (True, True, 5, 5)}
        for delta, screen in ((-1, 'within_bounds'), (1, 'within_bounds'),
                              (-2, 'outside_bounds'), (2, 'outside_bounds')):
            pairs = [dict(pair='pair', family='family', case='case', eligible=True,
                          native_tokens=delta, wall_ns=delta)]
            for endpoint in B.weighted_resources(pairs, measured, slots).values():
                self.assertEqual(endpoint['screen'], screen)

    def test_frozen_aa_has_adjacent_balanced_case_repetition_pairs(self):
        bundle = fixture('aa')
        schedule = bundle['objects'][bundle['schedule']]
        slots = schedule['slots']
        self.assertEqual(len(slots), 72)
        orders = [
            [10, 1, 7, 3, 5, 8, 9, 4, 2, 0, 6, 11],
            [7, 9, 1, 4, 0, 2, 3, 8, 6, 11, 10, 5],
            [6, 9, 7, 3, 5, 11, 4, 2, 0, 1, 10, 8],
        ]
        for repetition in (1, 2, 3):
            first = [slots[i]['side'] for i in range(0, 72, 2)
                     if slots[i]['repetition'] == repetition]
            self.assertEqual(first, ['base'] * 6 + ['head'] * 6)
            self.assertEqual([slots[i]['case'] for i in range(0, 72, 2)
                              if slots[i]['repetition'] == repetition],
                             ['fixture_' + str(index) for index in orders[repetition - 1]])
        for index in range(0, 72, 2):
            self.assertEqual(slots[index]['case'], slots[index + 1]['case'])
            self.assertEqual(slots[index]['pair'], slots[index + 1]['pair'])
        report = B.analyze(bundle)
        self.assertEqual(report['totals']['base']['slots'], 36)
        self.assertEqual(report['resources']['native_tokens']['screen'], 'within_bounds')
        # Even a freshly rehashed schedule cannot change its randomized order.
        old = bundle['schedule']
        schedule = bundle['objects'].pop(old)
        schedule['slots'][0:2] = reversed(schedule['slots'][0:2])
        bundle['schedule'] = B.identity(schedule)
        bundle['objects'][bundle['schedule']] = schedule
        with self.assertRaisesRegex(ValueError, 'frozen schedule'):
            B.analyze(bundle)

    def test_pair_requires_same_admitted_case_not_only_family(self):
        bundle = fixture()
        old = bundle['schedule']
        schedule = bundle['objects'].pop(old)
        package = bundle['objects'].pop(schedule['cases'])
        package['cases'].append(dict(id='another', family='navigation', prompt='Another fixture.'))
        schedule['cases'] = B.identity(package)
        bundle['objects'][schedule['cases']] = package
        schedule['slots'][1]['case'] = 'another'
        new = B.identity(schedule)
        bundle['objects'][new] = schedule
        bundle['schedule'] = new
        for port in ('receipts', 'grades'):
            for ref in list(bundle[port]):
                replace(bundle, ref, lambda record: record.update(schedule=new))
        with self.assertRaisesRegex(ValueError, 'pair case/repetition'):
            B.analyze(bundle)

    def test_case_package_admission_refuses_malformed_records(self):
        for mutate in (
            lambda package: package.update(schema='retired/0'),
            lambda package: package.update(extra=True),
            lambda package: package['cases'].clear(),
            lambda package: package['cases'].append(copy.deepcopy(package['cases'][0])),
            lambda package: package['cases'][0].update(prompt=''),
            lambda package: package['cases'][0].update(oracle='forbidden'),
        ):
            bundle = fixture()
            old = bundle['schedule']
            schedule = bundle['objects'].pop(old)
            package = bundle['objects'].pop(schedule['cases'])
            mutate(package)
            schedule['cases'] = B.identity(package)
            bundle['objects'][schedule['cases']] = package
            bundle['schedule'] = B.identity(schedule)
            bundle['objects'][bundle['schedule']] = schedule
            with self.assertRaises(ValueError):
                B.analyze(bundle)

    def test_case_repetition_cannot_be_paired_twice_under_different_labels(self):
        bundle = fixture()
        schedule = bundle['objects'].pop(bundle['schedule'])
        schedule['slots'].extend([
            dict(schedule['slots'][0], id='c', pair='p2'),
            dict(schedule['slots'][1], id='d', pair='p2'),
        ])
        bundle['schedule'] = B.identity(schedule)
        bundle['objects'][bundle['schedule']] = schedule
        with self.assertRaisesRegex(ValueError, 'duplicate case/repetition pair'):
            B.analyze(bundle)

    def test_missing_grade_is_unresolved(self):
        bundle = fixture()
        ref = bundle['grades'].pop()
        grade = bundle['objects'].pop(ref)
        bundle['objects'].pop(grade['evidence'])
        report = B.analyze(bundle)
        self.assertIn('semantic_grade_unresolved', report['limits'])
        self.assertEqual(report['totals']['head']['slots'], 1)
        self.assertFalse(report['pairs'][0]['correct_eligible'])

    def test_missing_timing_is_not_zero(self):
        bundle = fixture()
        replace(bundle, bundle['receipts'][0], lambda r: r.update(wall_ns=None, timing_complete=False))
        report = B.analyze(bundle)
        self.assertIsNone(report['totals']['base']['wall_ns'])
        self.assertFalse(report['pairs'][0]['correct_eligible'])

    def test_bad_evidence_is_refused(self):
        mutations = [
            lambda b: b.update(schema='af.agent-bench-evidence/0'),
            lambda b: b.update(extra=True),
            lambda b: b.update(schema='af.agent-bench-evidence/1'),
            lambda b: replace(b, b['receipts'][0], lambda r: r.update(status='not_started')),
            lambda b: replace(b, b['receipts'][1], lambda r: r['invocations'][0].update(id='call_a')),
            lambda b: replace(b, b['receipts'][0], lambda r: r['invocations'][0].update(chargeable=50)),
            lambda b: b['receipts'].pop(),
            lambda b: b['receipts'].append(b['receipts'][0]),
            lambda b: b['grades'].append(b['grades'][0]),
            lambda b: b['objects'][b['schedule']].update(mode='aa'),  # invalidates hash
            lambda b: b['objects'].update({B.identity({}): {}}),
            lambda b: replace(b, b['receipts'][0], lambda r: r.update(slot='foreign')),
            lambda b: replace(b, b['receipts'][0], lambda r: r.update(schedule='foreign')),
            lambda b: replace(b, b['receipts'][0], lambda r: r.update(wall_ns=True)),
            lambda b: replace(b, b['receipts'][0], lambda r: r.update(status='unknown')),
            lambda b: replace(b, b['receipts'][0], lambda r: r.update(answer=None)),
            lambda b: replace(b, b['receipts'][0], lambda r: r['invocations'].append(copy.deepcopy(r['invocations'][0]))),
            lambda b: replace(b, b['receipts'][0], lambda r: r['invocations'][0].update(cached_input=101)),
            lambda b: replace(b, b['receipts'][0], lambda r: r['invocations'][0].update(reasoning_output=11)),
            lambda b: replace(b, b['receipts'][0], lambda r: r['invocations'][0].update(input=-1)),
            lambda b: replace(b, b['receipts'][0], lambda r: r['invocations'][0].update(input=None)),
            lambda b: replace(b, b['grades'][0], lambda g: g.update(oracle='foreign')),
            lambda b: replace(b, b['grades'][0], lambda g: g.update(answer='foreign')),
        ]
        for index, mutate in enumerate(mutations):
            with self.subTest(index=index):
                bundle = fixture()
                mutate(bundle)
                with self.assertRaises(ValueError):
                    B.analyze(bundle)

    def test_frozen_schedule_shape_is_checked_after_rehashing(self):
        for mutate in (
            lambda s: s['slots'].pop(),
            lambda s: s['slots'][1].update(side='base'),
            lambda s: s['slots'][1].update(family='other'),
            lambda s: s['slots'][1].update(case='unknown'),
            lambda s: s['slots'][1].update(repetition=2),
            lambda s: s['slots'][1].update(repetition=True),
            lambda s: s['slots'][1].update(id='a'),
            lambda s: s.update(mode='aa'),
            lambda s: s.update(mode='exposed_ab'),
        ):
            bundle = fixture()
            old = bundle['schedule']
            schedule = bundle['objects'].pop(old)
            mutate(schedule)
            new = B.identity(schedule)
            bundle['objects'][new] = schedule
            bundle['schedule'] = new
            for port in ('receipts', 'grades'):
                for ref in list(bundle[port]):
                    replace(bundle, ref, lambda r: r.update(schedule=new))
            with self.assertRaises(ValueError):
                B.analyze(bundle)

    def test_cli_is_deterministic_and_does_not_modify_evidence(self):
        with tempfile.TemporaryDirectory() as tmp:
            path = Path(tmp) / 'evidence.json'
            raw = B.canonical(fixture())
            path.write_bytes(raw)
            command = [sys.executable, '-I', str(SCRIPT), str(path)]
            first = subprocess.check_output(command)
            self.assertEqual(first, subprocess.check_output(command))
            self.assertEqual(first, B.canonical(B.analyze(fixture())) + b'\n')
            self.assertEqual(path.read_bytes(), raw)
            for bad in (B.canonical(fixture()).decode('ascii').encode('utf-16'),
                        b'{"schema":1,"schema":2}', b'{"x":NaN}', b'[' * 2000,
                        b' ' * (B.MAX_BYTES + 1)):
                path.write_bytes(bad)
                result = subprocess.run(command, capture_output=True)
                self.assertEqual(result.returncode, 1)
                self.assertEqual(result.stdout, b'')


if __name__ == '__main__':
    unittest.main()
