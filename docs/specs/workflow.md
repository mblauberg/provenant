# Workflow

Owner: `crates/workflow` (coverage, receipts, sessions, chair logs, writing),
`crates/fabric-runtime/src/handoff/session` (session handoff)
Anchor: `crates/workflow/src/coverage.rs`, `crates/workflow/src/store.rs`,
`crates/fabric-runtime/src/handoff/session.rs`

Workflow checks review coverage and keeps receipts, session checkpoints and
chair logs. It grants no merge, install or release authority. Paths are
relative to `tools/front-door/`.

## Findings need one disposition each

Every selected upstream finding needs exactly one disposition and reason; an
identity-only disposition inherits the finding's descriptive fields.
- Why: reviewers reconcile findings by identity, and none may be silently dropped.

## Review coverage

A review counts only for a retained verdict on the exact subject, at least the
highest author tier, from an independent reviewer; authors, decision makers
(the chair included) and repeats never count. `inconclusive` never counts and
never blocks. Cross-family legs count only on observed family evidence, never
attestation; a waived leg needs a blocker with source references. The
`code-review` skill owns lens choice; the checker's counts may sit below it.
- Why: self-review, a stale subject or an attested family proves nothing.
- Owner: `crates/workflow/src/coverage.rs`, `crates/workflow/src/bind.rs`

## Receipts

A receipt records the chair's decision on one exact subject; a changed subject
resets it. The edge takes the repository and base from the forge, never the
request, so a delta review cannot shrink the covered paths. A downgrade needs a
recorded reason and never passes a crucial or terminal path floor. A receipt
whose shape this build cannot read approves nothing. Saves replace the whole
receipt at an expected revision: one winner, never a torn file.
- Why: acceptance must track the exact subject reviewed, and concurrent chairs
  must not lose decisions.
- Owner: `crates/workflow/src/receipt.rs`, `crates/workflow/src/store/`

## Sessions

A checkpoint is keyed exactly by project and host session; a session
registered to another project requires an explicit project selection. Each save
keeps the text it replaces as a bounded, hashed revision. Hooks are advisory:
they emit only their own key's checkpoint, and mark a missing or slow
source unavailable, never empty. Autosave never writes the chair's
`checkpoint.md`.
Hook state carries custody-ledger, lane and worktree rows; it has no checkpoint-derived PR rows or PR scan.
- Why: restoring the wrong or a lost checkpoint misdirects an agent, and absent
  evidence must not read as an empty queue.
- Owner: `crates/workflow/src/session.rs`, `crates/workflow/src/session/state.rs`

### Structured summary revisions and inbox coverage

Summary edits share the checkpoint's lock and revision guard. Inbox
reconciliation compares only the matching host, project and chair; anything
observed elsewhere or missed by an incomplete scan is unknown, not stale.
- Why: partial observation must not represent a live request as resolved.

### Explicit chairs are discoverable

Only an explicit checkpoint save registers a chair; lanes, autosave and reads
never do, and readers report stale discovery instead of repairing it.
- Why: delivery readers need exact chair identities without guessing ownership.

## Chair logs

The same operation ID and payload return the original sequence inside the
replay horizon; a different payload under that ID is refused. Lanes cannot
append, listing creates no state, and cost depends on retained data, not
lifetime history.
- Why: retries must not duplicate updates, and long sessions must stay bounded.

## Session handoff

A handoff sends a generated prompt, never the transcript, to a peer that has
opted in. The lease moves only after the peer accepts, and a repeated operation
never launches again.
- Why: a handoff must not lose work, launch twice or use an unregistered account.
