# Agent Fabric review portal supervisor

This dependency-free Rust crate owns the narrow native boundary required by
[review-bundle and portal custody](../../docs/specs/agent-fabric/review-custody.md)
and Console [artifact review and attention](../../docs/specs/console/artifact-review.md):

- the binary accepts only `portal-stdio-v1` and exactly
  `AGENT_FABRIC_REVIEW_SOCKET`, `AGENT_FABRIC_REVIEW_ACTION` and
  `AGENT_FABRIC_REVIEW_CONTRACT`;
- portal mode requires every inherited descriptor above stderr to be absent, connects to one
  AF_UNIX broker once, and relays bounded LF frames without interpreting JSON,
  UTF-8 or MCP;
- supervisor helpers mark FD 3 `CLOEXEC`, bind local peer credentials to
  PID/start/PGID/session identity plus Darwin audit-token PID generation, verify bounded ancestry,
  and perform polled TERM -> 250 ms -> KILL -> bounded WNOHANG reap cleanup only for an exact
  isolated group while retaining trigger/outcome evidence;
- custody helpers walk and retain no-follow directory descriptors, require a separately persisted
  same-filesystem private claim-directory identity outside the raced source namespace, and advance
  an exact owner/digest/single-link-matched entry by one caller-persisted phase per call using
  atomic rename, post-claim revalidation, directory `fsync` barriers and `unlinkat`;
- the caller durably stores `canonical | claimed | removed | integrity-failure`, persists `claimed`
  between rename and unlink, and never infers progress from missing paths. A phase/presence mismatch
  fails closed; exact claim-only presence recovers a crash-after-rename, while only durable
  `claimed` may confirm both paths absent as `removed`;
- the boundary recomputes and equality-checks the persisted
  `agent-fabric-custody-claim-v1` basename before mutation. It fsyncs canonical plus claim
  directories before returning `claimed`, and the claim directory before returning `removed`.

TypeScript remains the sole JSON-RPC/MCP parser, policy owner, ledger/journal
owner, cleanup-phase/CAS owner and one-use broker. The portal binary never invokes path cleanup or
receives a bearer capability; only the daemon may call the native custody boundary from persisted
row inputs.

## Fail-closed integration status

This crate alone does **not** prove `certifying-review-packet-only.v1`. Keep
every helper-backed certifying route `capability=false`; in particular, `setsid` escape prevention
and outer OS confinement remain explicitly false until daemon integration proves all of the
following on the activated build:

- action/contract/bundle equality and one-use broker admission using the peer
  identity returned here;
- pinned executable bytes, Darwin code identity, complete ancestry and outer
  OS confinement;
- control EOF, deadline, cancellation, provider exit and daemon/supervisor
  death wiring to the cleanup owner;
- durable persistence and comparison of the expected custody identity/digest/singleton link count
  passed to this crate's no-follow removal boundary, plus its v1 claim basename, phase and the distinct
  same-device private claim-directory path/device/inode kept outside provider mutation authority;
- current-build source/auth/tool/network denial plus `setsid`, double-fork,
  daemonisation and reparent escape canaries.

Run the crate gate with:

```sh
cargo fmt --check
test "$(awk '/^\[\[package\]\]/{count++} END{print count+0}' Cargo.lock)" -eq 1
cargo metadata --locked --offline --no-deps --format-version 1 >/dev/null
cargo clippy --locked --offline --all-targets -- -D warnings
cargo test --locked --offline
```
