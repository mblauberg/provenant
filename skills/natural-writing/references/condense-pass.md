# Condense pass

Use this when prose is over-long and the job is to shorten it while keeping
the meaning. A rewrite improves a passage in place; a condense makes it
shorter. Agree any target with the user or project, and don't force a
percentage cut when the source is already tight.

`natural-writing` owns this procedure for the writing family.
`engineering-writing` and `legal-writing` link here for the shared steps and
add only their own domain lock-list and stop conditions (commands, flags and
version numbers; source anchors, exhibit labels and forum wording). The
academic lock-list (LaTeX labels, citation keys, result macros) is in
[academic-prose.md](academic-prose.md).

## Procedure

1. **Measure.** Count the prose words (ignore macros, labels, code fences and
   frontmatter). Record the baseline so the delta can be reported.
2. **Lock invariants.** Facts, numbers, units, identifiers, commands, flags,
   dates, citations, defined terms, obligations, and any domain-specific
   anchor the owning skill's overlay names.
3. **Reverse-outline.** Write one phrase per paragraph stating its single
   point (the [paragraph test](#paragraph-test) is the per-paragraph form). A
   paragraph that resists a one-line summary, or repeats another line, is the
   cut or merge target.
4. **De-duplicate to one home.** State each fact, caveat, definition,
   decision rule and number once, in its primary section. Replace every other
   copy with a short cross-reference. Move detail to an appendix or table
   rather than delete it: moved is not lost.
5. **Cut fluff.** Throat-clearing, decorative tails, absence tombstones, and
   long hypotheticals that re-explain a point already made. Cut on sight:
   `This section provides an overview`, `It is worth noting`, `As previously
   mentioned`, and the recap closers `In conclusion` and `Overall`
   ([mechanisms.md](mechanisms.md#7-recap-and-optimism-closers)). Replace
   vague signposting with a specific pointer.
6. **Narrow, don't soften.** Replace a sweeping claim with the precise claim
   the evidence supports; it's shorter and safer
   ([mechanisms.md](mechanisms.md#4-unanchored-claims)). Delete an unbacked
   presumption outright. Never soften an honest negative.

## Stop rule

Stop the moment a further cut would remove a fact, number, unit, caveat,
scope condition, honest negative or defined term, break a cross-reference, or
force awkward phrasing. When unsure whether something is load-bearing, keep
it, and prefer compressing to deleting whenever a fact is involved. If the prose
is already tight, say so and stop. A forced cut that sounds strange is a
failure, not a win.

The stop rule and the verification below are the whole concision checklist;
don't keep a second one.

## Verify and report

A substantial condense, rewrite or relocation is only safe once what survives
is checked. A drafting model's own report that nothing was dropped can't be
trusted: lossy passes have silently cut qualifiers, numbers and
cross-references while reporting a clean pass.

- **Token check.** Set-diff the numbers, identifiers, commands, dates and
  defined terms between the before and after texts. Zero unexplained loss is
  the pass condition. For number-heavy documents, the persistent script in
  [claim-discipline.md](claim-discipline.md#mechanised-audit) is the stronger
  form.
- **Qualitative pass.** The token check sees tokens, not meaning. Follow it
  with an independent read: is any condition, caveat, obligation or honest
  negative now weaker, unowned or unfindable?
- **Relocation.** A constraint moved too far from the point that depends on
  it can stop doing its job. After relocating, re-read the passage that
  relied on the moved material, confirm it still lands, and leave a pointer
  to the new home.
- **Repairs.** Never patch a gap the checks expose by re-deriving content
  from memory; restore it from the before-text.
- **Report the delta.** State words before and after, the percentage, and
  that every locked invariant survived.

## Techniques

### Paragraph test

For each paragraph, ask:

1. What is the main point?
2. What evidence, method or reasoning supports it?
3. What limitation or consequence matters?
4. Which sentence does not serve those functions?
5. Can the paragraph start closer to the point?

If a paragraph has two unrelated claims, split it. If it repeats the same
claim in different words, cut the weaker sentence.

### Sentence repair

The principle is Strunk's Rule 13, omit needless words: a sentence should
contain no unnecessary words, as a drawing has no unnecessary lines. The
nominalised patterns are mechanism 6
([mechanisms.md](mechanisms.md#6-nominalisation-and-copula-avoidance)).

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

### Positive form

Strunk's Rule 11: put statements in positive form. `Not` is weak as mere
evasion (`not honest` for `dishonest`), but other negatives are strong. `No
subgroup estimate is reported` is direct and correct where the absence is the
claim, so don't force positive form there. It's a style rule, not a licence
to spin.

### Emphatic position

Place the most important word or idea at the end of the sentence (Strunk,
Rule 18); the start carries the next-strongest emphasis. End sections on the
decision or next action
([mechanisms.md](mechanisms.md#7-recap-and-optimism-closers)), and use
transitions for logic
([mechanisms.md](mechanisms.md#9-rhythm-and-density)).
