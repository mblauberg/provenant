## Breaking changes

### CLI and install

- Before the first 2.0 install, wind down every active lane. Install creates a
  fresh v2 state root, copies only `config.toml` and `host-id`, and does not
  migrate 1.x run state or read the old install receipt. It replaces or removes
  only Provenant artifact names declared by 2.0. After install, manually review
  the old v1 root and client configuration for anything you still need or for
  1.x entries outside those declared names; remove leftover state or entries
  yourself when they are no longer needed. The old harness and account
  selection from the receipt is not transferred: use autodetection or pass
  `--harness` again. A fresh custom `--state-dir` remains supported; an
  identified 1.x root must use the default v2 handoff or point at a fresh
  custom root and configure it manually. See the
  [state spec](../../specs/state.md#the-first-install-uses-a-fresh-root)
  and [install runbook](../../runbooks/install.md). (#3078)
