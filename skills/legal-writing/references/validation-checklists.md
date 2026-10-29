# Validation Checklists

## Reference Map

`legal-concision-and-anti-ai.md` loads for all prose (it holds the hard bans). Add only what the task
needs:

| Task/mode or content | Load |
|---|---|
| submissions / written advocacy | [argument-structure-and-paragraphing.md](argument-structure-and-paragraphing.md) |
| forms, affidavits, orders, annexures | [forum-and-document-recipes.md](forum-and-document-recipes.md) |
| statutory declarations, or choosing between a declaration, an affidavit and a witness statement | [statutory-declarations.md](statutory-declarations.md) |
| deeds, agreements, releases, guarantees, variations | [deeds-and-agreements.md](deeds-and-agreements.md) |
| tribunal work and merits review | [tribunals-and-merits-review.md](tribunals-and-merits-review.md) |
| correspondence | [legal-correspondence-style.md](legal-correspondence-style.md) |
| letters of demand, pre-action letters, settlement offers | [demands-and-pre-action-correspondence.md](demands-and-pre-action-correspondence.md) |
| AGLC citation in academic work, advices and research memoranda | [aglc4-citation.md](aglc4-citation.md) |
| non-filing legal decision overview | [decision-overviews.md](decision-overviews.md) |
| family-violence or redaction-sensitive material | [family-violence-and-redaction.md](family-violence-and-redaction.md) |
| house style questions | [house-style.md](house-style.md) |
| source anchors and citations | [source-boundary-and-citations.md](source-boundary-and-citations.md) |
| checking the finished draft | this file ([validation-checklists.md](validation-checklists.md)) |
| current-source and user-authority gates | [verification-and-escalation.md](verification-and-escalation.md) |

Run the checks the task needs, not all of them:

| Task/mode | Run |
|---|---|
| any filing-facing text | Lint Workflow first (it owns the mechanical bans), then Filing-Facing Style + Source |
| `draft` / `rewrite` submissions | Concision and naturalising + Argument Structure + Integrity |
| `condense` / `final-scrub` | Concision and naturalising + Integrity (condense gate) |
| affidavit work | Affidavit + Source + Safety |
| orders/minutes | Document Function + the Orders cut-list in `forum-and-document-recipes.md` |
| correspondence | Correspondence + Filing-Facing Style |
| letter of demand or pre-action letter | Correspondence + Source + the checks in `demands-and-pre-action-correspondence.md` |
| statutory declaration | Source + Safety + the regime and execution checks in `statutory-declarations.md` |
| deed or agreement | Document Function + the review pass in `deeds-and-agreements.md` |
| tribunal application, contentions or submissions | Concision and naturalising + Argument Structure + the completion gate in `tribunals-and-merits-review.md` |
| legal decision overview | Decision Overview + Source |

## Decision overview check

Apply [decision-overviews.md](decision-overviews.md): each issue keeps the shared
position, source support and remaining decision or professional confirmation
distinct, and open options retain their material legal consequences.

## Filing-facing style check

The lint owns the mechanical bans (em dashes, US legalese, internal markers,
process language, banned evidence paths, US dates), so run it rather than
eyeballing them. Then check what it cannot see:

- Australian spelling and day-month-year dates; en dashes only for ranges.
- No unresolved placeholders unless the file is deliberately a draft.
- Stable headings; paragraphs numbered where the forum expects; long paragraphs
  split.
- Single quotation marks, consistent; case names and legislation italicised;
  defined terms capitalised and consistent.
- Money and competing figures exact, not rounded.
- A party without a lawyer is a `self-represented litigant` or `litigant in
  person`.
- Every Markdown heading has a blank line before it (pandoc's
  `blank_before_header` rule); verify the rendered heading on changed pages.
- Every table column heading is true of every row beneath it.

## Concision and naturalising check

- The first paragraph states the relief, request, answer or next step.
- Each paragraph has one topic and one legal function. A longer revision is
  justified by source, safety, correspondence protection, enforceability or a
  necessary qualification.
