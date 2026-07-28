# Agent Fabric authority

## Risk and authority profile

Risk tier: **crucial**. The design affects a shared harness, credentials-adjacent provider processes, write authority and stateful runtime data.

The following envelope records the completed design pass. The active delivery authority is recorded in the canonical `.agent-run/AFAB-001/RUN.json` `delivery-run` receipt.

```yaml
authority:
  approver: human-maintainer
  expires_at: design-approval-or-rejection
  allowed_source_paths:
    - docs/specs/
  allowed_artifact_paths:
    - /tmp/fable-agent-fabric-design.md
    - /tmp/agent-fabric-review-*.md
  prohibited_actions:
    - implement-runtime-code
    - register-mcp-server
    - modify-provider-authentication
    - start-or-install-daemon
    - delete-or-compact-provider-sessions
    - change-model-routing
    - commit-or-release
  disclosure:
    external_provider_source: local-harness-docs-only
    secrets: prohibited
```

## AuthorityEnvelopeV2 contract and containment

`AuthorityEnvelopeV2` is the sole current authority input, delegation and
stored-envelope contract. `AuthorityInput` is only its type alias, not a second
wire shape. An envelope contains exactly:

```text
schemaVersion: 2
approval: { approvedBy, evidenceId, evidenceDigest: sha256 }
workspaceRoots: [workspace-relative-path]
sourcePaths: [workspace-relative-path]
artifactPaths: [workspace-relative-path]
actions: [daemon-grantable-agent-operation]
deniedPaths: [workspace-relative-path]
deniedActions: [daemon-grantable-agent-operation]
prohibitedActions: [action-name]
disclosure: { level: allowed | forbidden }
  | { level: scoped, scopes: [local | approved-provider | external] }
secrets: { access: none }
  | { access: use-without-disclosure, references: [secret-reference] }
deployment: { allowed: false }
  | { allowed: true, targets: [deployment-target] }
irreversibleActions: { allowed: false }
  | { allowed: true, actionIds: [approved-action-id] }
network: { toolEgress: none }
  | { toolEgress: allowlist, allowedHosts: [host] }
expiresAt: RFC3339-timestamp
budget: { recognised-budget-unit-key: non-negative safe integer }
```

Enabled/scoped policy lists are non-empty and unique. The protocol codec owns
the bounded string, collection and timestamp validation for this closed shape;
it may not add fields or policy arms outside this contract.

Source and artifact paths must be workspace-relative and contained by an
envelope's workspace roots. Delegation must not widen the parent: the approval
binding is unchanged; allowed roots, paths and actions narrow; path, action and
prohibited-action denials are preserved or strengthened; disclosure, secret,
deployment, irreversible-action and network policy narrow; expiry does not
extend; and each child budget key and amount is present in and no greater than
the parent. Invalid, extra-field, unknown-version and widening envelopes fail
closed before storage or use.

Provider projection has one narrow, stateless, write-free compiler. It accepts
a validated V2 envelope, the workspace-root resolver and an immutable provider
payload, then returns a new payload. On fresh admission for the current
`review-readonly` profile it rejects expired or provider-undisclosable
authority. Fresh admission and exact replay both reject caller-supplied trusted
provider controls and a working directory outside the delegated source paths
or overlapping a denied path. Exact replay may skip only the time-varying
expiry and disclosure checks for the already-bound envelope. The compiler
injects the canonical working directory, read-only root, exact read tools,
`approvalPolicy: never` and a read-only sandbox. It neither stores authority
nor invokes a provider; those remain separate owners.

This is a direct V2 cutover. There is no legacy decoder, dual parser or
compatibility bridge. Pre-release local state must already contain V2 authority
or be regenerated or reset. Any future write profile must extend this canonical
contract and preserve the same monotone containment boundary; it is unavailable
until its separate human and evidence gates pass.

## Leases, delegation and barriers

The daemon issues fenced, generation-bearing leases:

- one chair lease per run;
- one owner lease per active task;
- one write-scope lease per canonical path set;
- one adapter-turn lease per fabric-managed provider session. Attached
  interactive sessions have registry and mailbox identity but no fabricated   external turn control.

Every mutation supplies the expected lease generation. Stale generations fail closed. Child authority must be a strict or equal subset of its parent for paths, actions, disclosure, expiry and budget.

Lease expiry or generation change fences fabric mutations only. Before granting an overlapping successor write-scope or adapter-turn lease, the daemon proves one of:

1. the predecessor process or turn is terminal and its write capability has
   been revoked;
2. an operating-system sandbox prevents it reaching the successor's scope; or
3. it could produce only immutable patch artifacts and the sole serial applier
   rejects its old generation.

If none is provable, the scope is `quarantined` and no successor writer starts. Unmanaged interactive or full-access sessions are patch-only unless their liveness and revocation mechanism is enforced. Lease generation and action ID are propagated to adapter commands and serial-apply operations.

A subtree leader may close a subtree barrier. Only the chair closes a stage or run barrier. Closure requires:

