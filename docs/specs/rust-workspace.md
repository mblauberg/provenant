# Rust workspace rules

Durable engineering rules for the Rust workspace under `tools/front-door/`.
The [area specs](README.md) own each subsystem contract; this file owns what
they share. [ADR 0028](../adr/0028-rust-port-dependencies.md) decision 3 sets
the direct dependency list and [ADR 0029](../adr/0029-rust-rewrite-in-one-pass.md)
records the one-pass rewrite.

## Crate layering

The package and binary `provenant` stay at `tools/front-door/`, with library
packages `provenant-<name>` under `tools/front-door/crates/<name>/`. The
binary is the only executable. Share versions and features through
`[workspace.dependencies]` and commit one `Cargo.lock`. The table lists the allowed maximum for each
crate; a crate's `Cargo.toml` may declare fewer. A change to a crate's
responsibility or edges lands in this table in the same PR.

| Crate | Owns | Allowed internal dependencies |
|---|---|---|
| core | Pure IDs, roots, paths, clocks as values, shared enums | None |
| records | Versioned serde records, canonical byte encoders, digest contracts, closed adapter specs | core |
| os | Syscalls, concrete filesystem handles, bounded utility execution, capture, clocks, Git discovery, homes and scratch | core |
| config | Config loading/precedence, profiles, installation and managed-file rules | core, records, os |
| supervisor | Sans-IO process state machine, provider custody, guardian handoff and process supervision | core, records, os |
| confinement | Plan permissions, quarantine, secret redaction, safe Git/path policy, worktree lifecycle and scratch settlement | core, records, config, os, supervisor |
| providers | Adapter mechanics, account identity, verified lineage/availability, quota wire decoding, argv/environment, output classification and continuation | core, records, config, confinement, quota, os |
| quota | Quota windows, reset schedules, refresh scheduling and admission advice | records, config |
| telemetry | Terminal-attempt usage/cost reduction, price schema and bounded usage log | core, records, os |
| routing | Catalogue, named routes, eligibility and route decisions | core, records, config, quota, providers |
| sqlite | Shared safe SQLite opening, WAL setup, migration execution and budgeted busy retry | core, os |
| run | Dispatch, admission, the run registry and status, attempts, batch, retry and publication orchestration | core, records, config, os, supervisor, confinement, sqlite |
| fabric-store | SQLite schema/migrations, messages, claims, sessions and graph persistence | core, records, os, sqlite |
| fabric-runtime | Agent identity, federation routing of run queries and the execution facade over run | core, records, config, os, fabric-store |
| fabric-mcp | MCP registration, transport lifecycle and result rendering | core, records, fabric-runtime; fabric-store in tests only, to check store error rendering |
| workflow | Delivery/review validators, session, tracker and writing tools | core, records, os |
| cli | Argument parsing and output/exit rendering for all commands | core, records, config, confinement, providers, quota, telemetry, routing, run, fabric-runtime, workflow, os, supervisor |
| provenant | Binary composition, install identity and process-level setup | core, records, config, os, supervisor, confinement, providers, quota, telemetry, run, fabric-store, fabric-runtime, fabric-mcp, workflow, cli |

`records::ADAPTER_SPECS` is the compiled, closed adapter fact table. Config names,
CLI values, executable aliases and shared capability lookups use those rows.
`providers::REGISTRY` binds execution and quota mechanics to the same names;
argv builders and protocol decoding stay in the provider modules. Model effort
controls remain evidence-backed entries in the routing catalogue. Dispatch
probes only adapters named by its route, pool and explicit fallbacks.

The workspace policy test enforces these edges for normal, build and dev
dependencies, including target-specific tables. Dev-dependencies may also name
the crate itself for its `fixture` feature, and fabric-mcp may depend on
fabric-store in tests as listed above. No library may depend on cli; only the
root package `provenant` may import it.

The package `provenant` composes `cli` and `fabric-mcp` and owns install
identity and process-level setup. `help` and root resolution stay cheap: no
threads, no SQLite, no provider probes. Break cycles with typed events and
decisions, never by importing `run` into `providers` or `routing`. The
install digest covers shipped workspace source (each member's `src/`, `assets/`
and `build.rs`), manifests, the lockfile, the toolchain, Cargo configuration,
the release packager (`examples/package-release.rs`) and the bundled assets.
CLI composes run's `AttemptReader` with telemetry's `stats`; run has no
telemetry dependency. Their shared `AttemptFact` lives in records.

## Errors

Libraries return `Result` with `thiserror` enums, including refusal,
contention, cancel, malformed input and I/O cases. No production `unwrap`,
`expect`, `panic!` or silent success on a failed write. Keep the cause and
path without exposing secret values. The binary edge wraps context with
`Box<dyn Error>` and owns user-facing wording and exit codes. Expected
operational failures are data: an I/O error or ENOSPC during bookkeeping can
never become a provider stop request. Publication and recovery never rely
on `Drop` running: SIGKILL and owner death skip it whatever the panic
strategy.