- Procedural history is shortened, moved to a chronology or removed; concrete
  actors, dates, amounts and anchors replace vague summary.
- Throat-clearing, inflated adjectives and additive transitions are gone; noun
  stacks are unpacked without deleting defined terms.
- No legal term of art, anchor, qualification, redaction, protected-address limit
  or forum label was removed to improve flow.
- Affidavit text stays first-person evidence; submissions stay argument; orders
  stay commands; correspondence stays practical. Wrong-home material was moved,
  not polished. Any retained formal phrase has a legal, source, forum or safety
  reason.
- Stop when the next cut would remove a date, amount, party label, status,
  source cue, exhibit label, pinpoint, redaction qualifier, honest negative or
  forum wording.
- Final self-audit: name the single remaining defect, or stop.

## Argument structure check

For submissions; see `argument-structure-and-paragraphing.md`.

- Relief, grounds and reasons recoverable from the headings and first sentences.
- Argument headings are conclusions; paragraphs open with a contention and close
  on its conclusion.
- Each shared premise appears in full once; the grounds run are the strongest.
- One register for the stage, held throughout.
- Adverse authority is disclosed and met; the obvious weakness is confronted with
  one located concession; a reply answers every distinct contention.

## Integrity check

- Every cited authority is in the verified register or was opened and
  pinpoint-verified against an authorised primary source for this task.
- Holdings are at their true fact-specific altitude. No instrument, order term,
  finding, rule title or decision-maker label appears that the record does not
  contain.
- Any condense or relocation passed the token set-diff (figures, dates,
  pinpoints, labels, authorities, defined terms) and an independent qualitative
  pass.
- Nothing confidential, suppressed or privileged went to an external tool; the
  forum's AI-practice requirements were checked (see
  `source-boundary-and-citations.md`).

## Source check

Every factual assertion has a source that exists in the workspace; no fact is
inferred from a filename; no scratch, OCR, render or internal note is cited as
evidence; conflicting figures are preserved and attributed; procedural
statements were checked against official forms, fees, rules and filing methods.

## Document function check

Orders contain orders, not argument. Affidavits contain facts. Submissions
contain issues, law, argument and relief. Chronologies contain date, event,
source and relevance. Correspondence states the request and deadline. Proposed
orders were classified before cutting. Solicitor notes stay internal. Annexure
and exhibit indexes match their labels.

## Correspondence check

- Subject names the matter, action and deadline where useful; recipient, `Cc`,
  attachments and address handling are correct.
- The first paragraph states the request, action or response position.
- Protective labels, offer terms, non-admission or non-waiver wording, rights
  reservations, deadlines and service statements survive.
- Tone is courteous, firm, practical and annexure-safe, with no added
  accusation, concession, undertaking, offer or admission.

## Affidavit check

- Each person is introduced once by full name, short form and relationship, then
  referred to by the short form.
- The deponent swears experience, not self-diagnosis, absent medical evidence.
- No relief, argument, or characterisation of another forum's order as wrong;
  disputed liability is `disputed`, `not admitted`, `under appeal` or `unable to
  reconcile`.
- Direct personal-knowledge facts are preserved, not deleted for lack of an
  attachment. Producibility is stated once.
- The current verified form, length, layout and signing requirements are met and
  paragraph cross-references still resolve after rendering.

## Safety check

Protective, police-issued, interim and final safety material is named at its
exact source status; allegation, order and finding are not conflated. Sensitive
material is targeted and proportionate. Residential address and unsafe contact
details are excluded or handled under forum rules. Medical, reproductive-health,
third-party and broad message-history material is not filed unless necessary and
settled.

## Lint workflow

Pass explicit project paths; there are no implicit directories:

```bash
provenant check writing path/to/source path/to/correspondence --rules legal
```

To scan this skill's own references, select the product checkout; the skill's
JSON declares its quoted-example paths and exclusions:

```bash
provenant check writing skills/legal-writing --rules legal
```

Failures block filing-facing documents. Warnings are review prompts.
