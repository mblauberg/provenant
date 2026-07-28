# Agent Fabric observability, status, and operations

## Notification result-shape negotiation and revision invalidation

the notification-result contract owns the public result semantics. This section owns the daemon/client negotiation boundary and
persistence enforcement. Feature `native-notification-projection.v1` is a result-shape capability with no operation
grant: it may be advertised only when the daemon can condition all three affected projection operations on the
authenticated connection's negotiated features and the client can enforce the same condition after decoding.

The closed v1 summary is within the exact operator credential's existing project/session Attention visibility. It
exposes no destination, bearer value, deep link or cross-scope integration record. No per-field redaction arm exists in
v1 because every authorised Attention reader is authorised for this bounded status; any future visibility or payload
change requires another feature version.

The generated wire codecs represent `nativeNotification` as an optional schema property solely because the affected
operations each have two negotiated closed shapes. The connection-aware boundaries restore strictness: server dispatch
passes an explicit include/omit mode into snapshot, projection-page and view-page construction; the client rejects
absence in include mode and presence in omit mode before the value reaches the Console. Internal callers use omit mode
unless they request the extension explicitly. Validation recursively walks every Attention-typed value and conflict
candidate at the single public send and receive choke points. Mixed presence invalidates the whole result. A mismatch
closes the attempted attach and emits typed `protocol-incompatible` state; no cached, replayed, partial or fallback
projection from that result may enter the Console.

Console protocol binding records whether the feature was negotiated. A Console-local discriminated presentation value
separates a real `daemon-journal` summary from `feature-unavailable`; the latter is never inserted into the wire
`NativeNotificationDeliverySummary`. When the optional feature is unavailable its presenter, evaluation and export say
`notification status unavailable (feature not negotiated)` and do not fabricate a journal state, delivery revision,
claim generation, integration observation or observation time. It contributes neither zero nor an empty bucket to
notification aggregates, and exports retain explicit unknown/unavailable state. Protocol incompatibility is a connection
failure, never a per-row delivery summary or unavailable value.

The local Console makes one current-protocol connection attempt. It requires the exact current project/run/session
projection and artifact-read features; it does not retry an alternate optional profile or translate another result
shape. The request parser admits no more than 64 unique well-formed feature names combined across required and optional
arrays, each at most 64 bytes, ignores unknown optional names during negotiation and reports unknown required names as
unavailable. Unknown names can never enter the offered result feature set or operation-grant calculation. Count
overflow, duplicates, uppercase or invalid grammar, non-ASCII or over-64-byte names reject the whole request as
`PROTOCOL_INVALID` before classification. Comparison is exact ASCII byte equality with no truncation, folding or Unicode
normalisation. The exact grammar is `^[a-z][a-z0-9]*(?:-[a-z0-9]+)*(?:\.[a-z][a-z0-9]*(?:-[a-z0-9]+)*)*\.v[1-9][0-9]*$`;
duplicates within or across the two arrays reject.

The current baseline adds `AFTER INSERT`, `AFTER UPDATE` and `AFTER DELETE` triggers on `notification_deliveries` that
increment `daemon_global_state.revision` in the mutating transaction. They follow the existing projection-trigger
policy, compose with the evidence-registry constraints in the same schema and do not create events, Attention mutations
or delivery retries. Existing `integration_availability` triggers advance the revision for every stored mutation.
Availability writers make a semantically identical refresh a no-op, including `checked_at`, so one revision continues
to identify one exact snapshot, page and detail representation. Baseline catalogue verification rejects missing trigger
coverage before the result-shape feature is advertised.

Multiple row-trigger increments in one SQLite transaction are valid. The Console preserves stable IDs, focus, scroll,
drafts and pending actions when an eventless revision change reloads otherwise identical rows, and load tests bound
repeated refresh work under notification churn. The gate uses one Console, 1,000 open Attention rows, 2,000 delivery
transitions in 200 transactions of 10 over a simulated 10 seconds and exactly twenty 500 ms poll ticks. After warm-up it
requires no overlapping refresh, at most twenty completed resnapshots, p95 refresh at most 250 ms, total wall and
process CPU time at most five seconds and additional heap at most 32 MiB; host and Node version are recorded.

Deterministic verification additionally covers:

- negotiated and unnegotiated server responses for snapshot, projection-page Attention and view-page, including closed
  unknown/missing/malformed, mixed- presence and conflict-candidate negatives at both mandatory choke points;
- current client/server fixtures proving required-feature rejection, honest optional notification unavailability,
  whole-result/attach rejection of an unnegotiated extra field and no partial projection;
- forward-compatible bounded unknown-feature parsing, combined-count, cross-list duplicate and exact-grammar rejection,
  one connection attempt and zero fabricated or aggregate journal/freshness claims;
- delivery insert/update/delete plus availability changes advancing revision, invalidating a stale page/read transaction
  and refreshing Console polling while resize/resnapshot preserves UI state under bounded churn; and
- baseline reopen/catalogue behavior and absence of any notification-caused Attention acknowledgement, approval, focus
  or other authority effect.

## Capability, route-lineage, pressure, and Console reads

## Capability, route-lineage and context-pressure persistence

the deployed-route contract owns the closed public capability/discovery/route, context-pressure, topology-wave and
operational-span semantics; the activation contract owns `adapterEffectiveConfigurationV1` activation semantics. The daemon owns their
generated codecs, persistence and compare-and-set enforcement. The TypeScript caller and any offline Python route
resolver validate the same checked-in JSON Schemas; the resolver receives capability input explicitly and may not read
daemon activation configuration behind the caller.

