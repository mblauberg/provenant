---
name: natural-writing
description: "Use for making general, mixed, academic, or correspondence prose natural and direct while preserving facts, citations, numbers, and intent. Covers thesis and paper prose. Not for engineering or Australian legal prose; use its specialist."
---

# Natural writing

Rewrite prose in a natural, direct voice. Preserve meaning, facts, citations,
numbers and intent while removing filler, canned framing and chatbot residue.

## Hub role

This skill is the writing family's hub and owns academic prose directly. It
holds the shared prose doctrine, anti-AI taxonomy, condense pass,
claim-discipline schema, voice matching and the Australian English default.
`engineering-writing` and `legal-writing` link here rather than clone it. Load
[au-english.md](references/au-english.md) as the default house style unless a
project convention overrides it.

## Boundary

Use this skill for general, mixed or unclassified prose including
correspondence, and for academic prose: thesis chapters, papers, abstracts
and literature reviews. Defer technical docs and READMEs to
`engineering-writing`, and Australian legal drafting to `legal-writing`.
Correspondence on a specialist matter stays here, with that specialist as
companion.

Do not fabricate experience, invent sources, add fake messiness or optimise
for AI-detector scores. Formal prose is not proof of AI authorship.

## Modes

- `rewrite` (default): clean the full draft.
- `light-touch`: preserve wording; repair only clear defects.
- `match-voice`: follow a supplied writing sample.
- `precision-preserving`: protect high-stakes or citation-heavy wording.
- `full-rewrite`: reshape low-stakes prose when a normal pass fails.
- `diagnose`: identify the strongest problems before rewriting.
- `citation-safe`, `section-polish`, `final-scrub`: academic variants.

Choose the least invasive mode; combine any with `match-voice` given a sample.

## Workflow

1. Lock facts, logic, stance, citations, numbers, names, quotes and terms.
2. Diagnose the real defect: weak information, assistant residue, inflated
   language, repeated templates, unclear sentences or voice mismatch.
3. Rewrite structure, emphasis and rhythm rather than swapping synonyms. Use
   specifics already in the source; never invent texture.
4. Match confidence to evidence. Narrow or flag an unsupported claim, never
   strengthen it.
5. Repair remaining quality and voice mismatches, then check the result
   against the locked invariants and any sample.

Load [patterns.md](references/patterns.md) for the sign catalogue,
voice-matching rubric or genre guidance,
[anti-ai-taxonomy.md](references/anti-ai-taxonomy.md) for the tiered sweep,
[condense-pass.md](references/condense-pass.md) for shortening, and
[claim-discipline.md](references/claim-discipline.md) for observed, inferred,
limitation and pending claims. For academic prose load
[academic-prose.md](references/academic-prose.md): register, citation-key
preservation, evidence altitude and thesis AI tells. LaTeX invariants and
chapter structure live in `engineering-writing`.

## Bright-line rules

- Preserve facts, meaning, citations, citation keys, named entities and
  quoted terms; flag a missing source rather than inventing a key.
- Delete chatbot framing and tool residue instead of paraphrasing them.
- Prefer concrete nouns and verbs; state each supported point once.
- Repair repeated templates without manufacturing quirks.
- Keep one honest qualifier for weak evidence; do not hedge strong evidence.
- If the draft already reads naturally, edit lightly and say so.

## Output

Return the final text first, with notes only for material flags or choices.
For a diagnosis, lead with the strongest risks. Keep inline flags short, such
as `[FLAG: cite source]`.
