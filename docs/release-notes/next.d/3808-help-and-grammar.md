## Behaviour changes
### CLI and install

- `provenant help` resolves nested command paths and, refuses an unknown component with an error that lists the valid commands at that level (words after a command that takes arguments, as in `help run implement`, stay that command's arguments); `help --all` lists the global options once at the top instead of repeating them in every usage block. `config path [DIR]` takes an optional directory in place of the ignored `--project` flag, and the `pr` and `worktree create --base` help now describes what they do. (#3808 #3814)
