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
`github-workflow.md`). PRs select from the merge-base diff. Main and `land/**`
pushes always run the full pipeline; missing Git history selects conservatively.

| Job | Trigger | What it proves |
|---|---|---|
| `macos-scope` | Every run | Selects Rust and native work from changed paths; unavailable history selects everything. |
| `rust-lint` | Every run; Cargo steps only for Rust inputs | fmt, isolated fixture features, Clippy with `-D warnings`, harness assets, doc-truth, script fixtures, spelling, ShellCheck, actionlint, zizmor and local links. Own lint cache. |
| `generated-reference` | Rust inputs | The current CLI renders its contract reference; uploads the result and reports advisory reference drift. |
| `rust-test-archive` | Rust inputs | Builds one locked Linux recording-feature archive, validates all literal nextest selectors and quarantine once from its list, selects affected packages and tests the recording example. |
| `rust` (three hash partitions) | Rust inputs, after the archive | Runs the same archive with the affected filter and quarantine exclusion; uploads JUnit and runs quarantine once as advisory. No compilation or build-cache restore. |
| `rust-dependencies` | Rust inputs | Advisories, licences, dependency allowlist and unused dependencies. |
| `rust-macos-archive` | Native inputs | macOS-target Clippy and one native archive/list. This platform lint covers code absent from Linux. |
| `rust-macos-tests` (custody, provider-dispatch, dispatch, light) | Native inputs, after the archive | Parallel native jobs with unchanged filters and controls. Dispatch stays single-threaded; nextest's serial groups remain active. Light runs census, Cargo seed, capability and TLS. |
| `rust-macos-inventory` | Native inputs, after every native job, including failures | Collects all invocation reports and verifies each required native test passes exactly once with its required environment. |
| `rust-macos-release` | Native inputs | Clean release/package/install smoke without LTO, proc-macro strip regression coverage and Swift package tests. |
| `ci-status` | Always, after every required job | Rejects failures, cancellations and missing jobs; accepts skipped Rust/native jobs only when the selector explicitly unselected them. |

Plain Markdown in docs, skills, roles and the repository root, static documentation
images/PDFs and repository metadata such as `.gitignore` skip Rust builds/tests.
The inventory in this file remains a Rust/native trigger. Executable or structured
skill assets, scripts and unknown inputs retain Rust coverage: Rust tests embed
writing-rule JSON (`tests/checks.rs`) and policy tests read script sources, so a
blanket non-Rust extension filter would lose checks.
Harness, documentation and script checks still run for prose changes.

For PRs, tests cover each changed workspace crate and its reverse dependencies.
`scripts/select-affected-crate-tests` reads metadata from `tools/front-door`, with
paths relative to the repository. Main, `land/**` and manual runs test everything.
Unknown paths, unavailable metadata/history, lockfiles, workspace manifests,
toolchains, nextest configuration, workflow changes, build scripts and shared
assets conservatively select the full workspace. Build archives retain the full
inventory even when the PR execution filter narrows it.

Auxiliary workflows keep distinct coverage: `docs-drift` posts the weekly report,
`test-signals` provides stress/mutation signals (PR stress skips tool/build setup
when no Rust tests changed), and `verify-tiers` is an explicit
host request. Their caches restore only; none duplicates a mandatory CI dispatch.

## Required native test inventory

This table is the policy source for tests that must run natively on macOS.
`scripts/check-native-inventory` requires each row to appear in the macOS
nextest list and pass exactly once in its named invocation with the required
environment, and `scripts/check-nextest-filters` reads the same rows. The checks
cannot find a native test that has no row, so add a row with the test.

