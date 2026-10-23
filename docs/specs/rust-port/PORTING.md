# Rust rewrite rulebook

Version: **7**. Status: accepted 2026-10-23 with
[ADR 0029](../../adr/0029-rust-rewrite-in-one-pass.md), which supersedes the
landing method of [ADR 0028](../../adr/0028-rust-port-dependencies.md) and
keeps its dependency allowlist. Versions 1 to 6 ported unit by unit against
the Python and TypeScript implementation. Version 7 drops that: Provenant is
redesigned and rewritten in Rust in one pass, with no transition period.
Applies to every design and build lane. The [plan](README.md) owns
ownership, phases and cut-over.

## R0. Rewrite, do not port

The goal is a smaller, better product. The Python and TypeScript sources are
source material for what the product does, not a specification of how.

**Nothing we own is a compatibility contract.** The `provenant` command tree,
its flags and output, the Fabric MCP tool surface, file layouts, record
shapes, config formats and the SQLite schema are all open to redesign. Prune
features nobody needs. Each design doc lists what it drops and why.

**What we do not own stays fixed.** These are inputs from other tools:
- provider CLI wire forms (argv, environment, stream and result JSON of the
  Claude, Codex, Cursor and other adapters);
- harness hook protocols (stdin, stdout, exit codes and config locations that
  Claude Code, Codex and the other harnesses define);
- the Agent Skills `SKILL.md` format and its YAML frontmatter;
- MCP itself (JSON-RPC 2.0 over stdio) and Git.
Tests for these use recorded fixtures of the other tool's real behaviour.

**No transition work.** Do not build caller switches, selectors, Python→Rust
or TS→Rust call adapters, differential oracles against the old code, judges
recorded against the old implementation, or `pending` copies. Do not emulate
CPython or V8 behaviour (`repr`, `str.strip` classes, digit tables,
surrogateescape, float rounding, argparse wording).

**Delete as you land.** A build PR lands its Rust owner and deletes the
Python/TS it replaces, with that code's tests and helpers, in the same PR.
A test whose subject is deleted is deleted, not ported. A lint, validator or
test that a Rust type, enum, newtype or the compiler makes redundant is
deleted, not ported. Each PR body reports `Net lines:` across Python, TS and
Rust; the total should fall, and growth needs a reason.

## R1. State starts fresh

The rewritten binary uses a new state root chosen in the records and config
design. It never reads, migrates or deletes the old roots (`~/.agents` state,
`.agent-run/`, telemetry stores, transcripts). Going back is a reinstall of
the old version, so the change is reversible. A one-shot import exists only
where it is trivial and useful: Fabric messages and tasks, and the telemetry
ledger. Import reads the old files and never writes them.

## R2. One workspace, one binary, libraries by responsibility

The package and binary `provenant` stay at `tools/front-door/`, with library
packages `provenant-<name>` under `tools/front-door/crates/<name>/`. The
binary is the only executable. Share versions and features through
`[workspace.dependencies]` and commit one `Cargo.lock`. A design doc may
change a crate's responsibility or edges. The change lands in this table,
which `tests/test_rust_port_layering.py` reads, in the same PR.

| Crate | Owns | Allowed internal dependencies |
|---|---|---|
| core | Pure IDs, roots, paths, clocks as values, shared enums | None |
| records | Versioned serde records, canonical byte encoders, digest contracts | core |
| config | Config loading/precedence, profiles, installation and managed-file rules | core, records, supervisor's safe filesystem API |
| supervisor | Sans-IO process state machine, syscall boundary, capture, custody-safe OS handles | core, records |
| confinement | Plan permissions, quarantine, secret redaction, safe Git/path policy | core, records, config, supervisor's safe OS API |
| providers | Adapter profiles, argv/environment builders, output classification and continuation | core, records, config, confinement |
| quota | Windows, reset schedules, cooldowns and provider quota probes | core, records, config |
| telemetry | Ingest, usage/cost attribution, summaries and reports | core, records, config |
| routing | Catalogue, preferences, live probes and route decisions | core, records, config, quota, providers |
| run | Dispatch, admission, the run registry and status, attempts, batch, retry and publication orchestration | core, records, config, supervisor, confinement, providers, quota, routing, telemetry |
| fabric-store | SQLite schema/migrations, messages, claims, sessions and graph persistence | core, records, supervisor's safe lock API |
| fabric-runtime | Agent identity, federation routing of run queries and the execution facade over run | core, records, config, supervisor, run, fabric-store |
| fabric-mcp | MCP registration, transport lifecycle and result rendering | core, records, fabric-runtime |
| workflow | Delivery/review validators, session, tracker and writing tools | core, records, config, confinement |
| cli | Argument parsing and output/exit rendering for all commands | Libraries above as needed; no library imports cli |

The package `provenant` composes `cli` and `fabric-mcp` and owns install
identity and process-level setup. `help` and root resolution stay cheap: no
threads, no SQLite, no provider probes. Break cycles with typed events and
decisions, never by importing `run` into `providers` or `routing`. The
install digest covers all workspace source, manifests and the lockfile.

## R3. Typed errors in libraries, rendering at the edge

Libraries return `Result` with `thiserror` enums, including refusal,
contention, cancel, malformed input and I/O cases. No production `unwrap`,
`expect`, `panic!` or silent success on a failed write. Keep the cause and
path without exposing secret values. The binary edge wraps context with
`Box<dyn Error>` and owns user-facing wording and exit codes. Expected
operational failures are data: an I/O error or ENOSPC during bookkeeping can
never become a provider stop request. Publication and recovery never rely
on `Drop` running: SIGKILL and owner death skip it whatever the panic
strategy.

