## Fixes

### Lanes and settlement

- A lane whose writer rebased onto another branch now publishes. Publication accepts a tip that descends from the launch base, the origin default branch, the ref the lane launched against or any remote-tracking branch, records the ancestor in the reflog and `publication-ancestor.json`, and warns. A tip that descends from none of them settles `failed` with `LANE_TIP_UNROOTED` instead of staying `running`. (#3334)
- A handoff block repeated back to back is accepted with a warning: identical copies count once and differing copies settle on the last. (#3341)
