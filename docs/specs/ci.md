# Repository CI

Owner: repository CI, per [`MAINTAINING.md`](../../MAINTAINING.md)
Anchor: [`ci.yml`](../../.github/workflows/ci.yml); [`ci-suites.toml`](../../config/ci-suites.toml)

Design and alternatives: [ADR 0041](../adr/0041-tiered-ci-from-green-evidence.md).
Procedure: [CI runbook](../runbooks/ci.md#pipeline).

## One required check

Anchor: `ci-status` in `ci.yml`; `scripts/check-ci-status`

`ci-status` is the only required check. It passes only when every job of a
suite selected to run succeeded. A job may skip only when the selection
recorded its suite as covered or deferred; a failed, cancelled, absent or
unmapped job fails the gate.

Why: one stable gate lets the ruleset, landing and consumers ignore the
pipeline's shape.

Owner: repository CI.

## Suites are declared once

Anchor: `config/ci-suites.toml`; `scripts/select-ci-suites`; `scripts/check-ci-suites`

Every job that `ci-status` gates belongs to exactly one suite in the manifest.
The manifest decides when a suite runs; the workflow binds each job to its
suite's selection and adds no selection logic of its own.

Why: tiers, inputs and evidence rules change in one reviewed file, not in
scattered workflow conditionals.

Owner: repository CI.

## Main runs everything

Every push to `main` and every landing candidate runs every suite in full,
with no evidence carried forward and no test narrowing.

Why: `main` is the backstop for every pull-request shortcut; skipping there
would make their blind spots permanent.

Owner: repository CI.

## Pull requests start from green evidence

A pull-request run selects from the tree delta between the newest `main`
commit with a green `ci-status` push run that is an ancestor of the tested
commit, and the tested commit. A suite is covered only when that delta, or the
delta from the tree tested by the same pull request's previous green run
where the suite ran or was covered, changes none of the suite's inputs. That
earlier tree must be rebuilt from its recorded base and head and match its
recorded hash, so a moved base is never hidden. Missing evidence,
unreadable history, a selector failure, a pipeline change or a full request
runs every suite.

Why: coverage proven on `main` need not be repeated, and uncertainty widens
the run instead of narrowing it.

Owner: repository CI.

## Unknown paths are inputs

A changed path is inert for a suite only when the manifest declares it inert
for that suite and its old and new modes are both absent or regular
non-executable files. Every other path is an input, including any executable,
symbolic link or submodule on either side of the change.

Why: a name proves nothing about what a file does; only a declared exception
skips work.

Owner: repository CI.

## Skips are named

A main-tier suite is deferred on a pull request unless the delta touches its
declared pull-request paths or a full run is requested. The run summary names
every suite that did not run, whether it was covered or deferred, its evidence
commit and the reason. Within a suite that runs, tests matching the suite's
declared main-tier test filter are deferred on a pull request the same way,
and the summary names the filter.

Why: speed on pull requests must never read as coverage that did not happen;
`main` runs deferred suites after merge.

Owner: repository CI.

## Native evidence and quarantine

Anchor: `scripts/check-native-inventory`; `scripts/flaky-quarantine`

Whenever native macOS tests run, every declared native test must have passed
and every quarantine entry must resolve against both platform test lists. A
run that did not build the native list only warns on an entry it cannot
resolve; `main` always builds both lists, and a change to quarantine entries
pulls the native suite into its pull request.

Why: a quarantine entry must name a real test, but a pull request that defers
native tests cannot see the native list.

Owner: repository CI.

## Red main is visible

A failed `ci-status` on a `main` push opens, or updates, one red-main issue
naming the failed jobs and the commit range since the last green `main`. The
next green `main` run closes it.

Why: `main` is fixed forward, so its breakage must reach the owner at once.

Owner: repository CI.
