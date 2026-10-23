# Rust port rulebook

Version: **6**. Status: accepted 2026-10-23 with
[ADR 0028](../../adr/0028-rust-port-dependencies.md); version 3 scopes the
judge gates to cut-over, version 4 fixes number, timestamp and differential
rules from wave-1 review. Version 5 settles YAML, retires Python-AST tooling
and records fixture and macOS release checks. Version 6 makes the port
Rust-native (R0): parity only at the external contract, redesign inside it,
and delete the Python/TS owner, its tests and dead code in the cut-over
slice. Applies to every port lane.
The [plan](README.md) owns the boundary, phases and landing method.

## R0. Rust-native port: contract parity, free design, delete as you land

The goal is a smaller, better Rust product, not a transliteration. Each lane
spends its effort on the contract and on deletion, never on reproducing
Python or TypeScript internals.

**The external contract** is what survives outside this repository's source:
- persisted state that existing installs carry (run directories and records,
  Fabric SQLite, telemetry stores, config and managed files, J2 schemas,
  digests and hashed identities);
- the `provenant` CLI and MCP surface as users, other harnesses and installed
  hooks see it (J1 machine output, exit codes, J3 transcripts);
- provider wire forms (argv, environment, protocol frames).
Callers inside this repository (skills, hooks, scripts, docs, tests) are not
the contract: the cut-over slice updates them.

**Inside the contract, redesign.** Choose the Rust data model, module split and
algorithms that fit the job. Do not emulate CPython or V8 behaviour (`repr`,
`str.strip` classes, Unicode digit tables, surrogateescape, float rounding,
argparse wording, dict-order accidents, exception text) unless it is visible
at the contract. Where it is visible, one owner in `provenant-core` or
`provenant-records` implements it, and only for that contract. An internal
behaviour change needs no divergence entry; a contract-visible change does,
and the chair decides it.

**Prune before porting.** A unit whose only callers are tests, retired tools,
compatibility shims, superseded code paths or unused features is deleted, not
ported. Tests that exercise Python seams, monkeypatches or private helpers are
deleted with their subject. A lint, validator or test that a Rust type,
enum, newtype or the compiler makes redundant (shape, field presence, type,
exhaustiveness, null or unknown-variant checks) is deleted, not ported: make
the invalid state unrepresentable instead. Each lane lists its deletions
first.

**Shrink the codebase.** The port should leave much less code than it found.
Every slice PR reports its net line change across Python, TypeScript and
Rust (code and tests). A slice that does not reduce the total explains why
in its PR body.

**Cut over in vertical slices.** A slice is a command, entry point or subsystem
with its callers. One slice PR ports it, switches every in-repo caller to the
Rust path, and deletes the Python/TS source, its tests and helpers that no
longer have callers. Language-neutral judges (J1 golden, J2 schema, J3
transcript, lane conformance) are the proof and survive the deletion.
Differential tests against Python are optional scaffolding for risky logic and
leave with the Python. Inert library ports with no cut-over plan are not
started.

## R1. Version the rules and regenerate their consequences

Every port PR names this rulebook version and its source/oracle revisions.
A recurring review finding becomes a rule amendment with a reason and a
counterexample, then regenerates graph and inventory in the same PR. Bump
`VERSION` in [derive_graph.py](derive_graph.py) with a rulebook version change.
Change the generator rather than editing generated files. The chair assigns
one rulebook/lockfile integration owner. A rule cannot excuse a boundary
change: that still needs an approved divergence and, where required, the user.

## R2. One workspace, one front door, libraries by responsibility

Keep the existing package/binary `provenant` at `tools/front-door/`. Propose
adding a workspace to its existing `Cargo.toml`, with library packages
`provenant-<name>` under `tools/front-door/crates/<name>/`. The binary remains
the only executable front door. Compatibility wrappers exec its subcommands;
they do not retain a second runtime. Keep the current toolchain pin until a
separate change updates it. Share versions/features through workspace
dependencies and commit one `Cargo.lock`.

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
| run | Dispatch, attempts, admission, batch, retry and publication orchestration | core, records, config, supervisor, confinement, providers, quota, routing, telemetry |
| fabric-store | SQLite schema/migrations, messages, claims, sessions and graph persistence | core, records, supervisor's safe lock API |
| fabric-runtime | Identity, run registry/status, execution facade, federation and bounded graph owner | core, records, config, supervisor, run, fabric-store |
| fabric-mcp | MCP registration, transport lifecycle and result rendering | core, records, fabric-runtime |
| workflow | Delivery/review validators, session, tracker and writing tools | core, records, config, confinement |
| cli | Argument parsing and output/exit rendering for all commands | Libraries above as needed; no library imports cli |

