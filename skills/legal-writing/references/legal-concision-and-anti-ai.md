# Legal concision and machine-prose drafting

Load for any draft, rewrite, condense, diagnose or final-scrub task. The hub
`natural-writing` owns the mechanism catalogue and the condense procedure; this
file is the legal overlay: the Legal Function Test (a distinct scheme, not the
hub's observed/inferred claim classes), register, persuasion and the hard bans.

## Principle

Every sentence does legal work: relief, request, source-backed fact, rule,
contention, procedural step, limitation, safety qualification, consequence or a
necessary transition. Otherwise cut it or move it to the document that owns it.
Plain language is not casual language: keep terms of art, source anchors,
defined terms, forum labels, redaction limits and honest negatives where they
work. Cutting repetition is a persuasion move (see
`argument-structure-and-paragraphing.md`).

## Legal Function Test

Name each paragraph's one function: relief or order sought; procedural step;
source-backed fact; legal rule; contention; consequence or prejudice; safety or
redaction limit; practical request or deadline; correspondence protection (offer
term, non-admission, non-waiver, rights reservation); enforceable order detail
(actor, act, object, deadline or trigger). No function: cut. Two unrelated
functions: split. Another document owns it: move it, do not polish it in place.

## Condense

Follow the hub procedure. Legal specifics:

- Lock facts, dates, amounts, parties, source anchors, exhibit labels, rule
  numbers, defined terms, disputed-status wording, redaction limits and
  user-authority conditions. Put the operative point first.
- Reverse-outline by legal function. Move detail to the chronology, annexure
  index, affidavit or submissions; do not delete it.
- Stop before losing an anchor, party label, status, redaction qualifier,
  exhibit label, pinpoint, forum wording or safety boundary. There is no
  percentage target. If the result is longer, re-run the function test.
- Integrity: set-diff figures, dates, amounts, pinpoints, labels, authorities
  and defined terms, then an independent qualitative pass. A premise moved too
  far from its contention, or a numbered particular de-listed into prose, can
  cost identifiability: re-read the argument that relied on it.

## Wrong-home repair

Affidavit argument goes to submissions. Submission-level factual detail goes to
an affidavit, chronology or exhibit note. Reasons for an order go to
submissions. Relief buried in evidence goes to proposed orders. Correspondence
history goes to an outline if the email body does not need it. Internal
verification, manifest, render or audit language stays internal; translate it to
the source or legal act.

## Document locks

- Correspondence: protective labels, offer terms, non-admission or non-waiver,
  rights reservations, deadlines and attachment/service statements.
- Affidavit: facts within the deponent's direct knowledge (observations, receipt,
  conversations, state of mind, actions). A sworn paragraph can be the evidence;
  lack of an external document goes to weight, not deletion.
- Proposed orders: classify each item as order, undertaking, recital or reason.
  Orders need actor, act, object and deadline or trigger; flag missing
  enforceability details, never invent them.

## Legal-specific machine tells

- Process language in filing text: `agent summary`, `review set`, `render
  check`, `manifest proves`, `gate fired`, `source pack`, `audit found`.
  Translate to the legal act or cite the source.
- Noun towers (`multi-forum procedural consequence context`). Keep the defined
  term and move qualifiers into a clause: `The earlier order matters here
  because ...`. Replace abstract heads (`context`, `framework`, `posture`,
  `position`, `risk`) with the act, order, request or prejudice.
- Additive openers (`Moreover`, `Furthermore`) with no relation. Name the
  relation (`The prejudice is ...`) or omit. Never open consecutive paragraphs
  the same way. Formal connectives are proper register; govern by density.
- Rule-of-three by default cadence; hollow authority (a case recited, never
  applied to these facts); assertions with no record anchor. Polish without
  record engagement is the strongest tell judges name. Pinpoints and
  application are the strongest human signal.
- Buried verbs (`make a determination`, `give consideration to`) and ceremonial
  doublets (`null and void`, `each and every`): restore the verb, keep the single
  operative word, unless it is a quoted, statutory or form term.
- `reasonably arguable` at leave stage is the required standard, not a tell.

Do not casualise court prose to avoid a tell: contractions, chattiness and
anecdote are the wrong fix. Do not edit to beat a detector; disciplined formal
prose trips naive detectors. The fixes are anchors, application, calibration and
cuts.

## Defensive over-qualification and positive form

