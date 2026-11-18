# Continuous integration
Owner: repository maintainers
Anchor: [`ci.yml`](../../.github/workflows/ci.yml); [`check-native-inventory`](../../scripts/check-native-inventory)

## Rust front-door tests

Keep fixtures independent of host capacity and time: inject `Resources`
observations, an `AdmissionPolicy` and `provenant_core::TestClock` at admission
boundaries, and wait on a named condition with `provenant_test_support::wait_for`
and a bounded deadline instead of sleeping (Clippy rejects
direct sleeps through `clippy.toml`). Tests of native process or filesystem
behaviour may still need real host facts.

## Pipeline

Read the **Select CI suites** job summary for each suite's state, reason and
evidence. `covered` means prior green evidence covers its inputs; `deferred`
means the main-tier suite will run on main. Suites are declared in
[`config/ci-suites.toml`](../../config/ci-suites.toml).

To move a slow Rust test to the main tier, put it in a module named
`main_tier`; pull requests skip it and `main` and full runs execute it. The
filter is `main_tests` on the `rust` suite in the manifest. An on-demand test
that no CI tier runs uses `#[ignore = "reason"]` and is run by hand.

To request every suite, add `ci:full` to the pull request and push a commit, or
run `gh workflow run ci.yml --ref BRANCH -f tier=full`. When a `ci:red-main`
issue opens, inspect its failed jobs and comparison, then fix forward. A PR that
only repairs CI merges once `ci-status` is green.

See the [CI spec](../specs/ci.md), [ADR 0041](../adr/0041-tiered-ci-from-green-evidence.md)
and [test tiers](test-tiers.md).

## Toolchain updates

Dependabot updates the Rust toolchain pin, moving the root copy and
`tools/front-door/rust-toolchain.toml` in one grouped pull request; CI fails if
they differ. Its change requests a full suite run and auto-merges on green
`ci-status`. New Clippy lints appear in the lint job
summary; fix them on the Dependabot branch.

## Required native test inventory

[`config/native-test-inventory.json`](../../config/native-test-inventory.json)
declares the tests that must run natively on macOS, their invocations and
required environment. `scripts/check-native-inventory` resolves that declaration
against the platform test list and passing results. Add required native coverage
to the declaration with the test change.

## Time budgets and caches

From `tools/front-door`, use `--features fixture` for development: `cargo build
--locked --features fixture`, `cargo test --locked --workspace --features fixture`
(replace `--workspace` with `-p CRATE` for a focused run), `cargo clippy --locked
--workspace --all-targets --features fixture -- -D warnings`, and `cargo nextest
run --locked --workspace --features fixture`. Each manifest forwards this feature
to its dependencies; release and reference-generation commands keep empty
defaults so they contain no test helpers. CI's lint job also runs `cargo check`
and `cargo clippy --workspace --lib --bins --locked -- -D warnings` without the
feature, so the shipped build is linted as well. The root manifest's dev profile keeps
workspace line tables, removes dependency/build-script debuginfo and optimises
proc-macros; test inherits it. CI cache keys name the feature set and profile;
Linux build and both archives upload Cargo timings for comparison with the prior
PR run (use `cold_cache` for a cold comparison). The pinned Rust toolchain already
bundles LLD on x86_64 Linux; macOS keeps Xcode's system linker, with no extra local
linker installation (see `.cargo/config.toml`).

Keep CI reliable and bounded. Consult the workflow and nextest configuration
for current budgets, cache and retry settings; investigate failures rather than
pinning incidental timing or counts. Policy checks live in the repository's
scripts and tests.

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
tests with `--stress-count 20`, and `scripts/run-mutants` mutates diffs nightly
and on PRs labelled `mutants`.

Quarantine a flaky test in `tools/front-door/.config/quarantine.toml` only while
it has an open issue, an owner and an expiry within 30 days. CI validates entries
against platform `cargo nextest list` inventories, excludes them from blocking
partitions and reports their result in an advisory step. A PR without the native
list validates with `--partial-inventory`; main validates strictly against both
lists. Remove the entry in the PR that fixes the test.

## Tool downloads

Use [`download-with-retry`](../../scripts/download-with-retry) for workflow
tool downloads. Keep a tool's version and cache key aligned.
