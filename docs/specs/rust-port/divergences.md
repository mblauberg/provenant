# Differential divergence log

Version: 1. No whole-project old/new differential run has been performed by
this documentation lane. An empty log is not parity evidence.

For each mismatch, append a stable ID and the fields below. Keep fixed entries
as evidence; supersede them by reference rather than erasing them. Fixtures
must redact secrets and bind only named volatile fields. Link retained run
artefacts and CI evidence at the exact candidate and oracle revisions.

| Field | Required content |
|---|---|
| ID / unit | `RP-<number>`, graph node ID, rulebook version |
| Class | `bug` (Rust defect), `library` (dependency semantics), `python-bug` (old implementation defect, including TS), or `improvement` (intentional new behaviour) |
| Reproducer | Command/MCP transcript, fixture and normaliser revision; platform, config digest and isolated roots |
| Compared revisions | Pinned old revision/binary digest, new head/binary digest and judge revision |
| Expected / actual | Exact differing bytes or schema/value/effect paths, including exit and stderr |
| Evidence | Old-pass/mutant-fail judge proof and differential run artefacts |
| Disposition | Open, fixed (commit and rerun), or approved exception (approver, authority, scope and surviving gate) |
| Follow-up | Regression case and any rulebook/generator amendment |

`bug` and `library` stay blocking until fixed or explicitly approved as a
boundary change. `python-bug` and `improvement` need user approval before the
new behaviour becomes the contract. No lane can approve its own exception.

Existing front-door exceptions D1–D3 are already recorded in
[ADR 0027](../../adr/0027-rust-front-door.md#intentional-divergences-from-the-python-front-door).
They form the current baseline; they are not newly discovered port mismatches.

## RP-1001 — host identity JSON input (partial unit)

- Unit: `scripts/host_identity.py#unit-01`; rulebook v2 with the inert-crate
  judge-gate override from rulebook v3 / PR #1294.
- Class: `library`. Source/oracle revision:
  `e897b10d58b27a46544969594eed08a5ed64c72c`.
- Reproducer: in `tools/front-door`, run
  `cargo test -j 2 -p provenant-core python_extended_host_json -- --ignored`.
  Uses isolated temporary hosts files and the repository's Python module.
- Expected/actual: Python accepts UTF-8 BOM, UTF-16/32 byte encodings, nonfinite
  numbers, and lone surrogate escapes in otherwise unused members. The current
  UTF-8/serde reader rejects these and falls back to the hostname slug.
- Evidence: executable Python differential reproducer retained as an ignored,
  failing parity test. Ordinary host policy and duplicate-member fixtures pass.
  No independent old-pass/mutant-fail judge evidence is claimed.
- Disposition: open; this unit is partial and must not cut over. Slugs, name
  validation, strict duplicate detection, fallback, aliases and peer exclusion
  are ported. A compatibility representation/input decoder remains; no
  feature expansion or new dependency is authorised by this entry.

## RP-1002 — JSONC value representation and diagnostics (partial unit)

- Unit: `scripts/lib/jsonc.py#unit-01`; same rulebook and oracle as RP-1001.
- Class: `library`.
- Reproducer: `cargo test -j 2 -p provenant-core python_extended_jsonc_values
  -- --ignored` in `tools/front-door`.
- Expected/actual: Python accepts `NaN`, infinities, arbitrary-size integers
  (subject to its interpreter limit), overflow-to-infinity floats, and lone
  surrogate escapes. The current serde value model rejects or rounds these.
  Generic syntax errors also retain serde diagnostics rather than Python's
  message, character offset, line and column. Unterminated block comments
  already carry Python-compatible character positions.
- Evidence: ignored differential test demonstrates the value acceptance gap;
  active differential fixtures cover comments, escaped strings, duplicate
  members, malformed syntax and trailing commas. Judge evidence is deferred.
- Disposition: open; partial syntax port only. Value representation and edge
  error rendering remain. No changed contract is approved.

## RP-1003 — product pointer JSON/path encoding (partial unit)

- Unit: `scripts/lib/product_root_resolver.py#unit-01`; same rulebook and oracle
  as RP-1001.
- Class: `library`.
- Reproducer: `cargo test -j 2 -p provenant-core python_extended_pointer_json_and_non_utf8_paths
  -- --ignored` in `tools/front-door` on Unix. The fixture creates a product
  directory with byte `0xff` in its name when the filesystem permits it; the
  numeric fixture is independent of filename support.
- Expected/actual: Python writes the path using surrogateescape and JSON ASCII
  escapes, then reads it back. Rust currently returns `PointerError::Encoding`
  for that path and cannot read lone surrogate escapes. Nonfinite/overflow
  numbers or lone surrogates in unused JSON members likewise cause Rust to
  reject a pointer Python accepts.
- Evidence: the numeric differential fails; non-UTF-8 filename creation was
  refused by the local filesystem and that subcase is unverified locally.
  Active byte comparison covers Unicode
  paths. Policy tests cover stale pointers, invalid versions/types, containment,
  linked-directory refusals, linked-file replacement, private file modes and
  concurrent publication. Judge evidence is deferred.
- Disposition: open; pointer loading/writing for ordinary UTF-8 JSON paths is
  ported, including atomic publication. Non-UTF-8 record encoding and the shared
  JSON adaptation remain. The private `pending::atomic_write` belongs in
  `provenant-supervisor` once its safe publication API exists on main.
