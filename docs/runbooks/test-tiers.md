# Test tiers
Owner: repository maintainers
Anchor: scripts/verify-tier

The tiers describe what each place runs; they are not a checklist for a lane.
A lane runs formatting only: it never compiles, seeds or admits a build. The
chair merges slices into one integration worktree and runs one warm host-owned
integrated check per epic. CI runs clippy and the tests on the integration pull
request. `.worktrees/.cargo/config.toml` is the single mechanism that disables
lane cargo.

`provenant verify` runs the tiers below through the chair's one host-owned
build slot; the host cap and slot hygiene are in
[configuration](configuration.md#named-admission-classes). How a repository's
stack and tiers are detected is in
[stack discovery](configuration.md#stack-discovery).

## Tiers

`scripts/verify-tier` runs the tiers this repository declares in the `verify` port
of `config/ports.json` (`provenant config ports` shows the effective port):

- `0`: format, check and lib and bin tests for touched crates.
- `1`: adds the mapped integration binaries.
- `2`: the unpartitioned CI nextest suite with quarantine exclusions.
- `3`: tier 2 plus the advisory mutation script.

It prints the selected crates, targets and paths, comparing committed and
working-tree changes against `origin/main`. Tier names are free-form strings; a
tier declares its commands as argv arrays, its selection (`packages`,
`integrations` or `all`) and whether to warn under host load. The `lane` nextest
profile skips fixtures that need host control (hard links, pathname sockets);
CI's `ci` profile runs them (`tools/front-door/.config/nextest.toml`).

`provenant verify [TIER]` runs one job whose single step is the verify engine, so
a tier is one hold under the host's slot. `--commit REV` verifies an exact
revision by pinning the commit in the slot. `--affected`, or the `verify` port's
`affected` policy ([selection](configuration.md#affected-package-selection)), passes `PROVENANT_VERIFY_SELECTION` (`packages`, `full`,
`reason`) to `scripts/verify-tier`, which uses it for `selection: packages`
tiers in place of its own path mapping; `full` selects every workspace member. A
selection error warns and the engine keeps its own selection.

See [CI](ci.md#pipeline) for pull request selection. A configured host can run
`scripts/verify-tier --tier 2 --host ci` against a pushed branch; the workflow
must exist on the remote default branch and the credential must permit dispatch
and reading runs.

A configured runner is an argument array with `{commit}`, `{tier}` and optional
`{remote}` fields, such as `scripts/verify-peer-runner` for an SSH peer. It must
execute that pushed commit, print logs, and end with a JSON line holding
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
