<!-- Modified for Provenant. -->

# Design

Start from the approved outcome and the project's current visual language.
Infer missing low-consequence details from local evidence. Ask only when an
answer would materially change the result and cannot be discovered safely.

For materially ambiguous net-new work, use one optional brief: goal and users,
constraints and protected contracts, surface and scope, states,
interaction/recovery, content, visual direction, and success evidence. Batch
material unknowns into one question set; otherwise proceed and record
assumptions. Confirm only consequential unresolved choices.

Classify the work:

- **Preserve** fixes or completes the current language. It is the default for an
  existing product and serves the brief; it never vetoes stronger change the
  brief asks for.
- **Extend** adds a capability using current tokens, components, and patterns.
- **Overhaul** intentionally changes the language and needs explicit approval
  of that consequence.

For net-new or overhaul work, or a brief asking for a stronger result, name
the direction before building: a specific audience and context, the visual
direction in concrete terms (tone, density, type, colour, motion), and one
memorable, deliberate choice. Derive it from the brief and local owners, not a
stock template; revise any part you would produce for any similar brief.
Spend boldness in one place and keep the rest disciplined. When several
directions are requested, each names its own audience emphasis and memorable
choice, and they differ in the render, not only the description. Review the
render against the named direction and cut defaults that do not serve it.

Name protected contracts before editing: navigation, routes, component APIs,
brand assets, form and analytics semantics, legal copy, accessibility
behaviour, and public interfaces. Build component-first and reuse canonical
owners. Scan for the cheapest effective intervention in this order: remove
unnecessary treatment, use the platform, reuse, correct, then add. The order
keeps changes cheap, not timid; a brief asking for a stronger or more
distinctive result overrides "add last".

Inspect existing components, tokens, patterns, registries, and real consumers
before creating anything. Prefer modifying or composing the canonical owner for
the same UI role. In a shadcn/ui project, use `components.json` when present and
otherwise confirm the local component structure. Start from a suitable project
component, shadcn component, block, template, or reviewed registry item,
then customise the project-owned source to its content, states, tokens, and
accessibility contract. For new shadcn work without an established primitive
backend, choose Base UI; preserve a working Radix or other backend rather than
migrating for this preference alone. Add a component only when no existing
owner or composition fits; remove or deprecate the duplicate it supersedes.

Compose the smallest semantic parts. Preserve native elements, refs, forwarded
props, event handlers, accessible names, focus, dismissal, and state ownership.
Do not nest interactive controls, duplicate state, or push feature-specific
layout into a shared primitive. Prefer one extended owner over a near-copy.

For exploratory work, keep variants in the authorised scratch artefact and
tune one meaningful axis at a time within a direction. Do not activate live work or production
source through a request for directions alone. Once a direction is approved,
implement the smallest coherent slice, exercise real content and applicable
states, then integrate rather than leaving parallel primitives.

Polish is iterative craft judgement against real content, not a one-off gate.
Repeat until a fresh look finds nothing in scope worth changing:

- **Hierarchy:** one clear first read per view; secondary content recedes.
- **Alignment:** shared edges and baselines, corrected optically by eye.
- **Density:** grouping holds with real, long, and extreme data.
- **Type:** roles, measure, numerals, wrapping, and truncation.
- **Colour:** consistent roles, verified contrast, accent spent on attention.
- **States:** every applicable state, not only the happy path.
- **Interaction feedback:** press, focus, pending, and completion respond at
  once and in proportion to the action.

Distillation removes non-serving complexity without deleting capability,
meaning, accessibility, or identity. Completion requires fresh UI review and the
enclosing lifecycle's tests; author confidence is not evidence.

Sources: Anthropic, [frontend-design skill](https://github.com/anthropics/skills/tree/main/skills/frontend-design);
Linear, [A calmer interface for a product in motion](https://linear.app/now/behind-the-latest-design-refresh).