## R4. Serde records, exact numbers

Every record, config and protocol message is a typed serde model. Integers
are `i64` or `u64`, never `f64`; an out-of-range integer is a typed error.
Do not enable `serde_json`'s `arbitrary_precision`. Bytes that feed a digest
use an explicit canonical encoder in `provenant-records`, never `HashMap`
order. Read input with a size bound. Keep paths as `OsString` until a record
needs text. Choose `deny_unknown_fields`, defaults and versioning per record
in its design, for the record's own reasons. Writes are atomic and fsync'd
through the supervisor's safe filesystem API.

## R5. Threads at transport edges; supervisor stays sans-IO

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

## R6. Dependencies

[ADR 0028](../../adr/0028-rust-port-dependencies.md) decision 3 sets the
direct dependency list, decision 4 what is written in-house. Declare each
crate once in `[workspace.dependencies]`; members use `{ workspace = true }`.
A new crate, feature expansion or material transitive addition needs a rule
change here with a reason. Changes so far: `rusqlite` also serves run's
registry store, so admission and operation insertion stay atomic in one
database; `toml_edit` enables its `serde` feature (with `serde_spanned` and
`toml_datetime`) so config is a typed model under R4. A workspace policy test checks every
member's direct dependencies against this list, with `tempfile` in
dev-dependencies only. When parallel PRs conflict on `Cargo.lock`, take
`main`'s lockfile and run `cargo metadata --format-version 1 >/dev/null` in
`tools/front-door`.

Provenant's own data files that are YAML today become JSON or TOML in the
design that owns them. External YAML (`SKILL.md` frontmatter, `agents/*.yaml`
sidecars) uses the fail-closed subset reader in `provenant-records` from
ADR 0028 decision 5, tested against every tracked file of those kinds. There
is no PyYAML comparison. Python-AST tooling is deleted with the Python.

## R7. Unsafe has one address

All first-party `unsafe` lives in `supervisor::sys` behind narrow safe
wrappers with documented invariants. Every other crate, the binary included,
has `#![forbid(unsafe_code)]`. A workspace `clippy.toml` `disallowed-methods`
entry bans `std::process::Command::new` outside supervisor. Until a crate is rewritten its root carries a `// Rewrite pending` allow for that lint; the crate's first build PR removes it, and the cli build removes the last one. Do not export
raw owning descriptors or process handles that bypass custody. Each unsafe
block states validity, ownership, lifetime, concurrency and error conditions,
and a second reviewer checks it. Secret environment values stay off disk and
out of logs.

## R8. Diagnostics are separate from output

Internal diagnostics go to a small stderr module, bounded and redacted, never
mixed into MCP stdout or command result bytes. Record run, task and attempt
IDs and the resolved route, never prompt or credential contents. A failure to
log cannot change lifecycle state.

## R9. Tests prove the new design

Unit tests sit beside their module under `#[cfg(test)]`, with explicit clocks,
events and injected data instead of patched globals. Test transitions,
invalid transitions, bounds and typed failures. Integration tests in the
binary drive the real command tree and MCP server against a fake provider
binary and an isolated state root. Process and custody behaviour (no healthy
provider killed, terminal publication never lost, one writer, cancellation
scope) is tested with process fixtures on Linux and macOS, using FIFO or
handshake synchronisation instead of sleeps. Every test sets `HOME`, the
state root and any harness config path to a temporary directory, so no test
can read or write the old roots, the stable install or real harness config.
Repo-file fixtures use
`include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/..."))`; on macOS compare
canonicalised temporary paths, because `/var` links to `/private/var`.

Locally run `cargo fmt`, `cargo clippy` and the tests of the crates you
touched. CI runs the workspace on Linux and a release build on macOS, plus
whatever Python and TS tests still have a subject. Main may be red for the
old Python and TS paths during the rewrite; the Rust workspace must stay
green. Live provider runs need the user's authority.

## R10. How a lane works

1. **Design** (per subsystem, before code). Write
   `docs/specs/rust-port/design/<area>.md` from the old source and its real
   use: what the subsystem is for, the features kept and dropped, the command
   and MCP surface, the data model and on-disk layout, the crate API other
   areas call, the external contracts it touches and their fixtures, and the
   test plan. A cross-family council reviews it, and the owning chair accepts
   it.
2. **Build** against the accepted design. Land the Rust owner, delete what
   it replaces with those tests and helpers, and update in-repo callers that
   still exist (skills, hooks, docs). Callers whose own owner is not yet
   rewritten wait for that owner; break their tests by deleting them, not by
   adding adapters.
3. **Review.** Before the PR, a native subagent reviews the diff assuming it
   is wrong. The chair arranges cross-family review, then merges on green CI.
   Lanes open PRs and do not merge.

## R11. Authority and escalation

The user has delegated rewrite decisions to the chairs (2026-10-23): design,
feature pruning and deletion are chair decisions, with a cross-family council
for hard calls. Return to the user only for a dependency or ADR amendment
outside R6, or anything that would touch the old state roots, credentials or
the stable install. Each Mac's installed root is its primary checkout, and
harness skills, workflows, hooks and MCP entries link into it: never pull,
check out or reinstall there until cut-over. All work happens in worktrees
from `origin/main`. Escalate to the chair at once for weakened confinement
or custody, a healthy process signalled, or lost terminal publication. No
lane may lower a gate, invent approval, read credentials or modify the stable
install to get a green result.
