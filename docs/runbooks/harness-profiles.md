# Harness configuration

Read effective configuration with `provenant config show`. The Rust config
owner reads the selected state root's `config.toml` and validates owner sections.
Use `--state-dir /absolute/path` to select an isolated root. Change the
profile or a knob with `provenant config set profile economy` or
`provenant config set lanes.max_concurrency 4`; `config unset KNOB` reverts it.

Profiles are `balanced`, `economy` and `rigorous`; task-class presets include
`mechanical` and `bulk`. Inspect effective values with
`provenant config show --task-class mechanical` and override a wired knob for
one run with `provenant dispatch --knob NAME=VALUE`.

`config/harness-knobs.json` owns the workflow knob registry and presets.
Fabric's similarly named TOML assets supply only Fabric domain defaults; they
are not a second source for the workflow knob values.

The registry marks six knobs `wired`: `lanes.silence_seconds`,
`lanes.max_concurrency`, `review.extra_reviewers`, `review.cross_family`,
`review.rereview_scope` and `review.polish`. The other eight are advisory and
do not alter Rust runtime behaviour. Use `config show` to see each knob's
status and source. Install registrations with `provenant install --dry-run`
before applying an authorised install.
