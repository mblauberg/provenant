---
name: tracker
description: "Use for GitHub issue and project-board stewardship on any repo: duplicate checks before filing, epic placement, board/Horizon upkeep, PR issue-linking, closing stale issues, or steward-mode queue triage. Not for doing the work; use implement or orchestrate."
---

# Tracker

A generic issue-and-board steward. Read the repository's `Repository process`
declaration first (`MAINTAINING.md` or equivalent): a project with its own
tracker commands and runbook — for example sealle-app's `pnpm issue` commands
and `docs/runbooks/tracker.md` — owns the mechanics; this skill supplies the
judgement calls those commands don't make (duplicate calls, placement, closing
reasons) and falls back to raw `gh` only when no project command exists.

## Quickstart

1. **Search before filing.** Run at least two searches (title keywords, then a
   symbol or error string) and record what you ran and found in the issue body
   or triage comment, even when the result is "no duplicate found". A hit that
   only partially overlaps is a reuse-or-widen candidate, not evidence to skip
   filing.
2. **Place it.** An epic is an outcome with a `Done when`; nest a new issue
   under one only when it truly belongs to that outcome. Parents are optional —
   a bug or one-off tidy stands alone. Record the placement decision (why an
   epic, or why standalone) on the issue.
3. **Write for a stranger.** Cite `path#Symbol`, never line numbers. State the
   observed problem or wanted outcome and, for an epic, its `Done when`.
4. **Link, don't restate.** Sub-issues are the only parent relation; a blocker
   is a dependency, never a parent. Every PR carries one `Closes #n` or
   `Refs #n` line per issue it touches; never a closing keyword in a commit.
5. **Keep the board and Horizon true.** Status follows the work, not the
   calendar. Horizon is the owner's call — start new epics at `Later` and
   promote only under the repo's documented rule, logged.
6. **Sweep for stale.** Close with merge evidence or a stated reason (not
   planned, duplicate-of, superseded); update, don't just re-read, an issue
   whose evidence has moved past its last comment. Quiet is not done.

See [references/filing.md](references/filing.md) for duplicate/placement
detail and [references/linking-and-board.md](references/linking-and-board.md)
and [references/hygiene.md](references/hygiene.md) for the rest.

## Authority limits

Create, edit, comment, link, close and set Status inside the repo's rules.
Never delete an issue, never transfer it, never edit another author's comment,
and never change Horizon except by the repo's own promotion rule. Worker lanes
report follow-ups; only the coordinator or steward writes the tracker.

## Rate limits

Prefer REST for issue/PR state, labels, comments, search and closing; GraphQL
carries its own, smaller quota and is easy to exhaust with board reads. On a
rate limit or secondary limit, stop and report the reset time — never loop or
retry. See [references/rate-limits.md](references/rate-limits.md).

## Steward mode

A coordinator hands off all tracker work with: "load the `tracker` skill in
steward mode; queue `<path>`" — normally to a cheaper subagent (default route:
a Sonnet subagent). The steward drains an append-only queue file in order,
replays idempotently, and returns a digest of at most 15 lines. See
[references/steward.md](references/steward.md) for the queue schema and
replay contract.