The package `provenant` composes `cli` and `fabric-mcp`; it owns install identity
and process-level setup. Keep `help` and root resolution cheap: they must not
start threads, open SQLite or probe providers. Crates are scaffolded under
`tools/front-door/crates/`. Source-group placement in the inventory is
the initial owner; a large module can feed several crates, as `provider_exec`
does. Extract decisions and typed events to break cycles. Do not copy today's
cross-module global assignments into Rust or solve a cycle by importing `run`
into `providers` or `routing`.

Expanding the workspace also expands the install digest's input set. Before
using workspace libraries, amend the builder/checker to cover all workspace
source, manifests, lockfile and build inputs. Keep the embedded managed marker
and source digest contract compatible and test stale-library detection. A
binary stamped from only `src/**` would miss crate changes.

## R3. Typed errors in libraries; compatibility rendering at the edge

Use `thiserror` enums in libraries, including typed refusal, contention,
cancel, malformed input and I/O cases. Return `Result`; no production
`unwrap`, `expect`, `panic!` or silent success on a failed write. Preserve the
underlying cause and path without exposing secret values. Use `anyhow` or an
equivalent context wrapper only in the binary/CLI edge. Stable error variants
drive exact user-facing wording and exit status there; Rust's or a crate's
default `Display` text is not a compatibility contract.

Expected operational failures are data, not panics. In particular, I/O or
ENOSPC during bookkeeping cannot become a provider stop request. Publication
failure remains explicit even if diagnostics also fail. Review the current
`panic = "abort"` release profile before relying on unwinding cleanup: do
not claim `Drop` runs on abort or SIGKILL. Keep publication and recovery
explicit, with conformance for sudden owner death. Any profile change gets
its own review and gates.

## R4. Serde everywhere; J2 owns the contract

Every retained record and config has a typed `Serialize`/`Deserialize` model,
including subprocess protocols, owner records, launch/exit data, receipts,
profile/config overlays and Fabric rows at their wire edge. J2's versioned
JSON Schemas under `schemas/` are the contract. Do not generate a replacement
schema from the new structs and call that parity. Compare old fixtures and
new serialisation against the same J2 schemas and semantic validators.

For persisted records and contract output (R0), match defaults,
unknown-field handling, empty versus absent, nulls, integer ranges, milliseconds versus seconds, timestamp formatting and version refusal
exactly. Do not apply `deny_unknown_fields` or serde defaults indiscriminately.
Use bounded input reads; preserve malformed/oversized-input refusals. Preserve
paths as OS strings until a record's encoding rule applies. Cover Unicode,
non-UTF-8 names, escaped text, negative/fractional numbers and large integers.
Python and JavaScript number behaviour is not automatically reproduced by
`serde_json`.

Numbers are exact. A Python `int` maps to `i64` or `u64`, never `f64`: `300`
must not become `300.0` in output, records or hashed identities. Integers
outside the `i64`/`u64` range are refused with a typed `NumberOutOfRange`
error, never rounded through `f64`; this is a recorded `improvement`
divergence. Do not enable `serde_json`'s `arbitrary_precision`: Cargo unifies
it across the workspace and it breaks untagged, flattened and internally
tagged enums. `provenant-records` owns the checked codec; add a missing piece to
that crate in the slice that needs it. Float-second timestamps convert to datetimes through one
shared helper (owner `provenant-core`) that reproduces CPython's
`datetime.fromtimestamp` rounding (`modf`, then round-half-even to
microseconds); `(s * 1e6).round()` differs by 1 µs on a large share of stamps.

