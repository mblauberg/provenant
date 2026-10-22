# Fabric

Fabric provides a project mailbox, task ledger and a thin MCP front door to the
Python orchestration owners. Full provider output stays in run files.
One SQLite file, no daemon, no setup.

## Start

```sh
provenant fabric whoami
runtime/fabric/bin/fabric-mcp
```

`bin/fabric` and `bin/fabric-mcp` run TypeScript directly with `tsx`;
there is no build to keep in step with the source. Node 24.15 or newer within
major version 24 is required. `FABRIC_NODE`, `AGENT_FABRIC_TSX_LOADER` and
`AGENT_FABRIC_PRODUCT_ROOT` select the runtime, loader and product checkout.
Restart an existing MCP connection after changing source. `fabric_whoami`
reports `server_version`, `build_stale` and a restart fix when needed.
`install-harness --platform all` registers Fabric for detected client homes;
each client has its own seat and inbox, and a detected client with conflicting
instructions is skipped and reported. `check-provenant-install.py` warns about
routing drift (`--strict` fails); `install-harness` refreshes the instance
catalogue whenever a product snapshot exists, preserving three-way instance
overrides. Without a snapshot it skips and prints a hint; `--refresh-routing`
forces a refresh.

## MCP quickstart

```text
fabric_dispatch{prompt:"Review the change",adapter:"codex",model:"luna",effort:"high"}
fabric_status{ids:["mcp-a81f3c"],wait_seconds:55}
fabric_output{id:"mcp-a81f3c",part:"result",max_bytes:4000}
```

Copy the returned `Route:` line for provenance. `content` contains the owner's
verbatim digest; `structuredContent` contains minimal `fabric.status.v1` rows by
default. `detail: full` includes attempt history, evidence and provenance. A small
formatter supports older receipts when no digest exists. Request errors contain
one line with `fix:`. Recognised field names and mode synonyms are corrected;
model names accept case and punctuation variants, and a typo is corrected with a
warning when exactly one name is close and its version numbers match. Anything
else is rejected with the closest valid choices. Common spellings are normalised
with a warning rather than rejected: `run_id` or `task_id` for `id`, `id` for a
dispatch `task_id`, numeric strings such as `wait_seconds:"30"`, and a numeric
`until:"55"` (read as `wait_seconds`). Relative `cwd` and `worktree` paths resolve from
the caller directory.
Every shell subcommand accepts `--help` or `-h` and exits successfully before
opening the store or validating command arguments.
No provider output is embedded in status responses.

Without an MCP connection, `provenant fabric dispatch --adapter A --model M
--effort E --mode read_only|worktree_write --prompt-file F [--wait]` uses the
same dispatcher. Add `--worktree P`, `--cwd P` or `--id ID` as needed. Use
`--tasks F` for a JSON object with `tasks[]` and shared route fields. It prints
the Fabric run id and status. `--wait` waits up to 55 seconds; `lanes --wait`
waits for the finish. `--session NAME [--fresh]` runs one turn of a named
session, and `fabric session list | inspect NAME | forget NAME` manages names.

Owners are detached session leaders and outlive the CLI and MCP host. A
CLI-started owner is recorded as its own host. An MCP host that closes or is
signalled hands its runs to their owners rather than cancelling them. A fresh
`dispatch list` and `status <run-dir>` resolve these runs from their records.
Only a live host sends the inbox notice when a run finishes. `fabric_events`
exposes retained task-state events. `lanes --wait` reports each terminal or
input-required lane of the registered project, finished within the last day,
once per seat, including lanes that finished between waits; named ids narrow it
to those lanes, and a batch id to its tasks. `lanes --wait --all` holds the report
until every listed lane is terminal or needs input, `--summary` prints one bounded line per lane (at most 20, 160 characters each) at the end instead of as lanes land, and `--timeout N` ends the
wait with exit 124 naming the lanes still running (`--timeout 0` polls once; contention on the seen-cursor is bounded by it too, and unmarked lanes are reported again next wait). An unknown id fails the wait at once with the read error (exit 1) rather than being skipped. `lanes` default output shows active lanes, lanes needing attention (such as quarantined or input-required), and the 5 most recent terminal ones, then `N more; use --all`; `lanes --json` without `--all` caps at 20 rows with `omitted`/`omitted_hint`; `--all` lists all lanes. `lanes --project P` reads
another project; an empty listing says which project the cwd resolved to.
The next dispatch reaps a run only when its host is gone and its owner or
provider still runs: an owner that exited and left its provider behind, or a
run whose MCP host was killed with SIGKILL.

Eighteen tools are registered by default:

| Tool | Purpose |
| --- | --- |
| `fabric_dispatch` | One prompt, `tasks[]`, `resume`, `handoff` or a named `session` |
| `fabric_session` | `action: inspect`, `list` or `forget` a named session |
| `fabric_status` | Read run/task/batch IDs; bounded wait for `any` or `all` |
| `fabric_runs` | Versioned run and lane list with root-relative paths |
| `fabric_events` | Cursor-based terminal, input-required and inbox events |
| `fabric_cancel` | Cancel the owner and provider group |
| `fabric_output` | Bounded result, stderr, events or receipt slice |
| `fabric_hosts` | `action: list` or `doctor`, optional `hosts`; same SSH surface for every chair |
| `fabric_adapters` | Compact catalogue, CLI availability and guarantees; `models` lists live ids |
| `fabric_whoami` | Seat, project and server freshness |
| `fabric_send` | Send to a seat, team, chair or all |
| `fabric_inbox` | Peek headers or claim selected messages |
| `fabric_acknowledge` | Acknowledge using the claim token |
| `fabric_note` | Append activity |
| `fabric_activity` | Read recent activity or continue after a cursor |
| `fabric_task` | `action: create`, `claim`, `update` or `list` |
| `fabric_work_claim` | Advisory issue or path claim |
| `fabric_landing_lease` | Repository landing lease |

