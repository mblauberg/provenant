# Front door runbook

How the `provenant` command is built, installed, checked and rolled back. The
decision is [ADR 0027](../adr/0027-rust-front-door.md), amended 2026-10-23.

## Install

`scripts/install-harness --platform <platform>` builds and installs the Rust
binary. Cargo uses `CARGO_TARGET_DIR`, defaulting to
`~/.cache/provenant/cargo-target`, so subsequent installs build incrementally.
The installer prints `front-door=rust`. `PROVENANT_FRONT_DOOR` may be unset or
`rust`; other values exit 3 before installation.

Without `cargo` (on `PATH` or in `~/.cargo/bin`), installation exits 3 and prints:

```text
fix: curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
```

The Python front door has been deleted. Rollback is a git revert of the slice,
then a reinstall from the reverted checkout.

## Run from a checkout

Use the installed binary from the agent's checkout:

```sh
provenant fabric dispatch --help
```

The binary resolves the product checkout through `AGENT_FABRIC_PRODUCT_ROOT`,
the instance's product-root pointer, or `AGENTS_HOME`, in that order. It keeps
the caller's working directory, so Fabric resolves the caller's project,
including a registered linked worktree, as before. `provenant root` reports
the selected product checkout.

## Rebuild after a source change

Changing anything under `tools/front-door/` makes the installed binary stale.
`scripts/check-provenant-install.py` fails with "built from different sources".
Re-run `scripts/install-harness --platform <platform>`.

## Supply a prebuilt binary

For a host without a toolchain, build on a machine that has one:

```sh
cd tools/front-door
PROVENANT_FD_SOURCE_SHA256="$(python3 ../../scripts/lib/front_door_build.py digest .)" \
  MACOSX_DEPLOYMENT_TARGET=15.0 cargo build --release --locked
```

Copy `target/release/provenant` to the host as an absolute path, then on that
host:

```sh
PROVENANT_FRONT_DOOR_BINARY=/abs/path/provenant scripts/install-harness --platform <platform>
```

The installer accepts it only if it is a regular file (not a symlink), embeds
the digest of the checkout it is installing from, and prints usage from
`provenant help`. The workshop host is not touched by agents; the user runs
these steps there.

## Verify

The crate owns routing, resolution and exec coverage. The stale-checkout
advisory remains covered against the binary in `tests/test_stale.py`:

```sh
cargo test --manifest-path tools/front-door/Cargo.toml
TMPDIR=/tmp .venv/bin/python -m pytest tests/test_stale.py -q
```

Python integration tests build through the existing front-door build helper
unless `PROVENANT_FRONT_DOOR_BIN` names a binary to use. CI supplies its stamped
release build. The Python parity oracle and its benchmark have been deleted;
git history retains them.
