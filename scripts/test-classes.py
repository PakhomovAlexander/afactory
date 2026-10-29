#!/usr/bin/env python3
"""Resolve, audit, run and report reviewable validation classes (stdlib only)."""
import argparse
import datetime as dt
from decimal import Decimal
import hashlib
import importlib.util
import json
import os
from pathlib import Path
import re
import shutil
import subprocess
import sys
import xml.etree.ElementTree as ET

sys.dont_write_bytecode = True
ROOT = Path(__file__).resolve().parent.parent
CATALOG = ROOT / '.config/test-classes.json'
QUARANTINE = ROOT / '.config/test-quarantine.json'
CLASSES = ('functional', 'e2e', 'uix', 'perf')
FAST = list(CLASSES[:3])
# A listed test classifies as this, and no plan selects it (ADR-0264).
QUARANTINED = 'quarantine'
QUARANTINE_LIMIT = 20
QUARANTINE_DAYS = 14
SCHEMA = 'af.test-plan/2'
BINDING = 'af.release-tests/2'
PERF_VARIANTS = (
    ('trace', 'review-core/perf-trace,review-core/perf-calibration',
     'binary_id(=review-core) & test(perf_)'),
    ('calibration', 'review-pipeline/perf-calibration',
     'binary_id(=review-pipeline::perf_probe) & test(calibration_build)'),
)
# What each feature variant's fixed selector reaches: (binary, test name substring). The
# Makefile's perf-feature-tests runs the same selectors through cargo test.
FEATURE_SELECTORS = {'trace': ('review-core', 'perf_'),
                     'calibration': ('review-pipeline::perf_probe', 'calibration_build')}
FILTER_OPTIONS = ('-E', '--filterset', '--filter-expr')


def read(path):
    return json.loads(Path(path).read_text())


def write(path, value):
    path = Path(path)
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(json.dumps(value, indent=2, sort_keys=True) + '\n')


def catalog():
    value = read(CATALOG)
    if set(value) != {'schema', 'binaries', 'modules', 'overrides', 'scripts', 'utilities',
                      'always_on_scripts', 'exact', 'ignored', 'hosted_checks'} or value['schema'] != 'af.test-catalog/1':
        raise ValueError('unsupported test catalog')
    return value


def quarantine_today():
    """Today's date for the expiry guard; AF_TEST_QUARANTINE_TODAY (YYYY-MM-DD) replaces the clock."""
    value = os.getenv('AF_TEST_QUARANTINE_TODAY')
    if value is None:
        return dt.date.today()
    try:
        if re.fullmatch(r'\d{4}-\d{2}-\d{2}', value):
            return dt.date.fromisoformat(value)
    except ValueError:
        pass
    raise ValueError(f'AF_TEST_QUARANTINE_TODAY={value!r} is not a YYYY-MM-DD date')


def quarantine(cat=None, today=None, value=None):
    """Validate the quarantine file and return its tests: identity -> entry.

    Every refusal names the entry and the fix. The inventory guard runs in partition."""
    cat = cat or catalog()
    today = today or quarantine_today()
    if value is None:
        value = read(QUARANTINE)
    where = '.config/test-quarantine.json'
    if (not isinstance(value, dict) or set(value) != {'schema', 'tests'}
            or value['schema'] != 'af.test-quarantine/1' or not isinstance(value['tests'], dict)):
        raise ValueError(f'{where}: unsupported test quarantine; fix: write '
                         '{"schema": "af.test-quarantine/1", "tests": {...}}')
    tests = value['tests']
    if len(tests) > QUARANTINE_LIMIT:
        raise ValueError(f'{where}: {len(tests)} entries, at most {QUARANTINE_LIMIT}: {sorted(tests)}; '
                         'fix: fix and remove a quarantined test before adding another')

    def date(identity, entry, field):
        text = entry.get(field)
        try:
            if isinstance(text, str) and re.fullmatch(r'\d{4}-\d{2}-\d{2}', text):
                return dt.date.fromisoformat(text)
        except ValueError:
            pass
        raise ValueError(f'{where}: {identity}: `{field}` {text!r} is not a YYYY-MM-DD date; '
                         'fix: write an ISO date')

    for identity, entry in tests.items():
        def refuse(problem, fix):
            raise ValueError(f'{where}: {identity}: {problem}; fix: {fix}')
        if not re.fullmatch(r'[A-Za-z0-9_:./-]+ [A-Za-z0-9_:]+', identity):
            refuse('not a `<binary_id> <test name>` nextest identity',
                   'name the test the way .config/test-classes.json `exact` does')
        binary, name = identity.split(' ')
        for label, (selected, pattern) in FEATURE_SELECTORS.items():
            if binary == selected and pattern in name:
                refuse(f'the {label} feature build\'s fixed selector `{selected}` `{pattern}` runs it, and '
                       'quarantine covers the default-feature inventory only',
                       'fix the test, or exclude it from its feature selector by hand')
        if not isinstance(entry, dict):
            refuse('entry is not an object', 'write state, since, expires, evidence and optional issue')
        unknown = sorted(set(entry) - {'state', 'since', 'expires', 'evidence', 'issue'})
        if unknown:
            refuse(f'unknown field {unknown}', 'keep only state, since, expires, evidence and issue')
        missing = sorted({'state', 'since', 'expires', 'evidence'} - set(entry))
        if missing:
            refuse(f'missing field {missing}', 'write state, since, expires and evidence')
        if entry['state'] not in ('quarantined', 'flaky'):
            refuse(f'state {entry["state"]!r} is neither quarantined nor flaky',
                   'write "quarantined" or "flaky"')
        since, expires = date(identity, entry, 'since'), date(identity, entry, 'expires')
        if not since < expires <= since + dt.timedelta(days=QUARANTINE_DAYS):
            refuse(f'expires {expires} is not after since {since} and at most {QUARANTINE_DAYS} days after it',
                   f'set expires within {QUARANTINE_DAYS} days after since')
        evidence = entry['evidence']
        if (not isinstance(evidence, list) or not evidence
                or not all(isinstance(t, str) and re.fullmatch(r'[A-Za-z0-9][A-Za-z0-9_-]{0,127}', t)
                           for t in evidence)):
            refuse('evidence is not a non-empty list of af Task IDs',
                   'list the Tasks whose structured check reports show the test flaky')
        if 'issue' in entry and (type(entry['issue']) is not int or entry['issue'] < 1):
            refuse(f'issue {entry["issue"]!r} is not a positive integer', 'write the GitHub issue number')
        if expires < today:
            refuse(f'expired on {expires} (today is {today})',
                   'fix the test and remove the entry, or confirm it again and renew since and expires')
        if identity in cat['ignored']:
            refuse('an #[ignore] test cannot be quarantined', 'remove the entry')
    return tests