Dispatch accepts exactly one of `prompt` and `prompt_file`. Route controls are
`adapter`, `alias`, `model`, `effort`, `mode`, `worktree`, `cwd`, `network`,
`sandbox`, `capabilities`, `add_dirs`, `fallback` and `allow_secrets`. Pool
controls `route`, `rotate`, `council` and `models` pick the adapter and model
from a global route pool; `adapter` with `route` narrows the pool to that
adapter. A council runs as a batch with one task per member (CLI: `--route`,
`--rotate`, `--council N`, `--models a/m@e,b/m`). Mixed selectors settle by
precedence with a warning, never a rejection: `models` beats everything, an
explicit `model` beats `route`, `route` beats `alias`, and `council` beats
`rotate`. `confidential` (`--confidential`) works with any selector and keeps
every attempt, fallbacks included, off free and prompt-training models. See
`skills/orchestrate/references/routing-and-tiers.md#route-pools`. Before dispatch, Fabric scans
the prompt and eligible files under `add_dirs` for common live credential shapes.
A finding rejects with `error: secret_detected` and a location in `fix`; set
`allow_secrets: true` explicitly to proceed. The attempt records the override
and finding names. The scan reads whole files, skipping Git-ignored, vendored and binary files
and directories listed under `secret_scan_exclude` in `.agents/fabric-policy.json`
(which must resolve inside the project). Past 10,000
files or 64 MiB, dispatch rejects with `error: secret_scan_budget_exceeded`
naming the largest scanned subtree and up to three largest files encountered,
including the file that exceeds the budget. The scan also skips `__pycache__`,
`.pytest_cache`, `.mypy_cache`, `.ruff_cache`, `.next` and `.turbo` caches.
Narrow `add_dirs`, exclude the subtree, or explicitly set
`allow_secrets: true` and explain why in the prompt. The same policy's
`dispatch_defaults` may set `add_dirs`, `timeout_seconds` and `network` (Codex
only) for dispatches that leave them unset. Writers use `mode: worktree_write` and an
owned, registered linked worktree. The primary checkout is refused; create a linked
worktree. `cwd` selects an existing read-only directory inside any registered
Fabric project, and `prompt_file` may sit in the caller's directory or any
registered project, or a caller temporary directory (`TMPDIR`, `/tmp`,
`/private/tmp` or `/private/var/folders`). Credential stores and secret paths
remain refused. A project is registered once any Fabric command, such as
`fabric whoami`, has run inside it; an unregistered path is rejected with that
route or the alternative of dispatching from the other project. For a
directory outside the caller's, Fabric passes the owner a read root. The run
stays in the caller's run root, where `lanes` and `status` find it. Dispatch
records the cwd-derived project per task, so `lanes` and `lanes --wait` in that
project also find the task, including from its registered worktrees. Dispatch
writes a pointer under the target project's `.agent-run/runs/pointers/`;
discovery reads its own run root and these pointers, corroborating tasks and
routes against the source owner record or receipts. The pointer is
best effort: when the target run root cannot be written, dispatch still
starts, records a `lane_pointer_failed` warning in the run's
`dispatch-status.json`, and the lane is listed only from its source project. A
pointer is pruned only when its source run directory is gone, so the source
reaper's retention rules decide its lifetime; a pointer whose source no longer
maps any pointed task to this project (for example after a resume moved it) is
skipped but kept, since a later resume may move the task back. An imported
row shows its run, result and receipt paths absolute under the source run
root. `fabric_status` and `fabric_output` by id still resolve from the source
project only. A run that
finished outside the 24-hour lanes window is skipped before its receipts are
read unless history is requested. Recorded cwd projects are compared after
resolving symlinks. Other tasks in a mixed-project batch stay private to
their projects. Runs created before this change stay under the chair's project;
there is no registry-wide backfill. A resume recomputes the resumed task's
project from its effective cwd and writes its target pointer. The secret scan
still covers the prompt.
A prompt in a credential or authentication
store is refused wherever it sits. A resume reuses the saved read roots only
while each still resolves to the same directory in a registered project. The Python owner validates provider capabilities and
applies controls; Fabric does not claim a stronger guarantee than its receipt.
On macOS, non-Codex read-only launches use `sandbox-exec` when available. The
profile limits writes to the attempt directory and provider state. It denies
reads of home, shared temp and the workspace outside `cwd` and `add_dirs`, with
provider sign-in paths re-allowed. Each read-only profile also reads the provider's
own executable: the PATH entry, every link hop to the real file and, when that file
sits in an install folder rather than a plain `bin` directory (cursor-agent's
`~/.local/share/cursor-agent/versions/<version>`), that folder. Training routes also deny reads of protected
paths. OpenCode's profile also reads, by their unresolved names,
`opencode.json`, `opencode.jsonc`, `AGENTS.md`, `CLAUDE.md` and `.opencode/`
in each directory from `cwd` up to the repository root, since OpenCode loads
them at startup. Every read-only profile also reads the toolchain a lane runs
tests with. Candidates are `python3`, `python`, `uv` and `node` on the
provider's PATH, plus each `.venv` between `cwd` and the repository root, its
`bin/python` link and the interpreter its `pyvenv.cfg` names. Repository
content only names candidates; it never grants a path itself. A candidate is
granted only when it resolves to a real install. That means a regular
executable with a toolchain name, free-threaded `python3.14t` included, in
the `bin` of a prefix with that toolchain's `lib/python3.*` or
`lib/node_modules`. The prefix must not be home or above it, and must not be or
hold a credential store. Only the resolved executable and the prefix's
`lib`, `include` and `libexec` are granted, never the prefix itself, so a file
beside them stays unreadable. A framework build's prefix
(`Python.framework/Versions/X.Y`) also grants its `Python` library, which dyld
opens at launch, and `Resources`. Every component must be a real entry, not a
link, strictly below the prefix, so a link to `.` cannot grant the prefix.
Each is emitted as the canonical path checked. A
resolved `uv` binary is granted alone. When it is present, `pyproject.toml`, `uv.toml`, `uv.lock`
and `.python-version` above `cwd` are readable by unresolved name. Nothing is
written, and nothing is run to find these paths. Codex read-only uses its native read-only sandbox, which reads
everywhere and writes only the attempt's `TMPDIR` and `add_dirs` (a shared
lock directory, say). An `add_dir` holding `cwd`, or one whose name has a
character Codex reads as a pattern (`*?[]{}`), stays read-only and warns.
Inside another sandbox, where macOS refuses a nested one, the attempt records
an explicit unconfined-write warning. Without `sandbox-exec` read confinement,
a non-Codex `cwd` below the root warns that it is not a read boundary.
Every writer, Codex included, has the same Git write boundary: its own
per-worktree Git directory (`git rev-parse --absolute-git-dir`) plus the
common directory's `objects`, `refs` and `logs` and its `packed-refs`,
`packed-refs.lock` and `packed-refs.new` files. The rest of the common
directory, including `hooks`, `config`, `info` and other lanes'
`worktrees/<name>`, is never writable, because Git later runs what it names in
the primary checkout and every lane. A Codex writer's `add_dir` at or inside
the common directory is dropped with a warning. Codex writers also keep the
worktree `.git` marker and the private Git directory's `config.worktree`,
`commondir` and `gitdir` read-only. Writer attempts set `gc.auto=0`,
`maintenance.auto=false` and `rerere.enabled=false` through
`GIT_CONFIG_COUNT`, since those would write to the common directory. The
shared `config` is read-only, so a lane cannot record an upstream,
add a remote or add a worktree. `git push -u` still pushes but prints a
config-lock error; push with `git push origin HEAD` and open the pull request
with `gh pr create --head <branch>`.

