## Fixes

### CLI and install

- A read-only launch now selects the checked-out branch's fetched upstream, falling back to the remote default with a warning when no tracking configuration resolves, and names the tree it actually reads: an upstream pin reads the fetched commit while an unpinned launch reads the local checkout (#3714)
- `pr status` and `wait --pr` now embed the shared per-job CI digest in human and JSON output; compact CI output keeps non-test errors with their job, failed step and classification; and aggregate-gate cancellation is derived from job conclusions rather than a dependency field the jobs API does not return (#3647)
- The configuration completeness probe records every declared field even when an earlier value cannot be constructed, so a setting added after an unprobeable field can no longer hide from metadata or the registry; stranded-work reports now include interrupted merges and rebases with edits, dirty detached worktrees and changed lines beginning with `++` or `--` (#3691 #3650)
