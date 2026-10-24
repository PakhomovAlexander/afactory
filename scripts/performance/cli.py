#!/usr/bin/env python3
"""AF continuous performance laboratory (packages p1, p2, p3a, p3b, p4, p5, p6a, p6b, p7).

    python3 scripts/performance/cli.py list --suite fixtures/performance/suite.toml
    python3 scripts/performance/cli.py plan --base REF --head REF --tier quick ... \
        [--build-image IMAGE@sha256:... | --trusted-host-build]
    python3 scripts/performance/cli.py build --plan run.json [--repo .]
    python3 scripts/performance/cli.py run --plan run.json --build DIR --shard I --run-dir DIR \
        (--trusted-local --scratch DIR | --container --image sha256:... --scratch-fs MOUNT)
    python3 scripts/performance/cli.py aggregate --run DIR [--shard-bundle DIR...]
    python3 scripts/performance/cli.py compare --run DIR [--output verdict.json]
    python3 scripts/performance/cli.py golden [--write]
    python3 scripts/performance/cli.py capabilities [--scratch PATH]
    python3 scripts/performance/cli.py probe --trusted-local --build DIR --side SIDE \
        --instance ID --scratch DIR [--trace-output PATH]
    python3 scripts/performance/cli.py judge --request PATH --stdout PATH --exit-code N
    python3 scripts/performance/cli.py report --run DIR --output DIR [--job-summary PATH]
    python3 scripts/performance/cli.py profile --plan run.json (--case ID... | --verdict PATH)
        --recipe ID --output DIR [--build DIR | --no-build] [--parent-build DIR]
        [--perf PATH] [--inferno-bin DIR] [--samply PATH] [--valgrind-bin DIR]
        [--telemetry spans|content]
    python3 scripts/performance/cli.py telemetry-run --profile ID --trigger T --baseline-af PATH \
        --candidate-af PATH --baseline-sha SHA --candidate-sha SHA --authority-sha SHA \
        --output DIR
    python3 scripts/performance/cli.py telemetry-verify --result DIR [--summary]
    python3 scripts/performance/cli.py telemetry-history --root DIR [--import DIR...]
    python3 scripts/performance/cli.py telemetry-schedule --event-name NAME --sha SHA ...
    python3 scripts/performance/cli.py report --run DIR --output DIR [--diagnostic DIR...]
    python3 scripts/performance/cli.py ci-schedule --event-name NAME --event PATH --sha SHA ...
    python3 scripts/performance/cli.py ci-pack|ci-unpack|ci-stage|ci-aggregate|ci-credentials|
        ci-scratch ...
    python3 scripts/performance/cli.py ci-release-handoff --event-name NAME --event PATH ...
    python3 scripts/performance/cli.py ci-release-confirm --handoff DIR ...
    python3 scripts/performance/cli.py cohort-declare --authority SHA --cohort-id ID --tier T \
        --environment ID --recipe ID --group ID 'CPU MODEL' 'IMAGE' --start-date YYYY-MM-DD \
        --repository OWNER/NAME --after-run-number N [--branch main] [--days N] \
        --output cohort.json
    python3 scripts/performance/cli.py plan --scope calibration --authority SHA --cohort PATH \
        --member ID [--calibration PATH] ...
    python3 scripts/performance/cli.py calibration-prepare [--repo DIR] --cohort PATH \
        --evidence DIR --output calibration-parameters.json
    python3 scripts/performance/cli.py readiness [--repo DIR] --cohort PATH --calibration PATH \
        --evidence DIR --output readiness.json [--job-summary PATH]
    python3 scripts/performance/cli.py activate --verify [--manifest PATH] [--repo DIR] \
        [--revision REV] [--output PATH]
    python3 scripts/performance/cli.py activate --verify-proposed --base SHA --revision SHA \
        --repository OWNER/NAME [--token-env VAR] [--output PATH]
    python3 scripts/performance/cli.py equivalence-evidence --calibrated-activation PATH \
        --authority SHA --group ID --adapter-file PATH... --golden DIR=RUN \
        --post-merge-aa DIR=RUN --output equivalence.json
    python3 scripts/performance/cli.py cost-report --run DIR[=cold|warm]... --output cost.json
    python3 scripts/performance/cli.py shipping-report --suite PATH --run DIR... \
        --output shipping.json
    python3 scripts/performance/cli.py measure --case ID --metric KEY   (staged bootstrap only)
    python3 scripts/performance/cli.py harness-digest [--root DIR] [--check | --write] [--json]
    python3 scripts/performance/cli.py history --inputs DIR... --output DIR
    python3 scripts/performance/cli.py archive --root DIR (--init [--capacity-bytes N] |
        --import RUN_DIR | --fetch ACTIONS_RUN_ID --repository OWNER/NAME | --retain |
        --pin RUN_ID --reason REASON | --supersede RUN_ID | --status)

`list` and `plan` never build or execute anything; `compare` recomputes a verdict from
retained evidence and never adds a sample. `plan` also freezes how a Linux recipe without its
own image pin builds: in an image configured with `--build-image`, or as an explicitly trusted
host process with `--trusted-host-build`. `golden` checks (or rewrites) the native oracle's
golden entries from the independent reference generator; it never runs the product. `build`
builds a frozen plan's products and its one authority supervisor below the checkout's
target/performance/<run>, exactly as the plan froze them, and writes builds.json beside the
hashed artifacts, recording the run's execution start and total deadline; it runs no
sample. `run` executes exactly one frozen shard of a plan from a verified build, in the plan's
environment on this host, and writes that shard's bundle below the run directory; it never
rebuilds, retries or reorders. `aggregate` imports shard bundles uploaded from other hosts and
recomputes the verdict over every planned slot. `capabilities` reports what this host can
execute and why anything is unavailable.
`probe` runs one native probe sample, in explicitly trusted local mode, under the authority
supervisor and judges it against the golden entries; `judge` judges a recorded probe
execution. Neither writes evidence or makes a speed claim. `report` recomputes a run's verdict
and writes its self-contained offline report, matching JSON and job-summary table below a new
directory; it exits with the verdict's code. `history` writes compatible history across run
directories and archive roots, each run's report beside it. `archive` is the trusted collector's
content-addressed filesystem archive: it imports a verified run directory, fetches a run's
Actions transport artifacts with a read-only token named by --token-env, applies retention and
pins, and reports its health; it never executes anything it imports. Exit codes: 0 pass or
success, 2 invalid input or impossible plan, 3 fail, 4 incomplete evidence or unavailable
baseline, authority, toolchain, containment or archive health. `profile` creates one separate,
bounded diagnostic child run of a comparison (ADR-0174): it freezes a child plan linked to its
parent, selects explicit cases or the parent verdict's regressed then inconclusive ones, builds
the diagnostic recipe, and records CPU profiles with flamegraphs, phase traces with their
overhead, or counters, of native or CLI cases, each with what it observed or why it is
unavailable and how to reproduce it, in the explicitly selected trusted local or contained Linux
mode (ADR-0176); it never changes the parent's verdict. With `--telemetry spans|content` it
also records each eligible CLI case once per side at that level and reads it back through the
side's own `af telemetry` documents (ADR-0199). It exits 4 when anything requested is
unavailable. `probe --trace-output` writes a perf-trace probe's validated phase trace and its
region breakdown, a diagnostic never judged. `telemetry-run` measures Telemetry's own cost and
read latency on a baseline and a candidate binary with this checkout's bench scripts, advisory
and never a verdict; `telemetry-verify` reads its result bundle back offline and recomputes it;
`telemetry-history` keeps verified results in a durable root and draws their compatible history;
`telemetry-schedule` resolves one event of the Performance telemetry workflow (ADR-0199). The
`ci-*` commands are the continuous workflow's steps (ADR-0177): `ci-schedule`
resolves one Actions event from the trusted control and freezes its comparisons; `ci-pack` and
`ci-unpack` move one verified build to its shards with its modes; `ci-stage` writes a shard's
evidence as its transport artifact; `ci-aggregate` imports every arrived shard and reports;
`ci-credentials` refuses credentials where none may be; `ci-scratch` prepares a hosted runner's
loop-mounted sample scratch. `ci-release-handoff` verifies a completed upstream Release run with
a read-only token and decides, from the claim history, whether this run claims its release;
`ci-release-confirm` measures only when the written claim is the first valid one (ADR-0180).
`activate --verify` checks an activation manifest from its reviewed records alone, as every
active plan does (ADR-0183). `activate --verify-proposed` is the trusted activation workflow's
step (ADR-0184): run from the pull request's base revision, it compares the proposed activation
with that base and, when the manifest or a record it names changed, fetches the cohort's
retained Actions evidence (and a bridge's two runs) with a read-only token and recomputes
readiness and the bridge's evidence from it; an unchanged activation is verified locally. Both
activate nothing and exit 0 only when the activation verifies. `equivalence-evidence` computes a
bridge's execution evidence from its golden-check and post-merge A/A runs and exits 0 only when
both verify. `cost-report` and `shipping-report` compute the cost and shipping-budget
demonstrations from retained runs and exit 0 only when measured evidence demonstrates them.
`measure` is one repetition of a guarded kernel experiment (ADR-0185), reached only through the
literal bootstrap a captured code policy runs after it verified the protected harness: it builds
the probe offline into the kernel's target, runs one judged nonspawning library sample and
prints one `af.measure-report/1` line, or exits nonzero with no report. `harness-digest` prints
the protected harness digest; `--check` (repository checks and release preparation) requires the
staged policy, its inventory and any installed guarded measure to match it, and `--write`
re-pins the staged policy for a reviewed human installation.
"""
import argparse
import json
import os
import sys
import time

sys.dont_write_bytecode = True
sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))

from perflab import activation, archive, builds, bundle, calibration  # noqa: E402
from perflab import capability, contracts, costs  # noqa: E402
from perflab import diagnostics  # noqa: E402
from perflab import buildpack, ci, credentials, gitrefs, hostprep  # noqa: E402
from perflab import history, kernelguard, kernelmeasure, oracle, phasetrace, planning  # noqa: E402
from perflab import reference, render, telemetrycollect, telemetrylab  # noqa: E402
from perflab import release, report, runner  # noqa: E402
from perflab import transport  # noqa: E402
from perflab import suite as suite_module  # noqa: E402
from perflab.compare import InvalidInput, compare  # noqa: E402
from perflab.values import ContractError, document_bytes  # noqa: E402

EXIT_INVALID = 2
EXIT_FAIL = 3
EXIT_UNAVAILABLE = 4


def _emit(data, output):
    if output:
        with open(output, "wb") as stream:
            stream.write(data)
    else:
        sys.stdout.buffer.write(data)
        sys.stdout.flush()


def _refuse(status, reason, detail):
    doc = {"schema": contracts.REFUSAL_SCHEMA, "status": status, "reason": reason,
           "detail": detail[:4096]}
    sys.stdout.buffer.write(contracts.write_bytes(doc, "plan-refusal"))
    sys.stderr.write("refused (%s): %s\n" % (reason, detail))
    return EXIT_INVALID if status == "invalid" else EXIT_UNAVAILABLE


