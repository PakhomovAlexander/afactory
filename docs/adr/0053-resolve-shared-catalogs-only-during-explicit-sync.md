# ADR-0053: Resolve shared catalogs only during explicit sync

Status: accepted for the unreleased Task increment, 2026-09-11.

## Decision

Shared catalogs pin Pipeline, Worker and Task-kind packages by exact version and byte digest.
Explicit `af catalog sync` resolves a Git revision once and follows bounded catalog imports
within that commit. Another Git repository requires another explicit sync. A catalog cannot
initiate arbitrary transports, follow symlinks or select local credentials. Public HTTPS sync
uses controlled, supervised Git transport; private repositories can use an existing checkout.

Sync validates package identities, transitive dependency availability and the same Worker
payload schemas used at runtime. It stages and synchronizes files, then publishes through an
atomic absent-only rename. The lock records commit, source identity, catalog provenance and
package digests. Import activation remains an explicit project catalog edit and Git commit.
Namespaces have no implicit precedence; shadowed packages fail admission.

Normal Task capture reads the committed lock and vendored files. It verifies catalog provenance
and package digests, then includes the lock identities in captured Run authority. Resume restores
that authority and never rereads mutable imports or contacts their Git sources.

Task-kind packages name installed acceptance profiles. The selected package is a plan dependency;
its business kind and profile determine normalization and required output/evidence types before
compilation. Custom Review kinds preserve Review's domain exits, including changes requested
with satisfied Review-Task acceptance. Package text cannot install a domain handler or weaken
an installed profile. Document execution remains the separately scheduled P13 capability.

## Evidence

The deterministic gate passes 740 tests with zero failures and 15 existing opt-in probes ignored,
plus formatting, Clippy and frozen synthetic reproduction. The import fixture removes its source
checkout after sync, changes vendored Worker bytes after planning and executes the captured plan.
Fresh admission rejects the mutated package. Other cases reject cycles, missing dependencies,
namespace collisions, symlinks and an existing destination. Task-kind fixtures preserve custom
names, exact dependencies, immutable profiles and the Review/implementation acceptance distinction.

The default suite performs no remote fetch or paid inference. HTTPS transport and the remaining
Provider/source combinations need their declared live boundary probes before release.
