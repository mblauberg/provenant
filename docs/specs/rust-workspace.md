# Rust workspace rules

Owner: the chairs; manifests in `tools/front-door/Cargo.toml`
Anchor: `tools/front-door/tests/workspace_policy.rs`

Engineering rules for the Rust workspace under `tools/front-door/`, the sole
runtime ([ADR 0021](../adr/0021-configured-workspace-dispatch-boundaries.md)). Paths are relative to
`tools/front-door/`.

## Crate layering

A member crate depends only on the internal crates the `LAYERS` table in
`tests/workspace_policy.rs` allows; the table is the rule, and a change to a
crate's edges updates it in the same PR. `provenant` is the only executable.
- Why: one-way edges keep policy testable without the command layer; typed
  events break cycles instead of an import of `run` into `providers` or `routing`.
- Test: `tests/workspace_policy.rs::internal_edges_obey_the_layering_table`;
  `tests/workspace_policy.rs::libraries_cannot_depend_on_cli`
- Owner: `tests/workspace_policy.rs` (`LAYERS`)

## Dependencies

Direct dependencies come only from the `DIRECT` list in
`tests/workspace_policy.rs` ([ADR 0028](../adr/0028-rust-port-dependencies.md)
decision 3), declared once in `[workspace.dependencies]`. A new crate, feature
or material transitive addition needs a recorded reason in that list.
- Why: a small, audited supply chain with bounded, synchronous code.
- Test: `tests/workspace_policy.rs::every_member_obeys_the_direct_dependency_policy`;
  `tests/workspace_policy.rs::transitive_crates_and_renamed_dependencies_are_rejected`
- Owner: `Cargo.toml`, `deny.toml`

## Errors

Libraries return typed error enums; production code has no `unwrap`,
`expect`, `panic!` or direct printing, and the binary edge owns wording and
exit codes. Errors keep cause and path, never secret values. Publication and
recovery never rely on `Drop`.
- Why: a panic or silent success on a failed write loses evidence or custody.
- Test: `crates/supervisor/tests/custody.rs::owner_death_keeps_guardian_and_writer_live`
- Owner: `Cargo.toml` (`[workspace.lints]`)

## Unsafe code

First-party `unsafe` stays behind the declared safe boundaries; only the
supervisor may use OS process internals.
Each `unsafe` block documents its invariants and needs a second reviewer.
- Why: process custody is the safety core; one audited boundary is reviewable.
- Test: `tests/workspace_policy.rs::os_internals_are_reserved_for_supervisor`;
  `tests/workspace_policy.rs::native_boundaries_mirror_workspace_lints_except_for_unsafe_code`
- Owner: `Cargo.toml`, `clippy.toml`

## Numbers, digests and diagnostics

Integers, never floats, and `serde_json`'s `arbitrary_precision` stays off.
Digest bytes come from the canonical encoder, never map iteration order.
Diagnostics go to bounded, redacted stderr, never result bytes.
- Why: float or map-order digests change identity, and diagnostics must not
  corrupt result bytes.
- Test: `crates/records/tests/contracts.rs::canonical_v1_rejects_float_values`
- Owner: `crates/records/src/canonical.rs`

## Test isolation

Tests isolate user state and use controlled time and synchronisation.
- Why: tests must not alter a real installation or depend on timing races.
- Test: `crates/cli/src/check/dates.rs::workspace_rust_test_dates_use_injected_clocks`
- Owner: `tests/support/mod.rs`

## One owner per mechanism

Each mechanism has one implementation; callers supply the policy that varies.
- Why: duplicate mechanisms drift and make fixes incomplete.
- Test: none (review gate)

## Build for change; refuse what can lose data

Build modular mechanisms around a declared core and make variable behaviour
configurable. Design for foreseeable cases when it makes change easier. Prefer
warnings or retries to hard holds except at real boundaries of credentials, Git
isolation, data integrity and authority.
- Why: extensible ownership reduces the cost of change without losing custody.
- Test: none (review gate)
