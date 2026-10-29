## Breaking changes
### Git and safety

- **The managed Claude Bash hook refuses only irreversible actions:** a force push to the default branch, and recursive `rm` of the home directory, the repository root or the Provenant state directory. Each refusal names the alternative. `kill`, `pkill`, `sleep`, `git stash`, `git reset --hard`, `git clean -f`, `--no-verify` and branch changes in the primary checkout now run. The `[domains.guard] deny` rule ids are now `force-push`, `rm-root` and `rm-state`, and the old ids are rejected, so update any `deny` list that uses them (#1949).
