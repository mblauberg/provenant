# provenant-core

Pure shared values for the [state spec](../../../../docs/specs/state.md).

- Distinct generated identities accept 16 supplied bytes or 32 lowercase hex
  digits. `OperationId` uses the caller-key grammar in the [run spec](../../../../docs/specs/run.md) instead.
- `GitOid` accepts full SHA-1/SHA-256 hex. Provenant `Digest` requires the full
  `sha256:` spelling and is a distinct type.
- Cooperative `CancelToken` clones share a flag. Separate tokens stay independent.
  `WorkScope` carries typed project, workspace and run identities.
- `WallTime`/`UtcMicros` uses exact integer microseconds with one UTC wire form,
  including six fractional digits. Monotonic samples/deadlines have no serde
  implementation. The production clock belongs to supervisor; `TestClock`
  advances explicitly and commits no partial update on overflow.
- `WaitBudget` carries a monotonic deadline, capped at 14400 seconds for CLI and
  55 seconds for MCP, with a minimum of one second. `WaitBudget::up_to` builds
  an internal bound with no minimum, such as a store lock's busy timeout. A
  budget expiring never signals a provider. `Pause` is the cancellable sleep
  that genuine wait loops take, and `ThreadPause` parks the thread in short
  slices.
- `RelativePath` rejects absolute, dot, parent and NUL components and preserves
  native bytes. Records owns its durable path encoding.
- `RootInputs` captures the flag, environment, home, checkout, selected
  workspaces and old roots. Selection rejects empty/relative explicit inputs.
  `PathResolver` is the real filesystem seam: supervisor must resolve existing
  ancestor aliases and append a normal missing tail without creating anything.
  Core checks both the selected and bound path against protected roots. Owners
  must still use OS crate's safe handles for containment during I/O.

Core reads no environment, filesystem, entropy, clock or process state. Tests
inject those observations; no test discovers the user's home or old state.

From `tools/front-door`, run `cargo fmt --all`,
`cargo clippy -p provenant-core --all-targets -- -D warnings` and
`cargo test -p provenant-core`.
