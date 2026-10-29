## Additions
### Lanes and settlement

- Every settlement now passes one post-settlement seam. A settled lane posts a terse notice (at most 300 bytes: attempt, outcome, `(partial)`, model, staged recovery, head, pull request) to the dispatching chair's inbox and appends a `lane_settled` entry to that chair's session log, once per attempt (#2125, #2217). Lane-dispatched lanes stay quiet. `[lanes.settlement]` switches `notice`, `notice_read_only` and `chair_log`.
