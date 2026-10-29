## Additions

- Chair sessions now show their tokens. The SessionStart hook records the
  serving account (or `unattributed` for a config directory that is not a
  registered account, the default `~/.claude` included), and `quota usage` and
  `telemetry stats` read each session's transcript and its sub-agents' for
  tokens, cache economics and API-equivalent cost beside lane spend. Launch the
  chair with `CLAUDE_CONFIG_DIR` set to a registered account's directory to
  attribute it; the runbook describes the launch. Sessions whose transcript is
  unreadable are counted as unread, not as zero.
