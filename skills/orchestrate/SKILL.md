---
name: orchestrate
description: "Use to delegate or parallelise work: lanes, graphs, templates, review panels, waiting and landing."
---

# Orchestrate

`provenant templates` lists bundled, user, repository and inline templates and
`provenant templates show NAME --json` inspects one. Dotted names select
flavours such as `review.correctness`. `provenant help --all` lists commands
and options; `provenant run --explain` shows a compiled graph without starting
work. Check its authority, write scope and gates before submitting it.

## One accountable chair

One accountable chair owns coordination; native execution stays within its
assigned authority and never becomes a second chair or a parallel owner of the
same work. Only the chair holds the user, the boundaries, merge and release.
This rule names no lane shape: a lane that launches children coordinates only
its own subtree, with authority narrowed from its parent, and a native sub-agent
never re-plans or re-delegates the parent's whole assignment. The sections below
describe how the chair exercises that authority, not ways to divide it.

## Divide the work

The global rules own when and how to delegate. Any lane may launch child lanes.
`implement` starts a junior writer; `--route` picks another tier and `--plan`
sizes the brief first.

`run implement --branch NAME --base REF` creates a writer worktree;
`provenant worktree create BRANCH` and `--worktree` reuse one. `batch` gives
each slice its own worktree and integrates slices as they settle (see the
routing-and-templates runbook for `after` ordering and `max_parallel`). Name
disjoint write scopes for parallel writers; an overlap is an advisory
`PATH_CLAIM_OVERLAP` warning, never a refusal.

`lanes.writer_dirty` decides how a launch treats uncommitted edits already in
an existing worktree. By default (`wip_commit`) a tree whose lease has lapsed
has its edits committed as a local WIP commit and reported with
*WORKTREE_ADOPTED_DIRTY* naming the commit; `refuse` stops the submission
before a node spends tokens, and `continue_in_place` keeps the edits and warns.
Custody is a lease, not an exclusive lock: a tree whose holder is still live is
admitted under `lanes.worktree.sharing` (`warn` by default), and a tree whose
holder has settled is continued with `WRITER_HANDOVER`, keeping the prior
lane's uncommitted files in place. Only destruction of another live lane's work
is refused. The effective values and their sources are named in each warning
and refusal; override per run with `--knob lanes.writer_dirty=refuse`.

Inside a lane, `provenant run TEMPLATE` adds a child node to the calling graph
and `--name` names it. Attached children are cancelled when their parent
settles; `wait` collects them first, and `--detached` lets one outlive its
parent.

`cloud launch` hands a brief to a provider's cloud session and prints its link
and key; Provenant keeps no lifecycle for it. `cloud collect KEY` adds a
worktree holding the branch it pushed, which then goes through review like any
other.

## Worktree custody is a lease

A worktree and its branch carry a custody lease held by the launching attempt.
The lease is live while that attempt is nonterminal and lapses when it settles
(done, stopped, failed or cancelled); it is derived from the attempt state on
every read, so a crash cannot leave a stale reservation and no sweeper is
needed. A holder whose host is unreachable stays live until
`domains.fabric.lanes.lease_unreachable_seconds` (900), then lapses with
`LEASE_HOLDER_UNREACHABLE`.

Continuing a lapsed lease needs no flag: `--worktree`, `--branch`, `retry`,
`resume`, queue relaunch and `--adopt` all take the lease, warn `WRITER_HANDOVER`
with the prior lane, its terminal state and last head, and record the handover.
The prior lane's uncommitted files stay in place, listed in the warning and the
new brief footer, never reset, stashed or discarded. A head that moved is
re-pinned with `WRITER_HEAD_MOVED`, and an unreadable predecessor record is a
typed operation error, not an ownership refusal.

A second live writer follows `lanes.worktree.sharing` (host, project, launch or
owning-lane layer; default `warn`): `allow` proceeds silently and records the
co-writer, `warn` proceeds with `WORKTREE_SHARED` naming the holder, its state
and the setting, and `refuse` stops with `WORKTREE_SHARING_REFUSED` naming the
holder and source, with `next: provenant wait HOLDER` or `--destination new`.
The owning lane decides for its own tree with `--worktree-sharing refuse` or
`provenant lane share refuse`; stored in its lease, that wins over the other
layers for that worktree only. Path claims (`--write-scope`, `path_claim`) stay
advisory. Git's index lock and ref compare-and-swap protect concurrent commits;
a lost index lock retries up to `domains.fabric.lanes.index_lock_retries` (5)
before failing `Transient`.

Only destruction is refused: a reset, checkout, stash, clean, force-push,
non-fast-forward ref update, branch deletion or worktree removal that would
destroy another live lane's files or ref fails `DESTROYS_LIVE_WORK`, naming the
holder, its state and the files or ref, with `next: provenant wait HOLDER` or
`provenant stop HOLDER`. Adding commits, fast-forward pushes and edits to other
paths proceed. A lapsed lease never triggers that guard, but its uncommitted
files are retained under `<state>/retained/<attempt>/` before removal.

## Brief preamble

Standing lane rules go in a layered `lanes.brief_preamble` file, not in each
brief. The knob names a file at the host layer (`<state>/config.toml`) and at
the project layer (`.provenant/config.toml`) under `[knobs]`; `provenant run
--preamble-file PATH` adds the launch layer. The three layers concatenate in
host, project, launch order and each is labelled with its source in every lane
preamble, so briefs carry only the task. A file scoped to one role starts with
`<!-- provenant-roles: writer, review, read-only -->` on its first line and is
shown only to those roles; a file with no such header applies to all. A missing
or unreadable file warns with its layer and the launch proceeds. Cargo target,
CI scope and verification lines keep their own knobs and are not repeated in
the preamble file.

A project may declare cheap `verify.static_checks` entries in its project,
host or bound host-project port table: each has an `argv`, a repository-relative
`cwd` and a `timeout_seconds` cap. The writer preamble names them with their
source and tells the writer to run them before pushing. They need no build. A
failure is reported in the declaration, does not refuse the push, and CI still gates
the merge. The same list is recorded as `static_checks` on the launch receipt, with
each check carrying its source; with no declaration there is no preamble text
and no receipt field.

## Wait and recover

Block on one `wait` call, with `--any` for the first settlement; never poll.
Expiry leaves targets running. Reconcile the returned event before resuming.
Recover an existing graph with `status`, `output` and `retry` before launching
replacement work. Read a settled lane with `result NAME`, which carries its
declaration, head and verification; do not parse provider logs. An idle run or
empty queue means no eligible work now, not done.

For a Claude Code chair, arm a Monitor on `provenant events --follow --json
--consumer chair-<session>`; each JSON line is a notification while the chair
keeps working. A Monitor can expire; re-arm it with the same consumer to resume
from its cursor. For one outcome, run `provenant wait … --json` in the
background and receive the notification on exit. Delivery is at least once:
reconcile the result and deduplicate by event identity.

## Collect

Check commits, paths, acceptance evidence, review subject and the resolved
`adapter/model@effort` against the output, not the requested tier. A failed
child blocks dependent work, and a synthesis cannot waive a gate. Integrate each
ready head in dependency order, run affected checks and use proportionate review
(`code-review`). Read CI as `deliver` describes, not with one `gh` read per job.

[Lanes and panels](references/lanes.md) covers briefs, confinement and review
independence. [Herdr](references/herdr.md) covers watching panes and peers.