def quarantine_exclusion(held):
    """The nextest filter that leaves every quarantined identity out, or None when none is held."""
    if not held:
        return None
    clauses = [f'(binary_id(={binary}) & test(={name}))'
               for binary, name in sorted(identity.split(' ') for identity in held)]
    return 'not (' + ' | '.join(clauses) + ')'


def gate_arguments(arguments, held):
    """nextest-gate.py's arguments for the full gate's unplanned run (`make test`): the caller's,
    plus the quarantine exclusion when an entry is held, so an empty file changes nothing."""
    if any(a in FILTER_OPTIONS or a.startswith(tuple(o + '=' for o in FILTER_OPTIONS)) for a in arguments):
        raise ValueError('nextest-gate: the quarantine exclusion is this run\'s only filter; fix: run '
                         'scripts/nextest-gate.py with an explicit filter instead (make test-repeat)')
    exclusion = quarantine_exclusion(held)
    return [*arguments, '-E', exclusion] if exclusion else list(arguments)


def schema_documents():
    """Infrastructure contracts; generated JSON Schema parity is checked even on fast plans."""
    def obj(properties, required=None):
        return {'type': 'object', 'additionalProperties': False,
                'properties': properties, 'required': list(properties) if required is None else required}

    def mapping(value):
        return {'type': 'object', 'additionalProperties': value}

    text = {'type': 'string'}
    nullable = {'type': ['string', 'null']}
    kind = {'enum': list(CLASSES)}
    names = {'type': 'array', 'items': text, 'uniqueItems': True}
    primary = mapping(kind)
    cat = obj({'schema': {'const': 'af.test-catalog/1'}, 'binaries': primary,
               'modules': mapping(primary), 'overrides': mapping(primary), 'scripts': primary,
               'utilities': names, 'always_on_scripts': names, 'exact': primary, 'hosted_checks': primary,
               'ignored': mapping({'enum': ['live-container', 'namespace-child',
                                           'manual-measurement', 'subprocess-helper']})})
    plan = obj({'schema': {'const': SCHEMA}, 'profile': {'enum': ['full', 'fast', 'custom']},
                'context': {'enum': ['pr', 'gate', 'release_pr', 'local', 'release']}, 'sha': nullable, 'version': nullable,
                'labels': names, 'selected': {'type': 'array', 'items': kind,
                                             'minItems': 1, 'uniqueItems': True},
                'catalog_sha256': {'type': 'string', 'pattern': '^[a-f0-9]{64}$'},
                'classes': obj({c: obj({'selected': {'type': 'boolean'}, 'reason': text,
                                       'state': {'enum': ['SELECTED', 'NOT RUN']}}) for c in CLASSES})})
    binding = obj({'schema': {'const': BINDING}, 'sha': text, 'version': text,
                   'source_pr': {'anyOf': [{'type': 'null'}, obj({'repository': text,
                       'number': {'type': 'integer', 'minimum': 1}, 'merge_commit_sha': text,
                       'base': text, 'head': text, 'merged_at': text})]}, 'plan': plan})
    receipt = obj({'schema': {'const': 'af.test-class-receipt/2'}, 'plan': plan,
                   'platform': text, 'class': kind, 'status': {'const': 'success'}})
    day = {'type': 'string', 'pattern': r'^\d{4}-\d{2}-\d{2}$'}
    held = obj({'state': {'enum': ['quarantined', 'flaky']}, 'since': day, 'expires': day,
                'evidence': {'type': 'array', 'minItems': 1, 'items': {
                    'type': 'string', 'pattern': '^[A-Za-z0-9][A-Za-z0-9_-]{0,127}$'}},
                'issue': {'type': 'integer', 'minimum': 1}}, ['state', 'since', 'expires', 'evidence'])
    quarantined = obj({'schema': {'const': 'af.test-quarantine/1'},
                       'tests': {'type': 'object', 'maxProperties': QUARANTINE_LIMIT,
                                 'propertyNames': {'pattern': '^[A-Za-z0-9_:./-]+ [A-Za-z0-9_:]+$'},
                                 'additionalProperties': held}})
    return {f'{name}-v{version}.json': {'$schema': 'https://json-schema.org/draft/2020-12/schema',
                                      '$id': f'urn:af:schema:{name}:{version}', **document}
            for name, version, document in [('test-class-catalog', 1, cat), ('test-class-plan', 2, plan),
                                            ('release-test-binding', 2, binding),
                                            ('test-class-receipt', 2, receipt),
                                            ('test-quarantine', 1, quarantined)]}


def schema_parity(rewrite=False):
    for name, document in schema_documents().items():
        path = ROOT / 'schemas' / name
        rendered = json.dumps(document, indent=2, sort_keys=True) + '\n'
        if rewrite:
            path.write_text(rendered)
        elif path.read_text() != rendered:
            raise ValueError(f'{path}: schema parity drift')


