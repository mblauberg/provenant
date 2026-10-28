## Fixes

- `pr contributions`, chair summary and log, and the console no longer fail with "invalid pinned git executable" on hosts where Git is a symlink, such as Homebrew installations. The pin resolves the link and checks the real executable.
