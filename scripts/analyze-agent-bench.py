#!/usr/bin/env python3
"""Offline, structural agent-navigation evidence analysis; never executes a subject."""
import argparse
from fractions import Fraction
import hashlib
import json
from pathlib import Path
import re
import sys

sys.dont_write_bytecode = True
VERSION = 'af.agent-bench-evidence/2'
MAX_BYTES = 8 * 1024 * 1024
MAX_SLOTS = 512
MAX_COUNTER = (1 << 53) - 1
DIGEST = re.compile(r'sha256:[0-9a-f]{64}\Z')


def require(condition, reason):
    if not condition:
        raise ValueError(reason)


def fields(value, names):
    require(type(value) is dict and set(value) == set(names.split()),
            'unknown or missing fields: ' + names)


def integer(value):
    require(type(value) is int and 0 <= value <= MAX_COUNTER, 'invalid counter')
    return value


def name(value):
    require(type(value) is str and re.fullmatch(r'[A-Za-z0-9_-]{1,128}', value),
            'invalid identifier')
    return value


def canonical(value):
    return json.dumps(value, sort_keys=True, separators=(',', ':'),
                      ensure_ascii=True, allow_nan=False).encode('ascii')


def identity(value):
    return 'sha256:' + hashlib.sha256(canonical(value)).hexdigest()


def unique_members(pairs):
    result = {}
    for key, value in pairs:
        require(key not in result, 'duplicate JSON member')
        result[key] = value
    return result


def read(path):
    with Path(path).open('rb') as stream:
        raw = stream.read(MAX_BYTES + 1)
    require(len(raw) <= MAX_BYTES, 'evidence exceeds byte bound')
    return json.loads(raw.decode('utf-8'), object_pairs_hook=unique_members,
                      parse_constant=lambda _: require(False, 'non-finite JSON number'))


def frozen_slots(cases):
    """Reconstruct the reviewed exposed 12-case, three-repetition paired order."""
    require(len(cases) == 12, 'AA/AB requires twelve distinct cases')
    slots = []
    for repetition in range(1, 4):
        def order(case_id):
            raw = ('aa-order-v1\0agent-bench-aa-20261112-v1\0'
                   + str(repetition) + '\0' + case_id).encode('utf-8')
            return hashlib.sha256(raw).digest()
        for index, case_id in enumerate(sorted(cases, key=order)):
            pair = case_id + '-r' + str(repetition)
            sides = ('base', 'head') if index < 6 else ('head', 'base')
            for side in sides:
                slots.append(dict(id=pair + '-' + side, pair=pair,
                                  case=case_id, repetition=repetition,
                                  family=cases[case_id]['family'], side=side))
    return slots


def weighted_resources(pairs, per_slot, schedule_pairs):
    """Average repetitions within case, cases within family, then equal families.

    Exact rationals avoid roundoff near the 20% screen. Zero baselines remain explicit;
    one endpoint unit is the denominator for their diagnostic relative differences.
    This is a descriptive screen, never execution or equivalence authority.
    """
    endpoints = {}
    for metric, position in (('native_tokens', 2), ('wall_ns', 3)):
        by_family, zeros = {}, []
        complete = True
        for pair in pairs:
            if not pair['eligible']:
                complete = False
                continue
            slots = schedule_pairs[pair['pair']]
            baseline = per_slot[slots['base']['id']][position]
            if baseline == 0:
                zeros.append(pair['pair'])
            value = Fraction(pair[metric], max(baseline, 1))
            by_family.setdefault(pair['family'], {}).setdefault(pair['case'], []).append(value)
        families = {}
        for family, cases in sorted(by_family.items()):
            means = [sum(values, Fraction()) / len(values) for values in cases.values()]
            families[family] = sum(means, Fraction()) / len(means)
        mean = sum(families.values(), Fraction()) / len(families) if families and complete else None
        def rational(value):
            return dict(numerator=value.numerator, denominator=value.denominator)
        endpoints[metric] = dict(
            relative_difference=rational(mean) if mean is not None else None,
            families={key: rational(value) for key, value in families.items()} if complete else {},
            zero_baseline_pairs=zeros,
            screen=('incomplete' if mean is None else
                    'outside_bounds' if abs(mean) > Fraction(1, 5) else 'within_bounds'))
    return endpoints


