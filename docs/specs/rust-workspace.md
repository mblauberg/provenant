# Rust workspace rules

Owner: the chairs; manifests in `tools/front-door/Cargo.toml`
Anchor: `tools/front-door/Cargo.toml` (workspace dependencies and lints),
`crates/test-support/src/lib.rs::root_inputs`, `crates/core/src/time.rs::Clock`

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
Diagnostics go to bounded stderr, never result bytes; local text is never
masked, and `redaction` applies only to text published off the host.
- Why: float or map-order digests change identity, and diagnostics must not
  corrupt result bytes.
- Owner: `crates/records/src/canonical.rs`

## Test isolation

Tests isolate user state and use controlled time and synchronisation.
- Why: tests must not alter a real installation or depend on timing races.
- Owner: `crates/test-support/src/lib.rs` (`root_inputs`, `wait_for`, `process_is_gone`, `assert_process_gone`),
  `crates/core/src/time.rs::Clock`

## Bounded debug stack frames

Graph executor frames stay small and the threads that run graph execution share
one stack size. Large executor locals and futures are boxed and the largest
phases (the scheduling pass, gate evaluation, attempt admission, observation
recording, the launch preparation) are separate functions, so no debug-build
frame pools a whole phase's temporaries. The nesting of one launch still exceeds
the 2 MiB default of a spawned debug-build thread, so every thread that runs
graph execution takes `GRAPH_STACK_BYTES`; no thread sets its own number.
- Why: a launch overflowed default-stack threads in debug builds. Small frames
  keep the headroom wide; one named size keeps production workers and test
  harness threads from drifting apart.
- Test harness: `.cargo/config.toml` repeats the size as `RUST_MIN_STACK`
  (the only way to size libtest threads) and a test pins it to the constant.
- Owner: `tools/front-door/crates/fabric-runtime/src/graph.rs`
  (`GRAPH_STACK_BYTES`), `tools/front-door/src/fabric/owner.rs`,
  `tools/front-door/crates/cli/src/run/launch.rs`,
  `tools/front-door/src/check_providers.rs`, `tools/front-door/.cargo/config.toml`

## One owner per mechanism

Each mechanism has one implementation; callers supply the policy that varies.
- Why: duplicate mechanisms drift and make fixes incomplete.

## Build for change; refuse what can lose data

Build modular mechanisms around a declared core and make variable behaviour
configurable. Design for foreseeable cases when it makes change easier. Prefer
warnings or retries to hard holds except at the [hard boundaries](../adr/0035-three-hard-boundaries-and-flexible-everything-else.md).
- Why: extensible ownership reduces the cost of change without losing custody.
