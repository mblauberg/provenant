# Lane lifecycle

Owner: run and Fabric maintainers
Anchor: docs/specs/run.md, docs/specs/fabric.md

Create and own a local writer checkout with one dispatch:

```sh
provenant dispatch --operation-id fix-issue --model codex/gpt-6.1-sol \
  --mode worktree-write --worktree-branch fix/issue --base main \
  --deliver push --prompt-file brief.md
```

The receipt names the worktree. `--worktree PATH` still uses an existing
registered checkout without taking over its lifecycle. To register a native
linked checkout, use `provenant worktree adopt PATH --owner manual`; another
project's checkout and a live or reserved writer are refused.

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
`~/.config/gh` continue local work without publishing setup. Merge through
`provenant pr merge --head SHA N` or
`pr land`, or hand the merge to the chair. Publishing defaults on; disable it with
`--knob lanes.publish=false`, Fabric `knobs: ["lanes.publish=false"]`, or host
configuration `[knobs] "lanes.publish" = false`. Read-only lanes retain their
existing open-network gh read access.

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
cancelled lanes retain their worktrees. Explicitly supplied and adopted
worktrees retain their existing GC policy.

`resume ATTEMPT --operation-id ID --prompt-file followup.md` starts fresh by
default when continuation state is unavailable, preserving authority with
original context and the previous result attached. Ownership must match the
lane lineage for a previously lane-owned tree. Explicit unowned and adopted
checkouts resume in place unless a lane owner now names another lineage.
Attempts recorded before #1855 have no launch evidence of running under an
owned tree, so continuation resumes them in place (fail open).
A missing or reassigned checkout uses a fresh owned worktree when fallback is
allowed. Its receipt records `continuation: fresh`. Add `--no-fallback` to require
provider resume.
Custody, policy or launch refusals still need their named cause addressed.

Put graph documents and briefs in `.provenant/scratch/`, or `.agent-run/` when
that root already exists. Graph setup creates `graphs/` and excludes the
scratch root through `.git/info/exclude`. A bare `--document plan.json` resolves
there when no current-directory file exists. Explicit project-local paths
also work. A graph can use node `delivery: "integrate:integration"` with
top-level `integration_branch: "integration"` and `delivery: "pr"` for one
final PR. Delivery failures block dependants or retain the graph advance error;
`fabric graph resume --document PATH` retries final delivery.

Hard-linked writable files still block macOS launch. The diagnostic names the
file. Inspect it before removing disposable build output, then retry. The
hard-link fixture remains enabled in macOS CI because lane sandboxes can deny
the fixture's `hard_link` call before the launch check runs.
