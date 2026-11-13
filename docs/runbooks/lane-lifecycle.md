# Lane lifecycle
Owner: run and Fabric maintainers
Anchor: docs/specs/run.md, docs/specs/fabric.md

Start an approved writer with the `implement` template (`deliver` for the
composite through CI, review and landing), inspect `--explain`, then submit
without it. One writer owns each worktree; parallel slices integrate into one
branch before full review. Mechanism (confinement, redaction, custody, settlement,
priorities) is in the [run spec](../specs/run.md) and
[supervisor spec](../specs/supervisor.md); templates are in
[routing and templates](routing-and-templates.md).

## Launch refusals

- `run` from a linked worktree with no `--mode` and no target refuses with
  `REFUSED` and names both repairs: `--worktree PATH` for a writer or
  `--mode read-only` for a reader. To read another registered checkout pass
  both `--mode read-only` and `--worktree PATH`; readers take no writer lease.
- A new branch without `--base` starts from the fetched default branch of
  `origin` (local `HEAD` with a warning if that fails). With `--base` on an
  existing branch, the branch must already contain it. A branch checked out
  elsewhere must be selected with `--worktree`.
- After any worktree refusal run `provenant worktree doctor PATH`: it only
  observes, and reports the state, the reason and one suggested command (replace
  its `BRANCH` and `PATH` placeholders). A checkout with no lane owner record
  may have been made outside Provenant: check custody, then pass `--worktree
  PATH` explicitly.
- Dirty trees, held writer locks and retained import fences refuse a new
  writer; a clean worktree from a terminal lane is reused.
- Writer rebases use the lane's private Git and pass host import checks. If Git
  cannot write rebase state, stop with `needs_host_rebase`; never create a clone
  or second checkout to bypass it. A host-authorised merge of `origin/main` is
  the alternative.
- Linux has no enforcing backend and refuses lane launch rather than run
  unconfined. Each macOS lane resolves a
  [sandbox mode](configuration.md#lane-sandbox-mode); `status` shows
  `sandboxed(<source>)` on sandboxed lanes. Sandboxed lanes cannot create hard
  links or bind Unix sockets outside granted paths; `cargo nextest run --profile lane` excludes fixtures
  needing them and CI's `ci` profile keeps them.

## Delivery

After a settled writer without delivery:

```sh
provenant lane deliver ATTEMPT --deliver pr     # none | push | integrate:BRANCH | pr
```

It refuses a dirty checkout, changed head or live custody; repeating a
successful delivery returns its receipt. Default and primary checkout branches
require `pr`. Integration targets must already exist and be checked out
nowhere, the delivering lane's worktree included; conflicts leave checkouts and
refs intact. Failed and cancelled lanes keep their worktrees. Publishing from a
writer is on by default and gives it your GitHub credential; set
`lanes.publish=false` to withhold it so the chair publishes through host-side
delivery. An advisory `gh` deny list (`deny_verbs`, `deny_labels`) goes in
`projects/PROJECT_ID/github.json`; it guards against accident, not as a boundary.

GitHub login lookup uses the host-recorded executable search path and GitHub
configuration paths for initial launches and background retries alike. Missing
GitHub tools, failed login lookup or access policy restrictions produce a warning;
ordinary lane work can continue without publishing credentials.

## Recovery

Read `status TARGET`, `output TARGET` and `output TARGET --trace` before
replacing work, address the recorded cause, and never launch a second writer
while custody is uncertain or reset a worktree with edits.

| Intent | Command |
| --- | --- |
| Resume unfinished work | `provenant retry GRAPH` keeps completed outputs and reconciles uncertain launches under their recorded identity. |
| Retry a node | `provenant retry GRAPH/NODE`, optionally `--brief TEXT` for a replacement brief; review and land gates still apply. |
| Fall back to another route | Configured routes handle a typed unavailability once custody release is confirmed; an uncertain handoff blocks the switch. |
| Start fresh | A template with `identity = "fresh"` or a new operation ID. An unchanged `identity = "content"` submission reattaches, even after cancellation; `retry GRAPH` resumes it. |
| Settle an unlaunched or stuck attempt | `provenant reconcile --id RUN` after resolving any reported cause; it queues settlement in the background. |

`stop TARGET` stops a subtree; if the launcher has not let go in about four
seconds it reports the request pending and settles as it unwinds. Started
attempts are immutable; `edit` changes unstarted nodes only. A fenced import
names the retained attempt and an explicit abandonment command; abandonment
keeps the private Git evidence but stops automatic import, so recover that work
by hand. For a retained queued lane that never dispatched, use
`provenant reconcile --abandon RUN/ATTEMPT --reason TEXT`; this preserves its
evidence and refuses attempts with process evidence, a fence or worktree custody.
`reconcile` reports storage errors in `writer_reservation_errors`
and exits non-zero. A valid handoff declaring `stopped_early` or `blocked` makes
an otherwise successful lane settle as `review_required`.

When a release breaks state readability, finish or stop pending graphs and drain
writers on every host before [upgrading](install.md#bootstrap-and-upgrade).

## Waiting

One blocking call covers mixed work; do not poll or sleep in a loop:

```sh
provenant status --wait=30m --until any --target lane:ATTEMPT --target graph:GRAPH --target pr:2721
provenant status --wait=1h --follow --target graph:GRAPH --json
```

`--target` is repeatable (`lane:`, `graph:`, `pr:`, `land:`, `operation:`);
`land:N` observes only and grants no merge authority (`provenant land N --wait`
merges). `--until all` (default) waits for every target to be terminal or a graph to be paused, `any`
returns on the first transition; `--follow` streams and always waits for all.
An expired wait exits 124 and leaves work running; a refused operation exits 1.
Sources are sampled, so intermediate states between samples are not reported.

## Tests and Git

Tests run Git only through `provenant_os::fixture` (`git_command()` or
`run_git(cwd, args)`), which drops the lane's `GIT_*` binding so a fixture never
commits into the lane's own repository (`tests/workspace_policy.rs` enforces it).
