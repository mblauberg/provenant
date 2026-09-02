# Anti-AI taxonomy (tiered)

`natural-writing` owns this taxonomy for the whole writing family.
`engineering-writing` and `legal-writing` link here for the base sweep and add
only their own domain overlay (engineering process-language, filing-facing
hard bans); the academic overlay (thesis meta-discourse, repo jargon, viva
register) lives in [academic-prose.md](academic-prose.md). Read
[patterns.md](patterns.md) first for the decision rule, the research basis,
and the time-decay warning: treat every item below as a dated editing clue,
never proof of authorship, and calibrate on clusters and repeated structure,
not a lone word or mark.

## Core test

For each sentence, ask: would the writer have produced this because the
document needed it, or did a language model fill a paragraph shape? If the
sentence carries no fact, mechanism, number, decision, caveat, or necessary
transition, cut it or rewrite it so it does.

A shorter or smoother edit is not automatically better. Reject any edit that
drops a condition, number, actor, or obligation, turns an exact claim into a
vague benefit, or varies a defined technical term for style.

## How to read the tiers

No single word or mark proves machine drafting; density, clustering, and
convergence of signals are the signal. Work in three tiers: remove artefacts
outright, cap density signals, and police structural tells hardest, because
structural tells survive synonym swaps and register changes.

## Tier 1: artefacts (remove on sight)

These have no legitimate register value in any of this family's domains.

- **Chatbot framing.** Openers that talk to the reader instead of starting
  the content: `Sure`, `Of course`, `Great question`, `You're absolutely
  right`, `Let's dive in`, `I hope this helps`, `In today's fast-paced
  world`. Also prompt restatement, knowledge-cutoff or refusal text, and `As
  an AI language model`.
- **Throat-clearing and meta-discourse.** `It is important to note`, `It
  should be noted`, `As previously mentioned`, `In this document we will`,
  `This section explains`. Write about the subject, not the act of writing:
  `This guide explains how to configure the worker` becomes `Configure the
  worker.`
- **Interface meta-copy.** A heading, badge, label or helper sentence must
  name information or an action. Remove copy whose only job is to advertise
  the format, ease of reading or an obvious card or control. Keep instructions
  when the action, consequence or accessibility affordance is not self-evident.
  Do not repeat an actor in both a label and its sentence unless attribution
  changes authority, provenance, liability or negotiation meaning.
- **Tool and markup residue.** Literal tool strings (`turn0search0`,
  `oaicite`, `oai_citation`, `contentReference`, `attributableIndex`),
  `utm_source=chatgpt.com` in link URLs, stray `**bold**` markers or `#`
  headings in rendered prose, curly quotes inside code or commands (they
  break copy-paste), and emoji in documents whose surrounding convention does
  not already use them.
- **Fabricated references.** Never name a source, package, API, method,
  flag, version, or URL that has not been verified against the underlying
  material; an unverifiable identifier is both a defect and a strong tell.
  Mark it `[FLAG: verify]` instead.
- **Internal process language in deliverables.** Agent, build, workflow, or
  workspace vocabulary leaking into reader-facing text (`the subagent
  found`, `gate fired`, `per the manifest`). Translate to the actual fact or
  cite the real source.
- **Patronising expert instruction.** Directions, reminders or parentheticals
  telling a reader how to do the thing they are the expert in: telling a
  solicitor to identify a company by ACN, telling an auditor which standard
  applies, telling an engineer to use version control. It costs the writer
  standing and buries the one fact the reader actually needed. Supply the fact
  only the writer holds and stop; the method is the reader's. Offering to help
  with the specialist's own craft is the same defect in a friendlier voice.
- **Absence tombstones.** Never announce an absence that exists only because
  an editing instruction removed content (`Subgroup results are not
  discussed here because that section was removed`). The test: would this
  sentence exist if no one had asked to remove anything? A genuine,
  author-owned scope boundary is different and may stay, when it answers a
  question the reader would actually ask and the reason is about the
  subject, not the edit (`No subgroup estimate is reported because the
  sample does not support it`).

## Tier 2: density signals (a cluster is the tell)

Each item is legitimate in isolation; a pile of them in one passage is
machine texture.

- **Puffery adjectives.** Cut unless a metric or test backs them: `crucial`,
  `pivotal`, `vital`, `robust`, `seamless`, `comprehensive`, `holistic`,
  `cutting-edge`, `state-of-the-art`, `world-class`, `transformative`,
  `groundbreaking`. `robust` is allowed only next to a named robustness test.
- **Second-tier inflation vocabulary.** `leverage` (use `use`), `utilise`,
  `facilitate`, `foster`, `harness`, `navigate`, `streamline`, `nuanced`,
  `multifaceted`, `myriad`, `plethora`, `paramount`, `delve into`,
  `meticulous`, `intricate`. Replace with the plain word or the concrete
  claim. Collapse ceremonial doublets: `each and every` becomes `each`;
  `any and all` becomes `any`; `null and void` becomes `void`.
