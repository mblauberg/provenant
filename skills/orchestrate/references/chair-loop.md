# Chair loop

For a long-running chair that keeps several lanes busy across many wakes. A
project may supply its own chair skill with concrete commands; this reference
owns the portable protocol that such a skill specialises.

## Per-wake loop

A wake is a lane finishing, an owner message or the fallback timer. On every
wake, in this order:

1. **Reconcile.** Read the host session checkpoint with
   `provenant workflow session show <session-id>`, then live evidence:
   `provenant lanes`, native subagents, worktrees (dirty, ahead, merged), open pull
   requests, free memory. Evidence wins; correct the state file.
2. **Land** what is ready under the repository's merge workflow, then prune.
3. **Review** finished lanes; adjudicate returned reviews.
   A `quarantined` lane kept its result but not its `.agents/` edits: review the listed `protected.patch`, apply it deliberately in a worktree if adopted. Its history still holds the edit commit, so claim verification and Fabric landing refuse it until the chair rewrites the lane without that commit or releases the reviewed tip (see `orchestrate`). A `failed` lane with a restore fix still carries the edits: restore `.agents/` to the start commit first.
4. **Refill** from the frontier up to the resource budget.
5. **Tracker:** make issues and the board match what started and landed, when
   tracker writes are authorised — following `tracker`'s filing, linking and
   hygiene rules rather than restating them here; hand steady-state upkeep to
   its steward mode instead of doing it inline.
6. **Checkpoint** the state file, then arm the wait.

A wake that changes nothing stays silent. If reconcile shows no state change and
no decision is due, checkpoint and re-arm without an owner-facing message. A
landing, a new blocker, a due decision or an owner question still gets one.

Keep a wake short. Anything longer than a few commands goes to a lane.

## Frontier hook

The project declares one command or query that returns ready work not already
leased, for example a board query for `Ready` items in the current horizon
minus those with a live lane. Declare it in project instructions or the
project's chair skill. With a hook, the chair takes its order; without one, it
uses the tracker's declared ready state and asks the owner once. File
follow-ups under the right parent so the frontier stays true.

## Refill to the resource budget

Keep lanes running up to the smallest of: memory headroom divided by per-lane
cost, provider concurrency or quota, and review capacity. Refill whenever below
budget and independent frontier items exist. Measure free memory before
launching (on macOS, `vm_stat` free + inactive + speculative pages); Fabric
also queues attempts below its memory floor. Heavy processes (installs, suites,
typechecks) have their own smaller budget across all lanes, one at a time
unless the project declares a lock with more slots. Tell every lane its budget
in the brief. An empty frontier is an idle checkpoint, not a reason to invent
work.

## Fresh agents

One agent, one PR. Prefer a fresh agent briefed from files over a resume,
except for a tiny follow-up within about five minutes. Owner corrections go to
a fresh follow-up after the push; message a running agent only to stop harm.

## Lane ids

Start every lane id and native subagent description with a role prefix so
status lines and receipts group them: `tool-`, `skill-`, `ui-`, `api-`, `db-`,
`fix-`, `rev-`, `land-`, `scope-`. Projects may add prefixes. Follow it with a
short slug, and the issue number where one helps a human scan the status line:
`fix-1234-ledger-rounding`. A lane id is a status-line label, not a branch
name. When the lane owns a registered worktree, name its branch and worktree
per `setup-repo`'s branch naming doctrine instead — no issue number, no lane
code — independently of this lane id.

## Waiting

After dispatch, use one `provenant lanes --wait N` to wait up to N seconds
for any selected lane. Add `--condition all` to wait for the whole selection;
`--all` selects all lanes. Re-arm after each result. Use `--limit N` to bound
the report and `provenant output RUN/TASK/ATTEMPT --tail N` to read a result
tail. For CI, inspect the exact PR head with `gh pr checks <pr>`; the chair waits
for the required `ci-status` check before merging. Do not start one
wait per lane. `provenant lanes` is the source of truth after compaction or
restart; page further results with its cursor. Read retained events with
`provenant output RUN/TASK/ATTEMPT --part events` and its byte offset.

A dispatching sub-agent still blocks in the foreground on its own worker; see
[worker-liveness.md](worker-liveness.md). Registered worktrees share the
project, so the waiter works from any cwd in that project. Start inbox triage
with `fabric_mail`'s `read` operation (`after`, `limit`); advance its read
cursor with `mark_read` (`through`, `operation_id`) after processing.

## Peers and relay discipline

