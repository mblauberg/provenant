<!-- Modified for Provenant. -->

# Visual QA

Define a small state-by-viewport matrix from the acceptance criteria. Include
the critical path, relevant interaction/failure states, supported themes and
input modes, plus a component stress case for long, empty, dense, translated,
or unusual content where risk warrants it. Include an unbroken URL, UUID/hash,
long token, nested flex/grid child with `min-width: 0`, and line clamp or
multiline limit where those shapes can occur.

Capture a motion-bearing change per the motion evidence protocol in
[motion](motion.md); a frozen or finished-animation still is valid only for
states that carry no motion. Capture a still only after the viewport is
settled: fonts resolved, loading accounted for, intended content present, and
layout stable. Compare against approved references under like
conditions. A screenshot does not prove interaction, accessibility, source
correctness, or responsive behaviour beyond the captured state, and a
screenshot alone never certifies motion quality.

Use rendered inspection, screenshots, accessibility tree, keyboard path,
console, and network evidence as distinct lanes. Check hierarchy, alignment,
clipping, layering, focus, overflow, state continuity, assets, errors, and
unexpected requests. Use the existing `playwright` skill with the app-owned
preview for browser evidence. Check availability/version and never download
the tool implicitly. View each capture and verify the shipped artefact; mark
unavailable browser evidence `not tested`.

Translate subjective goals into binary conditions tied to the approved intent.
Record each matrix cell as tested, failed, not tested, or not applicable and
preserve evidence paths. Browser automation results do not establish WCAG
certification, real assistive-technology compatibility, cross-browser
completeness, or field performance. Final acceptance belongs to the enclosing
lifecycle through objective checks and tier-required review. Explicit user
acceptance is required only for `terminal` work or when approved scope asks
for it; a project may name its landing gate as acceptance.
