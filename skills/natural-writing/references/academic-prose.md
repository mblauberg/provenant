# Academic prose

Load for a thesis chapter, paper, abstract, literature review or response to
reviewers. It's the academic overlay on the hub: apply
[mechanisms.md](mechanisms.md), [au-english.md](au-english.md),
[condense-pass.md](condense-pass.md) and
[claim-discipline.md](claim-discipline.md) first. LaTeX command and layout
invariants live in `engineering-writing`.

Venue, institution, supervisor style, glossary and citation conventions take
precedence. Don't restructure a work, change a bibliography or replace
citations without explicit authority. Source discovery, integrity
adjudication and assessment-permission advice are out of scope; AI-use
disclosure wording is in scope, the policy is not.

## Register

Coursework and reports take the skill's default voice: plain, mildly casual,
anchored. The formal register here is for examination-grade theses, journal
submissions and venues that ask for it: calm, exact, authored, no
contractions. Neither should sound like marketing, a tutorial or a chatbot.

- Prefer: The evaluation uses records from two sites. The findings therefore do
  not establish performance at unobserved sites.
- Avoid: This robust and innovative framework seamlessly delivers insights
  across the broader research landscape.

Replace prestige words with the measurement or mechanism: `robust results`
becomes the metric and interval; `seamless integration` becomes the interface.

**Spelling.** Australian English as in [au-english.md](au-english.md): `per
cent` in running prose, `%` in tables, equations and captions; foreign
spelling kept in titles, quotes, identifiers and citation keys; sentence-case
headings, with initial capitals only for formal names.

**Tense.** Present for established knowledge and system properties; past for
completed experiments; present perfect for prior work that stays relevant;
future only for actual future work, never to hide missing results (see the
implicit-completion trap in [claim-discipline.md](claim-discipline.md)).

**Voice.** Choose by clarity: active when the actor matters, passive when the
artefact matters, first person only where the venue expects it. Vary rhythm
with meaning, since a short sentence after a long one lands the point. Don't
add fragments or roughness to imitate a human.

## Citations

- Preserve every `\cite{...}` exactly and invent no key. If a claim lacks
  support, add `[FLAG: cite source]`.
- A citation supports a claim, not a sentence's decoration. Weak: `Forecast
  accuracy matters \cite{a,b,c}.` Better: `Prior work scores short-term
  forecasts mostly by aggregate error, and few studies report calibration in
  low-volume regions \cite{a,b,c}.`
- Put the citation next to its claim and don't use a key as a noun. Write
  `Prior work ... \cite{key}`, not `\cite{key} shows ...`, unless the author's
  identity carries the contrast. In IEEE style write `in [1]`, not `in
  reference [1]`.
- Five sources cited together need a sentence that explains the shared claim.

## Evidence

Classes, wording, results and superiority claims, pilot evidence and
limitations follow [claim-discipline.md](claim-discipline.md). Two areas are
specific to academic work:

- **Small samples.** Follow the approved analysis plan; don't add a test or
  threshold while polishing. Name the interval or test actually used, pair
  significance with effect size, and read a p-value as compatibility.
- **Reproducibility.** Artifacts available, results reproduced and results
  replicated are different claims. Claim only what happened.

## Academic tells

- **Noun stacking** (`a class-specific generator prompt stack`). Fix each
  recurring tower once and reuse that phrasing.
- **Repo jargon** drafted in from a codebase: `gate`, `lane`, `shard`,
  `pipeline run`, status enums, ticket numbers. Name the concept (`no
  release-blocking check was enforced`), unless the work defines the term as a
  method step. Commands, paths and regexes belong in a footnote or appendix.
- **Reviewer replies.** Drop `You are absolutely right`; answer the point.

## Structure and process

- **Sections.** An abstract states problem, method, result boundary and
  limitation. A literature review synthesises rather than catalogues. Captions
  state what is shown, its unit, claim status and caveat. Extended
  implementation detail goes to appendices.
- **Full chapter or work.** Lock sources, claims, citations and validation
  commands first. Review chapters independently for unsupported claims,
  citation misuse and markup breakage, then review the whole for contribution
  logic. Preserve labels, macros, result commands, keys, equations and paths
  unless a structural change is authorised. Run the citation, style and build
  checks and report any skipped.
- **Condensing.** Count prose words with macros and labels excluded, lock every
  `\label`, key and result macro, and verify each survives by grep.

## Check

`provenant check writing path/to/file.tex --rules academic` is a review prompt.
Its silence is necessary, not sufficient: it can't see flat rhythm,
comma-gloss definitions or implicit-completion tense.

Final scrub: Australian spelling; no em-dashes or prose `---`; no `TODO`, `TBD`
or `insert result`; no guessed value in place of a result macro; no conclusion
beyond the evidence. Then ask once what still reads as machine-written, and
revise only if the answer names a fixable tell.
