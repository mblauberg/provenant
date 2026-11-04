## Fixes
### Lanes and settlement

- Claude and agy lanes no longer fail at launch with `credential root requires a selected fixture-backed mechanism` when user-skill discovery is on. A discovered skill folder, linked skill resource or other optional grant that falls inside a credential root is left out and named in a warning in the attempt record, and the lane launches. Skill omissions are listed under `user_skills.warnings`; omissions from toolchain or provider configuration are listed under `discovery_warnings`. A required grant inside a credential root still refuses, and the refusal now names the path and the guard that matched (#2810).
