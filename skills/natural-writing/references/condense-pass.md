# Condense pass

`natural-writing` owns this procedure for the writing family.
`engineering-writing` and `legal-writing` link here for the shared steps and
add only their own domain lock-list and stop conditions (commands, flags and
version numbers; source anchors, exhibit labels and forum wording). The
academic lock-list (LaTeX labels, citation keys, result macros) is in
[academic-prose.md](academic-prose.md).

Use this when prose is over-long and the job is to shorten it while keeping
the meaning; a rewrite improves a passage in place, a condense makes it
shorter. Agree any target with the user or project; do not force a
percentage cut when the source is already tight.

## Procedure

1. **Measure.** Count the prose words (ignore macros, labels, code fences and
   frontmatter). Record the baseline so the delta can be reported.
2. **Lock invariants.** Facts, numbers, units, identifiers, commands, flags,
   dates, citations, defined terms, obligations, and any domain-specific
   anchor the owning skill's overlay names.
3. **Reverse-outline.** Write one phrase per paragraph stating its single
   point (the Paragraph Test below is the per-paragraph form). A paragraph
   that resists a one-line summary, or repeats another line, is the cut or
   merge target.
4. **De-duplicate to one home.** State each fact, caveat, definition,
   decision-rule, and number once, in its primary section. Replace every
   other copy with a short cross-reference. Relocate detail to an appendix
   or table rather than delete it: moved is not lost.
5. **Cut fluff.** Throat-clearing, decorative tails, absence tombstones, and
   long hypothetical passages that re-explain a point already made.
   Throat-clearing to cut on sight: `This section provides an overview`,
   `It is worth noting`, `As previously mentioned`, `In conclusion`,
   `Overall`. Replace vague signposting with a specific pointer, not a
   decorative one.
6. **Narrow, do not soften.** Replace a sweeping claim with the precise claim
   the evidence supports; it is shorter and safer. Delete an unbacked
   presumption outright. Never soften an honest negative.

**Stop rule.** Stop the moment a further cut would remove a fact, number,
unit, caveat, scope condition, honest negative, or defined term, break a
cross-reference, or force awkward phrasing. When unsure whether something is
load-bearing, keep it, and prefer compressing to deleting whenever a fact is
involved. If the prose is already tight, say so and stop. A forced cut that
sounds strange is a failure, not a win.

**Report the delta.** State words before and after, the percentage, and that
every locked invariant survived.

## Paragraph test

For each paragraph, ask:

1. What is the main point?
2. What evidence, method, or reasoning supports it?
3. What limitation or consequence matters?
4. Which sentence does not serve those functions?
5. Can the paragraph start closer to the point?

If a paragraph has two unrelated claims, split it. If it repeats the same
claim with different words, cut the weaker sentence.

## Condense integrity

A substantial condense, rewrite, or relocation is only safe if what survives
is verified; a drafting model's own report that nothing was dropped is
untrustworthy, and lossy passes have silently cut qualifiers, numbers, and
cross-references while reporting a clean pass.

- Gate the pass with a deterministic token check: set-diff the numbers,
  identifiers, commands, dates, and defined terms between the before and
  after texts. Zero unexplained loss is the pass condition.
- The token check sees tokens, not meaning. Follow it with an independent
  qualitative pass: is any condition, caveat, obligation, or honest negative
  now weaker, unowned, or unfindable?
- Relocation can weaken as well as preserve: a constraint moved too far from
  the point that depends on it can stop doing its job. After relocating,
  re-read the passage that relied on the moved material and confirm it still
  lands, with a pointer to the new home.
- Never patch a gap the checks expose by re-deriving content from memory;
  restore it from the before-text.

## Sentence repair

The principle is Strunk's Rule 13, omit needless words: a sentence should
contain no unnecessary words, as a drawing has no unnecessary lines.

| Pattern | Repair |
| --- | --- |
| `It is important to note that X` | `X` |
| `In order to X` | `To X` |
| `The fact that X` | `X` or a noun phrase |
| `There are several factors that` | name the factors |
| `An evaluation of X was conducted` | `X was evaluated` |
| `This provides support for` | `This supports` |
| `the question as to whether` | `whether` |
| `is able to` / `has the ability to` | `can`, unless capacity is the point |

## Positive form

This is Strunk's Rule 11, put statements in positive form. `Not` is weak as
mere evasion (`not honest` for `dishonest`), but other negatives are strong:
`No subgroup estimate is reported` is direct and correct where the absence
is the claim. Positive form is a style rule, not a licence to spin; see
Positive form is not positive spin in
[anti-ai-taxonomy.md](anti-ai-taxonomy.md) for the integrity guard.

Do not force positive form when the absence is the claim.

## Emphatic position and transitions

Place the most important word or idea at the end of the sentence (Strunk,
Rule 18); the start carries the next-strongest emphasis. End sections on the
decision, risk, next action, or remaining uncertainty, not a generic
conclusion. Use transitions to express logic, not decoration: `However` for
real contrast, `Therefore` for a conclusion that follows, `In contrast` when
two things are directly compared. Avoid transitions that only announce
structure (`Having established this`, `Moving forward`).

The stop rule and the verify-and-report step above are the concision
checklist; do not maintain a second one.
