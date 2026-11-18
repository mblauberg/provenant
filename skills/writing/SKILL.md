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

Preserve facts, numbers, names, quotations and the strength of each claim.
Diagnose by [mechanism](references/mechanisms.md), rewrite with source support
and check the result against the original. Scale independent review and numerical
checks ([claims.md](references/claims.md)) to risk. Report material flags.

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