Schema equivalence does not prove canonical bytes. Use explicit encoders for
sorted keys, float spelling, escaping, whitespace and final newlines where
digests or golden output require them. Never serialise a `HashMap` and rely
on its order. S2/S3 opaque staged bytes remain opaque until their content
owner is ported. Rust checks sidecars before publishing and never silently
rewrites them. Match existing atomic/no-replace publication, permissions and
fsync policy through the supervisor's safe OS API.

## R5. Threads at transport edges; supervisor stays sans-IO

There is no async runtime. The MCP server reads stdin on one thread and runs
each tool call on a bounded worker thread, so a cancellation notification can
reach a running call. Facade waits use bounded condition-variable or poll
loops with explicit deadlines and queue bounds. Keep config parsing, route
and quota decisions, reducers and record codecs synchronous. One thread owns
each SQLite connection; never hold a transaction across a wait.
There is no daemon, global background scheduler or second provider owner.

The supervisor is `step(state, event) -> effects`, using explicit time values
and typed handles, with no filesystem, clock, process or async access in
`step`. A small OS loop applies its effects. The only provider-signalling API
accepts a closed `StopReason`; silence and bookkeeping errors have no such
conversion. Register child observation before spawn, preserve WNOWAIT identity
until cleanup and reap last, as the supervisor spec requires. Do not replace
that custody with a detached thread or `Child::try_wait`.

Conserve one owner for process lifecycle through S2–S4. Reuse the accepted S2
implementation and its observer protocol; move observer policy into Rust as
a later provider unit, validated by recorded adapter transcripts. Cancelling
an MCP tool call must not drop custody, release a writer lease early or kill
a sibling lane. A parent asks children to cancel through their markers;
it does not signal them by an inferred PID.

## R6. Direct dependency allowlist

[ADR 0028](../../adr/0028-rust-port-dependencies.md) sets the list. Declare
each crate once in `[workspace.dependencies]` with its pinned version and
features; member crates use `{ workspace = true }`.

| Dependency | Scope |
|---|---|
| `serde` (derive), `serde_json` | Typed records, config and JSON protocols; explicit encoders where bytes matter |
| `thiserror` | Typed library errors |
| `rusqlite` (`bundled`) | Fabric store; preserve transactions, WAL, busy timeout, pragmas and migration order |
| `sha2` | Existing SHA-256 identity and digest contracts |
| `regex` | Pattern rules ported from Python `re`; check each pattern for syntax differences |
| `jiff` | Timestamps, durations and local-time quota schedules |
| `clap` (derive) | Argument parsing; J1 compares help structurally, machine output byte for byte |
| `toml_edit` | Reading and editing Codex `config.toml` without reformatting it |
| `libc` | Supervisor syscall layer only (R7) |
| `tempfile` (dev only) | Test fixtures, never live state |

Written in-house, per the ADR: HTTP through `curl` with headers on stdin; the
MCP stdio JSON-RPC server; error context at the binary edge; stderr
diagnostics; random IDs from `getentropy`. There is no async runtime: use
threads. Provenant's own YAML data moves to JSON through the graph's separate
`records:yaml-data-to-json` migration, including runtime consumers and fixtures.
External YAML uses the fail-closed subset reader in `provenant-records` defined
by ADR 0028 decision 5; no YAML parser crate is added. The reader's CI gate
parses every tracked external file and, while Python exists, compares accepted
values with PyYAML. Exact PyYAML error wording is excluded from parity.
Record refusal of unsupported YAML constructs, ambiguous plain scalars and
duplicate keys as an approved `improvement` divergence: for example,
`enabled: on` must report the construct and line with `quote this`, rather
than silently become a boolean. Reader diagnostic wording differences are a
`library` divergence; retain construct and line information.

Python-AST tooling stays outside the port. No Python parser crate is allowed.
The graph's `retired` disposition records a deletion criterion for every unit;
these units are not dispatched as Rust ports. Delete each tool when its target
glob is empty. `scripts/static-security-check.py` keeps running in Python CI
until no tracked Python sources remain. Its Rust replacement is the R7 unsafe
guard plus a workspace `clippy.toml` `disallowed-methods` entry for
`std::process::Command::new` outside supervisor. The command lint is graph
work, including existing callers and scoped allowances, not a local exemption.
`scripts/lint-timebomb-fixtures.py` retires for Python and JS/TS when no
`tests/*.py` and no `*.test.ts` remain. A separate graph unit retargets its
date policy to Rust `#[test]` sources using the textual engine in
`provenant-cli`; it does not port Python AST analysis.