- required descendants are terminal, cancelled or explicitly degraded;
- no unresolved provider turn or active write lease remains;
- artifacts and hashes are recorded;
- required checks pass;
- required messages are acknowledged or abandoned with a reason;
- the checkpoint records mailbox cursors and provider resume references;
- human gates are resolved;
- the next owner acknowledges the exact generation and revision.

For the final run barrier, the human acceptance gate takes the place of next-owner acknowledgement.

Direct filesystem access cannot be perfectly fenced by the daemon. Shared source retains the serial-applier rule unless write scopes are provably disjoint and predecessor revocation is enforced.

## Security and privacy

- The daemon listens only on a per-user Unix socket by default.
- Socket and state directories reject group or world access.
- A discovery token authenticates only a same-user control-plane client; it is
  not an agent-authority credential. Its purpose is to deny access to sandboxed   worker processes that share the user ID but cannot read the discovery path;   adapters do not pass it into worker environments.
- On attach, the daemon issues a revocable, run-scoped capability bound to one
  fabric principal, permitted operations, mailbox, authority hash, expiry,   connection nonce and current lease generation.
- Grants above the client's registered role require chair approval recorded in
  the journal.
- The daemon derives sender, run and authority from authenticated context rather
  than MCP arguments. Chair-only, owner-only and recipient-only access controls   apply to every tool and resource read.
- Attach, takeover and token rotation are journalled and use compare-and-set
  against the current generation.
- Secrets never appear in configuration, messages, receipts or Herdr pane
  metadata.
- Adapters receive only the environment variables required for their provider.
- Message bodies cannot grant authority. Unrestricted same-user shell access
  may bypass cooperative controls, so receipts distinguish protocol-enforced   from operating-system-enforced authority.
- Project path resolution rejects traversal, symlink escape and paths outside
  the approved workspace roots.
- Read-only claims distinguish policy-only restrictions from substrate-enforced
  restrictions.
- Remote sockets, WebSocket listeners and external dashboards are disabled in
  the first release.

### Human operator principal and commands

The Console authenticates as a distinct `operator` principal, never as an agent or chair. An operator capability is revocable and binds:

- one operator, project, optional project session and principal generation;
- an explicit subset of `read`, `decide`, `steer`, `pause`, `resume`,
  `cancel`, `drain`, `stop`, `launch`, `takeover`, `git`, `git-authorise`,   `git-custody-resolve`, `agent-lifecycle-recovery-issue` and   `external-effect` operations;
- issue and expiry times no later than the project session;
- the current project/session generation; and
- for takeover, the handoff digest, old chair generation, expected run and
  session revisions and compare-and-set target revision.

A project-bound `launch` capability may create a reviewed session before a session ID exists. Every other session mutation requires the exact session ID and generation. Possession of `decide` does not imply `launch`, `takeover` or `external-effect`. `git` admits an already-authorised typed mutation; `git-authorise` may issue or revoke a narrower Git grant but cannot execute one. `git-custody-resolve` may adjudicate only an eligible unprovable Git custody and cannot execute Git or issue a grant. `agent-lifecycle-recovery-issue` is a session-bound local-control action only: it may issue the exact narrow fresh-rotate capability in the lifecycle-custody contract after its bound consequential gate, but cannot rotate, call a provider, take over a chair or abandon an agent. None implies another.

Every operator mutation carries the capability, stable command ID, expected revision, actor and provenance. The daemon derives project and actor identity from the authenticated connection, authorises the exact operation before the mutation and journals before/after state plus linked evidence. Retrying the same command and payload returns the committed result. Reusing a command ID with changed payload, project or expected revision fails as a conflict. Absent, expired, revoked, wrong-project, wrong-generation and action-insufficient capabilities fail closed.

Direct conversational input may resolve a gate only through an independently attested operator-input record containing the provider message ID, exact human utterance, input-channel provenance, expected gate revision and bound artifact digests. Echoes, pane or CLI injection, agent-authored text and unavailable direct-input provenance cannot approve. Consequential decisions also require a persisted preview and a separate explicit confirmation command.

### Exact scoped-operation targets and optional Herdr composition

The public `fabric.v1.scoped-gate.check` operation form is extended to require this closed target in addition to its existing exact project session, coordination run, dependency revision and protocol operation ID:

```yaml
operationTarget:
  kind: run
```

or:

```yaml
operationTarget:
  kind: task
  taskId: exact-task-in-coordination-run
```

No target-less operation check is accepted. This is an enforcement target, not authority: the daemon still derives identity, reauthorises the operation and checks the current dependency graph. The stored gate operation kind and the exact current affected-task bindings form one predicate; neither may be checked alone.

The optional Herdr boundary is one daemon-owned integration seam, not a direct Console-to-pane mutation path. It accepts only the closed operations `console.ensure-pane`, `agent.ensure-pane`, `panes.arrange`, `agent.project-metadata`, `attention.project`, `target.focus`, `agent.wake`, `notification.show` and the separately reference-validated `steer.inject-fire-and-forget`. Every effect has one stable Fabric action ID, is durably prepared before Herdr I/O, is marked dispatched before the call and uses evidence-only lookup after ambiguity or restart. Prepared actions are never dispatched by recovery. A missing, disabled or incompatible Herdr integration exposes typed unavailability/`visibility-degraded`; all Fabric and Console coordination remains portable without it. Pane/process presence, absence, focus or scrollback never proves provider identity, task state, message/result delivery or effect outcome.

