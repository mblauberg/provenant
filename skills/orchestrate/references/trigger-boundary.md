# Trigger boundary

The skill should fire when orchestration changes the expected quality or safety of the work, not when a
prompt merely sounds serious.

## Positive triggers

- explicit `use subagents`, `use several/many subagents`, `fan out`, `subagents in waves`, or equivalent;
- explicit `ultracode`, `run a workflow`, `use a workflow`, or a saved workflow command;
- explicit `deep research`, `web research`, or `review/refine` when paired with breadth, audit, or
  verification needs;
- repo-wide sweeps, multi-angle audits, and large migrations that decompose into independent slices;
- `independent second opinion`, `cross-check`, `red-team`, `adversarial review`;
- parallel research/source passes followed by synthesis;
- comparison of model outputs where a synthesis or judge is needed;
- high-stakes, low-oracle work requiring independent verification before trust;
- long-running work where context isolation, scratchpads, and resumable state matter.

## Negative triggers

- small typo, rename, formatting, one-line command, or simple explanation;
- ordinary single-file review with no real decomposition value;
- passive mentions of tool names (`codex`, `agy`, `cursor`, `kiro`, `claude`) without delegation intent;
- tightly coupled debugging where the next action depends on one local result;
- concurrent shared-state writes that cannot be partitioned;
- a standing run-until-STOP job with cross-session filesystem state; that is the
  **autopilot** skill's territory (it consumes this doctrine per iteration; this skill runs one
  engagement and finishes).

## Ambiguous triggers

Treat these as hints, not automatic triggers:

- `be thorough`;
- `audit this` on one small file;
- `compare outputs` when the user only wants a short opinion;
- `use web searches` without source breadth, deep research, audit, or verification need.

Decision rule: a positive trigger means evaluate orchestration, not automatically fan out. Apply the
entrypoint's decomposition/value gate. Parallel work requires independent information, stable
interfaces, non-overlapping writes, independently checkable returns, and expected information gain
above coordination, shared-state and tool-density cost. Otherwise keep serial ownership. State the
decomposition, keep workers bounded, and pilot a slice when value is uncertain. Hierarchy for show is
still waste: every worker must map to a useful independent slice or angle.
