#!/usr/bin/env python3
"""Check one issue-triage Task's two documents and merge them (docs/design/issue-triage.md §6.3).

Reads the author's `af/issue-triage@1` document from the `Triage` section of its report (the
Markdown `af task output --port report` writes with `--format markdown` or `--format
canonical-markdown`, or the `af.document-draft/2` draft) and the verifier's from the one fenced
json block of its `af/ReportEvaluation@1` summary, validates both against
`schemas/issue-triage-v1.json`, applies the hard rules and the merge rule, and prints one
`af/issue-triage-merged@1` record on stdout.

Exit 0: a merged record (verdict agreed, duplicate, disputed or needs-info). Exit 1: a refusal,
one line on stderr naming the rule. Exit 2: a usage error, such as a missing identity flag; the
runner publishes nothing. Exit 3: a security hold, decided before anything else from a
`class/security` or `triage/held` label (`--label` or `--owner-label`) or a `security` class from
either triager: stdout holds the private `af/issue-triage-hold@1` record
(`schemas/issue-triage-hold-v1.json`), no label is set, and nothing about the issue may be
published.

Documents are data: nothing in them is executed or followed. The only process this script starts
is `git -C REPO` with fixed arguments, to resolve the captured main and to test that a commit
the evidence names is an ancestor of it."""
import argparse
import json
import math
from pathlib import Path
import re
import subprocess
import sys

sys.dont_write_bytecode = True

ROOT = Path(__file__).resolve().parent.parent
TRIAGE_SCHEMA = ROOT / 'schemas' / 'issue-triage-v1.json'
MERGED_SCHEMA = ROOT / 'schemas' / 'issue-triage-merged-v1.json'
HOLD_SCHEMA = ROOT / 'schemas' / 'issue-triage-hold-v1.json'
EVALUATION_SCHEMA = ROOT / 'schemas' / 'report-evaluation-v1.json'

TRIAGE = 'af/issue-triage@1'
MERGED = 'af/issue-triage-merged@1'
HOLD = 'af/issue-triage-hold@1'
PIPELINE_BY_SIZE = {'S': 'light', 'M': 'default', 'L': 'design', 'XL': 'design'}
SIZE_ORDER = ['S', 'M', 'L', 'XL']
NAMESPACES = {
    'class': ('bug', 'perf', 'feature', 'aix', 'uix', 'sdlc', 'security', 'docs', 'research', 'decision'),
    'size': ('S', 'M', 'L', 'XL'),
    'pipeline': ('light', 'default', 'design', 'hand'),
    'triage': ('pending', 'needs-info', 'disputed', 'maybe-fixed', 'duplicate', 'held'),
}
HOLD_LABELS = ('class/security', 'triage/held')
CONFIDENCE_FLOOR = 0.6

EXIT_REFUSED = 1
EXIT_USAGE = 2
EXIT_HELD = 3

# The identities a publishable record carries (§6.3, §6.5): the flag that gives each, its key.
IDENTITIES = (('--task-id', 'task_id'), ('--document-id', 'document_id'), ('--evaluation-id', 'evaluation_id'),
              ('--bundle-sha256', 'bundle_sha256'), ('--main', 'main'), ('--policy-version', 'policy_version'),
              ('--event-id', 'event_id'))


class Refusal(Exception):
    """The Task's output is refused under `rule`; the runner records why and publishes nothing."""

    def __init__(self, rule, message):
        super().__init__(f'{rule}: {message}')
        self.rule = rule


class Usage(Exception):
    """The runner called the check without what a publishable record needs (exit 2)."""


# --- A JSON Schema subset: the keywords this repository's schemas use, nothing more. ----------

def _type_ok(value, name):
    if name == 'null':
        return value is None
    if name == 'boolean':
        return isinstance(value, bool)
    if name == 'integer':
        return isinstance(value, int) and not isinstance(value, bool)
    if name == 'number':
        return isinstance(value, (int, float)) and not isinstance(value, bool) and math.isfinite(value)
    if name == 'string':
        return isinstance(value, str)
    if name == 'array':
        return isinstance(value, list)
    if name == 'object':
        return isinstance(value, dict)
    raise ValueError(f'unsupported schema type {name!r}')


