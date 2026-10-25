# ADR 0027 - Replace the two-layer Python front door with one Rust binary

> Legacy source references below describe the pre-rewrite implementation.
> Use `provenant help` for current Rust commands and the accepted
> [Rust designs](../specs/README.md) for owners.

**Status:** Accepted 2026-10-22 (user direction in session: change the whole
front door to Rust at once, on a separate branch)

**Amends:** [ADR 0013](0013-thin-provenant-cli.md). Operations:
[`docs/runbooks/install.md`](../runbooks/install.md).

## Amendment — 2026-10-23

The user dropped the seven-day trials for every Rust supervisor slice. Once a
slice's conformance rows are green, switch its default and delete its Python
path and tests. Rollback is a git revert; Python is restored from history if
needed. S1a (#1231) switched both Macs to Rust; S1b removes the Python front
door, installer fallback and parity oracle. This amends the original trial and
no-toolchain fallback decision; the current decision below incorporates it.

## Context

`provenant` is the command every agent and the user type first, so its start-up
time is paid on every call. Before this decision it had two Python layers. The
installed stub at `~/.local/bin/provenant` found the product checkout and execed
the Python checkout dispatcher, which routed the verb to its owner script.
Each layer started an interpreter. Measured on the primary Mac, `provenant root`
took about 60 ms and `provenant help` about 120 ms, against a fork-and-exec floor
of about 6 ms
(the retired parity benchmark, retained in git history).

## Decision

1. **One std-only Rust binary** at `tools/front-door/` replaces both layers. It
   performs product-root resolution, verb routing, `help`, `root`, the
   stale-checkout advisory and the exec of the owner script. It has no crate
   dependencies, so there is nothing to audit but the standard library.
2. **One binary, two roles.** Installed outside any checkout it behaves as the
   stub; copied to `<checkout>/bin/provenant` it behaves as the dispatcher.
   The role comes from where it sits, as it does for the Python pair.
3. **Behaviour was pinned first.** The Rust binary passed the Python parity
   suite before it became the default. Rust crate tests now own that coverage;
   `tests/test_stale.py` keeps the behind-origin advisory against the binary.
4. **Rust is the only front door.** The Python dispatcher, installed template
   and parity oracle are deleted. `install-harness` builds Rust by default and
   exits 3 with a rustup `fix:` line when cargo is missing. A matching prebuilt
   binary remains supported. Rollback is a git revert and reinstall.
5. **Identity is the source digest.** The binary embeds the managed-file marker
   and `provenant-front-door source-sha256=<hex>` as lines of their own. The
   digest covers `Cargo.toml`, `Cargo.lock`, `rust-toolchain.toml` and `src/**`.
   `scripts/check-provenant-install.py` accepts a binary whose digest equals the
   checkout's, and otherwise asks for a rebuild.
   The existing installer's marker rule classifies a binary unchanged.
6. **`check` custody stays in Python for now.** Registered-checkout custody for
   `provenant check` is security-relevant and already tested. The binary calls
   `scripts/check_target.py`, which owns the Python custody functions and
   returns the checker and root.
   Superseded by the Rust rewrite (state.md): the binary selects the caller's
   checkout itself and `scripts/check_target.py` is deleted.
7. **Platforms.** The minimum is macOS 27, which both of the user's Macs run.
   macOS 15 and 26 are cheap, so darwin builds set
   `MACOSX_DEPLOYMENT_TARGET=15.0`. Linux is built and tested in CI. The
   toolchain is pinned by `rust-toolchain.toml` (1.94.1).

## Intentional divergences from the Python front door

- **D1.** `help` and usage need no resolvable product root. The Python stub
  exits 3 before printing help when no root resolves.
- **D2.** Pointer errors use the stub-style wording in both roles.
- **D3.** JSON in pointer files is parsed strictly; `NaN` and similar are
  rejected.

## Consequences

- Start-up drops from interpreter cost to process cost.
- A machine without `cargo` needs rustup or a prebuilt binary. The latter
  can be supplied with `PROVENANT_FRONT_DOOR_BINARY`; it is accepted only if its
  embedded digest matches the checkout.
- A source change in `tools/front-door/` makes an installed binary stale until
  `install-harness` is re-run; `check-provenant-install.py` says so.
- CI gains a `rust` job: `cargo fmt --check`, `clippy -D warnings`, `cargo test`,
  a release build and stale-checkout integration tests against that build.
- Lane conformance runs in Linux and macOS arm64 CI for tools and lane changes.

## Not decided here

Moving `check` custody, `lint` and `lease` routing details, or the owner
scripts themselves into Rust. Each needs its own decision.