Added requirements are:

- **FR-047:** Operation gate checks shall bind one exact run/task target and
  current dependency revision, and task/subtree gates shall block only matching   affected tasks while unrelated siblings remain runnable.
- **FR-048:** Optional Herdr effects shall use one stable daemon-owned action
  preparation/dispatch/recovery seam with closed operation variants and honest   disabled/degraded behaviour; pane state shall confer no Fabric truth.

Acceptance additionally requires:

- **AC-039:** closed-codec fixtures reject a missing, extra, malformed,
  cross-run or stale operation target. Runtime matrices cover task, subtree,   run and release gates against task and run targets, including two sibling   tasks invoking the same protocol operation at one dependency revision; only   the affected target blocks. Dependency rebinding changes the answer   atomically. Herdr fixtures cover every closed operation, disabled   portability, stable replay, prepare/dispatch crash points, lookup-only   ambiguity recovery and absence of every pane-derived authority, delivery or   completion claim.

### Provider-native input attestation principal

The public integration principal is the following closed authenticated shape:

```yaml
integrationPrincipalV1:
  kind: integration
  integrationId: exact-integration
  projectId: exact-project
  projectSessionId: exact-project-session
  runId: exact-coordination-run
  principalGeneration: positive-safe-integer
  providerId: exact-provider
  providerSessionRef: exact-provider-session
```

Its credential is issued only by trusted daemon composition and grants an explicit subset of exactly `fabric.v1.provider-state.report`, `fabric.v1.provider-action.reconcile`, `fabric.v1.operator-intervention.record`, `fabric.v1.visibility-failure.record`, `fabric.v1.budget.usage.record`, `fabric.v1.budget.usage.reconcile`, `fabric.v1.integration.input-attest`, `fabric.v1.resource.reconcile`, `fabric.v1.result-delivery.claim`, `fabric.v1.result-delivery.provider-accept` and `fabric.v1.result-delivery.consume`. No other operation may advertise or admit an integration principal. Every request must carry or resolve to the exact bound project/session/run and current principal generation; operation-specific provider action, resource, budget, delivery or native-event ownership is then rechecked at point of use.

The durable credential binds the full principal, granted subset and bounded issue/expiry/revocation state, but contains only the credential hash. A raw `afi_` bearer exists only in the trusted adapter's volatile custody and is forbidden from SQLite, discovery, events, logs, projections, errors, receipts and rendered content. Console, agent and ordinary operator principals cannot issue or use this credential. An integration principal cannot acquire agent, chair, operator, lease, gate-resolution, dispatch or topology authority; `operator-intervention.record` records only its closed provider-originated intervention fact and never authenticates a human/operator decision.

The public protocol authenticator resolves a current integration credential to the closed shape above. The daemon dispatcher has an exhaustive integration arm for only the granted operations and never falls through an agent or operator dispatcher. It reloads expiry, revocation, full binding and grant at point of use. The input-attest arm routes to the operator attestation store. The authenticated provider ID and provider-session reference must equal the attested native event; the request cannot select or substitute them. Project, project session, run, integration and principal generation are likewise derived and rechecked at point of use. Revocation, expiry, wrong project/session/run, wrong provider/session, stale generation, operation omission and token reuse across bindings fail before any mutation.

The trusted provider bridge may classify an event `direct-human` only from an authenticated provider-native callback that carries the immutable provider message/event identifiers, exact human utterance and role. Pane/scrollback observation, Herdr state, terminal input, CLI or MCP injection, echoed text, assistant/tool output, wrapper-created assertions and ambiguous or unavailable role provenance are ineligible. The adapter remains a trusted translation boundary, so conformance runs its production classification code against a fake native transport: there is no wrapper-only success path.

Before insert, the daemon derives one canonical ordered digest vector from the gate's persisted evidence references, preserving first occurrence, then the release receipt and accepted artifact digest when present. The attestation must match that vector exactly; missing, extra, wrong, duplicate or reordered values fail. The public gate sentinel `authenticated-human-operator` matches any active operator in the exact project, while an explicit operator ID matches only that principal. Gate resolution rechecks the attestation's exact operator, integration, generation, gate revision, command provenance and canonical digest vector against current durable state. A gate with no bound artifact digest cannot use conversational resolution.

Added requirements are:

- **FR-049:** A provider-native integration principal shall authenticate and
  dispatch only its explicit closed-operation subset under hash-only, exact   project/session/run/provider-session/generation authority without widening   agent or operator authority.
- **FR-050:** Conversational attestation and later gate resolution shall both
  match the gate's canonical ordered artifact-digest vector and exact attested   provenance.

Acceptance additionally requires:

