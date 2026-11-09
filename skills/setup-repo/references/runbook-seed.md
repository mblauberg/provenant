# Work-item runbook seed

Copy these sections into the target's declared work-item runbook (default
`docs/runbooks/github-workflow.md`), cut what does not apply, and let the project add
its own mechanics. Open the runbook with `Owner:` (the responsible role) and
`Anchor:` (the target's source symbols or tests that the text must track).
This repository's own runbook is the worked example.

## Statuses

Project Status is the sole workflow state: Backlog (untriaged or deferred), Ready
(bounded scope, authority and acceptance evidence), In progress, In review (PR,
exact-head checks or independent review active), Awaiting user (a user decision
remains), Done (integrated, or closed with a terminal reason). Labels describe
content or gates, never progress; never add a status label to `labels.yml`. A project
board is optional: create it with `gh project create` and set exactly these six
options in the UI, since the CLI cannot replace them.

## Branches

`<type>/<area>-<slug>`, at most 48 characters. `<type>` is `feat`, `fix`, `refactor`,
`perf`, `test`, `docs`, `chore`, `ci` or `build`; `<area>` is a declared area; the slug
is two to five kebab-case words. No issue numbers and no run vocabulary (batch, wave,
lane, model name) in a branch. `provenant worktree create BRANCH --base REV` makes the
worktree, named by replacing `/` with `-`.

## Closing links

One `Closes #n` per issue in the PR body only when merge leaves no user or external
gate; otherwise `References #n` and leave the issue open. Never use a closing keyword
in a commit message. A new commit needs green checks on the new head. Review
the delta of a later commit; a clean base merge needs only CI. Apply the
HARNESS head-move classes in shadow until tooling enforces them, retaining
current exact-head landing gates.

## Merge authority

The target's owner decides and records it. Ask whether agents may merge once
tier-appropriate review and CI are green on the exact head, or whether every merge
needs a user. Credential setup, rewrites of or pushes to shared branches outside
authorised merges and risk downgrades stay user gates. An authorised merge prunes its
own merged branch and worktree; the chair decides other branch and worktree removals,
recording each removed tip. Provenant's own choice (merge directly once gates pass) is an offer, not a default.