The current generated-contract inventory adds exactly:

- `adapter-capability-snapshot.v1.schema.json`;
- `capability-snapshot-ref.v1.schema.json`;
- `capability-snapshot-summary.v1.schema.json`;
- `discovery-surface-manifest.v1.schema.json`;
- `discovery-surface-ref.v1.schema.json`;
- `deployed-route-admission.v1.schema.json`;
- `deployed-route-dispatch.v1.schema.json`;
- `deployed-route-observation.v1.schema.json`;
- `actual-review-route-identity.v1.schema.json`;
- `adapter-effective-configuration.v1.schema.json`;
- `adapter-effective-configuration-ref.v1.schema.json`;
- `provider-context-pressure.v1.schema.json`;
- `provider-context-pressure-read-request.v1.schema.json`;
- `provider-context-pressure-read.v1.schema.json`;
- `topology-wave-plan-ref.v1.schema.json`;
- `topology-wave-plan.v1.schema.json`;
- `topology-wave-plan-current.v1.schema.json`;
- `topology-wave-plan-input.v1.schema.json`;
- `topology-wave-append-request.v1.schema.json`;
- `topology-wave-append-receipt.v1.schema.json`;
- `topology-wave-current-read-request.v1.schema.json`;
- `topology-wave-current-read.v1.schema.json`;
- `topology-wave-list-request.v1.schema.json`;
- `topology-wave-list.v1.schema.json`;
- `fabric-operational-span.v1.schema.json`;
- generated TypeScript validators/types; and
- the same hash-bound schemas as explicit Python validator inputs.

There is no hand-written parallel route codec. Generation checks fail when any generated surface or schema digest
differs. The pre-release database baseline is updated in place; no predecessor table, decoder, import or compatibility
view is retained.

```sql
adapter_capability_snapshots(
  adapter_id, snapshot_generation, snapshot_id,
  adapter_contract_digest, host_id, host_version,
  source TEXT NOT NULL CHECK(source IN
    ('runtime-discovery','version-pinned-conformance','unavailable')),
  observed_at, expires_at, capability_body_digest,
  snapshot_json TEXT NOT NULL,
  capability_kind TEXT GENERATED ALWAYS AS
    (json_extract(snapshot_json, '$.capabilities.kind')) STORED NOT NULL,
  snapshot_digest, created_at,
  PRIMARY KEY(adapter_id, snapshot_generation),
  UNIQUE(snapshot_id), UNIQUE(snapshot_digest),
  UNIQUE(adapter_id, snapshot_generation, snapshot_digest,
    capability_body_digest),
  CHECK(capability_kind IN ('available','unavailable')),
  CHECK((source='unavailable' AND capability_kind='unavailable') OR
    (source IN ('runtime-discovery','version-pinned-conformance') AND
      capability_kind='available'))
)

adapter_capability_current(
  adapter_id PRIMARY KEY,
  snapshot_generation, snapshot_digest, capability_body_digest, revision,
  FOREIGN KEY(adapter_id, snapshot_generation, snapshot_digest,
      capability_body_digest)
    REFERENCES adapter_capability_snapshots(
      adapter_id, snapshot_generation, snapshot_digest,
      capability_body_digest)
)
```

`snapshot_json` byte-equals JCS of the closed the Agent Fabric contract object. Insert validates the exact stable body preimage/digest,
snapshot digest, contiguous positive generation, `expires_at > observed_at`, sorted unique catalogues and the activated
contract. Snapshot rows are insert-only. The single current-pointer row advances by generation/digest/revision CAS in
the activation transaction and additionally equality-checks the referenced row's digest. An expired or unavailable
snapshot remains immutable audit evidence but cannot be selected by admission.

Discovery surfaces and effective configurations are immutable daemon evidence:

```sql
discovery_surface_manifests(
  evidence_id, evidence_revision, artifact_path, artifact_digest,
  host_id, host_version, provider_profile, raw_native_mode,
  permission_profile_digest, manifest_json, manifest_digest, created_at,
  PRIMARY KEY(evidence_id, evidence_revision),
  UNIQUE(evidence_id, evidence_revision, manifest_digest),
  FOREIGN KEY(evidence_id, evidence_revision)
    REFERENCES artifacts(artifact_id, revision)
)

adapter_activation_subjects(
  adapter_id, activation_id, activation_revision,
  evidence_id, evidence_revision, created_at,
  PRIMARY KEY(adapter_id, activation_id, activation_revision),
  UNIQUE(evidence_id, evidence_revision),
  FOREIGN KEY(evidence_id, evidence_revision)
    REFERENCES artifacts(artifact_id, revision)
)

adapter_provider_smoke_subjects(
  adapter_id, smoke_id, action_adapter_id, action_id,
  evidence_id, evidence_revision, created_at,
  PRIMARY KEY(adapter_id, smoke_id),
  UNIQUE(action_adapter_id, action_id),
  UNIQUE(evidence_id, evidence_revision),
  CHECK(action_adapter_id = adapter_id),
  FOREIGN KEY(evidence_id, evidence_revision)
    REFERENCES artifacts(artifact_id, revision),
  FOREIGN KEY(action_adapter_id, action_id)
    REFERENCES provider_action_pair_preflights(adapter_id, action_id)
)

adapter_effective_configurations(
  configuration_id, configuration_revision,
  adapter_id TEXT NOT NULL, adapter_contract_digest NOT NULL,
  executable_identity_digest NOT NULL,
  capability_snapshot_generation, capability_snapshot_digest,
  capability_body_digest,
  subject_kind TEXT NOT NULL CHECK(subject_kind IN
    ('activation','provider-smoke','provider-action')),
  subject_ref_digest TEXT NOT NULL,
  subject_activation_id, subject_activation_revision, subject_smoke_id,
  subject_action_adapter_id, subject_action_id,
  activation_configuration_id, activation_configuration_revision,
  activation_configuration_digest,
  activation_configuration_subject_kind TEXT,
  requested_configuration_digest,
  effective_configuration_digest, permission_profile_digest,
  discovery_surface_evidence_id, discovery_surface_evidence_revision,
  discovery_surface_digest,
  evidence_id, evidence_revision,
  configuration_json, configuration_digest, created_at,
  PRIMARY KEY(configuration_id, configuration_revision),
  UNIQUE(configuration_id, configuration_revision, configuration_digest),
  UNIQUE(evidence_id, evidence_revision),
  UNIQUE(configuration_digest),
  UNIQUE(adapter_id,subject_kind,configuration_id,
    configuration_revision,configuration_digest,
    adapter_contract_digest,executable_identity_digest),
  UNIQUE(subject_action_adapter_id,subject_action_id,subject_kind,
    adapter_contract_digest,configuration_id,configuration_revision,
    configuration_digest,effective_configuration_digest,
    executable_identity_digest),
  UNIQUE(subject_action_adapter_id, subject_action_id,
    configuration_id, configuration_revision, configuration_digest,
    capability_body_digest, permission_profile_digest,
    discovery_surface_evidence_id, discovery_surface_evidence_revision,
    discovery_surface_digest),
  FOREIGN KEY(evidence_id, evidence_revision)
    REFERENCES artifacts(artifact_id, revision),
  FOREIGN KEY(adapter_id, capability_snapshot_generation,
      capability_snapshot_digest, capability_body_digest)
    REFERENCES adapter_capability_snapshots(
      adapter_id, snapshot_generation, snapshot_digest,
      capability_body_digest),
  FOREIGN KEY(discovery_surface_evidence_id,
      discovery_surface_evidence_revision, discovery_surface_digest)
    REFERENCES discovery_surface_manifests(
      evidence_id, evidence_revision, manifest_digest),
  FOREIGN KEY(adapter_id, subject_activation_id, subject_activation_revision)
    REFERENCES adapter_activation_subjects(
      adapter_id, activation_id, activation_revision),
  FOREIGN KEY(adapter_id, subject_smoke_id)
    REFERENCES adapter_provider_smoke_subjects(adapter_id, smoke_id),
  FOREIGN KEY(subject_action_adapter_id, subject_action_id)
    REFERENCES provider_action_pair_preflights(adapter_id, action_id),
  FOREIGN KEY(adapter_id,activation_configuration_subject_kind,
      activation_configuration_id,activation_configuration_revision,
      activation_configuration_digest,adapter_contract_digest,
      executable_identity_digest)
    REFERENCES adapter_effective_configurations(
      adapter_id,subject_kind,configuration_id,configuration_revision,
      configuration_digest,adapter_contract_digest,
      executable_identity_digest),
  CHECK(
    (subject_kind='activation' AND
      activation_configuration_id IS NULL AND
      activation_configuration_revision IS NULL AND
      activation_configuration_digest IS NULL AND
      activation_configuration_subject_kind IS NULL) OR
    (subject_kind IN ('provider-smoke','provider-action') AND
      activation_configuration_id IS NOT NULL AND
      activation_configuration_revision IS NOT NULL AND
      activation_configuration_digest IS NOT NULL AND
      activation_configuration_subject_kind='activation')),
  CHECK(
    (subject_kind='activation' AND subject_activation_id IS NOT NULL AND
      subject_activation_revision IS NOT NULL AND subject_smoke_id IS NULL AND
      subject_action_adapter_id IS NULL AND subject_action_id IS NULL) OR
    (subject_kind='provider-smoke' AND subject_activation_id IS NULL AND
      subject_activation_revision IS NULL AND subject_smoke_id IS NOT NULL AND
      subject_action_adapter_id IS NULL AND subject_action_id IS NULL) OR
    (subject_kind='provider-action' AND subject_activation_id IS NULL AND
      subject_activation_revision IS NULL AND subject_smoke_id IS NULL AND
      subject_action_adapter_id IS NOT NULL AND
      subject_action_adapter_id=adapter_id AND subject_action_id IS NOT NULL))
)

CREATE UNIQUE INDEX one_effective_configuration_per_activation_subject
  ON adapter_effective_configurations(
    adapter_id, subject_activation_id, subject_activation_revision)
  WHERE subject_kind='activation';
CREATE UNIQUE INDEX one_effective_configuration_per_smoke_subject
  ON adapter_effective_configurations(adapter_id, subject_smoke_id)
  WHERE subject_kind='provider-smoke';
CREATE UNIQUE INDEX one_effective_configuration_per_provider_action
  ON adapter_effective_configurations(
    subject_action_adapter_id, subject_action_id)
  WHERE subject_kind='provider-action';
```

Each discovery row composite-foreign-keys the exact existing `EvidenceArtifactRegistration` revision. Its
`manifest_json` byte-equals RFC 8785 JCS of the digest-free `discoverySurfaceManifestV1`; `manifest_digest`,
`artifact_digest` and the registered artifact digest are equal, and the exact registered bytes reproduce them. Triggers
equality-copy host/version/profile/ raw-mode and permission fields from the manifest. Only the daemon renderer may
insert this evidence kind.

