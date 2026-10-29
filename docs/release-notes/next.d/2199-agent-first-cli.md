## Breaking changes

- Human command results now default to a compact owner-supplied summary: counts by state, action rows first, and one typed next command. `status`, `lanes`, `route list`, `quota show`, `pr status`, `accounts list` and `fabric graph status` summarise; other commands fall back to a truncator that says how much it omitted. Use global `--full` for complete detail (it no longer implies `status --all`) or `--json` for the machine envelope.
- `output --summary` is removed; `output` now shows the whole result (see the output notes).
- The `next:` line comes only from the typed `next` field; the `provenant help` fallback is gone.
- `Outcome::Pending` and exit code 5 report a merge that is accepted but not yet effective.
