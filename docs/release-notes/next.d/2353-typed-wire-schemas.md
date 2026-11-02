## Changes

- Command results now have published, versioned JSON Schemas (`docs/reference/schemas/`), generated from the same typed projections the CLI and MCP share: one lane row for `status` and `lanes`, typed `output` modes, cancel receipts and the `{code, message, cause, repair}` error envelope. Wire schema version 2 replaces version 1 without a dual-emit period; consumers that match on `schema_version` must accept 2 once commands move to the typed projections. `scripts/generate-reference --check` fails when the published schemas drift.