def cmd_list(args):
    try:
        loaded = suite_module.load(args.suite)
    except (ContractError, OSError) as error:
        sys.stderr.write("invalid registry: %s\n" % error)
        return EXIT_INVALID
    doc = loaded.doc
    if doc["probe"]["state"] == "supported":
        try:
            oracle.check_coverage(doc, oracle.load_golden(oracle.golden_path(args.suite, doc)))
        except (ContractError, OSError) as error:
            sys.stderr.write("invalid golden entries: %s\n" % error)
            return EXIT_INVALID
    tiers = {}
    for tier in sorted(doc["tiers"]):
        rows = []
        for item in loaded.instances(tier):
            case = doc["cases"][item["case_id"]]
            rows.append({"instance_id": item["instance_id"], "case_id": item["case_id"],
                         "criticality": item["criticality"], "pairs": case["pairs"][tier],
                         "execution": case["execution"]["state"],
                         "oracle": case["oracle"]["state"]})
        tiers[tier] = rows
    summary = {"schema": "af.perf-list/1", "suite_id": doc["suite_id"],
               "suite_sha256": loaded.raw_sha256, "policy_digest": loaded.policy_digest,
               "capabilities": {
                   "recipes": dict((key, value["state"]) for key, value in
                                   sorted(doc["recipes"].items())),
                   "environments": dict((key, value["state"]) for key, value in
                                        sorted(doc["environments"].items())),
                   "budgets": dict((key, value["state"]) for key, value in
                                   sorted(doc["budgets"].items()))},
               "tiers": tiers}
    if args.json:
        sys.stdout.write(json.dumps(summary, indent=2, sort_keys=True) + "\n")
        return 0
    print("%s  policy %s" % (doc["suite_id"], loaded.policy_digest))
    for tier, rows in tiers.items():
        critical = sum(1 for row in rows if row["criticality"] == "critical")
        print("\n%s: %d instances, K = %d critical" % (tier, len(rows), critical))
        for row in rows:
            print("  %-64s %-8s N=%-3d %s, oracle %s" % (
                row["instance_id"], row["criticality"], row["pairs"], row["execution"],
                row["oracle"]))
    return 0


def _load_json(path, name):
    with open(path, "rb") as stream:
        return contracts.read_json(stream.read(contracts.limits.MAX_JSON_BYTES + 1), name, path)


def cmd_plan(args):
    try:
        request = planning.Request(
            trigger=args.trigger, tier=args.tier, scope=args.scope, base=args.base,
            head=args.head, control=args.control, authority=args.authority,
            cases=args.case or [], shards=args.shards, pairs=args.pairs, seed=args.seed,
            run_id=args.run_id, parent_run_id=args.parent_run_id,
            environment=args.environment, pr_number=args.pr_number,
            pr_base_sha=args.pr_base_sha, pr_head_sha=args.pr_head_sha,
            remeasure_of=args.remeasure_of, member=args.member,
            cohort=_load_json(args.cohort, "cohort") if args.cohort else None,
            calibration=(_load_json(args.calibration, "calibration-parameters")
                         if args.calibration else None),
            parent_plan=_load_json(args.parent, "run") if args.parent else None,
            build_image=args.build_image, trusted_host_build=args.trusted_host_build)
        if args.scope != "diagnostic":
            if not args.head or not args.tier:
                raise ContractError("plan needs --head and --tier")
        result = planning.plan(gitrefs.Git(args.repo), request)
    except gitrefs.Unavailable as error:
        return _refuse("unavailable", error.reason, str(error))
    except (ContractError, OSError) as error:
        return _refuse("invalid", "invalid_plan", str(error))
    _emit(document_bytes(result), args.output)
    sys.stderr.write("frozen %s plan %s: %d instances, K = %d, %d shards\n" % (
        result["scope"], result["plan_digest"], len(result["cases"]),
        result["verdict_policy"]["family_size"], len(result["shards"])))
    return 0


def cmd_cohort_declare(args):
    """Declare a complete calibration cohort from the proposed authority's own registry."""
    try:
        git = gitrefs.Git(args.repo)
        authority = git.resolve(args.authority, "--authority")
        suite = planning.read_suite(git, authority)
        groups = [calibration.parse_group(text) for text in args.group]
        doc = calibration.declare(suite, authority, args.cohort_id, args.tier, args.environment,
                                  args.recipe, groups, args.start_date, args.repository,
                                  args.after_run_number, args.days, args.preparation_members,
                                  args.preparation_iterations, args.branch)
    except gitrefs.Unavailable as error:
        sys.stderr.write("unavailable: %s\n" % error)
        return EXIT_UNAVAILABLE
    except (ContractError, OSError) as error:
        sys.stderr.write("invalid cohort: %s\n" % error)
        return EXIT_INVALID
    _emit(contracts.write_bytes(doc, "cohort"), args.output)
    sys.stderr.write("declared cohort %s %s: %d members over %d days\n" % (
        doc["cohort_id"], contracts.canonical_digest(doc), len(doc["members"]),
        doc["schedule"]["minimum_days"]))
    return 0


def _collected(args, sealed=None):
    import tempfile
    cohort = _load_json(args.cohort, "cohort")
    git = gitrefs.Git(args.repo)
    replan = calibration.replanner(git, cohort, sealed)
    scratch = tempfile.mkdtemp(prefix="af-perf-calibration-")
    return cohort, scratch, calibration.collect(cohort, args.evidence, scratch, replan,
                                                calibration.producer_check(git, cohort))


def cmd_calibration_prepare(args):
    """Seal the calibration parameters from the cohort's A/A and preparation evidence."""
    import shutil
    scratch = None
    try:
        cohort, scratch, (runs, refused, _) = _collected(args)
        doc = calibration.prepare(cohort, runs, calibration.tolerances_of(runs, cohort))
    except (ContractError, OSError) as error:
        sys.stderr.write("invalid calibration evidence: %s\n" % error)
        return EXIT_INVALID
    finally:
        if scratch is not None:
            shutil.rmtree(scratch, ignore_errors=True)
    data = contracts.write_bytes(doc, "calibration-parameters")
    _emit(data, args.output)
    unavailable = sum(1 for group in doc["groups"] if group["iterations"] is None)
    sys.stderr.write("sealed calibration parameters %s: %d case/group entries, %d without a "
                     "loop count; %d dispatch(es) and artifact(s) refused\n"
                     % (contracts.canonical_digest(doc), len(doc["groups"]), unavailable,
                        len(refused)))
    return 0


def cmd_readiness(args):
    """Recompute a cohort's readiness from its retained evidence; exit 0 only when ready."""
    import shutil
    scratch = None
    try:
        sealed = _load_json(args.calibration, "calibration-parameters")
        cohort, scratch, (runs, refused, history) = _collected(args, sealed)
        doc = calibration.readiness(cohort, sealed, runs, refused, history)
    except (ContractError, OSError) as error:
        sys.stderr.write("invalid readiness evidence: %s\n" % error)
        return EXIT_INVALID
    finally:
        if scratch is not None:
            shutil.rmtree(scratch, ignore_errors=True)
    _emit(contracts.write_bytes(doc, "readiness"), args.output)
    if args.job_summary:
        with open(args.job_summary, "a", encoding="utf-8") as stream:
            stream.write(calibration.summary_markdown(doc) + "\n")
    sys.stderr.write("readiness of %s: %s (A/A %d/%d, power %d/%d whole trials)\n" % (
        doc["cohort_id"], "ready" if doc["ready"] else "not ready",
        doc["tallies"]["aa_within_tolerance"], doc["tallies"]["aa_members"],
        doc["tallies"]["power_qualifying"], doc["tallies"]["power_members"]))
    return 0 if doc["ready"] else EXIT_UNAVAILABLE


def cmd_activate(args):
    """Verify an activation manifest: `--verify` from its reviewed records alone, as every
    active plan does; `--verify-proposed` as the trusted activation workflow does, fetching and
    recomputing the retained evidence whenever the activation differs from `--base`. Exit 0
    only when it verifies."""
    import shutil
    import tempfile
    if not (args.verify or args.verify_proposed):
        sys.stderr.write("refused: activate only verifies (--verify or --verify-proposed); an "
                         "activation is a reviewed change, never written by this command\n")
        return EXIT_INVALID
    scratch = None
    try:
        git = gitrefs.Git(args.repo)
        revision = git.resolve(args.revision, "--revision")
        evidence = None
        if args.verify_proposed:
            if not args.base:
                raise ContractError("--verify-proposed needs --base, the trusted control "
                                    "revision the proposal is compared with")
            base = git.resolve(args.base, "--base")
            changed = activation.proposed_changes(git, base, revision, args.manifest)
            manifest, _ = activation.read_manifest(git, revision, args.manifest)
            if manifest is None:
                sys.stderr.write("no activation at %s; nothing activates, nothing to verify\n"
                                 % revision[:12])
                return 0
            if changed:
                if not args.repository:
                    raise ContractError("--verify-proposed of a changed activation needs "
                                        "--repository OWNER/NAME, where its cohort ran")
                sys.stderr.write("the activation changed (%s): fetching its retained evidence\n"
                                 % ", ".join(changed[:8]))
                cohort = contracts.read_json(
                    git.read_file(revision, manifest["cohort_path"]) or b"", "cohort",
                    manifest["cohort_path"])
                collector = transport.Collector(
                    transport.HttpsTransport(), args.repository,
                    transport.token_from(os.environ, args.token_env), api_url=args.api_url)
                scratch = tempfile.mkdtemp(prefix="af-perf-activation-")
                evidence = activation.fetch_evidence(collector, cohort,
                                                     os.path.join(scratch, "evidence"))
                if manifest["admission"]["kind"] == "equivalent_adapter":
                    record = contracts.read_json(
                        git.read_file(revision, manifest["admission"]["equivalence_path"])
                        or b"", "equivalence", manifest["admission"]["equivalence_path"])
                    activation.fetch_bridge_runs(collector, record, evidence)
                os.makedirs(os.path.join(scratch, "runs"))
            else:
                sys.stderr.write("the activation is unchanged from %s: verified locally, its "
                                 "cohort not fetched again\n" % base[:12])
        verified = activation.verify(git, revision, args.manifest, evidence,
                                     None if scratch is None else os.path.join(scratch, "runs"))
    except gitrefs.Unavailable as error:
        sys.stderr.write("unavailable: %s\n" % error)
        return EXIT_UNAVAILABLE
    except (ContractError, OSError) as error:
        sys.stderr.write("activation refused: %s\n" % error)
        return EXIT_INVALID
    finally:
        if scratch is not None:
            shutil.rmtree(scratch, ignore_errors=True)
    _emit(contracts.write_bytes(verified.doc, "activation-verification"), args.output)
    if args.job_summary:
        with open(args.job_summary, "a", encoding="utf-8") as stream:
            stream.write(activation.summary_markdown(verified.doc) + "\n")
    sys.stderr.write("verified %s activation %s of cohort %s (%s): calibrated %s on %s; "
                     "pending %s\n" % (verified.doc["admission"], verified.sha256,
                                       verified.doc["cohort_id"], verified.doc["mode"],
                                       verified.doc["environment_id"],
                                       ", ".join(verified.doc["calibrated_groups"]),
                                       ", ".join(verified.doc["pending"])))
    return 0


def cmd_equivalence_evidence(args):
    """Compute a bridge's execution evidence from its golden-check and post-merge A/A runs;
    exit 0 only when both verify."""
    try:
        git = gitrefs.Git(args.repo)
        revision = git.resolve(args.revision, "--revision")
        base, _ = activation.read_manifest(git, revision, args.calibrated_activation)
        if base is None:
            raise ContractError("--calibrated-activation: %s is not in revision %s"
                                % (args.calibrated_activation, revision[:12]))
        cohort = contracts.read_json(git.read_file(revision, base["cohort_path"]) or b"",
                                     "cohort", base["cohort_path"])
        bridged = git.resolve(args.authority, "--authority")
        admission = {"equivalence_group": args.group, "adapter_files": args.adapter_file}
        ctx = activation.equivalence_module.context(
            cohort, base, admission, bridged, activation._adapters(git, bridged))
        runs = {}
        for role, text in (("golden", args.golden), ("post_merge_aa", args.post_merge_aa)):
            path, sep, number = text.rpartition("=")
            if not sep or not path or not number.isdigit():
                raise ContractError("--%s: RUN_DIR=ACTIONS_RUN_ID, not %r"
                                    % (role.replace("_", "-"), text))
            runs[role] = (path, int(number))
        doc = activation.equivalence_module.compute(ctx, runs)
    except gitrefs.Unavailable as error:
        sys.stderr.write("unavailable: %s\n" % error)
        return EXIT_UNAVAILABLE
    except (ContractError, OSError) as error:
        sys.stderr.write("invalid equivalence evidence: %s\n" % error)
        return EXIT_INVALID
    _emit(contracts.write_bytes(doc, "equivalence"), args.output)
    for line in activation.equivalence_module.summary(doc):
        sys.stderr.write(line + "\n")
    return 0 if doc["verified"] else EXIT_UNAVAILABLE


