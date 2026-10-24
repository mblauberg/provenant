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

There is no `provenant profile` command, project JSON overlay, `--knob` flag
or `PROVENANT_KNOB_*` environment layer in the Rust CLI. Use explicit dispatch
controls and the registered TOML domains instead. The binary edge currently
returns `unavailable` for dispatch and run controls; the flags below describe
the parsed interface, not proof of executable dispatch.

## Owner table

Paths below are relative to `tools/front-door/crates/`.

| Concern | Rust owner | Command or configuration |
| --- | --- | --- |
| State directory | `core/src/roots.rs` | `--state-dir`, `PROVENANT_STATE_DIR` |
| Profile selection, defaults and validation | `config/src/loader.rs` | `provenant config show`; `profile` in `config.toml` |
| Registered domain composition | `cli/src/domains.rs` | `domains.routing`, `domains.quota`, `domains.fabric`, `domains.guard` |
| Route selection and effort | `routing/src/resolve.rs`, `cli/src/route.rs` | `provenant route list`, `provenant route resolve`; dispatch route flags |
| Batch concurrency and provider timeout | `cli/src/grammar.rs` | `provenant dispatch --manifest FILE --concurrency N`; `--provider-timeout` |
| Review coverage and risk | `workflow/src/coverage.rs`, `workflow/src/risk.rs` | `provenant workflow`; the constitution supplies policy floors |
| Quota probe ceilings | `quota/src/settings.rs` | `domains.quota.max_parallel`, `domains.quota.wall_budget_seconds` |
| Host policy and limits | `fabric-runtime/src/config.rs` | `domains.fabric`; `provenant fabric host list` |

## Legacy knob inventory

[`config/harness-knobs.json`](../../../config/harness-knobs.json) retains the
old knob names, presets and trade-offs as an advisory inventory. Rust does
not load that JSON registry. Its `reader` paths identify the current Rust
boundary for the concern, not a consumer of the retired knob. In particular,
summary size, polish, council size and repair budgets remain skill policy;
a Rust config reader does not turn them into runtime settings.

Use [the configuration runbook](../../runbooks/harness-profiles.md) for
operator commands and [the Rust designs](../rust-port/design/README.md) for
the accepted replacement boundaries. Do not use the legacy inventory as
proof that a dispatch applied a knob or stamped a legacy profile digest.