The two subject tables are immutable identity/evidence registries, not new activation or action state machines; their
evidence tuples foreign-key exact daemon registrations. A provider-smoke/action pair preflight exists before its
subject/config row, so the later route-to-configuration FK creates no cycle.

Effective-configuration insert validates the closed the activation contract object and its digest. A closed discriminator CHECK requires
exactly the activation columns, smoke column, or provider-action pair columns for its `subject_kind`; every other
subject column is null. Adapter ID, subject kind and subject-ref digest are nonnull, and the provider-action arm
separately proves its action-adapter column nonnull before equality, so SQLite's NULL CHECK semantics cannot bypass an
arm or its partial index. The selected columns reproduce `subjectRef` and `subject_ref_digest` and must satisfy the
displayed foreign key. The three partial unique indexes make the selected ref—not a caller-selected value—the one-to-one
subject identity. A different configuration ID, revision or digest cannot create a second effective configuration for
the same subject. The nullable activation-configuration tuple is all null only for an activation subject; smoke/action
subjects require a same-adapter activation parent with the same adapter contract and executable identity, and cannot
update it. Subject arm/ref digest, executable, snapshot instance/body, permission and discovery-surface tuples must
reproduce the JSON. Each row is also registered through the existing daemon-owned evidence registration path; no public
publisher may forge its evidence kind. There is no host-global config mutation, compatibility decoder or update path.

Certifying-review availability/admission/dispatch additionally require the referenced capability body to state
`safety.enforcedReadOnly=true` and the effective permission profile to be the exact enforced read-only profile. Generic
routes instead enforce their own matched profile and may be write- capable inside task authority; no store trigger
globally rewrites them to read- only.

The existing `provider_action_routes` row gains non-null `capability_snapshot_generation`, `capability_snapshot_digest`,
`capability_body_digest`, `effective_configuration_id`, `effective_configuration_revision`,
`effective_configuration_ref_digest`, `requested_configuration_digest`, `effective_route_configuration_digest`,
`deployed_route_admission_json`, `deployed_route_admission_digest`, `route_policy_revision`, `harness_revision`,
`harness_digest`, `context_policy_revision`, `context_policy_digest`, `permission_profile_digest`,
`discovery_surface_evidence_id`, `discovery_surface_evidence_revision` and `discovery_surface_digest` for every new
answer-bearing action. Foreign keys bind the exact adapter/generation/digest.

The provider-route relation below is the one canonical current `provider_action_routes` owner. It includes these
capability, configuration, discovery-surface and admission fields and their exact parent keys; this section does not
declare a second patch-shaped route relation.

The explicit composite foreign key means a digest cannot cross another adapter/generation. Historical routes reference
immutable snapshots, never the mutable current pointer. The route-admission action pair equals the row primary key; its
admitted adapter, contract and snapshot instance/body equal the foreign row. Its discovery-surface ref foreign-keys
`discovery_surface_manifests` and the exact existing `EvidenceArtifactRegistration` revision/artifact digest.
Host/version/ profile/raw-mode, permission and manifest digest must equality-bind route, snapshot, launch and
registration; evidence kind is `discovery-surface.v1`, `publisherKind=fabric` and `producer=fabric-daemon`. The
effective-configuration foreign row must have `subject_kind='provider-action'` and its exact subject ref must equal the
route action pair; adapter/contract, snapshot body, permission and surface fields reproduce admission. Requested and
admitted arms are immutable.

Every provider-I/O attempt appends its actual point-of-use snapshot:

```sql
provider_action_route_dispatches(
  adapter_id, action_id, dispatch_ordinal, admission_digest,
  capability_snapshot_generation, capability_snapshot_digest,
  capability_body_digest,
  effective_configuration_id, effective_configuration_revision,
  effective_configuration_ref_digest, permission_profile_digest,
  discovery_surface_evidence_id, discovery_surface_evidence_revision,
  discovery_surface_digest, dispatched_at, dispatch_json, dispatch_digest,
  PRIMARY KEY(adapter_id, action_id, dispatch_ordinal),
  UNIQUE(dispatch_digest),
  FOREIGN KEY(adapter_id, action_id, admission_digest,
      capability_body_digest, effective_configuration_id,
      effective_configuration_revision, effective_configuration_ref_digest,
      permission_profile_digest, discovery_surface_evidence_id,
      discovery_surface_evidence_revision, discovery_surface_digest)
    REFERENCES provider_action_routes(
      adapter_id, action_id, deployed_route_admission_digest,
      capability_body_digest, effective_configuration_id,
      effective_configuration_revision, effective_configuration_ref_digest,
      permission_profile_digest, discovery_surface_evidence_id,
      discovery_surface_evidence_revision, discovery_surface_digest),
  FOREIGN KEY(adapter_id, capability_snapshot_generation,
      capability_snapshot_digest, capability_body_digest)
    REFERENCES adapter_capability_snapshots(
      adapter_id, snapshot_generation, snapshot_digest,
      capability_body_digest),
  FOREIGN KEY(effective_configuration_id, effective_configuration_revision,
      effective_configuration_ref_digest)
    REFERENCES adapter_effective_configurations(
      configuration_id, configuration_revision, configuration_digest),
  FOREIGN KEY(discovery_surface_evidence_id,
      discovery_surface_evidence_revision, discovery_surface_digest)
    REFERENCES discovery_surface_manifests(
      evidence_id, evidence_revision, manifest_digest)
)
```