def cmd_cost_report(args):
    """The cost demonstration of retained runs; exit 0 only when it is demonstrated."""
    entries = []
    try:
        for item in args.run:
            path, sep, cache = item.rpartition("=")
            if not sep:
                # The cache is read from the run's build manifest (ADR-0184).
                entries.append((item, None))
                continue
            if not path:
                raise ContractError("--run: DIR, DIR=cold or DIR=warm, not %r" % item)
            entries.append((path, cache))
        doc = costs.cost_report(entries)
    except (ContractError, OSError) as error:
        sys.stderr.write("invalid cost evidence: %s\n" % error)
        return EXIT_INVALID
    _emit(contracts.write_bytes(doc, "cost"), args.output)
    if args.job_summary:
        with open(args.job_summary, "a", encoding="utf-8") as stream:
            stream.write(costs.summary_markdown(doc) + "\n")
    sys.stderr.write("cost demonstration: %s over %d runs\n" % (doc["status"], len(doc["runs"])))
    return 0 if doc["status"] == "demonstrated" else EXIT_UNAVAILABLE


def cmd_shipping_report(args):
    """The shipping-budget demonstration of retained runs; exit 0 only when demonstrated, 3
    when a sample missed its limit."""
    try:
        doc = costs.shipping_report(suite_module.load(args.suite), args.run)
    except (ContractError, OSError) as error:
        sys.stderr.write("invalid shipping evidence: %s\n" % error)
        return EXIT_INVALID
    _emit(contracts.write_bytes(doc, "shipping"), args.output)
    if args.job_summary:
        with open(args.job_summary, "a", encoding="utf-8") as stream:
            stream.write(costs.summary_markdown(doc) + "\n")
    sys.stderr.write("shipping budgets: %s over %d budget instances\n"
                     % (doc["status"], len(doc["budgets"])))
    return {"demonstrated": 0, "missed": EXIT_FAIL}.get(doc["status"], EXIT_UNAVAILABLE)


def cmd_compare(args):
    try:
        verdict = compare(args.run)
    except InvalidInput as error:
        sys.stderr.write("invalid input: %s\n" % error)
        return EXIT_INVALID
    except (ContractError, OSError) as error:
        sys.stderr.write("invalid input: %s\n" % error)
        return EXIT_INVALID
    _emit(document_bytes(verdict), args.output)
    aggregate = verdict["aggregate"]
    sys.stderr.write("%s (%s scope, %s evidence)%s\n" % (
        aggregate["result"], verdict["scope"], verdict["provenance"],
        "" if aggregate["satisfies_required_check"] else "; does not satisfy the required check"))
    for reason in aggregate["reasons"]:
        sys.stderr.write("  %s: %s\n" % (reason["code"], reason["detail"]))
    return aggregate["exit_code"]


def cmd_run(args):
    if args.trusted_local == args.container:
        sys.stderr.write("refused: choose exactly one of --trusted-local (macOS, a process "
                         "group, discovery and nonspawning library cases) and --container (a "
                         "pinned Linux sample container per sample)\n")
        return EXIT_INVALID
    try:
        options = runner.Options(
            plan=args.plan, build=args.build, shard=args.shard, run_dir=args.run_dir,
            repo=args.repo, mode=runner.TRUSTED_LOCAL if args.trusted_local
            else runner.LINUX_CONTAINER, scratch=args.scratch, docker=args.docker,
            image=args.image, scratch_fs=args.scratch_fs, deadline_unix_ms=args.deadline_unix_ms)
        doc = runner.run_shard(options)
    except runner.Unavailable as error:
        return _refuse("unavailable", error.reason, str(error))
    except (ContractError, OSError) as error:
        return _refuse("invalid", "invalid_run", str(error))
    print(os.path.join(args.run_dir, runner.SHARD_DIR, str(args.shard), runner.SHARD_FILE))
    sys.stderr.write("shard %d %s: %d of %d records, %d failed samples, %d unstarted%s\n" % (
        doc["shard"], doc["status"], doc["record_count"], doc["expected_records"],
        doc["slots"]["failed"], doc["slots"]["unstarted"],
        "; %s: %s" % (doc["failure"]["reason"], doc["failure"]["detail"])
        if doc["failure"] else ""))
    return 0 if doc["status"] == "complete" else EXIT_UNAVAILABLE


def _import_shard(run_dir, source, plan):
    """Copy one verified shard bundle into the run directory, never over a different one."""
    _, hashes = bundle.verify(source, plan["limits"]["shard_bundle_bytes"],
                              allowed=lambda path: path in (runner.SAMPLES, runner.SHARD_FILE)
                              or path.startswith(runner.LOGS + "/"), label=source)
    doc = contracts.read_json(bundle.read_member(source, runner.SHARD_FILE, hashes,
                                                 contracts.limits.MAX_JSON_BYTES, source),
                              "shard", os.path.join(source, runner.SHARD_FILE))
    if (doc["run_id"], doc["plan_digest"]) != (plan["run_id"], plan["plan_digest"]):
        raise ContractError("%s is a shard of another run or plan" % source)
    target = os.path.join(run_dir, runner.SHARD_DIR, str(doc["shard"]))
    if os.path.lexists(target):
        _, present = bundle.verify(target, plan["limits"]["shard_bundle_bytes"], label=target)
        if present != hashes:
            raise ContractError("%s already holds a different bundle of shard %d"
                                % (target, doc["shard"]))
        return doc["shard"], "already present"
    os.makedirs(os.path.join(run_dir, runner.SHARD_DIR), exist_ok=True)
    for relative in sorted(list(hashes) + [bundle.MANIFEST]):
        destination = os.path.join(target, relative)
        os.makedirs(os.path.dirname(destination), exist_ok=True)
        with open(os.path.join(source, relative), "rb") as stream:
            data = stream.read()
        with open(destination, "wb") as stream:
            stream.write(data)
    bundle.verify(target, plan["limits"]["shard_bundle_bytes"], label=target)
    return doc["shard"], "imported"


def cmd_aggregate(args):
    try:
        plan = _load_json(os.path.join(args.run, "run.json"), "run")
        for source in args.shard_bundle or []:
            index, what = _import_shard(args.run, source, plan)
            sys.stderr.write("shard %d %s from %s\n" % (index, what, source))
        verdict = compare(args.run)
    except InvalidInput as error:
        sys.stderr.write("invalid input: %s\n" % error)
        return EXIT_INVALID
    except (ContractError, OSError) as error:
        sys.stderr.write("invalid input: %s\n" % error)
        return EXIT_INVALID
    _emit(document_bytes(verdict), args.output)
    for entry in verdict["shards"]:
        sys.stderr.write("shard %d: %s, %d of %d planned records matched\n" % (
            entry["shard"], entry["status"] or "no evidence", entry["matched_records"],
            entry["expected_records"]))
    aggregate = verdict["aggregate"]
    sys.stderr.write("%s (%s scope, %s evidence)%s\n" % (
        aggregate["result"], verdict["scope"], verdict["provenance"],
        "" if aggregate["satisfies_required_check"] else "; does not satisfy the required check"))
    for reason in aggregate["reasons"]:
        sys.stderr.write("  %s: %s\n" % (reason["code"], reason["detail"]))
    return aggregate["exit_code"]


def cmd_golden(args):
    try:
        loaded = suite_module.load(args.suite)
        path = oracle.golden_path(args.suite, loaded.doc)
        generated = contracts.write_bytes(reference.golden_document(loaded.doc), "golden")
        if args.write:
            os.makedirs(os.path.dirname(path), exist_ok=True)
            with open(path, "wb") as stream:
                stream.write(generated)
            return 0
        with open(path, "rb") as stream:
            current = stream.read()
    except (ContractError, OSError) as error:
        sys.stderr.write("invalid golden input: %s\n" % error)
        return EXIT_INVALID
    if current != generated:
        sys.stderr.write("%s is not what perflab.reference derives; run python3 scripts/"
                         "performance/cli.py golden --write and review the change\n" % path)
        return 1
    return 0


def _report(result, stderr):
    sys.stdout.buffer.write(contracts.canonical_bytes(result) + b"\n")
    if stderr:
        sys.stderr.buffer.write(stderr[-4096:])
    sys.stderr.write("%s %s%s\n" % (result["instance_id"], result["status"], (
        ": %s: %s" % (result["failure"]["kind"], result["failure"]["detail"])
        if result["failure"] else "")))
    return 0 if result["status"] == "passed" else EXIT_FAIL


def _probe_binaries(args):
    """(supervisor, probe): from a verified build, rehashed now, or given explicitly."""
    if args.build:
        doc, paths = builds.verify_artifacts(args.build)
        side = args.side
        if side not in paths and side + "/probe" not in paths:
            raise ContractError("--side: one of " + ", ".join(contracts.SIDES))
        record = doc["sides"][side]["artifacts"]["probe"]
        if record is None:
            raise oracle.Unsupported("the %s build has no probe: %s" % (
                side, doc["sides"][side]["coverage_reason"]))
        supervisor = builds.verified_binary(paths["supervisor"],
                                            doc["supervisor"]["binary_sha256"])
        return supervisor, builds.verified_binary(paths[side + "/probe"], record["sha256"])
    if not (args.supervisor and args.binary):
        raise ContractError("probe needs --build and --side, or --supervisor and --binary")
    return os.path.abspath(args.supervisor), os.path.abspath(args.binary)


def cmd_probe(args):
    if not args.trusted_local:
        sys.stderr.write("refused: probe runs only in explicitly trusted local mode "
                         "(--trusted-local), whose process group is not Linux namespace "
                         "containment; Linux samples run in containers\n")
        return EXIT_INVALID
    try:
        doc, golden = oracle.load(args.suite)
        # An explicit timeout is checked before anything launches; only an omitted one takes
        # the case's declared value.
        timeout = (oracle.timeout_for(doc, args.instance) if args.timeout is None
                   else oracle.check_timeout(args.timeout))
        supervisor, binary = _probe_binaries(args)
        if args.trace_output and os.path.lexists(args.trace_output):
            raise ContractError("--trace-output: %s already exists" % args.trace_output)
        traces = [] if args.trace_output else None
        result, stderr = oracle.sample(doc, golden, supervisor, binary, args.instance,
                                       args.scratch, timeout, trace=traces)
        if traces is not None:
            _write_probe_trace(args.trace_output, traces[0] if traces else None)
    except oracle.Unsupported as error:
        sys.stderr.write("unavailable: %s\n" % error)
        return EXIT_UNAVAILABLE
    except (ContractError, OSError) as error:
        sys.stderr.write("invalid probe input: %s\n" % error)
        return EXIT_INVALID
    return _report(result, stderr)


