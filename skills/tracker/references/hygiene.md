# Hygiene sweep

Run this checklist when asked to sweep, and as part of a steward pass's
`run_sweep` event.

- **Stale.** No activity and no owner response past the repository's own
  staleness window (declare it if the repo hasn't). Comment naming the last
  known evidence and ask once before closing; close as not-planned if there is
  still no response after that.
- **Orphaned.** A child issue whose parent epic is closed or deleted, or a
  sub-issue link that no longer resolves. Re-parent under the right epic or
  make it standalone; never leave a dangling reference.
- **Mis-closed.** Closed without merge evidence, without a stated terminal
  reason, or closed by an unrelated event. Reopen with a comment stating why,
  or add the missing evidence and reason if the closure was actually correct.
- **Superseded.** A newer issue or decision has replaced this one's approach.
  Close as duplicate-of or superseded-by, linking the replacement; do not
  leave both open competing for the same work.

## Closing

Close with one of:

- **Completed**, with merge evidence (PR link, merged commit) — not "planned"
  with a successor mapped, which is still open work under another number.
- **Not planned**, with an evidence-based reason and, if relevant, the
  condition that would reopen it.
- **Duplicate-of**, linking the canonical issue.

An epic closes on its own `Done when` being met, never on its child count
reaching zero open — a completed child count can still leave the stated
outcome unmet, and a `Done when` can be met before every discretionary child
lands. Quiet (no recent comments) is not evidence of done; check the actual
state before closing or before leaving an issue open on the strength of
silence.

## Re-verification

Re-check against the current head, not the issue's own history, when: a
referenced PR merged since the last comment, a dependency issue closed, or
the sweep is running for the first time in a while. An issue's own comment
thread can be stale even when the issue itself looks active.
