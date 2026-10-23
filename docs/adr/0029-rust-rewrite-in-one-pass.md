# ADR 0029 - Rewrite in Rust in one pass, with no transition period

**Status:** Accepted 2026-10-23 (user direction in session: "Stop focusing on
building for the transition period ... just do the massive
rewrite/redesign/cleanup in rust properly at once. If you need to hold off on
installs as it goes - that's fine!")

**Supersedes:** [ADR 0028](0028-rust-port-dependencies.md) decisions 1
(continuous landing behind selectors), 5's PyYAML comparison gate and YAML
migration lane, and 7's retention of Python CI tooling during a transition.
ADR 0028's dependency allowlist (decisions 3, 4 and 6) stands. Plan and
rulebook: [Rust rewrite](../specs/rust-port/README.md),
[PORTING.md](../specs/rust-port/PORTING.md).

## Context

The first six rulebook versions ported unit by unit and kept main usable at
every step. That meant parity judges recorded against the old code,
differential oracles, Python-semantics emulation in Rust, selectors and
caller switches. About 73k lines of inert Rust landed while no Python was
deleted. Most of that effort served a transition the user does not need: the
swarm can keep running on the current install while the rewrite proceeds.

## Decision

1. **Redesign each subsystem, then build it.** Nothing Provenant owns is a
   compatibility contract: the command tree, MCP tools, record shapes, config
   formats and SQLite schema are all open to redesign, and unneeded features
   are dropped. Only other tools' interfaces stay fixed: provider CLI wire
   forms, harness hook protocols, `SKILL.md` and MCP.
2. **No transition work.** No selectors, call adapters, differential oracles,
   old-implementation judges or emulation. Each build PR lands Rust and
   deletes the Python/TS it replaces, with its tests. Main may be red for the
   old paths; the Rust workspace stays green.
3. **Install once.** The installed copy on each Mac stays on its current
   commit through the rewrite. One install switches both Macs after an
   end-to-end check. Rollback reinstalls the `pre-rust-rewrite` tag.
4. **Fresh state.** The new binary uses a new state root and never writes the
   old roots, so rollback is safe. A read-only import covers Fabric messages,
   tasks and the telemetry ledger.

## Consequences

- The swarm cannot use rewritten features until cut-over.
- A design mistake surfaces later than with incremental cut-over. Design docs
  get a cross-family council before build to compensate.
- Users of the old command names and output must relearn the new tree; the
  two Macs are the only installs.
