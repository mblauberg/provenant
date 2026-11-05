# Test tiers

Owner: repository maintainers
Anchor: scripts/verify-tier

Run `scripts/verify-tier` for T0 and `scripts/verify-tier --tier 1` for T1.
It prints the selected crates, integration targets and matching paths. Selection
compares committed and working-tree changes against `origin/main`. Configure the
base, workspace, commands, integration mappings, warning threshold and
runner hosts in `.provenant/verify.json`. T0 formats, checks and runs unit
tests for touched crates; T1 runs mapped integration binaries. Add mappings as
integration ownership is identified.

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
