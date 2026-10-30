# ADR 0036: One owner per mechanism

**Status:** Accepted

Changed: 2026-10-30, #2271.

## Context

Backoff schedules, transient-failure rules, time sampling, duration suffixes and
Git path discovery had each been written at several call sites. The copies
drifted: handoff, landing, source inventory and root selection trusted inherited
Git directory redirects, and the chair, handoff, guardian, quota and settlement
code each read the system clock directly. A fix to one copy left the others
wrong.

## Decision

Each mechanism has one implementation, and other code calls it rather than
copying it.

| Mechanism | Owner | Callers supply |
|---|---|---|
| Backoff | `provenant_core::retry::Backoff`, a capped exponential delay with optional caller-supplied jitter | the first delay, factor, cap and attempt number |
| Transient failure | `provenant_core::retry::is_transient`, over a typed `RetryCause` | the typed cause; deadlines and replay safety stay with the caller |
| Wall and monotonic time | `provenant_core::Clock`, produced in production by `provenant_os::clock::SystemClock` | a clock; callers never read system time |
| Expiry | `WallTime`, read from any RFC 3339 spelling through `provenant_core::rfc3339` | the instant and what happens after it |
| Duration suffixes | `providers::duration::suffix_seconds` | positive and range checks |
| Git path discovery | `provenant_os::git`: `repository_path`, `repository_marker` and executable selection | the field to discover and what to do with it |

Owners hold mechanism only. Domains choose their own parameters, budgets and
retry targets. The injected clock is the clock row: it is not a separate
decision. `clippy.toml` enforces it by disallowing `SystemTime::now`,
`jiff::Timestamp::now` and `jiff::Zoned::now` outside the production clock.

## Consequences

A fix to a schedule, a classification, a clock or a Git marker lands once. A
second copy is a review refusal; a new mechanism needs its owner first. Tests
exercise each owner, and callers test only their own mapping. Descriptor-bound
Git walks in `crates/confinement/src/repo.rs` read the gitfile themselves,
because identity checks must cover that read. The [test isolation
rules](../specs/rust-workspace.md#test-isolation) cover clocks in tests, and
[ADR 0035](0035-four-hard-boundaries-and-flexible-everything-else.md) keeps the
boundaries that these owners serve.
