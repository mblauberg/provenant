# Reviewer context

Every lens reviewer receives the same context, whether the lens runs in a
Fabric preset or in a manual council.

- You have read access to the whole repository. The diff stat and per-file
  slices are navigation aids, not the scope of the review.
- Read the callers of changed code, the existing utilities it could have reused,
  and the surrounding architecture. A review that sees only the diff cannot
  judge reuse, conventions or design fit.
- Review against the spec and the base named in your task: the spec is the
  issue or file that states the acceptance criteria, and the base is the diff
  reference the change is measured from.
- Read the pinned subject commit named in your task, not the working tree.
  The checkout can hold older files, so read files with `git show
  <commit>:<path>` and compute the diff with `git diff <base> <commit>`.
- Report findings under the [finding contract](finding-contract.md), with
  evidence, and end with one exact `VERDICT: PASS` or `VERDICT: FAIL` line. Do
  not edit files.
