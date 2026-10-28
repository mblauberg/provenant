# Human inbox

Owner: `crates/fabric-runtime`, `crates/cli`, `crates/fabric-mcp`
Anchor: `tests/inbox.rs`, `crates/fabric-runtime/src/inbox/tests.rs`

Use the inbox from a chair or a human-controlled session, with both apps closed.
The records use the same project binding as Fabric. Worker lanes cannot mutate
the inbox. The current surface supports local owners and local Fabric mail.
The complete console specification and remaining delivery scope live in
[#1968](https://github.com/mblauberg/provenant/issues/1968).

## Post and answer

Save request content as JSON. Do not supply project, host, actor, answer or
delivery fields:

```json
{
  "kind": "decision",
  "title": "Choose the repair",
  "body": "The affected check can be repaired within the approved scope.",
  "blocking": true,
  "options": [
    {"id": "repair", "label": "Repair", "description": "Repair the failing check."}
  ],
  "recommended_option_id": "repair"
}
```

```sh
provenant ask --file request.json --session HOST_SESSION_ID --operation-id ask-1 --json
provenant inbox --session HOST_SESSION_ID --json
provenant inbox REQUEST_ID --json
provenant answer REQUEST_ID --action choose --option repair \
  --expect-revision 1 --channel cli --operation-id answer-1 --json
```

Use the explicit originating session ID. It is a declared delivery target;
posting does not discover, register or authenticate a live provider session.
Ownership comes from this host's current Fabric project and inbox actor.
Change the agent label only deliberately: acknowledgement must use the request's
original actor as well as its exact host/project/session.
MCP and the terminal may use different labels by default. Inspect
`fabric_context` (`op: inspect`) in the originating MCP session and retain
`inbox.agent` with its checkpoint. For CLI acknowledgement, set
`PROVENANT_AGENT_LABEL` to that same label for the command; a similar label or
the same session ID alone is insufficient.

Use `kind: question` for information, or `kind: assumption` with a non-empty
`proceeding_with` string for a reversible working position. Only decisions can
set `blocking: true`. Decisions/questions accept `choose` or `reply`; assumptions
accept `confirm` or `override`. `reply` and `override` read exact UTF-8 text via
`--text-file reply.md` or piped `--text-stdin`, capped at 4 KiB. Compose human
replies in a file; piped input must finish, including EOF, within five seconds.
The two text sources are mutually exclusive.
`confirm` records acceptance of the saved `proceeding_with` value
and takes no option or text. Record `chat`, `console`, `phone_remote`, `cli` or
`unknown` explicitly; phone use is never inferred.

Every successful mutation returns an operation ID. Preserve the ID, input and
viewed revision when retrying after an uncertain reply. A changed operation or
actor conflicts. For a new answer after a conflict, inspect the item first.
An item cannot be edited into a new choice; close it as superseded, then post a
new request. Historical answers remain visible. See generated
[CLI](../reference/cli.md) and [MCP](../reference/mcp-tools.md) references for syntax.

## Consume once

An answer atomically queues a `human_answer` Fabric response to the original
actor with the host, session, request and immutable answer IDs. This is ordinary
mail, not an interrupt or live hook injection. Consume it at an existing
orchestration boundary or explicit continuation, check its exact target and
deduplicate by answer ID. Never spend model turns polling.

After consuming an answer, acknowledge it before acting:

```sh
provenant inbox acknowledge REQUEST_ID --answer ANSWER_ID \
  --session HOST_SESSION_ID --operation-id ack-1 --json
```

Reading or marking Fabric mail read does not acknowledge it. For this transport,
an explicit consumption acknowledgement can move `queued` directly to
`acknowledged`; no live offer was observed. Delivery state is independent of
resolution. Repeated acknowledgement is harmless and never repeats an action.
Neither the answer nor acknowledgement grants new authority or replaces review.

Use `inbox close` with a reason and expected revision to withdraw, supersede or
expire an item. Closure grants no approval and does not discard its answer or
pending delivery. Expiry is an explicit operation; no timer infers it.

## Limits and recovery

Pages default to 50 and cap at 100. Pass `next_cursor` back as `--cursor` with
the same filters/page size. A request change invalidates the cursor; restart
without it. `--host` accepts the exact local host ID only. A remote configured
Fabric home returns `unsupported_owner`, even if offline; no local mirror is
created. To operate remotely, control a session on the owning host directly.

Request/log retention and migration behaviour are documented by the
[store owner](../../tools/front-door/crates/fabric-store/README.md). No request
history or pending delivery is automatically pruned. Back up the private Fabric
store with its existing owner procedure before upgrading; older binaries refuse
the added schema. Do not lower the schema version manually.
