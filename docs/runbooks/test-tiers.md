# Test tiers

Owner: repository maintainers
Anchor: scripts/verify-tier

Run `scripts/verify-tier` for T0 and `scripts/verify-tier --tier 1` for T1.
It prints the selected crates, integration targets and matching paths. Selection
compares committed and working-tree changes against `origin/main`. Configure the
base, workspace, commands, integration mappings, warning threshold and
runner hosts in `.provenant/verify.json`. `package_paths` maps non-Rust paths
that affect a crate's unit-test harness to that package for T0 selection. T0
formats, checks and runs unit tests for touched crates; T1 runs mapped
integration binaries. Add mappings as integration ownership is identified.

T2 runs the unpartitioned CI nextest suite with the same quarantine exclusions and runs its example test unconditionally. CI differs in three ways: it narrows a pull request's tests to the affected set, runs the example test only when affected, and reruns quarantined tests in a separate advisory step. T3 repeats the
suite and runs the advisory mutation script; the CI native jobs additionally
cover native-only environments. Heavy tiers warn locally and continue. Configure
an appropriate host for them.

Use `scripts/verify-tier --tier 2 --host ci` after pushing the commit to a
branch. The `ci` runner dispatches the `verify-tiers.yml` workflow using `gh`,
then waits for its conclusion and verifies its commit. The workflow must first
exist on the remote's default branch; GitHub credentials must permit workflow
dispatch and reading runs. The runner uses hosted CI and no model tokens.

A configured runner is an argument array with `{commit}`, `{tier}` and optional `{remote}` fields.
It must execute that pushed commit and return a final JSON line with `commit`,
`tier` (a string) and integer `exit_status`. It exits zero when it successfully
returns the receipt, including when the tier itself failed. Any prior stdout
lines are displayed as logs. The entry point checks the pushed branch tip,
receipt and transport status before reporting the outcome. Runners for peers
or cloud sessions may use the same contract. For an SSH peer, configure an
argument array such as `["scripts/verify-peer-runner", "peer-name",
"https://example.org/owner/repo.git", "{commit}", "{tier}"]`. It clones the
pushed repository into peer scratch, checks out the requested commit and removes
the checkout afterwards. The caller must trust the runner's attestation that it
checked out the reported commit.

The default tier commands build the selected test binaries under a build slot,
then reuse nextest metadata to execute them without holding that slot. This
uses scratch metadata rather than a second target directory. Other Cargo test
commands still hold their slot until they exit. Existing lanes use the shim
version installed at their launch; the split takes effect in newly launched
lanes after upgrading Provenant.

Set `test_threads` to a positive integer in `.provenant/verify.json` to cap
test threads for tier runs. The default is unset. A cap reduces concurrent test
load across lanes but can make each run slower; it does not reserve a shared
execution slot.

## Capture idle supervisor CPU on macOS

Build the custody fixture, then run its manual capture outside nextest's
30-second test timeout. From `tools/front-door`:

```sh
cargo test --release --locked -p provenant-supervisor --test custody -- --capture-idle-cpu
```

The capture launches its own guardian and a provider waiting on a socket. It
runs `/usr/bin/sample` against that guardian for ten seconds, then measures its
native `PROC_PIDTASKINFO` CPU counter over at least one minute. The JSON output
includes the build version, process counts, elapsed time, CPU counters and
percentage of one core. The sample stays in `TMPDIR`; retain findings in the
report before that directory expires. Finish by cancelling through custody and
settling the fixture's writer. No existing lane is stopped or signalled.

This uses the custody fixture's recorded boot identity and observer. It measures
the guardian's event loop and census backstop, without a configured host's disk
or graph callbacks or a valid sandbox memory snapshot. Use a live waiting lane
for those additional costs. A result below 3% is evidence for this fixture,
not a substitute for a production-lane capture when its costs differ.

To include the production `HostHolder` child reader, use the composition
capture. Build once and run the binary directly so a minute-long capture does
not hold a shared build slot:

```sh
cargo build --release --locked -p provenant-cli --example supervisor_cpu \
  --config 'profile.release.debug="line-tables-only"' \
  --config 'profile.release.strip=false'
cpu_capture_bin="$(cargo metadata --format-version 1 --no-deps | python3 -c \
  'import json,sys; print(json.load(sys.stdin)["target_directory"])')/release/examples/supervisor_cpu"
"$cpu_capture_bin" idle
"$cpu_capture_bin" churn
```

Both modes use the same native CPU counter and ten-second sample as the
custody capture. `idle` holds its provider on a socket; `churn` loops spawning
`/usr/bin/true`. Each guardian reads a real pending graph through `HostHolder`
at the ordinary census cadence. The JSON identifies the workload and whether
the Fabric child reader was present. The example uses synthetic admission and
terminal records, with no provider credentials, disk callback or sandbox memory
snapshot. Its directory project has no `.git` or `config.toml`, so these numbers
exclude Git probes and user TOML parsing. Measure those costs in a live Git lane
on a host that permits the native utility sandbox. The example verifies
cancellation, resolved cleanup, exit publication and
writer-lock release before returning. Compare identical release builds and
retain the process counts and native counter deltas with the before/after
percentages; the sample paths expire with `TMPDIR`.
