# Agent Fabric provider route, budget, and lifecycle custody

## Provider-budget custody and Console decision projections

The current baseline gives each task-bound ephemeral provider action an immutable authority, task and canonical JSON reservation/settlement vector. Each vector key is a recognised qualified unit; the settlement value is an exact non-negative amount or the closed `unknown` marker, and action custody is `reserved | settled | usage-unknown`. SQLite triggers validate same-run
authority/task ownership, non-terminal task state, available `granted - reserved - consumed` capacity and complete vector shape, then couple every insert/state transition to `authority_budget`. They reject direct contradictory writes, rebinding, reversal, status mismatch, negative capacity and task terminal transition while a bound action remains open.

Fabric injects default `maxTurns: 1` for ordinary one-shot work before hashing or dispatch. Certifying review is the closed exception owned by the resolved target profile: direct-portal Claude/Codex may reserve up to 128 SDK turns and 112 portal calls/10 MiB, preserving 16 planning/final turns, while portal-helper Cursor/Agy reserve one Fabric turn plus at most 128 instrumented
helper calls/10 MiB. Both reserve the exact at-most-80-read/6-MiB mandatory set plus 32 direct or 48 helper exploration calls/4 MiB. An adapter that cannot enforce or reach them advertises certifying-review-packet-only.v1 false. The custody reserves `turns`, `review_read_ops`, `review_read_bytes`, one `provider_calls` and one `concurrent_turns` when configured, plus each
delegated cost, provider-qualified token and wall-clock dimension. It does not debit unrelated descendant, message or artifact capacity.

Because provider turns may exceed the public protocol's 30-second request maximum, task-bound answer-bearing spawn is a durable asynchronous action. Dispatch commits `prepared` custody and its command receipt atomically, then returns promptly while exactly one tracked daemon completion owns adapter I/O. A bounded FIFO worker claims `prepared -> dispatched` only within the
shared provider-turn ceiling. The chair uses `provider-action.read` to observe the terminal answer digest and safe structured review result; it does not redispatch. Raw certifying-review output remains daemon-private. Ordinary noncertifying local reconciliation cannot look up or quarantine queued or active work. Every certifying action instead enters the sole the provider-custody recovery contract
recovery owner before generic scans. Transport loss leaves the action live, daemon shutdown drains tracked work before adapter/database close, and restart uses the typed owner without blind replay.

Terminal adapter evidence moves exact usage to consumed and releases unused and concurrency reservation. A missing applicable usage value becomes unknown; an ambiguous action retains its reservation. Recovery validates an answer-bearing terminal lookup or replay before settlement. Empty, oversized or invalid non-review answer evidence is quarantined and freezes unproved
dimensions. Unsafe/malformed certifying-review output commits `UNUSABLE`, remains private/non-certifying and may still settle independently exact usage. A later authenticated reconciliation may retry the adapter's stable lookup and move unknown dimensions to exact settlement; clearing the authority-level unknown flag requires no other unknown owner. This specification is the closed
certifying exception: every proved-effect terminal settles exact authenticated usage or charges the remaining reservation, and its single recovery owner performs at most one pair lookup.

Operator projection joins an Attention gate only by exact gate ID, project session and coordination run, and exposes only pending/deferred rows. Intake read reconstructs a successor-request seed from stored message context and the current chair row; changed conversation correlation is recovery-required, and missing provider-session continuity yields no seed. Both paths use
strict current protocol schemas and add no Console-owned state.

The Claude certifying-review adapter receives only the bounded daemon-composed envelope and action-pair-only review-bundle portal. Its model-visible namespace has an empty read-only cwd and no HOME; any trusted per-action auth capsule remains outside that namespace under OS confinement. It has no project/workspace/plugin/source MCP,
Glob/Grep/Bash/edit/write/browser/general-network tool, and no portal other than the exact digest-bound Fabric portal. Cursor and Agy apply the same substrate rule. Unsupported adapters/platforms advertise certifying-review-packet-only.v1 false and fail before provider I/O. Explicit opus effort max does not change those bounds. Non-review provider work retains its separately
admitted source-tool policy.

Deterministic verification additionally covers conditional vector-reserve races, task-completion races, crash/restart settlement, direct-SQL invariant attacks, immutable action/budget binding, replay after task completion, adapter turn-cap enforcement, mixed exact/unknown usage and later reconciliation, recovered-answer validation, gate/intake positive and negative projections,
and Claude traversal/absolute/symlink/tool-denial fixtures.

## Provider launch contract and route admission

