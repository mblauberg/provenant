## Behaviour changes
### CLI and install

- The generated reference under `docs/reference/` is no longer tracked, so a command change no longer needs it regenerated. Read `provenant help --all` and `provenant config schema`, or run `scripts/generate-reference` to render it locally; CI only checks that it generates and uploads it as an artefact, and `--check` is gone (#3214).
