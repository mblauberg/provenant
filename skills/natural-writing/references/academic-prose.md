# Academic Prose

Use this reference when the prose is scholarly: a thesis chapter, a journal
paper, an abstract, a literature review, a response to reviewers, or any
citation-heavy argument. It is the academic overlay on this skill's hub
doctrine, so load the hub material first ([au-english.md](au-english.md),
[anti-ai-taxonomy.md](anti-ai-taxonomy.md), [condense-pass.md](condense-pass.md)
and [claim-discipline.md](claim-discipline.md)) and add only what follows. For
LaTeX invariants, chapter structure and whole-thesis review, use the
`engineering-writing` skill's academic artefacts reference.

The target venue, institution, supervisor-approved style, project glossary and
local citation conventions take precedence over these defaults. Do not
restructure a work, change a bibliography or replace citations without explicit
authority. Source discovery, bibliography management, integrity adjudication and
assessment-permission advice are out of scope. AI-use disclosure wording and
placement are in scope; permission and policy are not.

## Register

Scholarly prose should be formal enough for examination and direct enough to
read easily. It should not sound like marketing copy, a tutorial, a grant pitch
or a chatbot answer. The ideal register is calm, exact and authored.

Prefer:

```text
The evaluation uses records collected at two sites. The findings therefore do not establish performance at unobserved sites.
```

Avoid:

```text
This robust and innovative evaluation framework seamlessly ensures that the system delivers meaningful insights across the broader research landscape.
```

### Academic exceptions to the hub Australian English default

- Use `per cent` in running prose even where the value is a compact metric
  expression; use `%` in tables, equations, metric lists and captions. The
  Australian Government Style Manual's digital edition now prefers `%` with a
  numeral everywhere, but a thesis is academic writing, not government web copy,
  so `per cent` in running prose stays the academic convention: pick one and
  hold it.
- Preserve US or other spelling inside a title, quoted text, code identifier,
  package name, API field, citation key or filename. Citation keys are the one
  addition to the hub's preserve-list that matters most in thesis work.
- Use minimal capitalisation: sentence case for headings, initial capitals
  reserved for formal names and titles, lower case for generic references (the
  model, the corpus, distributed learning).

### Tense

- Present tense for established knowledge, thesis structure, equations and
  system properties: `The validator rejects malformed records.`
- Past tense for completed experiments: `The model was trained on the
  development split.`
- Present perfect for work that remains relevant: `Prior work has treated the
  task as supervised classification.`
- Future tense for actual future work only. Never use it to hide missing
  results.

### Person and voice

Most research theses use impersonal phrasing, but passive voice is not
mandatory. Choose by clarity: active when the actor matters (`The evaluation
script rejects malformed JSON`), passive when the artefact or result matters
(`Predictions were filtered before aggregation`), first person only where the
thesis, school style or supervisor expects it. Passive-heavy prose hides
causality and creates stale rhythm.

### Paragraph and sentence rhythm

A good academic paragraph usually carries a topic sentence with the point, then
evidence, mechanism, comparison or method detail, then a short consequence,
caveat or link forward. Avoid uniform paragraph architecture across a chapter:
not every paragraph needs three sentences, a citation in the same position or a
concluding phrase.

Apply a concrete test to any multi-sentence passage:

- Include at least one short sentence, roughly under twelve words, to break the
  metre and land a key point. A short sentence after a long one is the cheapest
  effective fix.
- Vary structure, not only length: do not run more than two or three
  consecutive subject-first declaratives.
- If every sentence has the same shape and weight, the passage reads as
  machine-balanced even when each sentence is correct.

The variation must carry meaning. Do not add fragments, roughness or filler to
imitate or evade a purported authorship signature.

### Restraint

| Avoid | Prefer |
| --- | --- |
| `groundbreaking contribution` | `contribution` or the specific contribution |
| `pivotal framework` | `framework` |
| `robust results` | the actual metric, confidence interval or validation gate |
| `seamless integration` | the mechanism or interface |
| `highly effective` | the measured effect |

