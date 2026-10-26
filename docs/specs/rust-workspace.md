# Rust workspace rules

Owner: the chairs; manifests in `tools/front-door/Cargo.toml`
Anchor: `tools/front-door/tests/workspace_policy.rs`

Durable engineering rules for the Rust workspace under `tools/front-door/`.
[ADR 0028](../adr/0028-rust-port-dependencies.md) sets the dependency list and
[ADR 0029](../adr/0029-rust-rewrite-in-one-pass.md) records the rewrite. Paths
are relative to `tools/front-door/`.

## Crate layering

Each member crate may depend only on the internal crates that the `LAYERS`
table in `tests/workspace_policy.rs` allows; a manifest may declare fewer. No
library depends on `cli`, `os` never depends on `supervisor`, and `routing`
never depends on `run`. A change to a crate's edges updates `LAYERS` in the
same PR. The binary `provenant` is the only executable.
- Why: one-way edges keep policy testable without the command layer; typed
  events break cycles, never an import of `run` into `providers` or
  `routing`.
- Test: `tests/workspace_policy.rs::internal_edges_obey_the_layering_table`;
  `::libraries_cannot_depend_on_cli`
- Owner: `tests/workspace_policy.rs` (`LAYERS`), each member's `Cargo.toml`

## Dependencies

Direct dependencies come only from the `DIRECT` list in
`tests/workspace_policy.rs` (ADR 0028 decision 3), declared once in
`[workspace.dependencies]`; `tempfile` is dev-only. A new crate, feature or
material transitive addition changes this rule with a reason. So far:
`rusqlite` also serves run's store, keeping admission atomic in one
database, and `toml_edit` enables `serde` for typed config. There is no
async runtime and no daemon.
- Why: a small, audited supply chain with bounded, synchronous code.
- Test: `tests/workspace_policy.rs::every_member_obeys_the_direct_dependency_policy`;
  `::transitive_crates_and_renamed_dependencies_are_rejected`
- Owner: `Cargo.toml` (`[workspace.dependencies]`), `deny.toml`

## Errors

Libraries return `thiserror` enums, including refusal, contention, cancel
and I/O. Production code has no `unwrap`, `expect`, `panic!` or direct
printing; the binary edge owns wording and exit codes. Errors keep cause and
path, never secret values. Publication and recovery never rely on `Drop`.
- Why: a panic or silent success on a failed write loses evidence or
  custody.
- Test: workspace clippy lints (`unwrap_used`, `expect_used`, `panic`,
  `print_stdout`, `print_stderr`), run in CI;
  `crates/supervisor/tests/custody.rs::owner_death_keeps_guardian_and_writer_live`
- Owner: `Cargo.toml` (`[workspace.lints]`)

## Unsafe code

First-party `unsafe` lives only in `os::sys` and supervisor's private
`process_os`, behind safe wrappers; every other crate forbids it. Only
supervisor may use OS `sys::{fs, child, raw_exec, reaper}`, `handoff_file`
and `from_handoff`, and `std::process::Command::new` is disallowed outside
those boundaries. Each block documents its invariants and gets a second
reviewer.
- Why: process custody is the safety core; one audited boundary is
  reviewable.
- Test: `tests/workspace_policy.rs::os_internals_are_reserved_for_supervisor`;
  `::native_boundaries_mirror_workspace_lints_except_for_unsafe_code`. The
  second review is a review gate.
- Owner: `Cargo.toml` (`unsafe_code = "forbid"`), `clippy.toml`

## Numbers, digests and diagnostics

Integers are `i64` or `u64`, never `f64`, and `arbitrary_precision` stays
off. Digest bytes come from the canonical encoder, never `HashMap` order.
Diagnostics go to bounded, redacted stderr, never MCP stdout or result
bytes, and never carry prompt or credential contents.
- Why: float or map-order digests change identity, and protocol streams must
  stay parseable.
- Test: `crates/records/tests/contracts.rs::canonical_v1_rejects_float_values`;
  `crates/fabric-mcp/src/stdio_tests.rs::oversize_frame_emits_a_diagnostic_and_broken_stdout_stops_writes`
- Owner: `crates/records/src/canonical.rs`, `crates/cli/src/render.rs`

## Test isolation

Every test points `HOME`, the state root, XDG and Git config and harness
config paths at temporary directories, so no test touches a real install.
Tests use explicit clocks and handshakes, never sleeps.
- Why: a test that touches the real install can destroy user state.
- Test: `crates/cli/src/check/dates.rs::workspace_rust_test_dates_use_injected_clocks`;
  clippy `disallowed_methods`. Home isolation has no guard test (gap).
- Owner: `tests/support/mod.rs`

## Simple first

Build the simplest design that is correct for the main path and the failures
that matter: data loss, credential exposure, killing a healthy provider,
writing outside the selected roots and silent wrong answers. Other edge cases
get a typed refusal or a PR note, and reviews block only on those classes or
a broken main path. Never mute `ci-status`.
- Why: guardrails for unobserved risks cost more than they save.
- Test: none (review gate)
- Owner: code review
