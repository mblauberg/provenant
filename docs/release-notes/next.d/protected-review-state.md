## Fixes

- Completed writers with protected changes now end as `review_required`, retain
  their candidate `head` and `protected_paths` in CLI and MCP lane output, and
  await chair review without automatic delivery. The CLI accepts
  `lanes --state review_required`.
- Protected-path checks compare committed changes with the merge base of the
  candidate and delivery target, so protected edits merged from upstream are
  not attributed to the writer.