Ordinals are contiguous and rows insert immediately before their provider I/O. The snapshot must be current and
unexpired at insertion, but may be a newer instance than admission only when its body digest and adapter/contract/host
are identical. Effective-configuration, permission and surface tuples remain admission-equal and the effective row must
still reproduce all of them. The public capability summary joins admission and latest dispatch snapshots separately,
including each one's source and clocks; no clock is copied between arms.

Terminal observation is append-only and separate:

```sql
provider_action_route_observations(
  adapter_id, action_id, admission_digest,
  observation_json, observation_digest, observed_at,
  PRIMARY KEY(adapter_id, action_id), UNIQUE(observation_digest),
  UNIQUE(adapter_id,action_id,admission_digest,observation_digest),
  FOREIGN KEY(adapter_id, action_id, admission_digest)
    REFERENCES provider_action_routes(
      adapter_id, action_id, deployed_route_admission_digest)
)
```

Insert equality-checks the parent action/admission digest and every closed field-evidence union. A missing provider
field persists its explicit unavailable arm rather than admission data. No update, replacement or recomputed admission
digest is legal. Public reads left-join zero or one observation and expose both immutable digests.

Certifying review evidence additionally stores nullable `route_observation_digest` and `actual_route_identity_digest`.
The latter can be nonnull only when observation endpoint provider/family/model are proved by provider result or
contract-defined adapter attestation; equality is evaluated separately against admission and resolved profile
requirements. Every other observed route arm is also equality-checked against admission; unavailable is honest but
proved inequality is mismatch. Missing proof and mismatch retain safe adverse findings but accept no resolution and
persist the respective closed blocker. Generic provider actions bypass this certification-only test.

Admission and dispatch use this order:

1. classify exact command/action-pair replay and create/attach the canonical pair preflight before any route/config
   subject;
2. for certifying work, reserve finding capacity before router I/O by inserting the finding-capacity row in `preflight`;
   the pre-router finding-capacity reservation keeps its attempt generation null until that admission transaction;
3. run the bounded pure resolver against explicit pinned inputs; its output is only the candidate receipt;
4. in one transaction validate authority/budget plus the current unexpired capability instance/body, adapter
   contract/host, model, raw effort, raw native mode, per-action effective configuration, permission profile,
   context-policy revision/digest and harness revision/digest plus discovery-surface registration/digest; insert the
   provider-action effective configuration; for certifying work, attach the existing finding-capacity reservation by
   assigning its positive attempt generation; insert every remaining authority and budget parent; insert the canonical
   provider action; insert its route last;
5. immediately before initial provider I/O or a permitted no-effect retry, read the current unexpired snapshot, require
   admitted body/contract/host/model/ effort/mode plus fixed effective-configuration/permission/harness/context/
   surface/route equality, and append the exact dispatch snapshot row; an instance-only refresh with equal body
   proceeds;
6. on body/permission/surface or other pre-effect drift, terminalise/supersede the zero-effect action and resolve afresh
   under a new action pair. After ambiguous effect, retain the original route and invoke only its existing pair-keyed
   recovery owner.

The reservation attaches to `provider_actions`, whose
`finding_capacity_reservation_digest` foreign key names the exact reservation.
The reservation state-update guard requires that matching action before the
`preflight -> attached` transition. Routes attach to their action; they do not
own a second reservation attachment or reservation-state trigger.

The pure resolver never persists, performs provider/network I/O or becomes the route owner. Its existing five-second
process-group TERM/KILL boundary remains binding. The daemon persistence wrapper is not callable as a resolver.

Topology waves use one append-only store and one current pointer:

The authority foreign key below depends on the exact four-column parent `UNIQUE(project_session_id, coordination_run_id,
authority_revision, authority_ref)` already declared by the current baseline for the squashed baseline.

```sql
topology_wave_plans(
  project_session_id, coordination_run_id, task_id,
  wave_id, wave_revision,
  predecessor_wave_id, predecessor_wave_revision, predecessor_plan_digest,
  chair_agent_id, principal_generation, chair_lease_generation,
  authority_revision, authority_ref, authority_digest,
  policy_revision, policy_ref, policy_digest,
  rationale_evidence_id, rationale_evidence_revision,
  state, plan_json, plan_digest, created_at,
  PRIMARY KEY(project_session_id, coordination_run_id, task_id,
    wave_id, wave_revision),
  UNIQUE(project_session_id, coordination_run_id, task_id,
    wave_id, wave_revision, plan_digest),
  UNIQUE(plan_digest),
  FOREIGN KEY(rationale_evidence_id, rationale_evidence_revision)
    REFERENCES artifacts(artifact_id, revision),
  FOREIGN KEY(coordination_run_id, task_id)
    REFERENCES tasks(run_id, task_id),
  FOREIGN KEY(coordination_run_id, chair_agent_id)
    REFERENCES agents(run_id, agent_id),
  FOREIGN KEY(project_session_id, coordination_run_id,
      authority_revision, authority_ref)
    REFERENCES run_authority_revisions(
      project_session_id, coordination_run_id,
      authority_revision, authority_ref),
  FOREIGN KEY(project_session_id, coordination_run_id, task_id,
      predecessor_wave_id, predecessor_wave_revision,
      predecessor_plan_digest)
    REFERENCES topology_wave_plans(
      project_session_id, coordination_run_id, task_id,
      wave_id, wave_revision, plan_digest)
)

topology_wave_current(
  project_session_id, coordination_run_id, task_id,
  wave_id, wave_revision, plan_digest, revision,
  PRIMARY KEY(project_session_id, coordination_run_id, task_id),
  FOREIGN KEY(project_session_id, coordination_run_id, task_id,
      wave_id, wave_revision, plan_digest)
    REFERENCES topology_wave_plans(
      project_session_id, coordination_run_id, task_id,
      wave_id, wave_revision, plan_digest)
)

topology_wave_append_receipts(
  command_id PRIMARY KEY, request_digest, actor_principal_digest,
  project_session_id, coordination_run_id, task_id,
  prior_wave_id, prior_wave_revision, prior_plan_digest,
  wave_id, wave_revision, plan_digest, pointer_revision,
  receipt_json, receipt_digest, created_at,
  UNIQUE(receipt_digest),
  FOREIGN KEY(project_session_id, coordination_run_id, task_id,
      wave_id, wave_revision, plan_digest)
    REFERENCES topology_wave_plans(
      project_session_id, coordination_run_id, task_id,
      wave_id, wave_revision, plan_digest)
)
```

