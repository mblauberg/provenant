---
name: natural-writing
description: "Use to rewrite, tighten or diagnose general, mixed, correspondence or academic/thesis prose without losing facts, citations or numbers; owns Australian English and the AI-prose mechanisms. Not for READMEs, commits or docs (engineering-writing) or Australian legal drafting (legal-writing)."
---

# Natural writing

The writing hub. It owns prose doctrine for the family: fidelity locking, the
mechanism catalogue, Australian English, claim discipline and academic prose.
`engineering-writing` and `legal-writing` add a domain overlay and link here.

## Target

A direct claim, a plain verb, an anchor from the source (number, name, path,
date, honest negative), one calibrated qualifier, and an ending on the
decision, risk or next action. Formal prose is not proof of AI authorship, so
edit for reader value, never for a detector score, and never add fake
messiness or invented detail.

## Workflow

1. **Lock** facts, numbers, names, quotes, citation keys, defined terms,
   modals and negations. List them first; they are the fidelity contract.
2. **Diagnose** by mechanism, not by phrase. Read
   [mechanisms.md](references/mechanisms.md) and name the two or three that
   cost the reader most. A swapped synonym leaves the mechanism in place and
   the tell moves to the next word.
3. **Rewrite once**, by mechanism: restate the claim directly, add only
   anchors the source holds, and flag a gap (`[FLAG: cite source]`) rather
   than filling it. Match a supplied voice sample over these defaults.
4. **Audit independently.** A style pass is where facts drift. Compare the
   locked list against the result and revert any sentence that lost or
   strengthened a claim. For high-stakes prose, run the audit as a separate
   clean-context reviewer given the before and after texts only.
5. **Report** the final text first, with notes only for material flags.

If the draft already reads well, edit lightly and say so. Choose the least
invasive pass: light-touch (repair named defects), rewrite (default) or
diagnose (findings only, no edit).

## Load when needed

- [mechanisms.md](references/mechanisms.md): the defects, each with a reason
  and a before/after pair.
- [tells-2026.md](references/tells-2026.md): what current models still do and
  what has faded, dated and sourced. Read before judging a draft's age.
- [au-english.md](references/au-english.md): the default house style, unless a
  repository, quote, API or form requires otherwise.
- [condense-pass.md](references/condense-pass.md): shortening with a stop rule.
- [claim-discipline.md](references/claim-discipline.md): observed, inferred,
  planned and pending claims; comparatives that carry their number.
- [academic-prose.md](references/academic-prose.md): thesis and paper
  register, citation-key preservation, evidence altitude. LaTeX invariants
  live in `engineering-writing`.

## Bounds

- Never style-pass gates, ADR decisions or process runbooks.
- Use the punctuation the sentence needs. `provenant check writing` flags
  em-dashes and lexical hints; its silence is necessary, not sufficient, and
  its word rules are advisory prompts for a human read.
- Do not fabricate experience or sources. Flag a missing source.
