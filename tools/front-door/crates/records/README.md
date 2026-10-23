# `provenant-records`

Shared durable-record boundaries for the Rust port:

- bounded, duplicate-safe JSON decoding and strict schema-version-one headers;
- `CanonicalV1` JSON bytes plus separate record and raw-file SHA-256 digests;
- tagged durable paths and a fail-closed YAML subset for skill frontmatter and
  agent sidecars;
- Serialize-only `AttemptFact` values constructed by the run owner.

The fixture inventory in `tests/external_yaml.rs` covers every tracked
`SKILL.md` frontmatter and `agents/*.yaml` sidecar in this repository. Add an
`include_str!` entry when a new tracked file of either kind is added.
