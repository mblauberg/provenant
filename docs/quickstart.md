# Quickstart

One runnable path from a fresh install to a reviewed change. Run these steps in
order. `provenant help COMMAND` prints a command's options and
`provenant help --all` the full catalogue. Dispatch runs on macOS arm64; other
hosts build from source with `--source`. The [install runbook](runbooks/install.md)
covers every option, upgrades and removal.

## 1. Install

```sh
./install.sh                                    # this tag's release, else latest
# or, from a checkout:
./install.sh --source /absolute/path/to/provenant
```

The shell installer verifies the release before running it, puts `provenant` on
PATH (`~/.local/bin` by default) and links the skills and instructions into your
detected harnesses. State defaults to `~/.local/share/provenant/v2`. A first
install prompts for providers, instruction files, hooks and accounts; `--yes`
skips those prompts. See the [install runbook](runbooks/install.md#install).

## 2. Verify the install

```sh
provenant check install
```

This compares the binary, assets, receipt and owned entries for every target and
reports drift without repairing it. Fix what it reports before dispatching work.

## 3. Register an account

Sign in with your provider's CLI first, then register that account directory.
Claude accounts live in `~/.claude-LABEL`; Codex uses `CODEX_HOME`.

```sh
CLAUDE_CONFIG_DIR=~/.claude-work claude          # sign in, then exit
provenant accounts register work --adapter claude --directory ~/.claude-work
provenant quota tick
```

`accounts register` grants the host access to that account; `quota tick`
initialises quota state. Repeat per account and provider.

## 4. Run a first read-only lane

```sh
provenant run explore --brief "List the commands that write install state and cite the file for each" --name map-install
provenant status map-install
provenant result map-install
```

`explore` maps an area and cites evidence; it cannot change your files. `status`
shows progress and `result` carries the handoff once the lane settles. A lane
invokes a real agent under the authority you give it, so start with work whose
output you can review. Never poll: block on `provenant wait` in the background
and carry on.

## 5. Run a first writer

A writer works in its own worktree on a branch you name.

```sh
provenant run implement --brief "Add a one-line note to notes.md describing this run" --name first-note --branch docs/quickstart-note --base HEAD
provenant status
provenant result first-note                     # once status shows it settled
```

`implement` commits, pushes its branch and hands off an outcome. Check the
commit, the paths it touched and the handoff before accepting anything. Use
`provenant worktree list` to see registered writer worktrees and
`provenant worktree remove` to clean up one you no longer need.

## Next

Read the [documentation index](README.md), the [architecture](ARCHITECTURE.md)
for lifecycle and boundaries, and the [work-item workflow](runbooks/github-workflow.md).
The [routing runbook](runbooks/routing-and-templates.md) explains how routes and
account settings are chosen.