def _write_probe_trace(path, data):
    """Validate the phase trace one probe sample exported and write it, with its phase summary
    on stderr: diagnostic evidence for an investigation, never a verdict sample (ADR-0199)."""
    if data is None:
        sys.stderr.write("no phase trace was exported: the probe is not a perf-trace build\n")
        return
    doc = phasetrace.load(data, path)
    with open(path, "xb") as stream:
        stream.write(data)
    sys.stderr.write("phase trace %s: %d spans, %d dropped, %d open\n"
                     % (path, len(doc["spans"]), doc["dropped"], doc["open"]))
    for row in phasetrace.phase_summary(doc):
        sys.stderr.write("  %-22s %6d spans  covered %12d ns  longest %12d ns\n"
                         % (row["phase"], row["spans"], row["covered_ns"], row["longest_ns"]))
    for line in phasetrace.region_breakdown(doc):
        sys.stderr.write(line + "\n")


def cmd_telemetry_run(args):
    try:
        root = telemetrylab.ROOT
        builds = {}
        for side in contracts.SIDES:
            path = getattr(args, side + "_build")
            builds[side] = None
            if path:
                af = getattr(args, side + "_af")
                builds[side] = telemetrylab.read_build(
                    path, side, getattr(args, side + "_sha"),
                    telemetrylab._binary(af, "--%s-af" % side)[1])
        result = telemetrylab.run(telemetrylab.Settings(
            profile=args.profile, trigger=args.trigger, baseline_af=args.baseline_af,
            candidate_af=args.candidate_af, baseline_sha=args.baseline_sha,
            candidate_sha=args.candidate_sha, authority_sha=args.authority_sha,
            experiments=args.experiment, rounds=args.rounds, pairs=args.pairs, scale=args.scale,
            corpus_runs=args.corpus_runs, output=args.output, work=args.work,
            result_id=args.result_id, root=root, mode=args.mode, docker=args.docker,
            image=args.image, scratch_fs=args.scratch_fs, builds=builds))
    except telemetrylab.Unavailable as error:
        return _refuse("unavailable", error.reason, str(error))
    except (ContractError, OSError) as error:
        return _refuse("invalid", "invalid_telemetry_run", str(error))
    sys.stderr.write(telemetrylab.headline(result) + "\n")
    print(os.path.join(os.path.abspath(args.output), telemetrylab.RESULT_JSON))
    # Advisory: a measured result exits 0 whatever it measured; 4 when nothing succeeded, a
    # cleanup was not confirmed or protected state changed.
    unsafe = (result["execution"]["cleanup"] != "confirmed" or result["execution"]["problems"])
    return EXIT_UNAVAILABLE if result["status"] == "failed" or unsafe else 0


def cmd_telemetry_schedule(args):
    try:
        event = {}
        if args.event:
            with open(args.event, "rb") as stream:
                event = json.loads(stream.read(contracts.limits.MAX_JSON_BYTES + 1))
        inputs = json.loads(args.inputs) if args.inputs else {}
        doc = telemetrylab.schedule(gitrefs.Git(args.repo), args.event_name, event, inputs,
                                    args.sha)
    except (ContractError, OSError, ValueError, KeyError, TypeError) as error:
        return _refuse("invalid", "invalid_telemetry_event", str(error))
    text = telemetrylab.schedule_outputs(doc)
    if args.github_output:
        with open(args.github_output, "a") as stream:
            stream.write(text)
    else:
        sys.stdout.write(text)
    if args.job_summary:
        with open(args.job_summary, "a") as stream:
            stream.write("### AF telemetry experiment (advisory): %s\n\n%s\n" % (
                doc["state"], doc["reason"] or "baseline %s, candidate %s, control %s, profile "
                "%s" % (doc["baseline_sha"], doc["candidate_sha"], doc["control_sha"],
                        doc["profile"])))
    return 0


def cmd_telemetry_verify(args):
    try:
        result, identity, _ = telemetrylab.verify(args.result, args.authority_root)
    except (ContractError, OSError) as error:
        return _refuse("invalid", "invalid_telemetry_result", str(error))
    sys.stderr.write("verified %s (bundle %s)\n" % (telemetrylab.headline(result), identity))
    if args.summary:
        sys.stdout.write(telemetrylab.summary_markdown(result))
    return 0


def cmd_telemetry_build_record(args):
    """Write one side's build record (`af.perf-telemetry-build/1`) beside its binary."""
    try:
        doc = {"schema": contracts.TELEMETRY_BUILD_SCHEMA, "side": args.side,
               "source_sha": args.source_sha,
               "binary_sha256": telemetrylab._binary(args.binary, "--binary")[1],
               "image": args.image, "rustc": args.rustc.strip()[:512],
               "cargo": args.cargo.strip()[:512], "command": args.command_text}
        data = contracts.write_bytes(doc, "telemetry-build")
        with open(args.output, "xb") as stream:
            stream.write(data)
    except (ContractError, OSError) as error:
        return _refuse("invalid", "invalid_telemetry_build", str(error))
    return 0


def cmd_telemetry_admit(args):
    """Admit a workflow's result bundle for upload: present, verified under this checkout's
    measurement authority and free of credentials, or not admitted. Writes `admitted=` and
    `outcome=` step outputs; exits 0 only when admitted."""
    outcome, detail, result = telemetrylab.admit_for_upload(args.result)
    lines = "admitted=%s\noutcome=%s\n" % ("true" if outcome == "admitted" else "false",
                                             outcome)
    if args.github_output:
        with open(args.github_output, "a") as stream:
            stream.write(lines)
    else:
        sys.stdout.write(lines)
    if args.job_summary:
        with open(args.job_summary, "a") as stream:
            if result is not None:
                stream.write(telemetrylab.summary_markdown(result))
            else:
                stream.write("### AF telemetry experiment (advisory): not uploaded\n\n%s\n"
                             % render.markdown_cell(detail))
    sys.stderr.write("%s: %s\n" % (outcome, detail))
    return 0 if outcome == "admitted" else EXIT_UNAVAILABLE


def cmd_telemetry_history(args):
    try:
        doc = telemetrylab.history(os.path.abspath(args.root), args.import_ or (),
                                   args.authority_root, args.bytes_max, args.results_max,
                                   args.retention_days)
    except (ContractError, OSError) as error:
        return _refuse("invalid", "invalid_telemetry_history", str(error))
    sys.stderr.write("%d results in %d groups, %d pruned, %d refused\n" % (
        len(doc["results"]), len(doc["groups"]), len(doc["pruned"]), len(doc["refused"])))
    print(os.path.join(os.path.abspath(args.root), telemetrylab.HISTORY_HTML))
    return 4 if doc["refused"] else 0


def cmd_telemetry_collect(args):
    """One bounded telemetry collection cycle (ADR-0200); the credential is read here, from the
    environment variable the configuration names, and given only to the collector."""
    try:
        config = telemetrycollect.load_config(args.config)
        token = transport.token_from(os.environ, config["token_env"])
        api = transport.Collector(transport.HttpsTransport(), config["repository"], token,
                                  api_url=config["api_url"],
                                  seconds=config["collection_seconds"])
        doc = telemetrycollect.Cycle(config, api, gitrefs.Git(config["authority_repository"]),
                                     time.time).run()
    except (ContractError, OSError) as error:
        return _refuse("invalid", "invalid_telemetry_collection", str(error))
    counts = {}
    for item in doc["runs"]:
        counts[item["outcome"]] = counts.get(item["outcome"], 0) + 1
    sys.stderr.write("telemetry collection %s: %s; %d results kept in %d bytes%s\n" % (
        doc["status"], ", ".join("%d %s" % (count, name) for name, count in sorted(
            counts.items())) or "no runs", doc["results"], doc["used_bytes"],
        ("; " + doc["detail"]) if doc["detail"] else ""))
    print(os.path.join(config["root"], telemetrylab.HISTORY_HTML))
    return 0 if doc["status"] == "ok" else EXIT_UNAVAILABLE


def cmd_build(args):
    try:
        with open(args.plan, "rb") as stream:
            plan = contracts.read_json(stream.read(contracts.limits.MAX_JSON_BYTES + 1), "run",
                                       args.plan)
        recipe = plan["profile"]["recipe"]
        contained = plan["profile"]["build"]["isolation"] == "container"
        # A container build verifies the image's own toolchain; only a host build needs one here.
        toolchain = None if contained else builds.Toolchain.discover(recipe["toolchain"],
                                                                     args.toolchain_bin)
        builder = builds.Builder(args.repo, plan, toolchain, cargo_home=args.cargo_home,
                                 keep_worktrees=args.keep_worktrees,
                                 zigbuild_bin=args.zigbuild_bin, build_image=args.build_image,
                                 docker=args.docker, deadline_unix_ms=args.deadline_unix_ms,
                                 build_cache=args.build_cache)
        doc = builder.build()
    except builds.Unavailable as error:
        return _refuse("unavailable", error.reason, str(error))
    except (ContractError, OSError) as error:
        return _refuse("invalid", "invalid_build", str(error))
    sys.stderr.write("built %s: supervisor %s, baseline %s (%s), candidate %s (%s); %s\n" % (
        builder.layout.build, doc["supervisor"]["status"], doc["sides"]["baseline"]["status"],
        doc["sides"]["baseline"]["probe_coverage"], doc["sides"]["candidate"]["status"],
        doc["sides"]["candidate"]["probe_coverage"], doc["cleanup"]["status"]))
    print(os.path.join(builder.layout.build, builds.BUILDS_FILE))
    failed = [doc["supervisor"]["status"]] + [doc["sides"][side]["status"]
                                                for side in contracts.SIDES]
    return 0 if all(status == "succeeded" for status in failed) else EXIT_UNAVAILABLE


def cmd_capabilities(args):
    lock = None
    try:
        loaded = suite_module.load(args.suite)
        if args.verify_scratch_enforcement:
            # The fill shares the scratch filesystem with every shard of this machine.
            lock = runner.machine_lock()
        doc = capability.report(loaded.doc, args.docker, args.scratch,
                                args.verify_scratch_enforcement)
    except runner.Unavailable as error:
        return _refuse("unavailable", error.reason, str(error))
    except (ContractError, OSError) as error:
        sys.stderr.write("invalid capability input: %s\n" % error)
        return EXIT_INVALID
    finally:
        if lock is not None:
            lock.release()
    sys.stdout.write(json.dumps(doc, indent=2, sort_keys=True) + "\n")
    return 0


def cmd_profile(args):
    if bool(args.case) == bool(args.verdict):
        sys.stderr.write("refused: choose explicit --case selections or automatic selection "
                         "from the parent's --verdict\n")
        return EXIT_INVALID

    def builder(plan, deadline_unix_ms):
        recipe = plan["profile"]["recipe"]
        contained = plan["profile"]["build"]["isolation"] == "container"
        toolchain = None if contained else builds.Toolchain.discover(recipe["toolchain"],
                                                                     args.toolchain_bin)
        built = builds.Builder(args.repo, plan, toolchain, cargo_home=args.cargo_home,
                               zigbuild_bin=args.zigbuild_bin, build_image=args.build_image,
                               docker=args.docker, deadline_unix_ms=deadline_unix_ms)
        built.build()
        return built.layout.build

    try:
        options = diagnostics.Options(
            plan=args.plan, verdict=args.verdict, cases=args.case or [],
            max_cases=args.max_cases, recipe=args.recipe, output=args.output, repo=args.repo,
            build=args.build, parent_build=args.parent_build, perf=args.perf,
            inferno_bin=args.inferno_bin, samply=args.samply, valgrind_bin=args.valgrind_bin,
            duration_seconds=args.duration_seconds, frequency_hz=args.frequency_hz,
            total_minutes=args.total_minutes, run_id=args.run_id, mode=args.mode,
            docker=args.docker, image=args.image, scratch_fs=args.scratch_fs,
            builder=None if args.no_build else builder, toolchain_bin=args.toolchain_bin,
            cargo_home=args.cargo_home, zigbuild_bin=args.zigbuild_bin,
            build_image=args.build_image, telemetry=args.telemetry)
        code, path, message = diagnostics.run_profile(gitrefs.Git(args.repo), options)
    except (gitrefs.Unavailable, diagnostics.Unavailable) as error:
        return _refuse("unavailable", error.reason, str(error))
    except (ContractError, OSError) as error:
        return _refuse("invalid", "invalid_profile", str(error))
    sys.stderr.write(message + "\n")
    if path is not None:
        print(path)
    return code


