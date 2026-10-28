# Lane lifecycle

Owner: run and Fabric maintainers
Anchor: docs/specs/run.md, docs/specs/fabric.md

Create a writer checkout, submit and wait with one command:

```sh
provenant run --prompt-file brief.md --route senior \
  --branch fix/issue --base main --deliver push
```

`run` waits up to 240 seconds, then prints the state and a result tail. Use
`--wait 30m` or `--wait 3600s` for a longer background wait. Timeout prints the
attempt ID and exact command to keep waiting; follow it rather than submitting
again. `provenant status` shows active lanes and the twenty newest settled
lanes, newest first. `--json` returns full records for the same selection.
Use `--all` with `--limit N` and the returned `--cursor N` to page all history.

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

The receipt names the worktree. `--worktree PATH` uses an existing checkout
without taking over its lifecycle. A missing branch name creates an owned
checkout; for an absolute path or a path below `.worktrees/`, use
`--branch BRANCH` to create it. To register a native linked checkout, use
`provenant worktree adopt PATH --owner manual`; another project's checkout and
a live or reserved writer are refused, and a live refusal names the process
(pid and command). The same command replaces a stale registration, such as one
left by an earlier tree at the same path; run it from outside the tree, since a
shell inside the tree counts as a live process.

Choose `--deliver none`, `push`, `integrate:integration` or `pr`. Push and PR
use origin and the lane branch. Integration targets must already exist;
`integrate:origin/integration` selects a remote branch. Local targets must be
checked out nowhere. Integration conflicts leave checkouts and destination
refs intact. Inspect the failure and resolve it through the owning lane.
Legacy `--deliver-remote`, `--deliver-branch` and `--open-pr` remain available
as an alternative to `--deliver`.

Writer lanes with open network can push and open pull requests using the host's
read-only gh configuration and the pinned `gh auth git-credential` helper for
HTTPS remotes; SSH remotes keep their own authentication. Custom gh configuration
paths and keychain-only logins are not mirrored. Hosts without gh or
`~/.config/gh` continue local work without publishing setup. Merge through `provenant land N`, or hand the merge to the chair. Landing
verifies green CI at the PR's current head; add `--head SHA` to pin a reviewed
head. Publishing defaults on; disable it with
`--knob lanes.publish=false`, Fabric `knobs: ["lanes.publish=false"]`, or host
configuration `[knobs] "lanes.publish" = false`. Read-only lanes retain their
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
changes and then deliver the reviewed head with:

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
cancelled lanes retain their worktrees. Explicitly supplied and adopted
worktrees retain their existing GC policy.

`provenant resume ATTEMPT --prompt-file followup.md` generates and prints an
operation ID; supply `--operation-id ID` for a planned retry. It pins the prior
attempt's applied provider, model and account, including when dispatch selected
a named route pool. A model capacity failure appears as `model_unavailable`
with its reason in lanes; resume can continue its provider session when supported.
Targets accept unique hex prefixes of at least eight
characters and `RUN/ATTEMPT`; an ambiguous prefix lists candidates. Resume
starts fresh by default when continuation state is unavailable, preserving authority with
original context and the previous result attached. Ownership must match the
lane lineage for a previously lane-owned tree. Explicit unowned and adopted
checkouts resume in place unless a lane owner now names another lineage.
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

Put graph documents and briefs in `.provenant/scratch/`, or `.agent-run/` when
that root already exists. Graph setup creates `graphs/` and excludes the
scratch root through `.git/info/exclude`. Every lane can read it:
read-only lanes through their working directory, writers through a read grant on
the primary checkout's scratch, since their linked worktree lacks untracked
files. Scratch read grants use canonical paths and must remain under the
canonical primary checkout. `--read-root DIR` grants any other directory;
`--prompt-file` grants its file. A bare `--document plan.json` resolves
there when no current-directory file exists. Explicit project-local paths
also work. A graph can use node `delivery: "integrate:integration"` with
top-level `integration_branch: "integration"` and `delivery: "pr"` for one
final PR. Delivery failures block dependants or retain the graph advance error;
`fabric graph resume --document PATH` retries final delivery.

Writer lanes keep the confinement policy compiled at launch. A diagnostic naming
a policy before v11, with its run and attempt IDs, at settlement or delivery means that lane retained the old
shared-object write grants; the host runs a full strict check of the shared
store before consuming its head. Allow these writers to settle before upgrading
to the release that introduces private writer object stores, as described in
the [upgrade procedure](install.md#bootstrap-and-upgrade). Upgrading does not
force-cancel running lanes.

Object-store lock-wait timeouts leave settlement non-terminal and retain custody
and private objects until the separate limit of 30 timeouts. Retry settlement
with `provenant reconcile --id RUN` once the competing host verification
finishes. Three verification retries terminally refuse import and
release custody. Copying and indexing have a separate
120-second budget; pack-count or import-budget refusals are terminal and keep
the private evidence for recovery. See the
[verification deadlines](../specs/run.md#writer-object-isolation).

On macOS, existing hard-linked files in the writable worktree stay readable
but are write-denied, including inside ignored directories. Excessive denials
still refuse launch; see the [writer limits and recovery advice](providers.md#writer-lanes).
The hard-link fixtures remain enabled in macOS CI because lane sandboxes can
deny their `hard_link` calls before the launch check runs.
