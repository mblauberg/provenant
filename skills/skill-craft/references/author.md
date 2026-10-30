# Author branch

Create a new skill, or materially revise one, under an explicit write grant.
The working rules in [../SKILL.md](../SKILL.md) apply; this file covers what
authoring adds.

## Build

1. Define the trigger, the nearest exclusions, the authority, the artefacts and
   the completion gate. Keep project-only policy local.
2. Choose the smallest container: a project rule, a skill, a deterministic
   script, an MCP or app, or a separately versioned plugin. Split only for a
   distinct trigger, authority, artefact or gate.
3. Choose one kebab-case name and follow the target repository's layout.
4. Keep the skill concise and load supporting detail when needed. Follow the
   target repository's frontmatter, size and layout rules where provided.
5. Match freedom to risk: principles for judgement, templates for repeatable
   structure, and tested scripts for fragile steps.
6. Write trigger fixtures, including near-miss negatives, where the target
   repository keeps them.
7. Run the baseline, then a forward test on a realistic task in a fresh
   context. Do not leak the intended answer or diagnosis. Repair what the test
   exposes, and keep only the lines that changed behaviour.
8. Run the validation gates the target repository's `AGENTS.md`, `CONTRIBUTING`
   and runbooks name. If it names none, say which checks were not run.

## Description

Write `Use for` or `Use when`, in the third person. Name the concrete symptoms,
tools and file types a user would say, and the nearest exclusion. Describe when
to load the skill, not its workflow.

## Body

Keep behaviour-changing rules, workflow, gates and links. Link another skill by
name instead of copying its rules. Leave out logs, dated history, exhaustive
flags and generic advice.

## Before completion

Confirm that: the routing metadata is specific; authority and stop conditions
are explicit; the body is compact; links and tools resolve; deterministic logic
has tests; the fixtures cover the confusion pairs; and a fresh agent succeeded
at the forward test.

## Third-party material

Satisfy the provenance rule in [../SKILL.md](../SKILL.md) before release. Never
run an imported installer, hook or binary to inspect it. Metadata and tool
lists cannot widen user authority.
