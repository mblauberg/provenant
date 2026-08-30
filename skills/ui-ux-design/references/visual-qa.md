
# Visual QA

Define a small state-by-viewport matrix from the acceptance criteria. Include
the critical path, relevant interaction/failure states, supported themes and
input modes, plus a component stress case for long, empty, dense, translated,
or unusual content where risk warrants it. Include an unbroken URL, UUID/hash,
long token, nested flex/grid child with `min-width: 0`, and line clamp or
multiline limit where those shapes can occur.

Capture only after the viewport is settled: fonts resolved, loading accounted
for, intended content present, layout stable, and animation paused or allowed
to finish consistently. Compare against approved references under like
conditions. A screenshot does not prove interaction, accessibility, source
correctness, or responsive behaviour beyond the captured state.

Use rendered inspection, screenshots, accessibility tree, keyboard path,
console, and network evidence as distinct lanes. Check hierarchy, alignment,
clipping, layering, focus, overflow, state continuity, assets, errors, and
unexpected requests. For a quick anti-pattern and contrast lead, run
`node "${AGENTS_HOME:-$HOME/.agents}/skills/ui-ux-design/scripts/detect.mjs" --help`;
its output or a build stamp is
not evidence that the page rendered as claimed, so verify the shipped artefact.

Translate subjective goals into binary conditions tied to the approved intent.
Record each matrix cell as tested, failed, not tested, or not applicable and
preserve evidence paths. Browser automation results do not establish WCAG
certification, real assistive-technology compatibility, cross-browser
completeness, or field performance. Final acceptance belongs to the enclosing
lifecycle and user, not this design method.
