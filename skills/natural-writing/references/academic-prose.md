# Academic prose

Load for a thesis chapter, paper, abstract, literature review or response to
reviewers. It is the academic overlay on the hub: apply
[mechanisms.md](mechanisms.md), [au-english.md](au-english.md),
[condense-pass.md](condense-pass.md) and
[claim-discipline.md](claim-discipline.md) first. LaTeX command and layout
invariants are in `engineering-writing`.

Venue, institution, supervisor style, glossary and citation conventions take
precedence. Do not restructure a work, change a bibliography or replace
citations without explicit authority. Source discovery, integrity adjudication
and assessment-permission advice are out of scope; AI-use disclosure wording is
in scope, the policy is not.

## Register

Formal enough for examination, direct enough to read: calm, exact, authored. It
should not sound like marketing, a tutorial or a chatbot.

- Prefer: The evaluation uses records from two sites. The findings therefore do
  not establish performance at unobserved sites.
- Avoid: This robust and innovative framework seamlessly delivers insights
  across the broader research landscape.

Replace prestige words with the measurement or mechanism (`robust results`
becomes the metric and interval; `seamless integration` becomes the interface).

Academic exceptions to the Australian English default:

- `per cent` in running prose, `%` in tables, equations and captions. Pick one
  and hold it.
- Preserve foreign spelling inside titles, quotes, identifiers and citation
  keys.
- Sentence-case headings; initial capitals only for formal names.

Tense: present for established knowledge and system properties; past for
completed experiments; present perfect for prior work that stays relevant;
future only for actual future work, never to hide missing results.

Voice: choose by clarity. Active when the actor matters, passive when the
artefact matters, first person only where the venue expects it. Vary rhythm
with meaning: a short sentence after a long one lands the point. Do not add
fragments or roughness to imitate a human.

## Citations

- Never invent a key. Preserve every `\cite{...}` exactly; if a claim lacks
  support, add `[FLAG: cite source]`.
- A citation supports a claim, not a sentence's decoration. Weak:
  `Forecast accuracy matters \cite{a,b,c}.` Better: `Prior work scores
  short-term forecasts mostly by aggregate error, and few studies report
  calibration in low-volume regions \cite{a,b,c}.`
- Keep the citation next to its claim. Do not use a key as a noun. Write
  `Prior work ... \cite{key}`, not `\cite{key} shows ...`, unless the author's
  identity carries the contrast. In IEEE style write `in [1]`, not `in
  reference [1]`.
- Five sources cited together need a sentence that explains the shared claim.

## Evidence

Use the hub claim classes, then:

- **Results.** State what the table reports before interpreting it. Avoid
  `superior` unless metric, comparator, interval and scope support it.
- **Small samples.** Follow the approved analysis plan; do not add a test or
  threshold while polishing. Name the interval or test actually used, pair
  significance with effect size, and read a p-value as compatibility.
- **Reproducibility.** Artifacts available, results reproduced and results
  replicated are different claims. Claim only what happened.
- Never promote `suggests` to `proves`, `pilot` to `final evidence`, or a
  specified extension to a completed result.

## Academic tells

- **Noun stacking** (`a class-specific generator prompt stack`). Fix each
  recurring tower once and reuse one phrasing.
- **Repo jargon** drafted in from a codebase: `gate`, `lane`, `shard`,
  `pipeline run`, status enums, ticket numbers. Name the concept (`no
  release-blocking check was enforced`), unless the work defines the term as a
  method step. Commands, paths and regexes belong in a footnote or appendix.
- **Reviewer replies.** Drop `You are absolutely right`; answer the point.

## Structure and process

An abstract states problem, method, result boundary and limitation. A literature
review synthesises rather than catalogues. Results report before interpretation.
Captions state what is shown, its unit, claim status and caveat. Extended
implementation detail goes to appendices.

For a full chapter or work, lock sources, claims, citations and validation
commands first. Review chapters independently for unsupported claims, citation
misuse and markup breakage, then review the whole for contribution logic.
Preserve labels, macros, result commands, keys, equations and paths unless a
structural change is authorised. Run the citation, style and build checks and
report any skipped.

For condensing, count prose words with macros and labels excluded, lock every
`\label`, key and result macro, and verify each survives by grep.

## Check

`provenant check writing path/to/file.tex --rules academic` is a review prompt.
Its silence is necessary, not sufficient: it cannot see flat rhythm,
comma-gloss definitions or implicit-completion tense.

Final scrub: Australian spelling; no em-dashes or prose `---`; no `TODO`, `TBD`
or `insert result`; no guessed value in place of a result macro; no conclusion
beyond the evidence. Then ask once what still reads as machine-written; revise
only if the answer names a fixable tell.
