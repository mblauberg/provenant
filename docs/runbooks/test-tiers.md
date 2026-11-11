# Test tiers
Owner: repository maintainers
Anchor: scripts/verify-tier

The tiers describe what each place runs; they are not a checklist for a lane.
Where the repository has CI, a lane runs one cheap compile check (`cargo check`
on the package it changed), pushes, and leaves T0 to T3 to CI. Set
`lanes.local_cargo = "check"` (or `none`) under `[knobs]` for the host,
`--set 'attempt.local_cargo="check"'` for a run, or `attempt.local_cargo` on a
graph node, and the lane's `cargo` shim refuses anything heavier.

`scripts/verify-tier` runs T0 (format, check and unit tests for touched crates);
`--tier 1` adds mapped integration binaries, `--tier 2` the unpartitioned CI
nextest suite with the same quarantine exclusions, and `--tier 3` repeats it with
the advisory mutation script. It prints the selected crates, targets and paths,
comparing committed and working-tree changes against `origin/main`. Base,
workspace, commands, integration mappings, `package_paths` (non-Rust paths that
affect a crate's unit tests), `test_threads` and runner hosts are set in
`.provenant/verify.json`. Heavy tiers warn locally and continue, so run them on
a host with spare capacity.

CI differs from T2 in that it narrows a pull request to the affected set, runs
the example test only when affected, and reruns quarantined tests in an advisory
step. `scripts/verify-tier --tier 2 --host ci` dispatches `verify-tiers.yml`
with `gh` against the pushed branch tip and waits; the workflow must already be
on the remote's default branch, and the credential must permit dispatch and
reading runs.

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
