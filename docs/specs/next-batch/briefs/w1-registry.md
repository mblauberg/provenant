# Slice w1-registry: Command registry replaces the dispatch match

Issues: #3774. Wave 1 of the next batch; integration branch `batch/next`. Expected size about 800 changed lines.

## Outcome

Replace the dispatch match in `src/lib.rs run()` with a registry: one module per command domain under `src/commands/` (ci, cloud, console, fabric, hooks, install, notes, tracker, pr, worktree, config, status, run, quota, accounts, and so on, following the grammar's top-level commands), each exporting a `DOMAIN` with its `Verb`s. Move each match arm's body into its domain module unchanged. `run()` parses, looks the verb up by path, calls it and emits exactly as today. Derive `provenant_cli::usage_verbs()` and the `external_contract` command list from `DOMAINS` (the grammar stays in `crates/cli/src/grammar.rs`; the registry is keyed by the parsed command path).
Add to `tests/workspace_policy.rs`: every grammar leaf maps to exactly one `Verb` and back; a production-file line-ceiling ratchet (record each current production `.rs` file over 2,000 lines with its present count as its ceiling in a table in the test; a new file may not exceed 2,000; growth past a ceiling fails with a message naming the file, its ceiling and how to raise it in the same PR).

## Interface to honour

These types exist on the integration branch (or, for wave 1 w1-interfaces, are yours to create exactly). Use them as written; do not add parallel types.

```rust
// src/commands/mod.rs  (owner: the binary; one module per domain under src/commands/)
pub struct Domain { pub name: &'static str, pub verbs: &'static [Verb] }
pub struct Verb {
    pub path: &'static [&'static str],      // ["worktree", "locate"]; the persisted usage verb is path.join(" ")
    pub stability: Stability,               // external_contract reads this
    pub run: fn(&Invocation) -> Result<Outcome, CommandError>,
}
pub struct Invocation<'a> { pub command: &'a cli::Command, pub globals: &'a cli::Globals }
pub static DOMAINS: &[&Domain] = &[&ci::DOMAIN, &cloud::DOMAIN, /* one per domain */];
// run() parses, finds the Verb by path, installs the output context, calls it, emits.
// Grammar stays in crates/cli/src/grammar.rs for this batch.
```

## Files (write scope)

- `tools/front-door/src/lib.rs`
- `tools/front-door/src/commands/`
- `tools/front-door/crates/cli/src/lib.rs`
- `tools/front-door/crates/cli/src/external_contract.rs`
- `tools/front-door/src/usage.rs`
- `tools/front-door/tests/workspace_policy.rs`
- `tools/front-door/clippy.toml`

## Tests

Before moving anything, snapshot today's `usage_verbs()` output as a characterisation test (the strings are persisted usage verbs, so the set must not change). Then the mapping test and the ratchet. Do not add per-command behaviour tests: the existing integration tests cover the moved bodies.

## Docs

Add *One owner per command domain* to `docs/specs/rust-workspace.md` (invariant: each command path has one owning domain module and one registry entry; Owner: `src/commands/mod.rs`).

## Release note

Add `docs/release-notes/next.d/next-batch-w1-registry.md` in the style of the existing fragments (a `## Fixes`, `## Changes` or `## Added` heading, an area subheading, one bullet ending with the issue numbers). Content: Internal: commands dispatch through a per-domain registry.

## Rules for this lane

- Work only in your worktree. Treat the write scope as advisory; list every out-of-scope edit in the handoff.
- Run `cargo fmt` only. Never run `cargo build`, `check`, `clippy` or `test`: the chair runs one integrated clippy check and CI runs the tests. Write code that you expect to compile, and say in the handoff which parts you could not check.
- No backwards compatibility: no shims, aliases, legacy readers or fallbacks. Move every caller and delete the old shape. Persisted formats change only through a migration step (`records::migrate`), never through a tolerant decoder.
- Warn and proceed except at the three boundaries: credentials never reach a lane, provider or output not granted them; nothing moves, rewrites or deletes another lane's refs, commits or uncommitted work; merge, release and ruleset changes need the owner. Every other limit is an overridable default with its source shown.
- No host- or repository-specific special cases. Fix root causes.
- Tests pin invariants at public boundaries, never prose, counts or snapshots, unless the brief says the value is a persisted contract. Each test cites the spec section it pins with `// Spec: docs/specs/NAME.md#anchor`.
- Names in *italics* below are new: create them. Paths are relative to the repository root unless they start with `crates/` or `src/`, which are under `tools/front-door/`.
- Commit with a conventional message (no issue-closing keywords) and end with the handoff block.