def resolve(labels=(), context='local', sha=None, version=None):
    """Positive labels replace defaults; negatives subtract. Contradictions never win."""
    if context not in ('pr', 'gate', 'release_pr', 'local', 'release'):
        raise ValueError('unknown selection context')
    if not isinstance(labels, (list, tuple)) or not all(isinstance(x, str) for x in labels):
        raise ValueError('labels must be strings')
    if any(value is not None and not isinstance(value, str) for value in (sha, version)):
        raise ValueError('SHA/version must be strings or null')
    labels = sorted(set(labels))
    known = {f'test:{c}' for c in CLASSES} | {f'no-{c}' for c in CLASSES}
    known |= {'release:fast', 'release:full'}
    for label in labels:
        if label.startswith(('test:', 'no-', 'release:')) and label not in known:
            raise ValueError(f'unknown reserved class label: {label!r}')
    positive = {c for c in CLASSES if f'test:{c}' in labels}
    negative = {c for c in CLASSES if f'no-{c}' in labels}
    if positive & negative:
        raise ValueError('contradictory class labels')
    fast, full = 'release:fast' in labels, 'release:full' in labels
    if fast and full:
        raise ValueError('contradictory release profiles')
    if full and (negative or (positive and positive != set(CLASSES))):
        raise ValueError('release:full refuses exclusions or a partial allowlist')
    defaults = FAST if context in ('pr', 'gate', 'release_pr') else list(CLASSES)
    if fast:
        defaults = FAST
    if full:
        selected, profile = list(CLASSES), 'full'
    else:
        selected = [c for c in CLASSES if c in (positive or set(defaults)) - negative]
        profile = 'full' if selected == list(CLASSES) else 'fast' if selected == FAST else 'custom'
    if not selected:
        raise ValueError('empty test selection')
    return {'schema': SCHEMA, 'profile': profile, 'context': context, 'sha': sha,
            'version': version, 'labels': labels, 'selected': selected,
            'catalog_sha256': hashlib.sha256(CATALOG.read_bytes()).hexdigest(),
            'classes': {c: {'selected': c in selected,
                            'reason': 'release:full' if full else
                            'explicit exclusion' if c in negative else
                            'explicit allowlist' if positive and c in positive else
                            'outside explicit allowlist' if positive else
                            'profile default' if c in selected else 'outside profile default',
                            'state': 'SELECTED' if c in selected else 'NOT RUN'} for c in CLASSES}}


def validate_plan(plan):
    if plan.get('schema') != SCHEMA:
        raise ValueError('unsupported test plan')
    expected = resolve(plan['labels'], plan['context'], plan['sha'], plan['version'])
    if plan != expected:
        raise ValueError('test plan differs from resolver/catalog')
    return plan


def event_labels(event):
    return [item['name'] for item in event['pull_request']['labels']]


def event_context(event):
    pr = event['pull_request']
    if pr.get('base', {}).get('ref', '').startswith('af-gate/'):
        return 'gate'
    return 'release_pr' if pr.get('head', {}).get('ref', '').startswith('release/v') else 'pr'


def check_release_pr(event):
    branch = event['pull_request'].get('head', {}).get('ref', '')
    if not branch.startswith('release/v'):
        return
    version = branch[len('release/v'):]
    manifest = re.search(r'^version = "([^"]+)"$', (ROOT / 'Cargo.toml').read_text(), re.M)
    if manifest is None or manifest.group(1) != version:
        raise ValueError('release PR branch/version mismatch')


def release_plan(annotation, sha, version):
    # A lightweight/manual tag or unrelated annotation carries no fast authority.
    if 'af.release-tests/' not in annotation:
        return resolve([], 'release', sha, version)
    try:
        binding = json.loads(annotation.strip())
    except json.JSONDecodeError as error:
        raise ValueError('malformed release selection annotation') from error
    if set(binding) != {'schema', 'sha', 'version', 'source_pr', 'plan'} or binding['schema'] != BINDING:
        raise ValueError('invalid release selection annotation')
    if binding['sha'] != sha or binding['version'] != version:
        raise ValueError('release selection SHA/version mismatch')
    validate_source_pr(binding['source_pr'], sha, version)
    plan = validate_plan(binding['plan'])
    context = 'release_pr' if binding['source_pr'] is not None else 'release'
    if plan['context'] != context or plan['sha'] != sha or plan['version'] != version:
        raise ValueError('release plan binding mismatch')
    if binding['source_pr'] is None and plan != resolve([], 'release', sha, version):
        raise ValueError('release labels require exact merged PR provenance')
    return plan


def validate_source_pr(source, sha, version):
    if source is None:
        return
    fields = {'repository', 'number', 'merge_commit_sha', 'base', 'head', 'merged_at'}
    if not isinstance(source, dict) or set(source) != fields:
        raise ValueError('invalid release PR provenance')
    if (not isinstance(source['repository'], str)
            or not re.fullmatch(r'[A-Za-z0-9_.-]+/[A-Za-z0-9_.-]+', source['repository'])
            or type(source['number']) is not int or source['number'] < 1
            or source['merge_commit_sha'] != sha or source['base'] != 'main'
            or source['head'] != 'release/v' + version
            or not isinstance(source['merged_at'], str) or not source['merged_at']):
        raise ValueError('release PR provenance does not match SHA/version/main')


def classify(binary, name, cat=None, held=None):
    """One default per registered binary; optional explicit top-module overrides.

    A quarantined identity (`held`, the quarantine file's tests) wins over every catalog rule."""
    cat = cat or catalog()
    held = quarantine(cat) if held is None else held
    if binary + ' ' + name in held:
        return QUARANTINED
    if binary + ' ' + name in cat['exact']:
        return cat['exact'][binary + ' ' + name]
    if binary in cat['modules']:
        module = name.split('::')[0]
        try:
            return cat['modules'][binary][module]
        except KeyError:
            raise ValueError(f'unclassified module {binary}: {module}') from None
    if binary not in cat['binaries']:
        raise ValueError(f'unclassified test binary {binary}')
    overrides = cat['overrides'].get(binary, {})
    matches = [c for prefix, c in overrides.items() if name.startswith(prefix)]
    if len(matches) > 1:
        raise ValueError(f'overlapping catalog overrides: {binary}: {name}')
    return matches[0] if matches else cat['binaries'][binary]


def partition(inventory, cat=None, held=None):
    cat = cat or catalog()
    held = quarantine(cat) if held is None else held
    entries = []
    for binary, suite in inventory['rust-suites'].items():
        if binary not in cat['binaries'] and binary not in cat['modules']:
            raise ValueError(f'unclassified test binary {binary}, even if it has no tests yet')
        if suite.get('status') == 'skipped':
            raise ValueError(f'inventory unexpectedly skipped binary {binary}')
        for name, test in suite['testcases'].items():
            if test['ignored'] and binary + ' ' + name in held:
                raise ValueError(f'.config/test-quarantine.json: {binary} {name}: an #[ignore] test '
                                 'cannot be quarantined; fix: remove the entry')
            ignored = cat['ignored'].get(binary + ' ' + name)
            if test['ignored'] != (ignored is not None):
                raise ValueError(f'ignored-test policy drift: {binary}: {name}')
            kind = classify(binary, name, cat, held)
            if kind not in (*CLASSES, QUARANTINED):
                raise ValueError(f'invalid class {kind}')
            entries.append({'binary': binary, 'test': name, 'class': kind,
                            'ignored': test['ignored'], 'ignored_policy': ignored})
    if not entries:
        raise ValueError('empty test inventory')
    identities = {(e['binary'], e['test']) for e in entries}
    if len(identities) != len(entries):
        raise ValueError('duplicate test identities')
    absent = sorted(set(held) - {b + ' ' + n for b, n in identities})
    if absent:
        raise ValueError(f'.config/test-quarantine.json: {absent[0]}: not in the test inventory; '
                         'fix: correct the identity or remove the entry')
    return entries


