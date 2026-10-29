---
name: legal-writing
description: "Use for Australian legal drafting: affidavits, submissions, orders, statutory declarations, deeds, demand and pre-action letters, decision overviews, AGLC4 citation, and condensing any of them. Not for general or academic prose about law (natural-writing) or engineering docs (engineering-writing). Drafting assistance, not legal advice."
---

# Australian legal writing

Stricter project and forum instructions prevail. Verify current law, forms and
procedure against official sources, and keep qualified human review for
filing-facing work. Sending, signing, filing and serving stay with the user.

The `natural-writing` hub owns Australian English, the mechanism catalogue and
the condense pass. This skill owns jurisdiction, forum, instrument and
filing-facing rules. Its Legal Function Test classifies sentences by legal
function and does not map to the hub's observed/inferred claim classes.

## Scope

Use it when the output is an Australian legal instrument, filing-facing text or
legal correspondence: court and tribunal forms, affidavits, statutory
declarations, witness statements, submissions, proposed orders, chronologies,
annexure indexes, deeds and agreements, letters of demand, registry and party
correspondence, non-filing decision overviews and AGLC4 citation.

Legal subject matter alone is not the trigger. An article, chapter or
plain-language explanation about law goes to `natural-writing`; a spec, README or
runbook goes to `engineering-writing`. This skill joins either as a companion
where legal status, authority, forum wording or source altitude must survive.

## Workflow

1. Read project instructions, live matter state and source-boundary rules. Load
   the matching jurisdiction, forum or document skill where one exists.
2. Choose the mode: draft, rewrite, condense, diagnose, correspondence,
   decision-overview or final-scrub. Load
   [legal concision](references/legal-concision-and-anti-ai.md) for all prose.
   [validation-checklists.md](references/validation-checklists.md) maps the other
   references to mode and document type.
3. Classify each paragraph by legal function before polishing. Move wrong-home
   material first.
4. Lock legal status, source anchors, exact quotations, authorities, labels,
   offer terms, non-admission and non-waiver wording, deadlines, attachments,
   redactions and user-authority conditions.
5. Draft the minimum complete text: relief, request or answer first, one
   proposition per paragraph, exact pinpoints, no internal agent language.
6. Run the lint, then the checks the task needs.

```sh
provenant check writing path/to/source --rules legal
```

The lint is a guardrail. Source checks, forum skills, render checks and user
gates still apply. Edits to filing-facing, source-boundary or lint rules need
independent review.

## Bounds

- Every filing-facing fact needs a verified real source anchor. Never invent or
  silently alter an authority, instrument, finding, rule title, forum term or
  decision-maker label. Verify canonical sources, not indexes, OCR, renders or
  agent notes.
- Never humanise by weakening a threshold, deleting an anchor, casualising court
  language or changing forum wording.
- Preserve contentions as contentions, in the register the stage requires.
  Protective-order, police-issued or safety material stays a dated source with
  the legal effect its verified text supports, not a finding.
- Current-law research runs `scout-swarm` with `external>=1`, and its claims
  carry the date retrieved.
- Condensing runs the token set-diff plus an independent qualitative pass. Final
  scrub adds no argument, fact, authority, history or courtesy closer.
