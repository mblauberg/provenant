# ADR 0041: Tiered CI from green evidence

**Status:** Accepted

## Context

Every pull-request update ran the whole pipeline: seven macOS jobs and ten
Linux jobs, about 17 to 27 minutes of wall time (median near 23) on 17 to 18
Nov 2026. The account runs at most five macOS jobs at once, so with about a
dozen live lanes the macOS queue, not the tests, set the pace: a `main` run
waited 15 minutes before its first job started. Selection lived in a shell
allowlist and in workflow conditionals, and pull requests selected from the
pull request's own merge base, which is not proven green when `main` is red
or still running.

## Decision

Declare suites in `config/ci-suites.toml`: the jobs each suite gates, its
tier, its inert paths and, for main-tier suites, the paths that pull it into a
pull request. One selector reads the manifest and the Git delta and records,
per suite, `run`, `covered` or `deferred` with its evidence and reason; the
`ci-status` gate reads the same record. The [spec](../specs/ci.md) holds the
invariants.

Pull requests start from the newest green `main` ancestor of the tested
commit, carry coverage forward per suite from that commit or from the same
pull request's previous green head, and narrow Linux Rust tests to the
affected crates and their reverse dependencies. Native macOS tests and the
macOS release smoke are main-tier: they run on a pull request only when their
declared paths change, when the `ci:full` label is present, or on a dispatch
with `tier=full`. Every push to `main` runs everything. A failed `main` run
opens or updates one `ci:red-main` issue; the next green run closes it.

## Alternatives and consequences

**GitHub merge queue.** A fast pull-request tier plus a full run per queue
entry would keep `main` green by construction, but it serialises merges
behind a 20-minute full run on five macOS slots, and GitHub offers it only to
organisation-owned repositories; this one is user-owned. ADR 0037's
exact-candidate landing already gates a batched candidate when an operator
wants combination proof. We accept fix-forward on `main`, made fast by the
red-main issue.

**Delta runs on `main`.** Carrying evidence forward on `main` would save
docs-only merge runs, but it would turn every inert-path or narrowing mistake
into permanent blindness. Nextest has no content-addressed result cache; the
cacheable part, dependency builds, is already restored from `main` caches.

**Cache keys per green `main` commit.** Rust caches already save only from
successful `main` jobs and restore the newest one. Keying each commit would
multiply entries in the repository's 10 GB cache and evict warm ones.

**A `labeled` trigger for `ci:full`.** A run started by any other label would
either cancel the live run or post a second `ci-status` on the same head that
could mask it. The label is read on every run instead; `workflow_dispatch`
with `tier=full` runs the full tier at once.

**Workflow conditionals per path.** They scatter policy across YAML that
cannot be tested; the manifest is checked against the workflow by
`scripts/check-ci-suites`.

Deferring native suites means a pull request can merge with a macOS-only
regression that `main` then catches. Every Rust pull request still runs the
macOS Clippy build, and the pull-request paths of each main-tier suite name
its platform crates, so the remaining exposure is behaviour that only native
tests observe in crates outside those paths.
