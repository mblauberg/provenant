# Workflow

Owner: `crates/workflow` (coverage, receipts, sessions, chair logs, writing),
`crates/fabric-runtime/src/handoff/session` (session handoff)
Anchor: `crates/workflow/src/coverage.rs`, `crates/workflow/src/store.rs`,
`crates/fabric-runtime/src/handoff/session.rs`

Workflow checks review coverage and keeps receipts, session checkpoints and
chair logs. It grants no merge, install or release authority. Paths are
relative to `tools/front-door/`.

## One accountable chair

Coordination has one accountable chair. Lanes, native sub-agents and nested
chairs narrow authority from it and never become a second owner of the same
work. The [orchestrate skill](../../skills/orchestrate/SKILL.md#one-accountable-chair)
owns the rule; this spec records only the chair identity that receipts,
checkpoints and delivery readers depend on.
- Why: acceptance, merge and release authority must name one owner, or
  concurrent agents can each believe they may land the same work.
- Owner: `skills/orchestrate/SKILL.md`

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
recorded reason and never passes a `crucial` or `terminal` path floor. A receipt
whose shape this build cannot read approves nothing. Saves replace the whole
receipt at an expected revision: one winner, never a torn file.
- Why: acceptance must track the exact subject reviewed, and concurrent chairs
  must not lose decisions.
- Owner: `crates/workflow/src/receipt.rs`, `crates/workflow/src/store/`

## Sessions

A checkpoint is keyed exactly by project and host session; a session
registered to another project requires an explicit project selection, and a
save without one stays in the working directory's checkout. From inside a
checkout, an explicit `--project PATH` naming another project warns and saves
under the selected project, naming both the working directory and the selected
path in the JSON warnings and human output. Each save keeps the text it
replaces as a bounded, hashed revision. Hooks are advisory:
they emit only their own key's checkpoint, and mark a missing or slow
source unavailable, never empty. Autosave never writes the chair's
`checkpoint.md`. Session-start inventory reads are bounded and paged: each child has one deadline
and one maximum page. Custody and lane inventories flush a page header before
presence observations and flush each inspected row with its cursor. A deadline
preserves complete rows and offers a continuation from the last inspected row,
including when that row was filtered out; an interrupted first row resumes the
page from its start. Custody continuation preserves the selected chair session
on every page. Lane pages use ordered index scans and defer the host census
until a non-terminal row needs it. A source that cannot produce its header is
reported unavailable, never empty, and the hook still returns inside its budget.
Hook state carries custody-ledger, lane and worktree rows. Inside a lane,
hook context is one line directing progress to the lane handoff; for a native
subagent marked by a non-empty `agent_id` or agent transcript path it is empty.
In both, chair checkpoint text, standing grants and save commands are withheld;
outside them it carries the chair checkpoint guidance. `agent_type` alone does
not mark a subagent because top-level `--agent` sessions also provide it.
When the digest is shown, the autosave's duplicate custody section is withheld
so the custody list is printed once. If output must be bounded, derived sections
shrink first: the chair's `## Intent` (up to 3 KiB) and `## Now` (up to 1 KiB)
stay intact while the derived digest and then the checkpoint excerpt give way.
A fresh checkpoint with nothing actionable drops the "no save needed by
freshness policy" boilerplate.
- Why: restoring the wrong or a lost checkpoint misdirects an agent, absent
  evidence must not read as an empty queue, and repeated custody or cut Intent/Now
  wastes the bounded hook output.
- Owner: `crates/workflow/src/session.rs`, `crates/workflow/src/session/state.rs`

### Bounded restore after compaction

The [session skill](../../skills/session/SKILL.md) restores after compaction in
three bounded steps: read the hook context, run one cursor-bounded state read
(`provenant status --session-stream --cursor <cursor>`), then act against the
owners. It reads state output, never its own driver or tool source, and does not
re-derive a wait by polling; `orchestrate` owns the wait rule.
- Why: open-ended re-orientation after compaction spends turns re-reading source
  instead of state, and a wait already reports its own event.
- Owner: `skills/session/SKILL.md`

### Structured summary revisions and inbox coverage

Summary edits share the checkpoint's lock and revision guard. Inbox
reconciliation compares only the matching host, project and chair; anything
observed elsewhere or missed by an incomplete scan is unknown, not stale.
- Why: partial observation must not represent a live request as resolved.

A console catalogue row derives its display line from the current checkpoint
without a model call: a structured `now` wins, then the first meaningful line of
`## Intent`, `## Goal`, `## Done`, then the H1 title; a deeper subheading stays in
its parent section. The line is published as `summary_line` (string or null) and
its source as `summary_source` (`structured`, `intent`, `goal`, `done`, `title`,
`unavailable`). The line is bounded. The full structured summary stays at
`record.summary`, so the fields a consumer decodes change only by these additive
names, with no data schema version bump (see [consumer contract](consumer-contract.md)).
- Why: chairs save Markdown checkpoints, not structured summaries, so the
  catalogue must remain useful without inventing content or spending a model call.
- Owner: `crates/workflow/src/session/summary.rs`, `crates/cli/src/console.rs`

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