- **AC-040:** A real public-protocol create/read context followed by a fake-
  native-provider direct-human callback, integration attestation and operator   gate resolution succeeds once. Missing/extra/wrong/duplicate/reordered   digests; echo, assistant/tool, injected, ambiguous and unavailable roles;   wrong provider/session/project/project-session/run/operator/generation;   expired/revoked/insufficient credentials; every ungranted or non-integration   operation; agent/operator-dispatch fallthrough; message replay; changed gate revision; changed   command provenance and restart all fail closed. Durable and rendered output   contains no `afi_` fragment, and disabled provider integration leaves typed   Console resolution available.

### Budgeted ephemeral review and revision-bound Console decisions

A task-bound ephemeral `provider-action.dispatch` spawn requires a delegated authority with a hard `turns` dimension. The admitted turn reservation is the positive safe-integer `maxTurns`, defaulting to one and injected into the provider payload before identity/persistence. Every shipped adapter must prove that ceiling at point of use: Claude receives the SDK cap; a one-shot adapter accepts exactly one and rejects a larger value. Provider calls and concurrent turns reserve one when configured. Each configured cost, provider-qualified token or wall-clock dimension is also reserved under its exact unit. Dimensions that the operation cannot consume, such as descendants, message bytes or artifact bytes, are neither debited nor fabricated as provider usage.

The daemon rechecks the task's non-terminal state, atomically reserves the complete applicable vector and inserts an immutable provider action bound to the exact authority and task. Failure of any predicate or ledger change rolls back all of them before provider work. While that action is open, the task cannot commit a terminal transition. Existing-action identity/replay is checked first, so an exact replay still returns its committed result after the task has later become terminal; a new action does not.

Task-bound answer-bearing dispatch does not hold a public protocol request open for the provider turn. After the immutable action and full budget reservation and command receipt commit together, Fabric queues exactly one daemon-owned completion and may return the `prepared` or `dispatched` action receipt. A bounded FIFO worker atomically claims `prepared -> dispatched` only when shared provider-turn capacity is available. `provider-action.read` observes that same action until terminal evidence supplies the bounded non-review answer or, for a certifying review, the answer digest and safe parsed result plus result digest. Connection closure, protocol timeout and exact command/action replay do not cancel or duplicate the effect. For ordinary noncertifying actions, live reconciliation observes locally owned prepared/dispatched work without lookup or quarantine. Every certifying action instead uses the sole recovery owner in the certifying route-integrity recovery contract. Daemon shutdown drains tracked work before closing its adapter and closes SQLite; restart uses its typed recovery rather than blind replay.

Terminal evidence settles every dimension exactly once: proven usage is consumed, unused reservation is released, concurrency is released, and an unreported applicable dimension becomes usage-unknown. Ambiguity retains the reservation while lookup may still prove the result. Quarantine freezes only unproved dimensions. An authenticated action reconciliation may later replace unknown values with exact adapter evidence and unfreeze a dimension when no other unknown owner remains. Restart applies the same transitions from the persisted action binding and cannot release or spend twice. Delegation computes available capacity as granted minus reserved minus consumed. the certifying-review contract is the closed certifying-review exception: every proved-effect terminal settles exact authenticated usage or conservatively charges the remaining reservation, so it never enters generic usage-unknown recovery.

The closed Attention summary may include `gateBinding` only as this shape:

```yaml
gateBinding:
  gateId: exact-scoped-gate
  gateRevision: positive-current-revision
  coordinationRunId: exact-row-run
```

The daemon derives it from an existing pending/deferred scoped gate whose project session and coordination run equal the Attention row. Missing, closed, cross-session or cross-run candidates omit the binding; the Console cannot parse an item title or accept operator text as a substitute.

A bound intake read may include `chairRequestSeed` containing only the durable prior request's conversation ID/base revision and the exact current run chair's agent/provider-session target. It is omitted when that correlation or current target cannot be proved. A successor `Discuss` or `Request changes` operation uses the normal revision-CAS intake-revise request with a new task request bound to the successor intake revision, existing gates and artifact digests. No projection itself mutates state or transfers authority.

Added requirements are:

- **FR-051:** Ephemeral provider review shall atomically reserve, durably bind,
  settle, release or freeze every applicable delegated provider-budget   dimension across concurrency and restart.
- **FR-052:** Attention gate and intake chair-request projections shall be
  strict, daemon-derived, revision-bound and incapable of conferring authority.

Acceptance additionally requires:

- **AC-041:** Concurrent bounded spawns cannot overbook any applicable unit;
  every adapter enforces the admitted turn ceiling; terminal lookup settles   once after restart; ambiguity retains; invalid/unprovable lookup freezes only   affected units; later exact reconciliation unfreezes them; exact replay adds   no reservation and survives later task completion; exhausted/unknown budgets,   task-completion races and all terminal task states reject new provider work.
- **AC-042:** Projection fixtures prove a live same-session/run gate and a
  durably correlated current-chair intake seed, while closed, missing, stale,   malformed and cross-boundary candidates fail closed or omit the optional   field. Console review/confirm tests then resolve/revise only those exact   bindings.

#### Owned four-slot profile

The protocol package owns schemas/certifying-review-four-slot-v1.schema.json and the checked-in profile document config/review-profiles/certifying-review-four-slot-v1.json. Both are closed and digest-bound. The profile has exactly these rules:

