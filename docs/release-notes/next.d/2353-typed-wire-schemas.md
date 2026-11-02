## Changes

- Published JSON Schemas in `docs/reference/schemas/`. The `lanes`, `cancel` and `output` envelope schemas accept the version 1 `--json` envelopes the CLI emits today, including non-ok statuses with `error: null` and the `{operation_id}`-only cancel failure. The `status` schema carries the planned typed `data` (lane rows and a string cursor in place of today's raw attempt records and numeric cursor), and the MCP schemas describe planned structured content. Version 2 is planned and not emitted yet. `scripts/generate-reference --check` detects generated reference drift.
