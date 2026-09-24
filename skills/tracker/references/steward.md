# Steward mode

The coordinator hands off all tracker work with a background worker prompt:
"load the `tracker` skill in steward mode; queue `<path>`" — default route: a
Sonnet subagent, or a Fabric lane once `gh` works inside its sandbox.

## Queue file

An append-only JSONL file the coordinator writes, one line per event:

```json
{"id": "evt-2026-09-24-0001", "type": "pr_opened", "payload": {"pr": 123, "issue": 872}}
```

- `id`: unique within the file (`load_events` rejects duplicates).
- `type`: one of `lane_followup`, `pr_opened`, `pr_landed`, `lane_abandoned`,
  `owner_decision`, `run_sweep`.
- `payload`: whatever context the event needs (lane report, PR number,
  decision text); shape is caller-defined.
- `outcome` (added by the steward, not the coordinator): `{"status": ...,
  "summary": ...}` once the event has been handled.

The coordinator only ever appends new lines; it never edits or removes a line
after writing it. Concurrent writers append to the same file only when they
coordinate their own locking — the schema does not arbitrate that.

## Replay

`skills/tracker/scripts/steward.py` owns the mechanical replay:

- `load_events(path)` parses and validates the queue.
- `pending(events)` returns events without an `outcome`.
- `record_outcome(path, id, outcome)` persists one event's outcome and
  returns `False` without changing anything if it is already recorded — the
  idempotence a crash-resume or a rerun relies on.
- `replay(path, handler)` drives a full pass: for each pending event, call
  `handler(event) -> {"status": ..., "summary": ...}`, persist it, and return
  the list this run actually processed (already-recorded events are skipped
  entirely, not re-handed to `handler`).

The steward (the agent, not the script) supplies the judgement inside
`handler`: for each event it triages, files, links, closes or updates the
board and, for `run_sweep`, runs the [hygiene sweep](hygiene.md) — following
the filing and linking rules in [filing.md](filing.md) and
[linking-and-board.md](linking-and-board.md) — then records the outcome via
`record_outcome` (directly, or through `replay`'s handler contract). A status
of `flagged` marks an outcome that still needs a coordinator or owner call
(an unsettled possible duplicate, a Horizon promotion, a Decision); everything
else is `status: done` or `status: no-op` with a one-line `summary`.

## Digest

`digest(processed, max_lines=15)` renders one line per processed event —
`flagged` outcomes prefixed `! ` — truncated to at most 15 lines with a
trailing `... +N more` when the run processed more than that. This bounded
digest is the only thing that goes back into the coordinator's context; full
detail stays in the queue file and the tracker itself.

## Authority recap

Create, edit, link, close and set Status inside the repo's rules. Never
delete an issue, never transfer it, never change Horizon except by the repo's
own promotion rule (logged), never close a Decision, and never write code.