SUPPORTED = {'$schema', '$id', '$comment', '$defs', 'title', 'description', '$ref', 'type', 'const', 'enum',
             'required', 'properties', 'additionalProperties', 'propertyNames', 'minProperties',
             'maxProperties', 'items', 'minItems', 'maxItems', 'uniqueItems', 'minLength', 'maxLength',
             'pattern', 'minimum', 'maximum', 'anyOf', 'allOf', 'not'}


class Validator:
    """Validates documents against schemas loaded by `$id`; `$ref` resolves a loaded `$id` and a
    `#/$defs/...` pointer. An unknown keyword is an error, never silently ignored."""

    def __init__(self, *paths):
        self.schemas = {}
        for path in paths:
            schema = json.loads(Path(path).read_text(encoding='utf-8'))
            self.schemas[schema['$id']] = schema

    def errors(self, value, schema_id):
        schema = self.schemas[schema_id]
        out = []
        self._check(value, schema, schema, '$', out)
        return out

    def _resolve(self, ref, base):
        uri, _, pointer = ref.partition('#')
        target = self.schemas[uri] if uri else base
        node = target
        for part in [p for p in pointer.split('/') if p]:
            node = node[part]
        return node, target

    def _check(self, value, schema, base, at, out):
        unknown = set(schema) - SUPPORTED
        if unknown:
            raise ValueError(f'unsupported schema keywords {sorted(unknown)} at {at}')
        if '$ref' in schema:
            node, target = self._resolve(schema['$ref'], base)
            self._check(value, node, target, at, out)
        if 'type' in schema:
            names = schema['type'] if isinstance(schema['type'], list) else [schema['type']]
            if not any(_type_ok(value, name) for name in names):
                out.append(f'{at}: expected {" or ".join(names)}, got {json.dumps(value)[:60]}')
                return
        if 'const' in schema and not _same(value, schema['const']):
            out.append(f'{at}: must be {json.dumps(schema["const"])}')
        if 'enum' in schema and not any(_same(value, option) for option in schema['enum']):
            out.append(f'{at}: must be one of {json.dumps(schema["enum"])}, got {json.dumps(value)[:60]}')
        for branch in schema.get('allOf', []):
            self._check(value, branch, base, at, out)
        if 'anyOf' in schema:
            if not any(not self._sub(value, branch, base, at) for branch in schema['anyOf']):
                out.append(f'{at}: matches none of its alternatives: '
                           + '; '.join(self._sub(value, schema['anyOf'][0], base, at)[:1]))
        if 'not' in schema and not self._sub(value, schema['not'], base, at):
            out.append(f'{at}: matches a shape it must not')
        if isinstance(value, dict):
            self._object(value, schema, base, at, out)
        if isinstance(value, list):
            self._array(value, schema, base, at, out)
        if isinstance(value, str):
            if len(value) < schema.get('minLength', 0):
                out.append(f'{at}: shorter than {schema["minLength"]} characters')
            if 'maxLength' in schema and len(value) > schema['maxLength']:
                out.append(f'{at}: longer than {schema["maxLength"]} characters')
            if 'pattern' in schema and not re.search(schema['pattern'], value):
                out.append(f'{at}: does not match {schema["pattern"]}')
        if _type_ok(value, 'number'):
            if 'minimum' in schema and value < schema['minimum']:
                out.append(f'{at}: below {schema["minimum"]}')
            if 'maximum' in schema and value > schema['maximum']:
                out.append(f'{at}: above {schema["maximum"]}')

    def _sub(self, value, schema, base, at):
        out = []
        self._check(value, schema, base, at, out)
        return out

    def _object(self, value, schema, base, at, out):
        for name in schema.get('required', []):
            if name not in value:
                out.append(f'{at}: missing {name}')
        if len(value) < schema.get('minProperties', 0):
            out.append(f'{at}: fewer than {schema["minProperties"]} properties')
        if 'maxProperties' in schema and len(value) > schema['maxProperties']:
            out.append(f'{at}: more than {schema["maxProperties"]} properties')
        properties = schema.get('properties', {})
        for name, item in value.items():
            if 'propertyNames' in schema:
                self._check(name, schema['propertyNames'], base, f'{at} key {name!r}', out)
            if name in properties:
                self._check(item, properties[name], base, f'{at}.{name}', out)
            elif schema.get('additionalProperties') is False:
                out.append(f'{at}: unknown property {name}')
            elif isinstance(schema.get('additionalProperties'), dict):
                self._check(item, schema['additionalProperties'], base, f'{at}.{name}', out)

    def _array(self, value, schema, base, at, out):
        if len(value) < schema.get('minItems', 0):
            out.append(f'{at}: fewer than {schema["minItems"]} items')
        if 'maxItems' in schema and len(value) > schema['maxItems']:
            out.append(f'{at}: more than {schema["maxItems"]} items')
        if schema.get('uniqueItems') and len({json.dumps(v, sort_keys=True) for v in value}) != len(value):
            out.append(f'{at}: items are not unique')
        if 'items' in schema:
            for index, item in enumerate(value):
                self._check(item, schema['items'], base, f'{at}[{index}]', out)


