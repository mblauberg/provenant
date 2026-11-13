# Reviewer context

Every lens reviewer receives the same context, in a template composite or a manual
council.

- You have read access to the whole repository. The diff stat and per-file
  slices are navigation aids, not the scope of the review.
- Read the callers of changed code, the existing utilities it could have
  reused, and the surrounding architecture. A review that sees only the diff
  cannot judge reuse, conventions or design fit.
- Review against the spec and the base named in your task: the spec is the
  issue or file with the acceptance criteria, and the base is the diff
  reference the change is measured from.
- Read the pinned subject commit named in your task, not the working tree. Use
  `git show <commit>:<path>` and `git diff <base> <commit>`. Do not edit files
  or run Git write commands.
- If you cannot read required context, say `inconclusive` and name the path and
  the coverage gap. "No findings" is a valid result when coverage is complete.

## What blocks

This is the only definition of blocking. A finding blocks when it shows one of:

- a failed acceptance criterion, which includes an undisclosed divergence from
  the spec and a write outside the write scope;
- a defect on a reachable default path;
- a breach of a real boundary: credentials, Git isolation, data integrity,
  authority, or one the repository declares;
- data loss.

A default path is what ordinary use reaches: normal input, the scenarios the
spec names, and ordinary failures such as a crash, a restart, a timeout or an
interleaving the design permits. Untrusted input the system is built to accept
is ordinary use, so an exploit through it is a default-path defect. Assume
tooling inside the host boundary cooperates: a peer tool that forges the
host's own state or ignores its locks is not a trigger.

Should-fix is a concrete cost on a default path that does not meet that test,
such as a broken repository convention. Everything else is a note, three at
most: a finding with no trigger, and anything off the default path that
neither breaches a boundary nor loses data. Use `blocking`, `should-fix` or
`note` as the finding's `severity`. Notes are reported, not repaired.

## What a finding states

Each finding names the location, the mechanism and the fix. Claims that
something is missing or unused cite the search that shows it. Give each
finding a `trigger` with:

- `sequence`: the real steps that reach the state;
- `reach`: `default_path`, `operator_mistake` or `attacker_or_unreachable`;
- `observed`: `true` if you saw it happen in a run or its output, `false` if
  you inferred it from the code.

`proposed_fix` removes the cause, because a patch at the symptom leaves the
next case open. A proposed guard says whether a design would make the state
unreachable, and proposes that design first. Do not ask for a guard against an
unobserved risk outside the hard boundaries; a guard or refusal there that
blocks ordinary work is itself a defect. When findings share a flaw in the
design, such as a list that gains a member each round, set
`design_defect: true` so the host escalates to principal design instead of
another patch.

## What not to report

Do not report style that lint covers, an abstraction no defect needs, tests for
states the types make impossible, demand rigid tests or example snapshots where
invariants suffice, or report work beyond the spec. A disclosed,
evidenced divergence that serves the stated outcome and stays in write scope is
a note, not scope growth. So is a request to restore a format the spec retires:
that is a spec conflict, not a defect.
