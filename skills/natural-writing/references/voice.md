# Default voice

The house register for anything this skill writes or rewrites: email, notes,
documents, reports and general prose. `engineering-writing` applies it at a
lighter dose. Formal registers are opt-in exceptions (end of this file), and a
supplied voice sample outranks all of it.

Reason: polished prose that never varies now reads as machine-made, and
readers strain to unpack it. A plain, mildly casual voice that stays exact
reads as written by someone and is faster to take in. Mainstream technical
style guides ask for the same thing: the Microsoft Writing Style Guide says to
use common contractions for a friendly tone
(<https://learn.microsoft.com/en-us/style-guide/word-choice/use-contractions>),
and Google's developer documentation style guide recommends common two-word
contractions, negations especially
(<https://developers.google.com/style/contractions>). Both retrieved
2026-10-30.

## Voice

- Mostly declarative. In reports and documents, stay impersonal or use first
  person only for actions actually taken (`I ran…`, `I split MR1…`), never for
  opinions or feelings. In correspondence, write as the sender.
- Confident where the evidence is direct, with no stacked hedges. Qualify only
  where the evidence is genuinely limited: `A pass only shows the ranking is
  order-independent, not that it's correct.`
- Contractions (`doesn't`, `can't`, `isn't`, `there's`, `that's`): about one
  every few paragraphs in documents, freely in email and chat.
- Mildly casual verbs where they're clearer than the formal one: `get thrown
  out`, `tell them apart`, `just lengthen the queue`, `drag it over 1 s`.

## Sentences

- Anchor every claim to something concrete: a number, seed, line, method name,
  date or test result. No vague praise and no `this is important`.
- Mix lengths. Put a short verdict next to a longer sentence that chains the
  reasoning with `so`, `because` or `which is why`.
- Show the causal chain inside one sentence, as observation, then reason, then
  consequence: `Each job scores 10, so the sort can only tell them apart by ID.`
- Keep the one number a claim rests on, even when a table also holds it, but
  don't recite the table row. A redundancy pass that strips that number leaves
  the claim unanchored.

## Structure

- In longer documents, bold run-in labels open paragraphs instead of extra
  headings. Use a full stop when several sentences follow (`MR1.`), and a colon
  with a lowercase continuation for a short answer (`Requirement: not
  satisfied.`, `20 users: all 8 threads busy`). Label paragraphs, not every
  bullet.
- Tables for anything parallel, prose for reasoning, bullets only for
  genuinely parallel items.
- Don't restate in prose what a table shows, explain the obvious, or write a
  caption that repeats the heading. Cut any sentence the reader could infer.

## Imperfection, bounded

The human texture is in rhythm and register: an uneven sentence, a
contraction, a plain verb, a short aside. It never comes from planted typos,
deliberate errors, vagueness, slang for effect or invented detail.

## Example

> Line coverage was 8/8 in both runs, yet all seven survivors were on the
> checks in lines 6, 9 and 12 of classify(). The baseline only uses scores 60,
> 75, 90 and −1, so none of its tests lands on a threshold.

## Formal exceptions

Switch to a formal register only when the context needs it. Drop the
contractions and casual verbs, and keep the anchors and plain structure.

- Legal drafting belongs to `legal-writing`, which never casualises court
  prose.
- Examination-grade theses, journal submissions, and any venue, institution or
  recipient whose style asks for formality take the register in
  [academic-prose.md](academic-prose.md#register).
- When a voice sample is supplied, match it
  ([mechanisms.md](mechanisms.md#voice-matching)).
