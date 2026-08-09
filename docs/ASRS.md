# Architecturally significant requirements

These are the quality attributes Provenant is built to hold, the order they
take when they conflict, and the test a change has to pass before it adds
mechanism. They exist because the failure this harness is most exposed to is
not a bug. It is accreted machinery: each piece defensible on its own, the sum
unmaintainable by one person and unusable by an agent.

[ADR 0021](adr/0021-proportionate-mechanism.md) records the decision to hold
these. [ADR 0001](adr/0001-personal-first-product-compatible.md) sets the
personal-first posture they assume. `HARNESS.md` carries the binding one-line
form; `MAINTAINING.md` applies it to repository change.

## The operating context, stated once

Everything below follows from this, so contradict it only with evidence:

- **One user, one uid, one machine.** Every agent here runs as the same user
  and can already read every other agent's files. A boundary between them is
  bookkeeping, not security. [ADR 0020](adr/0020-retire-the-daemon-fabric.md)
  settled this by deleting the runtime built on the opposite assumption.
- **The repository is public; the work is not.** Published source needs licence
  hygiene and secret scanning, which is why `scripts/public-release-check`
  earns its size. Run state under `.agent-run/`, `.worktrees/` and diagnostics
  printed to the user's own terminal are never published and need neither.
- **The operator is the author, the reviewer and the entire support team.**
  There is no on-call rotation to absorb a flaky check and no second maintainer
  to discover what a mechanism was for.
- **The agents are the primary users.** A rule an agent cannot satisfy, or a
  failure it cannot act on, is worse than the gap it closed.

## The attributes, in priority order

When two conflict, the higher one wins, and the change description says so.

### 1. Reliability in use

Works on the first call, in the environment that actually exists, and when it
cannot, fails with something the agent can act on. This outranks theoretical
correctness. A gate that is right in principle and refuses in practice is a
defect, and the refusal being deliberate does not make it less of one.

Concretely: no mechanism may fail closed on an environment it merely did not
anticipate. Unknown is not hostile. Prefer resolving what is actually installed
over asserting what should be, prefer a fallback over a refusal, and prefer a
diagnosis naming the missing thing over a status code.

### 2. Low maintenance

Every mechanism is a standing tax on one person: it is read on every audit,
kept green in every gate, and migrated through every refactor. A capability
that saves an hour a year and costs an hour a quarter is a net loss no matter
how correct it is. Code with no caller is not free; it is the most expensive
kind, because it still has to be understood before it can be changed.

Concretely: prefer deleting to deprecating, prefer one shared helper to four
hardened call sites, and prefer a gate that discovers its inputs to a list
somebody has to remember to update.

### 3. Simplicity proportionate to the risk actually carried

Complexity is bought, not free, and the price has to be justified against a
real failure. Sophistication that would be right for a multi-tenant service is
wrong here, and the fact that it is *better engineering* in the abstract is
precisely the trap.

The load-bearing exception is stated in full below, because "simplify" must
never be read as licence to remove the checks that exist for observed agent
failure modes.

### 4. Flexibility and extensibility through seams, not options

Extension should mean adding a file where something already looks, not editing
a central registry, adding a configuration flag, or threading a parameter
through five layers. Discovery beats enumeration: a list of things to run is a
list somebody will forget to add to, and that forgetting is silent.

The worked case is in this repository. A skill's JavaScript tests sat
unexecuted because the gate named its test files explicitly. The fix was for
`scripts/check-harness` to find `*.test.mjs` under `skills/`, so the next one
runs the moment it lands. Apply the same shape to profiles, lenses, rules and
checks.

Conversely, resist flexibility nobody asked for. A configuration knob with one
caller and one value is not extensibility, it is an extra state to test.

### 5. Personal-first, product-compatible

Unchanged from [ADR 0001](adr/0001-personal-first-product-compatible.md).
Optimise for single-operator macOS use. Keep the cheap seams that would survive
productisation, such as the portable and local configuration split. Do not
build installers, cross-platform matrices, supply-chain apparatus or
contribution surfaces on speculation.

## What is not negotiable

Simplicity is not a reason to weaken these. Each closes a failure mode that has
actually been observed here, repeatedly, and each first occurrence is expensive
or invisible:

