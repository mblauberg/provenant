# ADR 0021: Crate layering, one execution owner and delegation bounds

**Status:** Accepted

Changed: 2026-11-11, #3233.

## Context

Provenant ships one Rust binary built from many library crates. Without one-way
edges, policy cannot be tested without the command layer, and a second launch
path, scheduler or lifecycle store can grow beside the first.

## Decision

The `provenant` executable is the only binary, composed from the member crates
under `tools/front-door/crates/`. A member crate may depend only on the internal
crates the `LAYERS` table in `tools/front-door/tests/workspace_policy.rs` allows.
Bottom up:

- `core` has no internal dependencies. `os` and `records` sit on `core`, and
  `sqlite` on `core` and `os`.
- `config`, `supervisor`, `telemetry` and `quota` sit on those foundations.
  `confinement` builds on `config` and `supervisor`. `providers` builds on
  `confinement` and `quota`, and `routing` on `providers` and `quota`.
- `run` builds on `confinement`, `supervisor` and `sqlite`. It does not depend
  on `providers` or `routing`.
- `fabric-store` and `workflow` sit on `sqlite`. `fabric-runtime` composes
  `routing`, `confinement` and `fabric-store`.
- `cli` composes the domain crates. Libraries never depend on `cli`.

Execution has one owner per concern. Routing resolves routes, confinement
compiles access plans, providers prepare launches, supervisor owns processes,
and run owns attempts and terminal evidence. The CLI composes
those owners and adds no adapter parser, scheduler or lifecycle store.

Delegation can narrow authority, never widen it. Access boundaries are those of
[ADR 0035](0035-four-hard-boundaries-and-flexible-everything-else.md) and
[ADR 0040](0040-open-lane-policy-with-explicit-boundaries.md).

Rust is the sole runtime for Provenant-owned execution. The external provider,
hook and skill contracts remain owned by those tools.

## Why

- One-way edges keep policy testable without the command layer. Typed events
  break cycles instead of importing `run` into `providers`.
- One binary and no interpreter on the call path remove per-call startup cost.
  No Python or TypeScript fallback is kept.
- One launch path keeps CLI behaviour aligned. Attempt evidence has one
  owner, so a provider's success does not certify delivery acceptance.
- Existing seams are completed under change pressure. No universal dispatcher,
  unit of work or event framework is added before a concrete need.

## Consequences

Each subsystem has one implementation. A new internal edge changes `LAYERS` in
the same pull request, and the policy test enforces it. Dependency choices are
owned by [ADR 0028](0028-rust-port-dependencies.md). The
[workspace spec](../specs/rust-workspace.md) states the rules that apply to
each crate.