~~~yaml
resolvedReviewProfileSlotV1:
  schemaVersion: 1
  slot: native-or-other-primary-or-cursor-grok-or-agy-gemini
  adapterClass: primary-native-or-equal-primary-or-cursor-or-agy
  adapterId: exact-activated-adapter
  adapterContractDigest: sha256-prefixed-digest
  providerFamily: canonical-family
  model: exact-model
  requiredActualEndpointProvider: exact-provider-id
  requiredActualProviderFamily: exact-provider-family
  requiredActualModel: exact-provider-model
  requestedEffort: null-or-exact-effort
  resolvedEffort: resolvedEffortV1
  sourceMode: direct-portal-or-portal-helper
  runtimeIdentityDigest: sha256-prefixed-digest
  platformIdentityDigest: sha256-prefixed-digest
  providerTurnCeiling: positive-integer
  internalStepCeiling: nonnegative-integer
  mandatoryReadOps: nonnegative-integer
  mandatoryReadBytes: nonnegative-integer
  explorationReadOps: nonnegative-integer
  explorationReadBytes: nonnegative-integer
  routeAliases: ordered-nonempty-unique-ids
  riskReadMapDigest: sha256-prefixed-digest
  reviewerFamilyRelation: same-family-exempt-or-distinct-family-proved

resolvedReviewProfileV1:
  schemaVersion: 1
  profileId: certifying-review-four-slot-v1
  profileSchemaDigest: sha256-prefixed-digest
  targetChairFamily: openai-or-anthropic
  slots: exactly-four-resolvedReviewProfileSlotV1-in-profile-order
  resolvedProfileDigest: sha256-prefixed-canonical-profile-digest
~~~

`resolvedProfileDigest` hashes RFC 8785 JCS of the complete profile with only that digest omitted. Unknown/extra slots or fields, crossed availability identity, an inapplicable effort with nonnull request, or a relation other than the two admitted snapshot values rejects. The same slot object is stored, projected in Console and equality-checked at dispatch. `requiredActualProviderFamily` and `requiredActualModel` must equal the slot's admitted `providerFamily` and `model`; `requiredActualEndpointProvider` equals the resolved admitted endpoint behind the allowed route alias. None may be derived after terminal output.

The three `requiredActual*` fields are certification requirements, not aliases for admission. Endpoint provider, family and model must each be proved by the terminal `deployedRouteObservationV1` `observed` arm, sourced from the authenticated provider result or a contract-defined adapter attestation, and must equal both this profile and the admitted route. Their exact observation digest and admission digest bind the closed `actualReviewRouteIdentityV1` and its `actualRouteIdentityDigest`. Missing/unavailable proof emits `actual-route-unproved`; any proved inequality emits `actual-route-mismatch`. Any other route field that is observed rather than unavailable must also equal admission or emits the same mismatch. Either makes the result noncertifying and accepts no reported resolutions, but every safely parsed adverse P0-P2 finding is retained and added to the paged open set. This rule applies only to certifying review. Generic provider work continues under its matched permission profile and route authority.

Every resolved certifying slot also requires its capability snapshot `safety.enforcedReadOnly: true` and an equality-matched enforced read-only permission profile at availability, preparation, admission and dispatch. False or unknown produces the existing typed certifying-slot unavailable result before provider I/O; it cannot fall back to a generic call. This does not make generic answer-bearing work read-only: a non-review action may use an authority-matched write-capable profile as specified by the activation contract.

| Slot | Adapter class and ID | Family/model rule | Reviewer-family relation to target chair |
| --- | --- | --- | --- |
| native | primary-native; codex-app-server for OpenAI chair, claude-agent-sdk for Anthropic chair | exact activated native review route; family equals target chair | same-family-exempt |
| other-primary | equal-primary; claude-agent-sdk for OpenAI chair, codex-app-server for Anthropic chair | exact activated equal primary distinct from target chair | distinct-family-proved |
| cursor-grok | cursor; cursor-agent | family xai; exact activated model cursor-grok-4.5-high | distinct-family-proved |
| agy-gemini | agy; agy | family google; exact activated model Gemini 3.1 Pro (High) | distinct-family-proved |

The target chair and eligible publisher must be OpenAI or Anthropic and have the same family. The resolved snapshot names, for every slot, exact adapter class, adapter ID, adapter contract digest, model family, model, requested effort plus the tagged resolved-effort policy, source mode, provider-turn/internal-step ceiling, maximum read operations/bytes, mandatory/exploration subledger bounds, risk-map digests, route aliases and the one reviewer-family relation requirement. Missing or ambiguous resolution rejects target preparation. The native exemption is profile data, not reducer prose. For a Codex/OpenAI target, other-primary therefore resolves to Claude/Anthropic. A proved same-agent lifecycle rotation that satisfies the binding contract above preserves this matrix. Any other
    chair/family/adapter/contract/model/profile change makes the target stale;
    successful successor preparation then supersedes it and resolves the whole
    matrix again.

