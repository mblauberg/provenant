<p align="center">
  <picture>
    <source media="(prefers-color-scheme: dark)" srcset="./assets/brand/provenant-icon-dark.svg">
    <img src="./assets/brand/provenant-icon-light.svg" width="128" height="128" alt="Provenant icon">
  </picture>
</p>

<h1 align="center">Provenant</h1>

<p align="center">Rust tooling for confined coding-agent lanes, Fabric coordination, and quota-aware routing.</p>

<p align="center">
  <a href="https://github.com/mblauberg/provenant/actions/workflows/ci.yml"><img alt="CI" src="https://github.com/mblauberg/provenant/actions/workflows/ci.yml/badge.svg?branch=main"></a>
  <a href="https://github.com/mblauberg/provenant/releases/latest"><img alt="Latest release" src="https://img.shields.io/github/v/release/mblauberg/provenant?label=latest%20release"></a>
  <a href="LICENSE"><img alt="MIT licence" src="https://img.shields.io/github/license/mblauberg/provenant"></a>
  <a href="tools/front-door/Cargo.toml"><img alt="Rust MSRV 1.99.1" src="https://img.shields.io/badge/Rust%20MSRV-1.99.1-orange?logo=rust"></a>
  <a href="apps/quota-menubar/README.md"><img alt="macOS quota menu bar app" src="https://img.shields.io/badge/platform-macOS-lightgrey?logo=apple"></a>
</p>

Provenant coordinates coding agents as bounded work lanes. Each lane has an
explicit access mode and recorded result; the Rust host routes work, tracks
quota and run state, carries Fabric messages, and controls how writer changes
are delivered.

It is for developers who run coding agents in parallel and need isolated
worktrees, shared task communication, predictable model selection, and a
reviewable path from a writer lane back to a remote branch.

## Features

- **Confined provider lanes.** Claude Code, Codex, Cursor, Agy, and OpenCode lanes run with read-only
  or worktree-write access plans, and provider output and terminal state are
  recorded. Cursor, Agy, and OpenCode lanes use the same access plans; all five
  adapters are enabled by default.
- **Fabric messaging and dispatch.** The CLI and Fabric MCP surface provide
  messages, tasks, agent identity, host links, and local dispatch. Lanes can
  run on a configured peer host with `dispatch --host`; a peer writer names
  `--branch` and `--base`, and the peer creates its worktree and delivers with
  its own Git credentials.
- **Weighted, paced routes.** Named pools select among eligible models by
  configured weight and provider pacing. Inspect routes with
  `provenant route list`; edit pools with `provenant route set` and provider
  pacing with `provenant route pacing`.
- **Quota-aware admission.** Provider quota windows and account status inform
  route eligibility and dispatch admission. Inspect snapshots with
  `provenant quota show` and refresh them with `provenant quota tick`.
- **Host-side writer delivery.** A worktree writer can request delivery with
  `--worktree`, `--deliver-remote`, and `--deliver-branch`. The host verifies
  and transfers the final writer head using host Git authority; `--open-pr`
  optionally asks the host to create a pull request after verifying the pushed
  head.
- **Harness integration and hooks.** Installation can register Fabric MCP and
  supported client files for Claude Code, Codex, Cursor, Agy, and OpenCode.
  Hook handlers are explicit: Claude Code events and
  `provenant hook git pre-push` are implemented; other clients do not receive
  generic hooks.
- **Git worktrees.** `provenant worktree create`, `list`, and `remove` manage
  isolated project worktrees for concurrent lanes.
- **Usage telemetry.** `provenant telemetry stats` reports local usage and cost,
  and `provenant telemetry trace` writes attempts as a Chrome Trace Event file
  that opens in ui.perfetto.dev.
- **Quota menu bar app.** The optional macOS app displays account quota
  snapshots and lanes needing attention. Its Customise menu sets row figures,
  account names, order and visibility through `provenant accounts set` and
  `provenant quota display`. It reads reports through the CLI and does not read
  credentials itself.

## Install

Build the Rust binary and package a staged release from a checkout. Rust
1.99.1 is the minimum toolchain. The [install runbook](docs/runbooks/install.md)
documents release contents, installation targets, and recovery.

From the repository root:

```sh
cd tools/front-door
export PATH="$HOME/.cargo/bin:$PATH"
CARGO_PARENT="$(mktemp -d)"
export CARGO_TARGET_DIR="$CARGO_PARENT/cargo-target"
RELEASE_PARENT="$(mktemp -d)"
RELEASE="$RELEASE_PARENT/release"
cargo build --release --locked --bin provenant
cargo run --locked --example package-release -- --out "$RELEASE"
cd ../..
```

In the same shell session, preview an install for one selected harness,
inspect the proposed changes, and then apply it. This example installs Claude
Code:

