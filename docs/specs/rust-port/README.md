# Rust rewrite

Status: accepted, 2026-10-23. The user has decided to replace Provenant's
Python and TypeScript implementation with Rust in one pass: redesign each
subsystem, build it in Rust, delete what it replaces, and install once at
the end. There is no transition period with two implementations.
[ADR 0029](../../adr/0029-rust-rewrite-in-one-pass.md) records the decision.
[PORTING.md](PORTING.md) is the rulebook, and [LANE-BRIEF.md](LANE-BRIEF.md)
holds the design and build lane prompts. GitHub owns assignments and
delivery state.

## Phases

1. **Design.** One design doc per area under [design/](design/), written
   from the old source and its real use, reviewed by a cross-family council
   and accepted by the owning chair. The command tree is frozen in
   `design/cli.md` after the area docs propose their subtrees.
2. **Build.** Lanes build each crate against its accepted design and delete
   what it replaces in the same PR. Build follows the crate dependency order
   in PORTING.md R2: core and records, supervisor, config, confinement,
   providers and quota, routing and telemetry, run, then Fabric and cli.
   Workflow builds in parallel once core, records and config land.
3. **Cut-over.** When the binary drives a fake-provider dispatch, a Fabric
   MCP session and every hook end to end from an isolated state root,
   install it on both Macs in one step. The new state root starts fresh
   (PORTING.md R1).

## Areas and owners

| Area | Crates | Design doc | Chair |
|---|---|---|---|
| Execution | run, routing, providers, quota | `design/execution.md` | workshop |
| State and config | core, records, config; managed install and front-door builder | `design/state.md` | workshop |
| Supervisor and confinement | supervisor, confinement; worktrees and Git evidence | `design/supervisor.md` | workshop |
| Telemetry | telemetry | `design/telemetry.md` | workshop |
| Command surface | cli; hooks, harness checks, release checks | `design/cli.md` | workshop |
| Fabric | fabric-store, fabric-runtime, fabric-mcp; Fabric CLI | `design/fabric.md` | laptop |
| Workflow | workflow; delivery, review, writing linters, session, tracker, UI tooling | `design/workflow.md` | laptop |

Each design doc lists the API it needs from other areas. The owner of that
API answers in its own doc. Earlier inert Rust in `tools/front-door/crates/`
and held port PRs are source material: reuse what is good, delete the
emulation.

## The stable install during the rewrite

The swarm runs on the installed Provenant while the rewrite proceeds. On
each Mac the installed `provenant` resolves its root to the primary checkout,
which stays at its current commit until cut-over. Do not pull, check out or
reinstall in the primary checkout, and do not run the installer from a lane.
Lanes branch from `origin/main` in their own worktrees. Main may break the
old Python and TS paths as they are deleted; that does not affect the
installed copy.

## Cut-over and rollback

Before installing, the chairs check that the release binary passes the
end-to-end suite on both Macs and that no Python or Node is needed by the
product. Install on both Macs, then register MCP and hooks through the new
installer. Roll back by reinstalling from the tag `pre-rust-rewrite`, which
marks the last commit before the first deletion. The old state roots were
never written by the new binary, so the old install resumes where it left
off.

## Decided removals

Chair decisions, informed by the deletion council of 2026-10-23 (two
families) and a transcript usage check on the workshop Mac. A design doc may
remove more; it records each removal with its reason.

- **Dead code now:** `skills/deliver/scripts/select_security_evidence.py`,
  `scripts/lib/projection.py` and Rust `cli::projection`, the workflow
  `generate_policy_fixtures.py`, and `tests/test_delivery_validator_structure.py`.
- **Inert Rust emulation:** every `pending` module, `python_unicode` table,
  `core::py` and `core::js` emulation helper, `oracle.rs`/`test_support.rs`
  oracle harness and its generators, routing `casefold.rs` and the
  `delivery_run_shape` copy, and `crates/core/scripts/generate_unicode.py`.
  These exist only to reproduce Python or V8 behaviour, which R0 no longer
  requires. Each crate's first build step deletes its share.
- **Tools:** the `extract` utility with its utility schema and Ajv validation,
  and the direct `cf_dispatch.py`
  entry. Command aliases are dropped in the frozen command tree.
- **Port machinery:** the unit graph and its generator, inventory, ported
  list and divergence log, which tracked a unit-by-unit port.
- **Judges recorded against the old implementation:** `tests/golden/`,
  `tests/mcp_judge/`, `tests/lane_conformance/` and
  `tests/test_record_schemas.py` retire with the subjects they test.
  Rust integration tests in the new binary replace them (PORTING.md R9),
  and lane conformance's custody invariants are rewritten there first. The
  cli build removes their CI steps and the `PROVENANT_PORT_*` oracle
  variables in `.github/workflows/ci.yml` with them.
- **Python and TS CI tooling** (`scripts/static-security-check.py`,
  `scripts/lint-timebomb-fixtures.py`, the layering test) goes when no
  Python or TS remains; R7's guards replace the security check.

Left to the owning design doc, with the evidence so far:
- Session helpers (`context_audit`, `validate_idle_pause`,
  `bootstrap-autopilot`): the council proposed retiring them. Workshop
  transcripts mention them, so the workflow design checks for real use first.
- Delivery telemetry and retrospectives: the workshop telemetry ledger holds
  no delivery or change rows.