Publisher eligibility and reviewer-family relation are separate predicates. The eligible root's proved publisher family must equal the target chair family. External slots then prove only that reviewer family differs from the target chair family. Native is `same-family-exempt`. `same-family-forbidden` and `family-unproved` block under `reviewer-family-distinctness`. This relation makes no claim about authors, contributors or write-lease holder families, which this profile does not track. No publisher-independence flag, disposition or blocker exists.

Every resolved adapter must advertise activated certifying-review-packet-only.v1 under the exact adapter contract digest. That capability proves:

- model-visible source is limited to the action-bound Fabric portal. A provider
  without native portal MCP uses the trusted   `agent-fabric-review-portal-supervisor` Rust executable in fixed   `portal-stdio-v1` mode. Its absolute install path, device/inode, SHA-256 and   code identity are pinned by the activated adapter contract. Cursor/Agy MCP   configuration names that same binary and exact mode; no shell launcher,   bearer argument or inherited provider descriptor is permitted. The provider   MCP manager may launch it only from the exact outer-supervisor-admitted   provider-runtime closure. Neither launch is model command authority;
- the adapter receives a daemon-built minimal auth/config capsule when its CLI
  requires one: a per-action 0700 synthetic HOME containing only exact 0600   adapter auth/config bytes. Its path/value is outside the model-visible   filesystem/tool namespace. The helper environment contains exactly the   non-secret locators `AGENT_FABRIC_REVIEW_SOCKET`,   `AGENT_FABRIC_REVIEW_ACTION` and `AGENT_FABRIC_REVIEW_CONTRACT`; the last two   are the canonical provider action pair and activated contract digest. They   are correlation data, not authority. No real HOME, user/project state,   capability, bundle path or credential is inherited;
- outer OS confinement and canary transcripts deny every user/project/auth file
  read outside portal-returned bundle objects plus all workspace-index, shell, edit/write,   browser, arbitrary-network and provider-source effects; and
- process custody is crash-safe, not only deadline-safe. Before any custody
  directory, portal filesystem socket or capsule exists, the daemon must bind the already-opened recovery-root path/device/inode, all relative basenames, contract and expected capsule digest, then create exclusively beneath that no-follow root and fsync every artifact/directory/parent. Crash recovery may remove only a revalidated daemon-created partial object; any substituted identity is integrity failure. Private custody stores the canonical action directory and a distinct 0700 claim directory outside provider mutation authority, both paths/device/inode, and only relative socket/capsule basenames with their expected file device/inode/type and kind-specific digest. The socket is `S_IFSOCK` and its digest is the pinned domain-separated device/inode identity digest; the regular-file capsule digest hashes its exact bounded bytes. Both require `st_nlink=1` at capture, claim revalidation and unlink so a surviving hard-link alias cannot be reported removed. The Rust `agent-fabric-review-portal-supervisor` crate owns that removal boundary: it opens each directory no-follow, refuses a non-private, cross-device or self-referential claim namespace, equality-checks the persisted `agent-fabric-custody-claim-v1` basename, and advances one caller-persisted phase per call through atomic no-replace rename, post-claim revalidation, directory `fsync` barriers and `unlinkat`. This crash-recovery metadata never enters a public projection, provider/model input or receipt. Only the three minimum non-secret helper bootstrap values above may enter the isolated helper environment; they expose no device/inode, capsule locator or authority. Launch itself is the fixed `portal-stdio-v1` mode and nothing more. The launcher must pass exactly the single argument `portal-stdio-v1`, exactly those three locator environment values and one private supervisor control FD 3 marked `CLOEXEC`; the binary refuses any other argv, any fourth environment value and any inherited descriptor above standard error, so provider entry inherits exactly stdio FDs 0–2 and control FD 3 never crosses an exec. Control EOF/HUP, deadline, cancellation or provider exit makes the supervisor TERM the complete isolated group, wait 250 ms, then KILL and reap within a further 250 ms, proving descendant absence before it reports cleanup. It closes its descriptors but never removes persisted socket/capsule paths because it cannot advance daemon-owned cleanup phases. The daemon watches supervisor death and solely owns phase-aware path cleanup; after daemon death, restart resumes it from the unchanged persisted phase. [ADR 0018](../../adr/0018-accept-portal-stdio-v1-launch-custody.md) accepts that posture and records what it leaves open: no pre-exec registration handshake, committed process-custody row or ACK stands between the launch decision and `execve`, so a provider executable substituted inside that window is not detected. That is a rejected design rather than deferred work; only a multi-user host, or a provider binary sourced outside the user's own control, would reopen it.

