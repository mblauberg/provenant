## Behaviour changes
### CLI and install

- `provenant help` resolves nested command paths and, when a path stops at an unknown component, shows the help of the deepest matched command with its valid children; `help --all` lists the global options once at the top instead of repeating them in every usage block. `config path [DIR]` takes an optional directory in place of the ignored `--project` flag, and the `pr` and `worktree create --base` help now describes what they do. (#3808 #3814)
