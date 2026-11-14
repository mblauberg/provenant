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
or material transitive addition needs a recorded reason in that list. There is
no async runtime and no daemon.
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
- Test: workspace clippy lints in CI;
  `crates/supervisor/tests/custody.rs::owner_death_keeps_guardian_and_writer_live`
- Owner: `Cargo.toml` (`[workspace.lints]`)

## Unsafe code

First-party `unsafe` lives only in `os::sys`, supervisor's private
`process_os` and SQLite's private `read_only_vfs`, behind safe wrappers; every
other crate forbids it, and only supervisor may use the OS process internals.
Each `unsafe` block documents its invariants and needs a second reviewer.
- Why: process custody is the safety core; one audited boundary is reviewable.
- Test: `tests/workspace_policy.rs::os_internals_are_reserved_for_supervisor`;
  `tests/workspace_policy.rs::native_boundaries_mirror_workspace_lints_except_for_unsafe_code`;
  the second review is a review gate.
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

Every test points `HOME`, the state root, XDG, Git and harness config at
temporary directories, so no test touches a real install. Tests use explicit
clocks and handshakes, never sleeps; code reads wall time through an injected
`provenant_core::Clock`, and clippy disallows the direct `now` calls.
- Why: a test that touches the real install can destroy user state.
- Test: `crates/cli/src/check/dates.rs::workspace_rust_test_dates_use_injected_clocks`;
  clippy `disallowed_methods`. Home isolation has no guard test (gap).
- Owner: `tests/support/mod.rs`

## One owner per mechanism

Each mechanism has one implementation, and other code calls it rather than
copying it. Owners hold mechanism only: callers choose their own budgets,
retry targets and deadlines.
- Backoff and transient failure: `provenant_core::retry::Backoff` and
  `retry::is_transient` over a typed `RetryCause`. Callers supply the first
  delay, factor, cap, attempt and cause.
- Time: `provenant_core::Clock`, with `provenant_os::clock::SystemClock` as the
  production clock. Expiry reads RFC 3339 through `provenant_core::time::rfc3339`.
  Callers never read system time.
- Duration suffixes: `providers::duration::suffix_seconds`.
- Git path discovery: `provenant_os::git` (`repository_path`,
  `repository_marker`). Descriptor-bound walks in
  `crates/confinement/src/repo.rs` read the gitfile themselves, because identity
  checks must cover that read.
- Why: copies drifted, and a fix to one copy left the others wrong. Inherited
  Git directory redirects were trusted in several places.
- Test: `clippy.toml` `disallowed-methods`; unit tests in each owner crate.
- Owner: `crates/core/src/retry.rs`, `crates/os/src/git.rs`, `clippy.toml`

## Simple first

Build the simplest design that is correct for the main path and the failures
that matter: data loss, credential exposure, killing a healthy provider,
writing outside the selected roots and silent wrong answers. Other edge cases
get a typed refusal or a PR note. Reviews block by the
[code-review rubric](../../skills/code-review/SKILL.md#rubric). Never mute the
`ci-status` check.
- Why: guardrails for unobserved risks cost more than they save.
- Test: none (review gate)