The provider executable's identity must be revalidated immediately before `exec`, with no intervening provider-controlled step.
A provider must not be able to substitute a different executable between validation and `exec`.
The shipped review portal supervisor mechanism is `portal-stdio-v1`.
The elaborate launch protocol formerly specified here was never implemented and is recoverable from Git history.

provider_action_routes remains one insert-only row for every task-bound answer-bearing canonical `(adapter_id, action_id)` pair. For certifying review it additionally stores exact target, slot, slot-head generation at dispatch, delivery artifact/lineage, bundle/manifest/coverage, profile/schema, final-prompt and active target-chair binding generation/snapshot fields. Non-review
actions store those as null. Canonical route request/receipt JSON follows the one checked-in structural model-route.v1 schema; no database or artifact predicate exists in that codec.

Route columns store nullable `requested_effort`, closed `resolved_effort_kind=applied|inapplicable` and nullable `resolved_effort_value`; CHECK requires value nonnull only for applied and requested effort null for inapplicable. `reviewed_artifact_id` is nullable for the non-review arm. No sentinel or model-label-derived effort is stored.

~~~sql
provider_failure_substitution_events(
  adapter_id, action_id, event_generation, run_id,
  requested_family, requested_model, resolved_adapter_id,
  resolved_family, resolved_model, code, evidence_digest, created_at,
  PRIMARY KEY(adapter_id, action_id, event_generation),
  FOREIGN KEY(adapter_id, action_id)
    REFERENCES provider_action_pair_preflights(adapter_id, action_id)
)
~~~

Event generations are contiguous per pair and rows are immutable, so substitution followed by provider/routing failure is representable and ordered even when resolver failure correctly creates no provider action. The pair preflight is the parent of routes/actions, finding-capacity reservations and failure/substitution history. Its closed state is `resolving|admitted|released`;
owner/input identity is immutable, and only the one admission/release CAS may advance state. `admitted` requires the same-transaction provider action and route; `released` forbids them and consumes no finding/budget capacity. Exact replay returns the persisted ordered event/failure without rerunning the router; changed pair input conflicts. A pre-event crash may rerun only the
pure resolver under the same owner digest.

Its normalised certifying columns map one-for-one to providerRouteProjectionV1: route request/receipt digests, adapter/contract, family/model, requested effort/tagged resolved effort, target/slot, reviewed artifact, publication lineage, bundle/root/coverage/search/risk/mandatory-set/prompt digests, target chair agent/principal/lease/adapter/family/model/route,
provider-session/bridge/binding generations, adapter contract, profile digest and slot-head/attempt generations. Public action read never reconstructs a route. With `route_state=present` it joins that immutable row; with `missing` or `integrity-failed` it instead projects a null route plus the safe route-recovery evidence digest owned below. It then joins
provider_review_terminal_journal, whose unique key is adapter/action/target/slot/attempt and whose immutable columns are terminal kind, run-global terminal sequence, terminal-input digest, private answer/result/adapter-result digests, authenticated-usage digest, read-journal digest, public terminal projection digest and optional evidence-mutation-receipt digest. An append-only
terminal integrity-conflict row records a changed input digest without updating either owner.

Replay/input digest classification occurs before router work. Durable preflight and the in-process mutex key are exactly `(adapter_id, action_id)`. Its owner digest hashes run, authenticated actor/principal and the full canonical input. An exact retry joins; a different digest waits and conflicts before a router call. Cross-run same-pair use therefore runs the router at most
once and conflicts pre-router, while the same action ID under another adapter is legal. Every provider action, route, terminal, recovery, budget and adapter journal foreign key uses the pair; no action-ID-only index/lookup/sort remains. A five- second process-group-bounded resolver produces only a candidate receipt. After pair replay classification but before that resolver,
certifying dispatch CASes the exact open finding-set root and inserts either a normal worst-case 32-finding capacity reservation or a zero-new-finding resolution-only row. Capacity failure inserts no action/budget/route row and invokes no router or provider. The admission transaction then rechecks effort applicability, target/artifact/source currency, slot-head generation,
active chair binding/adapter contract and resolved adapter/family/model/effort against the profile and provider payload. Resolved adapter must equal requested action adapter and slot adapter. It attaches the existing reservation, inserts the action and command parents, and inserts the route last, atomically.

For certifying dispatch, the authenticated principal must be the current target chair at the active binding's principal/lease/provider-session/bridge generations. Exact durable replay is classified first and remains readable after rotation. A partial unique index permits only one nonterminal certifying action per target/slot/head generation. The slot head records its latest
attempt/action atomically at dispatch; a concurrent sibling action loses the CAS.