def analyze(bundle):
    """Verify every reference before deriving totals. No external paths are opened."""
    fields(bundle, 'schema schedule objects receipts grades')
    require(bundle['schema'] == VERSION, 'unsupported evidence version')
    objects = bundle['objects']
    require(type(objects) is dict and 1 <= len(objects) <= 4096, 'object count')
    for key, value in objects.items():
        require(type(key) is str and DIGEST.fullmatch(key) and identity(value) == key,
                'object digest mismatch')
    used = set()

    def get(ref):
        require(type(ref) is str and ref in objects, 'missing referenced object')
        used.add(ref)
        return objects[ref]

    schedule = get(bundle['schedule'])
    tool_only = type(schedule) is dict and schedule.get('schema') == 'af.agent-bench-tool-schedule/1'
    common_fields = 'schema protocol cases oracles source_base source_head model collector mode slots'
    fields(schedule, common_fields + (' configuration tools_base tools_head' if tool_only else ''))
    require(tool_only or schedule['schema'] == 'af.agent-bench-schedule/2', 'schedule version')
    require(schedule['mode'] in ('smoke', 'aa', 'exposed_ab'), 'unsupported mode')
    # These are retained package bytes/metadata, not claims of independent package admission.
    for key in ('protocol', 'cases', 'oracles', 'source_base', 'source_head', 'model', 'collector'):
        get(schedule[key])
    require(schedule['mode'] != 'aa' or schedule['source_base'] == schedule['source_head'],
            'A/A must bind the same source')
    if tool_only:
        require(schedule['mode'] == 'exposed_ab'
                and schedule['source_base'] == schedule['source_head'],
                'Tool-only A/B must bind the same source')
        get(schedule['configuration'])
        base_tools = ['af_list_paths', 'af_read_range', 'af_search']
        head_tools = base_tools + ['af_definition', 'af_references', 'af_implementation']
        for side, expected in (('base', base_tools), ('head', head_tools)):
            require(get(schedule['tools_' + side]) == expected,
                    'Tool-only A/B must bind the exact navigation catalogs')
    else:
        require(schedule['mode'] != 'exposed_ab' or schedule['source_base'] != schedule['source_head'],
                'A/B must bind different sources')
    package = get(schedule['cases'])
    fields(package, 'schema cases')
    require(package['schema'] == 'af.agent-bench-cases/1', 'case package version')
    require(type(package['cases']) is list and 1 <= len(package['cases']) <= MAX_SLOTS,
            'case package count')
    cases = {}
    for case in package['cases']:
        fields(case, 'id family prompt')
        name(case['id'])
        name(case['family'])
        require(type(case['prompt']) is str and 1 <= len(case['prompt']) <= 65536,
                'case prompt bound')
        require(case['id'] not in cases, 'duplicate case')
        cases[case['id']] = case
    slots = schedule['slots']
    require(type(slots) is list and 2 <= len(slots) <= MAX_SLOTS, 'slot count')
    if schedule['mode'] == 'smoke':
        require(len(slots) <= 4, 'smoke exceeds four slots')
    else:
        require(slots == frozen_slots(cases), 'AA/AB frozen schedule mismatch')
    pairs, slot_ids = {}, set()
    for slot in slots:
        fields(slot, 'id pair family side case repetition')
        for key in ('id', 'pair', 'family'):
            name(slot[key])
        require(type(slot['case']) is str and slot['case'] in cases
                and cases[slot['case']]['family'] == slot['family'],
                'slot case missing or family mismatch')
        require(integer(slot['repetition']) >= 1, 'invalid repetition')
        require(slot['id'] not in slot_ids, 'duplicate slot')
        slot_ids.add(slot['id'])
        require(slot['side'] in ('base', 'head'), 'invalid side')
        pair = pairs.setdefault(slot['pair'], {})
        require(slot['side'] not in pair, 'duplicate pair side')
        pair[slot['side']] = slot
    case_repetitions = set()
    for pair in pairs.values():
        require(set(pair) == {'base', 'head'}, 'unpaired schedule')
        require(all(pair['base'][key] == pair['head'][key]
                    for key in ('family', 'case', 'repetition')), 'pair case/repetition mismatch')
        case_repetition = (pair['base']['case'], pair['base']['repetition'])
        require(case_repetition not in case_repetitions, 'duplicate case/repetition pair')
        case_repetitions.add(case_repetition)

    def indexed(refs, kind):
        require(type(refs) is list and len(refs) <= MAX_SLOTS, kind + ' count')
        result = {}
        for ref in refs:
            record = get(ref)
            require(type(record) is dict and type(record.get('slot')) is str,
                    'missing slot')
            slot = record['slot']
            require(slot in slot_ids and slot not in result, 'foreign or duplicate ' + kind)
            require(record.get('schedule') == bundle['schedule'], 'foreign schedule')
            result[slot] = record
        return result

    receipts = indexed(bundle['receipts'], 'receipt')
    grades = indexed(bundle['grades'], 'grade')
    require(set(receipts) == slot_ids, 'missing terminal receipt')
    totals = {side: dict(slots=0, correct=0, known_native_tokens=0, native_tokens=0,
                         wall_ns=0, complete=True, failures=0, known_chargeable=0,
                         chargeable=0, charge_complete=True) for side in ('base', 'head')}
    limits = set()
    per_slot = {}
    seen_calls = set()
    for slot in slots:
        receipt = receipts[slot['id']]
        fields(receipt, 'schema schedule slot status wall_ns timing_complete invocations answer trace_complete')
        require(receipt['schema'] == 'af.agent-bench-receipt/1', 'receipt version')
        require(receipt['status'] in ('ok', 'error', 'timeout', 'cancelled', 'lost', 'not_started'),
                'unknown terminal status')
        require(type(receipt['timing_complete']) is bool and type(receipt['trace_complete']) is bool,
                'invalid completeness')
        elapsed = receipt['wall_ns']
        if elapsed is not None:
            integer(elapsed)
        require(not receipt['timing_complete'] or elapsed is not None, 'complete timing missing')
        if not receipt['timing_complete']:
            limits.add('monotonic_timing_incomplete')
        if not receipt['trace_complete']:
            limits.add('navigation_trace_incomplete')
        calls = receipt['invocations']
        require(type(calls) is list and len(calls) <= 128, 'invocation count')
        if receipt['status'] == 'not_started':
            require(not calls and receipt['answer'] is None and elapsed is None
                    and not receipt['timing_complete'] and not receipt['trace_complete'],
                    'not_started receipt contains activity')
        known, complete = 0, bool(calls)
        charge, charge_complete = 0, bool(calls)
        for call in calls:
            fields(call, 'id input output cached_input reasoning_output chargeable usage_complete')
            name(call['id'])
            require(call['id'] not in seen_calls, 'duplicate invocation across schedule')
            seen_calls.add(call['id'])
            require(type(call['usage_complete']) is bool, 'invalid usage completeness')
            for counter in ('input', 'output', 'cached_input', 'reasoning_output', 'chargeable'):
                if call[counter] is not None:
                    integer(call[counter])
            for subset, parent in (('cached_input', 'input'), ('reasoning_output', 'output')):
                require(call[subset] is None or (call[parent] is not None and call[subset] <= call[parent]),
                        'subset exceeds or lacks parent')
            present = call['input'] is not None and call['output'] is not None
            require(not call['usage_complete'] or present, 'complete usage missing counters')
            if call['chargeable'] is not None:
                require(present and call['cached_input'] is not None,
                        'charge lacks reconcilable native counters')
                require(call['chargeable'] == call['input'] - call['cached_input'] + call['output'],
                        'AF charge differs from native input minus cache plus output')
            complete = complete and call['usage_complete'] and present
            known += sum(call[key] for key in ('input', 'output') if call[key] is not None)
            charge += call['chargeable'] or 0
            charge_complete = charge_complete and call['chargeable'] is not None
        # Unknown-first/known-later retries never turn into complete usage.
        if not complete:
            limits.add('native_usage_incomplete')
        if not charge_complete:
            limits.add('af_charge_incomplete')
        answer = receipt['answer']
        if answer is not None:
            get(answer)
        require(receipt['status'] != 'ok' or (answer is not None and calls), 'successful receipt lacks answer/call')
        grade = grades.get(slot['id'])
        correct = False
        if grade is not None:
            fields(grade, 'schema schedule slot answer oracle verdict evidence')
            require(grade['schema'] == 'af.agent-bench-grade/1', 'grade version')
            require(grade['answer'] == answer and answer is not None, 'grade answer mismatch')
            require(grade['oracle'] == schedule['oracles'], 'grade oracle mismatch')
            get(grade['evidence'])
            require(grade['verdict'] in ('correct', 'wrong', 'ambiguous'), 'grade verdict')
            correct = grade['verdict'] == 'correct' and receipt['status'] == 'ok'
        if grade is None or grade['verdict'] == 'ambiguous':
            limits.add('semantic_grade_unresolved')
        resources = complete and receipt['timing_complete'] and elapsed is not None
        total = totals[slot['side']]
        total['slots'] += 1
        total['correct'] += int(correct)
        total['failures'] += int(receipt['status'] != 'ok')
        total['known_chargeable'] += charge
        total['chargeable'] += charge
        total['charge_complete'] = total['charge_complete'] and charge_complete
        total['known_native_tokens'] += known
        total['native_tokens'] += known
        total['wall_ns'] += elapsed or 0
        total['complete'] = total['complete'] and resources
        per_slot[slot['id']] = (correct, resources, known, elapsed)
    require(used == set(objects), 'unreferenced/foreign objects')
    deltas = []
    for pair_id, pair in sorted(pairs.items()):
        base, head = (per_slot[pair[side]['id']] for side in ('base', 'head'))
        eligible = base[1] and head[1]
        deltas.append(dict(pair=pair_id, family=pair['base']['family'],
                           case=pair['base']['case'], repetition=pair['base']['repetition'],
                           eligible=eligible, correct_eligible=eligible and base[0] and head[0],
                           native_tokens=head[2] - base[2] if eligible else None,
                           wall_ns=head[3] - base[3] if eligible else None))
    for total in totals.values():
        if not total['charge_complete']:
            total['chargeable'] = None
        if not total['complete']:
            total['native_tokens'] = total['wall_ns'] = None
    return dict(schema='af.agent-bench-analysis/2', schedule=bundle['schedule'],
                evidence=identity(bundle), mode=schedule['mode'], totals=totals, pairs=deltas,
                resources=weighted_resources(deltas, per_slot, pairs),
                limits=sorted(limits), qualification='structural_only', conclusion='no_improvement_claim')


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('evidence', type=Path)
    args = parser.parse_args()
    try:
        report = analyze(read(args.evidence))
        sys.stdout.buffer.write(canonical(report) + b'\n')
    except (ValueError, OSError, TypeError, RecursionError) as error:
        print('agent-bench evidence refused: ' + str(error), file=sys.stderr)
        return 1
    return 0


if __name__ == '__main__':
    raise SystemExit(main())
