## Behaviour changes
### CLI and install

- Claude Code answer delivery now runs on `PostToolUse` instead of `PreToolUse`:
  a Provenant hook failure can no longer make Claude refuse a Bash or Read call,
  and queued answers still reach the chair through `additionalContext`. Existing
  installs drop the managed `PreToolUse` entry on upgrade while foreign hooks on
  that event stay untouched; Codex keeps its declared `PreToolUse` mapping. A
  hook that fails inside Provenant still abstains with exit 0, but now records
  its cause and duration under `<state>/hooks/diagnostics/failures.jsonl`. (#3803)
