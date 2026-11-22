## Behaviour changes

### Git and safety

- A nested lane deeper than `domains.run.max_lane_depth` now warns and proceeds, naming the setting; `policy.LANE_DEPTH=refuse` restores the stop (#3551)
- Requesting hooks when no selected harness supports them now warns and skips hooks instead of failing the install; `policy.INSTALL_HOOKS_UNAVAILABLE=refuse` restores the stop (#3551)
- A PR opened over an attempt in an unexpected state now warns, naming the attempt and its state, and proceeds from the recorded branch and head (#3551)
- A hook worktree branch name that is not a valid ref is sanitised with a warning instead of refused (#3551)
- Policy rules now name their class precisely: immutable typed operation errors (identity, consistency, malformed input) are `operation_error`, no longer labelled as boundaries, and git-isolation rules are `other_work`; only credentials, other work and authority remain boundaries (#3551)
- Refusal messages for PR landing, publish and worktree removal now say what to do next (#3551)