Use strong language only when the evidence is strong and specific. Follow the
target style on the serial comma; where it is optional, add it only when it
removes ambiguity.

## Citation discipline

Never invent citation keys. Preserve existing `\cite{...}` keys exactly unless
the user explicitly asks to replace them with known valid keys. If a claim needs
support and no citation is present, add `[FLAG: cite source]` rather than
fabricating a likely key, and never rename a key for style.

Citations should support claims, not decorate sentences.

Weak:

```text
Forecast accuracy matters in operational planning \cite{a,b,c}.
```

Better:

```text
Prior work evaluates short-term demand forecasts mainly with aggregate error metrics, while fewer studies report calibration across low-volume regions \cite{a,b,c}.
```

Use citations for factual background, definitions, prior methods, empirical
claims, comparisons, evaluation conventions and limitation statements. Avoid
citation dumping: if five sources are cited together, the sentence should
explain the shared claim.

For numbered (IEEE-style) bibliographies:

- Keep the citation close to the claim it supports.
- Do not use a citation key as a noun in final prose.
- Prefer `Prior work ... \cite{key}` over `\cite{key} shows ...` unless author
  identity matters; use author names when the argumentative contrast depends on
  them.
- Per the IEEE Editorial Style Manual, write `in [1]`, not `in reference [1]`,
  and do not make a bracketed number the grammatical subject or carry an
  author's name inline (`In Patel [1] ...`).
- Keep reference order stable by preserving existing source order where
  possible.

## Claim discipline and evidence

Use the hub claim schema, then add the empirical-research specifics.

**Results claims.** State what a table reports before interpreting it.

Good:

```text
\Cref{tab:primary-results} reports mean absolute error on the held-out dataset. The comparison remains provisional until the planned paired analysis is complete.
```

Weak:

```text
\Cref{tab:rq1-accuracy} demonstrates the superiority of the proposed framework.
```

Avoid `superior` unless the metric, comparator, confidence interval and
evaluation scope support it.

**Small-sample evidence.** Follow the project's approved statistical analysis
plan; do not introduce a bootstrap, permutation test or significance threshold
while polishing prose. With few independent observations, state the uncertainty
and avoid a strong comparative claim unless the declared method supports it.
Name the interval or test actually used rather than writing only `95 per cent
interval` or `significant`. Do not report a bare `statistically significant` as
a pass or fail verdict: pair significance wording with the effect size and its
interval, and read a p-value as compatibility, not a gate. Statistical
significance is not practical significance.

