# Fresh-reader comparison fixtures

Prepared inputs only. No evaluation has run and no gap counts are claimed.
The chair runs `evaluate` with fresh agents after delivery, freezes the complete
plan and hashes before dispatch, and retains outputs and the canonical receipt
as run evidence. Do not commit generated answers or scratch-path citations.

`sources.json` records closed issues #761, #800 and #1564, source URLs,
retrieval date and hashes. Each plain-text input is the issue title and
unmodified body, captured through the public GitHub API. These are historical
evaluation inputs, not live specs or an archive of new change specs. The reader
sees one input only, with no comments, implementation, closure evidence or
repository reads. Keep `grading.md` out of every reader brief.

## Plan to freeze

Decision: does the advisory check find actionable missing context beyond cold
reading? The three issues span a precise governance edit, a document ownership
change and a terse runtime change. This convenience sample cannot establish a
general productivity gain.

Use paired fresh attempts with identical spec bytes and runtime, no inherited
conversation, and the spec text inline as the only context (instruct no file
reads; dispatch through `orchestrate`, native first). Freeze the exact routes,
seeds, repeats, timeouts, retries and graders before execution, following
`evaluate`:

- **Candidate:** the spec plus the fresh-reader brief from `scope`.
- **Without:** the same spec with only: "Read this spec cold. Identify any
  missing information needed to implement it. Do not consult other sources."
- **Previous (cold):** the same spec with only this literal reader prompt:
  "Read this spec cold. Before approval, would you be ready to implement it?
  State the goal, the files you would touch and every missing piece of
  information. Do not consult other sources." Do not alias it to Without; it
  differs only in asking for goal, files and gaps. Record the pre-change
  `skills/scope` digest at the base commit as provenance.

Package manifests belong to the controller, not the isolated reader's inputs.
Only the treatment's brief and the spec reach a reader. Start with two paired
repetitions per arm on each available primary provider family. Report any
unavailable family rather than silently substituting it.

Grade blinded outputs independently of their writer family. Count distinct
genuine gaps and noise per spec, arm and repetition using `grading.md`;
deduplicate paraphrases within an output. Report candidate-only genuine gaps
against both comparators, missed explicit constraints, elapsed time and token
usage. Retain failures, omissions, timeouts and disagreements, not just
successful reads. Adjudicate disputed gaps with a fresh independent grader.

Proposed retention threshold, to freeze before results: at least one actionable
candidate-only gap on two of the three specs versus cold reading, with genuine
gaps at least half of the candidate's classified questions. Also compare the
previous arm and report regressions. If there is no added value, drop the check
and recommend closing #1756 as not planned with the evidence; do not convert it
into a mandatory step.

Report a row per spec and arm: genuine gaps, noise, contested questions,
candidate-only gaps, missed constraints and cost, plus raw attempt identities.
The chair owns adoption and the PR evidence summary.
