# Relocate the Provenant product checkout

Use this when the product checkout should live somewhere other than the
instance root. The instance remains the small, user-owned state directory
(normally `~/.agents`); the product checkout contains the shipped skills,
agents, workflows and runtime.

## Prepare and install

Create or clone the product checkout at its new absolute path. Before running
the installer, confirm that path and the instance root you intend to use. The
commands below assume the default instance `~/.agents` and that `provenant` is
on `PATH`:

```sh
cd /path/to/provenant
./scripts/install-harness --platform all
```

Use the product checkout's `.venv/bin/python`, or set `HARNESS_PYTHON` to an
equivalent Python 3.11+ environment with the harness test dependencies before
installing.

For a non-default instance or bin directory, pass the exact absolute paths
explicitly:

```sh
AGENT_FABRIC_INSTANCE_ROOT=/abs/instance \
PROVENANT_BIN_DIR=/abs/bin \
./scripts/install-harness --platform all

AGENT_FABRIC_INSTANCE_ROOT=/abs/instance /abs/bin/provenant root
```

Use `--platform claude` or `--platform codex` only when deliberately
installing one primary.

The installer checks source files, existing managed links, client configuration,
the stable `provenant` shim and instance seed inputs before changing anything.
It then projects the product surfaces and writes
`<instance-root>/.agent-fabric/product-root.json` last (normally
`~/.agents/.agent-fabric/product-root.json`). A directory-level link to an
older checkout is ambiguous and is refused; per-entry links are rebound only
when their installation receipt records the old target.

If a directory-level link to the old checkout is refused, restore that target
as a real directory or deliberately recreate the canonical link to the new
product checkout, then re-run the installer. Do not replace it automatically.

Verify the result from the new checkout:

```sh
./scripts/check-harness
"${PROVENANT_BIN_DIR:-$HOME/.local/bin}/provenant" root
```

The `all` invocation preflights both primaries before changing either one and
publishes the shared product pointer once. It is idempotent. If only one
primary is installed, use its individual platform instead.

## Roll back

Re-run `install-harness` from the previous product checkout while retaining the
same exact instance root and client homes. If it is not the default, pass the
same `AGENT_FABRIC_INSTANCE_ROOT=/abs/instance` override. This rewrites the
pointer and repairs managed projections. If the new install stopped before the
final pointer write, the previous pointer remains in effect; resolve the
reported conflict and re-run the installer.

## Final removal

Removing the old checkout is a separate, explicit operation. First verify that
the pointer, `provenant root`, client links and any active worktrees no longer
refer to it. Confirm the exact old path and separately authorise its removal;
the installer never deletes a product checkout or migrates live state.

Keep the directory containing `provenant` on `PATH`; the installer warns when
it is not present, but does not edit shell startup files.

The stable/local `provenant` command is normally a regular shim, so check it
separately from symlinked managed projections. For both primaries, inspect
their managed links for the old target before removing it:

```sh
old="/path/to/old/provenant"
provenant_bin="${PROVENANT_BIN_DIR:-$HOME/.local/bin}/provenant"
if [ -L "$provenant_bin" ]; then
  realpath "$provenant_bin"
elif [ -f "$provenant_bin" ]; then
  grep -F "$old" "$provenant_bin" || true
else
  echo "missing Provenant command: $provenant_bin"
fi
for root in "${CLAUDE_CONFIG_DIR:-$HOME/.claude}" "${CODEX_HOME:-$HOME/.codex}"; do
  [ -d "$root" ] || continue
  find "$root" -type l -exec sh -c '
    for link do printf "%s -> %s\n" "$link" "$(realpath "$link")"; done
  ' sh {} +
done | grep -F "$old" || true
```

If `realpath` is not provided by the platform, use its equivalent (for
example, a small `python3` `Path.resolve()` check) so relative links are
resolved before comparing them.
