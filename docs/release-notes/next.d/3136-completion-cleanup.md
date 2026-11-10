## Behaviour changes
### Lanes and settlement

- Settlement now makes one detached attempt to collect a settled lane's worktree. If that attempt misses, nothing is queued: the daily or pressure `provenant gc` sweep collects the worktree. The host tick no longer reports a `completion_cleanup` task, and the completion request, claim and exhaustion files under `<state>/gc/completions`, `gc/completion-locks` and `gc/completion-exhausted` are no longer read. Those directories are inert after upgrade; you can delete them. (#3136)
