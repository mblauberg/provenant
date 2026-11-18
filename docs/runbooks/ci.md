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

`ci-status` is the single required check (also named in `MAINTAINING.md` and
`github-workflow.md`). On a PR synchronize event, a verified green previous PR
head carries Rust and macOS coverage forward only if its tree differs by plain
non-code paths; otherwise PRs select from the merge-base diff. Main and `land/**`
pushes always run the full pipeline; missing Git history selects conservatively.

The workflow and selectors define the jobs and their scope. Pull requests select
affected crates and reverse dependencies; uncertain selection expands coverage.
See [`select-affected-crate-tests`](../../scripts/select-affected-crate-tests)
and [`select-macos-checks`](../../scripts/select-macos-checks). Prose changes
still run documentation and harness checks. See [test tiers](test-tiers.md)
for host verification.

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
against `cargo nextest list`, excludes them from blocking partitions and reports
their result in an advisory step. Remove the entry in the PR that fixes the test.

## Tool downloads

Use [`download-with-retry`](../../scripts/download-with-retry) for workflow
tool downloads. Keep a tool's version and cache key aligned.
