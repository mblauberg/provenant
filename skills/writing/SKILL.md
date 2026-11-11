---
name: writing
description: "Use to write or tighten prose, emails, theses, docs, commits, PR bodies and release notes. Not for legal drafting (legal-writing)."
---

# Writing

The outcome is text a reader trusts without re-checking: every fact survives
the edit, nothing is invented, and it reads as written by a person who knew the
subject. Australian English, plain and mildly casual by default
([voice.md](references/voice.md)).

## Rules

- Facts don't move. Style passes are where they drift, so lock them first and
  audit against the lock.
- Never fabricate experience, sources, detail or fake messiness. Mark each gap
  `[FLAG: cite source]` or `[FLAG: verify]`.
- No em dash (U+2014) in output, examples included. Recast the sentence; a
  colon or spaced hyphen that keeps the same pivot moves the tell.
- A supplied voice sample beats these defaults.
- Edit for the reader, never a detector score. Formal prose isn't proof of AI
  authorship.
- One term per concept: repeat the precise noun, don't cycle `endpoint`,
  `route` and `URL` for variety.
- Never style-pass gates, ADR decisions, credential custody or literal
  runbooks. Edit them by hand, and check whether a test matches their exact
  wording first.

## Procedure

Pick the lightest pass: repair named defects, rewrite (default) or diagnose
(findings only). If the draft already reads well, say so and touch little.

1. **Lock** facts, numbers, names, quotes, citation keys, defined terms,
   modals and negations. List them.
2. **Diagnose** by mechanism ([mechanisms.md](references/mechanisms.md)), not
   by phrase. Name the two or three that cost the reader most.
3. **Rewrite once**, restating each claim directly and adding only anchors the
   source holds.
4. **Audit independently.** Compare the lock list with the result; revert any
   sentence that lost or strengthened a claim. For high-stakes text, a
   clean-context reviewer sees only the before and after. For number-heavy
   text, script the check ([claims.md](references/claims.md)).
5. **Report** the text first, with notes only for material flags.

`provenant check writing PATH` flags em dashes and lexical hints. Its silence
is necessary, not sufficient.

## References

- [voice.md](references/voice.md): register, punctuation and Australian
  English. Load before any write or rewrite.
- [mechanisms.md](references/mechanisms.md): the defects, with before/after
  pairs.
- [claims.md](references/claims.md): claim classes, comparatives, number
  audit and the condense pass.
- [engineering.md](references/engineering.md): READMEs, PRs, commits, error
  text, runbooks, release notes and process-language leaks. Specs and issue
  bodies go to `scope` and `tracker`; where a doc lives is `engineering-docs`.
- [academic.md](references/academic.md): thesis and paper register, citations,
  LaTeX invariants.
