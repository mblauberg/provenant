# Linking and board upkeep

## Linking

Sub-issues are the only parent relation. A blocker is a dependency
(`blocked_by`/`blocking`), never modelled as a parent — do not force a
dependency into the sub-issue tree to make it visible.

For checklist epics, keep future slices in the epic until work is leased.
Create leaf issues only on lease, reusing an existing issue for the same task:
one open issue per live task. Once an issue exists, use a native sub-issue link
as its only parent relation and remove its checklist mirror. A checklist is
planning text, not a second child inventory.

Every pull request carries one line per issue it affects, `Closes #n` when
merging leaves no user or external-action gate, otherwise `Refs #n` (or
`References #n`) leaving the issue open. Never put a closing keyword in a
commit message; the pull request owns closure. A branch may serve one issue,
several, or a slice of one (`setup-repo`'s naming doctrine covers branch
naming) — the PR's link lines are the source of truth for which issues it
covers, not the branch name.

## Board

Status follows the work, not intent: `Backlog` (untriaged or deferred),
`Ready` (bounded scope, authority and acceptance evidence), `In progress`
(an owner is executing), `In review` (PR, checks or independent review
active), `Awaiting user` (machine work done, a user decision remains), `Done`
(integrated or closed with its terminal reason recorded). Move an issue's
Status when its state actually changes, not on a schedule.

The repository declares concurrent writer lanes and an advisory Ready cap of
about two to three times that number. At the cap, pause new filing and work
down or reconcile existing items. This is judgement-based flow control, not a
mechanical block or a change to Status semantics. Filing is not approval:
agents never self-approve their issues into Ready.

## Priority or horizon

The repository's priority or horizon field, when it declares one, is the
owner's signal, not an agent's. Agents start new items at its documented
default and may promote an item only under the repository's own documented
promotion rule — never on judgement alone — and every promotion
gets a ledger entry (what moved, why, who/what rule authorised it). The same
restraint applies to demoting: record the reason.
