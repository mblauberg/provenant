# Natural writing patterns and evidence

Use this reference when a normal rewrite is insufficient, a writing sample
defines the voice, or the draft may contain model-associated patterns.

## Decision rule

Natural writing is faithful to the writer, audience, genre and purpose. It is
not conformity to an imagined universal human voice. Preserve dialect,
register and non-standard English unless the user asks for standardisation.

Treat every sign below as an editing clue, never proof of authorship. Judge the
draft by relevance, factual support, specificity, clarity, coherence, voice and
reader value. Do not worsen grammar, inject quirks or chase a detector score.

## Durable quality risks

**Assistant residue.** Remove chatbot wrappers, knowledge-cutoff disclaimers,
tool output, fake citations and generic offers of further help. These are
document defects regardless of who wrote them.

**Weak information.** Cut throat-clearing, repeated conclusions, vague
authority, unsupported interpretation and sentences that add no new claim.
Replace ceremony with facts already present in the source. If support is
missing, narrow the claim or flag it.

**Template repetition.** Repair recurring sentence frames, mechanically even
paragraphs, repeated transition openers, rule-of-three lists used by habit and
tidy challenge-then-optimism endings when they impair flow or depart from the
writer's sample. Do not manufacture unevenness for its own sake.

**Generic or inflated language.** Inspect promotional adjectives, prestige
verbs, clichés, abstract emotion labels, empty benefit claims and false ranges.
Keep a strong word when it is precise and supported; remove it when it only
performs importance.

**Missing specificity.** Prefer named actors, concrete actions, dates, numbers,
constraints and examples already available in the source. Never invent
personal detail or factual texture to make prose seem authentic.

**Voice mismatch.** Check formality, certainty, humour, warmth, sentence and
paragraph weight, punctuation habits, pronoun use and jargon tolerance against
the writer's sample and the document's job.

**Sentence construction.** Fix unclear actors, dangling openers, comma splices,
misplaced modifiers and buried verbs because they obstruct meaning, not because
they are authorship signals.

## Observed model-associated patterns

These findings are time-sensitive and corpus-level:

- Words including `delve`, `intricate` and `underscore` became
  overrepresented in recent scientific English. Their prevalence changes over
  time, and one occurrence proves nothing. Inspect clusters for precision and
  fit rather than maintaining a blacklist.
- Different model families show different word-frequency and
  morphosyntactic fingerprints. Some survive rewriting, translation and
  summarisation. A universal vocabulary list is therefore the wrong tool.
- Controlled corpora show higher reuse of syntactic templates in model output.
  This supports checking repeated constructions across a passage, not judging
  an isolated sentence.
- Experienced LLM users reviewing 300 American-English non-fiction articles
  cited vocabulary clusters, predictable structure, over-clean grammar, low
  originality, homogeneous quotations, over-explanation, consistent formatting
  and optimistic conclusions. The result was strong in that study but remains
  specific to its genre, language and 2024-era models.
- Broad 2026 comparisons find that most individual linguistic features vary by
  model and domain. Lexical-richness measures transfer better in some studies,
  but no universal direction or target score is safe for editing one draft.

Inspect lexical, syntactic, discourse and factual quality together. Compare
against the genre and a supplied voice sample whenever possible.

## Voice matching

Match sentence-length distribution, paragraph density, openings, punctuation,
pronouns, certainty, jargon, humour, warmth and bluntness. Borrow habits, not
signature phrases. The sample outranks these defaults.

Post-editing can move a draft closer to a writer's style while leaving residual
model style and reducing diversity. Return the draft for the writer's judgement
when voice authenticity matters.

## Editing modes and genres

- **`light-touch`**: repair only named defects and preserve most wording.
- **`precision-preserving`**: freeze claims, citations and uncertainty; prefer
  flags over smooth unsupported prose.
- **`full-rewrite`**: reshape low-stakes prose, cut scaffolding and strengthen a
  point of view already supported by the source.
- **Creative**: remove clichés, redundant exposition and purple prose; preserve
  authored quirks and tense.
- **Marketing**: replace superlatives with concrete benefits already supported;
  never invent claims.
- **Social**: cut announcement framing and engagement bait; keep the writer's
  actual register.
- **Journalistic**: use named attribution, dates and jurisdictions; remove vague
  sourcing and generic news wrappers.

Route software documentation and READMEs to `engineering-writing`, and
Australian legal drafting to `legal-writing`. Use `natural-writing` for
general, mixed or unclassified prose, and for scholarly prose with
[academic-prose.md](academic-prose.md).

## Detector limits

Detectors are not editing or authorship authorities. Large evaluations show
sensitivity to unseen generators, sampling, attacks, domains and decoding
settings. Controlled progressive-editing results show that intermediate mixed
authorship can be harder to detect than either endpoint.
Error rates also vary across demographic groups; disparities affecting English
language learners have been observed. Never use a score to accuse a writer or
to justify degrading clear prose.

## Research basis

- El Attar et al., [cross-model and cross-domain linguistic features
  study](https://arxiv.org/abs/2606.04177) (2026).
- Xia, Stańczak and Roth, [cross-dataset generalisation of linguistic
  features](https://aclanthology.org/2026.eacl-long.307/) (2026).
- Baumler et al., [human post-editing and residual model
  style](https://aclanthology.org/2026.acl-long.2030/) (2026).
- Stowe et al., [detector disparities across student
  groups](https://aclanthology.org/2026.acl-long.109/) (2026).
- Bsharat et al., [progressive human-to-model editing and mixed-authorship
  detection](https://arxiv.org/abs/2606.06481) (2026).
- Russell, Karpinska and Iyyer, [expert human detection and reported
  cues](https://aclanthology.org/2025.acl-long.267/) (2025).
- Sun et al., [model-specific word-frequency
  fingerprints](https://proceedings.mlr.press/v267/sun25z.html) (2025).
- Juzek and Ward, [lexical overrepresentation in scientific
  English](https://aclanthology.org/2025.coling-main.426/) (2025).
- Chakrabarty, Laban and Wu, [professional editing of creative
  prose](https://doi.org/10.1145/3706598.3713559) (2025).
- Fleisig et al., [LLMs and non-standard English
  varieties](https://aclanthology.org/2024.emnlp-main.750/) (2024).
- Shaib et al., [syntactic template reuse in model
  output](https://aclanthology.org/2024.emnlp-main.368/) (2024).
- Dugan et al., [RAID detector robustness
  benchmark](https://aclanthology.org/2024.acl-long.674/) (2024).