## Records and numbers

Every record, config and protocol message is a typed serde model. Integers
are `i64` or `u64`, never `f64`; an out-of-range integer is a typed error.
Do not enable `serde_json`'s `arbitrary_precision`. Bytes that feed a digest
use an explicit canonical encoder in `provenant-records`, never `HashMap`
order. Read input with a size bound. Keep paths as `OsString` until a record
needs text. Choose `deny_unknown_fields`, defaults and versioning per record
in its design, for the record's own reasons. Writes are atomic and fsync'd
through the OS crate's concrete filesystem API.

## Threads and the supervisor

There is no async runtime and no daemon. The MCP server reads stdin on one
thread and runs each tool call on a bounded worker so cancellation can reach
it. Waits are bounded, with explicit deadlines. One thread owns each SQLite
connection, and no transaction spans a wait.

The supervisor is `step(state, event) -> effects` with explicit time values
and typed handles, and no filesystem, clock or process access inside `step`.
A small OS loop applies effects. The only provider-signalling API takes a
closed `StopReason`; silence and bookkeeping errors cannot produce one.
Register child observation before spawn, keep WNOWAIT identity until cleanup
and reap last. Cancelling an MCP tool call never drops custody, releases a
writer lease early or kills a sibling lane.

## Dependencies

[ADR 0028](../adr/0028-rust-port-dependencies.md) decision 3 sets the
direct dependency list, decision 4 what is written in-house. Declare each
crate once in `[workspace.dependencies]`; members use `{ workspace = true }`.
A new crate, feature expansion or material transitive addition needs a rule
change here with a reason. Additions so far: `rusqlite` also serves run's
registry store, so admission and operation insertion stay atomic in one
database; `toml_edit` enables its `serde` feature (with `serde_spanned` and
`toml_datetime`) so config is a typed model (see Records and numbers). A workspace policy test checks every
member's direct dependencies against this list, with `tempfile` in
dev-dependencies only. When parallel PRs conflict on `Cargo.lock`, take
`main`'s lockfile and run `cargo metadata --format-version 1 >/dev/null` in
`tools/front-door`.

Provenant's own data files are JSON or TOML. External YAML (`SKILL.md` frontmatter, `agents/*.yaml`
sidecars) uses the fail-closed subset reader in `provenant-records` from
ADR 0028 decision 5, tested against every tracked file of those kinds.

## Unsafe

All first-party `unsafe` lives in `os::sys` and supervisor's private
`process_os` custody boundary behind safe wrappers with documented invariants.
Every other crate, the binary included, has `#![forbid(unsafe_code)]`.
A workspace `clippy.toml` entry restricts `std::process::Command::new` to
these boundaries. Raw owning descriptors and process handles are reserved for
supervisor custody. `workspace_policy` restricts consumers of OS `sys::{fs,
child,raw_exec,reaper}`, `handoff_file` and `from_handoff` to supervisor, and
requires explicit imports rather than broad OS or `sys` namespace aliases.
Each unsafe block states validity, ownership, lifetime, concurrency and error
conditions, and a second reviewer checks it. Secret environment values stay off disk and
out of logs.

## Diagnostics

Internal diagnostics go to a small stderr module, bounded and redacted, never
mixed into MCP stdout or command result bytes. Record run, task and attempt
IDs and the resolved route, never prompt or credential contents. A failure to
log cannot change lifecycle state.

## Tests

Unit tests sit beside their module under `#[cfg(test)]`, with explicit clocks,
events and injected data instead of patched globals. Test transitions,
invalid transitions, bounds and typed failures. Integration tests in the
binary drive the real command tree and MCP server against a fake provider
binary and an isolated state root. Process and custody behaviour (no healthy
provider killed, terminal publication never lost, one writer, cancellation
scope) is tested with process fixtures on macOS, using FIFO or handshake
synchronisation instead of sleeps. Utility execution fixtures also run on
Linux. Every test sets `HOME`, the
state root and any harness config path to a temporary directory, so no test
can read or write the old roots, the stable install or real harness config.
Repo-file fixtures use
`include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/..."))`; on macOS compare
canonicalised temporary paths, because `/var` links to `/private/var`.

Locally run `cargo fmt`, `cargo clippy` and the tests of the crates you
touched. CI runs the workspace on Linux and a release build on macOS, plus the harness asset and generic checks. Live provider runs need the user's authority.

## Simple first

Build the simplest design that is correct for the main path and the failures
that matter now: data loss, credential exposure, killing a healthy provider,
writing outside the selected roots, and silent wrong answers. Other edge cases
get a typed refusal or a note in the PR, not a guardrail; we harden later when
real use shows the need.

- Test the main path and those failures. Do not test what a type, the compiler
  or an upstream tool already guarantees, and do not add a test per branch.
- Never mute `ci-status`.
- Reviews block only on real defects in those failure classes or on a broken
  main path. Everything else is a follow-up note or a size cut.
- When a fix or a rule would add more code than the risk justifies, choose the
  smaller option and say so in the PR.