| Exact test name | Platform | Native invocation | Required environment | Nextest binary ID |
|---|---|---|---|---|
| `tests::staged_skills_native_probes` | macOS | `custody` | `PROVENANT_CONFINEMENT_REQUIRED` | `provenant-providers` |
| `permissions::tests::escape::native_sandbox_blocks_path_escape_probes` | macOS | `custody` | `PROVENANT_CONFINEMENT_REQUIRED` | `provenant-confinement` |
| `run::provider::project_paths::tests::configured_shared_path_allows_lane_lock_contended_by_host` | macOS | `custody` | `PROVENANT_CONFINEMENT_REQUIRED` | `provenant-cli` |
| `lane_prepare_isolates_nested_writer_state` | macOS | `capability` | `PROVENANT_CONFINEMENT_REQUIRED, PROVENANT_LANE_TOOLS_REQUIRED` | `provenant::worktrees` |
| `writer_production_plan_denies_outside_writes` | macOS | `dispatch` | `PROVENANT_CONFINEMENT_REQUIRED, PROVENANT_LANE_TOOLS_REQUIRED` | `provenant::worktrees` |
| `worktrees::census_tests::native_full_census_places_clean_merged_worktree_and_same_user_sleep` | macOS | `census` | `PROVENANT_CONFINEMENT_REQUIRED, PROVENANT_POSTGRES_REQUIRED` | `provenant-confinement` |
| `worktrees::census_tests::native_unreadable_windowserver_allows_cleanup_of_a_merged_clean_tree` | macOS | `census` | `PROVENANT_CONFINEMENT_REQUIRED, PROVENANT_POSTGRES_REQUIRED` | `provenant-confinement` |
| `sys::pid::tests::native_occupancy_reports_image_and_open_file_with_identity_completeness` | macOS | `census` | `PROVENANT_CONFINEMENT_REQUIRED, PROVENANT_POSTGRES_REQUIRED` | `provenant-os` |
| `sys::pid::tests::native_occupancy_never_proves_an_unqueryable_process_empty` | macOS | `census` | `PROVENANT_CONFINEMENT_REQUIRED, PROVENANT_POSTGRES_REQUIRED` | `provenant-os` |
| `sys::pid::tests::native_current_process_is_not_an_eligible_platform_daemon` | macOS | `census` | `PROVENANT_CONFINEMENT_REQUIRED, PROVENANT_POSTGRES_REQUIRED` | `provenant-os` |
| `sys::pid::tests::native_same_user_sleep_is_not_an_eligible_daemon` | macOS | `census` | `PROVENANT_CONFINEMENT_REQUIRED, PROVENANT_POSTGRES_REQUIRED` | `provenant-os` |
| `sys::pid::tests::native_offsealed_platform_process_is_refused_when_present` | macOS | `census` | `PROVENANT_CONFINEMENT_REQUIRED, PROVENANT_POSTGRES_REQUIRED` | `provenant-os` |
| `command_custom_shebang_runtime_keeps_credentials_and_writes_denied` | macOS | `provider-dispatch` | `PROVENANT_CONFINEMENT_REQUIRED` | `provenant::command_adapter` |
| `tests::claude_network_on_native_tls_files_are_readable` | macOS | `tls` | `PROVENANT_CONFINEMENT_REQUIRED` | `provenant-providers` |
| `opencode::tests::read_only_search_runs_in_native_profile` | macOS | `custody` | `PROVENANT_CONFINEMENT_REQUIRED` | `provenant-providers` |

## Time budgets and caches

Pass `--success-output immediate` to `cargo nextest run` to see `SKIP #n` lines
from passing tests whose native prerequisite is missing; the
`PROVENANT_CONFINEMENT_REQUIRED`, `PROVENANT_POSTGRES_REQUIRED` and
`PROVENANT_LANE_TOOLS_REQUIRED` variables make those checks fail instead.

Nextest marks a test slow after 10 seconds and stops it after three intervals.
Keep the CI retry count and `fail-on-flaky` unchanged; a test that needs more
time gets a commented override in `tools/front-door/.config/nextest.toml`. The
macOS archive and each native job have a 20-minute ceiling. Keep the serial
writer and isolated census invocations. Find slow cases with
`scripts/nextest-slowest` on a run's JUnit reports. Rust caches cover
`$GITHUB_WORKSPACE/tools/front-door/target`; a `build.build-dir` outside it must be added to the
action's `cache-directories`. To measure a cold run:
`gh workflow run ci.yml --ref BRANCH -f cold_cache=true` (fresh cache keys, no
caches deleted).

Only trusted pushes to `main` save Rust caches. PR, landing and auxiliary runs
restore only. rust-cache retains OS/architecture, toolchain, manifest and RUSTFLAGS
keys; explicit shared keys distinguish dev/default, lint/all-features,
Linux test/recording and macOS test/default. Linux compile jobs choose lld through
CI `RUSTFLAGS`; local macOS builds keep their native linker.

The test profile uses `debug = 0`; no backtrace-content test requires line tables.
The dev profile keeps line tables. Release keeps `opt-level = "z"`, unwind and the
proc-macro strip override, and chooses thin LTO with four codegen units to reduce
serial optimisation/link work. This may modestly increase binary size; there is
no measured size delta or numerical size limit in the existing release gate.
No timing experiments were run. The chair verifies CI and the release artifact
size before promotion.

Method-level source bans live in `clippy.toml`, including imported/renamed sleep
calls; the bounded `test-support::wait_for` has an explicit allowance. Remaining
source scanners in `workspace_policy.rs` enforce Git arguments/environment pairing
(and shell/Python fixtures), reserved OS namespace imports/aliases/globs, and
adapter identity values/patterns. Clippy method/type/macro bans cannot express
those contextual rules; manifest dependency/lint checks also remain.

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

Workflow tools not installed by `taiki-e/install-action` use
`scripts/download-with-retry` (doubling backoff on connection failures, 408, 429
and 5xx; gives up after 180 seconds or five attempts; other HTTP errors fail at
once). Every tool-install step has a `timeout-minutes`. The
`.github/actions/install-nextest` action pins nextest once and falls back to
`cargo install --locked`; its calling step supplies the timeout because
composite actions cannot. Tool archive caches are keyed by version and checksum,
so change a pin and its key together.
