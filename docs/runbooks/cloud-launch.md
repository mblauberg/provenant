# Cloud launch
Owner: provenant
Anchor: tools/front-door/src/cloud.rs; tools/front-door/crates/cli/src/cloud.rs

`provenant cloud launch` hands a brief to a provider cloud session, prints
the session link, and exits. `provenant cloud collect` later puts the
session's result in a local worktree. Provenant keeps no lifecycle for the
session. It does not poll, retry, cancel or supervise it. The provider's
interface owns all of that. Graphs, routes and `--set` cannot place work in
the cloud; a node naming `attempt.cloud` is refused.

## Set up a repository once

Do these steps in the provider's own interface. Do not put credentials in
briefs, commits or setup scripts.

1. **GitHub access.** For Claude, install the Claude GitHub app on the
   repository and grant it contents and pull request write access, so the
   session can clone and push. For Codex, connect GitHub in ChatGPT and allow
   the repository.
2. **Environment.** Create a cloud environment for the repository: network
   access and a setup script that installs the toolchain the brief's checks
   need (for this repository, the Rust toolchain from
   `tools/front-door/rust-toolchain.toml` and `cargo-nextest`). Note the
   environment name: Codex requires `--environment`; Claude uses its hosted
   default when none is given.
3. **Secrets.** Add any tokens the setup or the checks need as the
   environment's variables or secrets, never in the repository. Keep them to
   the least the session needs; a cloud session can read whatever its
   environment holds.
4. **Local CLIs.** Install and log in to `claude` or `codex` on the machine
   that launches. Make sure `origin` names the intended repository.

## Launch

Push the base first: the session starts from the commit you have checked out,
and it cannot see a commit that is only local. Launch warns when `HEAD` is on
no remote branch.

```sh
provenant cloud launch --file brief.md
provenant cloud launch --file brief.md --provider codex --environment my-env
```

Launch appends a short hand-off to the brief: start from that commit, push to
the named branch (`--branch`, default `cloud/<nonce>`), and end the last
commit message with a `Provenant-Launch: <nonce>` line. It prints the session
ID, the link and the collect command. It writes one small record under the
repository's Git directory (`<git-common-dir>/provenant/cloud/<key>.json`)
holding what `collect` needs; Provenant's state directory is not touched.

## Collect

```sh
provenant cloud collect <key>
```

For Claude, collect looks for the named branch on `origin`. Hosted Claude may
push a `claude/*` branch instead, so collect then looks for the `claude/*`
branch whose head carries the launch's trailer. It fetches that branch and
adds a worktree at `.worktrees/<branch>` beside the main checkout (`--path` to
choose another). For Codex, collect applies the session's diff, staged but
not committed, to a new worktree at the launch commit.

When nothing has been pushed yet, collect fails and prints the session link.
Open it to see progress, then run collect again. The collected branch goes
through the usual pull request and required CI like any other branch.
