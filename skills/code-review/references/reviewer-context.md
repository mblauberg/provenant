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
- Report only findings that name a location, a failing scenario, the mechanism
  and the smallest fix. Claims that something is missing cite the search that
  shows it. Class each finding blocking, should-fix or note (three notes at
  most). Do not report style that lint covers, new abstractions, tests for
  states the types make impossible, or scope growth.
- If you cannot read required context, say `inconclusive` and name the path and
  the coverage gap. "No findings" is a valid result when coverage is complete.
- End with a fenced `findings` block before the final non-empty line:
  `VERDICT: PASS`, `VERDICT: FAIL` or `VERDICT: INCONCLUSIVE`. A closed
  handoff fence may follow. The block uses schema version 1 and a JSON `findings`
  array, with at most 32 entries. Keep each summary to one line. Each finding has `id`, `severity`, `file`,
  `line`, `summary` and `status`; use an empty array when there are no findings.
