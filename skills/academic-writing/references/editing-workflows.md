# Editing Workflows

Use this reference to choose a repeatable process for a thesis-writing task.

## Reference Map

Beyond the hub default (Australian English) and academic register/workflow
mode loaded for every task, add overlays only as the mode requires:

- Voice diagnosis or final scrub: the hub anti-AI taxonomy plus its academic
  overlay, `anti-ai-thesis-patterns.md`.
- Rewrite, condense, or structural polish: the hub condense pass plus its
  overlay, `concision-and-structure.md`.
- Remaining overlays, load the one the task needs: `chapter-patterns.md`; the
  hub claim-discipline schema plus its overlay
  `claim-discipline-and-evidence.md`; `engineering-thesis-voice.md`; or
  `citation-and-latex-safety.md`.
- LaTeX-only work: start with `citation-and-latex-safety.md` and load prose
  guidance only if prose also changes.

A project adapter or keep-list takes precedence over any of the above.

## Diagnose Mode

Use when the user asks what is wrong, whether prose sounds AI-generated, or how to improve a section.

Output order:

1. Highest-risk issue first.
2. Concrete example or quoted short phrase.
3. Why it harms thesis quality.
4. Suggested repair.

Prioritise:

- unsupported claims
- invented or unsafe citations
- pending evidence presented as result
- LaTeX corruption risk
- AI-sounding generic prose
- density that blocks comprehension

## Rewrite Mode

Use when the user gives existing prose and wants improvement.

Process:

1. Lock invariants.
2. Identify section type.
3. Find the paragraph claim.
4. Remove throat-clearing.
5. Replace inflated language with concrete operations.
6. Split overloaded sentences.
7. Preserve citations and LaTeX exactly.
8. Return final prose first.

## Draft Mode

Use when writing from notes or bullet points.

Process:

1. Convert notes into claim order.
2. Decide paragraph topics.
3. Draft full paragraphs.
4. Add flags for missing citation or evidence.
5. Avoid filling unknown result values.
6. Keep section voice consistent with the surrounding thesis.

Do not turn bullet points into a bullet list unless the target section calls for one. Most thesis prose should be full paragraphs.

## Section Polish Mode (`section-polish`)

Use when multiple paragraphs, a full section or a chapter need flow.

Process:

1. Map each paragraph to one function.
2. Remove duplicate functions.
3. Check transitions express logic, not decoration.
4. Standardise terminology.
5. Move implementation detail to the right level.
6. Keep section headings and LaTeX structure unless asked to change them.
7. Run anti-AI and concision passes.

## Match-Voice Mode

Use when the user provides a writing sample.

Match:

- sentence-length distribution, not just the range (how often the writer uses short versus long sentences)
- paragraph density and typical paragraph length
- how sentences and paragraphs open
- certainty level and hedging habits
- punctuation habits, excluding em dashes
- jargon tolerance
- use of first person, if any
- preferred transitions

Borrow the writer's habits, not their fingerprints. Reproduce the distributional and structural tendencies above, but do not copy signature phrases, pet words, or catchphrases. Do not introduce slang or deliberate roughness.

## Citation-Safe Mode

Use for literature reviews, background, and citation-heavy discussion.

Process:

1. Preserve existing keys.
2. Check each citation supports the nearest claim.
3. Replace source catalogue paragraphs with synthesis where possible.
4. Flag unsupported claims.
5. Keep author names only when they matter.
6. Avoid citation dumping.

## Final-Scrub Mode

Use near submission or before supervisor review.

Check:

- Australian spelling.
- No em dashes or prose `---`.
- No placeholder phrases such as `TODO`, `TBD`, or `insert result`.
- No generic AI phrases.
- No unresolved citation placeholders.
- No result macro converted into guessed value.
- No paragraph starts with empty signposting.
- No conclusion overclaims beyond evidence.

Then run one adversarial self-audit pass. Ask plainly: what still makes this read as AI-generated? Answer in a sentence or two (for example: `every sentence is over thirty words`, `two balanced triads in one paragraph`, `a comparison claim with no number`). Revise only if the answer names a real, fixable tell. If the honest answer is `nothing specific`, stop. This single question catches the distributed, checker-invisible tells the fixed-pattern checks miss. Clean-looking prose can still fail on rhythm and structure alone.

If editing files, run the local checker script when available. Treat its silence as necessary, not sufficient: the checker scans fixed patterns and cannot see flat rhythm, comma-gloss definitions, or implicit-completion tense.

## Multi-Agent Review Mode

Use for full-chapter rewrites, whole-thesis rewrites, final-polish passes, or any rewrite where the user asks for slow, thorough, high-accuracy work.

1. Plan first. Identify target chapters, locked evidence, source-closure files, validation commands, and known no-go claims.
2. Exploration pass. Send small independent agents to inspect source-closure
   notes, chapter structure, citation risks, LaTeX risks, and claim-strength
   risks. Load `orchestrate`; route bounded extraction to scout capacity and
   chapter-level judgement to the risk-appropriate workhorse or flagship at
   runtime.
3. Independent second opinions. Follow the harness risk tier and use fresh,
   non-authoring reviewers from the available primary families. Keep prompts
   independent so reviewers do not anchor on each other's conclusions. Record
   unavailable or skipped distinct-family lanes without making them blockers.
4. Rewrite slowly. Apply one coherent chapter or section slice at a time. Preserve labels, macros, result commands, citation keys, equations, tables, and file paths unless the user explicitly approves structural changes.
5. Chapter-specific review. After each chapter rewrite, assign subagents to review that chapter for unsupported claims, citation misuse, lost technical meaning, concision failures, AI-sounding prose, LaTeX breakage, and chapter-boundary drift.
6. Whole-work review. After all chapter edits, send separate agents for:
   - whole-thesis coherence and contribution logic;
   - source-grounding and citation-risk audit;
   - LaTeX/build and cross-reference risk;
   - diff review against the pre-rewrite version;
   - risk-proportional independent whole-work second opinions.
7. Integrate only verified, actionable findings. Reviewer conclusions are
   evidence claims, not votes. Fix concrete defects; do not churn prose because
   a reviewer merely prefers a different style.
8. Validate before completion. Run the project citation, budget, style, and
   build checks that fit the touched files. Report skipped checks explicitly and
   obey the enclosing delivery run's repair cap.

For this protocol, "review" means defect-finding: unsupported claims, overclaiming, stale citations, missing caveats, broken LaTeX, wrong terminology, inflated prose, chapter-boundary errors, and loss of evidence. Praise and broad summaries are not useful review output.

## File-Editing Workflow

When editing thesis files:

1. Read surrounding paragraphs, not only the target sentence.
2. Preserve comments, fences, labels, and macro structure.
3. Make the smallest coherent prose edit.
4. Re-read the edited passage for LaTeX and claim drift.
5. Run targeted checks if available.

## Notes Format

After a rewrite, keep notes short:

```text
Notes:
- Flagged one unsupported claim.
- Preserved all citation keys and result macros.
- Replaced em dash punctuation with a full stop and a new sentence.
```

Do not add long explanations unless the user asks for rationale.
