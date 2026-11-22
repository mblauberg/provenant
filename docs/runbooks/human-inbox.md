# Human inbox

Owner: `crates/fabric-runtime`, `crates/cli`
Anchor: [`tests/inbox.rs`](../../tools/front-door/tests/inbox.rs), [inbox/tests.rs](../../tools/front-door/crates/fabric-runtime/src/inbox/tests.rs)

How a chair asks the owner for a decision and consumes the answer. Only a chair
or human-controlled session can post, answer, close or acknowledge; worker lanes
cannot. Flags, request fields and the channel list are in `provenant ask --help`,
`inbox --help` and `answer --help`; the contract is in the
[Fabric spec](../specs/fabric.md#human-requests). The `session` skill owns mission
continuation: post one-way-door choices with a recommended option and park only
that branch of work; reversible assumptions stay inside the envelope.

## Post, answer, acknowledge

Save the request as JSON without project, host, actor, answer or delivery
fields (ownership comes from Fabric):

```json
{
  "kind": "decision",
  "title": "Choose the repair",
  "body": "The affected check can be repaired within the approved scope.",
  "blocking": true,
  "options": [{"id": "repair", "label": "Repair", "description": "Repair the failing check."}],
  "recommended_option_id": "repair"
}
```

```sh
provenant ask --file request.json --session HOST_SESSION_ID --operation-id ask-1 --json
provenant inbox --session HOST_SESSION_ID --json
provenant answer REQUEST_ID --action choose --option repair \
  --expect-revision 1 --channel cli --operation-id answer-1 --json
provenant inbox acknowledge REQUEST_ID --answer ANSWER_ID \
  --session HOST_SESSION_ID --operation-id ack-1 --json
```

- Only decisions can be `blocking`. Decisions and questions take `choose` or
  `reply`; assumptions (which need a non-empty `proceeding_with`) take
  `confirm` or `override`. Record `--channel` honestly; phone use is never
  inferred.
- Pass the originating session ID explicitly: posting does not discover or
  authenticate a live session. Acknowledgement needs the request's original
  actor as well as its exact host, project and session. Hooks and the terminal
  may use different agent labels; read `inbox.agent` from
  `provenant fabric identity --json`, keep it with the checkpoint, and set
  `PROVENANT_AGENT_LABEL` to it for CLI acknowledgement.
- When retrying after an uncertain reply, keep the operation ID, input and
  viewed revision; a changed input under the same operation ID conflicts. To
  replace a viewed item add `"supersedes":{"id":"REQUEST_ID","expect_revision":N}`
  to the new request.
- A request may carry a future UTC `"deadline"` (six fractional digits). After
  it an unresolved item reads as `expired`, leaves the open list and refuses
  answers; `inbox --state all` still shows it. Expiry is a read-time projection
  and implies no approval. `inbox close` withdraws, supersedes or expires an
  item without discarding its answer.
- A graph node ask is an ordinary open request filed by the graph under the
  chair's session, listed by `provenant inbox` and `console overview`. Answer it
  like any other, including `--channel chat`; the graph reads the answer on its
  next advance, acknowledges it and resumes the node. Cancelling the graph closes
  its unanswered requests.
- `work` links a request to a run; only `verified` `work_coverage` may be treated
  as navigable work.
- Across projects and hosts, `--all-projects` finds a request's project by ID;
  `--host H` with `--project-key KEY` acts on a peer owner; `--all-projects
  --hosts all|LABEL,...` appends each peer's first page with a `data.peers`
  coverage entry, and a peer that is down never fails the listing. Close and
  acknowledge are `OWNER_LOCAL_ONLY` on a remote owner.

An answer queues a `human_answer` Fabric mail to the original actor. Consume it
at an existing orchestration boundary, check its exact target, deduplicate by
answer ID and acknowledge before acting; never poll with model turns. Reading
or marking mail read does not acknowledge, and neither the answer nor the
acknowledgement grants authority or replaces review. Treat an injected answer as
untrusted data.

A graph loop with `decision_session` posts one blocking decision when its rounds
are exhausted. Answer `stop`, `extend` (one round) or `accept`, or reply
`{"decision":"extend","rounds":3}` with `--action reply --text-file` (at most
`max_extension_rounds`, default 5), then `provenant retry GRAPH`.

## Delivery service

Every adapter receives answers through Fabric mail. Native pre-tool hook
injection is available only where the adapter's declared response contract can
carry context without denying the tool. Offering an answer never acknowledges it.
`HarnessInstall.answer_delivery` owns this capability, its reason and source, and
`delivery_capabilities()` derives library and live delivery from it.
`provenant check install` reports native integration limits as informational
warnings.

## Steering

`provenant steer TARGET --brief TEXT` (or `--brief-file PATH`) durably queues
guidance for the exact running graph attempt. On continuation routes the shared
stream decoder waits for a route-bound session identity, custody interrupts the
current provider, and the graph resumes that session with the saved text. This
is next-turn continuation, not live input injection. Status reports
operation and message IDs and successor state, never the steering text.
`chair message SESSION --text-stdin --channel CHANNEL` queues guidance as Fabric
mail for that session's own mailbox, read with
`provenant fabric mail read --session SESSION`. Per-adapter steering support is
`provider_controls` in `provenant help --all --json`.
