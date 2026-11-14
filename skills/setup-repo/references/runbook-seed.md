# Work-item runbook seed

Copy these sections into the target's declared work-item runbook (default
`docs/runbooks/github-workflow.md`), cut what does not apply, and let the project add
its own mechanics. Open the runbook with `Owner:` (the responsible role) and
`Anchor:` (the target's source symbols or tests that the text must track).
This repository's own runbook is the worked example.

## Statuses

Use this repository's [GitHub workflow](../../../docs/runbooks/github-workflow.md#status)
as a starting point; document the target's Status choices in its runbook. Labels
describe content or gates, not progress.

## Branches

Document the target's branch conventions.

## Closing links

Document the target's closing-link conventions. This repository's
[GitHub workflow](../../../docs/runbooks/github-workflow.md#branch-commit-pull-request)
is a worked example.

## Merge authority

The target's owner decides and records it. Ask whether agents may merge once
tier-appropriate review and CI are green on the exact head, or whether every merge
needs a user. Credential setup, rewrites of or pushes to shared branches outside
authorised merges and risk downgrades stay user gates. Record the resulting
authority in the target's runbook.
