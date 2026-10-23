# Fabric MCP transcript judge

This suite exercises the Fabric MCP stdio contract through a small Python
standard-library JSON-RPC client. Shared test support builds the Rust front
door. The client starts the same stable `provenant` command registered by
`scripts/configure-fabric-mcp.py`: the configured seat and state environment are
set, and no arguments make the binary exec `runtime/fabric/bin/fabric-mcp`.

The process runs with a temporary `HOME`, XDG directories, state database and
project directory. Run fixtures are seeded in that project. No provider is
started. `fabric_dispatch` cases stop at local validation and conflict checks;
remote fetch is likewise checked at its unconfigured-host response.

Run the comparison suite with:

```sh
TMPDIR=/tmp .venv/bin/python -m pytest tests/mcp_judge -q
```

Regenerate committed request/response JSONL using the TypeScript server with:

```sh
TMPDIR=/tmp .venv/bin/python -m pytest tests/mcp_judge --record -q
```

To point the same judge at another MCP server, set
`PROVENANT_MCP_SERVER_CMD` to its executable and any arguments, separated using
shell-like quoting. The command receives the isolated environment and project
working directory. The default command is the Rust binary built by test support.

## Coverage

`transcripts/protocol.jsonl` records initialization and `tools/list`, including
every tool name and input schema. `core.scenario.jsonl` drives tool calls and
their expected replies. It covers every listed tool with a callback response and
an error path. `fabric_dispatch` is limited to safe validation errors as
requested. `fabric_fetch` uses a fake SSH peer and seeded writer receipt/bundle;
it never contacts a real host. Stateful steps send a message, claim it from inbox and acknowledge it;
create/update/list a task; acquire/verify/release a work claim and landing lease;
append/read activity; inspect identity and sessions; and read seeded run status,
run listings and output.

Transcripts retain the wire requests and responses. Normalisation replaces the
per-test temporary root, ISO timestamps, UUIDs, Git object hashes, process ids,
run tokens, epoch-millisecond expiries and JSON-RPC ids. Object key ordering is
ignored by JSON comparison; `tools/list` entries are sorted by name because
their order is not part of the contract. Dynamic IDs are captured from earlier
responses and inserted into later requests, so the judge still verifies that
returned IDs work in follow-up calls.

## Adding a scenario

Add one JSON object per line to a `*.scenario.jsonl` file. Each row has a label,
`kind` (`happy` or `error`), and a request with the MCP tool name and arguments.
Use a `capture` map to save a dot-separated response path under a name, then
refer to it later as `{"$ref":"name"}`. Seeded fixture runs are available as
`fixture-run` / `fixture-task`; do not launch providers. Record the expected
wire transcript with `--record`, review the JSONL diff, then run without
`--record` to check it. The coverage test requires both kinds for every tool in
the golden list, with the explicit `fabric_dispatch` validation-only exception
requested above.

The Python test jobs already install the Node workspace before running pytest,
so CI can run this suite as part of the normal Python shard without another
build step or workflow change.
