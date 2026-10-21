# ADR-0170: Seed native fixtures, freeze the build image and bound the run by one deadline

**Status:** accepted (2026-10-20). Closes packages p2 and p3 of the
[continuous performance laboratory](../design/continuous-performance.md) before reports, against
the unverified snapshot of
[ADR-0169](0169-coordinate-a-machine-seed-each-side-once-and-build-linux-recipes-in-a-pinned-image.md).
Its decisions stand except where this ADR amends them:

- decision 5 (seeds now cover native fixtures);
- decision 6 (the build image is frozen by the plan, not chosen by the build);
- decision 4 (the report and ledger oracles check every Finding).

It also amends the host deadline of
[ADR-0166](0166-build-frozen-plans-and-supervise-samples-with-one-authority-instrument.md)'s
supervisor launch.

The new contract versions are:

| Contract | Version | What changed |
| --- | --- | --- |
| Plan | `af.perf-run/3` | frozen build, case seeds |
| Build record | `af.perf-builds/5` | run execution start and deadline |
| Shard record | `af.perf-shard/4` | run deadline, native seeds |
| Probe request | `af.perf-probe-request/2` | the seed copy |
| Golden entries | `af.perf-golden/2` | seed writer entries |
| CLI registry | `af.perf-cli/3` | enumeration and tally rules |
| Registry | `af.perf-suite/3` | case seeds, request version, canonical image pins |

## Context

The predecessor's checks passed, but its reviews and root found six gaps:

- **F1, native fixtures were not seeded.** Every source capture sample wrote and committed its
  100 or 10,000 files, and every state projection sample wrote its 10,000-event Task log, one
  FULL-synchronous transaction per event. Predecessor F3 required these fixtures to be written
  once per side and copied per sample, like the CLI seeds.
- **F2, the build image was chosen late.** `cli.py build --build-image` accepted any pinned
  image when the recipe had no pin. The plan never named the image, so its recipe identity did
  not bind the container the design requires it to bind.
- **The host backstop excluded the setup.** `supervise.launch` killed a supervisor after the
  child's timeout, grace and margin, although the supervisor runs the setup first. A valid
  seed whose writer took 110 s and whose publication took 80 s was killed at 185 s.
- **The report oracle accepted wrong reports.** It checked the summary's open count, the list
  length and one Finding. 99 fixed Findings and one open "Fixture finding 100", or the declared
  Finding 100 repeated 100 times, passed all nine rules. The ledger's line and pattern counts
  admitted the same kind of duplicate. These are synthetic counterexamples, not observed
  product failures.
- **A named image never compared equal.** For a `name@sha256:` pin, `compare` compared the pin
  with the image ID and with the digest suffix of the recorded reference. Neither equals the
  full pin, so a correctly built run was `builds_invalid`.
- **The total run deadline was never enforced.** The plan froze `run_total_minutes`, but only
  the shard's sampling deadline and an optional caller deadline applied. Build, preflight,
  seed preparation and a delayed shard could run past the frozen total.

## Considered options

1. **Keep generating native fixtures in each sample.** Rejected: predecessor F3 requires reuse,
   and the per-sample writes dominate the shard's wall time.
2. **Let the probe copy its seed inside its own setup.** Rejected: the copy would hide inside
   the probe's `setup_ms`, and the probe would read the sealed seed itself.
3. **Copy the sealed seed in the sample's supervised setup, and point the probe at its copy.**
   Chosen. The copy is the sample's `fixture_setup_ns`, apart from both the probe's setup and
   the region, and the probe only ever opens its own copy.
4. **Accept a writer's seed on the probe's word.** Rejected: a wrong seed would show up as the
   samples' oracle failures, never as the writer's. The writer's check now has its own golden
   entry from the independent reference generator.
5. **Keep choosing the image at build time and record it afterwards.** Rejected: nothing would
   bind the image a late build chooses to the plan, and two runs with different images would
   share one recipe identity.