`plan_json` validates the closed the Agent Fabric contract object; scalar columns equality-copy it and the plan digest is exact JCS.
Nested triggers validate canonical order, dependency/decomposability, topology, chair, stage owners, write partitions,
contention, budgets and stop conditions. The rationale tuple foreign-keys the exact existing evidence registration. A
revision is immutable; any rationale or state change appends the next contiguous revision. The current pointer advances
by exact CAS. Predecessor refs foreign-key the exact earlier plan tuple, and authority/policy/chair/dependency currency
is checked at append and derived again at read. Read currency treats a missing/noncontiguous/digest-invalid predecessor
chain as stale; it never requires the historical predecessor itself to remain the current pointer.
`fabric.v1.topology-wave.append` authenticates the current chair, derives predecessor as null only for the zero-pointer
arm or as the exact expected/current plan ref, plus all plan-owned identity/authority/policy/time/ digest fields, and
commits plan, pointer and immutable receipt together. Exact command replay by request digest returns the receipt before
live checks; changed replay conflicts. Current/list operations map only the discriminated the Agent Fabric contract projection and
ordinary scoped page envelope; missing pointer is unavailable/null and an existing pointed plan is always the nonnull
current or stale arm. No row grants authority, automatically chooses a topology or creates a second chair/policy state
machine.

Context pressure is operational state, not spend or authority budget. Existing provider generation/context-revision
telemetry remains append-only and lifecycle-owned as specified in the lifecycle-custody and rotation contracts. The baseline
`agent_adapter_bindings` relation adds exact `UNIQUE(run_id, agent_id, adapter_id)` beside its existing
`(run_id,agent_id)` primary key. The separate truthful projection foreign-keys that exact active agent/adapter identity:

```sql
provider_context_pressure_current(
  run_id, agent_id, adapter_id, provider_generation,
  context_revision, observation_source_event_id,
  pressure, source, confidence,
  window_tokens, used_tokens, remaining_tokens,
  observed_at, expires_at, evidence_digest, revision,
  PRIMARY KEY(run_id, agent_id),
  FOREIGN KEY(run_id, agent_id, adapter_id)
    REFERENCES agent_adapter_bindings(run_id, agent_id, adapter_id),
  FOREIGN KEY(run_id, agent_id, observation_source_event_id,
      provider_generation, context_revision, evidence_digest)
    REFERENCES provider_context_observation_audit(
      run_id, agent_id, source_event_id, provider_generation,
      context_revision, evidence_digest),
  CHECK(pressure IN ('low','medium','high','unknown')),
  CHECK(source IN ('native-exact','native-estimated','hook-boundary','unavailable')),
  CHECK(confidence IN ('exact','estimated','unknown')),
  CHECK(expires_at > observed_at),
  CHECK(source != 'unavailable' OR
    (pressure='unknown' AND confidence='unknown' AND window_tokens IS NULL AND
     used_tokens IS NULL AND remaining_tokens IS NULL)),
  CHECK(source != 'native-exact' OR
    (confidence='exact' AND window_tokens IS NOT NULL AND
     used_tokens IS NOT NULL AND remaining_tokens IS NOT NULL AND
     used_tokens + remaining_tokens = window_tokens)),
  CHECK(source != 'native-estimated' OR
    (confidence='estimated' AND window_tokens IS NOT NULL AND
     used_tokens IS NOT NULL AND remaining_tokens IS NOT NULL AND
     used_tokens + remaining_tokens = window_tokens)),
  CHECK(source != 'hook-boundary' OR
    (confidence IN ('exact','estimated') AND
     ((window_tokens IS NULL AND used_tokens IS NULL AND remaining_tokens IS NULL) OR
      (window_tokens IS NOT NULL AND used_tokens IS NOT NULL AND
       remaining_tokens IS NOT NULL AND
       used_tokens + remaining_tokens = window_tokens)))),
  CHECK(confidence != 'unknown' OR pressure='unknown')
)
```

Token fields are nullable nonnegative integers and satisfy the displayed closed source/confidence/nullability/arithmetic
checks. `pressure='unknown'` whenever the current-window basis cannot be proved. Cumulative provider usage cannot
populate current-window pressure unless the adapter contract defines it as such. No row reserves, consumes or releases
provider budget. Observation update CASes the same provider-generation/context-revision ordering already owned by
lifecycle; lower/reordered input is audit-only and cannot regress the projection or infer principal/bridge generations.

Adapter adoption starts the `BEGIN IMMEDIATE` adoption transaction before reading the current binding or pressure
projection. It captures the complete current pressure row, including its provider generation, context revision, evidence
digest and projection revision. When a row was captured, the daemon compare-and-deletes that exact row and requires
exactly one deletion; when no row was captured, it requires the row to remain absent. Only then may the same transaction
change the binding's adapter identity. A mismatch or crossed row aborts the transaction, and rollback after either the
clear or binding update restores the prior binding and pressure row together.

