# Chair loop

For a long-running chair that keeps several lanes busy across many wakes. A
project may supply its own chair skill with concrete commands; this reference
owns the portable protocol that such a skill specialises.

## Per-wake loop

A wake is a lane finishing, an owner message or the fallback timer. On every
wake, in this order:

1. **Reconcile.** Read the session state file (`session` owns it and the
   `.agent-run/` layout: one `STATE.md` per chair under `sessions/<session-id>/`),
   then live evidence:
   run states, native subagents, worktrees (dirty, ahead, merged), open pull
   requests, free memory. Evidence wins; correct the state file.
2. **Land** what is ready under the repository's merge workflow, then prune.
3. **Review** finished lanes; adjudicate returned reviews.
4. **Refill** from the frontier up to the resource budget.
5. **Tracker:** make issues and the board match what started and landed, when
   tracker writes are authorised — following `tracker`'s filing, linking and
   hygiene rules rather than restating them here; hand steady-state upkeep to
   its steward mode instead of doing it inline.
6. **Checkpoint** the state file, then arm the wait.

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

The chair never loops `fabric_status` calls. After dispatch it arms one wait:

- Where the harness notifies when a background command exits (Claude Code's
  background Bash), run `fabric watch <ids>` there and continue useful work.
  Where the harness offers a timer, also set one fallback wake of at least 20
  minutes in case the notifier dies.
- Elsewhere, block on `fabric watch <ids>` within the tool's largest timeout
  and re-arm it if the timeout expires.

`fabric watch` polls locally and prints only state changes, so the wait costs
no model tokens. It has no timeout of its own.

A dispatching sub-agent still blocks in the foreground on its own worker; see
[worker-liveness.md](worker-liveness.md). Run `fabric watch` from the directory
whose Fabric identity dispatched the runs, or it cannot find them.

## Token hygiene

- Delegate inspection. Exploration beyond a few files or commands goes to a
  lane that returns a digest and a path.
- Read bounded slices: `fabric_output` with `max_bytes` and `tail`, the result
  part before events or transcripts. Read diffs by stat first and open only what
  a verdict hinges on.
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
2. **Members.** At least three, across at least two model families.
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

## Waiting on the owner

When an item waits on the owner, such as a prototype review or a question, park
it with a ledger row or tracker comment and refill with other work. Never idle
the loop on it. If it truly blocks, lies inside authority and is not a user
gate, convene a decision council.
