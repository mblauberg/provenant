# Continuous integration
Owner: repository maintainers
Anchor: [`ci.yml`](../../.github/workflows/ci.yml); [`check-native-inventory`](../../scripts/check-native-inventory)

The required check, suite tiers and green-evidence rules are in the
[CI spec](../specs/ci.md); the design is in
[ADR 0041](../adr/0041-tiered-ci-from-green-evidence.md).

## Rust front-door tests

Keep fixtures independent of host capacity and time: inject `Resources`
observations, an `AdmissionPolicy` and `provenant_core::TestClock` at admission
boundaries, and wait on a named condition with `provenant_test_support::wait_for`
and a bounded deadline instead of sleeping (`clippy.toml` rejects direct
sleeps). Tests of native process or filesystem behaviour may still need real
host facts.

From `tools/front-door`, develop with `--features fixture` (for example `cargo
check --locked --workspace --features fixture`; use `-p CRATE` for a focused
run). Release and reference-generation builds keep empty defaults so they
contain no test helpers, and CI's lint job also checks and lints the shipped
build without the feature. Cache keys name the feature set and profile; a
`workflow_dispatch` with `cold_cache` gives a cold timing comparison.

## Pipeline

Read the **Select CI suites** job summary for each suite's state, reason and
evidence. `covered` means prior green evidence covers the suite's inputs;
`deferred` means a main-tier suite runs on main. Suites are declared in
[`config/ci-suites.toml`](../../config/ci-suites.toml).

To move a slow Rust test to the main tier, put it in a module named
`main_tier`; pull requests skip it, and `main` and full runs execute it. Native
macOS tests have no module path, so they are named in `main_tests` on the
`macos_native` suite. A test that no CI tier runs uses `#[ignore = "reason"]`
and is run by hand.

To request every suite, add the `ci:full` label to the pull request and push a
commit, or run `gh workflow run ci.yml --ref BRANCH -f tier=full`. When a
`ci:red-main` issue opens, inspect its failed jobs and comparison, then fix
forward.

## Toolchain updates

Dependabot moves the root `rust-toolchain.toml` and
`tools/front-door/rust-toolchain.toml` in one grouped pull request; CI fails if
they differ. The change runs every suite and auto-merges on green `ci-status`.
New Clippy lints appear in the lint job summary; fix them on the Dependabot
branch.

## Required native test inventory

[`config/native-test-inventory.json`](../../config/native-test-inventory.json)
declares the tests that must run natively on macOS, their invocations and
required environment. `scripts/check-native-inventory` resolves it against the
platform test list and passing results. Add required native coverage to the
declaration with the test change.

## Reproduce a CI failure locally

A suite that passes locally and fails in CI usually differs by developer
configuration. Vary `HOME` before `PATH`: run with `HOME` set to an empty
directory, one environment axis per run, and subtract only the named executables
under suspicion from the real `PATH` (a minimal allowlist `PATH` manufactures
unrelated failures). Record which axis changed the outcome.

## Advisory signals and quarantine

[`test-signals.yml`](../../.github/workflows/test-signals.yml) never joins
`ci-status` and always exits 0: `scripts/assertion-diff` flags weakened
assertions (a noisy line heuristic), the stress run repeats added or changed
tests, and `scripts/run-mutants` mutates diffs nightly and on PRs labelled
`mutants`.

Quarantine a flaky test in `tools/front-door/.config/quarantine.toml` only while
it has an open issue, an owner and an expiry within 30 days. CI validates entries
against the platform `cargo nextest list` inventories, excludes them from
blocking partitions and reports their result in an advisory step. A PR without
the native list validates with `--partial-inventory`; main validates strictly
against both lists. Remove the entry in the PR that fixes the test.

## Tool downloads

Use [`download-with-retry`](../../scripts/download-with-retry) for workflow tool
downloads, and keep a tool's version and cache key aligned.
