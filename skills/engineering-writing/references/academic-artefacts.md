# Academic Artefacts

Use this reference for the artefact side of scholarly writing: thesis and paper
LaTeX, chapter and section structure, engineering research voice, and the review
process for a whole chapter or work. For academic register, citation discipline,
claim wording and the academic AI tells, load the `natural-writing` skill's
academic prose reference; this file does not repeat them.

Diagnosis is read-only. Edit only assigned files. Do not restructure a work,
change a bibliography or replace citations without explicit authority.

## LaTeX invariants

Preserve exactly unless explicitly asked otherwise:

- `\cite{...}`
- `\Cref{...}`, `\cref{...}`, `\ref{...}`, `\autoref{...}`
- `\label{...}`
- project-defined commands and their arguments, such as `\result{...}`
- equations and math environments
- table column alignment
- figure paths
- bibliography commands
- glossary and acronym commands

Do not rewrite a macro argument for style. A macro argument can be a contract
with generated artefacts.

### Cross-references

Keep references specific:

```text
\Cref{tab:primary-results} reports the primary outcome.
```

Avoid vague references such as `The table below highlights important results`.
When a paragraph references several figures or tables, give each reference a
distinct role. Use `\cref` mid-sentence and `\Cref` at the start of a sentence;
never open a sentence with a lowercase `\cref` or a bare bracketed number, and
preserve the existing `\Cref`/`\cref` split rather than flattening it.

### Equations and symbols

Do not change symbols while polishing prose. If notation is unclear, flag it
with `[FLAG: define symbol before use]`.

Good:

```text
Equation~\ref{eq:pattern-score} scores candidate pattern evidence for retrieval. It does not produce the final risk label.
```

Avoid `The equation elegantly captures the relationship.`

### Result macros

Treat generated result macros such as `\result{PRIMARY_METRIC}` as locked. Do
not replace them with guessed numbers and do not hide unresolved tokens. If the
prose overclaims a pending token, rewrite the claim as conditional or flag it.

```text
Values in \Cref{tab:primary-results} are populated by the project's verified results source and remain provisional until its declared checks pass.
```

### Common hazards

- deleting braces around macros
- changing `_` in macro names or labels
- replacing `~` in non-breaking references
- introducing unescaped `%`, `_`, `&` or `#`
- changing table alignment while editing prose
- converting LaTeX `---` into a visible style problem instead of repairing the
  sentence punctuation

Keep the non-breaking tilde before `\ref`, `\cite` and `\eqref`, and put a
non-breaking space between a number and its unit or percent sign (siunitx
`\num`/`\qty`/`\SI`, or a thin `\,`) so a value never splits across a line.
cleveref's `\cref`/`\Cref` already insert that space, so prefer them over a
manual `Fig.~\ref`.

### Markup-safe editing checklist

- Every citation key preserved or explicitly verified.
- LaTeX commands and arguments unchanged.
- Result macros not converted into numbers.
- Cross-references still point to the same artefact.
- Unsupported claims flagged rather than smoothed away.

## Research voice

The voice target is precise without being brittle, concise without dropping
caveats, technical without becoming a schema dump, restrained without becoming
vague. A reader should be able to tell what was built, what was measured, what
failed, what remains uncertain and why the result matters.

Prefer verbs that name the operation (`trained`, `evaluated`, `measured`,
`compared`, `sampled`, `filtered`, `validated`, `rejected`, `exported`,
`quantised`, `aggregated`, `aligned`, `bounded`, `failed`) over verbs that add
prestige without information (`showcases`, `highlights`, `underscores`,
`facilitates`, `leverages`, `drives`, `enhances`, or `demonstrates` where no
demonstration is described).

Use the lowest abstraction level that still fits the point.

Too abstract:

```text
The architecture improves scalability and reliability by supporting enhanced memory processing.
```

Better:

```text
The architecture separates recent-window, episodic, pattern, and pinned-evidence retrieval, so each memory source can be ablated without changing the evaluator.
```

Use one term for one concept. Before editing, fix the system name, model name,
data split, metric, artefact or manifest, evaluation scope and claim status,
then keep those names stable.

Keep implementation detail at the level that supports the argument. Move
file-level detail to appendices, provenance notes or implementation tables
unless the chapter is explicitly about software architecture.

Good sentence patterns put the load-bearing element last:

- `X is evaluated on Y because Z.`
- `X rejects Y before Z, which prevents W.`
- `The comparison isolates X by holding Y constant.`
- `This result is conditional on X and does not establish Y.`

Replace `This section explores ...`, `The results highlight the importance of
...`, `The system leverages a robust framework to ...` and `The findings
demonstrate the potential of ...` with the actual claim, method or limitation.

## Chapter and section patterns

**Abstract.** Problem and context; gap or limitation; method or contribution;
evaluation basis; main result or expected result boundary; limitation or
deployment implication if important. Roughly 200 to 300 words as a single
unstructured paragraph; engineering theses do not use headed abstracts. Quote a
headline result only once it is claimable (a populated `\result`, not a
placeholder). Avoid broad claims, citation clutter and implementation detail.

**Introduction.** Lead from problem to contribution: what problem matters, why
existing approaches are insufficient, what research questions are asked, what
the contribution is, how the work is structured. These moves map to Swales' CARS
pattern: establish the territory, establish the gap, occupy the gap. State
contributions as an enumerated list and give a roadmap of one or two sentences
per chapter. Do not open with generic societal commentary.

