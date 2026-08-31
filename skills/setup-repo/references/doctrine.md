# GitHub doctrine

Extracted and generalised from provenant's
[`docs/runbooks/github-workflow.md`](https://github.com/mblauberg/provenant/blob/main/docs/runbooks/github-workflow.md).
When scaffolding a new project, write a copy of that runbook into the target
repository (e.g. `docs/runbooks/github-workflow.md`) with these project-neutral
sections, then let the project add its own mechanics (worktree helper, merge
authority, CI commands). Do not paraphrase the doctrine below into something
weaker; copy it and cut only what does not apply.

## Statuses (Project board)

| Status | Meaning |
|---|---|
| Backlog | Untriaged or explicitly deferred. |
| Ready | Accepted with bounded scope, authority and acceptance evidence. |
| In progress | An owner is executing the accepted work. |
| In review | The pull request, checks or independent review is active. |
| Awaiting user | Machine work is ready but a user decision or acceptance remains. |
| Done | The item is integrated, or closed with its terminal reason recorded. |

Project Status is the sole workflow state; labels describe content or
automation switches, never progress. Never add a status label to
`labels.yml`.

## Branch naming

Name every implementation branch `issue-<n>-<slug>`, for example
`issue-148-runbook-mechanics`. The number ties the branch back to its issue
unambiguously; the slug is a short kebab-case reminder, not the source of
truth.

```sh
gh issue develop <n> --name issue-<n>-<slug> --base main
```

## Closes vs References

Link every pull request to its issue with one of two keywords, chosen by
whether merge leaves any gate open:

- `Closes #<n>` only when merging leaves no user or external-action gate —
  merge auto-closes the issue.
- `References #<n>` otherwise, and leave the issue open for the remaining
  gate.

Never put a closing keyword in a commit message; the pull request owns
closure. Reference the issue from every commit body instead (`Refs #<n>`).

## Status transitions

| Transition | Owner | When |
|---|---|---|
| `Backlog` to `Ready`, `Done` or unchanged | Triage: user, or an agent inside granted authority | The triage result is recorded in an issue comment |
| `Ready` to `In progress` | Implementing agent | Work on the accepted scope starts |
| `In progress` to `In review` | Implementing agent | The pull request, exact-head checks or independent review is active |
| `In review` to `Awaiting user` | Implementing agent | Machine gates pass; a user decision or acceptance remains |
| Any later state to `Done` | Merging agent or user; merge auto-closes a `Closes #<n>` issue | No gate remains; the terminal reason or integrated pull request is recorded |

Triage records exactly one result per issue comment: **Accepted** (bounded
scope, authority, remaining gates; set `Ready`), **Rejected** (evidence-based
reason and reopen condition; set `Done`, close as not planned), **Deferred**
(reason and reconsideration condition; leave `Backlog`), or **Duplicate**
(link the canonical item; set `Done`, close as not planned).

A new commit invalidates exact-head evidence (checks, independent review);
rerun both against the new head before merge.

## Native sub-issues

The parent issue owns change scope, story and remaining gates; child issues own
independently deliverable slices. Creating the native `sub_issues` relation
requires explicit external-write authority and the child's numeric issue id;
otherwise return the proposed relation. A parent URL is navigation only. Never
mirror children in a checklist.

## Merge authority

State this explicitly in the target project's own runbook — it is a
per-project decision, not something this skill can decide for a new
repository:

- Whether agents may merge directly once tier-appropriate review and CI are
  green on the exact head, or whether every merge needs a user click.
- The standing user gates that never move regardless of automation: unmerged or
  forced branch deletion, history rewrites, credential/connector setup, pushes
  to shared branches outside authorised merges, and risk-tier downgrades.
  Pruning a *merged* branch's own refs and worktree is carried by the merge.

Provenant's own choice (agents merge directly once gates pass, user gate only
when the agent is stuck) is a proven default worth offering, but the new
project's owner must decide and record it, not inherit it silently.
