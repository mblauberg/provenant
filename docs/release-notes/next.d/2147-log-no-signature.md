## Fixes

- Host `git log` reads (continuation history, delivery commit summaries and push patches) pass `--no-show-signature`, so a `log.showSignature` setting cannot run `gpg` on the host.