def _same(a, b):
    return (type(a) is type(b) and a == b) or (_type_ok(a, 'number') and _type_ok(b, 'number') and a == b)


def validator():
    return Validator(TRIAGE_SCHEMA, MERGED_SCHEMA, HOLD_SCHEMA, EVALUATION_SCHEMA)


# --- Reading the two documents. --------------------------------------------------------------

FENCE = re.compile(r'^ {0,3}(`{3,}|~{3,})[ \t]*(\S*)[ \t]*$')


def fenced_blocks(text):
    """Every closed fenced block of `text`: (info word, body). A fence closes on a line holding only
    the same character at least as many times, as the kernel's renderer writes it."""
    blocks, lines, i = [], text.split('\n'), 0
    while i < len(lines):
        match = FENCE.match(lines[i])
        if not match:
            i += 1
            continue
        fence, info = match.group(1), match.group(2)
        close = re.compile(r'^ {0,3}' + re.escape(fence[0]) + '{' + str(len(fence)) + r',}[ \t]*$')
        end = next((j for j in range(i + 1, len(lines)) if close.match(lines[j])), None)
        if end is None:
            return blocks
        blocks.append((info, '\n'.join(lines[i + 1:end])))
        i = end + 1
    return blocks


def markdown_sections(text):
    """The `## ` sections of a rendered report, by heading; a heading inside a fence is content."""
    sections, current, fence = {}, None, None
    for line in text.split('\n'):
        match = FENCE.match(line)
        if fence is None and match:
            fence = match.group(1)
        elif fence is not None and re.fullmatch(r' {0,3}' + re.escape(fence[0]) + '{' + str(len(fence)) + r',}[ \t]*',
                                                line):
            fence = None
        elif fence is None and line.startswith('## '):
            current = line[3:].strip()
            sections.setdefault(current, [])
            continue
        if current is not None:
            sections[current].append(line)
    return {heading: '\n'.join(lines).strip('\n') for heading, lines in sections.items()}


ASCII_PUNCTUATION = frozenset('!"#$%&\'()*+,-./:;<=>?@[\\]^_`{|}~')


def canonical_text(text):
    """The authored text of a section body as the canonical renderer wrote it
    (`markdown_text` in crates/af-runtime/src/task/document.rs, the bytes of
    `--format canonical-markdown`): it escapes every backtick, `~` and other markup character with
    a backslash, and writes a word a GFM autolink could claim (`@`, `://`, `www.`) as inert code
    spans, each backtick of the word an escaped backtick between them. So a backslash before ASCII
    punctuation is that character, and a code span is its content verbatim. The padding the
    renderer trims from each line stays trimmed. Nothing in the text is evaluated."""
    out, i = [], 0
    while i < len(text):
        c = text[i]
        if c == '\\' and i + 1 < len(text) and text[i + 1] in ASCII_PUNCTUATION:
            out.append(text[i + 1])
            i += 2
            continue
        end = text.find('`', i + 1) if c == '`' else -1
        if end > 0 and '\n' not in text[i + 1:end]:
            out.append(text[i + 1:end])
            i = end + 1
            continue
        out.append(c)
        i += 1
    return ''.join(out)


def one_json_block(text, where, rule, required):
    """The one fenced `json` block of `text`, parsed; None when absent and not required."""
    blocks = [body for info, body in fenced_blocks(text) if info == 'json']
    if not blocks:
        if required:
            raise Refusal(rule, f'{where} holds no fenced json block')
        return None
    if len(blocks) > 1:
        raise Refusal(rule, f'{where} holds {len(blocks)} fenced json blocks; it must hold one')
    try:
        return json.loads(blocks[0])
    except json.JSONDecodeError as error:
        raise Refusal(rule, f'the fenced json block of {where} does not parse: {error}') from None


