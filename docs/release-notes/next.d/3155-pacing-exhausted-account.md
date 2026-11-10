## Changes
### Routing

- Route pacing now treats an account recorded `exhausted` as standby (score 0) even when the requested model's windows have not been probed, so a spent account no longer outranks ready ones. When launch preparation refuses the picked account because its allowance is spent, the launch moves to the next eligible account for the same model, at most once per account, and settles `usage_limited` naming each refused account only when none remain. A pinned account is never swapped. (#3155)
