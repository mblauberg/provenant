---
name: ui-ux-design
description: "Use for UI/UX: read-only review by default (visual, accessibility, responsive, interaction), or design, redesign, design systems, and live iteration. Pair changes with implement. Source-diff review: code-review; React profiling: react-performance."
---

# UI/UX design

Judge or shape frontend experiences against approved product intent, the
project's current design owners, and observable UI evidence.

## Review or change

**Review** covers critique, audit, comparison, and advice without requested
source changes. It is read-only. Inspect source and rendered state using
non-mutating navigation, GETs, and screenshots, with any output or cache outside
the reviewed project tree. Do not change project or external state. Write only
an assigned report outside that tree. Load [review](references/review.md).

**Design** covers directions and requested UI changes. Advice without source
changes remains read-only. A request to build, redesign, fix, polish, or
otherwise change the product uses `implement` as primary and this skill as its
design companion. Live iteration belongs to that change branch; load
[live](references/live.md). When review and changes are combined, keep
implementation primary and perform a fresh read-only review afterwards. An
explicit read-only constraint wins; otherwise ambiguous intent defaults to
review.

## Ground and complete the work

For design or changes, start with the approved outcome, canonical local
components and tokens, rendered pixels and states, supported users, and
viewports. Classify the work as preserve, extend, or overhaul; preserve is the
default for an existing product. Load
[reference grounding](references/reference-grounding.md) and
[design](references/design.md), including its reuse and composition guidance.

Load only the depth the request needs: [surfaces](references/surfaces.md),
[visual system](references/visual-system.md), [design systems](references/design-systems.md),
[interaction and states](references/interaction-states.md),
[responsive accessibility](references/responsive-accessibility.md),
[motion](references/motion.md), [content and conversion](references/content-conversion.md),
or [visual QA](references/visual-qa.md).

Label evidence as `verified` or `judgement`, and each selected lane as `tested`,
`failed`, `not tested`, or `not applicable`. Deterministic checks and user
acceptance remain with the enclosing implementation lifecycle; design does not
certify itself.