def author_document(*, document=None, draft=None):
    """The author's `af/issue-triage@1` from its rendered report or its draft; both need a
    nonblank `Rationale` section and a `Triage` section holding exactly one json block. The
    report is either form `af task output` writes: the presentation (`--format markdown`) keeps
    an authored closed fence as a fenced example; the canonical bytes (`--format
    canonical-markdown`) escape every fence, so a `Triage` section without a fence is read
    through `canonical_text` first."""
    if draft is not None:
        if not isinstance(draft, dict) or draft.get('schema') != 'af.document-draft/2':
            raise Refusal('author-draft', 'the draft is not an af.document-draft/2 document')
        if not isinstance(draft.get('sections'), list):
            raise Refusal('author-draft', 'the draft sections must be an array')
        sections = {}
        for section in draft['sections']:
            if isinstance(section, dict) and section.get('heading') in ('Triage', 'Rationale'):
                if section['heading'] in sections:
                    raise Refusal('author-draft', f'the draft has two {section["heading"]} sections')
                body = section.get('body')
                if body is not None and not isinstance(body, str):
                    raise Refusal('author-draft', f'the draft {section["heading"]} section body must be a string')
                sections[section['heading']] = body or ''
    else:
        sections = markdown_sections(document)
        if 'Triage' in sections and not fenced_blocks(sections['Triage']):
            sections['Triage'] = canonical_text(sections['Triage'])
    for heading in ('Triage', 'Rationale'):
        if not (sections.get(heading) or '').strip():
            raise Refusal('author-draft', f'the report has no nonblank {heading} section')
    return one_json_block(sections['Triage'], 'the Triage section', 'author-draft', required=True)


def verifier_document(evaluation, check):
    """The evaluation's outcome and the verifier's own `af/issue-triage@1`, or None when its
    summary holds none (which `check` refuses unless the issue is held)."""
    errors = check.errors(evaluation, 'urn:af:schema:report-evaluation:1')
    if errors:
        raise Refusal('evaluation-schema', '; '.join(errors[:5]))
    return evaluation['outcome'], one_json_block(evaluation['summary'], 'the evaluation summary',
                                                 'verifier-block', required=False)


# --- The hard rules. -------------------------------------------------------------------------

CHANGE = re.compile(r'(?i)(?<![\w-])(?:(no|not\s+a|without\s+a|without)\s+)?'
                    r'(contract|schema[- ]version|budget|sandbox[- ]boundary)\s+change\b')
COMMIT = re.compile(r'(?<![0-9A-Za-z#._:-])[0-9a-f]{7,40}(?![0-9A-Za-z_-])')
# A marked commit reference: `commit <hex>`, or `<hex>` as a code span.
MARKED_COMMIT = re.compile(r'(?i:\bcommit\s+)([0-9a-f]{7,40})(?![0-9A-Za-z_-])|`([0-9a-f]{7,40})`')
ALONE_COMMIT = re.compile(r'[0-9a-f]{7,40}')
PULL = re.compile(r'#([1-9][0-9]*)\b')


def proposed_changes(rationale):
    """The contract, schema-version, budget or sandbox-boundary changes a rationale names with
    the instructions' phrase, unless negated as `no X change`, `not a X change` or `without (a) X change`."""
    return [match.group(2).lower() for match in CHANGE.finditer(rationale) if not match.group(1)]


def references(evidence, issue=None):
    """The commits and pull requests (`PR #N`, `#N`) the evidence names; the triaged issue's own
    number is not a pull request. A commit is 7 to 40 lowercase hexadecimal digits. A marked one,
    `commit <hex>`, `<hex>` as a code span or an entry that is the hex string alone, is a commit
    whatever its mix of digits and letters, and Git decides whether it is reachable. An unmarked
    run in prose is a commit only when it mixes digits and letters: an all-digit run there is a
    number and an all-letter one a word."""
    commits, pulls = [], []
    for entry in evidence:
        marked = [a or b for a, b in MARKED_COMMIT.findall(entry)]
        if ALONE_COMMIT.fullmatch(entry.strip()):
            marked.append(entry.strip())
        unmarked = [token for token in COMMIT.findall(MARKED_COMMIT.sub(' ', entry))
                    if re.search('[a-f]', token) and re.search('[0-9]', token)]
        for token in marked + unmarked:
            if token not in commits:
                commits.append(token)
        for number in PULL.findall(entry):
            if int(number) not in pulls and int(number) != issue:
                pulls.append(int(number))
    return commits, pulls


