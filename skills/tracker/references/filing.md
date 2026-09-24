# Filing: duplicates and placement

## Evidence-based duplicate checks

Before filing, run at least two searches against the tracker (for example
`gh issue list --search "<keywords>"` and a search on a symbol, error string or
file path the new issue touches). Record, on the issue or in a triage comment:

- the queries run;
- the closest candidates found, with why each is or isn't the same issue.

Never assert "no duplicate" without the searches to back it; an agent's
unrecorded belief is not evidence for the next reader. Three outcomes follow
the search:

- **Duplicate.** Close the new report as duplicate-of the canonical issue and
  link it; add any new detail as a comment on the canonical issue instead.
- **Reuse or widen.** An existing issue covers most of the ask. Widen its scope
  in place (update the body, note what changed and why) rather than filing a
  near-duplicate.
- **New.** No real match. File it, placed per below.

## Placement

An epic is an outcome with a stated `Done when`, not a folder for related
work. Nest a new issue under an epic only when the issue is genuinely part of
that outcome — never because a parent happens to exist nearby. Parents are
optional:

- A bug stands alone unless it is explicitly a slice of an epic's `Done when`.
- One-off tidying, chores and small fixes stand alone by default.
- A new epic itself has no parent unless it is a slice of a larger declared
  outcome.

Record the placement decision on the issue (why this epic, or why standalone)
so a later reader does not have to re-derive it. Sub-issues are the only
parent relation the tracker recognises; use a native sub-issue link, never a
checklist mirror of children in the parent body.

## Body quality

Write so a newcomer can act without asking a question:

- Cite code as `path#Symbol`; never a line number, which drifts.
- State the observed behaviour or wanted outcome, and for a bug, how to
  reproduce it.
- For an epic, state its `Done when` explicitly; a percentage of closed
  children is not a completion signal (see
  [hygiene.md](hygiene.md)).
