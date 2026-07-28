---
name: natural-writing
description: "Use for making general, mixed, or correspondence prose natural and direct while preserving facts, citations, numbers, and intent. Not for engineering, academic, or Australian legal prose; use its specialist."
---

# Natural writing

Rewrite general prose in a natural, direct voice. Preserve meaning, facts,
citations, numbers and intent while removing filler, canned framing, synthetic
rhythm and chatbot residue.

## Hub role

This skill is the writing family's hub: it owns the shared prose doctrine,
the anti-AI taxonomy, condense pass, claim-discipline/evidence schema, voice
matching, and the always-on Australian English default. `academic-writing`,
`engineering-writing` and `legal-writing` link here rather than clone it,
keeping only their own domain specialization.
Load [au-english.md](references/au-english.md) as the default house style for
any prose without a project convention that overrides it.

## Boundary

Use this skill for general, mixed or unclassified prose, including
correspondence. Defer technical docs and READMEs to `engineering-writing`,
scholarly work to `academic-writing`, and Australian legal drafting to
`legal-writing`. Correspondence about a specialist matter stays here, with that
specialist as companion.

Do not fabricate experience, invent sources, add fake messiness or optimise
for AI-detector scores. Formal prose is not proof of AI authorship.

## Modes

- `rewrite` (default): clean the full draft.
- `light-touch`: preserve most wording and repair only clear defects.
- `match-voice`: follow a supplied writing sample.
- `precision-preserving`: protect high-stakes or citation-heavy wording.
- `full-rewrite`: reshape low-stakes prose when a normal pass is insufficient.
- `diagnose`: identify the strongest problems before rewriting.

Choose the least invasive mode that solves the problem. Combine any mode with
`match-voice` when the user supplies a sample.

## Workflow

1. Lock facts, logic, stance, citations, numbers, names, quotes and required
   terms.
2. Diagnose the real defect: weak information, assistant residue, generic or
   inflated language, repeated templates, unclear sentences or a voice mismatch.
3. Rewrite structure, emphasis and sentence rhythm rather than swapping
   synonyms. Use specifics already present in the source; never invent texture.
4. Match confidence to evidence. Narrow or flag an unsupported claim instead
   of strengthening it.
5. Repair remaining quality and voice mismatches, then compare the result
   against the locked invariants and any writing sample.

Load [patterns.md](references/patterns.md) for the sign catalogue, research,
voice-matching rubric, genre guidance or a stubborn second pass. Load
[anti-ai-taxonomy.md](references/anti-ai-taxonomy.md) for the tiered sweep,
[condense-pass.md](references/condense-pass.md) for shortening without losing
meaning, and [claim-discipline.md](references/claim-discipline.md) for wording
observed, inferred, limitation and pending claims.

## Bright-line rules

- Preserve facts, meaning, citations, named entities and quoted terms.
- Delete chatbot framing and tool residue instead of paraphrasing them.
- Prefer concrete nouns and verbs; state each supported point once.
- Repair repeated templates without manufacturing quirks or forced variation.
- Keep one honest qualifier for weak evidence; do not hedge strong evidence.
- If the draft already reads naturally, edit lightly and say so.

## Output

For a rewrite, return the final text first and add notes only for material
flags or choices. For a diagnosis, lead with the strongest quality and voice
risks. Keep inline flags short, such as `[FLAG: cite source]`.
