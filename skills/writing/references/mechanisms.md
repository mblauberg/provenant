# Mechanisms

Diagnose by mechanism, never by phrase. Judge clusters: one instance proves
nothing. The fix is always to restate the claim, because a swapped synonym
leaves the mechanism and the tell moves to the next word.

1. **Contrastive reframing** (`not just X but Y`, `more than a Z`). Denies a
   position nobody held. Keep a contrast only when the reader held the false
   view, and name who.
   - Before: This is not just a cache; it is a coordination layer.
   - After: The cache also decides which worker owns a key.
2. **Significance flagging** (`this matters`, `pivotal`, `underscores`). Give
   who is affected, what changes, by how much, or delete.
   - Before: This change is pivotal for reliability.
   - After: Retries now stop after three attempts, so a dead queue no longer
     holds the worker for 30 minutes.
3. **Mannered metaphor** (`a dial worth turning`, `earns its keep`). Newer
   models drift here once stock phrases are gone. Say the literal thing.
   - Before: The timeout is a dial worth turning.
   - After: Raising the timeout from 5 s to 20 s removed the failures on slow
     links.
4. **Unanchored claims.** No number, name, path, date, error text or honest
   negative; the root of 1 to 3. Add only anchors the source holds, or narrow
   the claim and flag the gap.
   - Before: The pipeline is fast and reliable.
   - After: The pipeline processed 12,000 records in 41 s; failure behaviour
     under a full disk is untested.
5. **Participle tails** (`, ensuring ...`, `, highlighting ...`). An effect
   asserted without evidence. Stop at the fact, or make the conclusion its own
   supported sentence.
   - Before: The loader skips bad rows, ensuring data quality.
   - After: The loader skips rows that fail validation and logs each one.
6. **Nominalisation and copula avoidance** (`conducts an evaluation of`,
   `serves as`). Use the plain verb, and `is` and `has`.
   - Before: The module serves as the entry point and provides a mechanism for
     the validation of input.
   - After: The module is the entry point and validates input.
7. **Recap and optimism closers** (`In summary`, `going forward`). End on the
   decision, risk, number or next action.
   - Before: In summary, these steps ensure a smooth migration going forward.
   - After: Run the migration on staging first; production waits on the
     backfill count matching.
8. **Structure misused.** Headings on short texts, bold-lead bullet lists,
   bullets where an argument needs prose. Prose for reasoning, lists for
   parallel items, headings only to navigate.
   - Before: bullets reading `Speed: high`, `Reliability: high`.
   - After: It processed 12,000 records in 41 s on one core; cost wasn't
     measured.
9. **Rhythm and density.** Uniform lengths, stacked `Moreover` and
   `Additionally`; or long dense sentences with no paragraph breaks. Let the
   real relation link sentences (`However`, `Therefore`), drop transitions
   that announce structure, put the key point in the shortest sentence, and
   split any sentence carrying a method, a result and a caveat.
   - Before: The job failed. Moreover, the retry also failed. Additionally,
     the queue filled.
   - After: The job failed and so did the retry, so the queue filled.
10. **Habitual triples and hedge stacks** (`may potentially`, `experts argue`
    with no source). Use the real count and one qualifier; name the source or
    drop the claim. Don't hedge strong evidence.
    - Before: The fix improves speed, stability and maintainability.
    - After: The fix removes the second query and halves latency; stability
      and maintainability weren't measured.
11. **Chat residue and sycophancy.** Validation openers, offers of more help,
    restating the prompt. Start with the content; state disagreement plainly.
    - Before: Great question! Let me walk you through it.
    - After: Use a lock file; the cache has no owner field.

Not mechanisms: em-dash density, `delve`-class vocabulary and single banned
words turn over with each model release. Don't hunt them by list; fix the
claim under them. Mechanisms 1, 2, 3, 5 and 11 persist in current models;
heavy bullets in short chat answers have largely faded.
