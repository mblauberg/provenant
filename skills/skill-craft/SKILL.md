---
name: skill-craft
description: "Use to audit, create or revise Agent Skills, including triggers, structure, examples and overlap. Default to read-only; edits need authority. Use the plugin tooling for packaging and implement for delivery."
---

# Skill craft

A skill is a reusable, triggerable procedure. Assess four pillars:
**Trigger** (requests and exclusions), **Structure** (contracts and disclosure),
**Steering** (leading words and bright-line rules), and **Pruning** (sediment,
duplication, sprawl and no-ops). Keep solved-task narratives and static
reference elsewhere.

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
- **Budgets:** soft ~500-word body per branch file; the standing
  catalogue-description lever is governed in `MAINTAINING.md`; link it,
  don't restate the number here.
- **Token model, three-tier:** always-loaded frontmatter, triggered branch
  body, and entrypoint references loaded at one hop. Reference documents may
  link to sibling references, but that depth is not recursively enforced by
  `scripts/check-harness`.
- **Tier placement:** keep hard rules in `SKILL.md`; references load late.
  For repeated misses, hoist the rule and concrete banned-pattern examples
  into the core instead of expanding the reference.
- **Reuse boundary:** global skills express cross-project triggers, procedures
  and gates. Convert contextual values into parameters, keep examples
  synthetic and leave project policy local. Global promotion requires a
  reusable trigger, artifact, fixture and ownership boundary. When evidence is
  limited, keep it opt-in and provisional. Follow the repository gate in
  [MAINTAINING.md](../../MAINTAINING.md).
- **Project skills that extend a global skill** state their own hard rules, so
  they work without the global skill installed; name each changed default
  under `Project overrides:`; and keep harness and Fabric mechanics out of
  everything but coordination skills. Global skills stay project-agnostic.
- **Trigger fixtures:** positive, negative, boundary and composition cases.
  Each skill has at least two `near-miss` negatives (`relation: negative`,
  target excluded from both expected routes) that share its vocabulary but
  belong elsewhere. Routing-text changes re-run the held-out set.
- **Behaviour changes:** run the fixtures with and without the skill and record
  decisions, differences, model/effort and omissions as reviewer evidence.
  Scores are advisory; only inventory checks may fail CI.
- **Contract-test boundary:** test machine-enforced invariants, never prose.
  Without an enforcing script, add that script or no test; never assert wording.
- **Failure taxonomy:** name the failure, don't just gesture at quality:
  premature-completion, duplication, sediment, sprawl, no-op, negation-only.
- **Description contract:** front-load the first 250 characters with trigger
  words and the nearest exclusion; keep boundary-routing negation explicit
  (name the skill to use instead); never drop it for brevity.
- **Provenance:** when copied or closely paraphrased third-party text, code or
  data ships, record its source URL, commit, retrieval date and licence, and
  retain the required notices and licence text. Ideas and methods alone do not
  require a notice.

Reference eval cases (not run by CI): [boundary traces](evals/boundary_trace_cases.yaml)
and [behaviour cases](evals/behaviour_cases.yaml).
