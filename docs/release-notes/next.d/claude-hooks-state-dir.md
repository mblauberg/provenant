## Fixes

- Claude Code session hooks now repeat a non-default `--state-dir` in every
  command they print: the `Save:` line, the digest heading, the bounded-output marker, the unread
  `pr status` hint and the `workflow session show` pointer. Following a hint
  saves to, or reads from, the store the hook used and never replaces a
  checkpoint in the default store. A directory that cannot be shown safely
  gives a plain note instead of a command (#1829).
