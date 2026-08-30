<!-- Modified from Impeccable for this harness; see the repository THIRD_PARTY_NOTICES.md. -->

# Design systems

Infer one focused job: **audit**, **consolidate**, **extend**, or **migrate**.
Audit is review and read-only. The other jobs require `implement` for source
change. When the project has no canonical owner, creating a global token
taxonomy, component API, theme model, or system location returns to `scope`.

Begin with current token definitions, theme owners, shared components,
documentation, and consumers. Classify drift as:

- a missing system capability;
- a consumer bypassing an existing owner; or
- a conceptual mismatch between owner and need.

Layer tokens deliberately: project primitives feed semantic roles, which feed
component aliases only when a component-specific contract is useful. Test
theme and contrast behaviour; do not create a flat variable dump. Components
need public behaviour, applicable states, accessibility,
composition, content stress, and version/migration expectations. Preserve
platform semantics rather than wrapping everything into a bespoke primitive.

For new shadcn/ui work with no established primitive backend, prefer the Base
UI variant. Preserve an existing Radix or other working backend when it is
already adopted or required for compatibility; do not migrate solely to satisfy
this preference. This is a conditional default for shadcn/ui projects, not a
universal stack requirement. Review third-party registry code before installing
it.

## Document or extract

When documenting an observed system, first locate its canonical artifact with
the documentation owner. Do not overwrite an existing document or sidecar.
Choose **scan** when tokens, components, or rendered consumers exist; use a
clearly marked **seed** only for a pre-implementation system. A compact document
may use YAML frontmatter for observed tokens/components followed by Overview,
Colors, Typography, Elevation, Components, and Do's and Don'ts. Verify any
external schema before claiming compatibility.

Keep project-native values normative. A machine sidecar may carry extensions
that the human document cannot express, but it must not become a competing
source of truth. Record observed, approved, and proposed values distinctly.
Extract incrementally only when repeated use with the same intent justifies a
shared token or component; name the consumer migration and no-overwrite
boundary before editing.

### Optional machine contract

Use this only when the project already owns this format or explicitly needs a
machine-readable `DESIGN.md`; ordinary design-system work may stay in existing
project-native files. Frontmatter may contain `name`,
`description`, `colors`, `typography`, `rounded`, `spacing`, and `components`.
Token references use `{path.to.token}`; component entries may reference
primitives and are limited to `backgroundColor`, `textColor`, `typography`,
`rounded`, `padding`, `size`, `height`, and `width`. The Markdown body uses
these exact H2s in order: Overview, Colors, Typography, Elevation, Components,
and Do's and Don'ts.

When existing project tooling owns `.impeccable/design.json`, put only
non-duplicating extensions there with `schemaVersion: 2`: `extensions` for
metadata the frontmatter cannot express, `components` for self-contained
renderable examples, and `narrative` derived from the six sections. When both
document and sidecar change, regenerate them together, resolve references,
parse both artefacts, and round-trip representative tokens and components
before handoff. Never create this sidecar merely to satisfy this reference.
Preserve a strict no-overwrite boundary and keep project-native values
normative.

Consolidation removes duplicate concepts after consumer evidence identifies a
canonical owner. Extension adds the smallest reusable capability and migrates
real consumers. Migration defines compatibility, sequence, fallback, and
parity across themes, states, viewports, and input modes. Completion requires
adoption and removal or explicit deprecation of bypasses; a new token or
component file alone is not a system change.

`engineering-docs` owns the durable location and lifecycle of design-system
documentation. Keep
this as a focused reference within UI/UX design, not a competing global skill.
