## Fixes

- Agy writer lanes can now start their shell. Every `run_command` failed with
  "fork/exec /bin/zsh: operation not permitted", because the profile did not
  allow the shell to run. Agy now runs with `SHELL=/bin/zsh`, and writers may
  execute that one binary (#1695).
