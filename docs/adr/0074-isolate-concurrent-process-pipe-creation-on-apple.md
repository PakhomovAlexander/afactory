# 0074 — Isolate concurrent process pipe creation on Apple

Status: accepted. Date: 2026-09-12.

## Context

A completed fake model occasionally retained stdout, and a killed model occasionally waited
for an unrelated process to close it. Pinned Rust 1.88 uses separate pipe and close-on-exec
calls on Apple platforms, so another concurrent spawn can inherit the transient descriptor.

## Decision

On Apple platforms the pinned Rust standard library creates pipes before setting close-on-exec.
The shared buffered and duplex spawn paths therefore serialize child creation through one short
critical section. This prevents supervised siblings from inheriting one another's transient pipe
ends. Execution, waiting and draining remain concurrent. It does not change unrelated callers
of `std::process::Command`. Concurrent short-lived and timed-out process fixtures retain separate
output and bounded lifetimes; they also check that Worker execution has not become serialized.

This supplements the shared process boundary in ADR-0026 and captured-output behavior in
ADR-0072. A slower drain or a longer Worker deadline does not close the inheritance window.
The critical section covers pipe creation and spawn, not paid Worker execution.

## Verification

The original model-supervision suite reproduced a killed-child drain delay. After the shared
spawn change, it passed twenty consecutive whole-suite runs. The concurrent process fixture
runs twelve Workers across twenty-four batches and checks output, deadline isolation and a
bound that excludes serialized Worker execution. The complete checkpoint gate passed.
