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

## Work links

The request JSON for CLI `ask --file` and MCP `fabric_inbox` `ask` accepts
optional `work`, containing required `run_id` and optional `task_id`,
`attempt_id`, `repository` and `head`. Repository uses the existing absolute-path
wire format, for example
`{"relative_to_root":{"encoding":"utf8","path":"repo/.git"}}` for `/repo/.git`.
Supply the canonical Git common directory, shared by linked worktrees. Head is
an exact 40- or 64-character Git object ID observed at launch.

Creation validates these claims through Run's host-sealed membership lookup for
the current project. A lookup result of `mismatch` is refused. The saved
`work_coverage` reports the lookup result: `verified`, `partial`, `unindexed`,
`repository_unavailable`, `head_unavailable` or `limit`. It is omitted when no work
was supplied. Run-only selection is at best partial. Observed contradictions are refused,
including run-only and capped lookups. A limit without an observed contradiction
leaves remaining candidates unknown. Nested, historical and unprepared
work may be unindexed. Neither case establishes membership or grants authority.
No Git probes, child-store traversal or later promotion of coverage occurs.
Item, history and list JSON preserve the links and creation-time coverage;
callers cannot supply coverage. `request.work` always contains claims; only
`verified` coverage permits a work link or task-label join. Other results must be
labelled unverified and must not become navigable work. Use the records owner's
`verified_work()` accessor and the binding [Fabric contract](../specs/fabric.md#human-requests).
Task labels remain a future overview concern.

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

## Delivery service

The local `offer_until_ack` API is available to delivery adapters. Adapters must resolve the target using authoritative session discovery and fail closed on missing or unreadable registration. Identity mismatches return `conflict`; discovery failures return `inbox_unavailable`.
It selects one explicitly registered checkpoint session using its exact host,
project and chair actor. Missing registration or a different binding is refused;
a similarly labelled session does not receive the answer. Discovery identifies
the chair, not provider liveness. A changed or missing checkpoint does not
transfer its registered identity. Explicit rebinding does not retarget old
answers: those stay addressed to their original host/session/actor.

Each batch contains immutable answers and delivery records, with request and
answer IDs. Batches cap at 100 answers and 8 KiB of compact JSON, including the
envelope and escaping. Adapters may request smaller limits and must reserve
space for their own protocol envelope. `truncated` identifies overflow; omitted
answers keep their previous delivery state. An answer that does not fit a smaller requested budget is skipped and remains queued; use the full 8 KiB boundary to offer it. Answers that cannot fit that boundary are refused when submitted, including JSON escaping and envelope overhead.

Offers atomically set `offered`, increment `attempts` and set `last_attempt_at`.
Newly queued answers start at zero offers. Offers repeat across service restarts
until explicit acknowledgement by the exact target; acknowledgement retains
its original timestamp on retries. Neither offering nor mail-read success proves
model consumption. Deduplicate by answer ID before taking action. An adapter
failure after an offer commits leaves it eligible to be offered again.

`provenant_cli::hooks::delivery_capabilities()` composes the delivery matrix.
The records-level matrix supplies Fabric mail fallback:

| Provider | Transport | Local offer reader | Live injection |
|---|---|---|---|
| Claude | Fabric mail | Library API | Unavailable |
| Codex | Fabric mail | Unavailable | Unavailable |
| Cursor | Fabric mail | Unavailable | Unavailable |
| Agy | Fabric mail | Unavailable | Unavailable |
| OpenCode | Fabric mail | Unavailable | Unavailable |

The local reader observes up to 16 indexed pending answers within an explicit
deadline. It creates no state and never records an offer attempt or acknowledges
an answer. It requires a current explicit save from that working directory.
Configured Fabric home mappings defer to mail; no remote I/O runs in the reader.
Internal candidate pages cap at 128 KiB; adapters must fit their final protocol
output within 8 KiB, skipping oversized candidates without hiding smaller ones.

This slice adds the service API. CLI/MCP syntax is unchanged. Hook composition,
remote delivery and provider input adapters remain separate work. Both apps may
stay closed; there is no background polling or provider launch.

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
