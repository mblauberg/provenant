# Harness profiles and Rust owners

Status: current implementation map. The Python profile resolver and mutation
commands are retired. The Rust configuration loader reads
`<state-dir>/config.toml`; `provenant config show` reports the resolved values.
Select the state directory with `--state-dir`, then `PROVENANT_STATE_DIR`,
otherwise `~/.local/share/provenant/v1`.

## Configuration

The header requires `schema_version = 1`. `profile` selects `balanced`,
`economy` or `rigorous`; each registered domain supplies its compiled defaults.
Explicit TOML values override those defaults, subject to domain validation.
A missing file selects defaults; malformed or unsupported configuration fails.

```toml
schema_version = 1
profile = "economy"
```

Knob overrides live in a `[knobs]` table and win over the profile preset,
which wins over the registry default. Set them with `provenant config set
<knob> <value>` or `provenant config set profile <name>`, clear them with
`provenant config unset <knob>`, and read effective values and their source
with `provenant config show [--task-class NAME]`. There is no project JSON
overlay, `--knob` flag or `PROVENANT_KNOB_*` environment layer.

## Owner table

Paths below are relative to `tools/front-door/crates/`.

| Concern | Rust owner | Command or configuration |
| --- | --- | --- |
| State directory | `core/src/roots.rs` | `--state-dir`, `PROVENANT_STATE_DIR` |
| Profile selection, defaults and validation | `config/src/loader.rs` | `provenant config show`; `profile` in `config.toml` |
| Knob registry, overrides and precedence | `config/src/knobs.rs` | `provenant config set`, `config unset`, `config show`; `[knobs]` in `config.toml` |
| Registered domain composition | `cli/src/domains.rs` | `domains.routing`, `domains.quota`, `domains.fabric`, `domains.guard` |
| Route selection and effort | `routing/src/resolve.rs`, `cli/src/route.rs` | `provenant route list`, `provenant route resolve`; dispatch route flags |
| Batch concurrency and provider timeout | `cli/src/grammar.rs` | `provenant dispatch --manifest FILE --concurrency N`; `--provider-timeout` |
| Review coverage and risk | `workflow/src/coverage.rs`, `workflow/src/risk.rs` | `provenant workflow`; the constitution supplies policy floors |
| Quota probe ceilings | `quota/src/settings.rs` | `domains.quota.max_parallel`, `domains.quota.wall_budget_seconds` |
| Host policy and limits | `fabric-runtime/src/config.rs` | `domains.fabric`; `provenant fabric host list` |

## Knob registry

[`config/harness-knobs.json`](../../../config/harness-knobs.json) is the knob
registry: names, types, ranges, presets and trade-offs. The binary embeds it at
build time, and `config set` validates against it. A knob's `reader` names its
consumer. Skill-read knobs (review, repair, council, writing and summary
policy) take effect when the skill reads `provenant config show`.

Use [the configuration runbook](../../runbooks/harness-profiles.md) for
operator commands and [the Rust designs](../rust-port/design/README.md) for
the accepted boundaries.
