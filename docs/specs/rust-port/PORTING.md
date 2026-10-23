# Rust port rulebook

Version: **1**. Status: proposed. Applies to whole-project port lanes after
the plan is accepted. Existing accepted ADRs prevail until explicitly amended.
The [plan](README.md) owns the boundary, phases and pending landing decision.

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
| config | Config loading/precedence, profiles, installation and managed-file rules | core, records |
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
start Tokio, open SQLite or probe providers. Crate names are proposed targets,
not scaffolding already present. Source-group placement in the inventory is
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

Match defaults, unknown-field handling, empty versus absent, nulls, integer
ranges, milliseconds versus seconds, timestamp formatting and version refusal
exactly. Do not apply `deny_unknown_fields` or serde defaults indiscriminately.
Use bounded input reads; preserve malformed/oversized-input refusals. Preserve
paths as OS strings until a record's encoding rule applies. Cover Unicode,
non-UTF-8 names, escaped text, negative/fractional numbers and large integers.
Python and JavaScript number behaviour is not automatically reproduced by
`serde_json`.

Schema equivalence does not prove canonical bytes. Use explicit encoders for
sorted keys, float spelling, escaping, whitespace and final newlines where
digests or golden output require them. Never serialise a `HashMap` and rely
on its order. S2/S3 opaque staged bytes remain opaque until their content
owner is ported. Rust checks sidecars before publishing and never silently
rewrites them. Match existing atomic/no-replace publication, permissions and
fsync policy through the supervisor's safe OS API.

## R5. Async at network/transport edges; supervisor stays sans-IO

Recommend Tokio for MCP transport, bounded HTTP requests and facade waits,
with explicit cancellation and queue bounds. Keep config parsing, route and
quota decisions, reducers and record codecs synchronous. Keep SQLite on a
bounded blocking owner; do not hold a SQLite transaction across an `await`.
There is no daemon, global background scheduler or second provider owner.

The supervisor is `step(state, event) -> effects`, using explicit time values
and typed handles, with no filesystem, clock, process or async access in
`step`. A small OS loop applies its effects. The only provider-signalling API
accepts a closed `StopReason`; silence and bookkeeping errors have no such
conversion. Register child observation before spawn, preserve WNOWAIT identity
until cleanup and reap last, as the supervisor spec requires. Do not replace
that custody with a detached Tokio task or `Child::try_wait`.

Conserve one owner for process lifecycle through S2–S4. Reuse the accepted S2
implementation and its observer protocol; move observer policy into Rust as
a later provider unit, validated by recorded adapter transcripts. Cancellation
of an MCP future must not drop custody, release a writer lease early or kill
a sibling lane. A parent asks children to cancel through their markers;
it does not signal them by an inferred PID.

## R6. Proposed direct dependency allowlist

**User approval to amend ADR 0027 is required before adopting this list.**
The current front door has no crate dependencies. The proposed supervisor
spec's `libc` allowance is narrower than this whole-project proposal.
This rulebook neither edits the ADR nor treats its own proposal as approval.
Review exact versions, features, licences and transitive lockfile changes
when approval is given; pin them rather than relying on `latest`.

| Dependency | Proposed reason and scope |
|---|---|
| `serde`, `serde_json` | Typed records/config/JSON protocol throughout; explicit encoding where bytes matter |
| `thiserror` | Typed library errors |
| `anyhow` | Binary-edge diagnostic context only |
| `rusqlite` | Fabric's existing SQLite model; preserve transactions, WAL, busy timeout, pragmas and migration ordering |
| `reqwest` | One HTTP client for quota/catalogue/provider probes; explicit timeouts, redirects, proxies and TLS behaviour |
| `rmcp` | Official MCP Rust SDK for stdio transport; J3 decides compatibility, never SDK defaults |
| `tokio` | Async edge runtime only, with the minimum features needed by transport/HTTP |
| `libc` | Supervisor syscall layer only, for process observation, signals, locks and safe publication |
| `tracing`, `tracing-subscriber` | Structured internal diagnostics with controlled sinks and filters |
| `sha2` | Existing SHA-256 identity and record digest contracts; no custom cryptographic implementation |
| `tempfile` (dev only) | Isolated unit/integration fixtures, never a live state directory |