def cmd_judge(args):
    try:
        doc, golden = oracle.load(args.suite)
        request = oracle.read_request(args.request)
        with open(args.stdout, "rb") as stream:
            stdout = stream.read(contracts.limits.RECORD_BYTES_MAX + 1)
        result = oracle.judge(doc, golden, request, args.exit_code, stdout)
    except (ContractError, OSError) as error:
        sys.stderr.write("invalid probe input: %s\n" % error)
        return EXIT_INVALID
    return _report(result, b"")


def cmd_report(args):
    try:
        doc = report.write(args.run, args.output, job_summary=args.job_summary,
                           diagnostics=args.diagnostic or ())
    except InvalidInput as error:
        sys.stderr.write("invalid input: %s\n" % error)
        return EXIT_INVALID
    except (ContractError, OSError) as error:
        sys.stderr.write("invalid input: %s\n" % error)
        return EXIT_INVALID
    print(os.path.join(os.path.abspath(args.output), report.REPORT_HTML))
    sys.stderr.write(report.headline(doc) + "\n")
    return doc["verdict"]["aggregate"]["exit_code"]


def cmd_history(args):
    try:
        doc = history.write(args.inputs, args.output)
    except InvalidInput as error:
        sys.stderr.write("invalid input: %s\n" % error)
        return EXIT_INVALID
    except (ContractError, OSError) as error:
        sys.stderr.write("invalid input: %s\n" % error)
        return EXIT_INVALID
    print(os.path.join(os.path.abspath(args.output), history.HISTORY_HTML))
    sys.stderr.write("%d runs in %d compatible groups; %d not shown\n" % (
        len(doc["runs"]), len(doc["groups"]), len(doc["unavailable"])))
    return 0


def cmd_archive(args):
    actions = [name for name in ("init", "import_dir", "fetch", "retain", "pin", "supersede",
                                 "status") if getattr(args, name) not in (None, False)]
    if len(actions) != 1:
        sys.stderr.write("refused: choose exactly one of --init, --import, --fetch, --retain, "
                         "--pin, --supersede and --status\n")
        return EXIT_INVALID
    action = actions[0]
    try:
        if action == "init":
            archive.Archive.create(args.root, args.capacity_bytes)
            sys.stderr.write("created the archive %s\n" % os.path.abspath(args.root))
            return 0
        with archive.Archive(args.root) as store:
            if action == "status":
                doc = store.status(args.now_unix_ms)
                sys.stdout.buffer.write(contracts.write_bytes(doc, "archive-status"))
                return 0 if doc["health"]["status"] == "ok" else EXIT_UNAVAILABLE
            if action == "import_dir":
                status, run_id = store.import_run(args.import_dir)
            elif action == "fetch":
                require_repository(args)
                collector = transport.Collector(
                    transport.HttpsTransport(), args.repository,
                    transport.token_from(os.environ, args.token_env), api_url=args.api_url)
                status, run_id = transport.fetch(store, collector, args.fetch, args.lab_run)
            elif action == "retain":
                written = store.retain(args.now_unix_ms)
                sys.stderr.write("%d retention and garbage collection entries written\n"
                                 % len(written))
                return 0
            elif action == "pin":
                if not args.reason:
                    raise ContractError("--pin needs --reason active_cohort or release_budget")
                store.pin(args.pin, args.reason, args.note or "")
                sys.stderr.write("pinned %s (%s)\n" % (args.pin, args.reason))
                return 0
            else:
                store.supersede(args.supersede, args.superseded_by)
                sys.stderr.write("superseded %s\n" % args.supersede)
                return 0
    except archive.Refused as refused:
        sys.stderr.write("refused (%s): %s\n" % (refused.status, refused))
        return EXIT_UNAVAILABLE
    except (ContractError, OSError) as error:
        sys.stderr.write("invalid archive input: %s\n" % error)
        return EXIT_INVALID
    sys.stderr.write("%s: %s\n" % (status.replace("_", " "), run_id))
    return 0


def require_repository(args):
    if not args.repository:
        raise ContractError("--fetch needs --repository OWNER/NAME, the configured repository")


# Continuous scheduling (ADR-0177) ------------------------------------------------------------

def _read_bytes(path, limit=None):
    with open(path, "rb") as stream:
        data = stream.read((limit or contracts.limits.MAX_JSON_BYTES) + 1)
    if len(data) > (limit or contracts.limits.MAX_JSON_BYTES):
        raise ContractError("%s is larger than its bound" % path)
    return data


def cmd_ci_schedule(args):
    try:
        payload = json.loads(_read_bytes(args.event).decode("utf-8")) if args.event else {}
        inputs = json.loads(args.inputs) if args.inputs else None
        images = {}
        for item in args.build_image or []:
            recipe, _, reference = item.partition("=")
            if not recipe or not reference:
                raise ContractError("--build-image RECIPE=NAME@sha256:...")
            images[recipe] = reference
        settings = ci.Settings(images, args.dedicated_label, args.executor)
        event = ci.Event(args.event_name, payload, args.sha, args.ref, args.repository,
                         args.run_id, args.run_attempt, inputs)
        claim = None
        if args.release_claim:
            claim = contracts.read_json(_read_bytes(args.release_claim), "release-claim",
                                        args.release_claim)
        doc, plans = ci.schedule(gitrefs.Git(args.repo), event, settings, release=claim)
        ci.write(args.output, doc, plans)
    except (ContractError, OSError, ValueError) as error:
        return _refuse("invalid", "invalid_event", str(error))
    values = ci.outputs(doc)
    if args.job_summary:
        with open(args.job_summary, "a", encoding="utf-8") as stream:
            stream.write(ci.summary_markdown(doc) + "\n")
    if args.github_output:
        with open(args.github_output, "a", encoding="utf-8") as stream:
            stream.write(ci.github_output_lines(values))
    sys.stderr.write("%s: %s%s\n" % (doc["status"], doc["detail"] or doc["reason"],
                                      "" if doc["reason"] is None else " (%s)" % doc["reason"]))
    for item in doc["comparisons"]:
        sys.stderr.write("  %s %s: %s%s\n" % (item["role"], item["run_id"], item["status"],
                                              "" if item["reason"] is None
                                              else " (%s: %s)" % (item["reason"],
                                                                  item["detail"])))
    return 0


def _release_api(args):
    return release.Api(transport.HttpsTransport(), args.repository,
                       transport.token_from(os.environ, args.token_env), api_url=args.api_url)


def _release_report(doc, args, path):
    """Write the handoff document at `path`, beside its claim, the job summary and outputs; the
    exit code of its decision."""
    with open(path, "xb") as stream:
        stream.write(contracts.write_bytes(doc, "release-handoff"))
    if args.job_summary:
        with open(args.job_summary, "a", encoding="utf-8") as stream:
            stream.write(release.summary_markdown(doc) + "\n")
    if args.github_output:
        with open(args.github_output, "a", encoding="utf-8") as stream:
            stream.write(ci.github_output_lines(release.outputs(doc)))
    sys.stderr.write("%s%s: %s\n" % (doc["status"], "" if doc["reason"] is None
                                      else " (%s)" % doc["reason"], doc["detail"]))
    return release.EXIT_CODES[doc["status"]]


def cmd_ci_release_handoff(args):
    try:
        payload = json.loads(_read_bytes(args.event).decode("utf-8")) if args.event else {}
        inputs = json.loads(args.inputs) if args.inputs else {}
        actions = release.Actions(args.repository, args.run_id, args.run_attempt,
                                  args.event_name, args.sha)
        remeasure = None
        if args.event_name == "workflow_dispatch":
            inputs = inputs if isinstance(inputs, dict) else {}
            run = str(inputs.get("release_run") or "")
            remeasure = (int(run) if run.isdigit() and len(run) <= 16 else None,
                         inputs.get("remeasure_of"))
        api = _release_api(args)
        os.makedirs(args.output)
        doc, claim = release.handoff(api, gitrefs.Git(args.repo), actions, payload=payload,
                                     remeasure=remeasure)
        if claim is not None:
            os.makedirs(os.path.join(args.output, "claim"))
            with open(os.path.join(args.output, "claim", release.CLAIM_FILE), "xb") as stream:
                stream.write(contracts.write_bytes(claim, "release-claim"))
    except (ContractError, OSError, ValueError) as error:
        sys.stderr.write("refused: %s\n" % error)
        return EXIT_INVALID
    return _release_report(doc, args, os.path.join(args.output, release.HANDOFF_FILE))


def cmd_ci_release_confirm(args):
    try:
        handoff = contracts.read_file(os.path.join(args.handoff, release.HANDOFF_FILE),
                                      "release-handoff")
        claim = contracts.read_file(os.path.join(args.handoff, "claim", release.CLAIM_FILE),
                                    "release-claim")
        if handoff["actions"]["repository"] != args.repository:
            raise ContractError("the handoff is of another repository")
        doc = release.confirm(_release_api(args), handoff, claim)
    except (ContractError, OSError, ValueError) as error:
        sys.stderr.write("refused: %s\n" % error)
        return EXIT_INVALID
    return _release_report(doc, args, os.path.join(args.handoff, release.CONFIRM_FILE))


def cmd_ci_pack(args):
    try:
        plan = _load_json(args.plan, "run")
        doc = buildpack.pack(args.build, plan, args.output)
    except (ContractError, OSError) as error:
        sys.stderr.write("refused: %s\n" % error)
        return EXIT_INVALID
    sys.stderr.write("packed %d files, %d bytes, archive %s\n" % (
        len(doc["entries"]), doc["bytes"], doc["archive"]["sha256"]))
    return 0


def cmd_ci_unpack(args):
    try:
        plan = _load_json(args.plan, "run")
        doc = buildpack.unpack(args.pack, plan, args.output)
    except (ContractError, OSError) as error:
        sys.stderr.write("refused: %s\n" % error)
        return EXIT_INVALID
    sys.stderr.write("unpacked %d verified files of run %s\n" % (len(doc["entries"]),
                                                               doc["run_id"]))
    return 0


def cmd_ci_stage(args):
    try:
        doc = transport.stage(args.output, args.run_dir, "shard_bundle", shard=args.shard)
    except (ContractError, OSError) as error:
        sys.stderr.write("nothing to stage: %s\n" % error)
        return EXIT_UNAVAILABLE
    sys.stderr.write("staged shard %d of run %s\n" % (doc["shard"], doc["run_id"]))
    return 0


def cmd_ci_aggregate(args):
    try:
        plan_bytes = _read_bytes(args.plan)
        plan = contracts.read_json(plan_bytes, "run", args.plan)
        builds_bytes = buildpack.read_member(args.build_pack, plan, "builds.json")
        doc, completeness = ci.aggregate(plan_bytes, builds_bytes, args.shards, args.run_dir,
                                         args.report, args.job_summary, args.transport)
    except (ContractError, OSError) as error:
        sys.stderr.write("invalid input: %s\n" % error)
        return EXIT_INVALID
    sys.stderr.write("%s; shards received %s of %d, missing %s\n" % (
        report.headline(doc), completeness["received"], completeness["planned"],
        completeness["missing"] or "none"))
    if args.enforcement == "required":
        return doc["verdict"]["aggregate"]["exit_code"]
    return 0


