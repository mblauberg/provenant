# ADR 0028: A small allowlist of major Rust dependencies

**Status:** Accepted

Changed: 2026-10-26, #1754.

## Context

Moving all runtime owners to Rust requires reliable parsing, SQLite, hashing,
time handling and argument decoding. Writing those primitives from scratch
would trade a short dependency list for a larger correctness burden.

## Decision

The surviving decision numbers stay stable for inbound citations.

### 3. Dependency allowlist

Use the small direct-dependency allowlist in the
[workspace manifest](../../tools/front-door/Cargo.toml). The manifest and
lockfile own names, features and versions; this ADR does not duplicate them.

Prefer established crates for typed serialisation, bundled SQLite,
cryptographic digests, regular expressions, time zones, CLI parsing, formatted
TOML edits and syscall bindings. Temporary fixture directories are test-only.

### 4. In-house primitives

Keep bounded HTTP calls on the system `curl` transport with secrets off argv.
Fabric has one small stdio JSON-RPC transport, so it needs neither an MCP SDK
nor an async runtime. Use threads, typed library errors and small edge
diagnostics rather than adding generic runtime and logging frameworks.

### 5. External YAML

Externally owned YAML formats use the records crate's fail-closed subset
reader. Unsupported constructs are errors with locations, not permissive
coercions. No Python parser or legacy-language parity oracle remains.

### 6. Dependency admission

New direct crates, feature expansion and material transitive additions require
an explicit reason: widely used, maintained, MIT or Apache-2.0 licensed, and
not cheaply replaceable in-house.

## Consequences

Bundled SQLite adds clean-build cost in exchange for one database version.
A narrow dependency set limits audit and update work; bespoke protocol and
subset readers carry focused conformance tests. This is a dependency policy,
not a migration rollout plan.