The displayed composite foreign key makes direct writes fail closed: an adapter-identity UPDATE or binding DELETE
aborts while its current pressure row remains. Provider-generation, context-revision and binding-revision advances
that retain the adapter identity remain legal. This adoption step removes only the obsolete current projection. It
creates no pressure history, re-keyed pressure row or synthetic unknown observation.

`fabric.v1.provider-context-pressure.read` and the negotiated scoped operator System projection map this row exactly to
the Agent Fabric contract `providerContextPressureV1`. The row's adapter and composite observation-audit foreign key populate the
corresponding wire fields. Read snapshot time derives the discriminated the Agent Fabric contract current/stale nonnull or
unavailable-null arm and `ageSeconds` from `observed_at/expires_at` without a write; no stored or projected percentage
exists. Missing row projects unavailable, not zero pressure.

Automatic pressure thresholds, hysteresis, maximum compaction counts and successor selection are absent. Existing
explicit lifecycle custody remains the only rotation/compaction mutator, preserves fresh-rotation versus same-history
recovery, and keeps parent/child custody independent.

Operational spans are append-only, bounded and content-free. Export validates the Agent Fabric contract codec and rejects prompts,
answers, tool arguments/results, artifact bytes, private messages, capabilities and absolute paths rather than redacting
after persistence. Generic span export never satisfies receipt, authority, review, disclosure or gate evidence.

Verification adds schema-generation parity; discovery manifest/artifact digest equality and exact registration-revision
foreign keys; capability current- pointer, expiry and same-body refresh races; effective-configuration
subject/activation lineage; raw/normalised effort and duplicate-subject rejection by all three partial unique indexes;
native-mode round-trip; actual review-route proof and every observed route- field mismatch; exact cut-custody ref joins;
point-of-use body/ permission/surface drift before effect; ambiguous-effect non-rerouting; honest observed unknown;
topology append/CAS/stale/authority joins; context-pressure/ budget separation, composite observation join,
discriminated stale read, crossed-arm rejection and no percentage; lower/reordered observation; and telemetry
content-denial fixtures. Full crash matrices prove there is one route owner and that lifecycle recovery remains ahead of
generic provider recovery.

The generated baseline schema audit prepares every relation under `PRAGMA foreign_keys=ON`, runs `PRAGMA
foreign_key_check`, and inserts negative fixtures for crossed artifact revision, agent adapter, topology task/chair,
rationale registration and route discovery-surface identities. No child relies on trigger prose where the displayed
composite foreign key can enforce the relationship.

## Exact Console read persistence and daemon owner

The daemon is the sole owner of the Console read-identity surface. It reuses the current baseline, the existing operator
snapshot revision and existing route and preparation codecs. No Console database, materialised route copy,
action-ID-only lookup, legacy projection or migration shim is permitted.

`review-target-preparation.current.read` authenticates the point-of-use operator credential for the exact
project/session/run, proves the credential's project ID, then reads `review_target_preparation_high_water`. An existing
run with no high-water row, or generation zero with no preparation row, maps to unavailable. A missing or zero high
water while any preparation row exists for that run is integrity failure, as is any unequal preparation/target/bundle
high-water triple. A positive equal triple must equal the run's greatest stored preparation generation, have exactly one
matching row, and equal that row's reserved target and bundle generations. A NULL, negative or otherwise out-of- domain
high-water or preparation generation is always integrity failure; no aggregate may hide it. The same read transaction
first rejects invalid-domain rows, then compares all three high waters, `MAX(preparation_generation)` and the matching
row. Both active and terminal rows are eligible; state is not a locator filter. The existing per-ID read mapper produces
the nested value so phase, progress and terminal correlation cannot drift. The wrapper generation equals the
high-water/row generation, while the accepted receipt reproduces the exact session/run and preparation ID. Operation
failures use the existing closed `reviewTargetPreparationReadErrorV1` codec unchanged.

Stable route-list membership uses an allocation ordinal rather than the
daemon-global projection revision. The allocation-ordinal relations are not in
the shipped baseline; `provider_route_list_high_water` and the `route_ordinal`
and `route_listed_at` columns do not exist. The requirement that list
membership be stable under concurrent allocation stands, and the mechanism is
recoverable from Git history.

Were that mechanism built, these requirements would bind it. Admission must
allocate the ordinal, keep the run high water equal to the greatest allocated
ordinal, and copy that positive ordinal to action and route in one transaction.
Ordinals must never recycle, and a resolver or preflight failure that creates no
action must allocate none. The provider action must survive legitimate route
missing or integrity recovery, because it, not the route, owns list membership
and the attached reservation. Task-bound action rows must never be deletable,
and their run, task, ordinal and listing timestamp must be immutable. A route
must bind a reservation that is `attached` at admission, and a later
`attached -> settled` transition must neither rewrite the immutable action or
route nor invalidate its foreign key.

Two requirements below do hold against the shipped schema. Every route,
dispatch, observation or recovery-state advance increments the owning action's
`journal_revision`, which the read wrapper exposes as `routeRevision`. No route
bytes or freshness label is copied into another store.

None of the supporting indexes formerly listed here exist in the shipped
baseline. The requirement that route-list and preparation lookups use bounded
indexed paths stands and is evidenced by the persistence invariant tests.

