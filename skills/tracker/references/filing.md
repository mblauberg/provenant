# Filing detail

## Evidence and provenance

An agent-filed defect cites an event that already happened (a failure,
contradiction or review finding) with evidence a newcomer can resolve. Refuse a
speculative defect; this evidence rule does not constrain forward-looking design. A feature the owner has authorised needs no prior event; it needs
a spec. Apply `source:agent` where the repository declares that label, through its
declarative sync, never by hand (pruning deletes undeclared labels). End the body
with `Filed-by: <PROVENANT_AGENT_LABEL> <run>` using actual identities; the shared
token's account name does not identify the filer.

## Search record

Put this block in the body:

```text
Searched: "<title keywords>"; "<symbol or error>"; open PRs touching <path>
Closest: #123 (different outcome: <why>), none otherwise
```

Never assert "no duplicate" without it. Outcomes: duplicate (close the new one
`duplicate-of`, add detail to the canonical issue), same outcome (extend in place and
note what changed), or new.

After any create error, timeout or slow response, look up issues with the exact
title before retrying, including closed issues, and compare returned titles literally. If the
create succeeded, use that issue. If the lookup cannot establish the result,
reconcile it before retrying; do not blindly repeat the create.

Optional aid, never a gate: semantic search with
`gh api 'search/issues?search_type=hybrid&q=TITLE+is:issue+repo:OWNER/NAME'`.
Include `is:issue`; without it GitHub silently falls back to keyword search.

## Placement

An epic is an outcome, not a folder. Nest an issue only when it serves that epic's
unchanged `Done when`; otherwise it stands alone or starts a successor. Bugs and
one-off tidying stand alone unless they are a slice of a `Done when`. One feature's
acceptance list and one PR train is a feature with task sub-issues, not an epic.
Record why on the issue. Never mirror children in a checklist in the parent body;
unstarted work is a `## Backlog` line until promoted.

## Follow-ups

A worker reports a follow-up as a line in its report. The chair dedupes it against
open issues and files it, or drops it. One finding rarely deserves its own issue.
