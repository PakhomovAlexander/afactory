# ADR-2026-11-10-report-provider-registry-namespace-refusals: Explain namespace ownership refusals

Status: proposed (2026-11-10).

## Context

In an agent's Linux user namespace, ancestor directories can report an overflow UID because
their owners are not mapped. The Provider registry's ownership check refuses them correctly,
but suggests moving the registry. Every permitted destination can have the same problem.
Status then probes ambient login candidates and exits successfully, making a registry refusal
look like missing configuration or credentials.

## Options

- Trust UID 65534, read-only mounts, or mount boundaries: rejected. An overflow UID does not
  identify the underlying owner, and another mount namespace may modify the backing paths.
- Copy credentials, change ownership, or request login: rejected. None establishes the missing
  ancestor ownership evidence, and these actions introduce unrelated side effects.
- Keep the refusal and distinguish it from an empty registry: chosen.

## Decision

Bounded reads of Linux's UID map and configured overflow UID may explain an existing ownership
refusal. They never authorize access. Missing, malformed or inconclusive evidence retains the
ordinary refusal. A mapped foreign UID remains foreign, including a genuinely mapped 65534.

A registry read or validation failure suppresses ambient Provider discovery. `af provider status`
returns the existing standard error result, exit 1 and `af/error@2` when JSON was requested.
Successful status documents retain their existing schema. A genuinely absent registry still
supports first-use ambient discovery. This refines the status behavior of ADR-0112 while retaining
ADR-0111's ownership and publication requirements.

## Consequences

The operator receives the failure before any unrelated Provider probe or login suggestion.
The diagnosis names the authorized host environment or a corrected namespace mapping as the
recovery path. AF cannot repair a caller's namespace or authorize a host execution route itself.
Task execution still requires verifiable Provider ownership; this change does not make the
restricted environment capable of running Tasks.
