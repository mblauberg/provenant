## Additions

- A lane that finishes only part of its brief is flagged instead of reading as
  complete. An attempt that settles `succeeded`, `review_required` or
  `uncommitted` carries `partial_signals` when its final answer ends with a
  valid `stopped_early` `handoff` block, the provider reports a stop other than a
  finished turn (a token or turn limit), or a writer left uncommitted edits.
  `lanes` shows `(partial)` beside the state with the signals and a resume
  hint; the outcome is unchanged and the chair decides whether to resume. Launch
  prompts now ask a lane that stops with work left to end with the `handoff`
  block. Prose is never read for the flag.