**Literature review.** Synthesise, not catalogue. Organise paragraphs by problem
framing, methodological family, dataset or evaluation limitation, architectural
choice, deployment constraint and research gap. Each paragraph should make a
synthesis claim and use sources as support.

Weak:

```text
Smith et al. did X. Jones et al. did Y. Lee et al. did Z.
```

Better:

```text
Prior short-term forecasting studies commonly report aggregate error. This supports benchmark comparison, but it can hide calibration differences across low-volume regions \cite{...}.
```

**Theory.** Define notation before use, keep symbols consistent, keep equations
close to the prose explaining their role, state assumptions explicitly, and do
not turn a design choice into a mathematical necessity.

**Methodology.** Reproducible and bounded: dataset construction or selection,
inclusion and exclusion rules, system components, evaluation scope, comparators,
metrics, statistical procedure, quality gates, deviation handling, compute and
environment with key hyperparameters, the random seed and run count, and a
pointer to a data and code availability statement. Map the protocol to a
recognised checklist (the NeurIPS paper checklist, or REFORMS for ML-based
science) and point the availability statement at a DOI-issuing archive such as
Zenodo or OSF with a tagged release and commit hash. If a result is not
claimable without an artefact, state the gate directly.

**Results.** Report before interpreting: scope and data included, primary metric
result, comparator or confidence interval, secondary metrics, caveat or gate
condition, minimal interpretation. Do not re-explain the method at length, claim
causality from descriptive results, read pilot artefacts as final evidence, or
hide missing values. Default to separate Results and Discussion for quantitative
work; combining them per research question is acceptable under a tight page
limit or a very long results section. Choose one mode and hold it.

**Discussion.** Interpretation tied to a research question, then explanation or
mechanism, comparison with prior work, limitation and implication. Keep
limitations concrete: dataset composition, synthetic versus real-world data,
model or provider boundary, device boundary, annotation uncertainty, metric
scope, untested deployment scenario.

**Conclusion.** Restate the problem, state what was built or evaluated, state
the strongest supported contribution, state the main limitation, state the most
important next work. Pair each contribution with its matching limitation rather
than listing all the wins and then all the caveats, and let future work follow
from the limitations. Do not end with `future research is important`.

**Captions.** Let the figure or table stand alone: what is shown, dataset or
scope, metric or unit, claim status if values are conditional, and any caveat if
the display could be overread. Do not repeat the paragraph that follows.

**Appendices.** Implementation detail, manifests, extended tables, validation
outputs and provenance. Explain why the appendix exists and how it supports the
main text. Link large datasets, code or model weights to a DOI-issuing archive
rather than swelling the appendix.

**AI-assisted-writing disclosure.** Routine grammar and spell checking needs no
acknowledgement; drafting, restructuring, idea generation or summarising should
be disclosed. Place one brief, specific statement in the preface,
acknowledgements or methods, naming the tool, its version and what it touched.
Acknowledge and disclose; do not cite an AI as a source unless the output is
itself the object of study. The author remains accountable for every claim. In
Australia, TEQSA expects a declaration and the institution sets the form, so
follow the course or higher-degree-research profile rather than a fixed
template.

## Whole-chapter and whole-work review

Use this protocol for full-chapter rewrites, whole-thesis rewrites, final-polish
passes, or any rewrite where the user asks for slow, thorough, high-accuracy
work.

1. Plan first. Identify target chapters, locked evidence, source-closure files,
   validation commands and known no-go claims.
2. Exploration pass. Send small independent agents to inspect source-closure
   notes, chapter structure, citation risks, markup risks and claim-strength
   risks. Load `orchestrate` for runtime routing; route bounded extraction to
   scout capacity and chapter-level judgement to the risk-appropriate workhorse
   or flagship at runtime. Do not select providers or model IDs here.
3. Independent second opinions. Follow the harness risk tier and use fresh,
   non-authoring reviewers from the available primary families. Keep prompts
   independent so reviewers do not anchor on each other. Record unavailable or
   skipped distinct-family lanes without making them blockers.
4. Rewrite slowly, one coherent chapter or section slice at a time, preserving
   labels, macros, result commands, citation keys, equations, tables and file
   paths unless the user explicitly approves structural change.
5. Chapter-specific review after each rewrite: unsupported claims, citation
   misuse, lost technical meaning, concision failures, AI-sounding prose, LaTeX
   breakage and chapter-boundary drift.
6. Whole-work review after all chapter edits: coherence and contribution logic;
   source-grounding and citation risk; build and cross-reference risk; a diff
   review against the pre-rewrite version; and risk-proportional independent
   second opinions.
7. Integrate only verified, actionable findings. Reviewer conclusions are
   evidence claims, not votes. Fix concrete defects; do not churn prose because
   a reviewer prefers a different style.
8. Validate before completion. Run the project citation, budget, style and build
   checks that fit the touched files. Report skipped checks explicitly and obey
   the enclosing delivery run's repair cap.

Here "review" means defect-finding: unsupported claims, overclaiming, stale
citations, missing caveats, broken markup, wrong terminology, inflated prose,
chapter-boundary errors and loss of evidence. Praise and broad summaries are not
useful review output.

## File-editing workflow

1. Read the surrounding paragraphs, not only the target sentence.
2. Preserve comments, fences, labels and macro structure.
3. Make the smallest coherent prose edit.
4. Re-read the edited passage for markup and claim drift.
5. Run targeted checks if available.

Keep post-rewrite notes short, for example: flagged one unsupported claim;
preserved all citation keys and result macros; replaced em-dash punctuation with
a full stop and a new sentence.
