<!-- Modified for Provenant. -->

# Review

Review is a negative capability: inspect and report without changing the
reviewed project. Do not use mutation-capable file operations, mutating shell,
package installs, browser writes, server injection, or project-local build and
cache output. Read-only browser navigation, GET requests, and screenshots are
allowed; submissions and external writes are not. If a rendered check requires
a build or capture, use an assigned output/cache path outside the protected
tree. An assigned report is the only permitted write and must also resolve
outside the protected tree.

Here, the protected tree is the reviewed project tree.

## Establish coverage

State the surface, representative viewport and input modes, applicable states,
and supplied references. Prefer the actual rendered surface; source and static
detector output are supporting evidence, not render proof. A quick review may
sample the critical path. A full review expands the state and viewport matrix,
but neither implies exhaustive coverage.

For deterministic leads, run
`node "${AGENTS_HOME:-$HOME/.agents}/skills/ui-ux-design/scripts/detect.mjs" --help`,
then scan the relevant file, directory, or URL. Treat every structured
`incomplete` target or engine failure as missing coverage, not a clean result.
A clean scan remains supporting evidence and never certifies the rendered UI.

For each selected lane, report one status: `tested`, `failed`, `not tested`, or
`not applicable`. Mark each claim `verified` when tied to an observed artefact
or reproducible check, otherwise `judgement`. Name the observed artefact: page,
state, screenshot, accessibility tree, source location, console event, or
network response.

If a brand, product, system, state, viewport, or assistive-technology owner was
not available, say `not reviewed`; do not replace it with generic taste. Use
`not verified` when an applicable claim lacks enough evidence. These are
different from a test that ran and failed.

## Findings

Route source-diff causality and the canonical severity/finding schema to
`code-review`. UI review adds the affected surface, state, viewport/input mode,
observed impact, evidence label, and the smallest useful remedy. Detector
findings remain leads until confirmed. Rank issues by user harm and reach, not
personal taste.

For a compact critique, cover the applicable dimensions among visual hierarchy,
layout, typography, colour, interaction, responsive behaviour, and
accessibility. For each, record the observation, divergence from an approved
owner or user goal, smallest remedy, and `pass`, `minor`, or `major` impact.
Those impact words describe UI judgement; source-diff severity remains with the
code-review owner.

Do not claim WCAG certification, cross-browser completeness, field
performance, or assistive-technology compatibility from screenshots, static
analysis, Lighthouse, or one browser run. A clean verdict means no supported
finding within stated coverage, not universal correctness.
<!-- Modified for Provenant. -->
