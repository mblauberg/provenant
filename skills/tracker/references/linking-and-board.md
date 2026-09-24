# Linking and board upkeep

## Linking

Sub-issues are the only parent relation. A blocker is a dependency
(`blocked_by`/`blocking`), never modelled as a parent — do not force a
dependency into the sub-issue tree to make it visible.

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

## Horizon

Horizon is the owner's priority signal, not an agent's. Agents start new
epics at `Later` and may promote an item only under the repository's own
documented promotion rule — never on judgement alone — and every promotion
gets a ledger entry (what moved, why, who/what rule authorised it). The same
restraint applies to demoting: record the reason.