class GitReach:
    """Reachability from the captured main of `repo`, and the merged pull requests the runner
    listed; None for `pulls` means no list was given, so no pull request is verified. `main` is
    resolved on first use, so a hold never runs Git."""

    def __init__(self, repo, main, pulls=None):
        self.repo, self.pulls, self._main, self._resolved = repo, pulls, main, None

    @property
    def main(self):
        if self._resolved is None:
            result = self._git('rev-parse', '--verify', '--end-of-options', f'{self._main}^{{commit}}')
            if result.returncode:
                raise Refusal('main', f'{self._main!r} is not a commit of {self.repo}: {result.stderr.strip()}')
            self._resolved = result.stdout.strip()
        return self._resolved

    def _git(self, *args):
        return subprocess.run(['git', '-C', str(self.repo), *args], capture_output=True, text=True)

    def commit(self, sha):
        return self._git('merge-base', '--is-ancestor', '--end-of-options', sha, self.main).returncode == 0

    def pull(self, number):
        return self.pulls is not None and number in self.pulls


def check_rules(document, who, reach):
    """Apply §6.3's hard rules to one triager's document; return the references it checked."""
    size_rank = SIZE_ORDER.index(document['size'])
    changes = proposed_changes(document['rationale'])
    if changes and size_rank < SIZE_ORDER.index('L'):
        raise Refusal('size-below-L', f'the {who} names a {changes[0]} change in its rationale with size '
                                      f'{document["size"]}; such a change is at least L')
    if document['adr'] is not None and document['size'] == 'S':
        raise Refusal('adr-with-S', f'the {who} needs an ADR ({document["adr"]}) with size S')
    checked = {'commits': [], 'pulls': [], 'unverified': []}
    state = document['resolution']['state']
    if state == 'open':
        return checked
    commits, pulls = references(document['resolution']['evidence'], document['issue'])
    for sha in commits:
        if not reach.commit(sha):
            raise Refusal('resolution-evidence', f'the {who} names commit {sha} as {state} evidence, and it '
                                                 f'is not reachable from the captured main {reach.main}')
        checked['commits'].append(sha)
    for number in pulls:
        (checked['pulls'] if reach.pull(number) else checked['unverified']).append(number)
    if not checked['commits'] and not checked['pulls']:
        listed = 'no merged-pulls list was given' if reach.pulls is None else 'none is a listed merged pull request'
        raise Refusal('resolution-evidence', f'the {who} claims {state} and its evidence names no commit '
                                             f'reachable from the captured main and no merged pull request '
                                             f'({len(pulls)} pull request reference(s); {listed})')
    return checked


# --- The merge rule. -------------------------------------------------------------------------

def _union(*lists):
    out = []
    for items in lists:
        for item in items:
            if item not in out:
                out.append(item)
    return out


def owner_overrides(labels):
    """The owner's labels in the runner's namespaces, by namespace; two in one namespace suspend
    write-back for the issue (§6.5)."""
    override = {}
    for label in labels:
        namespace, _, value = label.partition('/')
        if namespace not in NAMESPACES or value not in NAMESPACES[namespace]:
            raise Refusal('owner-override', f'{label!r} is not a label of a runner namespace')
        if namespace in override and override[namespace] != label:
            raise Refusal('owner-override', f'the owner set both {override[namespace]} and {label}; '
                                            'write-back stays suspended until the owner resolves them')
        override[namespace] = label
    return override or None


