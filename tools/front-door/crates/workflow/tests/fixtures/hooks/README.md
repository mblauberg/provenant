# Native hook wire fixtures

Recorded from upstream native contract examples on 2026-10-24, without
launching providers or reading local harness sessions:

- Claude JSON: verbatim SessionStart and PreCompact input examples in
  <https://code.claude.com/docs/en/hooks.md>. The resumed-session example
  includes host-owned optional fields; workflow ignores those fields.
- Codex PreCompact: the JSON asserted by
  `pre_compact_input_includes_lifecycle_metadata` in
  <https://github.com/openai/codex/blob/main/codex-rs/hooks/src/events/compact.rs>.
- Codex SessionStart: the same native fixture identity and model, serialised
  with the fields and null transcript defined by `SessionStartCommandInput::new`
  in <https://github.com/openai/codex/blob/main/codex-rs/hooks/src/schema.rs>.
  This fixture records the native serializer contract, not a live invocation.

SessionStart output uses `hookSpecificOutput`, `hookEventName` and
`additionalContext`, as defined by both hosts. PreCompact returns advisory
`systemMessage` only, with no blocking decision. Claude discards that field;
the composition edge can also report `HookContext::diagnostic` to stderr.
