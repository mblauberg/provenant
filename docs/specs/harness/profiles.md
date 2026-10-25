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
overlay or `PROVENANT_KNOB_*` environment layer. Dispatch accepts repeated
`--knob NAME=VALUE` flags; Fabric `fabric_submit` accepts `knobs: ["NAME=VALUE"]`.
These validated overrides win over configuration and task-class presets. Each
sealed task and attempt records the profile name, task class, effective knob
values and exact dispatch overrides. Continuations retain that stamp.

Batch concurrency resolves `lanes.max_concurrency` from the base profile
(`profile(None)` in submission), so task-class values do not apply. Explicit
`--concurrency N` or Fabric `concurrency` wins over that resolved value.
With no configuration or dispatch override, bare `dispatch --manifest` now
uses the registry default of **8**, replacing the former CLI default of 4.

## Owner table

Paths below are relative to `tools/front-door/crates/`.

| Concern | Rust owner | Command or configuration |
| --- | --- | --- |
| State directory | `core/src/roots.rs` | `--state-dir`, `PROVENANT_STATE_DIR` |
| Profile selection, defaults and validation | `config/src/loader.rs` | `provenant config show`; `profile` in `config.toml` |
| Knob registry, overrides and precedence | `config/src/knobs.rs` | `provenant config set`, `config unset`, `config show`; `[knobs]` in `config.toml` |
| Registered domain composition | `cli/src/domains.rs` | `domains.routing`, `domains.quota`, `domains.fabric`, `domains.guard`, `domains.run` |
| Route selection and effort | `routing/src/resolve.rs`, `cli/src/route.rs` | `provenant route list`, `provenant route resolve`; dispatch route flags |
| Batch concurrency | `cli/src/run.rs`, `run/src/launch.rs`, `run/src/admission.rs` | Base-profile `lanes.max_concurrency` supplies the default (registry: 8); task-class values do not apply; explicit `--concurrency N` or Fabric `concurrency` wins |
| Provider timeout | `cli/src/grammar.rs`, `run/src/launch.rs` | `--provider-timeout` |
| Silence observation | `run/src/launch.rs`, `supervisor/src/custody.rs`, `supervisor/src/guardian.rs` | `lanes.silence_seconds` marks observations silent after no output or CPU progress; silence never authorises a stop |
| Attempt profile stamp | `config/src/knobs.rs`, `records/src/run.rs`, `run/src/store.rs` | Effective task-class policy and dispatch `knobs`, retained with the sealed request and attempt |
| Effort shift (advisory) | No Rust consumer | Catalogue effort controls and task-class policy floors lack evidence |
| Pool mode (advisory) | No Rust consumer | Exact routes and explicit fallbacks have no weighted pool selection |
| Context ceiling (advisory) | No Rust consumer | Dispatch has no context-compaction path |
| Quota pace tolerance (advisory) | No Rust consumer | Quota does not compute pace against a tolerance |
| Review coverage and risk | `workflow/src/coverage.rs`, `workflow/src/risk.rs` | `provenant workflow`; the constitution supplies policy floors |
| Quota probe ceilings | `quota/src/settings.rs` | `domains.quota.max_parallel`, `domains.quota.wall_budget_seconds` |
| Host policy and limits | `fabric-runtime/src/config.rs` | `domains.fabric`; `provenant fabric host list` |

## Knob registry

[`config/harness-knobs.json`](../../../config/harness-knobs.json) is the knob
registry: names, types, ranges, presets and trade-offs. The binary embeds it at
build time, and `config set` validates against it. A wired knob's `reader`
names its consumer; advisory knobs carry a reason where a Rust owner is absent.
Skill-read knobs (review, repair, council, writing and summary policy) take effect when the skill reads `provenant config show`.

Use [the configuration runbook](../../runbooks/harness-profiles.md) for
operator commands and [the Rust designs](../rust-port/design/README.md) for
the accepted boundaries.