def merge(author, verifier, *, outcome='passed', owner_override=None):
    """Steps 2 to 4 of the merge rule of §6.3, in their order, as a pure function of the two
    documents, the evaluation's outcome and the owner's overrides (namespace -> label). Step 1,
    the hold, is `hold_triggers`, which `check` applies before anything else.

    `verifier` is the verifier's own document, which every outcome carries; the author's is
    never substituted for it. Returns the record's merge fields: verdict, the agreed fields,
    needs_info, reasons, labels."""
    if not isinstance(verifier, dict):
        raise Refusal('verifier-block', f'the evaluation summary must end with the verifier\'s own {TRIAGE} block')
    override = owner_override or {}
    effective = {name: override[name].split('/', 1)[1] for name in ('class', 'size', 'pipeline') if name in override}
    record = {'class': effective.get('class'), 'size': effective.get('size'),
              'pipeline': effective.get('pipeline'), 'adr': None, 'resolution': None, 'duplicates': [],
              'needs_info': [], 'reasons': []}

    questions = _union(author['needs_info'], verifier['needs_info'])
    reasons = []
    if questions:
        reasons.append('a triager has open questions')
    if author['confidence'] < CONFIDENCE_FLOOR:
        reasons.append(f'the author\'s confidence {author["confidence"]} is below {CONFIDENCE_FLOOR}')
    if outcome == 'inconclusive':
        reasons.append('the verifier found the evidence insufficient')
    if reasons:
        return _labelled(dict(record, verdict='needs-info', needs_info=questions, reasons=reasons), override)

    for field in ('class', 'size'):
        if author[field] != verifier[field]:
            reasons.append(f'{field}: the author says {author[field]}, the verifier {verifier[field]}')
    a_state, v_state = author['resolution']['state'], verifier['resolution']['state']
    if a_state != v_state:
        reasons.append(f'resolution: the author says {a_state}, the verifier {v_state}')
    if sorted(author['duplicates']) != sorted(verifier['duplicates']):
        reasons.append(f'duplicates: the author names {sorted(author["duplicates"])}, '
                       f'the verifier {sorted(verifier["duplicates"])}')
    # Owner labels determine effective values, never independent agreement or report acceptance.
    if outcome == 'failed' and not reasons:
        reasons.append('the verifier failed the author\'s report')
    if reasons:
        return _labelled(dict(record, verdict='disputed', reasons=reasons), override)

    size = effective.get('size', author['size'])
    record.update({
        'class': effective.get('class', author['class']), 'size': size,
        'pipeline': effective.get('pipeline', PIPELINE_BY_SIZE[size]),
        'adr': author['adr'] if author['adr'] is not None else verifier['adr'],
        'resolution': {'state': a_state,
                       **{key: _union(author['resolution'][key], verifier['resolution'][key])
                          for key in ('evidence', 'addressed', 'remaining')}},
        'duplicates': sorted(author['duplicates']),
    })
    return _labelled(dict(record, verdict='duplicate' if record['duplicates'] else 'agreed'), override)


def _labelled(record, override):
    verdict = record['verdict']
    if verdict == 'agreed':
        labels = {'class': f'class/{record["class"]}', 'size': f'size/{record["size"]}',
                  'pipeline': f'pipeline/{record["pipeline"]}'}
        if record['resolution']['state'] == 'fixed_candidate':
            labels['triage'] = 'triage/maybe-fixed'
    else:
        labels = {'triage': f'triage/{verdict}'}
    labels.update(override)
    record['labels'] = [labels[name] for name in NAMESPACES if name in labels]
    return record


def hold_triggers(labels, owner_labels, author, verifier):
    """Step 1 of the merge rule (§6.3, §7): the hold labels among the issue's labels and the
    owner's, and the triagers whose document names `security`. Either holds the issue."""
    held = _union([label for label in [*labels, *owner_labels] if label in HOLD_LABELS])
    triagers = [who for who, doc in (('author', author), ('verifier', verifier))
                if isinstance(doc, dict) and doc.get('class') == 'security']
    return held, triagers


def hold_record(author, held, triagers, identity):
    """The private `af/issue-triage-hold@1`: the issue when the author's document names one, why,
    what triggered the hold, and the generation identities the runner gave, as given."""
    issue = author.get('issue') if isinstance(author, dict) else None
    if not (isinstance(issue, int) and not isinstance(issue, bool) and issue >= 1):
        issue = None
    generation = {key: identity.get(key) or None
                  for key in ('bundle_sha256', 'main', 'policy_version', 'event_id')}
    record = {
        'schema': HOLD, 'issue': issue,
        'reasons': [f'the issue carries {label}' for label in held]
        + [f'the {who} classified security' for who in triagers],
        'held_labels': held, 'security_triagers': triagers,
        'generation': generation if any(generation.values()) else None,
    }
    errors = validator().errors(record, 'urn:af:schema:issue-triage-hold:1')
    if errors:
        raise Refusal('hold-schema', '; '.join(errors[:5]))
    return record


