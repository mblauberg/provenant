# Core: function, locks, house style, bans

## Legal Function Test

Name each paragraph's one function: relief or order sought; procedural step;
source-backed fact; legal rule; contention; consequence or prejudice; safety or
redaction limit; practical request or deadline; correspondence protection (offer
term, non-admission, non-waiver, rights reservation); enforceable order detail
(actor, act, object, deadline or trigger). No function: cut. Two unrelated
functions: split. Another document owns it: move it, do not polish it in place.

Wrong homes: affidavit argument goes to submissions; submission-level factual
detail to an affidavit, chronology or exhibit note; reasons for an order to
submissions; relief buried in evidence to proposed orders; verification,
manifest, render or audit language stays internal and is translated into the
source or legal act.

Plain language is not casual language. Keep terms of art, defined terms, forum
labels, redaction limits and honest negatives (non-service, non-admission, no
order, no finding, under appeal). Do not casualise court prose to avoid a
machine tell; fix with anchors, application and cuts.

## Locks

Never change, drop or soften: facts, dates, amounts, parties, source anchors,
exhibit labels, rule numbers, defined terms, disputed-status wording ('alleged',
'disputed', 'not admitted'), redaction limits, offer terms, non-admission and
non-waiver wording, deadlines, attachment and service statements, and
user-authority conditions.

- Affidavit: facts within the deponent's knowledge. A sworn paragraph can be the
  evidence; no external document goes to weight, not deletion.
- Proposed orders: classify each item as order, undertaking, recital or reason.
  An order needs actor, act, object and deadline or trigger. Flag missing
  detail; never invent it.

## Condense

- Put the operative point first. Reverse-outline by function; move detail to the
  chronology, annexure index or affidavit rather than deleting it.
- Stop before losing an anchor, party label, status, qualifier, exhibit label,
  pinpoint, forum wording or safety boundary. There is no percentage target; if
  the result is longer, re-run the function test.
- Integrity check: set-diff figures, dates, amounts, pinpoints, labels,
  authorities and defined terms, then an independent qualitative read. A premise
  moved away from its contention, or a numbered particular folded into prose,
  can cost identifiability.
- Final scrub adds no argument, fact, authority, history or courtesy closer.
  Finish by naming the single remaining defect (overclaim, missing source, wrong
  home, unsafe disclosure, lost negative); if none, stop.

## House style

Defaults marked **(default)** are drafting conventions, not forum rules; the
form or template governs.

- Australian English, single quotation marks, day-month-year dates. A quotation
  over about three lines is an indented block without marks. Figures exact with
  separator and two decimals where exactness matters (`$14,132.18`); preserve
  competing figures.
- No em dash anywhere (a lint blocker). En dash only for closed ranges. Do not
  use a spaced hyphen as a clause separator.
- 'self-represented litigant' or 'litigant in person', never `pro se`. Use
  'filed', 'served', 'lodged', 'affirmed', 'sworn', 'annexure', 'exhibit' only as
  the forum or form does. Keep older words (`pursuant to`) inside quotations,
  titles, form fields and defined terms only; the lint flags the rest.
- Define a short form once, in plain text: `the respondent, [FULL NAME]
  ('[SHORT FORM]')`. No bold party names, money or argument. Bold headings only
  **(default)**. Lettered topic headings (`A. Purpose`) for affidavits and
  submissions **(default)**; the letter never resets paragraph numbering.
- Number substantive paragraphs continuously, one proposition each, unless the
  forum prescribes a scheme. One sub-paragraph hierarchy, `(a)` then `(i)`,
  written literally in Markdown source. A cited paragraph number is the rendered
  number: adding a whole paragraph breaks later references, so prefer sentence
  edits or fix every reference and check the render.
- Every Markdown heading needs a blank line before it for pandoc.
- Average about 20 words; over about 24, or three or more list items, is a
  prompt to split or tabulate. Active voice unless the actor is unknown or less
  important than the act.

## Bans in filing-facing text

The lint fails on a subset and warns on the rest; this list is the standard.

- Internal markers (`TODO`, `TBD`, `TBC`, `FIXME`, `drafting note`,
  `agent note`, `AI-generated`) and any internal path or id, including in HTML
  comments: `.work/`, `<repository>/docs/audits/`, scratch or absolute repo paths, task ids.
- US dates and US legalese (`COMES NOW`, `wherefore`, `herein`, `heretofore`,
  `aforementioned`, `Honorable Court`, `esq.`).
- Process language in filing text (`agent summary`, `review set`, `render check`,
  `source pack`): cite the source or the legal act instead.
- Correspondence filler: `I trust this email finds you well`,
  `please be advised`, `kindly`, `for the avoidance of doubt`,
  `we reserve all rights` (state the next step, or omit unless instructed), and
  `without prejudice` unless that effect is intended and instructed.
- Affidavit overreach, which belongs in submissions: `jurisdictional error`,
  `plainly wrong`, `bad faith`, `abuse of process`, `the Court should find`,
  relief in the body (`I seek`), calling another forum's order an error (use
  'disputed', 'under appeal', 'unable to reconcile'), self-diagnosis without
  medical evidence, and 'can be produced if required' repeated per paragraph.
- `clearly` and `obviously` (omit); `currently` and `recent` (give the date);
  'proved family violence' (name the dated source at its true status).

## Persuasion and register

- Force comes from structure, precision and located candour. Overstatement
  (`overwhelmingly`, `plainly`, `beyond doubt`) costs credibility: assert, pin to
  a source, drop the intensifier. Calibrate; uniform confidence across strong and
  weak points reads as machine tone.
- Keep one qualifier that marks a genuine boundary (source limit, reserved issue,
  disputed status) and cut the belt-and-braces repetition and softening closers
  around it. Prefer positive form unless the negative is the fact.
- Hold the register the stage requires. Leave and interlocutory documents argue
  'reasonably arguable' or 'warrants leave' (the required standard, not a tell)
  and do not declare contested facts proved. Merits submissions may say 'is
  established' where the record carries it. Mark a mixed document's boundary with
  a heading. Uncontested procedural facts stay plain at every stage.
- Hollow authority (a case recited, never applied) and assertions with no record
  anchor are the strongest machine tells. Pinpoints and application are the
  strongest human signal.

## Quotation integrity

Quotation marks assert the words appear contiguous and in order in the source.
Never fuse two passages, findings or paragraphs inside one set of marks. Mark
every elision with an ellipsis and every change with square brackets; if an
elision crosses a sentence or pinpoint, quote separately or paraphrase with a
pinpoint to each. The lint cannot check this: re-open the source when a quote
spans two pinpoints.

## Cross-forum drift

Check for another forum's labels, powers, forms, deadlines or service rules
copied in; relief sought from a decision-maker who cannot grant it; an
allegation from one proceeding stated as proof in another; a subsisting order
called wrong without a verified basis, especially while review or appeal is
pending; and a safety instrument framed as a final finding.

## Lint

`provenant check writing PATH --rules legal` on explicit paths. Failures block a
filing-facing document; warnings are review prompts. Source checks, render
checks and user gates still apply.
