## New

### Lanes

- A bundled `light` preset (`attempt.preset = "light"`) makes a lane lighter for read-only or scratch work: shared isolation for unpinned readers, minimal supervision and no delivery receipt. It is node configuration, so it composes with every template and `provenant run --set`. `attempt.isolation` (`private` or `shared`), `attempt.supervision` (`standard` or `minimal`) and `attempt.receipts` (`delivery` or `none`) set each part alone, and match the new `lanes.isolation`, `lanes.supervision` and `lanes.receipts` knobs. Writers keep one writer per worktree, and secret redaction and object integrity stay on in every preset.