def filterset(entries, selected):
    """Generate exact, disjoint filters from the audited inventory; no name heuristics."""
    grouped = {}
    for entry in entries:
        grouped.setdefault(entry['binary'], []).append(entry)
    clauses = []
    for binary, tests in sorted(grouped.items()):
        names = [e['test'] for e in tests if e['class'] in selected]
        if not names:
            continue
        # Rust names are safe nextest literals; refuse expansion to a filter expression.
        if not re.fullmatch(r'[A-Za-z0-9_:./-]+', binary):
            raise ValueError('unsupported binary identity')
        for name in names:
            if not re.fullmatch(r'[A-Za-z0-9_:]+', name):
                raise ValueError(f'unsupported test identity {name!r}')
        roots = {}
        for entry in tests:
            roots.setdefault(entry['test'].split('::')[0], []).append(entry)
        patterns = []
        for root, members in sorted(roots.items()):
            chosen = [e['test'] for e in members if e['class'] in selected]
            if chosen and len(chosen) == len(members) and all('::' in n for n in chosen):
                patterns.append(re.escape(root) + '::')
            else:
                patterns.extend(re.escape(n) + '$' for n in chosen)
        pattern = '|'.join(patterns)
        clauses.append(f'(binary_id(={binary}) & test(/^({pattern})/))')
    return ' | '.join(clauses) or 'none()'


def audit_source(cat=None):
    """Catch new integration modules, binaries and script checks even on a fast plan."""
    cat = cat or catalog()
    schema_parity()
    for path in (ROOT / 'crates').glob('*/tests/it/main.rs'):
        binary = path.parts[-4] + '::it'
        # nextest's lib binary is the package name, and test binaries append ::name.
        modules = set(re.findall(r'^mod (\w+);', path.read_text(), re.M))
        declared = set(cat['modules'].get(binary, {}))
        # Fixture-only modules are catalogued too, so adding tests to them cannot hide them.
        if modules != declared:
            raise ValueError(f'{path.relative_to(ROOT)} module catalog drift: '
                             f'new={sorted(modules - declared)}, removed={sorted(declared - modules)}')
    actual_scripts = {p.name for p in (ROOT / 'scripts').glob('test-*.py')}
    declared_scripts = set(cat['scripts']) | set(cat['utilities'])
    if actual_scripts != declared_scripts:
        raise ValueError(f'script catalog drift: {actual_scripts ^ declared_scripts}')
    if set(cat['scripts']) & set(cat['utilities']) or not set(cat['always_on_scripts']) <= set(cat['scripts']):
        raise ValueError('script ownership overlaps or always-on script is unclassified')
    workflow = (ROOT / '.github/workflows/validate.yml').read_text()
    jobs = set(re.findall(r'^  ([A-Za-z0-9_-]+):$', workflow.split('\njobs:\n', 1)[1], re.M))
    if jobs != {'resolve', 'base', 'classes', 'lint', 'check'} | set(cat['hosted_checks']):
        raise ValueError('hosted validation job catalog drift')
    for mapping in [cat['binaries'], cat['scripts'], cat['exact'], cat['hosted_checks'], *cat['modules'].values(),
                    *cat['overrides'].values()]:
        if set(mapping.values()) - set(CLASSES):
            raise ValueError('unknown class in catalog')
    # The file's guards; each held identity must also resolve to a catalogued class.
    for identity in quarantine(cat):
        try:
            classify(*identity.split(' '), cat, {})
        except ValueError as error:
            raise ValueError(f'.config/test-quarantine.json: {identity}: {error}; '
                             'fix: correct the identity or remove the entry') from None


def aggregate(plan, needs):
    """Missing/skipped/cancelled selected output is failure, never an empty green gate."""
    validate_plan(plan)
    for job in ('resolve', 'base', 'classes'):
        if needs.get(job, {}).get('result') != 'success':
            raise ValueError(f'{job} did not succeed')
    for job, kind in catalog()['hosted_checks'].items():
        expected = 'success' if kind in plan['selected'] else 'skipped'
        if needs.get(job, {}).get('result') != expected:
            raise ValueError(f'{job} must be {expected} for this selection')
    output = needs['resolve'].get('outputs', {}).get('plan')
    if not output or json.loads(output) != plan:
        raise ValueError('resolver output missing or differs')


def receipts(plan, directory, platforms):
    expected = {(platform, kind) for platform in platforms for kind in plan['selected']}
    observed = set()
    for path in Path(directory).rglob('receipt.json'):
        value = read(path)
        key = value['platform'], value['class']
        if set(value) != {'schema', 'plan', 'platform', 'class', 'status'} or value['schema'] != 'af.test-class-receipt/2':
            raise ValueError('unknown receipt shape')
        if key not in expected or key in observed or value['plan'] != plan or value['status'] != 'success':
            raise ValueError(f'invalid/duplicate class receipt: {path}')
        observed.add(key)
    if observed != expected:
        raise ValueError(f'missing class receipts: {sorted(expected - observed)}')


