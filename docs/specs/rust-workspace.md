# Rust workspace rules

Owner: the chairs; manifests in `tools/front-door/Cargo.toml`
Anchor: `tools/front-door/Cargo.toml` (workspace dependencies and lints),
`crates/test-support/src/lib.rs::root_inputs`, `crates/core/src/time.rs::Clock`

Engineering rules for the Rust workspace under `tools/front-door/`, the sole
runtime ([ADR 0021](../adr/0021-configured-workspace-dispatch-boundaries.md)).
Paths are relative to `tools/front-door/`.

## Crate layering

Member manifests declare internal dependency edges. Libraries never depend on
the CLI, and edges follow the owning crate boundaries; a change to a permitted
edge updates the workspace policy gate in the same PR. `provenant` is the only
executable.

- Why: one-way edges keep policy testable without the command layer, and typed
  events break cycles instead of an import of `run` into `providers` or
  `routing`.
- Owner: member manifests in `crates/`; `tests/workspace_policy.rs`

## Dependencies

Direct dependencies use the audited `[workspace.dependencies]` declaration
([ADR 0028](../adr/0028-rust-port-dependencies.md) decision 3). A new crate,
feature or material transitive addition needs a recorded reason.

- Why: a small, audited supply chain with bounded, synchronous code.
- Owner: `Cargo.toml`, `deny.toml`

## Errors

Libraries return typed error enums. Production code has no `unwrap`, `expect`,
`panic!` or direct printing, and the binary edge owns wording and exit codes.
Errors keep cause and path, never secret values. Publication and recovery never
rely on `Drop`.

- Why: a panic or silent success on a failed write loses evidence or custody.
- Owner: `Cargo.toml` (`[workspace.lints]`)

## Unsafe code

First-party `unsafe` stays behind the declared safe boundaries, and only the
supervisor may use OS process internals. Each `unsafe` block documents its
invariants and needs a second reviewer.

- Why: process custody is the safety core; one audited boundary is reviewable.
- Owner: `Cargo.toml`, `clippy.toml`

## Numbers, digests and diagnostics

Integers, never floats, and `serde_json`'s `arbitrary_precision` stays off.
Digest bytes come from the canonical encoder, never map iteration order.
Diagnostics go to bounded stderr, never result bytes. Output masking happens only
when the opt-in `redaction` knob is on
([ADR 0035](../adr/0035-three-hard-boundaries-and-flexible-everything-else.md)).

- Why: float or map-order digests change identity, and diagnostics must not
  corrupt result bytes.
- Owner: `crates/records/src/canonical.rs`

## Test isolation

Tests isolate user state and use controlled time and synchronisation.

- Why: tests must not alter a real installation or depend on timing races.
- Owner: `crates/test-support/src/lib.rs` (`root_inputs`, `wait_for`,
  `process_is_gone`, `assert_process_gone`), `crates/core/src/time.rs::Clock`

## Bounded debug stack frames

Graph executor frames stay small: large locals and futures are boxed and the
largest phases are separate functions, so no debug-build frame pools a whole
phase's temporaries. Every thread that runs graph execution takes
`GRAPH_STACK_BYTES`, because one launch still nests deeper than the 2 MiB
default of a spawned debug-build thread; no thread sets its own number.
`.cargo/config.toml` repeats the size as `RUST_MIN_STACK`, the only way to size
libtest threads, and a test pins the two together.

- Why: one named size keeps production workers and test harness threads from
  drifting apart.
- Owner: `crates/fabric-runtime/src/graph.rs` (`GRAPH_STACK_BYTES`),
  `src/check_providers.rs`, `src/status_wait.rs`, `.cargo/config.toml`

## One owner per mechanism

Each mechanism has one implementation; callers supply the policy that varies.

- Why: duplicate mechanisms drift and make fixes incomplete.

## One owner per command domain

Each command path has one owning domain module under `src/commands/` and one
entry in `DOMAINS`; `run()` looks the parsed path up and calls its verb. A
domain module declares its paths, so usage verbs and the consumer contract's
command list derive from the same table instead of a second string list.

- Why: a new verb edits one domain and its registration, not an 800-line
  dispatch match in another file; a path cannot silently lose its owner.
- Owner: `src/commands/mod.rs` (`DOMAINS`, `Domain`, `Verb`).

The grammar's external subcommand (`hook <provider>`) has no static leaf, so
its domain keeps the pre-registry dispatch route that the hook handler needs.

## Build for change; refuse what can lose data

Build modular mechanisms around a declared core and make variable behaviour
configurable. Design for foreseeable cases when it makes change easier. Prefer
warnings or retries to hard holds except at the
[hard boundaries](../adr/0035-three-hard-boundaries-and-flexible-everything-else.md).

- Why: extensible ownership reduces the cost of change without losing custody.
