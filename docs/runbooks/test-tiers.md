# Test tiers
Owner: repository maintainers
Anchor: scripts/verify-tier

The tiers describe what each place runs; they are not a checklist for a lane.
A lane runs `cargo check` and clippy for touched crates on the configured
`CARGO_TARGET_DIR` (the worktree's own seeded target by default), plus tests filtered to changed
code only when that target is warm. It never starts a cold build. Set `CARGO_BUILD_BUILD_DIR` to the same warm directory when it is set,
so Cargo does not put intermediate build output in a separate cold directory. The whole workspace suite runs in CI. `lanes.verification` defaults to
`targeted` and may be overridden by host or project configuration or a brief's
`Verification scope:` line. The preamble names the source.

Compiling cargo commands and targeted test runs share the host-wide `build`
admission class, configured through `[admission.build].slots` and derived from
CPU cores and RAM when unset. Set it to `0` to disable the cap. Busy slots wait
a bounded time, then warn and proceed. Cargo serialises access to one target
with its own locks. `lanes.local_cargo` can warn when a command exceeds a host's
preferred scope; it does not refuse the command. By default,
`lanes.cargo_target` is `private`, the worktree's own target cloned warm from
the host seed. `project` uses the worktree's `target/`, or an absolute path
can be provided; never give several worktrees one target (see the run spec).

`scripts/verify-tier` runs the built-in `0` tier (format, check and unit tests
for touched crates), `1` adds mapped integration binaries, `2` runs the
unpartitioned CI nextest suite with quarantine exclusions, and `3` repeats it
with the advisory mutation script. It prints the selected crates, targets and
paths, comparing committed and working-tree changes against `origin/main`.
Base, workspace, commands, integration mappings, `package_paths` (non-Rust
paths that affect a crate's unit tests), `test_threads` and runner hosts are set
in the `verify` port in `config/ports.json` (also exposed through the optional `.provenant/ports.json` link). Tier names are free-form strings;
each tier declares commands as argv arrays with a stack and project-relative cwd,
package/integration/all selection, and whether
to warn under host load. Host-load warnings continue locally.

See [CI](ci.md#pipeline) for pull request selection. A configured host can
run `scripts/verify-tier --tier 2 --host ci` against a pushed branch; the
workflow must exist on the remote default branch and the credential must permit
dispatch and reading runs.

A configured runner is an argument array with `{commit}`, `{tier}` and optional
`{remote}` fields, such as `["scripts/verify-peer-runner", "peer-name",
"https://example.org/owner/repo.git", "{commit}", "{tier}"]` for an SSH peer. It
must execute that pushed commit, print logs, and end with a JSON line holding
`commit`, `tier` (a string) and integer `exit_status`. It exits zero whenever it
returns the receipt, even if the tier failed. The caller trusts the runner's
attestation that it checked out the reported commit.

## Idle supervisor CPU on macOS

To measure the guardian's idle cost, run the custody fixture's manual capture
outside nextest's 30-second timeout, from `tools/front-door`:

```sh
cargo test --release --locked -p provenant-supervisor --test custody -- --capture-idle-cpu
```

It runs `/usr/bin/sample` for ten seconds, then measures the guardian's CPU
counter over at least a minute, and prints JSON with percent of one core; the
sample stays in `TMPDIR`, so keep findings in your report. It uses synthetic
records with no disk callback, Git probe or sandbox snapshot, so below 3% is
evidence for the fixture, not for a production lane; use a live waiting lane for
those costs. The `supervisor_cpu` example (`idle` and `churn` modes) adds the
production `HostHolder` reader: build it once with `cargo build --release
--locked -p provenant-cli --example supervisor_cpu` and run the binary directly
so a long capture does not hold a build slot. Compare identical release builds.

Stack detection is read-only: `provenant config detect` shows proposed Rust and
Node commands and their working directories. Rust uses the nearest workspace or
package manifest within two levels, skipping equal-depth ties. Node uses the
root `package.json`, its package manager field or lockfile, and only scripts
that exist. The detected default and commands are below all configured layers and replace
builtin commands for matching tier keys. Whole argv elements `{packages}`,
`{integration_args}` and `{quarantine_args}` expand to argument lists; no
shell splitting occurs. Detected Rust-only tiers retain builtin selection; tiers containing non-Rust commands select all and expand selection placeholders to nothing. Use
`provenant config show --full` for effective leaves and sources, and
`provenant config path --project` for the host-project ports file.
