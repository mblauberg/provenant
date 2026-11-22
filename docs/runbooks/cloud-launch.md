# Cloud launch
Owner: provenant
Anchor: tools/front-door/src/cloud.rs; tools/front-door/crates/cli/src/cloud.rs

`provenant cloud launch` hands a brief to a provider cloud session, prints the
session link and exits. `provenant cloud collect` later puts the session's result
in a local worktree. Provenant keeps no lifecycle for the session: it does not
poll, retry, cancel or supervise it, and the provider's interface owns all of that.

## Set up a repository once

Do these steps in the provider's own interface. Keep credentials out of briefs,
commits and setup scripts.

1. **GitHub access.** For Claude, install the Claude GitHub app on the repository
   with contents and pull request write access, so the session can clone and push.
   For Codex, connect GitHub in ChatGPT and allow the repository.
2. **Environment.** Create a cloud environment with network access and a setup
   script that installs the toolchain the brief's checks need (for this repository,
   the Rust toolchain from `tools/front-door/rust-toolchain.toml` and
   `cargo-nextest`). Codex requires its name as `--environment`; Claude uses its
   hosted default when none is given.
3. **Secrets.** Add tokens the setup or checks need as the environment's variables
   or secrets, never in the repository, and only the least the session needs: a
   cloud session can read whatever its environment holds.
4. **Local CLIs.** Install and log in to `claude` or `codex` on the launching
   machine, and make sure `origin` names the intended repository.

## Launch

Push the base first: the session starts from the commit you have checked out and
cannot see a commit that is only local. Launch warns when `HEAD` is on no remote
branch.

```sh
provenant cloud launch --file brief.md
provenant cloud launch --file brief.md --provider codex --environment my-env
```

Keep the printed session link and collect command. Check progress in the provider
session, and repair a failed launch there before collecting.

## Collect

```sh
provenant cloud collect <key>
```

For Claude, collect looks for the named branch on `origin`. Hosted Claude may push
a `claude/*` branch instead, so it then looks for the `claude/*` branch whose head
carries the launch's trailer (`--branch` names another remote branch). It fetches
that branch and adds a worktree at the project's worktree-layout default, normally
`.worktrees/<branch>` beside the primary checkout (`--path` chooses another). For
Codex, collect applies the session's diff, staged but not committed, to a new
worktree at the launch commit.

When nothing has been pushed yet, collect fails and prints the session link; check
progress there and collect again. The collected branch goes through the usual pull
request and required CI.
