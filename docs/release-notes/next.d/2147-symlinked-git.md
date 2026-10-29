## Fixes

- `pr contributions`, the chair summary and log, and the console no longer fail with "invalid pinned git executable" when Git is a symlink, as in Homebrew installations (#2147).
- Host `git log` reads (continuation history, delivery commit summaries and push patches) pass `--no-show-signature`, so a `log.showSignature` setting cannot run `gpg` on the host (#2147).