Codex writers also receive write access to repository `.agent-run/runs/` and
`fabric.sqlite3`, `fabric.sqlite3-wal`, `fabric.sqlite3-shm` and
`fabric.sqlite3-journal` in `AGENT_FABRIC_STATE_DIRECTORY` (default
`~/.local/state/agent-harness/fabric`). Both sandbox profiles check these paths
like `add_dirs`; the configured state directory survives fresh and resumed
writer launches. Sessions and the Fabric catalogue receive no automatic grant.
Wrapped writer runs on macOS (agy, Claude, Cursor, OpenCode and Kiro) use
`sandbox-exec` to restrict writes to their worktree, declared `add_dirs`,
the Git write boundary, attempt files, device nodes and provider state. Where
protected-path policy applies, its read and write denies still take precedence
inside an `add_dir`.
Each attempt sets `TMPDIR`, `TMP` and `TEMP` to `<attempt>/tmp`, zsh's
`TMPPREFIX` to `<attempt>/tmp/zsh` so here-documents work in a confined lane, and
`XDG_CACHE_HOME` to `<attempt>/tmp/cache`, and `COREPACK_HOME` to
`<attempt>/tmp/cache/node/corepack`, `UV_CACHE_DIR` to `<cache>/uv` and
`npm_config_cache` to `<cache>/npm`, replacing inherited values, so gitleaks,
Corepack, uv, npm and other tool caches write there. Read-only attempts also
append `-p no:cacheprovider` to `PYTEST_ADDOPTS` and set `PYTHONPYCACHEPREFIX`,
`RUFF_CACHE_DIR` and `MYPY_CACHE_DIR` under the cache, so a reviewer can run a
targeted test while the workspace stays unwritable. Shared temp and general
user caches are not writable. Where the `ps` shim is on PATH, Fabric stages it
in `<attempt>/tmp/provenant-shim`, which every lane can read. Codex writers
without capabilities use Codex's native sandbox through a permissions profile
named for that plan, `-c default_permissions="provenant-<random>"`, which
extends `:workspace`, sets `network.enabled` and passes a `filesystem` table.
The table grants `:tmpdir` (the attempt's `TMPDIR`), `add_dirs` and the Git
write boundary and leaves the common directory and the `.git` marker
read-only. Codex applies the nearest entry, and the same flags apply on resume.
Codex merges config tables, so the unique name keeps a system config's grants
under a known profile name out of the lane's policy; the read-only profile is
named the same way. A fresh run also passes `--add-dir` for each `add_dir` and
`--cd <worktree>`. Codex refuses to launch with a symlinked grant path, so a
link planted at one stops the next attempt rather than moving its grant. Codex
keeps a writable root's `.agents/` read-only even before it exists, so a
linked-worktree Codex writer also gets the worktree's `.agents/` as an
`add_dir` unless it is a file or link. Fabric creates an absent one for the
attempt and removes it afterwards if still empty. Git can then rebase or merge
the integration branch over tracked skills. For every writer, when an
`.agents/` path in HEAD, the index or on disk ends up matching neither the
attempt's starting state nor the primary checkout's branch or its upstream, or
drops that branch's version after taking the branch in (unless the lane's
branch had changed that path too before the attempt), Fabric by default
quarantines the change: it archives the lane's files to `<attempt>/protected.patch`
(and its index or HEAD to `protected.index.patch` or `protected.head.patch` when
they differ), commits the start or integration-branch version back to the lane,
returns the index and files to their starting content. A successful provider
then finishes `quarantined` only when the restore was verified: its result
stays visible, `landable: false` marks it, and status/digest/lanes name the
reason and patches for chair review. Writers can move only their own branch
ref, and `landing-push` refuses any commit since the leased remote SHA that
changes `.agents/` without an [audited chair release](../../docs/specs/fabric-v2.md)
of the reviewed tip.
`.agents/fabric-policy.json` `instruction_changes` may instead be `allow` (keep
an ordinary edit, with a warning) or `deny` (retain the edit for review without
restoration). Denied edits, failed restoration and unverifiable checks leave
edits on the lane, so the lane ends `failed` (fix: restore `.agents/` to the
start commit before landing) with its output and patches retained and listed;
signal-killed providers stay `failed` without a terminal result or `interrupted` after one; explicit cancellations stay `cancelled`.
`protected.patch` may be empty for an index-only edit or carry
an unavailable-diff comment for unsafe states, with `quarantine.patch_error`
explaining capture failure. Spared processes alone do not prove instruction
edits. Writers stage proposed instruction edits outside `.agents/` for separate
review. If OS confinement is unavailable, agy write
dispatch is refused; other wrapped writer receipts warn that writes are
unconfined. Setting `PROVENANT_NO_OS_CONFINEMENT=1` has the same effect on new
wrapped attempts.
Any lane may pass `capabilities: ["postgres", "browser"]`, using either or
both distinct values; empty lists mean none. Other adapters' profiles already
leave System V IPC and Mach services open, so they take the list without a
grant; their browser lanes only move `MAC_CHROMIUM_TMPDIR` to `<attempt>/tmp`.
A Codex `sandbox: "full"` lane is unsandboxed, so it applies no capabilities
and warns. Otherwise a Codex lane needs `worktree_write`, `sandbox:
"workspace-write"`, macOS, usable `sandbox-exec` outside another sandbox and
applied `network: true`. These lanes keep the writer's enforced guarantee
and workspace-write receipt value, while running Codex's own sandbox in
`danger-full-access` inside the OS profile. The profile allows Codex's native
Mach services plus FSEvents, denies other Mach lookups and registrations,
denies signals except to processes in the same sandbox, denies preference
writes through `cfprefsd`, and denies System V IPC by default. `postgres` adds shared-memory and semaphore IPC. `browser`
adds the macOS browser services and the Chrome/Chromium rendezvous Mach lookup
and registration prefixes; browser lanes set `MAC_CHROMIUM_TMPDIR` to
`<attempt>/tmp` for Chrome's process-singleton socket. These capabilities add no
network access beyond a Codex writer whose network is enabled; Unix-domain
socket connects are limited to `cwd`, declared `add_dirs`, the attempt
directory, task Codex home and mDNSResponder.
SBPL's `(local ip "localhost:*")` also matches every local address, so Fabric
does not claim that an inbound loopback rule limits connections.

