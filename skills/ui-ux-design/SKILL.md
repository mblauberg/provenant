---
name: ui-ux-design
description: "Use for frontend UI/UX: read-only review of rendered visual, interaction, accessibility, responsive or design-system quality; or requested build, redesign, fix, polish, implementation, and live iteration. Not for source-diff review or React profiling."
---

<!-- Modified from Impeccable for this harness; see the repository THIRD_PARTY_NOTICES.md. -->

# UI/UX design

Judge or shape frontend experiences against approved product intent, the
project's real design owners, and observable UI evidence. Infer intent from the
whole request. Do not make the user select a mode or answer a taste
questionnaire, name an internal lifecycle skill, or recite an authority phrase.

## Select mode and authority

- **Review** is the read-only branch for critique, audit, comparison, advice
  without change intent, and every explicit read-only request. It permits no
  source or project-state mutation, installs, project-local build/cache output,
  dev-server injection, browser submission, or external write. Read-only browser
  navigation, GETs, and screenshots are allowed when output/cache stays outside
  the protected tree. Write only an assigned outside report. Load
  [review](references/review.md).
- **Design** covers advisory directions and requested change. A clear request to
  build, redesign, fix, polish, or implement routes internally to `implement` as
  primary with this skill as design companion. Do not ask the user to name that
  routing. Only genuinely unsettled consequential product choices route to
  `scope`; `engineering-docs` owns durable document placement. Load
  [design](references/design.md).
- **Live** is a write-capable design sub-mode, never review. Route setup,
  injection, wrapping, selection, and cleanup through the same internal
  implementation lifecycle and its bounded project paths. Load
  [live](references/live.md).

If one request combines review and source changes, keep implementation primary
and run a fresh read-only UI review after the changes. An explicit read-only
constraint wins. Stop only for a real missing path/action authority or a
material product choice that cannot be discovered or safely inferred.

## Ground the work

Load [reference grounding](references/reference-grounding.md). Establish
approved intent, canonical local owners, actual pixels and states, supported
users, and viewports. Classify change as preserve, extend, or overhaul;
preserve is the default for an existing product.

## Load only relevant depth

Use [surfaces](references/surfaces.md) for context; [visual system](references/visual-system.md)
for colour, type, space, imagery, and hierarchy; [design systems](references/design-systems.md)
for shared tokens and components; [interaction and states](references/interaction-states.md);
[responsive accessibility](references/responsive-accessibility.md); [motion](references/motion.md);
[content and conversion](references/content-conversion.md); and [visual QA](references/visual-qa.md).
Load only owners activated by the request.

## Boundaries and completion

`code-review` owns PR/diff review and its finding schema; `playwright` supplies
browser operation; `web-stack-conventions` supplies current standards deltas;
`react-performance` owns measured React performance; writing specialists own
general rewriting. Report evidence as `verified` or `judgement` and every
selected lane as `tested`, `failed`, `not tested`, or `not applicable`.
Design cannot certify itself; deterministic checks, fresh review, and user
acceptance stay with the enclosing lifecycle.
