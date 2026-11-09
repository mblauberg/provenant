## Fixes
### Lanes and settlement

- Writers that declare `stopped_early` now continue automatically on the same worktree, including graph nodes. Each continuation starts fresh with the handoff and original brief by default; `lane.resume_continuations` opts a model into retained-session resume. The default allows two continuations per run; model and project policy can override it or disable it with zero (#3086).
