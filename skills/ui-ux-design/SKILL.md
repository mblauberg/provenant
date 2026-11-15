---
name: ui-ux-design
description: "Use for UI/UX design and review, and the shadcn on Base UI starter."
---

# UI/UX design

Start from the approved outcome, existing components and tokens, supported users
and the actual rendered states. Keep the existing visual language unless the
outcome needs it changed. Apply the [design defaults](references/principles.md);
a project's own design doc overrides them. The `deliver` skill's engineering
stance records concerns, not specs: weigh alternatives, recommend the long-term
structure and record the call.

Review is read-only by default: inspect source and rendered surfaces without
changing project state, and keep captures and reports outside the reviewed tree.
A requested change is built through `deliver`, then reviewed again on the
rendered result. Use `playwright` for browser inspection; without a browser, mark
the rendered surface not tested. Source review is `code-review`.

## Evidence

- Scale proof to risk. Choose representative widths, states, themes, input modes
  and stress cases (long text, enlarged text, reduced motion).
- View every capture. A screenshot cannot prove keyboard operation, focus entry,
  containment or restoration, assistive-technology support, WCAG conformance,
  motion or cross-browser behaviour.
- Automated checks pin user-facing invariants, not pixels or geometry unless
  those are the invariant.
- Record each surface, state, viewport and input mode as tested, failed, not
  tested or not applicable, with the evidence path. Separate observed fact from
  judgement, and state user impact and the smallest useful remedy.

Retained visual prototypes live in the repository; a private artifact may present
one, and uploads need authority. For a new component system, start from the
[shadcn on Base UI guide](references/shadcn-base-ui.md) and
[starter](assets/shadcn-starter/); existing project rules take precedence.
