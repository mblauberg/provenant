# Autonomous-implementation mode

Pulls issues already marked **accepted/ready** by a user or governance gate
(an issue tracker `ready`/`accepted` label or column, or equivalent) and
drives each through the existing lifecycle skills, unattended, up to the
merge gate. User-controlled merge is the global default; the nearest
repository merge policy governs, and a repository that authorises agent
merges (as provenant's GitHub workflow runbook does) lets the run merge once
that policy's review and CI gates pass. This is a thin mode of `orchestrate`: it selects
and sequences work; it does not scope, review, or accept it, and it does not
fork a new receipt format.

This mode never assumes a delivery-run receipt already exists for a queued
issue. `implement`'s own entry gate only requires `RUN.json` for
substantial+ work and lets routine minor work proceed without one (see
`implement/SKILL.md`); an unattended queue has no user in the loop to
notice a missing receipt, so this mode cannot rely on that per-issue
discretion. It always directs `implement` to create the canonical
`delivery-run` (`deliver/templates/RUN.template.json`) for **every** queued
issue, including routine minor ones, and only ever reads/reports the one
receipt `implement` produces — it never creates, assumes, or forks a second
one.

## Entry gate

- Only issues already in an **accepted/ready** state route here. An issue
  missing acceptance criteria, non-goals, or authority bounds is out of scope
  for this mode — route it to `scope` first; never infer owner decisions to
  make an issue "ready enough".
- Requires the same build ceiling / write / external-action authority
  `implement`'s entry gate requires. Missing authority stops the mode before
  dispatch, not mid-run.
- Requires `implement` to create a `RUN.json` delivery-run receipt for every
  issue this mode dispatches — treat unattended queue processing as the
  "project policy requests one" condition in `implement`'s own entry gate, so
  the routine-minor-work exemption never applies here. This mode has no
  receipt of its own; it needs one machine-checkable receipt per issue to
  record and resume the queue.

## Loop

1. **SELECT** the next ready/accepted issue from the queue (oldest-first
   unless the user orders otherwise). One issue in flight per lease unless
   independent partitions justify parallel issues — apply the normal
   decomposition/value gate ("When This Pays" in `SKILL.md`) before running
   more than one at once.
2. **RUN** `implement` for that issue verbatim: its entry gate, adaptive
   plan, `tdd`/`refactor`/`diagnose` legs, `code-review`, and `evaluate`
   where required — and its loop step 1, directed to create the canonical
   `delivery-run` `RUN.json` for this issue regardless of size. This mode
   adds no parallel review path of its own — it delegates the entire
   verified-implementation loop, including receipt creation; it never
   pre-supposes a receipt is already sitting there.
3. **STOP** at `implement`'s own user-acceptance gate (`awaiting_acceptance`
   / PR opened for review) unless the nearest repository merge policy
   authorises agent merges, in which case merge once that policy's review
   and CI gates pass on the exact head. Where no such policy exists, do not
   merge. Never promote, and do not carry
   that issue's `RUN.json` past the state `implement` leaves it in. **Never
   fork a new receipt** — the canonical `delivery-run` from
   `deliver/templates/RUN.template.json`, created by `implement` at this
   mode's direction for this specific issue, is the only receipt; this mode
   reads and reports it, it never replaces it and never keeps a shadow
   receipt of its own.
4. **RECORD** the stopped issue (id, receipt path, gate reached) and advance
   to the next ready/accepted issue.
5. **DEFER** a blocked or repair-exhausted issue back to the user/`scope`,
   exactly as a standalone `implement` run would (its scaled repair-budget per
   risk tier: routine 2, substantial 4, crucial/terminal 5 cycles); this mode
   does not retry past that boundary or route around it.
6. **FINISH** when the ready/accepted queue snapshot is empty. There is no
   STOP file, no cross-session resume state, and no continuation past a
   user decision — each run is bounded to the queue it started with.

## Distinguishing from autopilot

| | Autonomous-implementation mode | `autopilot` |
|---|---|---|
| Scope source | pre-scoped accepted/ready issues only; never scopes | self-scopes open-ended missions |
| Duration | one bounded pass over the current queue snapshot | persistent, survives sessions/crashes, until user `STATUS: STOP` |
| User gate | per issue: user PR-review/merge by default, or the repository's agent-merge policy where one exists | user-out-of-loop during the loop; reaches users only at hard/one-way-door gates |
| State | uses `implement`/`deliver` receipts only, no durable lab state | `GOAL.md`/`STATE.md`/ADR/queue durable cross-session state |
| Authority | lower — never proceeds past the per-issue merge gate its repository's policy sets | higher — authorised to keep driving without per-item user sign-off |

Trigger this mode only on an explicit **bounded, pre-scoped issue queue**
("work through the ready/accepted issues", "implement the accepted backlog to
PR"). A request for a standing, run-until-STOP mission — even one whose work
is also software implementation — is `autopilot`'s territory; keep the
two triggers from overlapping. When a mission's boundedness is unclear,
default to this lower-authority mode and let the user explicitly promote to
`autopilot` if the work turns out to be open-ended.

## What this mode is not

- Not a scoping tool — an issue without acceptance criteria stops the mode;
  it does not trigger ad hoc scoping to make the issue runnable.
- Not a release tool — `release` remains a separate, user-authorised step.
  Merging follows the nearest repository merge policy; absent one, user merge
  happens after this mode stops.
- Not a new receipt format — every issue's evidence lives in its own
  `implement`/`deliver` receipt; this mode only sequences and reports them.
