## Fixes

### CLI and install

`provenant land N` now binds only pull request N. It no longer reads the end
state of other pull requests on the branch or emits "stale binding sweep
failed" contribution-binding warnings naming them. Stale bindings remain
reconciled by `pr open` and `pr contributions --bind`. (#3494)