A project may run one chair or specialised peer chairs (a lead that lands plus
domain chairs). Peer relays dominate attention if left loose:

- batch updates into one message per event; no acknowledgement-only replies;
- keep state in shared state files or Fabric tasks, not in chat;
- message a peer only when it must act or decide. Before each send, ask
  whether it must; if not, write it to state.

Peers message sessions, not lanes: a dispatched lane has no inbox and receives
new instructions only through `resume` or a new dispatch (see
[lane safety](lane-safety.md)).

## Blocked steps

When a subagent's routine step is refused by a permission classifier, the chair
runs it itself and reports it; it does not ask the owner or re-route the step to
another agent, which would launder the refusal. Routine means staging and
committing lane files, regenerating artifacts, merging and pushing lane or
integration branches, stopping idle local servers, pruning merged branches and
worktrees, and removing orphaned processes after checking them. Check the
target first; standing limits (no force-push, no shared-branch rewrites, no
deletion beyond authorised pruning, protected paths untouched) still hold. If
the classifier refuses the chair too, do not retry variants: batch the step into
one owner action list with a ready-to-run command and keep working.

## Delivery shape

Choose by size. Small fixes and features go **vertical**: one issue or a few,
end to end. Large related features, restructures and big refactors go
**horizontal**: layers in dependency order (for a front end, tokens, then
primitives, then blocks, then composites, then templates; for a back end, data
and read models, then API and contracts), each layer one cohesive PR the next
builds on, with vertical adoptions batched on top. Parallel vertical work on a
large related change creates overlap, redundancy and debt. Multi-issue PRs are
fine under either shape and cut the merge churn of parallel lanes. Classify the
work before batching it. When an epic is nearly done, re-home its leftover work
as leaves where it belongs, then close it once its done-when is verified.

## Token hygiene

- Delegate inspection. Exploration beyond a few files or commands goes to a
  lane that returns a digest and a path.
- Read bounded slices with `fabric_output`'s `read` operation (`stdout` or
  `stderr` part, target, offset and `max_bytes`); for the result or events use
  `provenant output --part result|events` before transcripts.
  Read diffs by stat first and open only what a verdict hinges on.
- Never read images in the chair. Captures go to reviewers, who look at them
  and return verdicts.
- Keep briefs in files and pass `prompt_file`; do not paste diffs into prompts.

## Reviewer git-verb denylist

Read-only lanes, including native subagents, often run inside shared
checkouts. Name the forbidden verbs in every reviewer brief: `checkout`,
`switch`, `restore`, `reset`, `stash`, `clean`, `rebase`, `merge`, `commit`,
`cherry-pick`, `am`, `apply`, `push`, `tag`, `branch -d/-D`, `worktree
add/remove`. Read other refs with `git show <ref>:<path>`, `git diff` and
`git log`. A lane that must write gets its own worktree.

## Decision council

Use when a decision blocks lanes, lies inside granted authority, and the owner
is away or has asked not to be consulted: competing reversible design
directions or a tooling choice within scope. Never for a user gate in
`HARNESS.md`, including an unresolved acceptance criterion, or a question the
owner already answered; the newest owner decision wins.

1. **Packet.** One file: question, options, constraints, prior owner
   decisions, evidence links.
2. **Members.** At least three, across at least two model families. A project
   may declare a smaller council; it still spans two families.
3. **Independent first round.** Members see only the packet. Each returns a
   choice, rationale and what would change its mind.
4. **Chair reconciles.** State the choice with its rationale, the strongest
   dissent and why it lost. Evidence decides, not vote count; a minority view
   may win.
5. **Record** the decision as an open row in the assumption ledger
   (`autopilot`) with members' routes and the reversal cost. Prefer the
   reversible option when the rest is close. Work continues; the owner confirms
   later.

The evidence council in [verification.md](verification.md) adjudicates claims;
this council makes a choice.

## Waiting on the owner or other sessions

When an item waits on the owner, such as a prototype review or a question, park
it with a ledger row or tracker comment and refill with other work. Never idle
the loop on it. If it truly blocks, lies inside authority and is not a user
gate, convene a decision council.

A chair whose remaining work is mostly waiting (another session's train, a
change window, owner applies) builds what it can now and hands the rest to the
owning session rather than idling: write a handoff file in the session
directory listing the pull requests, heads, lanes and close-out order, send
each owner its slice, mark the state file retired, then stop.

Show each new prototype at once (an Artifact, or screenshots for mobile) and
keep a short overview ready. Once a prototype is close, iterate in the product.
