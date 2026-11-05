# Lane lifecycle

Owner: run and Fabric maintainers
Anchor: docs/specs/run.md, docs/specs/fabric.md

For host-owned project paths, use `PROVENANT_PRIMARY_CHECKOUT`. A linked
worktree's read-only admin directory is `PROVENANT_WORKTREE_GITDIR`; the writer's
Git commands continue to use its private view. Project `shared_paths` can expose
an existing host lock directory read-write to writers, and `exec_roots` can
expose a repo-managed toolchain read-only. Both are empty by default. See
[project paths](providers.md#project-paths-and-host-checkout-identity).

Create a writer checkout, submit and wait with one command:

```sh
provenant run --prompt-file brief.md --route senior \
  --branch fix/issue --base main --deliver push
```

`run` waits up to 240 seconds, then prints the state and a result tail. Use
`--wait 30m` or `--wait 3600s` for a longer background wait. A wait that ends
before the run settles returns status `waiting` with exit 0 and prints the
exact command to keep waiting; follow it rather than submitting again. `provenant status` shows active lanes and the twenty newest settled
lanes, newest first. `--json` returns full records for the same selection.
Use `--all` with `--limit N` and the returned `--cursor N` to page all history.

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
successful lane settle as `review_required`. `run --wait` and `lanes` show the
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
Fabric tasks and manifests accept `"build_heavy": true`. `lanes --json` reports
the applied value as `priority`, and `requested_priority` when it differs. The
value is null before it is known and for older records without this evidence.
If the guardian already has a higher nice value, the provider inherits that
lower scheduling priority. If changing priority fails, the provider still
launches at its inherited priority and the reason is recorded as a warning.

When run from a linked Git worktree with no `--mode` and no target
(`--worktree`, `--branch` or `--worktree-branch`), `run` refuses with `REFUSED`
and names both repairs: `--worktree <path>` for a writer, or `--mode read-only`
for a reader.

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

The receipt names the worktree. `--worktree PATH` uses an existing checkout
without taking over its lifecycle. A missing branch name creates an owned
checkout; for an absolute path or a path below `.worktrees/`, use
`--branch BRANCH` to create it. Git's worktree inventory establishes membership,
so native linked checkouts and checkouts recreated at a previously used path
need no registration or adoption. Another project's checkout is refused.
A contended worktree registry or custody lock reports `WRITER_LOCK_BUSY`.
With `--wait`, admission retries lock contention and unknown writer observations
until the wait or admission deadline, whichever is sooner; without `--wait`,
it refuses after the existing lock budget. A live writer is refused immediately
with `WRITER_BUSY`. Inspect custody when writer state remains unknown.

A held writer lock reports busy; lifecycle removal and admission refuse an observed
live process and name its pid, command and working directory when available.

Live-writer detection at run admission excludes the invoking process and, by default,
its verified ancestor chain (such as the invoking shell). A run launched from inside
the target worktree is admitted when no other writer or foreign process is active in
the tree. When a genuine live writer or foreign process is observed, admission is refused
with an error naming the observed process (PID, command and working directory), rather
than blaming a previous attempt. Hosts can tighten this policy by setting
`confinement.census_exclude_ancestors = false` in `[knobs]` in host configuration
(default `true`); when disabled, any ancestor shell whose working directory is inside
the worktree is treated as a live writer and refused.

A new writer may reuse a registered worktree from a terminal lane when the tree is clean and
writer custody is free. Dirty trees, held locks and observed live writers still refuse. When
`--base` is supplied for a branch that already exists, Provenant accepts the branch only when
it already contains that base (reusing its existing worktree or creating one); otherwise the refusal
names the branch head and the base, and says to omit `--base` to reuse it.
If a linked checkout has no Provenant lane-owner record, the worktree list
shows that fact separately from its observed writer state. It may have been
created outside Provenant. Inspect custody and use `--worktree <path>` to select
the checkout explicitly; an unknown writer remains unknown until checked.

A refusal that says repository authority could not be revalidated includes the
underlying repository error and its reason. Inspect that reason and the
worktree's Git registration and configuration before submitting again.
Git tracking entries for other branches do not invalidate an unchanged
worktree. Other configuration changes still require a fresh probe.
After an unlaunched writer is rejected, reconciliation removes its retained
Git pin. Status and wait queue recovery. To retry directly, run
`provenant reconcile --id <run>` after resolving any reported cause.

Writers queue in arrival order for the repository Git setup lock, waiting up to
four hours by default. A dead waiter or holder releases its place. Set
`domains.fabric.lanes.setup_lock_wait_seconds` to a value from 1 to 14400 to
change this bound; the lock continues to serialise setup for the repository.
While a writer waits, `lanes` and `status` name `provenant-lane-setup.lock` as
the waiting reason. If the launcher exits, its wait indication clears.

Choose `--deliver none`, `push`, `integrate:integration` or `pr`. Push and PR
use origin and the lane branch. Integration targets must already exist;
`integrate:origin/integration` selects a remote branch. Local targets must be
checked out nowhere, including the delivering lane's own worktree. Integration
conflicts leave checkouts and destination refs intact. Inspect the failure and resolve it through the owning lane.

Publishing from a writer lane is opt-in and off by default: enabling it gives the
lane your GitHub credential, so a malicious lane can extract the token and act
outside the wrapper. By default the chair publishes through host-side delivery.
When enabled, writer lanes with open network can push and open pull requests using the host's
read-only gh configuration and the pinned `gh auth git-credential` helper for
HTTPS remotes; SSH remotes keep their own authentication. Custom gh configuration
paths and keychain-only logins are not mirrored. Hosts without gh or
`~/.config/gh` continue local work without publishing setup. Merge through `provenant land N --wait`, or hand the merge to the chair. Landing
verifies green CI at the PR's current head; add `--head SHA` to pin a reviewed
head. Enable publishing with
`--knob lanes.publish=true`, Fabric `knobs: ["lanes.publish=true"]`, or host
configuration `[knobs] "lanes.publish" = true`. Read-only lanes retain their
existing open-network gh read access.

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
When a writer settles as `review_required`, the chair can review its protected
changes and then deliver the reviewed head with (paths the project lists in
[`review_exempt`](configuration.md#writer-review-exempt-paths) never cause this):

```sh
provenant lane deliver --accept-review ATTEMPT
```

The delivery receipt records who accepted the review and the protected paths.
Lanes and peers cannot accept review. The same clean checkout and exact head
checks apply.
PR bodies contain bounded commit messages and run and attempt IDs. Default
and primary checkout branches require PR delivery. Owned checkout cleanup
requires a succeeded lane and successful requested delivery, with the recorded
head retained by the delivered ref or creation base. Local branches remain
until integration is proved. Cleanup uses the project's declared regenerable
and keep paths. `worktree-settlement.json` beside `result.md` records a pruning
intent before removal, then pruning or the retention reason. Failed and
cancelled lanes retain their worktrees. Explicitly supplied worktrees retain
their existing GC policy.

`provenant resume ATTEMPT --prompt-file followup.md` generates and prints an
operation ID; supply `--operation-id ID` for a planned retry. It pins the prior
attempt's applied provider, model and account, including when dispatch selected
a named route pool. A model capacity failure appears as `model_unavailable`
with its reason in lanes; resume can continue its provider session when supported.
Targets accept unique hex prefixes of at least eight
characters and `RUN/ATTEMPT`; an ambiguous prefix lists candidates. Resume
starts fresh by default when continuation state is unavailable, preserving authority with
original context and the previous result attached. Ownership must match the
lane lineage for a previously lane-owned tree. Explicit checkouts resume in
place unless a lane owner now names another lineage.
Attempts recorded before #1855 have no launch evidence of running under an
owned tree, so continuation resumes them in place (fail open).
A missing or reassigned checkout uses a fresh owned worktree when fallback is
allowed. Its receipt records `continuation: fresh`. Add `--no-fallback` to require
provider resume.
Custody, policy or launch refusals still need their named cause addressed.
An accepted continuation that is rejected during launch remains listed by
`provenant lanes --all`, with the reason on its attempt. Its result is empty
when no provider ran; `provenant output ATTEMPT` can still address it.
`provenant cancel ATTEMPT` generates and prints its operation ID. Cancel and
`provenant reconcile --id RUN` infer project scope from the run record.

Put graph documents and briefs in `.provenant/scratch/`. A legacy `.agent-run/`
is never used for new documents; Provenant does not migrate it. Graph setup
creates `graphs/` and excludes the scratch root through `.git/info/exclude`.
Every lane can read it: read-only lanes through their working directory, writers
through a read grant on the primary checkout's scratch, since their linked
worktree lacks untracked files. Scratch read grants use canonical paths and must
remain under the canonical primary checkout. `--read-root DIR` grants any other
directory; `--prompt-file` grants its file. A bare `--document plan.json`
resolves there when no current-directory file exists. Explicit project-local
paths also work. A graph can use node `delivery: "integrate:integration"`
with top-level `integration_branch: "integration"` and `delivery: "pr"` for one
final PR. Delivery failures block dependants or retain the graph advance error;
`fabric graph resume --document PATH` retries final delivery. Loop rounds
integrate fast-forward only. If a loop round's delivery fails at the Integrate
stage, the integration branch moved after the round launched; rerun the graph
from the new head rather than resuming it.

Drain writer lanes before upgrading, as described in the
[upgrade procedure](install.md#bootstrap-and-upgrade). Upgrading does not
force-cancel running lanes. An older interrupted settlement with its private
objects can resume through the new Git receive import. Without those objects,
recover its commits on the host and start a fresh run; the refusal names this
recovery path.

`provenant reconcile --id RUN` queues settlement in the background and returns
without waiting for Git import. The supervisor resumes its durable phase after
a crash, with at most three attempts that reach import and backoff between
claims. Concurrent reconcile requests share one settlement worker;
queued or terminal-only runs do not spawn one. `provenant cancel ATTEMPT` records
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
import, ancestry checks and instruction quarantine at settlement. If Git cannot
write rebase state, stop with `needs_host_rebase`; do not create a clone or
second checkout to bypass the permission failure. A host-authorised merge of
`origin/main` is an alternative when rebasing is blocked.

On macOS, existing hard-linked files in the writable worktree no longer refuse
a launch or become write-denied; only Git control files with another name do.
See the [writer limits](providers.md#writer-lanes).
The hard-link fixtures remain enabled in macOS CI because lane sandboxes can
deny their `hard_link` calls before the launch check runs.

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
provenant status --wait=30m --until all --target lane:RUN_ID --target land:2721
provenant status --wait=1h --follow --target graph:GRAPH_ID --target pr:2721 --json
```

`status ID --wait` also keeps the usual lane/graph ID resolution. `--target`
is repeatable; explicit prefixes disambiguate lanes, graphs and PR numbers.
`land:N` observes PR N only: it grants no merge authority. Use `land N --wait`
when a merge is authorised.

The default `--until any` returns on the first observed semantic transition,
or immediately for a terminal target.
An initially running lane selection does not count its older settled attempts.
`--until all` waits for every target to become terminal. A lane selection is
terminal when all its attempts, including policy successors, have settled;
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
checkpoints; use the graph's returned resume command when continuation needs
an explicit decision. Repeated status calls or shell sleep loops are unnecessary.