def aggregate_metrics(plan, directory, platforms):
    """Combine disjoint class jobs; base/container/perf-smoke command intervals remain separate."""
    result = {'schema': 'af.test-validation-metrics/1', 'sha': plan['sha'], 'platforms': {},
              'note': 'Observed command envelope excludes resolver, runner setup and final aggregation. '
                      'Runner seconds sum outer class commands only; base/container/perf-smoke costs are separate. '
                      'These are not billed minutes or wall savings.'}
    for platform in platforms:
        classes, class_intervals, separate, inventory_counts = {}, [], {}, {}
        for kind in CLASSES:
            if kind not in plan['selected']:
                classes[kind] = {'state': 'NOT RUN', 'reason': plan['classes'][kind]['reason']}
                continue
            roots = [p.parent for p in Path(directory).rglob('receipt.json')
                     if read(p)['platform'] == platform and read(p)['class'] == kind]
            if len(roots) != 1:
                raise ValueError('metrics require exactly one selected class receipt')
            root = roots[0]
            report_path = root / 'report.json'
            metrics_path = root.parent / 'class-metrics.jsonl'
            records = [json.loads(line) for line in metrics_path.read_text().splitlines()]
            outer = [r for r in records if r['label'] == 'class-' + kind]
            if len(outer) != 1 or outer[0]['exit_code'] != 0:
                raise ValueError(f'missing/failed outer class metric: {kind}')
            seconds = duration(outer[0]['elapsed_seconds'])
            if seconds is None:
                raise ValueError(f'missing outer class duration: {kind}')
            report_value = read(report_path)
            if report_value['plan'] != plan:
                raise ValueError('metric report belongs to another plan')
            inventory_counts.update({c: r.get('inventory_count') for c, r in report_value['classes'].items()})
            row = report_value['classes'][kind]
            classes[kind] = dict(row, state='RAN', command_seconds=seconds)
            class_intervals.append(dict(outer[0], elapsed_seconds=seconds))
        intervals = list(class_intervals)
        for name in ('base', 'container'):
            paths = list(Path(directory).glob(f'validation-{name}-{platform}-*/base-metrics.jsonl')) if name == 'base' else (
                list(Path(directory).glob('validation-container-*/container-metrics.jsonl'))
                if platform == 'Linux' and 'e2e' in plan['selected'] else [])
            records = [json.loads(line) for path in paths for line in path.read_text().splitlines()]
            separate[name + '_command_seconds'] = sum(r['elapsed_seconds'] for r in records) if records else None
            intervals.extend(records)
        smoke_paths = (list(Path(directory).glob('validation-telemetry-smoke-*/telemetry-smoke-metrics.jsonl'))
                       + list(Path(directory).glob('validation-telemetry-runtime-smoke-*/telemetry-runtime-smoke-metrics.jsonl'))
                       if platform == 'Linux' and 'perf' in plan['selected'] else [])
        smoke_records = [json.loads(line) for path in smoke_paths for line in path.read_text().splitlines()]
        separate['perf_smoke'] = {
            'state': ('RAN' if smoke_records else 'NO EVIDENCE') if 'perf' in plan['selected'] and platform == 'Linux'
                     else 'NOT RUN',
            'command_seconds': sum(r['elapsed_seconds'] for r in smoke_records) if smoke_records else None}
        intervals.extend(smoke_records)
        starts = [dt.datetime.fromisoformat(r['started_at']) for r in intervals]
        ends = [start + dt.timedelta(seconds=r['elapsed_seconds']) for start, r in zip(starts, intervals)]
        runner = sum(r['elapsed_seconds'] for r in class_intervals)
        for kind, row in classes.items():
            if row['state'] == 'RAN':
                row['runner_cost_share'] = row['command_seconds'] / runner if runner else None
            else:
                row['not_run'] = inventory_counts.get(kind)
        base_junit = list(Path(directory).glob(f'validation-base-{platform}-*/base-evidence/*.xml'))
        base_counts = report(plan, base_junit)['classes']['functional'] if base_junit else None
        if base_counts:
            base_counts.update(selected=True, state='ALWAYS ON (functional)')
        result['platforms'][platform] = {
            'classes': classes, **separate, 'always_on_functional_test_counts': base_counts,
            'observed_runner_command_seconds': runner,
            'parallel_class_critical_path_seconds': max(row['command_seconds'] for row in classes.values()
                                                       if row['state'] == 'RAN'),
            'last_finishing_command': intervals[ends.index(max(ends))]['label'],
            'observed_command_envelope_seconds': (max(ends) - min(starts)).total_seconds()}
    return result


def aggregate_summary(value):
    lines = ['### Observed validation costs', '', value['note'], '']
    for platform, data in value['platforms'].items():
        lines += [f'**{platform}**: command envelope {data["observed_command_envelope_seconds"]:.3f}s; '
                  f'parallel class critical path {data["parallel_class_critical_path_seconds"]:.3f}s; '
                  f'class runner command cost {data["observed_runner_command_seconds"]:.3f}s.', '',
                  f'Base commands: {data["base_command_seconds"]}; '
                  f'container commands: {data["container_command_seconds"]}.',
                  f'Perf smoke: {data["perf_smoke"]["state"]}; command seconds: '
                  f'{data["perf_smoke"]["command_seconds"]}.', '',
                  '| Class | State | Executed | Passed | Failed | Skipped | Not run | Test seconds | Command seconds | Class runner share |',
                  '| --- | --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: |']
        for kind, row in data['classes'].items():
            values = [kind, row['state']] + [str(row[k]) if row.get(k) is not None else 'unknown' for k in
                      ('executed', 'passed', 'failed', 'skipped', 'not_run', 'test_seconds', 'command_seconds', 'runner_cost_share')]
            lines.append('| ' + ' | '.join(values) + ' |')
        lines.append('')
    return '\n'.join(lines) + '\n'


def duration(value):
    if value is None:
        return None
    value = Decimal(str(value))
    if not value.is_finite() or value < 0:
        raise ValueError('invalid duration')
    return float(value)