```sh
STATE="$HOME/.local/share/provenant/v1"
"$RELEASE/bin/provenant" --state-dir "$STATE" install \
  --source "$RELEASE" --harness claude \
  --bin-dir "$HOME/.local/bin" --dry-run
# After reviewing the preview, apply the install:
"$RELEASE/bin/provenant" --state-dir "$STATE" install \
  --source "$RELEASE" --harness claude \
  --bin-dir "$HOME/.local/bin"
"$RELEASE/bin/provenant" --state-dir "$STATE" check install --source "$RELEASE"
```

Close each selected harness and configuration editor before applying an
install. `--hooks` defaults to `off`; use only the hook handlers provided by
the selected integration. Repeat `--harness` to select more than one client.
Upgrading from a pre-1.0 install needs `--legacy-checkout`; see
[Migrating from a pre-1.0 install](docs/runbooks/install.md#migrating-from-a-pre-10-install).
See the [install runbook](docs/runbooks/install.md), which also covers Fabric
MCP registration, before changing a live installation. Build the optional menu bar app from its
[macOS instructions](apps/quota-menubar/README.md).

## Quick start

With Rust 1.99.1 installed, build from `tools/front-door`, where the pinned
toolchain file lives, and inspect the available commands and routes:

```sh
cd tools/front-door
BIN="${CARGO_TARGET_DIR:-$PWD/target}/release/provenant"
cargo build --release --locked --bin provenant
"$BIN" help
"$BIN" route list
"$BIN" route resolve --route junior
"$BIN" quota show
printf '%s\n' 'Summarise this repository structure in three bullets.' | \
  "$BIN" dispatch \
    --operation-id "readme-smoke-$(date +%s)" --prompt-stdin \
    --route junior --mode read-only
"$BIN" lanes --all
```

The dispatch command starts a real read-only provider lane. Install and sign in
to a supported provider CLI first; the run uses that provider's quota. Use
`provenant help dispatch` for all dispatch controls and
`provenant help route` for route commands.

## Architecture

The `provenant` binary is the CLI and composition edge. The run crate owns
dispatch and attempt state; routing, quota, providers, confinement, and the
supervisor own their separate decisions. Fabric MCP reaches the same run
service through the Fabric runtime. The quota app reads the CLI's reports.


The CLI writes run state under the selected state root. Fabric uses its own
SQLite store for messages, tasks, and host records. By default the state root
is `~/.local/share/provenant/v1`; `--state-dir` selects another root.
[`docs/ARCHITECTURE.md`](docs/ARCHITECTURE.md) has the crate map and state root layout, and
the [Rust design index](docs/specs/rust-port/design/README.md) lists crate
responsibilities.

## Platform

The quota menu bar companion requires macOS 14 or later. CI runs Rust workspace
checks on Linux and builds the release binary on macOS; see
[the CI workflow](.github/workflows/ci.yml). The platform badge refers to the
optional menu bar app.

## Documentation

| Guide | Use it for |
| --- | --- |
| [Rust design index](docs/specs/rust-port/design/README.md) | Crate owners, command boundaries, and current Rust decisions |
| [Architecture](docs/ARCHITECTURE.md) | Product components and system flow |
| [Rust rewrite rulebook](docs/specs/rust-port/PORTING.md) | Rust implementation constraints and checks |
| [Install runbook](docs/runbooks/install.md) | Build, package, install, uninstall, and migrate a release; register Fabric MCP |
| [Configuration](docs/runbooks/configuration.md) | Profiles, knobs, writer write paths, quota display, and chair account switching |
| [Providers](docs/runbooks/providers.md) | Per-adapter launch notes |
| [Telemetry](docs/runbooks/telemetry.md) | Usage, cost, trace export, and quota commands |
| [Worktrees](docs/worktrees.md) | Worktree policy, writer dispatch, and custody recovery |
| [Model routing](docs/runbooks/model-routing.md) | Pools, weights, provider pacing, and quota eligibility |
| [Fabric hosts](docs/runbooks/fabric-hosts.md) | Configure peer hosts and peer-host dispatch |
| [Quota menu bar](apps/quota-menubar/README.md) | Build and run the optional macOS app |
| [Benchmark runbook](docs/runbooks/bench.md) | Mine, verify, and replay benchmark tasks, with the split and leakage rules |

## Contributing

Read [AGENTS.md](AGENTS.md) for repository-wide instructions and
[the GitHub work-item workflow](docs/runbooks/github-workflow.md) for branch,
issue, and delivery mechanics. Rust changes live under `tools/front-door/`;
the package is intentionally split into libraries by responsibility. Keep
changes within the owning crate and follow the accepted design before changing
its public command or record contract.

## Licence

Provenant is available under the [MIT licence](LICENSE).