Any new direct crate, feature expansion or material transitive addition needs
a rule change here with a reason and the affected gates. No per-lane
dependency shopping. When parallel PRs conflict on `Cargo.lock`, take
`main`'s lockfile and run `cargo metadata --format-version 1 >/dev/null` in
`tools/front-door` to add your crate's entries back.

## R7. Unsafe has one address

All first-party `unsafe` lives in `supervisor::sys`, with narrow safe wrappers,
documented invariants and Linux/macOS-specific modules. Other crates forbid
unsafe code. Do not export raw owning descriptors or process handles that let
callers bypass custody or signal a provider. The syscall layer covers any
required descriptor locks and publication primitives as well as processes;
callers elsewhere use its safe API. Audit dependency unsafe separately through
lockfile review; this rule applies to first-party source, not a claim that
dependencies contain none.

The existing front door has unsafe in `src/exec.rs`, `src/resolve.rs` and
`src/lane/`.
The graph's `workspace:front-door-syscalls` unit relocates it without changing
root resolution or exec behaviour. `workspace:front-door-unsafe-guard` moves
the remaining lane syscalls into supervisor and forbids unsafe in the binary.
Reuse is not an exemption from this rule.

Each unsafe block states validity, ownership, lifetime, concurrency and error
conditions. A second reviewer checks syscall effects and failure paths.
Environment mutation, FFI or signal handlers needing unsafe elsewhere require
a design change, not a local exception. Keep secret environment values off
disk and logs; preserve the launch record's name-only redaction.

## R8. Logging is separate from output

Use structured `tracing` events for internal diagnostics, bounded and redacted.
Do not emit them on MCP stdout or mix them with CLI result bytes. Respect the
existing stderr and file behaviour; enabling a new sink/filter is a boundary
change if a caller observes it. Preserve error/status text through the edge
renderer. Record run/task/attempt IDs and actual route provenance, never prompt
or credential contents by default. Log failure cannot decide lifecycle state.

## R9. Tests prove behaviour

Keep pure unit tests in the owning crate, beside the module under `#[cfg(test)]`.
Use explicit clocks/events and injected data values rather than patched
globals, monkeypatches, private function names or private call order. Test
state-machine transitions, invalid transitions, bounds and typed failures.
Exercise unsafe/OS effects through process fixtures and conformance. Use
FIFO/handshake synchronisation rather than timing sleeps as assertions.

J1 golden CLI, J2 schema, J3 MCP transcript and lane conformance suites are
release gates. Preserve them after the old implementation is removed.
S1 parity tests can retire only when their cases have moved to surviving Rust
tests, including stale advisory and root-resolution cases. Existing Python/TS
tests that assert policy remain evidence to translate; internal seam tests
retire only under the named deletion criteria or R0 pruning. Do not count deleted tests as
passing tests. Each judge must pass against old code and reject relevant
mutants such as a changed exit code, missing record field, reordered terminal
publication, weakened tool schema or a silence-triggered kill.