Keep one qualifier that defines a genuine boundary (a source limit, reserved
issue, non-admission, redaction condition, disputed status, authority gap). Cut
the repetition around it: belt-and-braces recitals, and closers that re-soften
a settled point (`the records cut both ways`). Use positive form to make the
position direct (`excludes the source document`), but keep negatives that are the
fact: non-service, non-filing, non-payment, non-admission, no order, no finding.
Positive form is style, not licence to soften.

## Persuasive strength

Force comes from structure, precision and located candour, not intensifiers.
Overstatement (`overwhelmingly`, `plainly`, `beyond doubt`) costs credibility;
frequent intensifiers correlate with losing appeals and emotional language with
losing merits votes (Long and Christensen 2008; Black et al 2016).

- Assert the proposition, pin it to a source, drop the intensifier.
- Argumentative point-headings, not topic labels.
- Keep your answer in the main clause and subordinate the opponent's point.
- One located concession beats a fog of caveats; put it in the next sentence.
- Confront the obvious weakness and meet it.
- Calibrate, never flatten: uniform confidence across strong and weak points
  reads as machine tone.

## Register at leave and interlocutory stage

Pick the register the stage requires and hold it. Leave documents argue
`reasonably arguable` or `warrants leave`; they do not declare contested facts
proved. A merits submission may assert `is established` where the record carries
it. Mixed documents mark the boundary with a heading, not sentence by sentence.
Uncontested procedural facts stay plain fact at every stage.

## Hard bans in filing-facing text

The lint (`provenant check writing <paths> --rules legal`) fails on a subset and
warns on the rest; this list is the standard, the lint a partial check.

- The em dash (U+2014) anywhere. The en dash only for closed ranges.
- Internal markers: `TODO`, `TBD`, `TBC`, `FIXME`, `drafting note`, `agent note`,
  `AI`, `generated`.
- Internal paths or ids, including in HTML comments (pandoc strips them from the
  render but they still ship in the source): `.work/`, `docs/audits/`, scratch
  paths, absolute repo paths, task ids.
- US dates; US legalese (`COMES NOW`, `wherefore`, `herein`, `heretofore`,
  `aforementioned`, `Honorable Court`, `pro se`, `esq.`).
- Dated wording the lint warns on (`pursuant to`, `prior to`, `whilst`,
  `utilise`, `hereby`, `thereof`, `the said`), unless quoted.
- Correspondence filler: `I trust this email finds you well`, `please be
  advised`, `kindly`, `as previously stated`, `we note with concern`,
  `for the avoidance of doubt`, `we reserve all rights` (state the next step or
  omit unless instructed), and `without prejudice` unless that legal effect is
  intended and instructed.
- Affidavit overreach, to submissions unless quoting a source: `jurisdictional
  error`, `plainly wrong`, `bad faith`, `abuse of process`, `dishonest`, `the
  Court should find`, `is an error` of another forum's order (use `disputed`,
  `under appeal`, `unable to reconcile`), conclusions on a contested figure,
  relief in the body (`I seek`), self-diagnosis without medical evidence, and
  `can be produced if required` repeated per paragraph (say it once).
- Risk words: `clearly`, `obviously` (omit); `currently` (as at [date]);
  `recent` (dated [date]); `proved family violence` (name the dated source at
  its true status).

## Quotation integrity

Quotation marks assert the words appear in the source, contiguous and in order.
Never fuse two passages, findings or paragraphs inside one set of marks. Mark
every elision with an ellipsis and every change with square brackets; if the
elision crosses a sentence or pinpoint, quote separately. If the words are not
contiguous, drop the marks and paraphrase with a pinpoint to each source. The
lint cannot check this; re-open the source when a quote spans two pinpoints.

## Cross-forum drift

Check for another forum's labels, powers, forms, deadlines or service rules
copied in; relief sought from a decision-maker who cannot grant it; an
allegation from one proceeding stated as proof in another; a subsisting order
called wrong without a verified basis, especially while review or appeal is
pending; a protective or interim safety instrument framed as a final finding;
and internal analysis left in filing text.

## Final self-audit

Name the single concrete remaining defect, if any: overclaim, missing source,
wrong home, unsafe disclosure, machine rhythm, noun stack, false transition,
unsupported legal conclusion, broad fairness closer, or a lost negative or
source boundary. If none, stop; do not rewrite for a vague feeling.
