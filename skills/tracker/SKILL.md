---
name: tracker
description: "Use for GitHub issues/boards: dedupe before filing, epic placement, priorities, PR links, stale issues and steward queues. Not for execution; use implement or orchestrate."
---

# Tracker

A generic issue-and-board steward. Search the repository's instructions and
docs for a `Repository process` heading and read it first. A project with its own tracker commands and runbook owns the mechanics;
this skill supplies duplicate, placement and closure judgements. Use raw `gh`
only when no project command exists.

## Quickstart

1. **Evidence before filing.** An agent-filed issue cites an event that already
   happened, such as a failure, contradiction or review finding. Refuse to file
   speculation without evidence. Agents never approve their own filings.
2. **Search before filing.** Run at least two searches (title keywords, then a
   symbol or error string) and record what you ran and found in the issue body,
   even when no duplicate is found. Consider reusing or
   widening partial matches.
3. **Place it.** An epic is an outcome with a `Done when` needing at least two
   independently shippable features over more than one wave; if it fits one
   feature's acceptance list (about 10 lines) and one PR train, file a feature
   with sub-issues. Nest a new issue under an epic when it belongs to that
   outcome. Bugs and one-off tidying can stand alone. Record why.
   For new scoping, later epics hold a `## Backlog` checklist: a line becomes
   an issue with a full brief when leased or Ready, one open issue per live
   task. Keep existing well-scoped leaves.
4. **Write for a stranger.** The body holds all content: cite `path#Symbol`,
   never line numbers, and never point at scratch files, plan codes, run
   directories, `/tmp` or claude.ai pages. Links to tracked files are fine.
   State the observed problem or outcome and an epic's `Done when`.
   Apply `source:agent` where the repository declares that label, and append `Filed-by: <PROVENANT_AGENT_LABEL> <run>` using actual identities.
5. **Link, don't restate.** Sub-issues are the only parent relation; a blocker
   is a dependency, never a parent. Every PR carries one `Closes #n` or
   `Refs #n` line per issue it touches; never a closing keyword in a commit.
6. **Keep the board true.** Observe the repository's advisory Ready cap, about
   two to three times its declared concurrent writer lanes; pause filing at
   the cap. Status follows the work. Start priority or horizon at the
   repository's documented default; promote only under its logged rule.
7. **Sweep for stale.** Re-verify content when a referenced PR merges, a
   dependency closes or evidence moves; age and `updatedAt` do not decide it.
   Update moved evidence; close with merge evidence or a stated reason
   (not planned, duplicate-of, superseded). Quiet is not done.

See [references/filing.md](references/filing.md) for duplicate/placement
detail and [references/linking-and-board.md](references/linking-and-board.md)
and [references/hygiene.md](references/hygiene.md) for the rest.

## Authority limits

Create, edit, comment, link, close and set Status inside the repo's rules.
Never delete an issue, never transfer it, never edit another author's comment,
and never change a priority or horizon field except by the repo's own rule.
Worker lanes report follow-ups; only the coordinator or steward writes the
tracker.

## Rate limits

Prefer REST for issue/PR state, labels, comments, search and closing; GraphQL
carries its own, smaller quota and is easy to exhaust with board reads. On a
rate limit or secondary limit, stop and report the reset time — never loop or
retry. See [references/rate-limits.md](references/rate-limits.md).

## Steward mode

A coordinator can delegate tracker work to an authorised worker using
Fabric tasks and current issue/PR references. Before retrying a write,
reconcile current issues and tasks with `gh` reads to avoid duplicates.
Return a digest of at most 15 lines with links and blockers. See
[references/steward.md](references/steward.md).

Reference eval cases (not run by CI): [placement cases](evals/placement_cases.yaml).
