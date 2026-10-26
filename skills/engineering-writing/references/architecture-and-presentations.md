# Architecture and Communication Documents

Use this reference for architecture descriptions, decision records, design documents, presentations to non-technical stakeholders, and meeting briefs. These documents either fix how the system is structured or carry an engineering message to people who will not read the code.

## Contents

- [Architecture description](#architecture-description)
- [Architecture decision record (ADR)](#architecture-decision-record-adr)
- [Design document](#design-document)
- [Presentations and slide decks](#presentations-and-slide-decks)
- [Progressive disclosure for mixed audiences](#progressive-disclosure-for-mixed-audiences)
- [Meeting brief](#meeting-brief)

## Architecture description

Reader's job: understand the system's structure, the significant decisions behind it, and how it addresses each stakeholder's concerns. Grounded in ISO/IEC/IEEE 42010 and the C4 model.

The 42010 idea: an architecture description frames **stakeholders** and their **concerns** through **views**, each governed by a **viewpoint** (the conventions for that view). Every concern that matters should be addressed by at least one view. So: identify who cares, what they care about, then show the structure from the angles that answer those concerns.

Use the C4 model for the day-to-day views, from most to least zoomed-out:

1. **System context**: the system as one box, its users, and the external systems it talks to. For non-technical readers and the opening of any architecture doc.
2. **Container**: the deployable/runnable units (web app, API, database, worker, queue) and how they communicate. The most useful level for most teams.
3. **Component**: the major parts inside one container and their responsibilities.
4. **Code**: class/schema level, generated on demand, rarely maintained by hand.

Skeleton:

1. Purpose and scope of the description.
2. Stakeholders and their concerns (link to the stakeholder analysis).
3. Context view (C4 system context): what the system is and its boundary.
4. Container view: the runtime pieces, responsibilities, and interactions.
5. Key decisions and constraints (link to ADRs).
6. Quality attributes: how the architecture meets the significant non-functional requirements (performance, availability, security, cost), with the trade-offs named.
7. Risks and areas of known weakness.

Discipline: start with the context view so a non-specialist can orient before the detail. State the trade-offs as well as the choices; an architecture doc with no trade-offs is marketing. Say what the architecture does not yet handle (scale ceilings, single points of failure, deferred concerns). Keep diagrams and prose consistent; a diagram that contradicts the text is a defect.

## Architecture decision record (ADR)

Reader's job: a future developer needs to know what was decided, why, and what it cost, without reconstructing the debate. Grounded in Michael Nygard's format. Keep it to one or two pages.

Skeleton:

1. **Title**: short, and a decision, for example `0007. Use Postgres for the primary datastore`.
2. **Status**: proposed or accepted.
3. **Context**: the forces at play, the constraints, and the options in the frame. Facts and pressures, not the decision yet.
4. **Decision**: the choice, stated in the active voice and present tense. `We will use Postgres.`
5. **Consequences**: what follows, positive, negative, and neutral. List the negatives honestly; every real decision has them.

Discipline: one decision per record. Write it as a note to a future colleague, in full sentences. Record the alternatives you rejected and why, so no one re-litigates them. Amend accepted ADRs in place with one `Changed:` line; git keeps history; see `engineering-docs`. Number ADRs sequentially and keep them in the repository (`docs/adr/`) beside the code they govern.

## Design document

Reader's job: understand how a feature or change will be built before it is built, so reviewers can catch problems on paper. Heavier than an ADR (which records one decision), lighter than a full architecture description.

Skeleton:

1. Problem and context: what we are solving and why now.
2. Goals and non-goals: what this design does and explicitly does not address.
3. Proposed design: the approach, with a diagram at the right C4 level.
4. Alternatives considered: options weighed, with the reason each was not chosen.
5. Data model / API / interface changes.
6. Cross-cutting concerns: security, privacy, performance, migration, observability, failure modes.
7. Testing and rollout: how it will be verified and released, including rollback.
8. Open questions and risks.

Discipline: non-goals are as valuable as goals. Show the alternatives so reviewers trust the choice. Name the failure modes and the rollback. Distinguish what is decided from what is still open.

## Presentations and slide decks

Reader's job: for a non-technical audience (sponsors, clients, partners, a board), understand what changed, what it means for them, and what decision is being asked, without following the technical detail. Use the assertion-evidence approach (Alley, Penn State); it measurably beats topic-plus-bullets decks on comprehension and recall.

- **One idea per slide, asserted in the title.** Each slide has a single assertion, written as a full sentence in the title (`Search now returns results in under 200 ms`), supported by one piece of visual evidence: a chart, a diagram, a photo, a single number. Not a bullet-point list restating the title.
- **The slide is the evidence; the speaker carries the argument.** Before writing a sentence on the glass, ask whether it can be spoken instead. Speaker notes hold the explanation, transitions, and nuance; the glass holds the assertion and its evidence. If a printed record is needed, generate a notes handout rather than thickening the slides.
- **Order the deck outside-in for decision meetings.** Business first: the problem, what it means for the audience, what you would build, how it runs and what it costs, then the ask and the decision list. Put the ask after the case is made, but disclose it on the agenda slide so nobody feels ambushed; the numbers land once scope, plan, and costs are on the table.
- **State the ask once, clearly, and keep it consistent.** The recommendation must not move, blur, or reprice across the deck.
- **Run a repetition audit across slides as well as within them.** The tell is the same idea re-aphorised on adjacent slides, the same stat tiles appearing twice, or a closing line repeated at both a section and the close. Say each thing once, on the slide that owns it.
- **One numbering scheme.** Stages, gates, phases, and milestones share a single scheme, and it matches the documents the deck summarises. Two slides using different labels for the same checkpoint is a structural defect, not a style issue.
- **Every figure must inform the decision.** For each number, chart, or diagram ask: what does the room decide differently because of this? Cut decorative market-size precision; round on the glass and keep the exact figure in the sources. Never write `nearly` beside an exact figure. Lead with the outcome number the audience feels (days to cash), not the intermediate metric it is derived from, and never present two different metrics as if they were the same measurement.
- **Read every stat as your most sceptical listener.** A true, cited figure can still argue against you: a "95% of code is AI-generated" stat in a compensation pitch invites "then why pay you?", and comparing one person's cost to a whole team's reads as cherry-picking in a negotiation. Reframe or drop any stat whose hostile reading beats its friendly one.
- **Audit talk time before trimming slides.** Words in the speaker notes divided by roughly 140 wpm gives the scripted minutes; add demo and discussion time and check the total against the slot. Length problems are usually redundancy, not slide count.
- **Time-box live demos with a core path.** Write the runbook as a short core path (the steps that carry the argument, with explicit audience check-ins) plus clearly marked if-time extensions. An untimed demo eats the meeting and buries the decision.
- **Translate, do not dumb down.** Define or avoid jargon; use the audience's language (cost, risk, time, value). Keep roadmaps at theme level; task detail belongs in the pre-read documents, cross-referenced.
- **Sweep titles and notes for slide-specific machine tells**, on top of the general sweep in `engineer-voice.md`: aphorism and chiasmus pairs (`the app wins the firms; the funding earns the revenue`), `X, not Y` slogan titles, coined abstractions no engineer says aloud (`structured distrust`), motivational-poster closers (`build it properly, on the right foundation`), and scripted sincerity in the speaker notes (`let me be straight with you`, `worth naming out loud`, `the sentence that matters`). Replace each with the plain assertion; conversational is fine, performed is not.

Validation before presenting: a non-technical reviewer can state the decision being asked after the agenda slide; the ask appears once and consistently; stages, gates, and milestones use one scheme that matches the pack documents; every figure survives "what does the room decide with this?" and the hostile-reading test; scripted talk time plus demo fits the slot with room for discussion; no slide relies on unexplained jargon.

## Progressive disclosure for mixed audiences

Keep the primary layer concise: the adopted design, important trade-offs,
material risks, open decisions and any operational cost range. Offer optional
depth through stable, human-labelled links to the canonical requirements,
architecture, design, security and specification material. Links must be
accessible and useful to the intended reader, not raw internal mechanics they
cannot open or interpret.

Keep target design separate from current implementation status when both are
relevant; omit status narration when it does not help the reader decide or act.
For cost estimates, name the period, range, assumptions and contributing
services or components, then link those drivers to the technical choices or
evidence behind them.

## Meeting brief

Reader's job: walk into a meeting knowing the purpose, the decisions needed, and the context, in a few minutes of reading.

Skeleton:

1. Purpose: why this meeting, in one line.
2. Decisions or outcomes needed: the specific things that must be resolved.
3. Background: the minimum context to decide, with links for depth.
4. Options or proposal: what is on the table, with the recommendation if there is one.
5. Questions to answer.
6. Next steps and owners (fill after the meeting).

Discipline: front-load the decisions needed; a brief that buries the ask in background wastes the room. One page is usually enough. State the recommendation and let the meeting test it, rather than presenting a neutral menu with no view.