def cmd_ci_credentials(args):
    findings = []
    try:
        for git_dir in args.git_dir or []:
            findings += credentials.git_config(git_dir)
        if args.environment:
            findings += credentials.environment(dict(os.environ))
        for root in args.scan or []:
            findings += credentials.tree(root)
    except (ContractError, OSError) as error:
        sys.stderr.write("refused: %s\n" % error)
        return EXIT_INVALID
    for finding in findings:
        sys.stderr.write("credential: %s\n" % finding)
    if findings:
        sys.stderr.write("refused: %d credential findings where none may be\n" % len(findings))
        return EXIT_UNAVAILABLE
    sys.stderr.write("no credential found\n")
    return 0


def cmd_ci_scratch(args):
    try:
        if args.cleanup:
            problems = hostprep.cleanup(args.image, args.mount)
            for problem in problems:
                sys.stderr.write("cleanup: %s\n" % problem)
            return EXIT_UNAVAILABLE if problems else 0
        record = hostprep.prepare(args.image, args.mount, args.bytes)
    except hostprep.Refused as error:
        return _refuse("unavailable", error.reason, str(error))
    except (ContractError, OSError) as error:
        return _refuse("invalid", "invalid_scratch", str(error))
    sys.stderr.write("mounted %d bytes at %s (%d bytes were free)\n" % (
        record["size_bytes"], record["mount"], record["free_bytes"]))
    return 0


def cmd_measure(args):
    """One guarded kernel measure repetition (ADR-0185): reached through the literal bootstrap
    after the protected harness verified; one judged sample, one report line, or nonzero."""
    return kernelmeasure.measure(os.getcwd(), args.case, args.metric, os.environ, sys.stdout,
                                 sys.stderr)


def cmd_harness_digest(args):
    """The protected harness digest of a checkout, and the deterministic staged-policy check
    (`--check`) or re-pin (`--write`)."""
    root = os.path.abspath(args.root)
    try:
        if args.write:
            found = kernelguard.write_staged(root)
            sys.stderr.write("re-pinned %s and %s to %s; review the change, then re-install "
                             "it into .af/ by hand\n" % (kernelguard.STAGED_POLICY,
                                                          kernelguard.STAGED_INVENTORY, found))
            print(found)
            return 0
        if args.check:
            found, problems = kernelguard.check_staged(root)
            for problem in problems:
                sys.stderr.write("stale: %s\n" % problem)
            if problems:
                sys.stderr.write("run python3 scripts/performance/cli.py harness-digest --write "
                                 "and follow %s/README.md to re-pin\n" % kernelguard.STAGED_DIR)
                return 1
            print(found)
            return 0
        found, entries = kernelguard.checkout_digest(root)
    except (ContractError, OSError) as error:
        sys.stderr.write("invalid harness: %s\n" % error)
        return EXIT_INVALID
    if args.json:
        _emit(document_bytes(kernelguard.inventory_document(found, entries)), None)
    else:
        print("%s %d" % (found, len(entries)))
    return 0


