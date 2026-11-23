## Fixes
### CLI and install

- `provenant output` now omits a result's header lines when their value is JSON
  `null`: a present key with no value no longer prints an `unavailable` line for
  the base, committed head, publication, residual tree state or changed files.
  (#3807)
