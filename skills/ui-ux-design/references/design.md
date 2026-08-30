<!-- Modified for Provenant. -->

# Design

Start from the approved outcome and the project's current visual language.
Infer missing low-consequence details from local evidence. Ask only when an
answer would materially change the result and cannot be discovered safely.

For materially ambiguous net-new work, use one low-ceremony optional brief:
goal and users, constraints and protected contracts, surface and scope,
applicable states, interaction/recovery, content, visual direction, and success
evidence. Batch genuinely material unknowns into one question set; otherwise
proceed on the explicit request and record reasonable assumptions. Ask for
confirmation only when an unresolved choice is consequential, not as a routine
approval stop.

Classify the work:

- **Preserve** fixes or completes the current language and is the default for
  existing products.
- **Extend** adds a capability using current tokens, components, and patterns.
- **Overhaul** intentionally changes the language and needs explicit approval
  of that consequence.

Name protected contracts before editing: navigation, routes, component APIs,
brand assets, form and analytics semantics, legal copy, accessibility
behaviour, and public interfaces. Build component-first and reuse canonical
owners. Choose the cheapest effective intervention in this order when
appropriate: remove unnecessary treatment, use the platform, reuse, correct,
then add.

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

For exploratory work, vary one meaningful axis at a time and keep variants in
the authorised scratch artefact. Do not activate live work or production
source through a request for directions alone. Once a direction is approved,
implement the smallest coherent slice, exercise real content and applicable
states, then integrate rather than leaving parallel primitives.

Polish is a bounded pass over hierarchy, alignment, density, type, colour,
states, and interaction feedback. Distillation removes non-serving complexity
without deleting capability, meaning, accessibility, or identity. Completion
requires fresh UI review and the enclosing lifecycle's tests; author confidence
is not evidence.
<!-- Modified for Provenant. -->
