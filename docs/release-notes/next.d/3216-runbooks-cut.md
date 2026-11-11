## Behaviour changes
### CLI and install

- The runbooks are cut to the procedures the tool cannot make obvious. `model-routing`, `model-policy` and `presets` merge into `routing-and-templates`, `disk` into `gc`, and `agy-authentication` into `providers`; `docs/runbooks/README.md` indexes the rest. `scripts/check-doc-truth` now fails on a command or flag in a runbook that `provenant help --all --json` does not list (#3216).
