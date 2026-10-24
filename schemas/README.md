# Record contracts

Rust typed records in `tools/front-door/crates/records` and each owning crate
replace the legacy Python JSON schemas and synthetic fixtures. Test current
records at their CLI, provider, MCP and persistence boundaries. External wire
fixtures remain under `tools/front-door/tests/fixtures` and crate test folders.

The old record-schema validator and its Python/TypeScript oracle are retired.
