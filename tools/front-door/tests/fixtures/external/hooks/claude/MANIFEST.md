# Claude hook recordings

Tool: Claude Code 2.1.310. Date: 2026-10-24. Platform: macOS.
Captured from real PreToolUse, manual PreCompact and SessionStart(compact)
events in a disposable project, with project-only settings and a tee hook.
The host result recordings demonstrate denial for JSON/exit 0 and stderr/exit 2.
No provider was run by this build lane. `post-tool-use.json` is derived from the
PreToolUse capture: its event name is changed to PostToolUse and a tool_response
is added; all other field names, types and values are retained. It is not a
separate host capture.

Redactions: session_id, prompt_id, tool_use_id and result uuid use stable
placeholders; transcript_path, cwd and the fallback hook path use /recorded.
Field names, types, event names and observed host behaviour are retained.
JSON is re-encoded as compact UTF-8 plus LF after redaction. Digests are SHA-256
of the committed bytes. Raw captures are never copied into the repository.

| File | Direction | Byte digest |
| --- | --- | --- |
| `post-tool-use.json` | derived host -> hook stdin | `19febed09d749ba6a68020c3dc21453cda68f830765ac9424dfbb6bdceb4aaa3` |
| `pre-compact.json` | host -> hook stdin | `447a01f386efd27231e49e80d30ed0292353f734c1dac7699af4302075579af7` |
| `compact.json` | host -> hook stdin | `973d21fd60a06b30dd9c25f08da8b91547033bdc625d567b5e94f389149afc5f` |
| `host-deny-json.json` | host -> caller result after hook response | `acca3e0e34269d5c528f5b533ac6a5ba19d467ee395a3fa2b98e55470c9e982f` |
| `host-deny-exit2.json` | host -> caller result after hook response | `387cd98c00da0ec8271bd8a80309d8dbe08ca646d41fc8d1c70ae98e4b1e7670` |
