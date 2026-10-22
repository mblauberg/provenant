# ADR 0027 - Replace the two-layer Python front door with one Rust binary

**Status:** Accepted 2026-10-22 (user direction in session: change the whole
front door to Rust at once, on a separate branch)

**Amends:** [ADR 0013](0013-thin-provenant-cli.md). Operations:
[`docs/runbooks/front-door.md`](../runbooks/front-door.md).

## Context

`provenant` is the command every agent and the user type first, so its start-up
time is paid on every call. Today it is two Python layers. The installed stub
(`scripts/provenant.template`, copied to `~/.local/bin/provenant`) finds the
product checkout and execs the checkout dispatcher (`scripts/provenant`), which
routes the verb to its owner script. Each layer starts an interpreter. Measured
on the primary Mac, `provenant root` takes about 60 ms and `provenant help`
about 120 ms, against a fork-and-exec floor of about 6 ms
(`tests/front_door_parity/bench_front_door.py`).

## Decision

1. **One std-only Rust binary** at `tools/front-door/` replaces both layers. It
   performs product-root resolution, verb routing, `help`, `root`, the
   stale-checkout advisory and the exec of the owner script. It has no crate
   dependencies, so there is nothing to audit but the standard library.
2. **One binary, two roles.** Installed outside any checkout it behaves as the
   stub; copied to `<checkout>/scripts/provenant` it behaves as the dispatcher.
   The role comes from where it sits, as it does for the Python pair.
3. **Behaviour is pinned first.** `tests/front_door_parity/` states the front
   door contract as executable assertions and passes against the Python
   implementation. The Rust binary must pass the same suite
   (`PROVENANT_PARITY_IMPL=rust`) before it is installed by default.
4. **Python stays the fallback and the oracle.** `scripts/provenant.template`
   and `scripts/provenant` remain until the Rust path has run in production long
   enough to retire them. `install-harness` picks the front door with
   `PROVENANT_FRONT_DOOR=auto|rust|python`. `auto` installs the binary when
   `cargo` builds it and the smoke run passes, and otherwise installs the Python
   stub without failing.
5. **Identity is the source digest.** The binary embeds the managed-file marker
   and `provenant-front-door source-sha256=<hex>` as lines of their own. The
   digest covers `Cargo.toml`, `Cargo.lock`, `rust-toolchain.toml` and `src/**`.
   `scripts/check-provenant-install.py` accepts the template byte for byte, or a
   binary whose digest equals the checkout's, and otherwise asks for a rebuild.
   The existing installer's marker rule classifies a binary unchanged.
6. **`check` custody stays in Python for now.** Registered-checkout custody for
   `provenant check` is security-relevant and already tested. The binary calls
   `scripts/check_target.py`, which loads the Python dispatcher and returns the
   checker and root, so no custody rule is written twice.
7. **Platforms.** The minimum is macOS 27, which both of the user's Macs run.
   macOS 25 and 26 are cheap, so darwin builds set
   `MACOSX_DEPLOYMENT_TARGET=25.0`. Linux is built and tested in CI. The
   toolchain is pinned by `rust-toolchain.toml` (1.94.1).

## Intentional divergences from the Python front door

- **D1.** `help` and usage need no resolvable product root. The Python stub
  exits 3 before printing help when no root resolves.
- **D2.** Pointer errors use the stub-style wording in both roles.
- **D3.** JSON in pointer files is parsed strictly; `NaN` and similar are
  rejected.

## Consequences

- Start-up drops from interpreter cost to process cost.
- A machine without `cargo` keeps working on the Python stub. A prebuilt binary
  can be supplied with `PROVENANT_FRONT_DOOR_BINARY`; it is accepted only if its
  embedded digest matches the checkout.
- A source change in `tools/front-door/` makes an installed binary stale until
  `install-harness` is re-run; `check-provenant-install.py` says so.
- CI gains a `rust` job: `cargo fmt --check`, `clippy -D warnings`, `cargo test`,
  a release build and the parity suite against that build.
- There is no macOS CI leg. macOS behaviour is checked on the user's Macs.

## Not decided here

Moving `check` custody, `lint` and `lease` routing details, or the owner
scripts themselves into Rust. Each needs its own decision.