6. **Rewrite the plan with the chosen image after the build.** Rejected: a frozen plan is
   never rewritten.
7. **Freeze the image, or an explicit trusted host build, when the plan is frozen, inside the
   recipe identity.** Chosen.
8. **Compare a named digest with the local image ID.** Rejected: a registry manifest digest is
   not the image ID, and a digest under another repository name is not the declared image.
9. **Pin every Finding's whole JSON.** Rejected: that freezes documented fields the fixture does
   not declare, such as bodies, confidences, aliases and Round bookkeeping.
10. **Start the total deadline at each shard.** Rejected: a delayed shard would reset it.
11. **Record one execution start and absolute deadline when the run's build begins.** Chosen.
    Every shard reads it from the build it verifies, and a caller only tightens it.

## Decision

1. **Native seeds.**
   - **Declared.** A library case's probe names its seed in the registry (`probe.seed`): the
     writer contract and the instance parameters the seed is written over. Source capture reads
     `source_capture_seed_v1` over `files` and `tree`, one seed per file count. State
     projection reads `state_projection_seed_v1` over `log`, so the application-cold and
     prewarmed projections share one seed per side.
   - **In the plan.** Each plan case names its seed (`seed_id`, writer case and contract,
     parameters), and the shard refuses a plan seed that differs from the authority registry's.
   - **Written once.** Before a side's first sample that needs it, the shard runs the side's own
     probe on the writer's request as the child of one supervised request. The probe writes
     into the seed's new directory. In a container that directory is bound writable at
     `/perf/seed-out` and is the only writable bind besides the per-sample scratch.
     - The source writer commits the files through Git with automatic garbage collection and
       maintenance off, so no detached process changes the repository after the writer exits.
     - Its check reads the written tree back as a source manifest.
     - The state writer appends the log through the protected public writer and projects it
       through a fresh Store.
   - **Judged before use.** The driver judges the writer's message against the seed's golden
     entry (`af.perf-golden/2` `seeds`), whose digest and count are the readers' own. Only then
     is the seed sealed and identified by its digest, as a CLI seed is. A writer that fails is
     recorded on the seed (`fixture_failed` or `oracle_mismatch`) and fails that side's samples
     as `launch_failure`. It is never retried.
   - **Copied privately.** Each sample's untimed setup, `setup.sh native_seed_copy`, copies the
     sealed seed into the sample's own `seed/` and creates the probe's empty `probe/`. The copy
     is new directories and files of this user, never links. The probe request
     (`af.perf-probe-request/2`) names that copy in `seed`, outside the probe's scratch. The
     probe refuses a seed for a contract that reads none, and a missing one for a contract that
     reads one.
   - **Timing.** The source sample captures its copy into a fresh CAS in its own work
     directory, and the state sample projects its copy through a fresh Store. The copy's time
     is the record's `fixture_setup_ns`, the probe's own setup is `setup_ms`, the region is
     `wall_ms`, and the writer's time is the seed's `writer_wall_ns`.
   - **Verified and removed.** The digest is checked before and after every sample, and a
     changed seed stops the shard (`seed_changed`). Seeds are removed when the shard ends.
     `compare` requires every passing seeded library record to name a ready native seed of
     its own side, with that seed ID and writer.
   - **Recorded.** `af.perf-shard/4` records each seed's `kind` (`cli` or `native`). A native
     seed has no AF hash. `writer_wall_ns` replaces `setup_wall_ns`.
   - **Diagnostic samples.** `cli.py probe` of a seeded instance writes its own seed through the
     same probe, judges the writer, and reads that seed once.
