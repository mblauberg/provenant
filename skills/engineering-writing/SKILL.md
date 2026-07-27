---
name: engineering-writing
description: "Use for drafting or condensing software-engineering prose such as requirements/specs, READMEs, commits, PRs, runbooks, errors, and design notes. Not for general, academic, or Australian legal prose."
---

# Engineering writing

Write clear, brief, accurate and useful engineering prose in Australian English.
Cut first. Add only what serves the reader's job and belongs in this document.

This skill is a specialization of the `natural-writing` hub: it owns
codebase/README/commit/PR artefact types and links to the hub for the
Australian English default, the anti-AI taxonomy and the condense pass. See
`references/process.md`, `references/style-standard.md` and
`references/engineer-voice.md` for where each hub link applies.

## Workflow

1. Name the audience, prior knowledge, required action or decision, and format
   constraint. If the reader's job is unclear, flag it before drafting.
2. Choose one relevant pattern: [document patterns](references/document-patterns.md)
   for codebase/short-form prose, [requirements and planning](references/requirements-and-planning.md)
   for already-decided pre-build content, or [architecture and communication](references/architecture-and-presentations.md)
   for approved designs, ADRs, presentations and briefs. Do not load all three.
3. Structure before sentences: front-load the point, use decision-oriented
   headings and one idea or requirement per unit, then move wrong-home material.
4. Lock facts, logic, stance, evidence, obligations, behaviour, identifiers,
   paths, commands, flags, parameters, keys, error codes, numbers, units, dates,
   citations and quoted terms.
5. Revise in separate structure, accuracy, clarity/concision, voice and
   Australian-English passes. Load only the needed section from
   [process](references/process.md), [style standard](references/style-standard.md),
   [engineer voice](references/engineer-voice.md) or the `natural-writing`
   skill's Australian English default;
   use [sentence mechanics](references/strunk-mechanics.md) or
   [sources](references/sources.md) only for a specific problem. Load the hub
   anti-AI taxonomy and condense pass from the `natural-writing` skill
   alongside `engineer-voice.md` and `process.md` respectively.
6. Optional, reader-facing prose only and within disclosure authority: consider
   a final voice pass by the model under human-facing final polish in the
   advisory model dossier. Never required; the chair owns substance, so diff
   the return for changed facts.

Preserve technical meaning and evidence altitude. Distinguish observation from
interpretation; attach numbers to comparatives. Verify every package, API,
version, flag and URL against code or documentation, otherwise mark
`[FLAG: verify]`. Never invent facts to fill a document. Keep genuine caveats
and honest negatives while cutting padding, marketing language, AI tells,
instruction-leaking absence tombstones and internal process language from
reader-facing prose. Match existing repository conventions without copying
signature phrases.

For condensation, follow the reference stop and integrity checks and report the
before/after word delta. Review mode returns severity, evidence, impact and fix;
rewrite mode returns text first; final scrub makes defect fixes only.

For local files, use the checker as a review prompt, not proof:

```sh
python3 "${AGENTS_HOME:-$HOME/.agents}/skills/engineering-writing/scripts/check_engineering_style.py" path/to/file.md
```

Flag unverifiable support, audience dependence, domain-required terms, or a cut
that would drop an obligation. Use `[FLAG: verify source]`,
`[FLAG: define audience]`, `[FLAG: preserve exact term]` or
`[FLAG: shortening drops obligation]` where applicable. Do not smooth
uncertainty away.
