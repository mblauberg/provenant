# Slice w1-writer-settlement: Writer settlement decides Git-operation state before publication

## Amendments (chair, 23 Nov 2026, against main af8a73e3e9 after v2.8.2)
These supersede any conflicting text below.
- #3840 changed the settlement driver: phases Import, Publish, Record, with the host merge identified by its recorded OID (`publication_tip`). The Git-operation decision runs before `begin_import` and also on resume when the recorded phase is already Publish or Record.
- Owner rule (scope note on #3407, 19 Nov; the issue body's "Under the owner rules" line): a mid-operation state confined to the lane's own Git (private dir or candidate git-common) warns, names the latest commit (symbolic or detached HEAD) and where the in-progress state is retained, and proceeds with the lane's committed tip, leaving that in-progress state intact (never reset or cleaned). Settlement refuses only when it would leave the host checkout mid-operation, and then before import, with checkout bytes unchanged. This replaces the "nothing is published" fixtures and the "must never publish a tip from a stopped mid-operation lane" sentence below.
- Fixtures: mid-cherry-pick (resolved, uncommitted) in candidate git-common -> warning naming the commit, committed tip published, in-progress state still present; mid-rebase in the private dir -> same; host checkout mid-operation -> refused before import, checkout bytes unchanged; clean lane publishes as before.
- Bisect (item 4) is already fixed (`BISECT_START` in both lists); out of scope beyond the shared inventory.


Issues: #3407. Wave 1 of the next batch; integration branch `batch/next`. Expected size about 450 changed lines.

## Outcome

Writer settlement must decide whether a Git operation is in progress before it imports objects or publishes the branch, and must look in the lane's candidate git-common directory as well as the private directory. In `crates/run/src/writer.rs` `terminal()`, move the `check_writer_terminal` decision ahead of the object import and the branch publication; in `crates/confinement/src/worktrees/custody.rs` inspect every directory the lane's Git operations can write (private dir and candidate git-common). Re-trace items 3, 5 and 6 of #3407 (refusal restores the checkout via unpublish; detached private HEAD loses the recovery location; merge restored in two directories with different rules) and fix any that still hold within this scope; report the rest. This is boundary 2: settlement must never leave the host checkout mid-operation or publish a tip from a stopped mid-operation lane.


## Files (write scope)

- `tools/front-door/crates/run/src/writer.rs`
- `tools/front-door/crates/confinement/src/worktrees/custody.rs`
- `tools/front-door/crates/run/src/objects/view.rs`

## Tests

Fixtures: a lane stopped mid-cherry-pick resolved-but-uncommitted in candidate git-common is detected and nothing is published; a lane stopped mid-rebase in the private dir likewise; a clean lane publishes as before.

## Docs

Amend the settlement invariant in `docs/specs/run-settlement.md`: the Git-operation decision precedes publication.

## Release note

Add `docs/release-notes/next.d/next-batch-w1-writer-settlement.md` in the style of the existing fragments (a `## Fixes`, `## Changes` or `## Added` heading, an area subheading, one bullet ending with the issue numbers). Content: Fixed: writer settlement checks for an in-progress Git operation in every lane Git directory before publishing the branch.

## Rules for this lane

- Work only in your worktree. Treat the write scope as advisory; list every out-of-scope edit in the handoff.
- Run `cargo fmt` only. Never run `cargo build`, `check`, `clippy` or `test`: the chair runs one integrated clippy check and CI runs the tests. Write code that you expect to compile, and say in the handoff which parts you could not check.
- No backwards compatibility: no shims, aliases, legacy readers or fallbacks. Move every caller and delete the old shape. Persisted formats change only through a migration step (`records::migrate`), never through a tolerant decoder.
- Warn and proceed except at the three boundaries: credentials never reach a lane, provider or output not granted them; nothing moves, rewrites or deletes another lane's refs, commits or uncommitted work; merge, release and ruleset changes need the owner. Every other limit is an overridable default with its source shown.
- No host- or repository-specific special cases. Fix root causes.
- Tests pin invariants at public boundaries, never prose, counts or snapshots, unless the brief says the value is a persisted contract. Each test cites the spec section it pins with `// Spec: docs/specs/NAME.md#anchor`.
- Names in *italics* below are new: create them. Paths are relative to the repository root unless they start with `crates/` or `src/`, which are under `tools/front-door/`.
- Commit with a conventional message (no issue-closing keywords) and end with the handoff block.
