---
name: natural-writing
description: "Use to write, rewrite, tighten or diagnose emails and general, mixed, correspondence or academic/thesis prose without losing facts, citations or numbers; owns Australian English and the AI-prose mechanisms. Not for READMEs, commits or docs (engineering-writing) or Australian legal drafting (legal-writing)."
---

# Natural writing

The family's writing hub, owning prose doctrine: fidelity locking, the
mechanism catalogue, Australian English, claim discipline and academic prose.
`engineering-writing` and `legal-writing` add a domain overlay and link here.

## Hard rules

- Facts don't move. Style passes are where they drift, so lock them (step 1)
  and audit against the lock (step 4).
- Don't fabricate experience, sources or detail, or add fake messiness.
  Flag each gap (`[FLAG: cite source]`).
- Edit for the reader, never a detector score. Formal prose isn't proof of
  AI authorship.
- Use the punctuation the sentence needs, without em-dashes. Run `provenant
  check writing` when available: it flags em-dashes and lexical hints, its
  silence is necessary but not sufficient, and its word rules are advisory.
- Never style-pass gates, ADR decisions or process runbooks.
- A supplied voice sample beats these defaults.

## Target

A direct claim, a plain verb, an anchor from the source (number, name, path,
date, honest negative), one calibrated qualifier, and an ending on the
decision, risk or next action. The default voice is plain and mildly casual
([voice.md](references/voice.md)); a formal register is the exception.

## Procedure

Pick the least invasive pass: light-touch (repair named defects), rewrite
(default) or diagnose (findings only, no edit). If the draft already reads
well, edit lightly and say so.

1. **Lock** facts, numbers, names, quotes, citation keys, defined terms,
   modals and negations. List them first; they're the fidelity contract.
2. **Diagnose** by mechanism, not phrase. Read
   [mechanisms.md](references/mechanisms.md) and name the two or three that
   cost the reader most. A swapped synonym leaves the mechanism, so the tell
   moves to the next word.
3. **Rewrite once**, by mechanism, in [voice.md](references/voice.md)'s
   register. Restate each claim directly and add only anchors the source
   holds.
4. **Audit independently.** Compare the locked list against the result and
   revert any sentence that lost or strengthened a claim.
   - High-stakes prose: a separate clean-context reviewer sees only the
     before and after.
   - Number-heavy text: script the comparison
     ([claim-discipline.md](references/claim-discipline.md#mechanised-audit)).
   - Long documents: split reviewers by lens (redundancy, register) and
     triage their findings yourself.
5. **Report** the final text first, with notes only for material flags.

## References

- [voice.md](references/voice.md): load before any write or rewrite. The
  default register, rules, example and formal exceptions.
- [mechanisms.md](references/mechanisms.md): the defects, each with a reason
  and a before/after pair.
- [tells-2026.md](references/tells-2026.md): read before judging a draft's
  age. What current models still do and what has faded, dated and sourced.
- [au-english.md](references/au-english.md): house style unless a
  repository, quote, API or form requires otherwise.
- [condense-pass.md](references/condense-pass.md): shortening, with a stop
  rule.
- [claim-discipline.md](references/claim-discipline.md): observed, inferred,
  planned and pending claims; comparatives that carry their number.
- [academic-prose.md](references/academic-prose.md): thesis and paper
  register, citation-key preservation, evidence altitude. LaTeX invariants
  live in `engineering-writing`.
