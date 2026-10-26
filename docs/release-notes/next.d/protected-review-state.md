## Behaviour changes

- Completed writers with protected changes now end as `review_required`, retain
  their candidate `head` and `protected_paths` in CLI and MCP lane output, and
  await chair review without automatic delivery. The CLI accepts
  `lanes --state review_required`, and the menu bar attention view includes
  these lanes.
- Succeeded writers report their candidate `head` even without delivery.
- Protected-path checks exclude protected edits present at lane start and clean
  protected edits merged from upstream. Unresolvable delivery target refs or
  merge bases fall back to the lane's start head so quarantine can still finish.
