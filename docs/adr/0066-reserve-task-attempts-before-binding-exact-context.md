# ADR-0066: Reserve Task Attempts before binding exact context

Status: accepted for the Task increment implementation; unreleased.

## Context

Review's rendered input includes the real Attempt ID and reservation alongside its Round,
Subject and captured package authority. Rendering before reservation cannot supply that identity.
Predicting the next ID races parallel dispatch and retries. Holding the common Store lock while
rendering also prevents a domain adapter from reading the same serialized Store connection.

## Decision

The common runtime persists an unstarted `Reserved` execution record before pure context
capture. The Store returns a Rust capability containing the actual Attempt, invocation, writer
epoch, allowance and admitted retry feedback. It cannot be deserialized from Worker output.
The host renders with that capability outside the Store lock. The trusted domain validates the
context against the same capability before the Store persists `ContextBound` and returns an
executable prepared capability. Starting work requires that exact bound context and writer.

```text
common Store       pure domain capture       common Store          Worker
     |                     |                       |                  |
  reserve ---------------->|                       |                  |
     | real Attempt ID     | render exact input    |                  |
     |                     +-- context + capability --> validate/bind |
     |                                             |-- start -------->|
     |                                             |<-- result/usage -|
     |                                             | settle + publish |
```

Preparation failure releases the unstarted reservation. No Attempt begins and no model usage
is charged. A crash or lost lease leaves recovery responsible for releasing unstarted work;
started work retains its existing conservative charge. Context capture consumes the original
Attempt deadline, and binding does not extend either the reservation or Task allowance. A
context cannot be rebound, including under the same writer. Changed or stale capabilities fail.

The public host and authority interfaces have reservation-aware hooks. Existing command and
model protocols delegate to their captured context validation; adapters that include Attempt
authority can require it explicitly. Captured-host and Provider wrappers preserve those hooks.
The historical combined `Prepared` record and Store API remain readable and usable; new common
runtime dispatch uses the two durable transitions. This adds variants only to the unreleased
Task contract and does not change frozen Review Kernel lifecycle events.

ADR-0065's zero-Attempt context-failure guarantee means zero **started** Attempts and zero spend.
Such a failure can now have a released reservation in its inspectable execution history. Its
bounded diagnostic remains in the durable run report.

## Verification and remaining migration

Store tests refuse unbound starts and rebinding, release an unbound old-writer reservation after
reopening, fence stale capabilities and refuse release of started work. A runtime fixture renders
its actual reservation into the context, rejects a changed identity before work, preserves the
seven-token result across replay, and retains zero spend on rejected context. Existing command,
model retry, Provider admission and domain-publication recovery tests use this same path.

This establishes the common preparation boundary. Legacy Review command migration still needs
its domain result and selected-evidence adapters, bounded owned Scatter and Round continuation.
