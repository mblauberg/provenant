---
name: academic-writing
description: "Use for drafting, rewriting, condensing, or checking academic prose and LaTeX while preserving claims and citations. Not for general, engineering, or Australian legal prose; use the matching writing skill."
---

# Academic writing

Write precise, concrete, restrained and defensible academic prose. This skill
owns academic voice cleanup; do not also invoke a general writing skill unless
the user asks. It does not cover source discovery, bibliography management,
invented citations, integrity adjudication or assessment-permission advice.
AI-use disclosure wording and placement are in scope; permission and policy are
not.

The target venue, institution, supervisor-approved style, project glossary and
local LaTeX/citation conventions take precedence over these defaults. Diagnosis
is read-only. Edit only assigned files and do not restructure a work, change a
bibliography or replace citations without explicit authority.

## Workflow

1. Identify the section and mode: `draft`, `rewrite`, `condense`, `diagnose`,
   `section-polish`, `match-voice`, `citation-safe` or `final-scrub`.
2. Lock claims, numbers, units, citations, quotations, LaTeX commands, equations,
   labels, cross-references, result macros, paths and technical names.
3. This skill is a specialization of the `natural-writing` hub: it owns
   thesis register, LaTeX/citation-key preservation and the empirical-research
   overlays, and links to the hub for the shared doctrine. Load the
   `natural-writing` skill for its Australian English default and, for prose,
   [academic register](references/academic-style-au.md) plus
   the matching [workflow mode](references/editing-workflows.md); see that
   file's reference map for which remaining hub/overlay pair (anti-AI, condense,
   claim discipline, chapter, engineering voice or citation/LaTeX safety) a
   mode needs. A project adapter or keep-list takes precedence.
4. Preserve meaning and evidence altitude. Separate observation, interpretation,
   limitation and future work. Flag unsupported claims; never strengthen them.
5. Follow the target language style (otherwise Australian English). Use concrete
   verbs and purposeful sentence variety. Cut noun stacks, inflation, empty
   meta-discourse and instruction-leaking absence tombstones; apply punctuation
   defaults only where the project is silent. Keep honest negatives and defined
   technical terms. Do not edit to beat an AI detector.
6. For a full chapter, paper or document rewrite or final polish, use the
   risk-proportional independent review in `editing-workflows.md`. Load
   `orchestrate` for runtime routing; do not select providers or model IDs here.
   Preserve labels, macros and citation keys, then validate the whole work.
7. Run one adversarial final scrub: name and repair only a real remaining tell.

For condensation, follow the reference stop rule, report words before/after and
percentage cut, and verify every locked invariant. If another cut would lose a
fact, caveat, scope condition, term or reference, stop.

For local files, use the checker as a review prompt, not proof:

```sh
python3 "$(provenant root)/skills/academic-writing/scripts/check_academic_style.py" path/to/file.tex
```

Return rewritten prose first and concise risk flags second. For diagnosis,
return highest-risk findings first. Leave unsafe sentences unchanged and state
why. Match surrounding project conventions when editing files.