When a differential is used, it and record comparisons are exact, including number spelling;
no epsilon or `f64` equality in test helpers. Each crate runs its oracles
through one helper that takes the interpreter from `PROVENANT_PORT_PYTHON`
(default `python3`, at least 3.12) or `PROVENANT_PORT_NODE` (default `node`,
at least 24). A missing or older interpreter skips with a `skip:` line, but
fails the test when `PROVENANT_PORT_DIFF_REQUIRED=1`, which CI sets. On macOS
`/var` is a symlink to `/private/var`: compare canonicalised temporary paths.
The full Rust test job runs on Linux; a macOS release-build smoke job shares
its path filter. Keep that release build: stripping proc-macro dylibs broke
macOS installation while debug and Linux checks passed (#1339).

Repo-file fixtures use `include_str!(concat!(env!("CARGO_MANIFEST_DIR"),
"/..."))`. Deep relative `include_str!` paths can escape a nested `.worktrees/`
checkout into the primary checkout and pass locally, then fail in CI.
Fractional-second UTC offsets differ across CPython point releases. Use the
core `py` fromisoformat helper, which probes the interpreter, rather than
fixed oracle fixtures.

Locally run only targeted tests, fmt, lint/type checks and the graph fixtures.
Full suites, `scripts/check-harness`, platform conformance and stress run in CI.
No merge without the PR's exact-head required checks. A new head invalidates
the old evidence. Stress supplements deterministic gates; it cannot replace
them. Live provider runs require the user's authority.

Golden `machine` cases are byte-compared after normalisation and pin the
Python and SQLite versions in `tests/golden/manifest.json`; the recorder and
Python CI must use that interpreter. `help` cases compare structurally (exit
code, subcommand and flag set), so the Rust CLI need not reproduce argparse
text. Re-record with `python tests/golden/run.py --record --verb V` when a
change alters CLI output on purpose, and list the change in `divergences.md`.

## R10. How to port one unit

1. Claim one graph unit and a disjoint crate/module write scope. Record rulebook
   version, oracle/judge revisions and pinned stable install identity. Check
   `loader_review`, cyclic interfaces, runtime entry points and external
   prerequisites. A graph wave is not a work claim.
2. Read the Python/TS source and its tests, including neighbours and callers.
   List observable behaviours, errors, effects, config/env inputs and retained
   record fields. Separate policy tests from test seams and approved retirements.
3. Work by slice (R0). List the slice's contract surface and check a
   language-neutral judge covers it; add the missing golden, schema or
   conformance cases first, recorded against the old implementation. The
   judges gate the switch and the deletion in the same PR. Use owners on
   main; if an owner API is missing, add it to the owner crate in the same
   slice when it is small, otherwise stop the slice and report the API. Do
   not add new `pending` copies.
4. Port the behaviour through the agreed crate API. Delete duplicate/private
   machinery when the new owner makes it unnecessary. Automate repetitive
   symbol/record mapping in a deterministic, reviewed script. Keep one small
   change and one logical commit per unit; do not copy code solely to match
   the old module layout.
5. Run targeted crate checks and applicable black-box gates against explicit
   old/new paths and isolated state. In CI, run the full required matrix.
   Differential record and database comparisons include failure and recovery
   cases, not just successful requests.
6. Append each contract-visible mismatch to [divergences.md](divergences.md),
   classified `bug|library|python-bug|improvement`, with evidence and
   disposition. Fix Rust bugs. Internal differences need no entry.
7. A principal reviewer gets the diff, contract and evidence and assumes the
   patch is wrong. Provide the review ladder required by `HARNESS.md`; the
   chair arranges cross-family review. Record actual resolved routes only.
   Recurring findings update this rulebook before the next affected wave.
8. Commit generated changes, push the assigned branch and open the PR against
   main. CI is the gate. Lanes do not merge; the chair merges through the
   repository workflow. The same PR removes the Python/TS source, its tests
   and any fallback once its judges pass and no in-repo caller remains.

## R11. Escalation stops the affected unit

Escalate to the chair for an uncovered boundary, ambiguous dynamic dependency,
missing judge runner at cut-over, survived mutant, source cycle without an
agreed API (add a small owner API in the slice, otherwise stop the slice),
unit exceeding its half-day scope, repeated repair, or overlap with S2.
Escalate immediately for confinement/custody weakening, a healthy process
signalled, lost terminal publication, unverifiable rollback or a live database
schema change. Do not continue promotion while those remain unresolved. Escalation stops
only the affected unit; the lane continues with the rest of its batch.

The user has delegated port decisions to the chairs (2026-10-23): intentional
contract improvements, cut-over and deletion are chair decisions, with a
cross-family council for hard calls. Return to the user only for an
unapproved dependency/ADR amendment or a state migration that cannot be
rolled back. Preparing
inert crates and this proposal does not need that landing answer. Classifying
a difference is not accepting it. No lane may lower a gate, invent approval,
read credentials or modify the stable install to get a green result.
