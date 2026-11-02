## Changes

- Published version 2 typed command and MCP JSON Schemas in `docs/reference/schemas/` ahead of migration. The CLI and MCP do not emit these typed projections yet; current CLI `--json` envelopes use version 1. The planned status projection uses lane rows and a string cursor in place of today's raw attempt records and numeric cursor. `scripts/generate-reference --check` detects generated reference drift.
