# Claude hook recordings

Tool: Claude Code 2.1.310. Date: 2026-10-24. Platform: macOS.
Captured from real PreToolUse, manual PreCompact and SessionStart(compact)
events in a disposable project, with project-only settings and a tee hook.
The host result recordings demonstrate denial for JSON/exit 0 and stderr/exit 2.
No provider was run by this build lane.

Redactions: session_id, prompt_id, tool_use_id and result uuid use stable
placeholders; transcript_path, cwd and the fallback hook path use /recorded.
Field names, types, event names and observed host behaviour are retained.
JSON is re-encoded as compact UTF-8 plus LF after redaction. Digests are SHA-256
of the committed bytes. Raw captures are never copied into the repository.

| File | Direction | Byte digest |
| --- | --- | --- |
| `pre-tool-use.json` | host -> hook stdin | `6147ed15ada7009317fc10116a1b9d05a17fa20f677c334d8522146ea7d56c89` |
| `pre-compact.json` | host -> hook stdin | `447a01f386efd27231e49e80d30ed0292353f734c1dac7699af4302075579af7` |
| `compact.json` | host -> hook stdin | `973d21fd60a06b30dd9c25f08da8b91547033bdc625d567b5e94f389149afc5f` |
| `host-deny-json.json` | host -> caller result after hook response | `acca3e0e34269d5c528f5b533ac6a5ba19d467ee395a3fa2b98e55470c9e982f` |
| `host-deny-exit2.json` | host -> caller result after hook response | `387cd98c00da0ec8271bd8a80309d8dbe08ca646d41fc8d1c70ae98e4b1e7670` |
