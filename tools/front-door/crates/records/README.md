# `provenant-records`

Shared durable-record boundaries for the front door:

- bounded, duplicate-safe JSON decoding and checked schema-version-one headers;
- bounded JSONC tokenization with original source spans;
- `CanonicalV1` JSON bytes plus separate record and raw-file SHA-256 digests;
- tagged durable paths;
- Serialize-only `AttemptFact` values constructed by the run owner.
Stored records accept additional fields from newer writers. Records included
in canonical digests retain those fields in flattened `ExtraFields` maps,
including nested paths, routes and request inputs. Unsupported schema versions
remain distinct from malformed data. User-authored policies remain strict.
