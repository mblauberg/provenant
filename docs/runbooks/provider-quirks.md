# Provider quirks

Only the `claude` and `codex` adapters are enabled for execution in
[`activation.json`](../../tools/front-door/tests/fixtures/providers/activation.json).
The implemented `agy`, `cursor` and `opencode` adapters are disabled. There are
no Kiro, Copilot or Pi adapters. Use `provenant route list` to inspect the
catalogue and current availability; a catalogue entry does not enable an
adapter.

## OpenCode (disabled)

The adapter code prepares this invocation if OpenCode is enabled in a future
activation record:

```text
opencode run --standalone --format json --log-level error [-s SESSION] -m MODEL[#effort]
```

It sends the prompt on stdin, uses private attempt state as `HOME`, and does not
read the user's OpenCode configuration or authentication. Only keyless models
can answer. Inline configuration sets an empty `mcp` object and restricts
editing, shell and web access according to the access mode. The adapter sets
`OPENCODE_DISABLE_AUTOUPDATE`, `OPENCODE_DISABLE_PROJECT_CONFIG`,
`OPENCODE_DISABLE_CLAUDE_CODE` and `OPENCODE_DISABLE_FILEWATCHER`.

## General guidance

Use the activation fixture for execution status and the Rust provider source
for launch behaviour. A route listing or an installed provider CLI does not
mean that dispatch is enabled.