The trusted adapter transport may contact its fixed provider endpoint outside the model tool sandbox; it cannot expose a general network tool. When provider API transport and model-visible web tooling share one process, capability requires provider-native proof that policy separates them or a contract-pinned, destination-constrained proxy for the provider API bytes. Otherwise it is false. The threat model covers accidental or model-induced misuse of an authentic pinned runtime. A deliberately compromised provider binary is outside it and must instead fail binary provenance/code-identity admission. Claude SDK and Codex app-server may bypass the stdio/Unix-socket helper only through their native dynamic-tool transports when those transports expose the same two generated schemas, action capability, read/search ledger, source isolation, terminal framing and journal evidence. Their activation canaries have full parity: exact discovery/two positive calls; project/user/auth absolute and relative read denial; shell, write/edit, browser/web/network, unrelated MCP/ resource/prompt denial; bundle crossing; deadline/cancel cleanup; and absence of credential/capability text. Codex native confinement is mandatory, not inferred from Claude or the app-server sandbox. A direct route that cannot prove every canary uses `portal-stdio-v1` when its provider integration can confine that helper equivalently, otherwise advertises capability false. Cursor/Agy always use the pinned stdio helper and create no source workspace. Their model-visible allowlist contains exactly `mcp(agent-fabric-review-bundle/review_bundle_read)` and `mcp(agent-fabric-review-bundle/review_bundle_search)`. Adapter bootstrap may not be represented as a model tool: the outer adapter supervisor may execute the exact provider-runtime closure, and the provider MCP manager may internally launch the exact helper/path/digest/fixed argv. Neither grants the model an executable tool. Every other `mcp(*)`, `command(*)`, read/write/shell, browser/web, network, resource or prompt path is denied before effect. Discovery must return exactly the one server and two tools defined in the complete-review-bundle contract. Any extra surface, successful denied effect or outside-portal source read invalidates the action.

Confinement has two distinct executable allowlists. Trusted adapter bootstrap may launch only the activated provider-runtime closure plus portal helper/broker; model-triggerable descendants may reach only the two portal calls and no executable tool. For Cursor, activation resolves/bypasses the shell launcher to one pinned real target where the build supports it. Otherwise the contract must pin and confine the exact launcher, shell, Node, index and private cache/data closure with fixed argv. Agy must likewise prove direct execution; if its hook transits `/bin/sh`, the only alternative is one exact path/inode/digest-pinned, fixed-argv trampoline whose complete child closure is canary-proved. If either closure cannot be proved on the current build, capability remains false. Agy's signed native executable is pinned by path, code identity and digest under the same rule. Seatbelt/`sandbox-exec` is an exact-OS-version canary capability, not a portable assumption: deprecation, absence, syntax/semantic drift or a failed positive/negative canary advertises false.

The TypeScript daemon must exclusively create, retain and accept one per-action AF_UNIX listener; neither the listener nor an accepted FD may be passed with `SCM_RIGHTS` or inherited by the Rust supervisor, provider or helper, and no descriptor is passed by `SCM_RIGHTS` anywhere on this path today. The Rust `portal-stdio-v1` helper owns only its connecting client FD. Peer identity is proved from kernel-authenticated credentials rather than from the wire. The Rust supervisor's `observe_peer` reads local peer credentials from the connected AF_UNIX peer before any byte is read, taking `LOCAL_PEERPID` and, on Darwin, `LOCAL_PEERTOKEN`, then binds effective UID/GID, exact PID, process start token, PGID and session, rejecting a tampered audit-token identity or PID generation; `verify_process_within_custody` rejects ancestry outside the persisted provider root. The broker must additionally prove the exact helper executable path, device/inode, digest and code identity, and the action/contract locators must match the persisted record. The first valid connection must atomically consume the broker slot; a second connection or reconnect fails, and the helper connects once and never retries. A platform that cannot prove equivalent peer credentials and process identity advertises the capability false. The action capability remains broker-side and is never an argument, environment, config value or model input. Wrong-listener/accepted-FD, inherited-FD, relayed-peer and `SCM_RIGHTS` attempts are activation negatives; the broker must observe the helper itself as peer, never the supervisor or provider root.

The Rust `portal-stdio-v1` mode is a `std`-only opaque bounded stdio-to-AF_UNIX byte relay. It enforces only fixed byte/framing and lifecycle bounds; it does not parse, generate or transform JSON-RPC, MCP or hook JSON. TypeScript is the sole semantic parser, schema validator, policy owner, ledger and canonical journal. The Console/TUI, daemon and protocol remain TypeScript; this narrow native boundary does not create a second protocol implementation.

On daemon restart, recovery must verify both PID and start time before any signal. Before provider continue/exec, the daemon must have persisted the canonical custody directory and a distinct 0700 claim directory under a daemon-private recovery root outside provider/supervisor mutation authority, including both paths/device/inode plus each socket/capsule device/inode/kind-specific digest and independent persisted cleanup phase. The directories must be distinct on one filesystem; activation probes atomic cross-directory no-replace rename and provider denial of the claim namespace before launch. Recovery opens both directories no-follow, equality-checks both identities and rejects absolute/traversing basenames. For each expected child, persisted canonical permits exact canonical-to-claim rename or recovery of an exact claim, after which the daemon must durably fsync both directories and CAS claimed before unlink. Only persisted claimed may unlink/recover absence, fsync the claim directory and CAS removed; canonical with both absent, both present or any substituted identity is integrity failure. After both child phases are removed and both directories fsynced, the daemon must persist children-removed, remove/fsync the canonical directory and persist canonical-removed, then remove/fsync the claim directory and persist removed/cleaned. Each missing-directory crash arm is admitted only from its immediately preceding durable phase. Digest-only lookup, direct unlink from the raced canonical namespace and unproved filesystem absence are insufficient. PID/start inspection is never signal authority. The live direct-parent supervisor retains its provider group leader unreaped through TERM, bounded wait, KILL and descendant-absence proof, preventing PID/PGID reuse before final reap. A restarted daemon may signal only through an OS identity-stable handle that cannot retarget after exit. The live daemon likewise may not signal the provider after supervisor death unless it acquired and retained that handle before provider continue. A Darwin daemon without an activated equivalent never signals orphaned persisted PIDs/PGIDs on either path. Exact live/ambiguous or mismatched records are quarantined with capability false, and identity-safe path cleanup proceeds only after exact process absence. FD-3 closure lets a surviving supervisor kill/reap its owned group after daemon death while leaving paths to daemon recovery. Startup evidence and canaries cover daemon-only crash, supervisor-only crash, simultaneous restart and exit/PID reuse exactly between observation and attempted restart/supervisor-death signal, which must emit no signal. Capability remains false unless current-build negatives prove a pinned child cannot escape cleanup through `setsid`, `setpgid`/job-control group split, double-fork, daemonisation or reparenting and that no descendant, listener or auth capsule survives.

