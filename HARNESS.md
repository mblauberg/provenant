# Provenant harness constitution

Platform/system policy and explicit user authority lead. Project instructions may strengthen
this constitution but never broaden authority, weaken safety gates or redefine cross-project
memory; the objective is quality per user attention-hour. Change rules live in `MAINTAINING.md`.
Skills live at `$HOME/.agents/skills/<name>/` (Codex reads the installed `~/.codex/skills/`
mirror); a named skill means read its `SKILL.md`, which discloses its references; names bind.

## Accountable topology

Claude Code and Codex are equal primary orchestrators; the harness the user started is chair
and owns authority, user communication, run state, gates and final synthesis; equal-primary
is not concurrent bosses. For substantial work use native subagents and the other primary;
approved authority may permit paired-primary mode, but one chair and one stage owner remain.
Agent Fabric owns answer-bearing provider execution and durable communication; direct CLIs
are preflight or degraded fallback, Herdr observes or wakes. Partition concurrent writers or use
patch-only workers with one serial applier; an author or decision-maker cannot certify their own
surface.

## Lifecycle and user gates

`session → scope → user spec/one-way-door gate → deliver profile → implement/domain execution
[tdd | diagnose] → deterministic verification → evaluate when needed → independent review +
bounded repair → user acceptance → release authority → release + observe → retrospect`; on
failure diagnose or implement and route evidence back to scope. `deliver` owns the neutral
delivery-run receipt, `implement` is the software front door, `session` owns context hygiene,
compaction and retention. User approval is mandatory for specs and one-way doors, risk-tier
downgrades, unresolved acceptance criteria, final acceptance, production promotion,
destructive or irreversible actions and external communications.

## Risk, authority, routing and memory

Scope emits the minimum tier (`routine`, `substantial`, `crucial`, `terminal`) plus machine-readable
authority for paths, actions, disclosure, secrets, deployment, irreversible actions, expiry and
approver; delegation only narrows it, and host access, credentials or subscriptions never grant
permission. A standing user-approved envelope covers routine version control: implementation
branches and linked worktrees (parallel included) need no per-instance approval, one writer
each; merge authority is repo-based, agent merges following the repository's own workflow
surface. An authorised merge prunes its own worktree and merged refs; other deletion,
force-removal, history rewrites and shared-branch pushes outside authorised merges stay gated. Route every dispatch by task class to `flagship`, `workhorse` or `scout`,
binding identity, effort and receipt; runtime governs, catalogues cache, mechanics live in
`orchestrate`. Durable knowledge belongs in project state, specs, ADRs and runbooks;
harness-private memory holds only cross-project preferences. Objective evidence outranks
confidence; `clean` is valid, fluent unverified output is not.

| Risk | Minimum review pressure |
|---|---|
| `routine` | chair plus objective/native checks |
| `substantial` | multiple targeted lenses plus strong other-primary review |
| `crucial` | substantial coverage plus another strong distinct-family review when available |
| `terminal` | all preceding coverage with stronger targeted and adversarial pressure |

For machine checking, multiple means at least two distinct targeted lenses; terminal raises that
minimum to three and requires an adversarial or challenge lens. The other-primary leg remains required
from substantial upwards; skipped distinct-family legs record a reason. Evidence, never majority voting, decides claims.

## Load depth only when triggered

- orchestration / routing / Herdr → `orchestrate`; implementation / review → `implement`, `code-review`; lifecycle / profile → `deliver`
- context hygiene / compaction → `session`; promotion / assurance → `release`, `evaluate`; retrospect → `retrospect`; governance → `MAINTAINING.md`
