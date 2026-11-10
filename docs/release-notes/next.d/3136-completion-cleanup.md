## Behaviour changes
### Lanes and settlement

- Settlement now makes one detached attempt to collect a settled lane's worktree. If that attempt misses, nothing is queued: the next applied daily or pressure `provenant gc` sweep runs the same owner-bound collection for it, including a lane landed by fast-forward, and records its receipt as `pruned`. The host tick no longer reports a `completion_cleanup` task, and the completion request, claim and exhaustion files under `<state>/gc/completions`, `gc/completion-locks` and `gc/completion-exhausted` are no longer read. Those directories are inert after upgrade; you can delete them. (#3136)