2. **The build image is frozen with the plan.**
   - `profile.build` records how the recipe builds:
     - `container` in a named image, either the recipe's own `container_image` (`recipe`) or
       one `plan --build-image` configured for a Linux recipe without a pin (`configured`);
     - `trusted_host`, for an unpinned Linux recipe planned with `--trusted-host-build`;
     - `native_host`, for a macOS recipe.
   - `profile.recipe_digest` covers the recipe and that build, so plans whose effective builds
     differ have different recipe identities, which history can tell apart.
   - Planning refuses an unpinned Linux recipe without either option, both options together, a
     configured image that is not exactly the recipe's pin, a host build of a pinned recipe,
     and either option for a macOS recipe. A diagnostic child plan takes its parent's build.
   - The build reads the frozen build. `build --build-image` may only restate the frozen image:
     anything else is refused (`build_image_mismatch`) before anything is created. The build's
     `--trusted-host-build` is removed.
3. **Canonical image identities.** An image is pinned as either canonical form:
   - `sha256:<image ID>`, the image whose inspected ID is exactly it;
   - lowercase `name@sha256:<manifest digest>` without a tag, an image whose repository digests
     hold exactly that reference.

   A recipe may pin either form. The build records the frozen reference and the image's own ID.
   `compare` requires the recorded reference to be exactly the frozen one, and for an image ID
   also the recorded ID. A frozen host build must have run on a host.
4. **The host backstop covers the setup.** A supervised request's host backstop is the setup's
   deadline and cleanup grace, plus the child's deadline and cleanup grace, plus the host margin.
   Neither timed region changes. The run's total deadline dominates it: the backstop never
   reaches past it, and nothing is launched once it has passed.
5. **Every declared Finding is checked** (`af.perf-cli/3`). Three rules are added:
   - `stdout_enumerates` requires every complete stdout line to match a declared pattern,
     whose group `n` numbers the item. The numbers must be exactly `first..last`, and
     `distinct` groups must differ between lines.
   - `json_enumerates` requires every element of a list to match exactly one item number's
     declared fields, `{n}` standing for its number and `numbered` fields holding it as an
     integer. Every number must be matched exactly once, and `distinct` fields must differ.
   - `json_tally` requires a summary object's counts to be exactly those of a field over a list.

   The ledger requires each of the 100 Findings once on its own line, with its own key, minor,
   open, ungrouped and at the line its title numbers. The report requires each Finding once,
   with its own key, title, open status, minor severity, file and line, and a findings summary
   that tallies them. Documented fields the fixture does not declare stay free.
6. **One run, one start, one total deadline.**
   - **Established by the build.** The build is the run's first execution. `af.perf-builds/5`
     records the run's `started_unix_ms` and `deadline_unix_ms`, the start plus the plan's frozen
     `run_total_minutes`. A caller's `--deadline-unix-ms` only tightens the build's own deadline.
     Every build step stops at the earlier of its own deadline and that one, its whole session
     ended, and no step starts after it.
   - **Read by every shard.** Each shard reads the run deadline from its verified build and
     never resets it.
     - A shard refuses before its preflight once the deadline has passed (`run_deadline`), and
       when its clock reads before the run's start (`clock_skew`).
     - Its own deadline is the earliest of the run's deadline, a caller's tighter one and its
       sampling deadline.
     - Every slot's worst case, a pending seed's writing included, must end 30 s before that
       deadline, which is kept free for the terminal record. Otherwise the rest stays
       unstarted and the shard is `interrupted` with the deadline that bound it.
   - **Recorded.** `af.perf-shard/4` records the run's execution, the caller's deadline and the
     shard's effective deadline.
   - **Checked by `compare`.** The run deadline must be the plan's frozen total, and every
     shard must name its build's run. A build that ended after the run deadline, a shard that
     ended after it, or a record that started after its shard's deadline is
     `run_deadline_exceeded`, and the run is incomplete.

## Consequences

