## Fixes
### Lanes and settlement

- Read-only lanes accept `--base` as the diff base when `--head` pins the commit. A read-only lane warns when the checkout it reads (the `--worktree` named, or the caller's checkout) is behind the default branch of `origin`, from the last fetched refs or the remote's advertised HEAD (#3002).
