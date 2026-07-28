## Repository process

### Tracker

- Choice: `<github-issues|tracker-name|none>`
- Pointer: `<tracker-url-or-none>`

### Docs layout

- Pointer: `<docs-index-or-home-list>`

### Specifications

- Owns: `<non-derivable-intent|intent-and-structure>`
- Drift gate: `<gate-command-or-none>`

Default is `non-derivable-intent`: requirements, must-never rules, ordering
constraints and failure semantics. Code and schemas own what exists, tests own
that it behaves, and the tracker owns delivery state.

Choose `intent-and-structure` only with a drift gate named above. A second copy
of a structure its owner also defines will diverge, and an ungated copy diverges
silently — the reader cannot tell which of the two is current.

### Merge policy

- Pointer: `<merge-policy-and-authority-path>`

### Work-item runbook

- Pointer: `<work-item-runbook-path-or-none>`
