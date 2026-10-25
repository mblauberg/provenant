# provenant-core

Pure shared values for the [state spec](../../../../docs/specs/state.md).

- Distinct generated identities accept 16 supplied bytes or 32 lowercase hex
  digits. `OperationId` uses execution-run's caller-key grammar instead.
- `GitOid` accepts full SHA-1/SHA-256 hex. Provenant `Digest` requires the full
  `sha256:` spelling and is a distinct type.
- Cooperative `CancelToken` clones share a flag. Separate tokens stay independent.
  `WorkScope` carries typed project, workspace and run identities.
- `WallTime`/`UtcMicros` uses exact integer microseconds with one UTC wire form,
  including six fractional digits. Monotonic samples/deadlines have no serde
  implementation. The production clock belongs to supervisor; `TestClock`
  advances explicitly and commits no partial update on overflow.
- `WaitBudget` carries a monotonic deadline, capped at 14400 seconds for CLI and
  55 seconds for MCP, with a minimum of one second. A budget expiring never
  signals a provider.
- `RelativePath` rejects absolute, dot, parent and NUL components and preserves
  native bytes. Records owns its durable path encoding.
- `RootInputs` captures the flag, environment, home, checkout, selected
  workspaces and old roots. Selection rejects empty/relative explicit inputs.
  `PathResolver` is the real filesystem seam: supervisor must resolve existing
  ancestor aliases and append a normal missing tail without creating anything.
  Core checks both the selected and bound path against protected roots. Owners
  must still use supervisor's safe handles for containment during I/O.

Core reads no environment, filesystem, entropy, clock or process state. Tests
inject those observations; no test discovers the user's home or old state.

## Decided removals

The old root pointers, host slugging, path resolution/ignore-file effects,
Python/V8 emulation, generated Unicode table, `pending` copies and differential
oracle harness are deleted. Their domain owners implement the accepted design;
there are no compatibility adapters. Shared Python root/host leaves remain
until their last importer is removed in state slice 5.

From `tools/front-door`, run `cargo fmt --all`,
`cargo clippy -p provenant-core --all-targets -- -D warnings` and
`cargo test -p provenant-core`.
