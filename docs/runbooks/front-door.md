# Front door runbook

How the `provenant` command is built, installed, checked and rolled back. The
decision is [ADR 0027](../adr/0027-rust-front-door.md).

## Choose what gets installed

`scripts/install-harness` reads `PROVENANT_FRONT_DOOR`:

| Value | Effect |
|---|---|
| `auto` (default) | Build the Rust binary if the crate and `cargo` are present and the smoke run passes; otherwise install the Python stub and say why on stderr. |
| `rust` | Require the binary. Exit 3 if it cannot be produced. |
| `python` | Install `scripts/provenant.template`. |

Any other value exits 3 before anything changes. The installer prints
`front-door=rust` or `front-door=python` after the command line.

## Roll back to the Python stub

```sh
PROVENANT_FRONT_DOOR=python scripts/install-harness
```

The installer treats the old binary as a managed file (it carries the marker
line) and replaces it atomically, mode 0755.

## Rebuild after a source change

Changing anything under `tools/front-door/` makes the installed binary stale.
`scripts/check-provenant-install.py` fails with "built from different sources".
Re-run `scripts/install-harness` (with `PROVENANT_FRONT_DOOR=rust` to insist).

## Supply a prebuilt binary

For a host without a toolchain, build on a machine that has one:

```sh
cd tools/front-door
PROVENANT_FD_SOURCE_SHA256="$(python3 ../../scripts/lib/front_door_build.py digest .)" \
  MACOSX_DEPLOYMENT_TARGET=25.0 cargo build --release --locked
```

Copy `target/release/provenant` to the host as an absolute path, then on that
host:

```sh
PROVENANT_FRONT_DOOR=rust PROVENANT_FRONT_DOOR_BINARY=/abs/path/provenant scripts/install-harness
```

The installer accepts it only if it is a regular file (not a symlink), embeds
the digest of the checkout it is installing from, and prints usage from
`provenant help`. The workshop host is not touched by agents; the user runs
these steps there.

## Run the parity suite

```sh
.venv/bin/python -m pytest tests/front_door_parity -q                 # Python reference
PROVENANT_PARITY_IMPL=rust .venv/bin/python -m pytest tests/front_door_parity -q
```

`PROVENANT_FRONT_DOOR_BIN` overrides the binary path
(default `tools/front-door/target/release/provenant`).
`PROVENANT_PARITY_REFERENCE_ROOT` names a tree whose Python dispatcher is the
byte reference for help text. Cases for `lint` and `lease` skip while the
reference dispatcher lacks those verbs.

## Compare start-up time

```sh
.venv/bin/python tests/front_door_parity/bench_front_door.py --impl python --out python.json
.venv/bin/python tests/front_door_parity/bench_front_door.py --impl rust --out rust.json
.venv/bin/python tests/front_door_parity/bench_front_door.py --compare python.json rust.json
```

This reports medians; it is not a pass or fail gate.