The route read starts from exact `(adapter_id,action_id)` in `provider_actions`, then equality-joins task/run/requested
session and left- joins the pair-keyed route, dispatch, observation and live route-recovery owner. An exact action pair
that is not an answer-bearing route-list member returns `NOT_FOUND`; its lack of route/recovery is legitimate. An intact
listed row maps through the existing full `PROVIDER_ROUTE_V1_CODEC`. A recovery-owned missing/integrity-failed state
maps the null route/evidence arm and exposes the action's listing timestamp as wrapper `createdAt`. No route plus no
exact recovery evidence is an operation integrity error, not an invented missing arm. Every child is pair-keyed; no caller-stamped adapter ID and no action-only query may
participate. Crossed parents are scope or integrity errors, never partial route objects.

The route list operation and its ordinal-bound paging are not implemented; the watermark, cursor and contiguity-proof
mechanism formerly specified here depended on relations the baseline does not contain. These requirements bind whatever
implements it. Every page must scan at most 256 consecutive unfiltered members strictly after the cursor's last-scanned
tuple and at or below the watermark. It must classify each scanned member through either the canonical present route or
the composite-FK-bound recovery arm before applying any nullable predicate, and any orphaned, crossed or unparseable
member must fail the whole list with `INTEGRITY_FAILURE`. Target and slot filters must join either the immutable
certifying route fields or the exact daemon-derived recovery-custody tuple; they must never trust a route whose
integrity failed, and must never silently exclude an unclassifiable member.

Operator projection source queries are likewise exactly scoped. Work, Agent and Activity rows join `projects ->
project_sessions -> runs -> tasks|agents`; source rows, summary builders, detail references and detail readers carry the
same project/ session/run/local-ID tuple. Activity message-body refs and reads equality-carry that tuple, and embedded
task/agent IDs inherit it. Evidence derives the closed project/session/run scope arm from its actual nullable
registration columns and always includes project ID; nonnull Evidence task ID requires the run arm. It never flattens on
`evidence_id` alone, never invents a run for a project file or private Git diff, and never drops those approved Evidence
rows. The existing projection transaction constructs the Console read-identity composite ID with the pinned view prefix and
rejects duplicate item IDs before publication. Detail reads equality-check outer scope, detail-ref scope and source row
at the requested snapshot. Run-local IDs reused under another run therefore coexist without collision and cannot
cross-select. Work source pages order by `(project_id,project_session_id,run_id,task_id)` and Agent source pages by
`(project_id,project_session_id,run_id,agent_id)`. The existing numeric cursor is the position in that exact snapshot
order; local-ID-only ordering is forbidden. Activity pages retain reverse source revision and total tie-break by
`(source_revision DESC,project_id,project_session_id,run_id,event_id)`.

The operation registry declares all three as operator-only under `console-read-identity.v1`. The current Console
requires that feature and the 1,048,576-byte frame maximum during initialize; absence is incompatible, not a legacy
fallback. When the daemon's offered registry contains the feature, current-project operator `read` credential
provisioning shall preissue all three exact operation names; initialize never mints them. Initialize intersects those
preissued operations with the negotiated required feature and operator principal, and current-Console initialize fails
incompatible unless the resulting `allowedOperations` contains all three. Every request carries that credential and
project ID; point-of-use authentication revalidates project authority generation, active seat, project/session/run,
principal generation, operation subset and expiry. These reads do not require or mint chair authority. The private
control protocol exposes no new mutation or filesystem path.

Implementation is TDD. Database fixtures deliberately reuse task and agent IDs across two runs. Distinct Activity rows
in both runs prove row/summary/detail/ message-body reads must stay in the exact run. Evidence retains its globally
unique artifact ID while a cross-scope detail request must fail for the right reason. Tests cover absent/ zero/positive,
NULL/negative, crossed-triple and lagging high water, NULL/ negative or crossed reserved preparation generations, active
and every terminal preparation state, pair- keyed route reads, declared columns and conjunctive indexes, ordinal
allocation and non-reuse, continuous-append page progress, generic versus certifying filters, missing/integrity-failed
recovery arms, cursor/filter/principal/ watermark substitution, expired capability freshness, action journal revision on
every route child change, closed error/digest arms, maximal digest item IDs, maximal single-route frame fit, worst-case
8-route page fit and a no- action-ID-only-query source assertion. Frame limits use a maximal 1,024-byte request/result
cursor. Negative route-arm fixtures reject null and half-null certifying identity plus crossed/null/mutated recovery
custody and crossed present-route target/slot/attempt/reservation custody, plus direct-SQL null high-water/route
identity. Attempt-allocation fixtures reject gap, reuse, crossed capacity target/slot, null owner/state, nonnull
preflight/ released state and split slot-head/reservation/ action/route admission commit. An interior ordinal-gap
fixture on a later page fails before task, target or slot filtering; crash/restart tests prove admitted action
membership cannot be deleted or renumbered. An exact non-answer-bearing action-pair read returns `NOT_FOUND`, while only
a nonnull-ordinal member lacking both route and recovery fails integrity. Target/slot filters cannot hide that orphan,
and multi-page Work/Agent/Activity fixtures with reused local IDs prove total-order pagination without gaps or replay. A
selective-filter load fixture accepts empty progress pages, scans no more than 256 members per page and classifies every
ordinal at most once. The zero-watermark fixture returns an empty page with null cursor immediately. Initialize fixtures
reject a missing feature, each missing preissued or intersected operation and a narrowed frame maximum; the positive arm
returns all three, and a wrong-reason negative proves initialize never adds an operation to the credential.
Protocol/daemon contract tests prove the nested preparation and present route values are produced by their existing
codecs. Full migration generation, foreign-key check, schema, runtime, evaluation and load gates remain binding.
