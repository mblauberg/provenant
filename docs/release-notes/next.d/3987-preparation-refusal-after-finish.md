## Fixes
### Fabric
- A finished task no longer creates an automatic follow-up. Queued steering on a successful attempt stays accepted but resolves as not-continued, so a second task cannot refuse at preparation and hide the successful attempt. (#3987)
- A writer-lineage failure at launch is a typed `preparation_refused` outcome whose note names the retained worktree's owner, source and head to repair, instead of an "invalid run record" telling the operator to reconcile. Status keeps showing the latest successful attempt, its result path and its summary; a preparation refusal that never launched remains visible in its own operation outcome. (#3987)
- A new graph can adopt a retained writer worktree from a finished attempt in an earlier graph when the writer, terminal record, released custody and lane owner match. It inherits the tree and its commits, not the earlier graph's source and head pins. (#3987)
