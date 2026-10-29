## Breaking changes
### Git and safety

- **The Claude Bash hook now refuses only irreversible actions.** It blocks a force push to the default branch and recursive `rm` of your home directory, the repository root or the Provenant state directory, and everything else, including `git reset --hard` and `--no-verify`, now runs. The `[domains.guard] deny` rule ids are now `force-push`, `rm-root` and `rm-state`, so update any `deny` list that uses the old ones (#1949).