def check(author, evaluation, labels, reach, *, owner_labels=(), identity=None, hand_triage=None):
    """Hold, validate, apply the hard rules and merge: the `af/issue-triage-hold@1` record with
    exit code 3, or the `af/issue-triage-merged@1` record with 0. `author` or `evaluation` is None
    when it could not be read: only a hold label then gives a result. `identity` holds the values
    of IDENTITIES by key. Raises Usage when a publishable record would lack an identity, and
    Refusal for anything else the runner must not publish."""
    schema = validator()
    identity = identity or {}
    # A hold label decides before anything else: no document is read further.
    held, _ = hold_triggers(labels, owner_labels, None, None)
    if held:
        return hold_record(author, held, [], identity), EXIT_HELD
    if author is None:
        raise Refusal('author-draft', 'the author\'s report could not be read')
    try:
        outcome, verifier, unreadable = *verifier_document(evaluation, schema), None
    except Refusal as refusal:
        outcome, verifier, unreadable = None, None, refusal
    # A document that names security holds the issue unvalidated, so nothing about it is published.
    _, triagers = hold_triggers((), (), author, verifier)
    if triagers:
        return hold_record(author, [], triagers, identity), EXIT_HELD
    if unreadable is not None:
        raise unreadable
    if verifier is None:
        raise Refusal('verifier-block', f'the {outcome} evaluation\'s summary holds no {TRIAGE} block; every '
                                        'outcome must end with the verifier\'s own document')
    missing = [flag for flag, key in IDENTITIES if not identity.get(key)]
    if missing:
        raise Usage(f'a publishable record needs {", ".join(missing)}')
    if identity['document_id'] != evaluation['document_id']:
        raise Refusal('document-identity', f'--document-id {identity["document_id"]} is not the Document the '
                                           f'evaluation judged, {evaluation["document_id"]}')
    for who, doc in (('author', author), ('verifier', verifier)):
        errors = schema.errors(doc, 'urn:af:schema:issue-triage:1')
        if errors:
            raise Refusal('triage-schema', f'the {who}\'s document: ' + '; '.join(errors[:5]))
    if (verifier['issue'], verifier['body_sha256']) != (author['issue'], author['body_sha256']):
        raise Refusal('issue-identity', 'the verifier triaged another issue or body than the author')
    override = owner_overrides(owner_labels)
    evidence = {'commits': [], 'pulls': [], 'unverified': []}
    for who, doc in (('author', author), ('verifier', verifier)):
        checked = check_rules(doc, who, reach)
        for key in evidence:
            evidence[key] = _union(evidence[key], checked[key])
    merged = merge(author, verifier, outcome=outcome, owner_override=override)
    if hand_triage is not None and merged['verdict'] == 'agreed':
        recorded = hand_triage.get('issues', {}).get(str(author['issue']))
        if recorded and recorded['state'] != merged['resolution']['state']:
            raise Refusal('hand-triage', f'issue #{author["issue"]} was verified by hand as {recorded["state"]} '
                                         f'on {hand_triage.get("verified_on")}; the triagers agreed on '
                                         f'{merged["resolution"]["state"]}')
    record = {
        'schema': MERGED, 'issue': author['issue'], 'body_sha256': author['body_sha256'],
        'verdict': merged['verdict'],
        **{key: merged[key] for key in ('class', 'size', 'pipeline', 'adr', 'resolution', 'duplicates',
                                        'needs_info', 'reasons', 'labels')},
        'evidence_check': {'commits': evidence['commits'], 'pulls': evidence['pulls'],
                           'unverified': [f'#{number}' for number in evidence['unverified']]},
        'generation': {'issue': author['issue'], 'bundle_sha256': identity['bundle_sha256'],
                       'main_commit': reach.main, 'policy_version': identity['policy_version'],
                       'event_id': identity['event_id']},
        'sources': {key: identity[key] for key in ('task_id', 'document_id', 'evaluation_id')},
        'author': author, 'verifier': verifier, 'owner_override': override,
    }
    errors = schema.errors(record, 'urn:af:schema:issue-triage-merged:1')
    if errors:
        raise Refusal('merged-schema', '; '.join(errors[:5]))
    return record, 0