- **Copula avoidance.** `serves as`, `stands as`, `acts as`, `boasts`,
  `features`, `represents`, `plays a role in` where plain `is`, `has`, or
  `includes` would do.
- **Register lifts and personification.** Reaching for the weightier
  near-synonym the work does not need (`utilise` for `use`, `interlocutor`
  for `sender`); each is defensible alone, together they read as machine
  vocabulary. Do not personify a system or document as a debating actor
  (`the detector takes a different position`); state the mechanism.
- **Interpretation-smuggling verbs.** `underscores`, `showcases`,
  `highlights`, `demonstrates`, `reflects`, `serves as a testament to`. If
  the interpretation matters, attribute or evidence it; if not, delete it.
- **Vague benefit claims.** `streamlines workflows`, `enhances reliability`,
  `drives impact`, `ensures quality`. Replace with what changed and its
  number: `p99 latency dropped from 800 ms to 210 ms`.
- **Empty `-ing` tails.** `The system filters invalid input, ensuring
  reliable evaluation` becomes `The system filters invalid input before
  metric aggregation.` Name the mechanism or cut the tail.
- **Decorative tails and glosses.** Trailing temporal or prepositional
  padding (`over time`, `as the process evolves`); cut it or state the
  actual increment. Inline comma-gloss definitions; define the term in its
  own short sentence instead.
- **Formulaic contrast and symmetry.** `not only X, but also Y`, `more than
  just`, false ranges (`from ingestion to insight` where the poles are not a
  real range). Use `from X to Y` only for a genuine measured range. Not
  every list has three items; vary the count.
- **Fake conclusions.** `In summary`, `In conclusion`, `Overall`,
  `Ultimately`, and the `despite challenges ... the future outlook` closing
  formula. End on the concrete decision, risk, number, or next action.
- **Vague authority.** `industry reports suggest`, `it is widely accepted`,
  `many teams find` with no citation. Name the source or drop the appeal.
- **Conjunctive-adverb chaining.** Stacked `Moreover`, `Furthermore`,
  `Additionally` as sentence or paragraph openers. Let the logical relation
  carry the link, or name it. Never open consecutive paragraphs with
  additive adverbs.
- **Hedge-and-reassure stacking.** More than one qualifier before an
  assertion (`It is worth noting that, generally speaking, in most
  cases...`), and steering cues `notably`, `interestingly`, `importantly` as
  openers. State the claim once with the single qualifier the evidence
  warrants.

## Tier 3: structural tells (police hardest)

Word-independent patterns; they survive synonym swaps, and formal register
does not excuse them.

- **Metronome rhythm.** Uniform sentence and paragraph length. Vary length
  deliberately and land the operative point in the shortest sentence.
- **Over-signposting.** A connective opening every paragraph, plus
  meta-signposts (`As mentioned earlier`, `Having covered X, we turn to
  Y`). Prefer a substantive connector (`Because the cache is write-through,
  ...`) and delete the rest.
- **Both-sides seesaw.** `On the one hand ... on the other` ending
  noncommittally. Commit to the position the evidence supports; a named,
  specific contrast is fine.
- **Hollow topic sentences.** Openers that restate the heading without
  adding a checkable proposition.
- **Recap endings.** Conclusions that restate what was said. End on the
  decision, risk, number, or next action.
- **Evenly weighted lists.** Every bullet the same length regardless of
  importance, and the `bold term: gloss` bullet as every section's default
  shape. Genuine enumerations (steps, fields, statutory elements) correctly
  stay parallel.
- **Situational list labels.** A bold lead-in that narrates a condition
  instead of naming the thing: `Ready to go live when the founders decide`,
  `A base to hire onto`, `Evidence on hand`. Name it with a short noun phrase
  the reader can scan and refer back to (`Go-live readiness`, `Technical
  continuity`, `Audit-ready records`), and let the sentence after it carry the
  condition. A label that resists a noun phrase usually marks two items fused
  into one, or a bullet with nothing to name.
- **Sentence-ending participle synthesis.** `, highlighting ...`,
  `, underscoring ...`, `, reflecting ...` appending an unsourced conclusion
  to a factual sentence. Stop at the fact, or make the conclusion its own
  supported sentence.
- **Template sections.** Every section built the same way (preview, three
  equal blocks, recap). Swap test: if the paragraphs can be reordered
  without breaking anything, the section has no load-bearing spine.
- **Uniform confidence.** The same assertive register across strong and weak
  claims flattens the writer's most useful signal; see Calibrated
  confidence below.
- **Unanchored claims.** Polished prose with nothing checkable: no number,
  command, file path, error text, measurement, source pinpoint, or code
  reference. Specific anchors and honest negatives are the strongest human
  signals available.

## Markdown residue in prose

