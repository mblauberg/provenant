# Lane lifecycle
Owner: run and Fabric maintainers
Anchor: docs/specs/run.md, docs/specs/fabric.md

Start an approved writer with the `implement` template (`deliver` for the
composite through CI, review and landing), inspect `--explain`, then submit
without it. A worktree's owning lane decides who else writes in it; authorised
writers share its occupancy lease. Independent slices use separate worktrees
and integrate into one branch before full review. Mechanism (confinement,
custody, settlement, priorities) is in the [run spec](../specs/run.md) and
[supervisor spec](../specs/supervisor.md); templates are in
[routing and templates](routing-and-templates.md).

## Launch refusals

A refusal names its cause and repair. These rules sit behind them.

- After any worktree refusal run `provenant worktree doctor PATH`: it only
  observes, and reports the state, the reason and one suggested command. A
  checkout with no lane owner record may have been made outside Provenant:
  check custody, then pass `--worktree PATH` explicitly.
- A nested writer may share its parent's exact live checkout, edits included.
  Independent work gets a separate checkout. Dirty trees and retained import
  fences protect ordinary reuse; a clean worktree from a terminal lane is reused.
- `write_scope` path claims are advisory: they warn the owning lane about
  overlaps and refuse nothing. Commit only owned paths with
  `git commit -- <owned paths>`; Git serialises commits through `index.lock`.
- Removal and GC need exclusive occupancy with no live holders. Never discard a
  live holder's worktree or delete its lease to bypass custody.
- Linux has no enforcing sandbox backend, so a selected enforcing profile fails
  rather than running unconfined. Each lane resolves a
  [sandbox mode](configuration.md#lane-sandbox-mode); `status` shows
  `sandboxed(<source>)` on sandboxed lanes.

## Delivery

After a settled writer without delivery:

```sh
provenant lane deliver NAME/NODE#N --deliver pr     # none | push | integrate:BRANCH | pr
```

It refuses a dirty checkout, changed head or live custody; repeating a
successful delivery returns its receipt. Default and primary checkout branches
require `pr`. An integration target must already exist and be checked out
nowhere, the delivering lane's worktree included; conflicts leave checkouts and
refs intact. Failed and cancelled lanes keep their worktrees.

- Publishing from a writer is on by default and gives it your GitHub
  credential. Set `lanes.publish` to `false` to withhold it, so the chair publishes
  through host-side delivery. The provider receipt's `github_credential` names
  the identity, source and delivery each lane got.
- An advisory `gh` deny list (`deny_verbs`, `deny_labels`) goes in
  `projects/PROJECT_ID/github.json` under the state root; matching calls warn
  and continue. The lane's `gh` warns every `lanes.gh_budget` calls and never
  stops one.
- A lane opens its pull request with `gh pr create --body-file`; `provenant pr
  open` is for a settled attempt (see [GitHub workflow](github-workflow.md#branch-commit-pull-request)).
- Writers follow `lanes.ci_scope` (default `none`: push and report, and the chair
  runs CI once for the batch). Pass `--set attempt.ci_scope=targeted` (or `full`)
  for a lane that must wait for CI. `run --explain` shows both values and their
  sources.
- Missing GitHub tools, a failed login lookup or an access restriction warn;
  ordinary lane work continues without publishing credentials.

## Cargo target and seeding

Lanes build in the worktree's own target in the lane cache
(`lanes.cargo_target` is `private`), cloned from the repository's host seed. The
[run spec](../specs/run.md#cargo-target-seed) owns the lifecycle,
[configuration](configuration.md#lane-cargo-target) the knobs and [GC](gc.md)
retention.

- Only `provenant worktree seed-build` and tick build host seeds, from the
  configured verify base and never from uncommitted work. After a toolchain
  change no seed matches until one of them runs. A seed whose fingerprints name
  no readable dep-info is not used (`seed_layout_unrecognised`).
- A `seeded` report proves the clone. Where the filesystem cannot clone, the lane
  warns `clone_failed` and builds cold. Cargo's verbose `Fresh` lines prove
  dependency reuse.
- Settlement starts a detached cleanup that removes the target once writer
  custody is free; worktree removal and lane-cache GC catch the rest. A
  continuation reuses a target still present and otherwise seeds a new one.
- Outside a lane, `provenant worktree seed-target PATH` seeds the same target
  and prints it for `CARGO_TARGET_DIR`.
- `domains.fabric.lanes.cargo_seed.max_generations` limits history per
  repository and `max_bytes` limits the host, so the cap can evict a
  repository's only generation. Compiler, host platform and effective lane
  profile must match; a lane with unsupported compiler, target or build-flag
  overrides builds cold and warns with the setting class, never its value.
- Native npm, pnpm and Corepack launches keep host cache selectors when a
  provider changes HOME or XDG state; explicit settings win. Corepack keeps its
  downloads shared through `COREPACK_HOME`.

## Recovery

Read `status TARGET`, `output TARGET` and `trace TARGET` before replacing work,
address the recorded cause, and never launch a second writer while custody is
uncertain or reset a worktree with edits.

| Intent | Command |
| --- | --- |
| Resume unfinished work | `provenant retry GRAPH` keeps completed outputs and reconciles uncertain launches under their recorded identity. |
| Retry a node | `provenant retry GRAPH/NODE`, optionally `--brief TEXT` for a replacement brief; review and land gates still apply. |
| Fall back to another route | Configured routes handle a typed unavailability once custody release is confirmed; an uncertain handoff blocks the switch. |
| Start fresh | A new `--name`. The same input under the same name finds its graph; changed input starts `NAME-2`. |
| Settle an unlaunched or stuck attempt | `provenant reconcile TARGET` after resolving any reported cause; it queues settlement in the background. |

- `stop TARGET` stops a subtree and reports the request pending until the
  launcher lets go. Started attempts are immutable; `edit` changes unstarted
  nodes only.
- A fenced import names the retained attempt and an explicit abandonment
  command. Abandonment keeps the private Git evidence but stops automatic
  import, so recover that work by hand. For a retained queued lane that never
  dispatched, run `provenant reconcile --abandon NAME/NODE#N --reason TEXT`; it
  refuses attempts with process evidence, a fence or worktree custody.
- `reconcile` reports storage errors in `writer_reservation_errors` and exits
  non-zero.
- A valid handoff declaring `stopped_early` or `blocked` makes an otherwise
  successful lane settle as `review_required`.

## Waiting

One blocking call covers mixed work; never poll or sleep in a loop:

```sh
provenant wait NAME/NODE NAME2 --any --pr 2721 --timeout 30m
provenant wait NAME --follow --timeout 1h --json
```

Targets are graph names, nodes or attempts; `--pr NUMBER` takes a pull request.
A pull request wait observes only and grants no merge authority (`provenant land`
merges). An expired wait exits 124 and leaves work running; a refused operation
exits 1. Sources are sampled, so intermediate states between samples are not
reported. Flags are in `provenant wait --help`.

## Tests and Git

Tests run Git only through `provenant_os::fixture` (`git_command()` or
`run_git(cwd, args)`), which drops the lane's `GIT_*` binding so a fixture never
commits into the lane's own repository (the workspace policy gate enforces it).
The [verifier](../../scripts/verify-nextest) also clears the calling lane's
`PROVENANT_GIT` executable override before tests; fixtures that need a selected
executable set it explicitly.
