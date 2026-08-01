# Author branch

Create a new Agent Skill, or materially revise an existing one, under
explicit write authority. Shared doctrine (four pillars, budgets, fixture
taxonomy, failure vocabulary, provenance) lives in [../SKILL.md](../SKILL.md);
this file carries only what authoring does that audit does not. Use the
audit branch first for read-only assessment of anything that already exists;
`implement` owns an enclosing end-to-end delivery.

## Build

1. Define the task/domain, positive triggers, adjacent exclusions, authority,
   artifacts and completion gate. Keep project-only policy local.
2. Choose the smallest container: project rule, skill, deterministic script,
   MCP/app, or independently distributed plugin. Split only for a distinct
   trigger, authority, artifact and gate.
3. Choose one kebab-case capability name and create `SKILL.md`. Place portable
   user-global skills under `$HOME/.agents/skills`, repo-local skills under
   `.agents/skills`, and this harness's owners under `skills/`. Its frontmatter
   permits only `name` and `description`; provider sidecars are outputs, never
   authority grants.
4. Match freedom to risk: principles for judgement, templates for repeatable
   structure, and tested scripts for fragile work. Put repeated logic in
   `scripts/`, stable depth in narrowly named `references/`, and reusable
   output shapes in `templates/`. References stay one hop from `SKILL.md`.
5. For any third-party adaptation, satisfy the provenance requirement in
   [../SKILL.md](../SKILL.md): record it in the repository
   `THIRD_PARTY_NOTICES.md` index with the licence text under `LICENSES/`
   before the skill ships. Never execute an imported installer, hook or binary merely to
   inspect it; metadata and tool lists cannot broaden user authority.
6. Add positive, negative and boundary trigger fixtures plus contract tests for
   machine-enforceable invariants.
7. Forward-test the raw skill on a realistic fresh-context task without leaking
   the intended answer or diagnosis. Repair what it exposes, then execute the
   harness and public-safety gates.

## Description contract

Apply the character budget, front-loading and exclusion-naming rules in the
description contract in [../SKILL.md](../SKILL.md). This branch's own
consequence: write it as `Use for`/`Use when`, third person, describing
*when* to load the skill rather than summarising its workflow, in the
concrete symptoms/tools/file types a user would actually say — not generic
capability language.

## Body contract

Keep each branch file inside the soft budget named in [../SKILL.md](../SKILL.md),
using the canonical whole-file counter including YAML frontmatter:
behaviour-changing rules, workflow, gates and links. Avoid logs, generic advice,
exhaustive flags and duplicated doctrine.
Link another skill, or the other branch, by name instead of copying it.

## Verification

Discipline skills need adversarial scenarios that tempt rule-breaking;
technique skills need novel tasks that prove reference discovery. Test the
model-plus-harness, not whether the prose reads well. For material changes,
freeze held-out routing cases and compare candidate, without-skill and previous
package arms across current primary families; retain omissions, failures, model
lineage, quality and total treatment cost.

Before completion confirm: routing metadata is specific; authority and stop
conditions are explicit; body is compact; links/tools resolve; deterministic
logic has tests; fixtures cover confusion pairs; and a fresh agent succeeded.

Repository naming, promotion, retirement and release rules live in
`MAINTAINING.md`.