- **An author may not certify their own surface.** LLM agents report clean
  results their own output contradicts. This is the single most reliable
  failure mode in the system, and the review ladder in `HARNESS.md` exists for
  it. Cross-family review, fresh review contexts and the producer/validator
  separation in `deliver` all serve it.
- **The user gates.** Specification approval, acceptance and release authority
  stay separate user decisions.
- **Evidence outranks confidence.** A worker's report is a claim. Confirm
  commits in `git log`, counts against the worker's own transcript, and re-run
  any failure a worker calls environmental.
- **One writer per source surface.** Concurrency here is real, and the failure
  is silent corruption rather than a crash.
- **Public-repository hygiene.** Secrets, personal absolute paths, private
  project names and unlicensed third-party material stay out of published
  history.

When one of these looks like overengineering, the question to ask is whether
its *mechanism* is proportionate, never whether the *guarantee* is worth
keeping.

## The proportionality test

Answer all four before adding a mechanism. Put the answers in the change
description, not just in your head.

1. **Which failure?** Name a failure that has actually happened here, or one
   whose first occurrence is unrecoverable. "Could happen" is not an answer,
   and neither is "an attacker might".
2. **Who is the adversary?** If the answer is the user, their own shell, their
   own configuration or their own machine, there is no adversary. Defending
   against yourself costs reliability and buys nothing.
3. **What is the cheapest thing that detects it?** Compare honestly against a
   shared helper every call site uses, a one-line grep, a single test, a
   comment, and doing nothing and fixing it when it bites. Pick the cheapest
   that closes the named failure.
4. **What does it cost when it is wrong?** Estimate the failure mode of the
   mechanism itself. A check that fails closed on an unanticipated environment
   is a reliability regression wearing a safety badge.

A mechanism that cannot answer 1 and 2 does not go in.

## The removal test

Apply it during any audit. A mechanism is a deletion candidate when any of
these holds:

- it has no caller outside its own tests;
- no document tells an agent to use it, so no agent will;
- its rule is already enforced somewhere cheaper, such as a shared helper, a
  type, or a single grep;
- it has never fired, and the failure it guards has never been observed;
- it exists to satisfy a rule rather than a requirement.

Deletion is the default outcome. Preserving the tip on an archive branch and
recording it in the change description costs nothing and makes the deletion
reversible; that is how the retired subsystems were handled.

## Worked examples

Both directions, all from this repository, so the calibration is real.

| Case | Verdict | Why |
|---|---|---|
| The daemon fabric: five packages, ~350,000 lines, eighteen preconditions before a first message ([ADR 0020](adr/0020-retire-the-daemon-fabric.md)) | Removed, replaced by 605 lines | Agents could not use it. The trust boundary it enforced does not exist on one machine. |
| `scripts/check-harness` discovering `*.test.mjs` instead of listing them | Kept | Closes a failure that had already happened silently, and removes the maintenance step rather than adding one. |
| The `deliver` producer and validator reading one shared written contract and never importing each other | Kept | Author-cannot-certify, applied to code. The separation is the guarantee. |
| A 310-line abstract-interpretation visitor enforcing a subprocess drain rule across four call sites | Disproportionate | The invariant is real; the mechanism is not. A shared bounded-process helper enforces it at the source for the cost of one grep. |
| Pinning git lookup to a fixed directory list and suppressing global git configuration in a local worktree helper | Disproportionate | Fails closed on any git installed elsewhere. Test 2 has no answer: the adversary would be the user's own PATH. |
| A hard per-module line cap that a module satisfied by injecting `globals()` into a helper instead of importing it | Rule beat requirement | The requirement was reviewability; the rule delivered a module dictionary passed at runtime, which is strictly harder to review. Fix the rule. |

The last row is the pattern to watch for hardest. When a constraint is
satisfied by a construction nobody would choose on its merits, the constraint
is what needs changing.

## Applying this to a change

- **Adding a check or gate:** run the proportionality test. State the failure
  it has seen. Prefer extending an existing check to adding a parallel one.
- **Adding a skill:** `MAINTAINING.md` owns this. The relevant clause is that a
  skill retains only rules that change behaviour or prevent observed failures.
- **Hardening existing code:** name the adversary first. If the answer is the
  operator's own environment, stop.
- **Auditing:** run the removal test across the surface, not just the file in
  front of you. A user correction names a class, not an instance.
