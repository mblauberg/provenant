# Assumptions and owner-away steering

Applies to autopilot missions and to any long-running chair whose owner steps
away. Decisions keep flowing; the owner confirms them later in one pass.

## Assumption ledger

One untracked, run-local Markdown file per chair session or mission, for
example `.agent-run/sessions/<session-id>/ASSUMPTIONS.md`. Project instructions
may name the path. One row per decision or assumption the owner has not yet
confirmed:

```markdown
- [ ] A12 2026-09-24 Use option B for the export format. Basis: council (claude/opus, codex/gpt-6-sol, agy/gemini-3.8-flash). Reversal: one migration. Refs: #412
- [x] A11 2026-09-23 Keep weekly digests. Confirmed by owner 2026-09-24.
- [x] A9 2026-09-22 Drop the legacy import. Overturned by owner 2026-09-24: keep it; follow-up #418.
```

- `- [ ]` is open, `- [x]` is settled. A settled row records how and when:
  confirmed, or overturned with its follow-up.
- Each row carries an id, a date, the decision in one sentence, its basis
  (owner direction, council members' routes, or evidence link), the reversal
  cost and references.
- The ledger never blocks closure. An issue closes on what its pull request
  delivered; an open assumption stays in the ledger, not in the issue.
- Loss is acceptable. A decision that must survive graduates to its durable
  owner (ADR, spec, issue) when it is confirmed.
- Write the row when the decision is made, before the work that depends on it.

When the owner asks "anything to confirm?", list the open rows oldest first,
one line each, with the default already taken. Mark each answer as it arrives.

## Owner-away steering

- Act on the newest owner direction; it supersedes older ones. Keep the
  directions in force linked from the state file.
- Stay inside the granted envelope. Refill from the frontier; do not widen
  scope to keep lanes busy.
- Make in-authority decisions rather than waiting. Significant or contested
  ones go to a decision council (`orchestrate`). Either way, write a ledger
  row.
- A `HARNESS.md` user gate stops only its own item. Park that item with its
  question and continue the rest.
- When the owner returns, give a short digest: what landed, what is in flight,
  the count of open assumptions, and the decisions only the owner can make.
