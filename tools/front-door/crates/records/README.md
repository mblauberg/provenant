# `provenant-records`

Shared durable-record boundaries for the front door:

- bounded, duplicate-safe JSON decoding and strict schema-version-one headers;
- bounded JSONC tokenization with original source spans;
- `CanonicalV1` JSON bytes plus separate record and raw-file SHA-256 digests;
- tagged durable paths;
- Serialize-only `AttemptFact` values constructed by the run owner.
