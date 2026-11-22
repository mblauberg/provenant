# Slice w1-names: One close-match ranker and name refusal

Issues: #3808. Wave 1 of the next batch; integration branch `batch/next`. Expected size about 500 changed lines. Starts after w1-interfaces have integrated.

## Outcome

Adopt `core::names` everywhere a close match is computed today and delete the copies: `crates/fabric-runtime/src/graph/template.rs` `edit_distance` and its ranking, `crates/config/src/knobs.rs` `distance`, the ranker in `crates/routing/src/catalog.rs`, and the call sites in `crates/cli/src/lib.rs` and `crates/cli/src/route.rs` (`edit_distance(..) <= 2`). Each owner implements `Catalogue` for its names (models, adapters, templates, knobs) and refuses with `NameRefusal` mapped into its existing typed code, adding `candidates` to the error data. Fix the known gaps on the way: `run --model <bare unknown>` and `route resolve <unknown adapter>/<model>` list candidates. Keep the redaction guards `bare_model_detail` and `clap_error` intact. Make `--name` with capitals fold to lower case and warn *NAME_CASE_FOLDED* instead of refusing, using `core::names::valid_name`; delete `graph_name_fault` and `Target::parse::check_name` in favour of it.

## Interface to honour

These types exist on the integration branch (or, for wave 1 w1-interfaces, are yours to create exactly). Use them as written; do not add parallel types.

```rust
// crates/core/src/names.rs  (owner: core; no dependency on records)
/// Ranked close matches: case-insensitive; exact-after-folding first, then
/// prefix, then substring, then edit distance <= max(1, len/3); at most 5.
pub fn close_matches<'a>(query: &str, universe: impl IntoIterator<Item = &'a str>) -> Vec<&'a str>;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct NameKind(pub &'static str); // "model", "adapter", "account", "graph", "command", ...

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Fold { Exact, CaseFolded, Alias { alias: String } }

pub struct Resolved<T> { pub item: T, pub canonical: String, pub fold: Fold }

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NameRefusal { pub kind: NameKind, pub query: String, pub candidates: Vec<String> }
impl std::fmt::Display for NameRefusal {} // "unknown model `x`; close matches: a, b"

pub trait Catalogue {
    type Item;
    const KIND: NameKind;
    fn names(&self) -> Vec<String>;
    fn lookup(&self, canonical: &str) -> Option<Self::Item>;
    fn aliases(&self) -> Vec<(String, String)> { Vec::new() } // (alias, canonical)
}
/// Exact, then case-folded, then alias; else a refusal carrying close matches.
/// A fold other than Exact is the caller's cue to emit a warning.
pub fn resolve<C: Catalogue>(catalogue: &C, query: &str) -> Result<Resolved<C::Item>, NameRefusal>;
```

## Files (write scope)

- `tools/front-door/crates/core/src/lib.rs`
- `tools/front-door/crates/fabric-runtime/src/graph/template.rs`
- `tools/front-door/crates/config/src/knobs.rs`
- `tools/front-door/crates/routing/src/catalog.rs`
- `tools/front-door/crates/cli/src/lib.rs`
- `tools/front-door/crates/cli/src/route.rs`
- `tools/front-door/crates/cli/src/route/`
- `tools/front-door/crates/cli/src/run/provider/preparation.rs`

## Tests

Per catalogue: a one-edit typo of each name ranks it first; an unknown bare model lists close matches; a capitalised `--name` proceeds with the warning. Assert codes, candidate membership and order, never message prose.

## Docs

Add *Every name refusal lists close matches* to `docs/specs/cli.md`.

## Release note

Add `docs/release-notes/next.d/next-batch-w1-names.md` in the style of the existing fragments (a `## Fixes`, `## Changes` or `## Added` heading, an area subheading, one bullet ending with the issue numbers). Content: Changed: unknown model, adapter, template and knob names list close matches; names with capitals are lower-cased with a warning.

## Rules for this lane

- Work only in your worktree. Treat the write scope as advisory; list every out-of-scope edit in the handoff.
- Run `cargo fmt` only. Never run `cargo build`, `check`, `clippy` or `test`: the chair runs one integrated clippy check and CI runs the tests. Write code that you expect to compile, and say in the handoff which parts you could not check.
- No backwards compatibility: no shims, aliases, legacy readers or fallbacks. Move every caller and delete the old shape. Persisted formats change only through a migration step (`records::migrate`), never through a tolerant decoder.
- Warn and proceed except at the three boundaries: credentials never reach a lane, provider or output not granted them; nothing moves, rewrites or deletes another lane's refs, commits or uncommitted work; merge, release and ruleset changes need the owner. Every other limit is an overridable default with its source shown.
- No host- or repository-specific special cases. Fix root causes.
- Tests pin invariants at public boundaries, never prose, counts or snapshots, unless the brief says the value is a persisted contract. Each test cites the spec section it pins with `// Spec: docs/specs/NAME.md#anchor`.
- Names in *italics* below are new: create them. Paths are relative to the repository root unless they start with `crates/` or `src/`, which are under `tools/front-door/`.
- Commit with a conventional message (no issue-closing keywords) and end with the handoff block.
