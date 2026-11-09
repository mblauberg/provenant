# Lane lifecycle

Owner: run and Fabric maintainers
Anchor: docs/specs/run.md, docs/specs/fabric.md

For host-owned project paths, use `PROVENANT_PRIMARY_CHECKOUT`. A linked
worktree's read-only admin directory is `PROVENANT_WORKTREE_GITDIR`; the writer's
Git commands continue to use its private view. Project `shared_paths` can expose
an existing host lock directory read-write to writers, and `exec_roots` can
expose a repo-managed toolchain read-only. Both are empty by default. See
[project paths](providers.md#project-paths-and-host-checkout-identity).

Start an approved writer with the `implement` template, or use `deliver` for
the composite through CI, review and landing:

```sh
provenant run --use implement --file brief.md --explain
```

Check the work binding, literal write scope, acceptance and authority in the
explanation, then submit without `--explain`. One writer owns each worktree;
parallel slices integrate into one branch before full review. See
[templates](presets.md) for composition and the default choices.

Run confinement is enforced by Seatbelt on macOS. Linux refuses launch because
it has no enforcing backend; it does not run a lane unconfined. Optional host
restrictions in `[domains.run.host_policy]` apply across providers, direct and
child launches. Open-network lanes can read immutable GitHub CLI configuration,
including read-only lanes; explicit credential denials and narrower network
settings remove that exception. See [run host policy](configuration.md#run-host-policy)
and [provider boundaries](providers.md#lane-filesystem-and-network-policy).

Use `provenant status` for graph, node and attempt rows, `output TARGET` for
results and `output TARGET --trace` for the timeline. Use `steer` for a message,
`stop` for a subtree, `retry` to continue or redo work and `edit` for patches to
unstarted nodes. Inside a worker, `run --use TEMPLATE` adds a child sharing the
graph's budget and trace. A wait expiry leaves work running.

New stdout/stderr captures and settled `result.md` files mask known credential
shapes before storage. Settlement also masks them in the attempt's Codex and
Claude session transcripts, including Claude's `.txt` and `.json` tool-result
files, before provider state is retained or archived. Result digests identify
the stored bytes, and `redacted: true` means the stored result is a filtered
rendering or the fixed withheld placeholder. A result redaction failure withholds
the result; a transcript failure keeps the filtered result. Either one settles
a lane that would otherwise succeed as failed, instead of retrying forever. A
cancelled, timed-out or lost lane keeps its outcome and names the failure in its
reason.

Some provider stores are not scrubbed and keep whatever a lane printed. These
are Codex's SQLite databases with their WAL, SHM and journal companions, agy's
`agy-home` copy of its history, and Claude state outside `projects/` other than
`history.jsonl` and `session_index.jsonl`. Unknown secret formats, earlier run
files and provider history archived before this rule still need care when
sharing. See
[output redaction and identity](../specs/run.md#output-redaction-and-identity)
for the full list.

A valid final handoff declaring `stopped_early` or `blocked` makes an otherwise
successful lane settle as `review_required`. `run --wait` and `status` show the
state, so the unfinished work is visible without inspecting partial signals.

On macOS, the host selects a nice value for the lane, and the guardian applies
it to the provider process before confinement. Its descendants inherit that
value.
Configure the lane classes in the selected state directory's `config.toml`:

```toml
[domains.fabric.lanes.priority]
writer = 10
read-only = 5
build-heavy = 15
```

Values range from 0 to 19, with higher values yielding more CPU time to other
work. These are the defaults. A worktree-write lane uses `writer`; other lanes
use `read-only`. Set `--build-heavy` to use `build-heavy` for either mode.
Fabric tasks and manifests accept `"build_heavy": true`. `status --json` reports
the applied value as `priority`, and `requested_priority` when it differs. The
value is null before it is known and for older records without this evidence.
If the guardian already has a higher nice value, the provider inherits that
lower scheduling priority. If changing priority fails, the provider still
launches at its inherited priority and the reason is recorded as a warning.

When run from a linked Git worktree with no `--mode` and no target
(`--worktree`, `--branch` or `--worktree-branch`), `run` refuses with `REFUSED`
and names both repairs: `--worktree <path>` for a writer, or `--mode read-only`
for a reader. To read another registered checkout, pass both `--mode read-only`
and `--worktree PATH`. Readers use that checkout without acquiring its writer
lease, including while a writer holds it. The checkout and its Git metadata
remain read-only even when the project configures ordinary read-only write
exceptions. Explicit write roots must not overlap the checkout.

When a writer launch creates a new branch without `--base`, it fetches the
default branch advertised by `origin` and starts from that commit. If the
remote cannot be fetched or has no default branch, it uses local `HEAD` and
warns with the commit ID. An explicit base and an existing branch keep their
usual behaviour. `provenant worktree create` uses the same default.

Read-only lanes can run offline Cargo tests, including doc-tests, using their
private `CARGO_TARGET_DIR` under lane scratch; they may execute files they
write to scratch but nowhere else. When a Swift toolchain is selected, the `swift` shim sends
SwiftPM build output to a scratch-only path and keeps the Clang module cache and `TMPDIR`
(where SwiftPM runs the compiled package manifest) in the lane scratch as well. Both toolchains stay read-only, and the worktree,
Git metadata and shared caches retain the read-only profile.

On macOS the state-root path of a lane's `TMPDIR` can exceed Darwin's 104-byte
`sun_path` limit, so tools that bind Unix sockets fail. At launch preparation
the attempt gets a symlink `/private/tmp/pv-<32 hex>` to its `tmp` directory
(the hex is the first 16 bytes of SHA-256 over a fixed domain string and the
canonical attempt path), and `TMPDIR`, `TMPPREFIX` and `CLAUDE_CODE_TMPDIR`
name `<alias>/candidate-N`. Only that string changes: grants, staging,
settlement and retention use the real `tmp`, and the sandbox resolves the
link to it, so no grant names the alias. A writer's profile also lets it bind
and connect to Unix-domain sockets under its own scratch. A name already taken
by anything other than our own link owned by the same user is left alone and
the lane uses the real path. Terminal commit, unreserved reconciliation and
retention unlink the alias (never recursively) when they remove `tmp`.

The receipt names the worktree. For writers, `--worktree PATH` uses an existing checkout
without taking over its lifecycle. A missing branch name creates an owned
checkout; for an absolute path or a path below `.worktrees/`, use
`--branch BRANCH` to create it. Git's worktree inventory establishes membership,
so native linked checkouts and checkouts recreated at a previously used path
need no registration or adoption. Another project's checkout is refused.
A held writer lock reports busy and admission names the holder when its attempt
can be identified. Lifecycle removal refuses an observed live process; admission
treats census observations as advisory when claiming an unowned checkout,
replaying its operation or replacing a confirmed terminal lane owner.

Live-writer detection at run admission excludes the invoking process and, by default,
its verified ancestor chain (such as the invoking shell). A run launched from inside
the target worktree is admitted when writer custody is free and the Git boundary
is safe. Bystander observations warn with the observed process (PID, command
and working directory) when available. Hosts can tighten the census policy by setting
`confinement.census_exclude_ancestors = false` in `[knobs]` in host configuration
(default `true`); when disabled, ancestor shells inside the worktree contribute
to census observations too.

Landing cleanup checks cwd, executable images and open files as well as writer
custody. A positive occupant always keeps the worktree. Unreadable processes
with stable real, effective and saved system uids (0–499), all foreign to the
candidate worktree owners, do not block cleanup solely on missing cwd or
descriptor evidence. This trusts foreign system processes; root privileges do
not prove that a process cannot retain a worktree path. Same-user processes,
mixed setuid credentials, missing or changing credentials, and incomplete
process listings remain uncertain and keep the tree when a complete census is
required. Provenant services have no executable-name exception. Transient
descriptor churn or unavailability retries within the observation budget;
persistent gaps still keep a possible writer's worktree. See the
[census policy](../specs/supervisor.md#unreadable-processes-need-positive-non-occupancy-evidence)
for the observation limits and system-process trust assumption.

A new writer may reuse a registered worktree from a terminal lane when the tree is clean and
writer custody is free. Dirty trees, held locks and retained import fences still refuse. When
`--base` is supplied for a branch that already exists, Provenant accepts the branch only when
it already contains that base; otherwise the refusal names the branch head and
the base. A branch checked out elsewhere must be selected with `--worktree`.
If a linked checkout has no Provenant lane-owner record, the worktree list
shows that fact separately from its observed writer state. It may have been
created outside Provenant. Inspect custody and use `--worktree <path>` to select
the checkout explicitly; unknown custody remains refused until checked. When a
local `--branch` launch finds that branch in a clean unowned linked checkout,
the refusal and local graph status and output show a shell-quoted absolute path
for `--worktree`. The checkout is rechecked under the ordinary custody rules.

Use `provenant worktree doctor PATH` after a worktree refusal. It reports the
checkout's state, the reason and one suggested command for dirty, stale
registration, live writer, unregistered and detached checkouts. Replace `BRANCH`
or `PATH` placeholders before running a suggestion. Unknown occupancy includes
its scope, evidence and source; inspect those before retrying. Doctor only
observes the checkout: it does not run its suggestion, acquire writer custody
for a lane or clear retained fences. Admission still rechecks custody and the
Git boundary after a clean diagnosis.

A refusal that says repository authority could not be revalidated includes the
underlying repository error and its reason. Inspect that reason and the
worktree's Git registration and configuration before submitting again.
Git tracking entries for other branches do not invalidate an unchanged
worktree. Other configuration changes still require a fresh probe.
After an unlaunched writer is rejected, reconciliation removes its retained
Git pin. Status and wait queue recovery. To retry directly, run
`provenant reconcile --id <run>` after resolving any reported cause.

Overlapping origin refresh requests share one fetch. The repository setup lock
waits up to 60 seconds and serialises only upstream configuration writes; origin refresh,
repository verification, branch creation and provider launch run outside it.
The lock is released before provider launch and custody confirmation.

Use `provenant lane deliver ATTEMPT --deliver` with `none`, `push`,
`integrate:integration` or `pr`. Push and PR
use origin and the lane branch. Integration targets must already exist;
`integrate:origin/integration` selects a remote branch. Local targets must be
checked out nowhere, including the delivering lane's own worktree. Integration
conflicts leave checkouts and destination refs intact. Inspect the failure and resolve it through the owning lane.

Writer Git inherits host global and system Git configuration. Launch places the
resolved global name and email in a private global config, with
`user.useConfigOnly=true`; repository identity still takes precedence. A missing
identity makes Git refuse a commit instead of generating a hostname identity,
and identity values containing control characters refuse launch. The host global
config files included by the lane are readable inside the writer sandbox. The
lane's GitHub token helper resets earlier credential helpers for Git processes
that retain the lane environment; the wrapper also restores this precedence when
`GH_TOKEN` survives an otherwise cleared environment. Explicit Git command-line
configuration can override it.

Publishing from a writer lane is on by default: it gives a networked writer
lane your GitHub credential, so a malicious lane can extract the token and act
outside the wrapper. Set `lanes.publish=false` to withhold it, after which the
chair publishes through host-side delivery. Writer lanes with open network can push and open pull requests using the host's
read-only gh configuration and the pinned `gh auth git-credential` helper for
HTTPS remotes; SSH remotes keep their own authentication. Custom gh configuration
paths and keychain-only logins are not mirrored. Hosts without gh or
`~/.config/gh` continue local work without publishing setup. Merge through `provenant land N --wait`, or hand the merge to the chair. Landing
verifies green CI at the PR's current head; add `--head SHA` to pin a reviewed
head. Disable publishing with
Fabric `knobs: ["lanes.publish=false"]` or host configuration `[knobs] "lanes.publish" = false`. Read-only lanes retain their
existing open-network gh read access and never receive publishing setup.

For a project, the host can configure an advisory GitHub CLI deny list in
`projects/<ProjectId>/github.json` under its state root. The project ID is
host-local. Find it in the `projects` array of `provenant quota usage --json`,
matched to the repository's Git common directory. For example:

```json
{
  "deny_verbs": ["release create", "workflow run"],
  "deny_labels": ["ci"]
}
```

Both lists default to empty. A denied verb matches a command token prefix. For
pull request and issue creation and editing, label checks recognise long label
flags with separate or `=` values, including comma-separated lists. For
creation, they also recognise `-l` with separate, `=` or attached values. A
policy label and each value in a comma-separated list are trimmed; empty policy
labels are ignored. A refusal exits 1 and prints `github.deny_verbs` or
`github.deny_labels` on standard error; other commands continue to the host's
GitHub CLI. The shim and policy are active only when publishing is available,
so disabling publishing or running without the host's gh configuration leaves
the policy inactive. This is an advisory guard against accidental use, not a
security boundary: a lane can invoke the real `gh` directly or use another
GitHub API client. Use GitHub permissions and repository rules to enforce
restrictions.

After a settled writer without delivery, run:

```sh
provenant lane deliver ATTEMPT --deliver pr
```

This refuses a dirty checkout, a changed head or live custody. Repeating a
successful delivery returns its receipt, including after checkout cleanup.
PR bodies contain bounded commit messages and run and attempt IDs. Default
and primary checkout branches require PR delivery. Owned checkout cleanup
requires a succeeded lane and successful requested delivery, with the recorded
head retained by the delivered ref or creation base. Local branches remain
until integration is proved. Cleanup uses the project's declared regenerable
and keep paths. `worktree-settlement.json` beside `result.md` records a pruning
intent before removal, then pruning or the retention reason. Failed and
cancelled lanes retain their worktrees. Explicitly supplied worktrees retain
their existing GC policy.

## Recovery

Inspect `status TARGET`, `output TARGET` and `output TARGET --trace` before
replacing work. `retry TARGET` continues or redoes the target, optionally with
a new brief or an authorised budget increase. Address the recorded cause
first. Recovery reconciles uncertain launches, reuses finished outputs and
continues only unfinished work; never launch a second writer while custody
is uncertain. Do not reset a worktree containing edits.

Choose recovery by the saved cause and the work you intend to retain:

| Intent | Existing command and effect |
| --- | --- |
| Resume unfinished work | `provenant retry GRAPH` advances the retained checkpoint, preserving completed outputs and reconciling any uncertain launch under its recorded attempt identity. |
| Retry a node | `provenant retry GRAPH/NODE` retains prior attempts and reopens dependent work through the graph patch path. |
| Repair with a changed brief | `provenant retry GRAPH/NODE --brief TEXT` records a replacement brief for the retried work; review and land gates still apply. |
| Fall back to another route | Let the node's configured routes handle a typed unavailability result after custody release is confirmed. An uncertain handoff or a failed release blocks a route switch; inspect `status` and retained evidence first. |
| Start fresh work | Submit a template with `identity = "fresh"`, or use a new operation ID in the run document. An unchanged `identity = "content"` submission reattaches even after cancellation; use `retry GRAPH` to resume that graph. |

Template identity replaces the removed preset `--fresh` flag. See
[composition rules](presets.md#composition-rules) for what participates in replay
identity. A fresh run never clears an earlier writer's custody fence.

Use `edit` for a patch to unstarted nodes, with the expected graph revision
and an idempotency key. Started attempts stay immutable. `stop TARGET` stops
its subtree unless a child is explicitly detached. Put repository templates
in `.provenant/templates/` and temporary briefs in `.provenant/scratch/`.
Delivery failure blocks dependent work; retry only after checking the
integration head and retained evidence. Review remains bound to its subject.

When a release breaks state readability, finish or stop pending graphs and drain
writers on every host before [upgrading](install.md#bootstrap-and-upgrade).
Routine releases preserve state readability and allow running graphs and lanes
to continue across upgrades. Install on all hosts before new launches. Preserve
unreadable records and report them.

A retained slot-root reservation is carried to its named attempt during locked
recovery. The attempt evidence is synced before the slot-root record is removed;
a storage failure preserves a custody fence. If the source remains, a later
recovery checks the same bytes before completing the cutover.
`provenant reconcile --id RUN` performs this
cutover before discovering attempts. Unreadable or conflicting evidence remains
visible as a reservation issue and continues to fence writers; retain it for host
investigation. Reconcile reports cutover storage errors in
`writer_reservation_errors`, returns nonzero and continues judging independent
known runs. The report includes reservations from every project selected by
`--id`, even when the command starts in another project. Reading status does not
perform the cutover.

Cancelling a lane that has not launched a provider settles it as cancelled
with nothing to import, and frees its queue place and its worktree's writer
before `stop` returns, so a new `provenant run` there succeeds straight away.
A launcher still preparing the writer (for example waiting on
`provenant-repository.lock`) sees the request within a fraction of a second,
stops waiting and never advances towards launch. If it has not let go within
about four seconds, `stop` reports the request as pending and the launcher
settles the attempt as it unwinds. A launcher that dies before the provider
launches leaves an attempt that status observations settle by
themselves once its writer lock is free; it no longer shows as `starting`.

`provenant reconcile --id RUN` queues settlement in the background and returns
without waiting for Git import. The supervisor resumes its durable phase after
a crash, with at most three attempts that reach import and backoff between
claims. Concurrent reconcile requests share one settlement worker;
queued or terminal-only runs do not spawn one. `provenant stop ATTEMPT` records
a durable request that can stop an active import. Provider exit releases host
build capacity and live custody before settlement completes; a settling
reservation still prevents another writer from editing that worktree. A terminal
import refusal releases the reservation and retains private Git for host
recovery. A lane wait returns a terminal outcome only after custody is released,
so host pickup does not need a second reconcile after a successful wait.
For a stopped merge, settlement imports the merge parents and staged objects
through the verified Git import, then restores the unmerged index and merge
metadata in the worktree. A continuation writer receives that merge state and
can resolve and commit it. If import remains fenced, the refusal identifies
the retained attempt and provides the explicit abandonment command. Abandonment
keeps the private Git evidence but stops automatic import; recover that work
manually before relying on a later writer.
See [publication order](../specs/run.md#publication-order) and
[lane Git and import](../specs/run.md#lane-git-and-import).

Writer rebases use the lane's private Git directory and pass through host object
import and ancestry checks at settlement. If Git cannot
write rebase state, stop with `needs_host_rebase`; do not create a clone or
second checkout to bypass the permission failure. A host-authorised merge of
`origin/main` is an alternative when rebasing is blocked.

On macOS, lane profiles still deny creating hard links and pathname Unix socket
binds outside granted private paths. The lane nextest profile excludes native
fixtures that need hard links or short socket paths; the `ci` profile retains
them. Do not treat these exclusions as resolved by ambient access. See the
[writer limits](providers.md#writer-lanes).

## Tests and Git

Tests run Git only through `provenant_os::fixture`: `git_command()` for a
`std::process::Command`, or `run_git(cwd, args)` through the executor. Both
drop the lane's `GIT_*` binding, so a fixture never commits into the lane's own
repository. An `ExecOptions` that runs Git sets `env` explicitly; `tests/workspace_policy.rs`
fails on one that inherits the environment.

A root-package `cargo` command run inside a lane has failed in `build.rs` with
`cannot resolve source inventory git index`. The lane that hit it reported
`GIT_CONFIG_GLOBAL=/dev/null` as a workaround. The cause is unconfirmed: it was not
reproduced outside a lane sandbox. `build.rs` removes `GIT_*` before it runs Git, so the likely
reason is that it then loses the lane's binding. Track it in a separate issue before
relying on this workaround.

## Wait on mixed work

Use one blocking status call when a chair has several kinds of work outstanding:

```sh
provenant status --wait --target lane:ATTEMPT_ID --target graph:GRAPH_ID --target pr:2721
provenant status --wait=30m --until any --target lane:RUN_ID --target land:2721
provenant status --wait=30m --until all --target lane:RUN_ID --target land:2721
provenant status --wait=1h --follow --target graph:GRAPH_ID --target pr:2721 --json
```

`status ID --wait` also keeps the usual lane/graph ID resolution. `--target`
is repeatable; explicit prefixes disambiguate lanes, graphs and PR numbers.
`land:N` observes PR N only: it grants no merge authority. Use `land N --wait`
when a merge is authorised.

For a stored refused operation, use `provenant status operation:OPERATION_ID`
to inspect its diagnostic. `provenant status operation:OPERATION_ID --wait`
returns immediately with exit 1. Use `--target operation:OPERATION_ID` to
include it in a mixed wait; a completed wait that observes a refusal exits 1.
Unsupported adapter capabilities and secret input refused by the run service
have such a record, including nested requests whose canonical child identity can
be constructed. Other pre-admission refusals may not yet have one.

The default `--until all` waits for every target to become terminal. Use
`--until any` to return on the first observed semantic transition, or
immediately for a terminal target.
An initially running lane selection does not count its older settled attempts.
A lane selection is terminal when all its attempts have settled;
a graph when complete or cancelled; a PR when merged or closed. A ready or
failing open PR is notable but remains nonterminal. `--follow` streams initial
snapshots and each observed transition, and always waits for all targets to
become terminal, regardless of `--until`.

Each event occupies one line. With `--json`, stdout is newline-delimited JSON:
`snapshot`/`transition` records carry `target`, `terminal` and `data`; the final
`end` record carries `condition_met` and `timed_out`. An expired wait exits 124
and leaves work running. Identical observations and elapsed-time changes emit
no event. Initial transient reads retry within the budget; other unreadable
sources fail the call initially. Later read failures retain the last good
observation until recovery or timeout.

The coordinator receives source events through one channel. Sources reuse the
existing store and GitHub observations at bounded intervals; this is not a
GitHub webhook subscription and cannot report intermediate states between
samples. Configure the defaults in the selected state directory's `config.toml`:

```toml
[domains.wait]
timeout_seconds = 300
local_interval_ms = 500
pr_interval_ms = 5000
max_targets = 64
pr_read_timeout_seconds = 60
```

`--wait=DURATION` overrides the timeout for one call (1 second to 4 hours).
Source intervals accept 10 to 60000 milliseconds. `max_targets` caps the target
count (default 64, 1 to 1024). `pr_read_timeout_seconds` bounds individual
GitHub PR read operations (default 60 seconds, 1 to 600). Waiting observes graph
checkpoints; use `retry` on the graph when continuation needs
an explicit decision. Repeated status calls or shell sleep loops are unnecessary.
