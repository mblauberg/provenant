# Harness profiles

A profile sets the harness's cost and quality knobs. The registry, presets and
resolution order are specified in
[harness profiles](../specs/harness/profiles.md). With no profile the harness
behaves as it did before profiles existed.

## Switch

1. Run `scripts/provenant-profile list` to see each knob's default, allowed values,
   trade-off and whether it is `wired` (code or a skill reads it) or
   `advisory` (recorded, but the owning prose or constant still holds today's
   value). `scripts/provenant-profile diff balanced economy` shows what a preset
   changes, and `--task-class legwork` shows the result for one class.
2. Pick a scope. `scripts/provenant-profile use economy` writes your user file,
   `~/.agents/config/harness-profile.json`, and applies everywhere. Add
   `--project` to write `.agents/harness-profile.json` in the current checkout
   instead, so one repository can run `rigorous` while the rest stay
   `balanced`. Add `--task-class critical-review` to scope a preset or knob to
   one task class. A project file may raise `review.*` knobs but not lower
   them; lower them in your user file or the environment.
3. Adjust single knobs with `scripts/provenant-profile set lanes.max_concurrency=2`
   (the same `--project` and `--task-class` flags apply) and drop them again
   with `scripts/provenant-profile reset lanes.max_concurrency`, or `reset preset`.
   Values are checked against the registry, so a typo is an error rather
   than a silent no-op.
4. For one session or one experiment, use the environment instead of a file:
   `PROVENANT_PROFILE=rigorous` or `PROVENANT_KNOB_EFFORT_SHIFT=1`. For one
   dispatch, pass `skills/orchestrate/scripts/dispatch_run.py --knob effort.shift=1`. Both beat the
   files, including when the dispatch is placed on a peer host.
5. Confirm with `scripts/provenant-profile show --resolved` (add `--task-class` and
   `--risk-tier` to see a specific lane). Each line names the layer that set
   the value; `floor:crucial` means a risk-tier floor overrode the profile.

## Compare

1. Hold the work constant. Choose a repeatable slice, such as a replay bench
   task (issue #1064) or a batch of similar Sealle issues at one risk tier,
   and run it once per profile in the same checkout, changing only
   `PROVENANT_PROFILE` or the knob under test.
2. Every attempt receipt records the profile in force as `profile.preset`,
   `profile.digest` and the full `profile.knobs` map. Equal digests mean equal
   behaviour, however the knobs were set.
3. Group the receipts by digest and compare outcomes and usage. Until the
   ledger carries the digest, read the receipts directly, for example:

   ```sh
   jq -s 'group_by(.profile.digest) | map({digest: .[0].profile.digest,
     preset: .[0].profile.preset, attempts: length,
     ok: (map(select(.status == "ok")) | length),
     input: (map(.context.input_tokens // 0) | add),
     output: (map(.context.output_tokens // 0) | add)})' \
     .agent-run/runs/*/tasks/*/attempt-*/attempt.json
   ```

   Review findings, retries and wall time come from the run's review records
   and `timing`. Treat a side with fewer than ten attempts as inconclusive.
4. Keep the winner by writing it to the user or project file, and record the
   comparison and decision in the issue that prompted it.