Each task keeps one `CODEX_HOME` at `<task directory>/codex-home` across its
attempts. Existing `auth.json`, `AGENTS.md`, `HARNESS.md` and `skills` entries are symlinked
from `CODEX_HOME` supplied by the caller, or `~/.codex`; the profile grants
writes to the task home and the literal source `auth.json` only. It grants
each Git write boundary path by its own name, so a link planted at one never
moves a later grant. Fabric recreates the task home links
every attempt and fails a symlinked or non-directory task home, while allowing
the lane to overwrite the literal source `auth.json` for token refresh, a file
it could already read. That grant names the unresolved source path and covers
in-place rewrites only, not deleting, renaming or replacing the entry. A
symlinked or non-regular source `auth.json`, or a source Codex home inside a
lane-writable path or reached through a symlink, fails the attempt before launch.

Use Chrome with `--no-sandbox`, because macOS refuses Chrome's nested sandbox
inside `sandbox-exec`. PostgreSQL socket paths under the lane's `TMPDIR` exceed
macOS's 103-byte socket-path limit; use TCP or a shorter socket directory.
Protected-path dispatches to training routes still refuse without OS read
confinement.
The receipt records sorted `applied.capabilities`, the read-denied `applied.protected_paths` (empty on a non-training route) and `applied.confinement` as `sandbox-exec`, `provider-native` or `none`;
`applied.write_boundary` records the effective writable paths, including
declared `add_dirs` for confined writers, or the native sandbox and, for
Codex, its permissions `profile` name and a writer's `filesystem` table;
`workspace.cwd` is the provider cwd and `workspace.root` is the caller workspace.
Read-only macOS launches can read `~/.gitconfig` and
`$XDG_CONFIG_HOME/git/config` (default `~/.config/git/config`) so `git status`
works. Other home-directory reads remain denied apart from provider state and
sign-in files listed by the adapter profile: the login keychain for agy and for
Claude lanes on OAuth sign-in (bare and endpoint routes, which use a key, do not
get it); and for Kiro, `~/Library/Application Support/kiro-cli` and its
`~/.local/bin` launcher links, but not its cross-project `history`. Kiro can
write there only `data.sqlite3` and `.refresh.lock`, in place, and SQLite's
`-wal`, `-shm` and `-journal` sidecars, which it needs to read its sign-in. It
cannot create links, directories or FIFOs there, and the shell hooks and binaries beside them stay
read-only. An expired Kiro sign-in has not been seen to renew inside the
sandbox: the lane waits at a browser sign-in until it times out. Run
`kiro-cli whoami` outside the sandbox to renew it. A confined Kiro dispatch refuses until kiro-cli has unpacked its
engine outside the sandbox. Writable provider state is granted by its own
name, and a launch fails if any such path is a symbolic link, so a link a lane
planted cannot move the next attempt's grant to the link's target. The keychain file grant, together with the profile's unrestricted
`mach-lookup`, lets an OAuth Claude lane query the whole login keychain through
the security service; SBPL has no narrower filter, and read-only Claude lanes
have no shell. `~/.claude/projects` and `~/.codex/sessions` stay unreadable
even where provider state covers them, except a Claude lane's own session
transcript (`<session>.jsonl` and `<session>/`, for a UUID session id), which it needs to resume.
These read denies apply only to read-only lanes; a writer's profile denies reads of protected paths alone. Claude lanes get `CLAUDE_CODE_TMPDIR` in
the attempt's `tmp`, which is private to the user (mode 0700), so they never
touch `/tmp/claude-<uid>`.
Projects declare protected paths in `.agents/fabric-policy.json`, relative to
the directory holding `.agents/`. Fabric checks the workspace root and the Git
toplevels of the workspace, cwd and worktree; for a non-Git workspace, it also
checks immediate child repository toplevels and mirrors repository paths into
their registered worktrees. A route marked as training, or lacking a resolved
training flag, cannot receive a prompt file or additional directory that
overlaps one, or a cwd inside one. Its sandbox denies reads of protected paths
in every registered worktree; without usable `sandbox-exec`, dispatch is rejected. Non-training
routes keep their usual access.