def report(plan, junit_paths, step_paths=(), inventory=None, provenance=None, replay=False):
    validate_plan(plan)
    cat = catalog()
    held = quarantine(cat)
    rows = {c: {'selected': c in plan['selected'], 'state': 'NO EVIDENCE' if c in plan['selected'] else 'NOT RUN', 'count': 0,
                'executed': 0, 'passed': 0, 'failed': 0, 'skipped': 0, 'not_run': None,
                'test_seconds': None, 'step_seconds': None} for c in CLASSES}
    seen, variants, missing, walls, steps, timings = set(), set(), [], [], [], []
    spec = importlib.util.spec_from_file_location('class_test_times', ROOT / 'scripts/test-time-report.py')
    reader = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(reader)
    patterns = reader.exclusive_patterns(ROOT / '.config/nextest.toml', True)
    for path in junit_paths:
        root = ET.parse(path).getroot()
        if root.tag != 'testsuites':
            raise ValueError(f'{path}: expected testsuites JUnit root')
        cases = list(root.iter('testcase'))
        for attribute, count in [('tests', len(cases)),
                                 ('failures', sum(c.find('failure') is not None for c in cases)),
                                 ('errors', sum(c.find('error') is not None for c in cases)),
                                 ('skipped', sum(c.find('skipped') is not None for c in cases))]:
            if root.get(attribute) is not None and root.get(attribute) != str(count):
                raise ValueError(f'{path}: JUnit {attribute} count mismatch')
        walls.append({'path': str(path), 'wall_seconds': duration(root.get('time'))})
        for suite in root.iter('testsuite'):
            for case in suite.findall('testcase'):
                binary = case.get('classname') or suite.get('name')
                name = case.get('name')
                key = binary, name
                variant = Path(path).stem if Path(path).stem.startswith('feature-') else 'default'
                if (*key, variant) in variants:
                    raise ValueError(f'duplicate JUnit identity {key}; supply one source run')
                variants.add((*key, variant))
                seen.add(key)
                if binary in cat['scripts']:
                    kind = cat['scripts'][binary]
                else:
                    kind = classify(binary, name, cat, held)
                if kind == QUARANTINED:
                    continue  # Excluded from every class row and timing estimate.
                row = rows[kind]
                row['count'] += 1
                skip = case.find('skipped') is not None
                failed = case.find('failure') is not None or case.find('error') is not None
                row['skipped'] += skip
                row['executed'] += not skip
                row['failed'] += failed and not skip
                row['passed'] += not failed and not skip
                seconds = duration(case.get('time'))
                if seconds is None:
                    missing.append(f'{binary}::{name}: missing time')
                else:
                    row['test_seconds'] = (row['test_seconds'] or 0) + seconds
                timings.append((kind, seconds, skip, any(p.search(name) for p in patterns),
                                binary not in cat['scripts'] and variant == 'default'))
                row['state'] = 'OBSERVED' if row['selected'] else 'NOT RUN (historical evidence only)'
    for kind, row in rows.items():
        if any(c == kind and seconds is None and not skip for c, seconds, skip, _, _eligible in timings):
            row['test_seconds'] = None
    for path in step_paths:
        for line in Path(path).read_text().splitlines():
            step = json.loads(line)
            if step['schema'] != 'af.ci-step/1':
                raise ValueError('unknown ci-step schema')
            steps.append(step)
    for step in steps:
        # Class runner emits a single outer interval; inner steps are retained separately.
        if step['label'] in {'class-' + c for c in CLASSES}:
            kind = step['label'][6:]
            rows[kind]['step_seconds'] = duration(step['elapsed_seconds'])
    if inventory is not None:
        for kind, row in rows.items():
            expected = {(e['binary'], e['test']) for e in inventory if e['class'] == kind}
            row['inventory_count'] = len(expected)
            row['not_run'] = len(expected - seen) if row['selected'] else len(expected)
            if row['selected']:
                missing += [f'{b}::{n}: no JUnit record' for b, n in sorted(expected - seen)]
    else:
        missing.append('no inventory supplied; omitted tests and not-run counts unknown')
    measured = [r['step_seconds'] for r in rows.values() if r['selected'] and r['step_seconds'] is not None]
    for row in rows.values():
        if row['test_seconds'] is not None:
            row['test_seconds'] = round(row['test_seconds'], 6)
    selected_count = len(plan['selected'])
    estimates = {}
    for profile, selected in [('functional+e2e', ['functional', 'e2e']), ('fast', FAST),
                              ('full', list(CLASSES)), ('perf-only', ['perf'])]:
        jobs = {}
        for kind in selected:
            cases = [(s, exclusive) for c, s, skip, exclusive, eligible in timings
                     if c == kind and not skip and eligible]
            if not cases or any(s is None for s, _ in cases):
                jobs[kind] = None
            else:
                exclusive = sum(s for s, e in cases if e)
                parallel = [s for s, e in cases if not e]
                jobs[kind] = exclusive + max(sum(parallel) / 4, max(parallel, default=0))
        estimates[profile] = {'classes': selected, 'test_only_job_seconds_lower_bound': jobs,
                              'parallel_test_wall_lower_bound': max(jobs.values()) if all(
                                  x is not None for x in jobs.values()) else None,
                              'assumed_threads': 4,
                              'scope': 'estimate from observed cases only; excludes builds, scripts, '
                                       'features, containers, missing tests and queue time; no savings claim'}
    return {'schema': 'af.test-class-report/1', 'plan': plan, 'provenance': provenance,
            'replay_estimate': replay, 'classes': rows, 'junit_runs': walls,
            'missing_data': missing, 'ci_steps': steps, 'replay_scenarios': estimates if replay else None,
            'parallel_class_critical_path_seconds': max(measured) if len(measured) == selected_count else None,
            'class_runner_seconds': sum(measured) if len(measured) == selected_count else None,
            'note': 'Critical path is max class job time, excludes queue/build/base/container unless included in '
                    'outer evidence; runner cost is sum, never wall savings. Historical replays are estimates. '
                    'JUnit test-seconds are not job wall time. Missing times remain unknown.'}


def summary(value):
    lines = ['### Validation classes', '', '| Class | State | Count | Executed | Passed | Failed | Skipped | Not run | Test seconds | Job seconds |',
             '| --- | --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: |']
    for kind, row in value['classes'].items():
        vals = [kind, row['state']] + [str(row[k]) if row[k] is not None else 'unknown' for k in
                ('count', 'executed', 'passed', 'failed', 'skipped', 'not_run', 'test_seconds', 'step_seconds')]
        lines.append('| ' + ' | '.join(vals) + ' |')
    lines += ['', value['note'], '', f"Missing evidence entries: {len(value['missing_data'])}."]
    return '\n'.join(lines) + '\n'


def call(command, label, stage='tests'):
    subprocess.run([sys.executable, str(ROOT / 'scripts/ci-step.py'), '--stage', stage,
                    label, *command], check=True, cwd=ROOT)


