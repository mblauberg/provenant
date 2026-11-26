# Repository CI

Owner: repository CI, per [`MAINTAINING.md`](../../MAINTAINING.md)
Anchor: [`ci.yml`](../../.github/workflows/ci.yml); [`ci-suites.toml`](../../config/ci-suites.toml)

Design and alternatives: [ADR 0041](../adr/0041-tiered-ci-from-green-evidence.md).
Procedure: [CI runbook](../runbooks/ci.md#pipeline).

## One required check

`ci-status` is the only required check. It passes only when every job of a
suite selected to run succeeded. A job may skip only when the selection
recorded its suite as covered or deferred; a failed, cancelled, absent or
unmapped job fails the gate.

- Why: one stable gate lets the ruleset, landing and consumers ignore the
  pipeline's shape.
- Owner: `ci-status` in `ci.yml`; `scripts/check-ci-status`

## Suites are declared once

Every job that `ci-status` gates belongs to exactly one suite in the manifest.
The manifest decides when a suite runs; the workflow binds each job to its
suite's selection and adds no selection logic of its own.

- Why: tiers, inputs and evidence rules change in one reviewed file, not in
  scattered workflow conditionals.
- Owner: `config/ci-suites.toml`; `scripts/select-ci-suites`; `scripts/check-ci-suites`

## Cheap gates run first

A job declared as a `gate` in the manifest must finish before any job of a
gated suite starts, directly or through an intermediate job. A failed gate
leaves gated jobs skipped, so no scarce runner is claimed for a change that
does not compile; a skipped gate (its own suite covered by evidence) still lets
an independently selected gated suite run. `scripts/check-ci-suites` refuses a
workflow that drops the edge.

- Why: the cheap Linux compile check fails before macOS archive, lint and
  release jobs spend the scarce pool on the same error.
- Owner: `config/ci-suites.toml` (`gate`); `scripts/check-ci-suites`

## Main runs everything

Every push to `main` and every landing candidate runs every suite in full, with
no evidence carried forward and no test narrowing.

- Why: `main` is the backstop for every pull-request shortcut; skipping there
  would make their blind spots permanent.

## Pull requests start from green evidence

A pull-request run selects from the tree delta between the newest `main` commit
with a green `ci-status` push run that is an ancestor of the tested commit, and
the tested commit. A suite is covered only when that delta, or the delta from the
tree the same pull request's previous green run tested (where the suite ran or
was covered), changes none of the suite's inputs. That earlier tree must be
rebuilt from its recorded base and head and match its recorded hash, so a moved
base is never hidden. Missing evidence, unreadable history, a selector failure,
a pipeline change or a full request runs every suite.

- Why: coverage proven on `main` need not be repeated, and uncertainty widens
  the run instead of narrowing it.

## Unknown paths are inputs

A changed path is inert for a suite only when the manifest declares it inert for
that suite and its old and new modes are both absent or regular non-executable
files. Every other path is an input, including any executable, symbolic link or
submodule on either side of the change.

- Why: a name proves nothing about what a file does; only a declared exception
  skips work.

## Skips are named

A main-tier suite is deferred on a pull request unless the delta touches its
declared pull-request paths or a full run is requested. The `ci:macos` PR label
forces all macOS suites on the next run, before evidence reuse; dispatches also
run them. `ci:full`, full dispatch and main run every suite. macOS Clippy paths
cover every Rust file containing macOS-specific cfg, checked by
`scripts/check-ci-suites`; cross-platform changes can still break macOS and
main remains the backstop. Every Cargo package a
main-tier suite's jobs name in a `package(=NAME)` test filter has at least one
pull-request path inside that package's directory, either the whole crate or the
narrow files its filtered tests cover; a glob shared across crates does not
count, and `scripts/check-ci-suites` refuses a manifest that leaves one out.
Within a suite that runs, tests matching its declared main-tier test filter are
deferred the same way, including when an earlier run of the same pull request
covered the suite. The run summary names every suite that did not run, covered
or deferred, with its evidence commit and reason, and names the filter.

- Why: speed on pull requests must never read as coverage that did not happen,
  and a package tested by name but never watched would defer its only real test
  to `main`.
- Owner: `scripts/check-ci-suites`; `scripts/flaky-quarantine`

## Native evidence and quarantine

Whenever native macOS tests run, every declared native test must have passed and
every quarantine entry must resolve against both platform test lists. A run that
did not build the native list only warns on an entry it cannot resolve; `main`
always builds both lists, and a change to quarantine entries pulls the native
suite into its pull request.

- Why: a quarantine entry must name a real test, but a pull request that defers
  native tests cannot see the native list.
- Owner: `scripts/check-native-inventory`; `scripts/flaky-quarantine`

## Red main is visible

A failed `ci-status` on a `main` push opens, or updates, one red-main issue
naming the failed jobs and the commit range since the last green `main`; the
next green `main` run closes it. A cancelled run reports nothing, because jobs
that still run after a cancellation fail on missing evidence rather than on
`main`.

- Why: `main` is fixed forward, so its breakage must reach the owner at once.
- Owner: `scripts/report-main-health`
