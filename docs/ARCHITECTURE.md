# Provenant architecture

Owner: the chairs. Anchor: `tools/front-door/Cargo.toml` (members),
`tools/front-door/tests/workspace_policy.rs` (`LAYERS`).

## Purpose

Provenant coordinates agent work, from a brief through execution, review and
landing. It provides a command line, reusable skills and a local Fabric for
runs, sessions and evidence. Projects retain their own policy and authority.

## Trust boundaries

Three outcomes are hard boundaries
([ADR 0035](adr/0035-three-hard-boundaries-and-flexible-everything-else.md)):
credentials reach only the lane, provider or output granted them; nothing
moves, rewrites or deletes another lane's refs, commits or uncommitted work;
merge, release and ruleset changes need the owner. Everything else warns and
proceeds, with its source shown. [Global rules](../GLOBAL-AGENTS.md) define
authority and user gates; a message is not a grant
([human inbox](runbooks/human-inbox.md)).

## Ownership

Each concern has one owner; callers supply the policy that varies
([Rust workspace](specs/rust-workspace.md#one-owner-per-mechanism)). This page
names owners and the standard channels between them. Code owns detail; the
layering test owns permitted edges. When a concern below has two owners, that
is a defect: fix it or link the issue that tracks it.

## Domains

| Domain | Owner | Owns | Never |
|---|---|---|---|
| Kernel primitives | `core`, `os`, `sqlite` | ids, roots, time and `Clock`; descriptor-relative fs and atomic publish; bounded exec; host Git binary; admission classes; change signal; SQLite open, format and busy policy | domain policy |
| Records and wire | `records` | the command envelope, exit table, record codecs, canonical digests, the policy table, adapter identities, shared facts (route, handoff, stop, landing, custody) | I/O |
| Configuration | `config` | `config.toml`, knobs, domain registry, port table layering, project binding, repository files, managed install and release | reading another owner's state |
| Custody and confinement | `supervisor`, `confinement` | process custody and leases; the closed Git utility API; linked worktrees; permission plans and sandbox profiles; publication and transport | provider policy |
| Providers | `providers` (adapter facts in `records::adapters`) | adapter launch and decode, accounts, live quota probes, package caches, skill staging, updates | routing decisions |
| Quota and routing | `quota`, `routing` | pure admission, readiness and schedule; catalogue, model policy, pools and route resolution | process or state effects |
| Run | `run` | the attempt: admission ledger, holder, launch hand-off, settlement, delivery, writer, controls, run store | coordination across attempts |
| Builds and caches | `run::build`, `confinement::cache` | the chair's one host-owned warm check, build slots, host caches | lane builds (lanes run fmt only) |
| Reclamation | retention and gc owners in `run`, `confinement` and `src/` (one engine: #3775) | what is reclaimable, budgets, sweeps driven by `tick` | another lane's refs or uncommitted work |
| Fabric | `fabric-store`, `fabric-runtime` | the coordination store, graphs, lane relay, peers over SSH, inbox, notes, event journal | a second run status |
| Workflow and landing | `workflow`, `records::landing`, `src/pr` | risk, review coverage, receipts, session checkpoints; PR, CI evidence and landing | merging without the owner's rules |
| Telemetry | `telemetry` | usage ledger, cost and value | live probes |
| Command surface | `cli` (grammar, render, external contract), `src/` (composition and process edge) | verbs, envelopes, help, hooks, `tick`, `wait` | domain policy (split: #3774) |
| CI and test infrastructure | `select`, `test-support`, `.github/`, `scripts/` | affected-crate selection, fixtures, the `ci-status` gate | production dependencies |
| Apps | `apps/quota-menubar` (`ProvenantKit`) | the menu agent and Console | reading state or credentials directly |
| Doctrine and skills | `GLOBAL-AGENTS.md`, `skills/`, `docs/` | ambient rules, skills, specs, runbooks, ADRs | implemented fields and defaults |

Libraries never depend on the CLI. Pure crates (`quota`, `routing`, `records`)
take snapshots in and return decisions out; `src/` performs the effects.

## Standard channels

- **Command result.** Every `--json` command prints one
  `records::CommandResult` (`schema_version`, `command`, `status`, `data`,
  `error`). `Outcome::exit_code` is the only exit table. Errors carry a stable
  `code` ([ADR 0034](adr/0034-refusals-carry-stable-typed-codes.md)), a cause and a
  repair (required on every error: reliability-refactors slice 4). Human output is evolving; scripts and the apps read `--json`.
- **Diagnostics and warnings.** Bounded stderr, never result bytes; typed
  warnings in the envelope are #3778. Policy
  checks go through the `records::policy` table: boundary rules refuse,
  everything else warns with its rule, source and knob
  ([ADR 0035](adr/0035-three-hard-boundaries-and-flexible-everything-else.md)).
- **Outward contract.** `cli::external_contract` declares commands, lane
  environment, schemas and exit codes with a stability tier
  ([consumer contract](specs/consumer-contract.md)).
- **Records and state.** One state root (`core::roots`). Durable stores are
  private SQLite opened through `sqlite`; files are versioned records written
  by atomic publish; digests come from `records::canonical`. A mutation and its
  event commit together.
- **Events.** The Fabric event journal is the source for waits and
  subscribers (`provenant events --follow --consumer`); the `os::changes`
  generation signal is a cheap hint that something changed, never data.
- **Lane to host.** Lanes open stores read-only and reach the host only
  through their attempt-bound relay. A lane's result is its handoff record.
- **Host to host.** The fixed SSH peer protocol in `fabric-runtime::hosts`;
  one home host per project.
- **Configuration layers.** Lowest to highest: compiled default, detected,
  host, host-project, project, then risk tier, template and launch request.
  Every effective value reports the layer that set it (`config show --full`);
  one layer vocabulary across settings is #3777.
- **Apps.** The Swift apps run the CLI and decode its JSON; they watch the
  change signal to refresh. Wire fixtures are generated from Rust (console
  polish epic).

## Where detail lives

- [Run lifecycle](specs/run.md) and [lane lifecycle](runbooks/lane-lifecycle.md):
  launch, observe, recover and settle work.
- [Supervisor](specs/supervisor.md): writer custody and shared Git metadata;
  [worktrees](worktrees.md) describe their use.
- [Fabric](specs/fabric.md): coordinate work and retain its records.
- [Routing](specs/routing.md): select execution routes from policy and evidence.
- [Rust workspace](specs/rust-workspace.md): module ownership and layering.
- [Runbooks](runbooks/README.md): operator procedures.