Library capability references: [Serde](https://serde.rs/),
[rusqlite](https://github.com/rusqlite/rusqlite),
[reqwest](https://docs.rs/reqwest/latest/reqwest/) and
[rmcp](https://github.com/modelcontextprotocol/rust-sdk). These support the
selection, not a claim that their defaults match Provenant.

Start with the existing hand-written front-door parser and explicit parsing
for ported commands. `clap` is **not initially allowlisted**: add it only by
rule change if its flag/help/error behaviour can reproduce J1 byte for byte.
YAML/TOML codecs, regex, date/time and UUID helpers are also not implicitly
allowed. The first config/codec lane must propose the minimum maintained
codec set, prove old-input compatibility and amend this rule with approval
before dispatch; do not hand-roll a general YAML parser to avoid review.
Any new direct crate, feature expansion or material transitive addition needs
a rulebook/lockfile review with a reason and affected gates. An addition beyond
the approved ADR envelope returns to the user. No per-lane dependency shopping.

## R7. Unsafe has one address

All first-party `unsafe` lives in `supervisor::sys`, with narrow safe wrappers,
documented invariants and Linux/macOS-specific modules. Other crates forbid
unsafe code. Do not export raw owning descriptors or process handles that let
callers bypass custody or signal a provider. The syscall layer covers any
required descriptor locks and publication primitives as well as processes;
callers elsewhere use its safe API. Audit dependency unsafe separately through
lockfile review; this rule applies to first-party source, not a claim that
dependencies contain none.

The existing front door has unsafe in `src/exec.rs` and `src/resolve.rs`.
The graph's `workspace:front-door-syscalls` unit relocates it without changing
root resolution or exec behaviour; reuse is not an exemption from this rule.

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
retire only under the named deletion criteria. Do not count deleted tests as
passing tests. Each judge must pass against old code and reject relevant
mutants such as a changed exit code, missing record field, reordered terminal
publication, weakened tool schema or a silence-triggered kill.

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
3. Check each behaviour has an independent old-pass/mutant-fail judge. Ask the
   judge owner to fill a gap before coding that behaviour. Refine an oversized
   unit into cohesive half-day units and regenerate graph/dependants first.
4. Port the behaviour through the agreed crate API. Delete duplicate/private
   machinery when the new owner makes it unnecessary. Automate repetitive
   symbol/record mapping in a deterministic, reviewed script. Keep one small
   change and one logical commit per unit; do not copy code solely to match
   the old module layout.
5. Run targeted crate checks and applicable black-box gates against explicit
   old/new paths and isolated state. In CI, run the full required matrix.
   Differential record and database comparisons include failure and recovery
   cases, not just successful requests.
6. Append each mismatch to [divergences.md](divergences.md), classified
   `bug|library|python-bug|improvement`, with evidence and disposition. Fix Rust
   bugs. A library mismatch is still a mismatch; adapt it or escalate. Never
   silently accept a difference because the new behaviour looks cleaner.
7. A principal reviewer gets the diff, contract and evidence and assumes the
   patch is wrong. Provide the review ladder required by `HARNESS.md`; the
   chair arranges cross-family review. Record actual resolved routes only.
   Recurring findings update this rulebook before the next affected wave.
8. Commit generated changes, push the assigned branch and open the PR against
   the chosen integration branch. CI is the gate. Do not merge or cut over
   from a lane without repository workflow authority. Remove fallback/source
   only after the relevant judges pass and surviving caller/test proof.

## R11. Escalation stops the affected unit

Escalate to the chair for an uncovered boundary, ambiguous dynamic dependency,
missing judge runner, survived mutant, source cycle without an agreed API,
unit exceeding its half-day scope, repeated repair, or overlap with S2.
Escalate immediately for confinement/custody weakening, a healthy process
signalled, lost terminal publication, unverifiable rollback or a live database
schema change. Do not continue promotion while those remain unresolved.

Return to the user for an external boundary change, intentional Python-bug
fix/improvement, unapproved dependency/ADR amendment, unresolved landing
choice when it affects publication, state migration or live cut-over. Preparing
inert crates and this proposal does not need that landing answer. Classifying
a difference is not accepting it. No lane may lower a gate, invent approval,
read credentials or modify the stable install to get a green result.
