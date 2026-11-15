# Rust workspace rules

Owner: the chairs; manifests in `tools/front-door/Cargo.toml`
Anchor: `tools/front-door/Cargo.toml` (workspace dependencies and lints)

Engineering rules for the Rust workspace under `tools/front-door/`, the sole
runtime ([ADR 0021](../adr/0021-configured-workspace-dispatch-boundaries.md)). Paths are relative to
`tools/front-door/`.

## Crate layering

Member manifests declare internal dependency edges. Libraries never depend on
the CLI; edges follow the owning crate boundaries. Changes to permitted edges
update the workspace policy gate in the same PR. `provenant` is the only executable.
- Why: one-way edges keep policy testable without the command layer; typed
  events break cycles instead of an import of `run` into `providers` or `routing`.
- Owner: member manifests in `crates/`

## Dependencies

Direct dependencies use the audited `[workspace.dependencies]` declaration
([ADR 0028](../adr/0028-rust-port-dependencies.md) decision 3). A new crate,
feature or material transitive addition needs a recorded reason.
- Why: a small, audited supply chain with bounded, synchronous code.
- Owner: `Cargo.toml`, `deny.toml`

## Errors

Libraries return typed error enums; production code has no `unwrap`,
`expect`, `panic!` or direct printing, and the binary edge owns wording and
exit codes. Errors keep cause and path, never secret values. Publication and
recovery never rely on `Drop`.
- Why: a panic or silent success on a failed write loses evidence or custody.
- Owner: `Cargo.toml` (`[workspace.lints]`)

## Unsafe code

First-party `unsafe` stays behind the declared safe boundaries; only the
supervisor may use OS process internals.
Each `unsafe` block documents its invariants and needs a second reviewer.
- Why: process custody is the safety core; one audited boundary is reviewable.
- Owner: `Cargo.toml`, `clippy.toml`

## Numbers, digests and diagnostics

Integers, never floats, and `serde_json`'s `arbitrary_precision` stays off.
Digest bytes come from the canonical encoder, never map iteration order.
Diagnostics go to bounded, redacted stderr, never result bytes.
- Why: float or map-order digests change identity, and diagnostics must not
  corrupt result bytes.
- Owner: `crates/records/src/canonical.rs`

## Test isolation

Tests isolate user state and use controlled time and synchronisation.
- Why: tests must not alter a real installation or depend on timing races.
- Owner: `tests/support/mod.rs`

## One owner per mechanism

Each mechanism has one implementation; callers supply the policy that varies.
- Why: duplicate mechanisms drift and make fixes incomplete.

## Build for change; refuse what can lose data

Build modular mechanisms around a declared core and make variable behaviour
configurable. Design for foreseeable cases when it makes change easier. Prefer
warnings or retries to hard holds except at real boundaries of credentials, Git
isolation, data integrity and authority.
- Why: extensible ownership reduces the cost of change without losing custody.
