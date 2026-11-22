# Work-item runbook seed

Copy these sections into the target's declared work-item runbook (default
`docs/runbooks/github-workflow.md`), cut what does not apply, and let the project add
its own mechanics. Open the runbook with `Owner:` (the responsible role) and
`Anchor:` (the target's source symbols or tests that the text must track).

## Statuses

Project Status is the only workflow state: Backlog, Ready, In progress, In
review, Awaiting user, Done. Labels describe content or gates, never progress.

## Branches

`<type>/<area>-<slug>`, with no issue number or run vocabulary.

## Closing links

`Closes #N` only when merge leaves no user or external-action gate; otherwise
`References #N`. Commits never carry a closing keyword.

## Merge authority

The target's owner decides and records it. Ask whether agents may merge once
tier-appropriate review and CI are green on the exact head, or whether every merge
needs a user. Credential setup, rewrites of or pushes to shared branches outside
authorised merges and risk downgrades stay user gates. Record the resulting
authority in the target's runbook.
