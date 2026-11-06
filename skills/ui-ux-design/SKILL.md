---
name: ui-ux-design
description: "Use for UI/UX review, design, design systems and the shadcn on Base UI starter. Review is read-only by default; pair changes with implement. Source review belongs to code-review."
---

# UI/UX design

Start from the approved outcome, existing components and tokens, supported users and actual rendered states. Preserve the existing visual language when the brief calls for it; change it deliberately when the outcome requires it. Read [craft](references/craft.md) for interaction, accessibility and visual proof. Apply the [product design defaults](references/principles.md) when designing or judging a surface: action safety, feedback, surface states, modality, tables, keyboard, accessibility, colour, tokens, content and trust cues. A project's own design doc overrides them.

Review is read-only: inspect source and rendered surfaces without changing project or external state. Store captures and reports outside the reviewed tree; recording findings as an indexed repository document is a separate companion step under `engineering-docs` authority. If a browser is unavailable, mark the rendered surface not tested. Use playwright for browser inspection. For a requested change, implement owns delivery; review the rendered result afresh after repair.

Scale proof to the risk and changed surfaces. Choose representative viewports, states and input modes from the criteria. View every capture; screenshots alone do not establish keyboard operation, accessibility conformance or behaviour. Record the evidence path, what was tested and what was not. Separate observed facts from design judgement.

For independent capture questions, use `explore` over named areas. Inspect the plan before submission:

```sh
provenant run --use explore --route junior --over desktop,mobile --file brief.md --explain
```

Retained visual prototypes live in the repository and are indexed through engineering-docs. A private Claude artifact can present an authorised prototype; the repository file remains the source. Uploads need authority.

For a new component system, the [shadcn on Base UI guide](references/shadcn-base-ui.md) and [starter](assets/shadcn-starter/) provide a starting point. Existing project rules and harvest boundaries take precedence.
