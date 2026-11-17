"""Closed current-release documents for the asynchronous native nightly service."""
from perflab.spec import Const, DIGEST, Field, Int, Map, Nullable, Obj, Str, json_schema
from perflab.nightly_contracts import TAG_PATTERN, night_date
from perflab.values import require

OPERATIONS = ('nightly_build_v1', 'nightly_compare_v1', 'nightly_report_v1', 'publish_nightly_v1')
CONDITIONS = dict(zip(OPERATIONS, ('nightly_built', 'nightly_compared', 'nightly_reported', 'nightly_published')))
TEXT = Str(min_len=1, max_len=4096)
PATH = Str(pattern=r'^/[A-Za-z0-9_./-]+$', max_len=4096)
TAG = Str(pattern=TAG_PATTERN, min_len=18, max_len=18)
TIME = Int(1, 9007199254740991)
SERVICE = Obj(Field('schema', Const('af/nightly-service@1')), Field('root', PATH),
    Field('repository', Str(pattern=r'^[A-Za-z0-9_.-]{1,100}/[A-Za-z0-9_.-]{1,100}$')),
    Field('nightly_config', PATH), Field('nightly_config_digest', DIGEST),
    Field('executor_config', PATH), Field('executor_config_digest', DIGEST),
    Field('max_jobs', Int(1, 365)), Field('max_job_wall_ms', Int(60000, 82800000)),
    Field('max_bytes', Int(1048576, 2**50)))
PHASE = Obj(Field('intent_id', DIGEST), Field('input_digest', DIGEST), Field('inputs_digest', DIGEST),
    Field('state', Str(enum=('queued', 'running', 'done', 'failed', 'uncertain'))),
    Field('started_unix_ms', Nullable(TIME)), Field('detail', TEXT), Field('evidence', Nullable(DIGEST)))
JOB = Obj(Field('schema', Const('af/nightly-job@1')), Field('nightly_tag', TAG),
    Field('configuration_digest', DIGEST), Field('created_unix_ms', TIME), Field('deadline_unix_ms', TIME),
    Field('phases', Map(r'^(nightly_build_v1|nightly_compare_v1|nightly_report_v1|publish_nightly_v1)$', PHASE, max_props=4)))
COMPLETION = Obj(Field('schema', Const('af/nightly-completion@1')), Field('nightly_tag', TAG),
    Field('configuration_digest', DIGEST), Field('operation', Str(enum=OPERATIONS)),
    Field('artifacts', Map(r'^[a-z_]+$', DIGEST, max_props=8)), Field('detail', TEXT))
NODES = {'nightly-service-v1.json': SERVICE, 'nightly-job-v1.json': JOB, 'nightly-completion-v1.json': COMPLETION}


def schemas():
    return {name: json_schema('urn:af:schema:' + name[:-5], name[:-5],
        'Native nightly asynchronous service; admission binds exact provenance.', node) for name, node in NODES.items()}


def validate(value, name):
    NODES[name].validate(value, name)
    if 'nightly_tag' in value:
        night_date(value['nightly_tag'][8:].replace('.', '-'))
    if name == 'nightly-job-v1.json':
        require(value['created_unix_ms'] < value['deadline_unix_ms'], 'invalid job deadline')
        phases = value['phases']
        for index, op in enumerate(OPERATIONS):
            if op in phases:
                phase = phases[op]
                require((phase['state'] == 'done') == (phase['evidence'] is not None), 'completion evidence missing')
                require(phase['state'] != 'queued' or phase['started_unix_ms'] is None, 'queued phase already started')
                require(index == 0 or phases.get(OPERATIONS[index-1], {}).get('state') == 'done', 'incomplete predecessor')
    return value