def parser():
    top = argparse.ArgumentParser(prog="cli.py", description=__doc__.split("\n\n")[0])
    commands = top.add_subparsers(dest="command")
    commands.required = True

    listing = commands.add_parser("list", help="validate and expand a registry")
    listing.add_argument("--suite", default="fixtures/performance/suite.toml")
    listing.add_argument("--json", action="store_true")
    listing.set_defaults(handler=cmd_list)

    plan = commands.add_parser("plan", help="resolve and freeze a comparison plan")
    plan.add_argument("--repo", default=".")
    plan.add_argument("--trigger", default="manual", choices=planning.TRIGGERS)
    plan.add_argument("--scope", default="full", choices=contracts.SCOPES)
    plan.add_argument("--tier", choices=suite_module.TIERS)
    plan.add_argument("--base")
    plan.add_argument("--head")
    plan.add_argument("--control")
    plan.add_argument("--authority")
    plan.add_argument("--case", action="append", help="instance or case ID; repeatable")
    plan.add_argument("--environment")
    plan.add_argument("--shards", type=int)
    plan.add_argument("--pairs", type=int)
    plan.add_argument("--seed", type=int)
    plan.add_argument("--run-id")
    plan.add_argument("--parent-run-id", help="the run this plan reruns")
    plan.add_argument("--remeasure-of", help="the run a manual remeasurement repeats")
    plan.add_argument("--pr-number", type=int)
    plan.add_argument("--pr-base-sha")
    plan.add_argument("--pr-head-sha")
    plan.add_argument("--cohort", help="calibration cohort JSON")
    plan.add_argument("--member", help="calibration cohort member ID")
    plan.add_argument("--calibration", help="a slowdown member's sealed calibration "
                      "parameters JSON")
    plan.add_argument("--parent", help="the parent run.json of a diagnostic plan")
    plan.add_argument("--build-image", help="freeze a Linux recipe without its own image pin "
                      "to build in this image, pinned by digest in canonical form "
                      "(NAME@sha256:<manifest digest> or sha256:<image ID>); a recipe's own "
                      "pin may only be restated")
    plan.add_argument("--trusted-host-build", action="store_true",
                      help="freeze a Linux recipe without an image pin to build as an "
                      "explicitly trusted host process that claims no filesystem isolation")
    plan.add_argument("--output")
    plan.set_defaults(handler=cmd_plan)

    comparison = commands.add_parser("compare", help="recompute a verdict from evidence")
    comparison.add_argument("--run", required=True)
    comparison.add_argument("--output")
    comparison.set_defaults(handler=cmd_compare)

    run = commands.add_parser("run", help="execute exactly one frozen shard of a plan")
    run.add_argument("--plan", required=True, help="the frozen run.json")
    run.add_argument("--build", required=True, help="the verified build directory of the plan")
    run.add_argument("--shard", required=True, type=int, help="the planned shard index")
    run.add_argument("--run-dir", required=True, help="the run directory; created with the run "
                     "bundle when absent, the shard's bundle goes below shards/<index>")
    run.add_argument("--repo", default=".", help="the checkout holding the plan's commits; the "
                     "authority registry, CLI fixtures and each side's Cargo.lock are read from "
                     "its Git objects")
    run.add_argument("--trusted-local", action="store_true",
                     help="explicitly trusted local mode: a process group on macOS, discovery "
                     "and nonspawning library cases only")
    run.add_argument("--scratch", help="trusted local mode: a private parent directory owned "
                     "by you and writable by no one else")
    run.add_argument("--container", action="store_true",
                     help="one pinned Linux sample container per sample, the supervisor its "
                     "PID 1")
    run.add_argument("--image", help="--container: the sample image, pinned by digest")
    run.add_argument("--scratch-fs", help="--container: the dedicated disk-bounded scratch "
                     "filesystem's mount point; its enforcement is verified first")
    run.add_argument("--docker", default="docker")
    run.add_argument("--deadline-unix-ms", type=int, help="a caller's deadline; it only "
                     "tightens the run's total deadline that the build recorded and the shard's "
                     "sampling deadline")
    run.set_defaults(handler=cmd_run)

    aggregate = commands.add_parser("aggregate", help="import shard bundles and recompute "
                                    "the verdict")
    aggregate.add_argument("--run", required=True)
    aggregate.add_argument("--shard-bundle", action="append",
                           help="a shard bundle directory uploaded from another host; "
                           "repeatable")
    aggregate.add_argument("--output")
    aggregate.set_defaults(handler=cmd_aggregate)

    golden = commands.add_parser("golden", help="check or rewrite the native golden entries")
    golden.add_argument("--suite", default="fixtures/performance/suite.toml")
    golden.add_argument("--write", action="store_true")
    golden.set_defaults(handler=cmd_golden)

    build = commands.add_parser("build", help="build a frozen plan's products and supervisor")
    build.add_argument("--plan", required=True, help="the frozen run.json")
    build.add_argument("--repo", default=".", help="the owning checkout holding the plan's "
                       "commits; builds go below its target/performance/<run>")
    build.add_argument("--toolchain-bin", help="the recipe toolchain's bin directory; "
                       "default: the sysroot rustup selects for the recipe's exact version")
    build.add_argument("--cargo-home", help="a pre-fetched Cargo home holding every locked "
                       "dependency; default: a run-private one filled by cargo fetch --locked")
    build.add_argument("--zigbuild-bin", help="a directory holding the shipping recipe's "
                       "cargo-zigbuild and zig executables (inside the build image for a "
                       "container build, where python3 -m ziglang may stand for zig); each must "
                       "report the recipe's exact version")
    build.add_argument("--build-image", help="restate the build image the plan froze; any "
                       "other reference refuses, and a build never chooses an image")
    build.add_argument("--deadline-unix-ms", type=int, help="a caller's deadline; it only "
                       "tightens the run's total deadline this build establishes")
    build.add_argument("--build-cache", choices=("cold", "warm"), default="cold",
                       help="cold: the measured pass starts from observed empty targets; warm: "
                       "a bounded priming build of the same sources fills the run's own targets "
                       "first, recorded apart (ADR-0185)")
    build.add_argument("--docker", default="docker")
    build.add_argument("--keep-worktrees", action="store_true",
                       help="keep the worktrees and targets instead of removing them")
    build.set_defaults(handler=cmd_build)

    profile = commands.add_parser("profile", help="create a bounded diagnostic child run")
    profile.add_argument("--plan", required=True, help="the parent comparison's run.json")
    profile.add_argument("--case", action="append", help="an instance to diagnose; repeatable, "
                         "at most 8")
    profile.add_argument("--verdict", help="the parent's verdict.json: select its regressed, "
                         "then inconclusive cases by instance ID")
    profile.add_argument("--max-cases", type=int, help="automatic selection: at most this many "
                         "cases (default 3, at most 8)")
    profile.add_argument("--recipe", required=True, help="a diagnostic recipe of the "
                         "authority's fixtures/performance/diagnostics.toml")
    profile.add_argument("--output", required=True, help="the child run's new directory")
    profile.add_argument("--repo", default=".")
    profile.add_argument("--run-id", help="the child run's ID; default: a new opaque one")
    profile.add_argument("--build", help="an existing build of the child plan; default: build "
                         "it below the checkout's target/performance/<run>")
    profile.add_argument("--no-build", action="store_true", help="build nothing; without "
                         "--build every capture is recorded unavailable")
    profile.add_argument("--parent-build", help="the parent's verified build, whose "
                         "uninstrumented probe a trace's overhead is measured against")
    profile.add_argument("--perf", help="the configured Linux perf executable")
    profile.add_argument("--inferno-bin", help="the directory of the pinned inferno 0.12.8 "
                         "executables")
    profile.add_argument("--samply", help="the configured samply executable (macOS)")
    profile.add_argument("--valgrind-bin", help="the directory of valgrind and "
                         "callgrind_control")
    profile.add_argument("--duration-seconds", type=int, help="recorded region per side "
                         "(default 10, at most 30)")
    profile.add_argument("--frequency-hz", type=int, help="sampling frequency (default 997, at "
                         "most 1999)")
    profile.add_argument("--total-minutes", type=int, help="the child run's total wall bound, "
                         "builds included (default 15, at most 30)")
    profile.add_argument("--mode", required=True, choices=("trusted_local", "linux_container"),
                         help="the execution mode, selected explicitly and recorded; "
                         "linux_container runs every execution in its own sample container and "
                         "only for an environment that declares it")
    profile.add_argument("--image", help="linux_container: the sample image, pinned by digest")
    profile.add_argument("--scratch-fs", help="linux_container: the dedicated disk-bounded "
                         "scratch filesystem's mount point, verified before anything runs")
    profile.add_argument("--toolchain-bin")
    profile.add_argument("--cargo-home")
    profile.add_argument("--zigbuild-bin")
    profile.add_argument("--build-image")
    profile.add_argument("--docker", default="docker")
    profile.add_argument("--telemetry", choices=("spans", "content"),
                         help="also record each eligible selected CLI case once per side at "
                         "this telemetry level and read it back through that side's own af "
                         "telemetry documents (ADR-0199); content is perturbing; never a "
                         "verdict sample")
    profile.set_defaults(handler=cmd_profile)

    capabilities = commands.add_parser("capabilities", help="report what this host can run")
    capabilities.add_argument("--suite", default="fixtures/performance/suite.toml")
    capabilities.add_argument("--docker", default="docker")
    capabilities.add_argument("--scratch", help="the dedicated disk-bounded scratch "
                              "filesystem's mount point")
    capabilities.add_argument("--verify-scratch-enforcement", action="store_true",
                              help="fill the scratch filesystem to verify its bound")
    capabilities.set_defaults(handler=cmd_capabilities)

    probe = commands.add_parser("probe", help="run and judge one trusted local probe sample")
    probe.add_argument("--suite", default="fixtures/performance/suite.toml")
    probe.add_argument("--trusted-local", action="store_true",
                       help="acknowledge explicitly trusted local mode: a process group, not "
                       "Linux namespace containment; only nonspawning cases run")
    probe.add_argument("--build", help="a build directory whose hashed artifacts to use")
    probe.add_argument("--side", default="baseline", help="the build side whose probe runs")
    probe.add_argument("--supervisor", help="a built perf-supervisor example, without --build")
    probe.add_argument("--binary", help="a built perf-probe example, without --build")
    probe.add_argument("--instance", required=True, help="native instance or sentinel ID")
    probe.add_argument("--scratch", required=True,
                       help="a private parent directory, owned by you and writable by no one "
                       "else, below which a private namespace is created and removed; nothing "
                       "else in it is touched")
    probe.add_argument("--timeout", type=int,
                       help="seconds, 1 to 120; default: the case's declared timeout")
    probe.add_argument("--trace-output",
                       help="with a perf-trace probe build: write the measured request's "
                       "validated phase trace to this new file and print its phase summary; "
                       "diagnostic only, never a verdict sample")
    probe.set_defaults(handler=cmd_probe)

    telemetry_run = commands.add_parser(
        "telemetry-run", help="measure Telemetry itself on a baseline and a candidate binary "
        "with this checkout's bench scripts (advisory, ADR-0199)")
    telemetry_run.add_argument("--profile", required=True,
                               choices=("pr", "nightly", "release", "manual"))
    telemetry_run.add_argument("--trigger", required=True,
                               choices=("pr", "main_push", "nightly", "release", "manual"))
    telemetry_run.add_argument("--baseline-af", required=True)
    telemetry_run.add_argument("--candidate-af", required=True)
    telemetry_run.add_argument("--baseline-sha", required=True)
    telemetry_run.add_argument("--candidate-sha", required=True)
    telemetry_run.add_argument("--authority-sha", required=True,
                               help="the commit of this checkout, whose scripts and fixtures "
                               "measure both sides")
    telemetry_run.add_argument("--experiment", action="append",
                               choices=("overhead", "invocation_cost", "read_latency"),
                               help="measure only this experiment (repeatable)")
    telemetry_run.add_argument("--rounds", type=int)
    telemetry_run.add_argument("--pairs", type=int, help="pairs per script run, 1 to 40")
    telemetry_run.add_argument("--scale", choices=("full", "smoke"))
    telemetry_run.add_argument("--corpus-runs", type=int)
    telemetry_run.add_argument("--output", required=True, help="the result's new directory")
    telemetry_run.add_argument("--work", help="a new private work directory (default: "
                               "OUTPUT.work, removed afterwards)")
    telemetry_run.add_argument("--result-id")
    telemetry_run.add_argument("--mode", required=True, choices=("trusted_local",
                                                                "linux_container"),
                               help="where every execution runs: linux_container, each in its "
                               "own container of the pinned image (required for pull "
                               "requests), or trusted_local, on this host for trusted source")
    telemetry_run.add_argument("--docker", default="docker")
    telemetry_run.add_argument("--image", help="linux_container: the sample image, pinned by "
                               "digest")
    telemetry_run.add_argument("--scratch-fs", help="linux_container: the dedicated "
                               "disk-bounded scratch filesystem's mount point")
    telemetry_run.add_argument("--baseline-build", help="the baseline's build record "
                               "(af.perf-telemetry-build/1)")
    telemetry_run.add_argument("--candidate-build", help="the candidate's build record")
    telemetry_run.set_defaults(handler=cmd_telemetry_run)

    telemetry_schedule = commands.add_parser(
        "telemetry-schedule", help="resolve one Actions event of the Performance telemetry "
        "workflow from the trusted control")
    telemetry_schedule.add_argument("--repo", default=".")
    telemetry_schedule.add_argument("--event-name", required=True)
    telemetry_schedule.add_argument("--event", help="the event's JSON payload")
    telemetry_schedule.add_argument("--inputs", help="a dispatch's inputs as JSON")
    telemetry_schedule.add_argument("--sha", required=True)
    telemetry_schedule.add_argument("--github-output")
    telemetry_schedule.add_argument("--job-summary")
    telemetry_schedule.set_defaults(handler=cmd_telemetry_schedule)

    telemetry_verify = commands.add_parser(
        "telemetry-verify", help="verify a telemetry result bundle offline and recompute it")
    telemetry_verify.add_argument("--result", required=True)
    telemetry_verify.add_argument("--summary", action="store_true",
                                  help="print its job-summary Markdown")
    telemetry_verify.add_argument("--authority-root", help="a checkout of exactly the "
                                  "measurement authority the result names (default: this one)")
    telemetry_verify.set_defaults(handler=cmd_telemetry_verify)

    telemetry_build = commands.add_parser(
        "telemetry-build-record", help="write one side's telemetry build record")
    telemetry_build.add_argument("--side", required=True, choices=("baseline", "candidate"))
    telemetry_build.add_argument("--source-sha", required=True)
    telemetry_build.add_argument("--binary", required=True)
    telemetry_build.add_argument("--image", required=True)
    telemetry_build.add_argument("--rustc", required=True)
    telemetry_build.add_argument("--cargo", required=True)
    telemetry_build.add_argument("--command", dest="command_text", required=True)
    telemetry_build.add_argument("--output", required=True)
    telemetry_build.set_defaults(handler=cmd_telemetry_build_record)

    telemetry_admit = commands.add_parser(
        "telemetry-admit", help="admit a workflow's telemetry result for upload: verified and "
        "free of credentials, or not uploaded")
    telemetry_admit.add_argument("--result", required=True)
    telemetry_admit.add_argument("--github-output")
    telemetry_admit.add_argument("--job-summary")
    telemetry_admit.set_defaults(handler=cmd_telemetry_admit)

    telemetry_collect = commands.add_parser(
        "telemetry-collect", help="one bounded collection cycle of the Performance telemetry "
        "workflow's results into a durable history root (trusted collector)")
    telemetry_collect.add_argument("--config", required=True,
                                   help="the collector's af.perf-telemetry-collector/1 TOML")
    telemetry_collect.set_defaults(handler=cmd_telemetry_collect)

    telemetry_history = commands.add_parser(
        "telemetry-history", help="keep verified telemetry results in a durable root and draw "
        "its compatible history")
    telemetry_history.add_argument("--root", required=True)
    telemetry_history.add_argument("--import", dest="import_", action="append",
                                   help="a result bundle to verify and keep (repeatable)")
    telemetry_history.add_argument("--authority-root", help="a checkout of exactly the "
                                   "measurement authority the imports name (default: this one)")
    telemetry_history.add_argument("--bytes-max", type=int,
                                   default=telemetrylab.HISTORY_BYTES_DEFAULT)
    telemetry_history.add_argument("--results-max", type=int,
                                   default=telemetrylab.HISTORY_RESULTS_DEFAULT)
    telemetry_history.add_argument("--retention-days", type=int)
    telemetry_history.set_defaults(handler=cmd_telemetry_history)

    judging = commands.add_parser("judge", help="judge one recorded probe execution")
    judging.add_argument("--suite", default="fixtures/performance/suite.toml")
    judging.add_argument("--request", required=True)
    judging.add_argument("--stdout", required=True)
    judging.add_argument("--exit-code", type=int, required=True)
    judging.set_defaults(handler=cmd_judge)

    reporting = commands.add_parser("report", help="write a run's offline report")
    reporting.add_argument("--run", required=True, help="the run directory")
    reporting.add_argument("--output", required=True, help="a new directory for the report")
    reporting.add_argument("--job-summary", help="a file the compact summary table is "
                           "appended to, such as $GITHUB_STEP_SUMMARY")
    reporting.add_argument("--diagnostic", action="append",
                           help="a diagnostic child run directory of this run to verify and "
                                "link; repeatable (ADR-0177)")
    reporting.set_defaults(handler=cmd_report)

    trend = commands.add_parser("history", help="write compatible history across runs")
    trend.add_argument("--inputs", required=True, nargs="+",
                       help="run directories and archive roots")
    trend.add_argument("--output", required=True, help="a new directory for the history")
    trend.set_defaults(handler=cmd_history)

    store = commands.add_parser("archive", help="the trusted collector's evidence archive")
    store.add_argument("--root", required=True, help="the archive root on a persistent "
                       "filesystem")
    store.add_argument("--init", action="store_true", help="create an archive at --root")
    store.add_argument("--capacity-bytes", type=int, default=contracts.limits.ARCHIVE_BYTES_DEFAULT,
                       help="--init: the archive's capacity, default 100 GiB, at most 1 TiB")
    store.add_argument("--import", dest="import_dir", help="admit a verified run directory")
    store.add_argument("--fetch", type=int, help="collect an Actions run's af-perf-* transport "
                       "artifacts from --repository")
    store.add_argument("--repository", help="--fetch: the configured OWNER/NAME")
    store.add_argument("--lab-run", help="--fetch: collect only this laboratory run's "
                       "af-perf-<run>-* artifacts, when one Actions run holds several")
    store.add_argument("--api-url", default=transport.API_URL,
                       help="--fetch: the API origin the token is sent to")
    store.add_argument("--token-env", default=transport.TOKEN_ENV,
                       help="--fetch: the environment variable holding a token with read "
                       "access to the repository's contents and Actions artifacts")
    store.add_argument("--retain", action="store_true", help="apply retention and collect "
                       "unreferenced objects")
    store.add_argument("--pin", help="pin an archived run")
    store.add_argument("--reason", choices=("active_cohort", "release_budget"))
    store.add_argument("--note")
    store.add_argument("--supersede", help="supersede a pinned run")
    store.add_argument("--superseded-by", help="--supersede: the run that supersedes it")
    store.add_argument("--status", action="store_true", help="print the archive's health")
    store.add_argument("--now-unix-ms", type=int, help="--retain, --status: evaluate at this "
                       "instant instead of now")
    store.set_defaults(handler=cmd_archive)

    scheduling = commands.add_parser("ci-schedule", help="resolve one Actions event and freeze "
                                     "its comparisons from the trusted control (ADR-0177)")
    scheduling.add_argument("--repo", default=".")
    scheduling.add_argument("--event-name", required=True)
    scheduling.add_argument("--event", help="the event payload, GITHUB_EVENT_PATH")
    scheduling.add_argument("--inputs", help="a dispatch's inputs as JSON")
    scheduling.add_argument("--sha", required=True, help="GITHUB_SHA")
    scheduling.add_argument("--ref", default="", help="GITHUB_REF")
    scheduling.add_argument("--repository", required=True, help="GITHUB_REPOSITORY")
    scheduling.add_argument("--run-id", required=True, type=int, help="GITHUB_RUN_ID")
    scheduling.add_argument("--run-attempt", required=True, type=int, help="GITHUB_RUN_ATTEMPT")
    scheduling.add_argument("--build-image", action="append",
                            help="RECIPE=NAME@sha256:... a Linux recipe's registry build image")
    scheduling.add_argument("--executor", default="ephemeral_hosted",
                            choices=contracts.EXECUTORS)
    scheduling.add_argument("--dedicated-label", help="the dedicated runner label a trusted "
                            "event may use")
    scheduling.add_argument("--output", required=True, help="a new directory for schedule.json "
                            "and the frozen plans")
    scheduling.add_argument("--release-claim", help="the confirmed af.perf-release-claim/1 "
                            "this run wrote, when it measures a release (ADR-0180)")
    scheduling.add_argument("--github-output", help="the job's GITHUB_OUTPUT file")
    scheduling.add_argument("--job-summary", help="a file the schedule's summary is appended "
                            "to, such as $GITHUB_STEP_SUMMARY")
    scheduling.set_defaults(handler=cmd_ci_schedule)

    handing = commands.add_parser("ci-release-handoff", help="verify a completed upstream "
                                  "Release run and decide whether this run claims its release "
                                  "(ADR-0180)")
    handing.add_argument("--repo", default=".", help="the trusted checkout, with its tags")
    handing.add_argument("--event-name", required=True,
                         choices=("workflow_run", "workflow_dispatch"))
    handing.add_argument("--event", help="the event payload, GITHUB_EVENT_PATH")
    handing.add_argument("--inputs", help="a remeasurement dispatch's inputs as JSON: "
                         "release_run and remeasure_of")
    handing.add_argument("--sha", required=True, help="GITHUB_SHA")
    handing.add_argument("--repository", required=True, help="GITHUB_REPOSITORY")
    handing.add_argument("--run-id", required=True, type=int, help="GITHUB_RUN_ID")
    handing.add_argument("--run-attempt", required=True, type=int, help="GITHUB_RUN_ATTEMPT")
    handing.add_argument("--output", required=True, help="a new directory for handoff.json and "
                         "the claim to upload")
    confirming = commands.add_parser("ci-release-confirm", help="confirm that this run's "
                                     "written claim is the first valid one")
    confirming.add_argument("--handoff", required=True, help="the handoff's output directory")
    confirming.add_argument("--repository", required=True, help="GITHUB_REPOSITORY")
    for command in (handing, confirming):
        command.add_argument("--api-url", default=transport.API_URL)
        command.add_argument("--token-env", default=release.TOKEN_ENV,
                             help="the environment variable holding the run's read-only token")
        command.add_argument("--github-output", help="the job's GITHUB_OUTPUT file")
        command.add_argument("--job-summary", help="a file the decision is appended to")
    handing.set_defaults(handler=cmd_ci_release_handoff)
    confirming.set_defaults(handler=cmd_ci_release_confirm)

    packing = commands.add_parser("ci-pack", help="pack a verified build for its shards")
    packing.add_argument("--plan", required=True)
    packing.add_argument("--build", required=True, help="the build directory")
    packing.add_argument("--output", required=True, help="a new directory for the pack")
    packing.set_defaults(handler=cmd_ci_pack)

    unpacking = commands.add_parser("ci-unpack", help="verify and unpack a packed build")
    unpacking.add_argument("--plan", required=True)
    unpacking.add_argument("--pack", required=True, help="the downloaded pack directory")
    unpacking.add_argument("--output", required=True, help="a new build directory")
    unpacking.set_defaults(handler=cmd_ci_unpack)

    staging = commands.add_parser("ci-stage", help="stage a shard bundle as its transport "
                                  "artifact directory")
    staging.add_argument("--run-dir", required=True)
    staging.add_argument("--shard", required=True, type=int)
    staging.add_argument("--output", required=True, help="a new directory to upload")
    staging.set_defaults(handler=cmd_ci_stage)

    aggregating = commands.add_parser("ci-aggregate", help="import every shard artifact, "
                                      "recompute the verdict and write the report")
    aggregating.add_argument("--plan", required=True)
    aggregating.add_argument("--build-pack", required=True)
    aggregating.add_argument("--shards", required=True, help="the directory of downloaded "
                             "shard artifacts, one subdirectory each")
    aggregating.add_argument("--run-dir", required=True, help="a new run directory")
    aggregating.add_argument("--report", required=True, help="a new report directory")
    aggregating.add_argument("--transport", help="a new directory for the run bundle's "
                             "transport artifact")
    aggregating.add_argument("--job-summary")
    aggregating.add_argument("--enforcement", default="advisory",
                             choices=("advisory", "required"))
    aggregating.set_defaults(handler=cmd_ci_aggregate)

    checking = commands.add_parser("ci-credentials", help="refuse credentials where a "
                                   "credential-free job must have none")
    checking.add_argument("--git-dir", action="append")
    checking.add_argument("--environment", action="store_true")
    checking.add_argument("--scan", action="append", help="a directory whose files to scan")
    checking.set_defaults(handler=cmd_ci_credentials)

    scratch = commands.add_parser("ci-scratch", help="prepare or remove a hosted runner's "
                                  "loop-mounted sample scratch")
    scratch.add_argument("--image", required=True)
    scratch.add_argument("--mount", required=True)
    scratch.add_argument("--bytes", type=int, default=2 * 1024 * 1024 * 1024)
    scratch.add_argument("--cleanup", action="store_true")
    scratch.set_defaults(handler=cmd_ci_scratch)

    declaring = commands.add_parser("cohort-declare", help="declare a fixed calibration cohort "
                                    "before any member runs")
    declaring.add_argument("--repo", default=".")
    declaring.add_argument("--authority", required=True)
    declaring.add_argument("--cohort-id", required=True)
    declaring.add_argument("--tier", required=True, choices=("quick", "nightly", "release"))
    declaring.add_argument("--environment", required=True)
    declaring.add_argument("--recipe", required=True)
    declaring.add_argument("--group", action="append", nargs=3, required=True,
                           metavar=("ID", "CPU_MODEL", "IMAGE"),
                           help="an allowed group: ID 'CPU MODEL' 'NAME@sha256:DIGEST'")
    declaring.add_argument("--start-date", required=True, help="the first day, YYYY-MM-DD UTC")
    declaring.add_argument("--days", type=int, default=5)
    declaring.add_argument("--preparation-members", type=int, default=2)
    declaring.add_argument("--preparation-iterations", type=int, default=1000000)
    declaring.add_argument("--repository", required=True, help="OWNER/NAME whose performance "
                           "workflow dispatches every member")
    declaring.add_argument("--branch", default="main", help="the branch members are "
                           "dispatched from")
    declaring.add_argument("--after-run-number", type=int, required=True,
                           help="the performance workflow's latest run number now, before any "
                           "member is dispatched (0 before its first run); every later run must "
                           "be listed when readiness is judged")
    declaring.add_argument("--output")
    declaring.set_defaults(handler=cmd_cohort_declare)
    preparing = commands.add_parser("calibration-prepare", help="seal A0 and loop counts from "
                                    "the A/A and preparation evidence")
    preparing.add_argument("--repo", default=".", help="the repository whose authority each "
                           "member's plan is frozen again from")
    preparing.add_argument("--cohort", required=True)
    preparing.add_argument("--evidence", required=True, help="runs.json, attempts/RUN.json, "
                           "jobs/RUN.json, actions.json and one directory per downloaded "
                           "af-perf-* artifact")
    preparing.add_argument("--output")
    preparing.set_defaults(handler=cmd_calibration_prepare)
    ready = commands.add_parser("readiness", help="recompute a cohort's readiness from its "
                                "retained evidence")
    ready.add_argument("--repo", default=".")
    ready.add_argument("--cohort", required=True)
    ready.add_argument("--calibration", required=True)
    ready.add_argument("--evidence", required=True)
    ready.add_argument("--output")
    ready.add_argument("--job-summary")
    ready.set_defaults(handler=cmd_readiness)
    activating = commands.add_parser("activate", help="verify an activation manifest from its "
                                     "reviewed records or the cohort's retained evidence")
    verifying = activating.add_mutually_exclusive_group()
    verifying.add_argument("--verify", action="store_true", help="verify the manifest from its "
                           "reviewed records, as every active plan does")
    verifying.add_argument("--verify-proposed", action="store_true", help="verify a proposed "
                           "activation as the trusted activation workflow does: fetch and "
                           "recompute its retained evidence when it differs from --base")
    activating.add_argument("--base", help="the trusted control revision a proposal is "
                            "compared with (--verify-proposed)")
    activating.add_argument("--manifest", default=planning.ACTIVATION_PATH)
    activating.add_argument("--repo", default=".")
    activating.add_argument("--revision", default="HEAD", help="the revision whose manifest and "
                            "records are verified")
    activating.add_argument("--repository", help="OWNER/NAME the cohort ran in")
    activating.add_argument("--token-env", default=transport.TOKEN_ENV)
    activating.add_argument("--api-url", default=transport.API_URL)
    activating.add_argument("--output")
    activating.add_argument("--job-summary")
    activating.set_defaults(handler=cmd_activate)
    bridging = commands.add_parser("equivalence-evidence", help="a bridge's execution evidence "
                                   "from its golden-check and post-merge A/A runs")
    bridging.add_argument("--repo", default=".")
    bridging.add_argument("--revision", default="HEAD", help="the revision holding the "
                          "calibrated activation and its records")
    bridging.add_argument("--calibrated-activation", required=True, help="the retained "
                          "calibrated activation manifest the bridge extends")
    bridging.add_argument("--authority", required=True, help="the bridged authority SHA")
    bridging.add_argument("--group", required=True, help="the adapters' equivalence group")
    bridging.add_argument("--adapter-file", action="append", required=True)
    bridging.add_argument("--golden", required=True, help="RUN_DIR=ACTIONS_RUN_ID of the run "
                          "comparing the calibrated with the bridged product source")
    bridging.add_argument("--post-merge-aa", required=True, help="RUN_DIR=ACTIONS_RUN_ID of the "
                          "post-merge A/A through the bridge")
    bridging.add_argument("--output")
    bridging.set_defaults(handler=cmd_equivalence_evidence)
    costing = commands.add_parser("cost-report", help="the cost demonstration of retained runs")
    costing.add_argument("--run", action="append", required=True,
                         help="a retained run directory, DIR; its build cache is read from "
                         "its build manifest, and DIR=cold or DIR=warm must agree with it")
    costing.add_argument("--output")
    costing.add_argument("--job-summary")
    costing.set_defaults(handler=cmd_cost_report)
    shipping = commands.add_parser("shipping-report", help="the shipping-budget demonstration "
                                   "of retained runs")
    shipping.add_argument("--suite", default=planning.SUITE_PATH, help="the authority's "
                          "registry, which names every budget, environment and instance")
    shipping.add_argument("--run", action="append", required=True)
    shipping.add_argument("--output")
    shipping.add_argument("--job-summary")
    shipping.set_defaults(handler=cmd_shipping_report)
    measuring = commands.add_parser("measure", help="one guarded kernel measure repetition; "
                                    "run only through the staged bootstrap")
    measuring.add_argument("--case", required=True, help="a native instance ID, or a case ID "
                           "with one instance")
    measuring.add_argument("--metric", required=True, help="a metric the case declares")
    measuring.set_defaults(handler=cmd_measure)
    pinning = commands.add_parser("harness-digest", help="the guarded kernel experiments' "
                                  "protected harness digest and staged policy")
    pinning.add_argument("--root", default=".")
    action = pinning.add_mutually_exclusive_group()
    action.add_argument("--check", action="store_true", help="the staged policy, inventory and "
                        "any installed guarded measure must match the harness")
    action.add_argument("--write", action="store_true", help="re-pin the staged policy and "
                        "inventory")
    pinning.add_argument("--json", action="store_true")
    pinning.set_defaults(handler=cmd_harness_digest)
    return top


def main(argv=None):
    args = parser().parse_args(argv)
    return args.handler(args)


if __name__ == "__main__":
    sys.exit(main())