On macOS, a Codex `read-only` or `workspace-write` provider gets a bundled `ps`
shim on PATH because seatbelt blocks the setuid `/bin/ps`. Its `lstart` uses
`strftime("%c", localtime(start))` in the caller's locale, matching `/bin/ps`;
when libproc cannot read a live PID, `sysctl(KERN_PROC_PID)` supplies its PID,
parent, group, user, start and elapsed times. A PID that no longer exists is
reported as gone. Fabric falls back to the PATH command when `/bin/ps` cannot
execute.

`tasks` contains 1–64 task objects with the same prompt and route fields plus
optional `id`; `concurrency` is 1–8. Top-level route controls and timeout apply
as defaults, with each task taking precedence. Prompt file paths resolve from
the caller workspace, including when `cwd` selects a subdirectory. Defaults are
55 seconds of waiting for a single dispatch and zero for a batch. Timeouts default to 3,600 seconds for
read-only work and 10,800 seconds for writers. `resume` retains the same run ID,
route and controls, including the exact prior capability list; only
`context_ceiling`, `timeout_seconds`, and `allow_secrets` may change. Use a new
dispatch to change the rest. With `resume`, `task_id` selects one task of a
batch; a task's own ID also works. `handoff: <run id>` starts a fresh run primed
with that task's route and result tail, the cheap alternative to resuming a large
session. `context_ceiling` (default 300,000 tokens, clamped to 100k–1M) lowers
auto-compaction where the provider supports it; it never raises it. Each attempt records `context`,
and the terminal Route line shows it as `ctx 212k/1M`. See
[`docs/specs/fabric-v2.md`](../../docs/specs/fabric-v2.md#session-context).

`session: "<name>"` names a provider conversation that outlives a run ID. The
first call starts an ordinary dispatch; later calls, from any agent in the same
project, resume its provider session through the `resume` path above. Names are
case-sensitive, one per project. The store keeps only the alias: the actual
adapter, provider-native session ID and the run, task, attempt and result path
of the last clean turn (`ok` or `input_required`). A failed or cancelled turn
leaves that pointer where it was, and the next turn resumes from the pointed-to
attempt and its provider session, not from a later failed attempt. A turn is
the whole owner invocation, fallback attempts included; the pointer moves to
the attempt that ended it cleanly. A name runs one turn at a time; another call
gets `session_busy` with the active run ID. The turn records its run before
the owner starts, so a caller that exits or dies while waiting leaves the name
busy until the run's owner finishes; an owner whose identity cannot be verified
counts as alive. Busy is read from the run itself: a launcher that dies before
starting the owner leaves an attempt that status reports `interrupted`, so the
name, resume and `fresh` all proceed. If that record cannot be written, nothing is launched and the
reply carries `session_unrecorded` and `session_error`. `fabric_session` with `action: inspect`, `list` or
`forget` reads or drops a name; `forget` is refused while a turn is active and
leaves run files and provider history in place. A provider without native
continuation (Copilot), a last clean turn with no provider session ID, or a
provider that no longer has the session returns `continuation_unsupported`; a
named session is never silently relaunched. `fresh: true` then starts a new
session primed with the last clean result, as `handoff` does.

Status accepts `ids`, `wait_seconds` (0–55), `until: any|all`, `detail`,
`tail_chars` and `fields`. A terminal brief row ends with the last 1,200
characters of its result the first time the session sees that lane, so a short
answer needs no `result.md` read and batch polls do not resend finished lanes.
Requesting one lane, or passing `tail_chars` (0–4,000; 0 disables; the total
across rows is bounded), shows the tail again. `detail: full` keeps a
one-line summary per attempt instead of a second copy of the row; `fields`
returns only the named keys (`fields:["attempts"]` gives the raw history).
While lanes run, the text ends with a hint to block with `provenant lanes --wait
--all --timeout 900` instead of polling.
When every requested lane is running and `wait_seconds` is omitted, the hint
also suggests `wait_seconds: 55`; the default wait remains unchanged.
New attempts wait when available host memory is below 10% of physical RAM for
`worktree_write` or 5% for `read_only`; set either percentage from 0 to 100 in
`<workspace_root>/.agents/fabric-policy.json` as `{"memory_floor_percent":{"worktree_write":10,"read_only":5}}` (either key may be omitted, and 0 disables that mode's floor).
Queued time does not use the execution timeout, and each attempt waits for memory admission up to its own dispatch `timeout_seconds`. Within a project, queued attempts are admitted in order of queue entry, but an attempt waits only for an earlier waiter that is live, has republished within three polls and whose floor is currently met; expiry fails the attempt as `memory_unavailable`.
Owners serialise admission through a per-user host lock in `$XDG_STATE_HOME/provenant/admission.lock`
(default `~/.local/state/provenant/admission.lock`) and hold it for up to 20 seconds after provider start; lock creation failure admits with a warning, while probe failure holds and retries until the wait expires. `fabric_status` and `fabric status` show the available percentage, floor and wait budget and allow cancellation.

### Live provider sandbox smoke

The live check is opt-in and makes real provider calls. On macOS with
`sandbox-exec`, run:

```sh
python3 runtime/fabric/live-sandbox-smoke.py --execute
```

It dispatches one short `read_only` and one `worktree_write` task for each
writer adapter among Codex, Claude, agy, Kiro and OpenCode, sequentially and
without fallback; `--adapter NAME` (repeatable) narrows the set and `--timeout`
sets the per-task limit (default 300 seconds). Each task tries to read a
declared protected file inside its cwd, write outside its boundary, and write
inside its cwd. Every lane must fail the outside write; a writer must succeed
inside its worktree and a read-only lane must fail there. The protected read
must fail only where the receipt's `applied.protected_paths` covers it, which
means a training or unresolved route; a non-training route reports
`protected_read_expected: true`. Kiro runs its own `auto` model, because the
catalogue has no Kiro alias, and OpenCode runs a free training route, so both
exercise the protected-read denial; agy runs `gemini-3.8-flash` by name. Each
row records the probe `method`: read-only Claude, OpenCode and Kiro lanes have
no permitted shell or write tool, so their writes fail at the tool layer rather
than by an OS denial. A missing executable, or a provider reporting a usage, rate, sign-in
or model limit, is reported as `skipped` with its reason, unless a forbidden
file already exists on disk, which fails the row; the command exits nonzero only
when a row fails. It prints JSON
containing each adapter, mode, resolved `Route:` line, confinement, warnings
and probe result, and creates its Git fixtures and run artifacts in a temporary
directory that is removed when the command exits. Do not add this command to
the default test run.
An invalid floor records a failed attempt with a fix. The parent keeps a
watchdog for child owners and excludes their published queued time.

Run and task IDs keep the `mcp-rand6` form.
Without IDs it returns active and last-24-hour runs, capped at 20 rows. Rows
include the latest attempt and count. Full detail adds history and the worktree
ledger: branch tip, dirty state and ahead count; unavailable Git facts are null.
Ledger reads are shared per worktree within a response and omitted for terminal
brief rows. New successful attempt, batch task and run statuses are `ok`.
Status and output readers accept `succeeded` in older retained files.
Unpublished batch children remain visible until an attempt or terminal batch
summary accounts for them.

`fabric_runs` also takes `state` (`running`, `terminal`, `active`, or an outcome
such as `failed`) and `limit` (default 20); by default it replies with one line
per lane and no structured rows, and `detail: full` adds them. `fabric_cancel`
takes a run, batch or task id: a batch task id stops that task alone (through
`run_controls.py cancel --task-id`) and leaves its siblings running. Its reply is
a one-line outcome; `detail: full` adds the structured rows.

`provenant lanes --json` and `fabric_runs` expose `fabric.runs.v1`. The response
has `status: ok|unknown` and `runs`; read failure is `unknown` with an `error`,
never an apparently empty successful list. The default list covers active and
recent runs (at most 20 task rows); an ID reads that run or task. Each row has
`id`, `run_id`, `task_id`, `run_path`, `state`, `status`, `route`, `model`,
`started_at`, `ended_at`, `last_progress_at`, `pgid`, `pgid_alive`,
`result_path`, `receipt_path`, `writer`, `worktree` and `attempt`. Paths are
relative to the project's `.agent-run` root. A task imported from another
project has `source_project` naming the launching project, an empty `run_path`
and null artifact paths. Read its artifacts, status or output from that source
project; `lanes --project <source_project>` also returns its local paths. Older
runs without a recorded cwd project remain readable from their original root.
`source_project` is display-only. A corrupt or unreadable pointed run leaves
local lanes readable and reports `discovery_error`. Lane discovery does not
consult the project registry.
`ended_at` is `null` until the
attempt is terminal; a closure the owner never wrote takes the run's recorded
finish time. Missing facts are `null`; `pgid_alive: null` means
the process identity cannot be verified. Both `tasks/<id>/attempt-NNN/` and
`dispatch/tasks/<id>/attempt-NNN/` receipts are read internally. Fabric's status
and output readers use the same underlying receipt scanner; consumers should
use the versioned response instead of opening receipt files themselves. The
response reserves an optional `claims` field for the work-claims reader.

`provenant events --follow` prints one JSON line per new terminal or
`input_required` attempt and per unread inbox message, and stays open until
stopped. Add `--until-idle` to exit once no run is active and a poll finds no
new events; this lets a background monitor wake a chair and finish. `fabric_events` returns
`fabric.events.v1` with `events` and an opaque `cursor`; pass that cursor back
with `wait_seconds` (0–55) to wait for changes. Run one follower per session;
restart it only after exit. The stream is a bounded recent view of retained
attempts and the active inbox, so a chair should reconcile with `fabric_runs`
after a long disconnect. Arm a fallback wake of at least 20 minutes.

Output defaults to 4,000 bytes and caps each request at 20,000. Continue at
`next_offset`; pages preserve UTF-8 boundaries and `eof` reflects the current
file size. Use `tail: true` for a bounded tail, including while a run is active.
Paths must resolve to regular files inside the retained run directory. For a batch, select a task ID.
`detail: full` adds adapter profiles or the agent list to discovery responses.
CLI presence does not prove authentication; `auth?` makes that uncertainty explicit.
`fabric_adapters` with `models: <adapter>` and optional `match` (CLI
`fabric adapters --models <adapter> [--match S]`) lists that adapter's live model
ids from a listing-only `model-route probe` (30-second deadline, cached for a
day). A large family shows only its count, and a reply that would pass 4 KB drops
whole groups and says how many; `match` narrows either. Dispatch any id as `model: "<adapter>/<id>"`; an id
outside the catalogue runs with a note. A Claude or Codex seat keeps its own
models on native subagents: pool picks and the default route skip them, a pool
holding only them is refused (`route_native_only`), a council hands them back as
a spawn line, and an explicit request for them is rejected as
`native_seat_model` on a Claude seat and runs with a leading `NATIVE:` warning on a Codex seat (`skills/orchestrate/references/routing-and-tiers.md#native-first`).

For a Claude seat, the own-seat check applies after batch defaults and selector
precedence settle, before local or peer placement (including qualified sessions).
An explicit own-seat Claude route in any task refuses the entire batch:
`native_seat_model` names the offending task id, every task returns as terminal
and rejected, and no task is placed. Other task validation failures reject only
those tasks; valid siblings still run. Selector-free continuations and handoffs
that inherit their route remain exempt.

## Hosts

`provenant hosts list` lists instance-owned host configuration;
`provenant hosts doctor --json` reports local and peer diagnostics with a
bounded SSH call per peer. `fabric_hosts` delegates to the same Python owner.
Existing local commands remain unchanged. See the [host setup
runbook](../../docs/runbooks/fabric-hosts.md) for configuration, restricted keys,
protocol framing and diagnostic limits.

## Mailbox and identity

`fabric_inbox` defaults to a non-claiming peek of ten headers: ID, sender, kind
and an 80-character preview. `ids:[...]` claims up to 100 selected messages;
`claim:true` claims available messages up to `limit`. Bodies are capped at
4 KiB each, with `body_path` for the full text. Deliveries older than fourteen
days stay in storage but are excluded from the active inbox.

`fabric_inbox{digest:true}` and `fabric inbox --digest` return
`fabric.inbox_digest.v1`: unread `total`, up to 20 `groups` by sender and task,
and `truncated`. Each group has a count, an 80-character one-line summary and a
sample message ID. Fetch a full body by calling `fabric_inbox` with `ids:[id]`;
the digest neither claims nor acknowledges deliveries.

A claim lasts five minutes by default. Acknowledge only after processing the
message; expired claims redeliver. Claim tokens prevent another reader from
acknowledging the delivery. `wait_seconds`, `task_id`, `peek` and
`claim_seconds` remain available for existing integrations.

`chair` resolves through `PROVENANT_CHAIR`; `/root`, `root` and `parent` use
`PROVENANT_PARENT`, then the chair. Only known project seats are selected. An
unbound caller can use a registered seat named `chair`; otherwise the request
is rejected with `fix: pass to:<seat>` and the currently valid labels, seats
and teams. Fabric never guesses the first seat.
Owner completion posts `run_terminal` to the dispatching seat. Observing a
terminal row through dispatch, cancel or status acknowledges its notices,
including a notice that arrives after the response.

Registered Git worktrees share one project while retaining their own cwd.
`AGENT_FABRIC_LABEL` separates seats of one provider. A label remains bound to its
first announced provider. Task ownership is cooperative routing metadata, not
an access-control boundary. Reply parents and linked task IDs must exist in the
same project.

The shell mailbox retains its explicit claim workflow:

```sh
fabric inbox --peek
fabric inbox --limit 5
fabric ack <message-id> <claim-id>
fabric watch mcp-a81f3c mcp-b22c       # state changes; exits when all terminal
fabric watch --activity --interval 2 # activity stream
fabric status <run-id> --wait-seconds 55
fabric adapters --json
```

### Work claims and landing

`fabric_work_claim` records an advisory issue or repository-relative path claim
for one chair session. Conflicting live issue claims and overlapping path
prefixes are refused on acquisition; dispatch does not enforce claims. Acquire
and verify a claim before dispatch or landing, and do not dispatch against a
conflicting live claim. Pass a distinct, stable `session_id` without `/` for
each chair, even when two sessions use the same Fabric seat. The returned `id`
and `generation` identify renew, verify and release operations; expired claims
lapse. `#869` and `869` share an issue key, and path claims compare without case.
Active claims appear in `fabric work-claims` and `fabric_status` when
full detail is requested or ownership is present.
`ttl_seconds` is accepted as an alias of `seconds`; values above 3,600 seconds
are clamped to 3,600 with a warning.

`fabric_landing_lease` separately acquires one repository-wide landing lease
with the expected remote integration SHA. It has a holder, expiry, generation,
verify and release operations. To wait for another chair to land, use
`fabric_landing_lease` with `action: "wait"` and `wait_seconds` (clamped to 55) or the
shell `fabric landing-wait --timeout 900` (exit 124 on timeout) instead of
sleep loops; both return when the holder, generation or release state changes.
`expected_sha` may be a unique prefix of at least seven hex digits or uppercase
and is resolved with Git in the project. Invalid values return
`expected_sha_invalid`; unknown or ambiguous prefixes return
`expected_sha_unresolved` with a command to obtain the full SHA.
An expired lease can be taken over; the prior
holder and new generation are recorded in activity. Before pushing, run
`fabric landing-push <session-id> <generation> <branch> --label <seat>` from the
landing checkout, using the same label as the MCP lease holder. Alternatively,
export that label as `AGENT_FABRIC_LABEL` in the shell. The command checks the
remote SHA and live lease, confirms the remote commit is an ancestor of HEAD,
then pushes with `--force-with-lease` and releases the lease on success. The
command waits at most two minutes for Git; its persisted hold lasts another
thirty seconds. A push orphaned by a crash is fenced by the remote SHA, not by
the hold. If the push succeeds but release fails, it reports `pushed` with a
`release_warning`. Renew or release an aborted lease as appropriate.
`landing-push --remote R` pushes to a remote other than `origin`. With
`--json`, Git and hook output go to stderr, so stdout holds only the result.

Scripts use the same lease from a shell, without an MCP connection:

```sh
fabric landing-acquire <session> (--expected-sha SHA | --branch B [--remote R]) [--seconds N]
fabric landing-verify <session> <generation> [--expected-sha SHA] [--branch B [--remote R]]
fabric landing-release <session> <generation>
fabric landing-status
```

Each takes `--label L` and `--json`. Acquire returns `holder`, `label`,
`session`, `generation`, `expected_sha`, `expires_at` (ISO 8601 UTC) and
`taken_over_from` after a takeover. Pass its `label` to later calls. Verify
returns `valid` and, when false, `reason`: `stale`, `sha_mismatch` (the given
SHA) or `sha_moved` (the remote tip from `git ls-remote`). Release never
touches another holder's lease. It returns `released: true`, or
`released: false` with `reason` `expired`, `not_held` or `pushing`. Status
returns `lease: null` or the live lease. Exit codes are 0 for ok (including an
unreleased `expired` or `not_held`), 1 for refused or stale, 2 for usage and 3
for held by another holder or a push in progress. A store that cannot be
opened exits non-zero with nothing on stdout. `provenant lease <verb>` maps
onto these commands; the [landing lease
runbook](../../docs/runbooks/landing-lease.md) shows a repository script's
calls.

## Run storage and boundaries

`identity.runRoot(cwd)` uses the primary checkout's `.agent-run` for Git and the
workspace directory's `.agent-run` otherwise. New runs are stored under
`runs/YYYYMMDD-HHMM-dispatch|batch-slug-rand6/`; IDs stay `mcp-rand6`.
Owner stdout, stderr and staging inputs live inside `_owner/`. New run creation
adds `/.agent-run/`, `/.worktrees/` and `/.work/` to Git's local exclude file when
writable. Readers discover only current timestamped runs in the shared
`.agent-run/runs` directory.

Dispatch-time maintenance scans the shared run directory. It closes active receipts
older than 48 hours with no observed live owner as `interrupted`.
Fabric startup and status reads also finalise running attempts as `lost` when
their process group is gone or its recorded OS start time (`pgid_started_at`)
differs. Legacy attempts without that identity need both a dead group and
`last_progress_at` older than 48 hours. The finaliser takes the writer's custody
lock and leaves live owners alone; status checks only the runs selected for
the response. Lost attempts retain their end time and liveness evidence and
count as provider errors in telemetry. Retention is
seven days for successful/cancelled runs and fourteen days for failures
(`AGENT_FABRIC_RUN_RETENTION_HOURS` overrides). Live runs,
`KEEP`, active receipts and delivery `RUN.json` files are preserved; unknown new
run directories are left for the cleanup owner. PID start identity is required
before signalling a recorded process group. A missing `ps` capability cannot
certify process death or safe cancellation.

`AGENT_FABRIC_STATE_DIRECTORY` defaults to
`~/.local/state/agent-harness/fabric`. New directories use mode 0700; existing
permissions are preserved. SQLite WAL, immediate claims and durable terminal
observation records handle concurrent readers. The boundary is one local OS
user; the mailbox is not a security boundary between processes of that user.

## Verification and integration

```sh
npm --prefix runtime/fabric run typecheck
npm --prefix runtime/fabric test
node runtime/fabric/mcp-smoke.mjs
node runtime/fabric/tests/tool-budget.mjs
```

The MCP smoke uses fixture owners through the registered launcher. It covers
a real linked worktree, writer arguments, inherited Git redirects, question/resume,
failed-resume recovery, status, cancel, bounded output, two-seat reply correlation,
owner-bound tasks, mailbox claims and terminal-notice acknowledgement.
The layout test reads the shared `tests/fixtures/fabric-v1/layout-cases.json`,
including its symlinked-cwd case.

Before: 15 tools, 9,077 characters, approximately 2,269 tokens (`chars / 4`).
The default v2 tool-list test enforces at least a 35% reduction.

Claude dispatch accepts `account: "label"` (CLI `--account label`) from
`accounts.claude` in telemetry configuration. Without a label it chooses from
cached quota, preferring five-hour room and projected unused weekly allowance
after excluding weekly deficits when a healthy account exists. No usable cache
keeps the default identity. Receipts and telemetry record the selected label.