**Reproducibility.** Distinguish Artifacts Available (deposited and citable)
from Results Reproduced (others rerun the work using the author's artefacts) and
Results Replicated (others obtain the results without those artefacts). Claim
only that artefacts are available unless independent reproduction has actually
happened: reproducible does not mean reproduced.

**Claim wording by evidence strength.**

| Evidence strength | Good wording |
| --- | --- |
| direct measurement | `measured`, `observed`, `recorded` |
| supported inference | `suggests`, `is consistent with`, `indicates` |
| protocol guarantee | `requires`, `rejects`, `enforces` |
| implementation fact | `implements`, `loads`, `exports`, `validates` |
| limitation | `does not measure`, `does not establish`, `remains untested` |
| future work | `is left for future work`, `requires separate evaluation` |

Never promote `suggests` to `proves`, `supports` to `confirms`, `pilot` to
`final evidence`, or a specified extension to a completed result.

## Academic AI tells

The hub taxonomy applies unchanged. These are the thesis-specific additions.

**Noun stacking.** Thesis-specific examples: `a class-specific generator prompt
stack`, `an author-process register fingerprint`, `corpus-attrition forensics`.
Fix each recurring tower once and reuse one phrasing throughout; restacking the
same defined term differently on every mention compounds the load. Keep defined
terms and honest negatives intact while lightening the prose around them.

**Repo and implementation jargon.** Tooling vocabulary leaks into academic prose
when the writing is drafted from a codebase, and it reads as internal process
language rather than scholarship.

- `audit` for an analysis is repo language: rename the analytical act to
  `analysis` and pipeline gate names to `check` or `review`. Keep an explicitly
  defined term unchanged, such as a named `0.75 audit ceiling` threshold.
- Other common leaks: `gate`, `lane`, `shard`, `pipeline run`, `hard block
  fired`, `flag set`, status enums, file and function names, and ticket or ADR
  numbers in body prose. Name the concept, not the mechanism: `no
  release-blocking check was enforced`, not `no hard block fired`. A gate the
  work has explicitly defined as a method step stays; only the build-process
  sense is jargon.
- Instructional register (`run the gate`, `promote the artefact`, `the manifest
  pins`), state-machine register (`stood down` for `was not run`), internal
  component handles, and literal regexes, paths or CLI commands in running prose
  all belong in a footnote, appendix or methods sentence, not the argument.

**Reviewer and viva dialogue.** In a response to reviewers or viva preparation,
drop generic flattery openers such as `You are absolutely right` and `Great
question`. Answer the substantive point directly.

## Modes

The hub modes apply. Academic work adds these variants.

- `citation-safe`: for literature reviews, background and citation-heavy
  discussion. Preserve keys, check each citation supports the nearest claim,
  replace source-catalogue paragraphs with synthesis, flag unsupported claims,
  keep author names only where they matter.
- `section-polish`: map each paragraph to one function, remove duplicate
  functions, check transitions express logic rather than decoration,
  standardise terminology, move implementation detail to the right level, and
  keep headings and document structure unless asked to change them.
- `final-scrub`: check Australian spelling; no em dashes or prose `---`; no
  `TODO`, `TBD` or `insert result` placeholders; no unresolved citation
  placeholder; no result macro converted into a guessed value; no empty
  signposting; no conclusion overclaiming beyond evidence. Then run one
  adversarial self-audit: what still makes this read as AI-generated? Revise
  only if the answer names a real, fixable tell; if the honest answer is
  `nothing specific`, stop.

Diagnosis is read-only, and prioritises unsupported claims, unsafe citations,
pending evidence presented as result, markup corruption risk, generic prose and
comprehension-blocking density, in that order.

## Condensing academic prose

Follow the hub condense procedure and stop rule. Two additions:

- The checker's `--wordcount` mode ignores macros, labels and math when counting
  prose words, so the reported count matches what an examiner reads rather than
  the source markup. Report words before and after plus the percentage cut.
- Lock every referenced `\label`, every citation key, every result macro and
  every cross-reference target before condensing, and verify each survives in
  its new home by grep, not by re-reading for a feeling of completeness.

Split a sentence when it carries a method, a result, a caveat, a future-work
boundary, or two or more cross-references:

Before:

```text
The study evaluates the primary and comparator methods on two datasets with five paired runs and confidence intervals, while pilot observations are retained only as protocol-refinement evidence and do not support the final inference.
```

After:

```text
The study evaluates the primary and comparator methods on two datasets. Comparisons use five paired runs and confidence intervals. Pilot observations support protocol refinement only, not the final inference.
```

## Checker

For local files, use the checker as a review prompt, not proof:

```sh
python3 "$(provenant root)/skills/natural-writing/scripts/check_academic_style.py" path/to/file.tex
```

Its silence is necessary, not sufficient: it scans fixed patterns and cannot see
flat rhythm, comma-gloss definitions or implicit-completion tense.

## Checklist

- Australian English mechanics from the hub applied, with the academic
  exceptions above.
- No em dashes and no prose `---`.
- No generic throat-clearing, inflated novelty or impact language.
- Every citation key preserved or explicitly verified; unsupported claims
  flagged.
- Citations sit near the claim they support; no citation dumping.
- Small-sample uncertainty stated rather than implied.
- Reproducibility claims scoped to what actually happened.
- Every sentence carries a claim, method, evidence, limitation or necessary
  transition, and the passage has at least one short sentence.
