---
name: skill-craft
description: "Use for creating, revising, or read-only auditing an Agent Skill: SKILL.md, triggers, progressive disclosure, fixtures, overlap, and token cost. Defaults to audit; edits need explicit authority. Not for plugin packaging or delivery; use implement."
---

# Skill craft

A skill is a reusable, triggerable procedure, not a solved-task narrative
and not a place to park static reference. Every skill earns its slot on four
pillars: **Trigger** (fires on the right requests, stays silent on adjacent
ones), **Structure** (frontmatter contract, progressive disclosure, soft
length budgets), **Steering** (leading words and bright-line rules over
repeated emphasis), and **Pruning** (cutting sediment, duplication, sprawl
and no-ops before they ship).

This skill has two branches and defaults to the safer one.

- New or materially revised skill → [references/author.md](references/author.md).
- Assessment of an existing skill, read-only → [references/audit.md](references/audit.md).
- **Default with no explicit authoring request: audit.** Do not cross
  audit → edit without an authority envelope naming `implement` as
  action-owner; surface that requirement instead of silently fixing what an
  audit finds.

## Shared doctrine

Both branches follow this without restating it in two voices:

- **Three-mode invocation:** direct audit; direct authoring under authority;
  or composed, where audit is a companion to a primary lifecycle owner (for
  example `implement`) that stays the action-owner.
- **Budgets:** branch-file limits use the canonical whole-file counter,
  including YAML frontmatter. `SKILL.md` is at or below 500 words and the audit
  reference is at or below 600; the standing catalogue-description lever is
  governed in `MAINTAINING.md`.
- **Token model, three-tier:** always-loaded frontmatter, triggered branch
  body, and entrypoint references loaded at one hop. Reference documents may
  link to sibling references, but that depth is not recursively enforced by
  `check_harness.py`.
- **Reuse boundary:** global skills express cross-project triggers, procedures
  and gates. Convert contextual values into parameters, keep examples
  synthetic and leave project policy local. Global promotion requires evidence
  from at least two projects. Use
  [promotion_readiness.py](scripts/promotion_readiness.py) to integrity-check the
  local evidence inventory before human review; hashes prove retained content,
  not its truth or promotion authority. Follow the repository gate in
  [MAINTAINING.md](../../MAINTAINING.md).
- **Trigger fixtures:** positive, negative, boundary and composition cases.
  A keyword match is a candidate, not ground truth. Changes to routing text
  re-run the held-out set.
- **Failure taxonomy:** name the failure, don't just gesture at quality:
  premature-completion, duplication, sediment, sprawl, no-op, negation-only.
- **Description contract:** front-load the first 250 characters with trigger
  words and the nearest exclusion; keep boundary-routing negation explicit
  (name the skill to use instead); never drop it for brevity.
- **Provenance:** any adaptation from a third-party source records source
  URL, version/commit, retrieval date and licence in the repository
  `THIRD_PARTY_NOTICES.md` index, with the full licence text under `LICENSES/`.
  See [THIRD_PARTY_NOTICES.md](../../THIRD_PARTY_NOTICES.md) for this skill's
  own Skill Optimizer lineage.
