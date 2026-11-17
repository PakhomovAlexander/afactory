"""Full native release implementation. Imported by the external worker only."""
import os
from pathlib import Path
from types import SimpleNamespace

import build
import publish
import report
import scenario as runtime
from perflab import contracts, executor, release_comparison
from perflab.values import ContractError, canonical_digest, require, sha256_bytes

OPS = runtime.OPS


class Native:
    def __init__(self, service):
        self.service = service
        self.config = build.load_config(service.config['nightly_config'])
        self.config['report'] = report.load_config(service.config['nightly_config'])['report']
        require(self.config['repository'] == service.config['repository'] and 'report' in self.config
                and 'publish' in self.config, 'nightly configuration lacks scoped report/publication')
        require(self.config['report']['executor_config'] == service.config['executor_config'], 'executor differs')
        runtime.trusted_key(self.config['public_key'])
        self.executor = executor.load_config(service.config['executor_config'])
        require(self.executor['backend'] == 'local' and self.executor['repository'] == self.config['repository']
                and self.executor['core']['tier'] == 'nightly' and not self.executor['core']['cases'],
                'release needs the full native local nightly family')

    @staticmethod
    def source_key(git, head):
        embedded = git.read_file(head, 'crates/af/keys/nightly.pub')
        require(embedded == (runtime.ROOT / 'crates/af/keys/nightly.pub').read_bytes(),
                'chosen build source does not carry the authorized nightly public identity')

    def record(self, tag):
        date = tag[8:].replace('.', '-')
        record = contracts.read_file(str(Path(self.config['builds_root']) / date / 'nightly.json'), 'nightly-build')
        require(record['tag'] == tag and record['date'] == date, 'build belongs to another night')
        require(record['build_image'] == self.config['build_image'], 'build used another pinned image')
        require(self.config.get('baseline_ref') in (None, record['baseline_tag']), 'build uses another baseline tag')
        self.source_key(build.Mirror(self.config['mirror']), record['head_sha'])
        return record

    def comparison_id(self, tag):
        return release_comparison.comparison_id({'kind': 'release', 'key': tag})

    def bundle(self, tag):
        return self.service.root / ('comparison-' + tag)

    def verified(self, record):
        manifest, comparison = release_comparison.verify_export(str(self.bundle(record['tag'])))
        require(comparison['comparison_id'] == self.comparison_id(record['tag'])
                and comparison['submission'] == {'kind': 'release', 'key': record['tag']}
                and comparison['sources']['baseline_sha'] == record['baseline_sha']
                and comparison['sources']['candidate_sha'] == record['head_sha'], 'comparison source identity differs')
        require(comparison['status'] == 'complete' and comparison['verification'] == 'verified'
                and comparison['performance']['core_verdict'] in ('pass', 'fail')
                and comparison['members'] and all(m['state'] == 'complete' and m['verification'] == 'verified'
                    for m in comparison['members'])
                and {lane['lane'] for lane in comparison['lanes']} == set(release_comparison.LANES)
                and all(lane['state'] in ('complete', 'not_applicable') for lane in comparison['lanes']),
                'missing, failed, unverified or incomplete native family; publication blocked')
        base = self.bundle(record['tag']) / 'evidence/comparisons' / comparison['comparison_id']
        declaration = contracts.read_file(str(base / 'comparison.json'), 'comparison')
        require(declaration['configuration_digest'] == canonical_digest(self.executor)
                and comparison['journal_id'] == self.executor['journal_id']
                and comparison['machine_id'] == self.executor['machine_id']
                and comparison['sources']['control_sha'] == self.executor['control_sha'], 'comparison authority differs')
        verdict = contracts.read_file(str(base / 'aggregate/report/verdict.json'), 'verdict')
        require(verdict['provenance'] == 'measured' and verdict['aggregate']['result'] == comparison['performance']['core_verdict']
                and verdict['aggregate']['coverage']['missing_records'] == 0, 'core lacks complete measured evidence')
        return manifest, comparison

    def certificate(self, job, op, artifacts):
        completion = runtime.load(self.service.completion_path(job['nightly_tag'], op), 'nightly-completion-v1.json')
        require(canonical_digest(completion) == job['phases'][op]['evidence'] and completion['artifacts'] == artifacts,
                'phase evidence changed after Wait')

    @staticmethod
    def report_hashes(path):
        return {name: sha256_bytes(executor.read(str(path / file), 4 * 1024 * 1024))
            for name, file in (('daily', 'daily.json'), ('html', 'report.html'), ('digest', 'digest.md'))}

    def run(self, job, op, resumed):
        try:
            return self.run_phase(job, op, resumed)
        except (build.Refused, report.Refused, report.Unavailable, publish.Refused, publish.Failed) as error:
            raise ContractError(str(error)) from error

    def run_phase(self, job, op, resumed):
        tag = job['nightly_tag']
        date = tag[8:].replace('.', '-')
        if op == OPS[0]:
            path = Path(self.config['builds_root']) / date / 'nightly.json'
            if not path.exists():
                require(not resumed, 'build interrupted without a sealed record; no duplicate build')
                result = build.main(['build', '--config', self.service.config['nightly_config'], '--date', date],
                                    source_check=self.source_key)
                require(result == 0, 'nightly build refused or skipped; no successful release')
            record = self.record(tag)
            require(record['status'] == 'built', 'no new build; no successful release completion')
            return {'build': canonical_digest(record)}, 'built exact source ' + record['head_sha']
        record = self.record(tag)
        artifacts = {'build': canonical_digest(record)}
        self.certificate(job, OPS[0], artifacts)
        require(record['status'] == 'built', 'no built night')
        if op == OPS[1]:
            ident = self.comparison_id(tag)
            if not self.bundle(tag).exists():
                with executor.Journal(self.executor) as journal:
                    declaration = Path(journal.root) / 'comparisons' / ident / 'comparison.json'
                    if not declaration.exists():
                        launch = release_comparison.preflight(journal, record['baseline_sha'], record['head_sha'], tag, 'release', launch=True)
                        require(launch['status'] == 'launched', 'native comparison launch refused: ' + launch['detail'])
                        held = launch['comparison']
                    else:
                        held = contracts.read_file(str(declaration), 'comparison')
                        require(held['sources']['baseline_sha'] == record['baseline_sha']
                                and held['sources']['candidate_sha'] == record['head_sha']
                                and held['configuration_digest'] == canonical_digest(self.executor), 'retained comparison changed')
                    require(held['submission'] == {'kind': 'release', 'key': tag}, 'comparison submission differs')
                    release_comparison.advance(journal, ident)
                    status = release_comparison.status(journal, ident)
                    if status['status'] == 'running':
                        allowed = {item['request_id'] for item in held['partitions']}
                        allowed.add(held['telemetry']['request_id'])
                        allowed.update(item['request_id'] for item in held['diagnostics'])
                        require(all(journal.state(i)['state'] != 'queued' or i in allowed for i in journal.names()),
                                'unrelated queued original in nightly executor; no unrelated work authorized')
                        reserve = (self.executor['run_seconds'] + self.executor['cycle_seconds']) * 1000
                        require(runtime.now_ms() + reserve <= job['deadline_unix_ms'],
                                'remaining nightly wall budget cannot admit a complete native cycle')
                        journal.cycle()
                        return None, 'full native family in progress: ' + ident
                    checked = release_comparison.status(journal, ident, verify=executor.verify_original)
                    release_comparison.export(journal, ident, str(self.bundle(tag)))
                    require(checked['status'] == 'complete' and checked['verification'] == 'verified', 'incomplete comparison; evidence retained')
            manifest, comparison = self.verified(record)
            artifacts.update(manifest=canonical_digest(manifest), comparison=canonical_digest(comparison))
            return artifacts, 'verified full native core ' + comparison['performance']['core_verdict']
        manifest, comparison = self.verified(record)
        artifacts.update(manifest=canonical_digest(manifest), comparison=canonical_digest(comparison))
        self.certificate(job, OPS[1], artifacts)
        output = self.service.root / ('report-' + tag)
        if op == OPS[2]:
            if not output.exists():
                pending = self.service.root / ('report-' + tag + '.pending')
                require(not pending.exists(), 'interrupted report retained; inspect before retry')
                core, telemetry = self.report_inputs(record, comparison)
                doc = report.compose(self.config, date, report.GitHub(self.config['repository'], build.read_token(self.config)),
                    report.Host(), runtime.now_ms, comparison_core=core, comparison_telemetry=telemetry)
                require(doc['performance'] is not None and doc['performance']['provenance'] == 'measured'
                        and doc['performance']['result'] in ('pass', 'fail'), 'report lacks measured comparison')
                report.write(doc, output=str(pending))
                os.rename(pending, output)
                executor.sync_dir(str(output.parent))
            daily = contracts.read_file(str(output / 'daily.json'), 'nightly-daily')
            require(daily['tag'] == tag and daily['build']['head_sha'] == record['head_sha']
                    and daily['baseline']['sha'] == record['baseline_sha'] and daily['baseline']['tag'] == record['baseline_tag']
                    and daily['performance'] is not None and daily['performance']['request_id'] == comparison['comparison_id']
                    and daily['performance']['result'] == comparison['performance']['core_verdict'], 'daily report differs')
            artifacts.update(self.report_hashes(output))
            return artifacts, 'report composed from verified full native comparison'
        require(op == OPS[3], 'unknown release phase')
        artifacts.update(self.report_hashes(output))
        self.certificate(job, OPS[2], artifacts)
        runtime.trusted_key(self.config['public_key'])
        receipt_path = Path(self.config['builds_root']) / date / 'publish.json'
        if receipt_path.exists():
            receipt = contracts.read_file(str(receipt_path), 'nightly-publish')
        else:
            require(not resumed, 'publication uncertain without receipt; no repeated mutation authorized')
            args = SimpleNamespace(date=date, report=str(output), notify_again=False)
            receipt, failures = publish.publish(self.config, args, None, publish.https_send, publish.run_command, runtime.now_ms)
            require(not failures, 'publication has failed notification/pruning steps; receipt retained')
        github = publish.GitHub(self.config['repository'], publish.read_secret(
            self.config['publish']['github_token_file'], 'github_token_file', r'^[^\s]+$'))
        publish.reconcile(self.config, record, receipt, str(output), github, publish.run_command)
        return {'build': canonical_digest(record), 'comparison': canonical_digest(comparison),
                'publication': canonical_digest(receipt)}, 'published signed nightly with full verified comparison'

    def report_inputs(self, record, comparison):
        ident = comparison['comparison_id']
        root = self.bundle(record['tag']) / 'evidence'
        family = root / 'comparisons' / ident
        declaration = contracts.read_file(str(family / 'comparison.json'), 'comparison')
        plan = contracts.read_file(str(family / 'plan.json'), 'run')
        verdict = contracts.read_file(str(family / 'aggregate/report/verdict.json'), 'verdict')
        doc = contracts.read_file(str(family / 'aggregate/report/report.json'), 'report')
        result = contracts.read_file(str(root / 'requests' / declaration['partitions'][0]['request_id'] / 'result.json'), 'executor-result')
        request = dict(request_id=ident, sources=comparison['sources'],
            submission=dict(baseline=dict(tag=record['baseline_tag'], sha=record['baseline_sha'])))
        verdict_hash = sha256_bytes(executor.read(str(family / 'aggregate/report/verdict.json')))
        tel_id = declaration['telemetry']['request_id']
        tel_request = contracts.read_file(str(root / 'requests' / tel_id / 'request.json'), 'executor-request')
        view = report.Journal(self.executor['root'], self.executor['journal_id'], self.executor['repository'])
        telemetry = view.telemetry_figures(record, tel_request)
        return (request, result, plan, verdict, verdict_hash, doc), telemetry
