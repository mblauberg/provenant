---
name: legal-writing
description: "Use for drafting, reviewing, condensing, or source-checking Australian legal documents (forms, affidavits, submissions and orders), legal decision overviews, or correspondence. Not for academic, engineering, or general prose; use its specialist."
---

# Australian legal writing

General Australian legal style, document-shape and source-discipline layer.
Stricter project and forum instructions override it. This is drafting
assistance, not legal advice: verify current law, forms and procedure against
official sources and retain qualified or human review for filing-facing work.

This skill is a specialization of the `natural-writing` hub: it owns
jurisdiction, forum and filing-facing rules, and links to the hub for the
Australian English default, the anti-AI taxonomy and the condense pass (see
`references/australian-english-house-style.md`,
`references/legal-concision-and-anti-ai.md` and
`references/forbidden-patterns.md` for where each hub link applies). The
Legal Function Test remains this skill's own claim-classification scheme; it
does not map to the hub's observed/inferred evidence schema.

## Workflow

1. Read project instructions, live matter state and source-boundary rules. Load
   the matching jurisdiction, forum and document skill where one exists.
2. Choose `draft`, `rewrite`, `condense`, `diagnose`, `correspondence`,
   `decision-overview` or `final-scrub`. Load [legal concision](references/legal-concision-and-anti-ai.md)
   for all prose; the reference map in
   [validation-checklists.md](references/validation-checklists.md) gives the
   remaining reference each mode or content type needs. For `decision-overview`,
   load [decision overviews](references/decision-overviews.md) directly.
3. Classify each sentence by legal function before polishing. Affidavits and
   witness statements give evidence; submissions argue; orders command;
   chronologies organise; internal notes analyse; correspondence communicates.
   Move wrong-home material first.
4. Lock legal status, source anchors, exact quotations, authorities, labels,
   offer terms, non-admission/non-waiver wording, deadlines, attachments,
   redactions and user-authority conditions.
5. Draft the minimum complete text in Australian English. Front-load the relief,
   request, answer or next step; use one proposition per paragraph, one home per
   point and exact pinpoints. Cut padding, duplicate history, intensifiers and
   internal agent language.

Every filing-facing fact needs a verified real source anchor. Never humanise by
weakening a threshold, deleting an anchor, casualising court language or
changing forum wording.

Never invent or silently alter an authority, instrument, finding, rule title,
forum term or decision-maker label. Verify canonical sources, not indexes,
summaries, OCR, renders or agent notes. Preserve contentions as contentions and
hold the register required by the procedural stage. Protective-order,
police-issued or other safety material remains a dated source with the legal
effect its verified text supports; do not convert it into a finding. Keep
source files, renders, OCR/transcripts, field maps and QA reports separate.

For substantial condensation or relocation, run the deterministic token
set-diff and an independent qualitative pass. Stop before losing an anchor,
qualification, disputed status, redaction, label, amount, date, forum wording
or authority condition. Final scrub adds no new argument, fact or authority.
It adds no history or courtesy closer unless legally necessary.

```sh
python3 "$(provenant root)/skills/legal-writing/scripts/lint_legal_style.py" path/to/source
```

Lint is a guardrail; source checks, forum skills, render checks and user gates
still apply. Changes to filing-facing, source-boundary or lint rules require the
owning harness's independent review gate.