def build_inventory(plan, kind, out, build_scope):
    """Compile the explicit execution scope before reporting successful build completion.

    Native execution needs the entire plan before its first build receipt starts the test
    allowance. Independent hosted jobs need only their own class's variants. Keep Cargo's
    JSON stdout separate from ci-step's human output.
    Children inherit the gate's process group and supervisor deadline as before.
    """
    validate_plan(plan)
    if kind not in plan['selected']:
        raise ValueError(f'{kind} is NOT RUN in this plan')
    if build_scope not in ('class', 'plan'):
        raise ValueError('unknown build scope')
    selected = plan['selected'] if build_scope == 'plan' else [kind]
    variants = [('nextest-list.json', [])]
    if 'perf' in selected:
        variants += [(f'feature-{label}-list.json', ['--features', features])
                     for label, features, _ in PERF_VARIANTS]
    for filename, arguments in variants:
        with (out / filename).open('w') as stream:
            subprocess.run(['cargo', 'nextest', 'list', '--workspace', '--locked',
                            *arguments, '--message-format', 'json'],
                           stdout=stream, check=True, cwd=ROOT)


def run_class(plan, kind, out, threads, build_scope='class'):
    validate_plan(plan)
    audit_source()
    if kind not in plan['selected']:
        raise ValueError(f'{kind} is NOT RUN in this plan')
    out.mkdir(parents=True, exist_ok=True)
    for path in [*out.glob('*.xml'), out / 'receipt.json', out / 'report.json']:
        path.unlink(missing_ok=True)
    write(out / 'plan.json', plan)
    # Ownership is mandatory even for a single selected class, before any Cargo process.
    call([sys.executable, 'scripts/test-class-python.py', 'test-gate-owner.py',
          str(out / 'test-gate-owner.py.xml')], 'gate-owner', 'preflight')
    # Every class audits the whole inventory, even if its own selection is small.
    call([sys.executable, str(Path(__file__)), 'build-inventory',
          '--plan', str(out / 'plan.json'), '--class', kind, '--build-scope', build_scope,
          '--output', str(out)], 'class-build', 'build')
    entries = partition(read(out / 'nextest-list.json'))
    write(out / 'inventory.json', entries)
    expression = filterset(entries, [kind])
    failure = None
    def rust_run(arguments, label, destination, expected):
        shared = (Path(os.environ['AF_GATE_NEXTEST_RUN']) / 'store/ci/junit.xml'
                  if os.getenv('AF_GATE_NEXTEST_RUN') else ROOT / 'target/nextest/ci/junit.xml')
        shared.unlink(missing_ok=True)
        try:
            call([sys.executable, 'scripts/nextest-gate.py', '--workspace', '--locked',
                  '--profile', 'ci', '--test-threads', str(threads), *arguments], label)
        finally:
            if shared.is_file():
                shutil.copyfile(shared, destination)
        if not destination.is_file():
            raise ValueError(f'{label}: no JUnit execution evidence')
        cases = list(ET.parse(destination).getroot().iter('testcase'))
        observed = {(c.get('classname'), c.get('name')) for c in cases if c.find('skipped') is None}
        if observed != expected:
            raise ValueError(f'{label}: execution inventory mismatch; missing={sorted(expected - observed)}; '
                             f'extra={sorted(observed - expected)}')

    try:
        for script, primary in catalog()['scripts'].items():
            if primary == kind and script not in catalog()['always_on_scripts']:
                call([sys.executable, 'scripts/test-class-python.py', script, str(out / (script + '.xml'))], script)
        if kind == 'perf':
            call([sys.executable, 'scripts/performance/schemas.py'], 'performance-schemas')
            call([sys.executable, 'scripts/performance/cli.py', 'harness-digest', '--check'], 'performance-kernel-harness')
        expected = {(e['binary'], e['test']) for e in entries if e['class'] == kind and not e['ignored']}
        if not expected:
            raise ValueError(f'{kind}: no runnable Rust tests')
        rust_run(['-E', expression], f'rust-{kind}', out / 'rust.xml', expected)
        if kind == 'perf':
            for label, features, expression in PERF_VARIANTS:
                feature_entries = partition(read(out / f'feature-{label}-list.json'))
                binary, pattern = FEATURE_SELECTORS[label]
                expected = {(e['binary'], e['test']) for e in feature_entries
                            if e['binary'] == binary and pattern in e['test'] and not e['ignored']}
                if not expected:
                    raise ValueError(f'feature-{label}: no runnable tests')
                if any(e['class'] != 'perf' for e in feature_entries if (e['binary'], e['test']) in expected):
                    raise ValueError(f'feature-{label}: selector crosses the primary class catalog')
                rust_run(['--features', features, '-E', expression], f'feature-{label}',
                         out / f'feature-{label}.xml', expected)
    except subprocess.CalledProcessError as error:
        failure = error
    finally:
        result = report(plan, sorted(out.glob('*.xml')), inventory=entries,
                        provenance={'sha': plan['sha'], 'run_id': os.getenv('GITHUB_RUN_ID')})
        write(out / 'report.json', result)
        if os.getenv('GITHUB_STEP_SUMMARY'):
            with Path(os.environ['GITHUB_STEP_SUMMARY']).open('a') as stream:
                stream.write(summary(result))
    if failure:
        raise failure
    write(out / 'receipt.json', {'schema': 'af.test-class-receipt/2', 'plan': plan,
                                 'platform': os.getenv('RUNNER_OS', sys.platform),
                                 'class': kind, 'status': 'success'})


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    sub = parser.add_subparsers(dest='command', required=True)
    plan = sub.add_parser('plan')
    plan.add_argument('--context', choices=('pr', 'gate', 'release_pr', 'local', 'release'), default='local')
    plan.add_argument('--label', action='append', default=[])
    plan.add_argument('--event')
    plan.add_argument('--bound')
    plan.add_argument('--sha', default=os.getenv('GITHUB_SHA'))
    plan.add_argument('--version')
    plan.add_argument('--output', required=True)
    plan.add_argument('--github-output')
    sub.add_parser('audit')
    schema = sub.add_parser('schemas')
    schema.add_argument('--write', action='store_true')
    measurement = sub.add_parser('measurement-scope')
    measurement.add_argument('--event', required=True)
    measurement.add_argument('--event-name', required=True)
    measurement.add_argument('--release-claim', default='')
    measurement.add_argument('--github-output', required=True)
    inv = sub.add_parser('inventory')
    inv.add_argument('path')
    inv.add_argument('--output', required=True)
    run = sub.add_parser('run')
    run.add_argument('--plan', required=True)
    run.add_argument('--class', dest='kind', choices=CLASSES, required=True)
    run.add_argument('--output', required=True)
    run.add_argument('--threads', type=int, default=max(4, (os.cpu_count() or 4) // 2))
    run.add_argument('--build-scope', choices=('class', 'plan'), default='class')
    build = sub.add_parser('build-inventory')
    build.add_argument('--plan', required=True)
    build.add_argument('--output', required=True)
    build.add_argument('--class', dest='kind', choices=CLASSES, required=True)
    build.add_argument('--build-scope', choices=('class', 'plan'), required=True)
    execute = sub.add_parser('execute')
    execute.add_argument('--plan', required=True)
    execute.add_argument('--threads', type=int, default=4)
    release = sub.add_parser('release-plan')
    release.add_argument('--annotation', required=True)
    release.add_argument('--sha', required=True)
    release.add_argument('--version', required=True)
    release.add_argument('--output', required=True)
    gate = sub.add_parser('aggregate')
    gate.add_argument('--plan', required=True)
    gate.add_argument('--needs', required=True)
    gate.add_argument('--evidence', required=True)
    gate.add_argument('--platforms', nargs='+', required=True)
    gate_run = sub.add_parser('nextest-gate', help='run scripts/nextest-gate.py without the quarantined tests')
    gate_run.add_argument('arguments', nargs=argparse.REMAINDER)
    metrics = sub.add_parser('report')
    metrics.add_argument('--plan', required=True)
    metrics.add_argument('--junit', nargs='*', default=[])
    metrics.add_argument('--steps', nargs='*', default=[])
    metrics.add_argument('--inventory')
    metrics.add_argument('--source-run', required=True)
    metrics.add_argument('--source-sha', required=True)
    metrics.add_argument('--replay', action='store_true')
    metrics.add_argument('--output', required=True)
    args = parser.parse_args()
    if args.command == 'plan':
        audit_source()
        labels = event_labels(read(args.event)) if args.event else args.label
        context = event_context(read(args.event)) if args.event and args.context == 'pr' else args.context
        version = args.version
        if args.event and context == 'release_pr':
            version = read(args.event)['pull_request']['head']['ref'][len('release/v'):]
        value = validate_plan(json.loads(args.bound)) if args.bound else resolve(
            labels, context, args.sha, version)
        if args.event:
            check_release_pr(read(args.event))
        if args.bound and (value['context'] not in ('release', 'release_pr') or value['sha'] != args.sha):
            raise ValueError('bound release plan does not name this commit')
        if args.bound and args.version is not None and value['version'] != args.version:
            raise ValueError('bound release plan does not name this version')
        write(args.output, value)
        print(json.dumps(value, indent=2))
        if args.github_output:
            with Path(args.github_output).open('a') as stream:
                stream.write('plan=' + json.dumps(value, separators=(',', ':')) + '\n')
                stream.write('selected=' + json.dumps(value['selected']) + '\n')
                stream.write('e2e=' + str('e2e' in value['selected']).lower() + '\n')
                stream.write('perf=' + str('perf' in value['selected']).lower() + '\n')
    elif args.command == 'audit':
        audit_source()
    elif args.command == 'nextest-gate':
        arguments = args.arguments[1:] if args.arguments[:1] == ['--'] else args.arguments
        gate = str(ROOT / 'scripts/nextest-gate.py')
        os.execv(sys.executable, [sys.executable, gate, *gate_arguments(arguments, quarantine())])
    elif args.command == 'schemas':
        schema_parity(args.write)
    elif args.command == 'measurement-scope':
        exempt = False
        if args.event_name == 'pull_request' and not args.release_claim:
            event = read(args.event)
            exempt = 'perf' not in resolve(event_labels(event), event_context(event))['selected']
        with Path(args.github_output).open('a') as stream:
            stream.write('exempt=' + str(exempt).lower() + '\n')
    elif args.command == 'inventory':
        write(args.output, partition(read(args.path)))
    elif args.command == 'run':
        run_class(read(args.plan), args.kind, Path(args.output).resolve(), args.threads,
                  args.build_scope)
    elif args.command == 'build-inventory':
        build_inventory(read(args.plan), args.kind, Path(args.output), args.build_scope)
    elif args.command == 'execute':
        plan = validate_plan(read(args.plan))
        evidence_root = (Path(os.environ['AF_GATE_NEXTEST_RUN']) / 'classes'
                         if os.getenv('AF_GATE_NEXTEST_RUN') else ROOT / 'target/test-classes/local')
        for kind in plan['selected']:
            call([sys.executable, str(Path(__file__)), 'run', '--plan', args.plan,
                  '--class', kind, '--output', str(evidence_root / kind),
                  '--threads', str(args.threads), '--build-scope', 'plan'], 'class-' + kind)
            print(summary(read(evidence_root / kind / 'report.json')))
        if 'e2e' in plan['selected']:
            call(['make', 'review-kernel-container-probes'], 'container-probes')
    elif args.command == 'release-plan':
        write(args.output, release_plan(Path(args.annotation).read_text(), args.sha, args.version))
    elif args.command == 'aggregate':
        plan = read(args.plan)
        aggregate(plan, read(args.needs))
        receipts(plan, args.evidence, args.platforms)
        value = aggregate_metrics(plan, args.evidence, args.platforms)
        write('target/test-classes/validation-metrics.json', value)
        print(aggregate_summary(value))
        if os.getenv('GITHUB_STEP_SUMMARY'):
            with Path(os.environ['GITHUB_STEP_SUMMARY']).open('a') as stream:
                stream.write(aggregate_summary(value))
    elif args.command == 'report':
        value = report(read(args.plan), args.junit, args.steps,
                       read(args.inventory) if args.inventory else None,
                       {'source_run': args.source_run, 'source_sha': args.source_sha}, args.replay)
        write(args.output, value)
        print(summary(value))
        if os.getenv('GITHUB_STEP_SUMMARY'):
            with Path(os.environ['GITHUB_STEP_SUMMARY']).open('a') as stream:
                stream.write(summary(value))
    return 0


if __name__ == '__main__':
    try:
        sys.exit(main())
    except (ValueError, KeyError, OSError, subprocess.CalledProcessError) as error:
        print(f'test-classes: {error}', file=sys.stderr)
        sys.exit(1)
