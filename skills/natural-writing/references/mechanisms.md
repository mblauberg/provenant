# Mechanisms

The catalogue of defects the skill diagnoses. Each entry gives the pattern
(what the sentence does), why a reader minds, the fix, and one before/after
pair. Judge clusters, not single words: one instance proves nothing. The fix
is always to restate the claim, never to swap a word, because the tell moves
to the replacement.

## 1. Contrastive reframing

**Pattern.** A sentence denies a position nobody held in order to introduce
the real one (`not just X but Y`, `more than a Z`, `rather than simply`).

**Cost.** The reader spends attention on a straw claim.

**Fix.** Keep a contrast only when the reader held the false view, and name
who held it. Otherwise state the real claim.

- Before: This is not just a cache; it is a coordination layer.
- After: The cache also decides which worker owns a key.

## 2. Significance flagging

**Pattern.** A sentence announces that something matters instead of saying
what changes (`this matters`, `a pivotal moment`, `underscores`, `serves as a
testament`).

**Cost.** The reader gets emphasis with no content.

**Fix.** Give the consequence: who is affected, what changes, by how much. If
there isn't one, delete the sentence.

- Before: This change is pivotal for reliability.
- After: Retries now stop after three attempts, so a dead queue no longer
  holds the worker for 30 minutes.

## 3. Mannered metaphor

**Pattern.** A figure stands in for a direct statement (`a dial worth
turning`, `earns its keep`). Newer models drift here even when the stock
phrases are gone.

**Cost.** The reader decodes the figure, then the claim.

**Fix.** Say the literal thing.

- Before: The timeout is a dial worth turning.
- After: Raising the timeout from 5 s to 20 s removed the failures on slow
  links.

## 4. Unanchored claims

**Pattern.** A claim with no number, name, path, date, error text or honest
negative. This is the common root of mechanisms 1 to 3 and the best cure for
them.

**Cost.** The reader can't check it, so it carries no information.

**Fix.** Add only anchors the source holds. Where none exists, narrow the
claim or flag the gap.

- Before: The pipeline is fast and reliable.
- After: The pipeline processed 12,000 records in 41 s; failure behaviour
  under a full disk is untested.

## 5. Participle tails

**Pattern.** A trailing `-ing` clause adds an empty synthesis (`, highlighting
...`, `, ensuring ...`).

**Cost.** It asserts an effect without evidence.

**Fix.** Stop at the fact, or make the conclusion its own supported sentence.

- Before: The loader skips bad rows, ensuring data quality.
- After: The loader skips rows that fail validation and logs each one.

## 6. Nominalisation and copula avoidance

**Pattern.** Verbs become nouns and `is` becomes `serves as` (`conducts an
evaluation of`, `provides a mechanism for`).

**Cost.** The actor and the action vanish.

**Fix.** Use the plain verb, and use `is` and `has`. The phrase-level repairs
for shortening are in the table in [condense-pass.md](condense-pass.md).

- Before: The module serves as the entry point and provides a mechanism for
  the validation of input.
- After: The module is the entry point and validates input.

## 7. Recap and optimism closers

**Pattern.** A closing sentence repeats the section, or ends on challenges
followed by hope (`In summary`, `going forward`).

**Cost.** The section ends on filler, not the decision.

**Fix.** End on the decision, risk, number, next action or remaining
uncertainty. Cut section recaps.

- Before: In summary, these steps ensure a smooth migration going forward.
- After: Run the migration on staging first; production waits on the backfill
  count matching.

## 8. Document structure misused

**Pattern.** Headers on short texts, bold-lead bullet lists, uniform bullet
weight, and bullets where an argument needs prose. Chat has moved away from
this; long documents and READMEs still show it.

**Cost.** Reasoning is chopped into fragments and the links are lost.

**Fix.** Prose for reasoning, lists for parallel items (steps, fields,
flags), headings only to navigate.

- Before: bullets reading `Speed: high`, `Reliability: high`, `Cost: low`.
- After: It processed 12,000 records in 41 s on one core; cost wasn't
  measured.

## 9. Rhythm and density

**Pattern.** Uniform sentence length and stacked connectives (`Moreover`,
`Additionally`). The newest models also write long dense sentences with few
paragraph breaks.

**Cost.** Nothing stands out, so the key point looks like support.

**Fix.** Let the logical relation link sentences: `However` for a real
contrast, `Therefore` for a conclusion that follows, `In contrast` when two
things are directly compared. Drop transitions that only announce structure
(`Having established this`, `Moving forward`). Put the key point in the
shortest sentence, and split any sentence that carries a method, a result and
a caveat.

- Before: The job failed. Moreover, the retry also failed. Additionally, the
  queue filled.
- After: The job failed and so did the retry, so the queue filled.

## 10. Habitual triples and hedging stacks

**Pattern.** Three items because three sounds complete; `may potentially`;
`experts argue` with no source.

**Cost.** The list is padded and the hedge hides the evidence's strength.

**Fix.** Use the real count and one calibrated qualifier, and name the source
or drop the claim. Don't hedge strong evidence.

- Before: The fix improves speed, stability and maintainability.
- After: The fix removes the second query and halves latency; stability and
  maintainability weren't measured.

## 11. Chat residue and sycophancy

**Pattern.** Validation openers, offers of more help, restating the prompt,
neutrality disclaimers.

**Cost.** The answer starts late and the validation weakens the advice.

**Fix.** Start with the content. In review or advice, state disagreement
plainly.

- Before: Great question! Let me walk you through it. ... Let me know if you'd
  like more help.
- After: Use a lock file; the cache has no owner field.

## Not mechanisms

Em-dash density, `delve`-class vocabulary and any single banned word are
churning surface forms, not proof of AI authorship. Don't hunt them by list;
fix the claim under them. House style still avoids em-dashes
([au-english.md](au-english.md)), and a colon that reproduces the same pivot
isn't a fix.

## Voice matching

Take these from a supplied sample: sentence-length spread, paragraph density,
openings, punctuation, pronouns, certainty, jargon, warmth and bluntness.
Borrow habits, not signature phrases. The sample outranks the defaults, and
dialect and register stay as they are unless standardisation is asked for.
