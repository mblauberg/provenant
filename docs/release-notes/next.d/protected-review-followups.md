## Fixes

- `lanes` now shows the review line for any `review_required` lane, including
  one with no protected paths (#1747).
- A writer left off its lane branch now fails with "writer HEAD is not the
  lane branch" instead of "writer head changed during quarantine" (#1747).
- When a writer's delivery remote has no `HEAD`, protected-path attribution
  now uses the lane's base branch on that same remote, so protected edits
  merged from it no longer count as the writer's own. It never borrows another
  remote's refs; if neither resolves, the lane start is the baseline and review
  is required (#1747).
