## Additions
### Lanes and settlement

- Every settlement now passes one post-settlement seam. A settled lane posts a terse notice (at most 300 bytes: attempt, outcome, `(partial)`, model, staged recovery, head, pull request) to the dispatching chair's inbox and appends a `lane_settled` entry to that chair's session log, once per attempt (#2125, #2217). Lane-dispatched lanes stay quiet. `[lanes.settlement]` switches `notice`, `notice_read_only` and `chair_log`.
- The run store moves to schema v6. A settlement's reactions are recorded in the terminal commit's own transaction, so a crash between commit and reaction loses nothing: crash recovery drains them, a failed reaction retries, and finished rows are pruned with `[retention] logs_days`. Upgrade both hosts together.
