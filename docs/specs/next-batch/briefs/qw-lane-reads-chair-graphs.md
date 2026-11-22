# Quick win qw-lane-reads-chair-graphs: A lane may read any graph its own chair session launched

Issues: #3192. Integration branch `batch/quick-wins`; about 350 changed lines.

## Outcome

The lane read path allows status, output and trace for any graph whose chair session equals the lane's own. Writes (steer, stop, retry) stay limited to its own graph. If the graph record has no chair session field, add one, set at submission.

## Acceptance

The lane read of a sibling graph from the same chair session succeeds; a graph from another session stays `not_found`.

## Files (write scope)

- `tools/front-door/crates/cli/src/fabric/graph_relay.rs`
- `tools/front-door/crates/fabric-runtime/src/coordination/facade.rs`
- `tools/front-door/src/fabric/lane_read_tests.rs`

## Tests

Flip the two denial tests in `src/fabric/lane_read_tests.rs` to the new rule and add the cross-session denial.

## Release note

Add `docs/release-notes/next.d/3192-lane-reads-chair-graphs.md` in the style of the existing fragments (heading, area subheading, one bullet ending with #3192).

## Rules for this lane

- Work only in your worktree. Treat the write scope as advisory; list every out-of-scope edit in the handoff.
- Run `cargo fmt` only. Never run `cargo build`, `check`, `clippy` or `test`: the chair runs one integrated clippy check and CI runs the tests. Write code that you expect to compile, and say in the handoff which parts you could not check.
- No backwards compatibility: no shims, aliases, legacy readers or fallbacks. Move every caller and delete the old shape. Persisted formats change only through a migration step (`records::migrate`), never through a tolerant decoder.
- Warn and proceed except at the three boundaries: credentials never reach a lane, provider or output not granted them; nothing moves, rewrites or deletes another lane's refs, commits or uncommitted work; merge, release and ruleset changes need the owner. Every other limit is an overridable default with its source shown.
- No host- or repository-specific special cases. Fix root causes.
- Tests pin invariants at public boundaries, never prose, counts or snapshots, unless the brief says the value is a persisted contract. Each test cites the spec section it pins with `// Spec: docs/specs/NAME.md#anchor`.
- Names in *italics* below are new: create them. Paths are relative to the repository root unless they start with `crates/` or `src/`, which are under `tools/front-door/`.
- Commit with a conventional message (no issue-closing keywords) and end with the handoff block.
