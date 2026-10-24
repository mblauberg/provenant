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

- **Confined provider lanes.** Claude Code and Codex lanes run with read-only
  or worktree-write access plans, and provider output and terminal state are
  recorded. Agy, Cursor, and OpenCode adapters are catalogued but stay
  execution-disabled until a live confinement capture proves each mode.
- **Fabric messaging and dispatch.** The CLI and Fabric MCP surface provide
  messages, tasks, agent identity, host links, and local dispatch. Peer-host
  dispatch (`dispatch --host`) is not available in this build.
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
- **Quota menu bar app.** The optional macOS app displays account quota
  snapshots and lanes needing attention. It reads reports through the CLI and
  does not read credentials itself.

## Install

Build the Rust binary and package a staged release from a checkout. Rust
1.99.1 is the minimum toolchain. The [front-door runbook](docs/runbooks/front-door.md)
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
Code and uses the current checkout as the legacy-entry inventory:

```sh
LEGACY_CHECKOUT="$(git rev-parse --show-toplevel)"
STATE="$HOME/.local/share/provenant/v1"
"$RELEASE/bin/provenant" --state-dir "$STATE" install \
  --source "$RELEASE" --harness claude \
  --bin-dir "$HOME/.local/bin" \
  --legacy-checkout "$LEGACY_CHECKOUT" --dry-run
# After reviewing the preview, apply the install:
"$RELEASE/bin/provenant" --state-dir "$STATE" install \
  --source "$RELEASE" --harness claude \
  --bin-dir "$HOME/.local/bin" \
  --legacy-checkout "$LEGACY_CHECKOUT"
"$RELEASE/bin/provenant" --state-dir "$STATE" check install --source "$RELEASE"
```

Close each selected harness and configuration editor before applying an
install. `--hooks` defaults to `off`; use only the hook handlers provided by
the selected integration. Repeat `--harness` to select more than one client.
See [Fabric MCP registration](docs/runbooks/fabric-mcp-registration.md) and
the [install runbook](docs/runbooks/front-door.md) before changing a live
installation. Build the optional menu bar app from its
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

```mermaid
flowchart TB
    App["Quota menu bar app"] -->|"quota show and tick"| CLI["provenant CLI"]
    CLI --> Run["run"]
    CLI --> Workflow["workflow"]
    CLI --> Telemetry["telemetry"]
    CLI --> MCP["fabric-mcp"]
    MCP --> Runtime["fabric-runtime"]
    Runtime --> Run
    Runtime --> FabricStore["fabric-store"]
    Run --> Routing["routing"]
    Run --> Quota["quota"]
    Run --> Providers["providers"]
    Run --> Confinement["confinement"]
    Run --> Supervisor["supervisor"]
    Run --> Records["records"]
    Routing --> Quota
    Routing --> Providers
    Config["config"] -.-> Routing
    Config -.-> Run
    Config -.-> Workflow
    Telemetry --> Records
    Workflow --> Records
    Records --> Core["core"]
```

The CLI writes run state under the selected state root. Fabric uses its own
SQLite store for messages, tasks, and host records. By default the state root
is `~/.local/share/provenant/v1`; `--state-dir` selects another root.
[`docs/ARCHITECTURE.md`](docs/ARCHITECTURE.md) explains the wider system and
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
| [Release and install runbook](docs/runbooks/front-door.md) | Build, package, install, and recover a release |
| [Model routing](docs/runbooks/model-routing.md) | Pools, weights, provider pacing, and quota eligibility |
| [Fabric MCP registration](docs/runbooks/fabric-mcp-registration.md) | Register Fabric with supported clients |
| [Fabric hosts](docs/runbooks/fabric-hosts.md) | Configure peer hosts and the peer protocol |
| [Quota menu bar](apps/quota-menubar/README.md) | Build and run the optional macOS app |
| [Benchmark runbook](docs/runbooks/bench.md) | Mine, verify, and replay benchmark tasks |

## Contributing

Read [AGENTS.md](AGENTS.md) for repository-wide instructions and
[the GitHub work-item workflow](docs/runbooks/github-workflow.md) for branch,
issue, and delivery mechanics. Rust changes live under `tools/front-door/`;
the package is intentionally split into libraries by responsibility. Keep
changes within the owning crate and follow the accepted design before changing
its public command or record contract.

## Licence

Provenant is available under the [MIT licence](LICENSE).