def _load_json(path, what):
    try:
        return json.loads(Path(path).read_text(encoding='utf-8'))
    except (OSError, UnicodeDecodeError, json.JSONDecodeError) as error:
        raise Refusal('input', f'{what} {path}: {error}') from None


def _read_author(args):
    if args.document is not None:
        try:
            text = Path(args.document).read_text(encoding='utf-8')
        except (OSError, UnicodeDecodeError) as error:
            raise Refusal('input', f'document {args.document}: {error}') from None
        return author_document(document=text)
    return author_document(draft=_load_json(args.draft, 'draft'))


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    author = parser.add_mutually_exclusive_group(required=True)
    author.add_argument('--document', help='the author\'s report as `af task output TASK --port report` writes it '
                                           'with --format markdown or --format canonical-markdown')
    author.add_argument('--draft', help='the author\'s af.document-draft/2 JSON')
    parser.add_argument('--evaluation', required=True, help='the verifier\'s af/ReportEvaluation@1 JSON payload')
    parser.add_argument('--repo', default='.', help='the Git repository holding --main (default: .)')
    parser.add_argument('--pulls', help='a JSON list of the merged pull request numbers, written by the runner')
    parser.add_argument('--label', action='append', default=[], help='a label the issue carries now (repeatable)')
    parser.add_argument('--owner-label', action='append', default=[],
                        help='a label the owner set in a runner namespace: an override (repeatable)')
    parser.add_argument('--hand-triage', help='a hand-verified af.hand-triage/1 file whose recorded resolution '
                                              'state an agreed record must reach')
    identities = parser.add_argument_group('identities', 'required for every record but a hold (§6.3, §6.5)')
    identities.add_argument('--task-id', help='the triage Task\'s id')
    identities.add_argument('--document-id', help='the author\'s af/Document@1 artifact id, the one the evaluation '
                                                  'judged')
    identities.add_argument('--evaluation-id', help='the evaluation\'s artifact id')
    identities.add_argument('--bundle-sha256', help='the sha256 of the Task\'s input bundle')
    identities.add_argument('--main', help='the captured main commit the Task\'s Snapshot was built from')
    identities.add_argument('--policy-version', help='the runner\'s policy version')
    identities.add_argument('--event-id', help='the id of the event that triggered this generation')
    args = parser.parse_args(argv)
    held = any(label in HOLD_LABELS for label in [*args.label, *args.owner_label])
    identity = {key: getattr(args, flag[2:].replace('-', '_')) for flag, key in IDENTITIES}
    try:
        if held:
            # A label holds before any author, evaluation, pulls or hand-triage input is read.
            record, code = check(None, None, args.label, None, owner_labels=args.owner_label, identity=identity)
        else:
            document = _read_author(args)
            evaluation, verifier = None, None
            # Stop at the first known security class; `check` writes the existing private hold.
            if not hold_triggers((), (), document, None)[1]:
                evaluation = _load_json(args.evaluation, 'evaluation')
                _, verifier = verifier_document(evaluation, validator())
            pulls, hand = None, None
            if not hold_triggers((), (), document, verifier)[1]:
                if args.pulls is not None:
                    pulls = _load_json(args.pulls, 'pulls')
                    if not isinstance(pulls, list) or not all(isinstance(n, int) and not isinstance(n, bool) and n > 0
                                                              for n in pulls):
                        raise Refusal('input', f'pulls {args.pulls}: not a JSON list of pull request numbers')
                    pulls = set(pulls)
                hand = _load_json(args.hand_triage, 'hand triage') if args.hand_triage else None
            record, code = check(
                document, evaluation, args.label, GitReach(args.repo, args.main, pulls),
                owner_labels=args.owner_label, hand_triage=hand, identity=identity)
    except Usage as usage:
        parser.print_usage(sys.stderr)
        print(f'backlog-check-triage: error: {usage}', file=sys.stderr)
        return EXIT_USAGE
    except Refusal as refusal:
        print(f'backlog-check-triage: refused: {refusal}', file=sys.stderr)
        return EXIT_REFUSED
    print(json.dumps(record, indent=2, ensure_ascii=False))
    if code == EXIT_HELD:
        print('backlog-check-triage: held: security hold; publish nothing about this issue', file=sys.stderr)
    return code


if __name__ == '__main__':
    sys.exit(main())
