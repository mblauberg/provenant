# Academic prose and LaTeX

For a thesis chapter, paper, abstract, literature review or reviewer reply.
Venue, institution, supervisor style, glossary and citation conventions take
precedence. Don't restructure a work, change a bibliography or replace
citations without explicit authority. Diagnosis is read-only; edit only
assigned files.

## Register

Coursework and reports take the default voice. Examination-grade theses,
journal submissions and venues that ask for it take a calm, exact, authored
register with no contractions and no marketing, tutorial or chatbot tone.

- Prefer: The evaluation uses records from two sites. The findings therefore
  do not establish performance at unobserved sites.
- Avoid: This robust and innovative framework seamlessly delivers insights
  across the broader research landscape.

Replace prestige words with the measurement or mechanism: `robust results`
becomes the metric and interval; `seamless integration` becomes the interface.

**Tense.** Present for established knowledge and system properties; past for
completed experiments; present perfect for prior work that stays relevant;
future only for real future work, never to hide missing results.
**Voice.** Active when the actor matters, passive when the artefact matters.
Don't add fragments or roughness to imitate a human.

## Citations and evidence

- Preserve every `\cite{...}` exactly and invent no key. An unsupported claim
  gets `[FLAG: cite source]`.
- A citation supports a claim, not decoration. Put it beside its claim; write
  `Prior work scores short-term forecasts by aggregate error \cite{key}`, not
  `\cite{key} shows ...`, unless the author's identity carries the contrast.
  Five sources cited together need a sentence explaining the shared claim.
- Small samples: follow the approved analysis plan and add no test or
  threshold while polishing. Name the interval or test used and pair
  significance with effect size.
- Artifacts available, results reproduced and results replicated are different
  claims; claim only what happened. Claim classes are in
  [claims.md](claims.md).

## Academic tells

- **Noun stacking** (`a class-specific generator prompt stack`): fix each
  recurring tower once and reuse the phrasing.
- **Repo jargon** drafted in from a codebase (`gate`, `lane`, `shard`, status
  enums, ticket numbers): name the concept, unless the work defines it as a
  method step. Commands and paths go in a footnote or appendix.
- **Reviewer replies:** drop `You are absolutely right`; answer the point.

## Structure

An abstract states problem, method, result boundary and limitation. A
literature review synthesises rather than catalogues. Captions state what is
shown, its unit, claim status and caveat. For a whole chapter, lock sources,
claims, citations and validation commands first; review chapters for
unsupported claims, citation misuse and markup breakage, then the whole for
contribution logic. To condense, count prose words with macros and labels
excluded and verify by grep that every `\label`, key and result macro
survives ([claims.md](claims.md)).

## LaTeX invariants

Preserve exactly unless told otherwise: `\cite`, `\Cref`, `\cref`, `\ref`,
`\autoref`, `\label`, project-defined command arguments (result macros
included), equations, symbols, table alignment, figure paths, and
bibliography, glossary and acronym commands.

- A macro argument can be a contract with generated artefacts. Never rewrite
  one for style, replace a result macro with a guessed value or hide an
  unresolved token. Keep the existing `\Cref`/`\cref` split. Flag unclear
  notation rather than changing symbols.
- Hazards: deleting braces around macros, changing `_` in labels, dropping the
  non-breaking `~` before `\ref`, `\cite` and `\eqref`, leaving `%`, `_`, `&`
  or `#` unescaped in prose, and shifting table alignment. Keep a
  non-breaking space between a number and its unit (`\cref` supplies its own).
- LaTeX `---` in prose is a punctuation defect: repair the sentence.
- Read surrounding paragraphs, keep comments and fences, make the smallest
  coherent edit, then re-read for markup and claim drift.

## Check

The academic rule set in `provenant check writing --help` is a review prompt; it can't
see flat rhythm, comma-gloss definitions or implicit-completion tense. Final
scrub: Australian spelling; no em dashes or prose `---`; no `TODO`, `TBD` or
`insert result`; no conclusion beyond the evidence. Then ask once what still
reads as machine-written, and revise only if the answer names a fixable tell.
