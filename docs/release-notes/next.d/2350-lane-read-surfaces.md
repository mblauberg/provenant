## Additions
### Lanes and settlement

- `provenant output --to FILE` writes the selected range to FILE, replacing it, so a large part can be saved without printing it; page on with `--offset`. `provenant output --path` prints where the attempt keeps its `result`, `prompt` or `stderr` file, for `--part` result, prompt or stderr (#2350).
- `provenant output RUN` and `provenant output RUN/TASK` read the newest settled attempt of that run or task and name the attempt they chose. A run that has several tasks asks for RUN/TASK. A failed attempt's output names the last line of its provider error and its result path (#2350).
- `provenant lanes --json` rows gain `handoff`, the attempt's parsed handoff block or null, and `settlement`, which holds the settled `reason` for a settled lane and is null otherwise (#2350).
- A `review_required` lane prints `next: provenant lane deliver --accept-review ATTEMPT` with its own attempt ID, so the chair can see how to accept the review (#2350).