Training on Markdown leaves structural habits that outlast the word-level
tells: bold-emphasised key terms mid-sentence; a bold term, a colon, then a
gloss, repeated as a fake definition list; Title Case headings where the
surrounding convention is sentence case; skipped heading levels; horizontal
rules before headings; tables where prose would do; everything broken into
suspiciously even parts (always three, always four).

The em dash is the Markdown artefact that survives a "no formatting"
instruction; see [au-english.md](au-english.md) for the ban and its
punctuation replacements. Do not "fix" a banned em dash with a colon, spaced
hyphen, or semicolon that reproduces the same dramatic pivot.

## Noun-stacking and nominal compression

A noun tower piles two to four premodifiers onto an abstract head noun the
reader unpacks last (`a class-specific generator prompt stack`, `an
author-process register fingerprint`). Each word may be correct; the pile
still reads machine-assembled.

- Cap the premodifiers. Move qualifiers into a relative or prepositional
  clause: `a register fingerprint left by how each class was authored`, not
  `an author-process register fingerprint`.
- Unpack vague container heads (`envelope`, `surface`, `posture`, `regime`,
  `landscape`, `forensics`, `boundary`, `profile`): `the measured size,
  memory, and latency of the local deployment`, not `the measured local
  deployment envelope`.
- Prefer a verb plus a couple of nouns over a noun pile: `passively scoring
  risk on the device`, not `passive on-device risk scoring`.
- Release heavy nominalisation. A verb buried in an `-ion`/`-ment`/`-ance`
  noun usually reads better freed: `When a validity gate fails, narrow the
  claim`, not `Failure of a validity gate should narrow the claim`.
- Fix each recurring tower once and reuse the repaired phrasing; restacking
  the same defined term differently on every mention compounds the load.

Keep defined terms and honest negatives intact while lightening the prose
around them.

## Additive drafting

Additive drafting is the habit of adding another sentence or paragraph
because the text feels incomplete, then joining it with a neutral connector.
Before adding one, name the new job it does for the reader. Treat these as
failed reasons: `sets the scene`, `sounds more complete`, `adds a
transition`, `makes the tone warmer`, `summarises what was just said`. In a
rewrite, condense, or scrub pass, a net addition is a warning sign: cut
again, or state the specific fact, condition, or step that required the
extra words.

## Defensive over-qualification

The mirror of hedge stacking: pre-emptive disclaimers no one asked for,
wrapped around a point out of worry the reader will over-read it. Symptoms:
a sentence that first states what something does not do, then what it does,
then how little is claimed for it; re-listing every excluded case when one
clean scope statement would carry the boundary; closers that re-soften a
point already made.

Cut-or-keep test: would removing the words remove a real boundary (a scope
limit, an untested case, a reserved issue, a non-admission), or only remove
anxiety? Keep the one qualifier that defines a genuine boundary; cut the
repetition around it.

## Calibrated confidence

Match assertion strength to evidence strength, sentence by sentence. Uniform
confidence reads as machine tone and wastes the reader's trust; intensifiers
(`clearly`, `obviously`, `undoubtedly`, `overwhelmingly`) cost credibility
rather than adding it. State strong claims plainly with their evidence. Give
weak claims one located qualifier. Never flatten both to the same middle
register, and never dress a hypothesis as a finding.

## Positive form is not positive spin

Positive form (Strunk's Rule 11) removes tame, evasive negatives (`did not
support` becomes the direct claim it bounds). It must never soften an honest
limitation, failure, or withheld claim into reassurance: a measured overrun
stays an overrun, a missed defect stays a miss, an unestablished claim stays
withheld. Positive *form* is the target; positive *spin* is itself an AI
tell and a defect. After any results-framing change, sweep for half-pivot
residue: structure, order, and headings that lag the rephrase and keep
promising what the new framing withdrew.

## Final scrub

One pass, in order: Tier 1 artefacts, Tier 2 density signals, Tier 3
structural tells, Markdown residue, and noun towers. Then confirm: no em
dash and no colon, spaced hyphen, or semicolon reproducing its pivot; every
sentence carries a fact, mechanism, decision, caveat, or necessary
transition; at least one short sentence in the passage; confidence is
calibrated, not flat.

**Final self-audit.** Before returning the text, name the single concrete
remaining defect, if any: unanchored claim, flat rhythm, noun tower, false
transition, uniform confidence, recap ending, internal process language, or
dropped caveat. If no concrete defect can be named, stop. Fix defects, not
style anxiety.

## On AI detectors

See [patterns.md](patterns.md), Detector limits. Do not edit to lower a
detector score: a score is not evidence of authorship, and the traits that
make prose read as machine-written (flat rhythm, low specificity, inflated
register, unanchored claims) are exactly the ones this taxonomy already
removes for their own sake. Detector accuracy is uneven and falls hardest on
formal, non-native, short, or heavily paraphrased prose. Never strip correct
passive voice, formal register, or technical precision solely to appease a
tool.