The Agy policy and PreToolUse hooks encode the two exact MCP exceptions and the adapter-owned helper bootstrap, then hard-deny read_url(*), execute_url(*), read_file(*), write_file(*), every other mcp(*) and every other command(*). An Agy auth file may exist in its synthetic HOME only when an absolute-path auth- read canary proves the model cannot read it. Cursor receives per-action synthetic HOME, workspace and data directories and no global CLI config. It exposes only those two MCP tools and denies filesystem Read, Write, Shell, WebSearch/WebFetch and every unrelated MCP/resource. Current-build canaries must prove exact discovery, both portal calls, every negative and that auth remains adapter-internal (for example Keychain), never a model-readable capsule file. Hardened wrappers add outer confinement and fail-closed stream/hook evidence; stock/tool-policy-only CLI cannot certify. Unsupported adapters/platforms advertise the capability false. Missing activation, contract-digest mismatch or unenforceable sandbox returns CERTIFYING_REVIEW_CAPABILITY_UNAVAILABLE before router/provider I/O, action or budget reservation. It never falls back to a noncertifying provider call. The profile resolver appends a safe availability revision for the exact key `(projectSessionId, profileId, profileSchemaDigest, targetChairFamily, slot, adapterId, adapterContractDigest, providerFamily, model, sourceMode, runtimeIdentityDigest, platformIdentityDigest)` and advances one current head by CAS. No shorter profile/slot cache key is authoritative. It projects the exact `certifyingSlotUnavailable` reason vocabulary in the operator-control contract. `review-target.prepare` checks those rows in its bounded DB-only admission and rejects before creating a preparation when any required slot is unavailable. `review-completion.read` consults the same rows even when no target exists: it returns top-level `certifying-review-capability-unavailable` plus nonempty typed `unavailableSlots[]`, not a misleading sole `missing-target`. No target, action or budget row is needed to make this blocker observable.

Direct-portal slots reserve at most 128 provider turns and at most 112 portal operations, preserving at least 16 provider turns for planning and final answer. Portal-helper slots reserve one Fabric provider turn and at most 128 trusted internal portal calls. Both source modes reserve two nonfungible portal subledgers before provider I/O:

- mandatory: exactly the target mandatory-read count and exact mandatory bytes,
  bounded by 80 operations/6 MiB; and
- exploration: exactly 32 operations/4 MiB for direct-portal and 48 operations/
  4 MiB for portal-helper.

Each mode therefore reserves at most 10 MiB combined canonical wire bytes. The final target dynamically recomputes its exact codec/body/object/wire bytes from the approved immutable run-start to its actual sealed HEAD. With the 2 MiB maximum risk sample it must fit the 6 MiB mandatory and 10 MiB combined ceilings before target commit. No prior delivery-HEAD count or design-time raw byte observation is an acceptance oracle.

The combined ceiling is therefore 112 operations/10 MiB direct and 128 operations/10 MiB through the helper. The admission transaction atomically reserves provider turns/calls/concurrency plus mandatory and exploration `review_read_ops`/`review_read_bytes`. No slot may borrow between the subledgers or narrow its source-mode exploration headroom.

After action-capability authentication, the first successful read of each exact mandatory digest debits one mandatory operation and the exact canonical MCP response byte length. Every search, optional read, duplicate mandatory read and authenticated malformed/out-of-bundle attempt debits one exploration operation. Successful exploration also debits its exact canonical response byte length; an error debits zero bytes. The daemon reserves the operation before work, commits the exact byte debit before returning bytes, and journals subledger, ordinal, tool, request/result digest, status and byte count. Search therefore consumes budget exactly like read and cannot spend mandatory capacity. Unauthenticated calls identify no action and change no ledger. Exhaustion returns the closed budget error without source bytes and is noncertifying when it prevents the mandatory predicate.

Mandatory satisfaction counts unique root/page/chunk responses only. Duplicate reads and searches debit exploration but never satisfy a mandatory entry. Read call order is otherwise free; only the immutable manifest/page/chunk ordinal and digest chains are ordered.