- **Versions.** The earlier versions of the seven contracts in the table above are refused with
  no reader. Their schemas are replaced by `performance-run-v3.json`,
  `performance-builds-v5.json`, `performance-shard-v4.json`,
  `performance-probe-request-v2.json`, `performance-golden-v2.json`, `performance-cli-v3.json`
  and `performance-suite-v3.json`. The probe's message stays `af.perf-probe/1`.
- **Regenerated.** The golden entries were regenerated by the reference generator; the source
  capture and state projection entries are those of their `_v2` contracts. The synthetic
  comparison bundles were also regenerated.
- **Unchanged bounds.** The 15-second production lease, the 8-MiB envelope cap, every pair
  count, warmup, sample timeout and resource bound are unchanged. Old fixtures are not read.
- **Adapters.** The probe's adapter serves the two writer contracts and the two `_v2` readers.
  `review-pipeline` gains a build script that records cargo's target layout, as
  `review-process` does, so its test can build the `perf-probe` example into its own target.
- **Real processes.** The probe's integration test runs the built `perf-probe` example as
  independent processes for the cold and prewarmed projections and both source sizes:
  - each writer once, judged by the trusted driver against its golden seed entry;
  - two samples per reader, each on its own copy of the one sealed seed;
  - a sample on a copy it damaged fails alone, and the next fresh copy passes;
  - the sealed seed's identity is unchanged after every sample, and changed bytes change it.
- **Stand-in shards.** The Python suite runs native seeds through real shards with stand-in
  binaries:
  - one write per side and seed;
  - private copies with the copy timed as setup;
  - a failed or mismatched writer fails only its side and is never retried;
  - a tampered seed stops the shard, and seeds are removed on every path;
  - `compare` refuses a seeded record without its side's seed.
- **Deadline tests.** These use deterministic clocks and real processes. A build step and its
  escaped descendant end at the run deadline, a supervisor near both of its limits is not
  killed, and a delayed shard leaves the rest unstarted.
- **Validated on the development host** (macOS arm64, Docker Desktop's Linux arm64 VM, Rust
  1.88.0, load average 11 to 79 from other work):
  - **Tests.** The laboratory's 291 Python tests pass under the host's Python 3.15, and as a
    non-root user under Python 3.9.25 and 3.11.17 in containers. The probe's 27
    integration tests pass, the three seed tests included. In one full run an authority sample
    lost its 15-second production lease after 26.7 s of setup under load and passed when
    rerun; no lease or deadline was changed.
  - **A real container build of the frozen image path.** The source was an owned Git fixture
    commit of this tree, made before this ADR's text was final. A same-source partial plan of
    the musl shipping recipe froze the local image `sha256:62e76ea1…` with
    `plan --build-image`, and a plan without it was refused (exit 2).
    - `build` with another image was refused (`build_image_mismatch`, exit 4) before anything
      was created.
    - `build` without the option ran exactly the frozen image: the dependency fetch in a
      container with network, the supervisor, `af` and probe compiles in containers without
      it. The executables are static x86-64, and the embedded commit was verified.
    - `builds.json` (`af.perf-builds/5`) records that image and the run's start with its
      120-minute deadline. One attempt was made, and it succeeded. This does not explain the
      earlier `E0463` failures on Docker Desktop bind mounts.
  - **The actual aggregator over that build.** `cli.py aggregate` reported no build finding
    (incomplete only for the shards no host here can run). The same record with the reference
    respelled `af-perf-shipping-validation@sha256:62e76ea1…` was `builds_invalid`. A shard of
    the plan refused here with `host_mismatch` (exit 4) before creating anything: this host has
    neither an x86-64 Linux environment nor a verified 2-GiB scratch.
  - **A diagnostic seeded sample with the real binaries.** `cli.py probe` ran the prewarmed
    projection with debug builds of the probe and supervisor. The probe wrote its seed, the
    writer's check matched its golden entry, and the sample passed against its own; the
    namespace was removed.
- **Not evidence.** None of this is calibrated performance, a hosted x86-64 run or a 2-GiB
  Linux scratch.
