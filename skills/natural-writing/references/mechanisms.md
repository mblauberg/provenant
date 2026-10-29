# Mechanisms

Each entry defines a defect by what the sentence does, says why a reader
minds, and shows one pair. Judge clusters, not single words: one instance
proves nothing. The fix is to restate the claim, not to swap a word.

## 1. Contrastive reframing

A sentence denies a position nobody held in order to introduce the real one
(`not just X but Y`, `more than a Z`, `rather than simply`). The reader spends
attention on a straw claim. Keep a contrast only when the reader held the
false view, and name who held it.

- Before: This is not just a cache; it is a coordination layer.
- After: The cache also decides which worker owns a key.

## 2. Significance flagging

A sentence announces that something matters instead of saying what changes
(`this matters`, `a pivotal moment`, `underscores`, `serves as a testament`).
Give the consequence: who is affected, what changes, by how much, or delete.

- Before: This change is pivotal for reliability.
- After: Retries now stop after three attempts, so a dead queue no longer
  holds the worker for 30 minutes.

## 3. Mannered metaphor

A figure stands in for a direct statement (`a dial worth turning`, `earns its
keep`). The reader decodes the figure and then the claim. Say the literal
thing. Newer models drift here even when stock phrases are gone.

- Before: The timeout is a dial worth turning.
- After: Raising the timeout from 5 s to 20 s removed the failures on slow
  links.

## 4. Unanchored claims

A claim with no number, name, path, date, error text or honest negative. This
is the common root of mechanisms 1 to 3 and the best cure for them. Add only
anchors the source holds; where none exists, narrow the claim or flag it.

- Before: The pipeline is fast and reliable.
- After: The pipeline processed 12,000 records in 41 s; failure behaviour
  under a full disk is untested.

## 5. Participle tails

A trailing `-ing` clause adds an empty synthesis (`, highlighting ...`,
`, ensuring ...`). It asserts an effect without evidence. Stop at the fact, or
make the conclusion its own supported sentence.

- Before: The loader skips bad rows, ensuring data quality.
- After: The loader skips rows that fail validation and logs each one.

## 6. Nominalisation and copula avoidance

Verbs become nouns and `is` becomes `serves as` (`conducts an evaluation of`,
`provides a mechanism for`). The actor and action vanish. Use the plain verb;
use `is` and `has`.

- Before: The module serves as the entry point and provides a mechanism for
  the validation of input.
- After: The module is the entry point and validates input.

## 7. Recap and optimism closers

A closing sentence repeats the section or ends on challenges followed by
hope. End on the decision, risk, number or next action; cut section recaps.

- Before: In summary, these steps ensure a smooth migration going forward.
- After: Run the migration on staging first; production waits on the backfill
  count matching.

## 8. Document structure misused

Headers on short texts, bold-lead bullet lists, uniform bullet weight and
bullets where an argument needs prose. Use prose for reasoning, lists for
parallel items (steps, fields, flags), headings only to navigate. Chat has
moved away from this; long documents and READMEs still show it.

## 9. Rhythm and density

Uniform sentence length, stacked connectives (`Moreover`, `Additionally`) and,
in the newest models, long dense sentences with few paragraph breaks. Let the
logical relation link sentences; put the key point in the shortest sentence;
split a sentence that carries a method, a result and a caveat.

## 10. Habitual triples and hedging stacks

Three items because three sounds complete; `may potentially` and `experts
argue` with no source. Use the real count, one calibrated qualifier, and name
the source or drop the claim. Do not hedge strong evidence.

## 11. Chat residue and sycophancy

Validation openers, offers of more help, restating the prompt, neutrality
disclaimers. Start with the content. In review or advice, state disagreement
plainly.

## Not mechanisms

Em-dash density, `delve`-class vocabulary and any single banned word are
churning surface forms. Do not target them by list; fix the claim under them.
Do not replace a dash with a colon that reproduces the same pivot.

## Voice matching

Match sentence-length spread, paragraph density, openings, punctuation,
pronouns, certainty, jargon, warmth and bluntness from a supplied sample.
Borrow habits, not signature phrases. The sample outranks these defaults, and
dialect and register are preserved unless standardisation is asked for.
