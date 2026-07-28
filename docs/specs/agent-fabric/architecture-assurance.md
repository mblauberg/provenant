# Agent Fabric architecture assurance

## Decision and relationship to existing specs

Implement the accepted evidence-backed operational hardening without reopening the accepted architecture in the existing Agent Fabric, harness lifecycle and activation contracts.
Superseded review transcripts and adjudication notes are run evidence, not durable research owners.

- the Agent Fabric contract remains the fabric behaviour contract.
- the harness lifecycle contract remains the domain-neutral delivery lifecycle and explicitly does not reimplement the fabric.
- the activation contract remains the adapter activation and local-operations contract.
- This spec owns operational bounds, portability, deterministic exports, database enforcement and repository assurance.

The human's instruction to review the source, select the exact design and implement it entirely is the implementation
authority for this spec. It does not authorise Git push, public release, remote listeners, provider login changes,
destructive pruning or production promotion.

## Outcome

Turn the activated local fabric into a bounded, versioned, migratable and reproducibly tested shared system that can be
safely admitted to exact project roots. Preserve one daemon, one SQLite transaction owner, provider-neutral MCP access
and Herdr's non-authoritative visibility role.

Because the system has not reached its first accepted release, hardening targets the current baseline rather than
historical installation compatibility. The checked-in schema is squashed to one fresh-state migration. Startup against
any earlier migration registry or unknown schema fingerprint returns a typed cutover-required failure before mutation.
It leaves the database and filesystem evidence untouched. Preflight/backfill code, fixtures and fallback branches whose
only purpose is importing those earlier shapes shall be removed.

Protocol initialization still rejects mismatched peers, but it does not retry an old profile or translate old result
shapes. Independently optional current features continue to use exact negotiation. The adapter compatibility registry
pins external executable, package and schema artifacts by hash; repository-owned wrapper code carries Git provenance
(repository commit plus tracked wrapper path) instead of hash manifests.

## Required behaviour

## Repository assurance

1. Pull-request CI runs the Python/harness gate and the full Node fabric gate: clean install, typecheck,
   unit/integration/acceptance tests, build, evaluation, load and production dependency audit.
2. GitHub Actions use immutable commit SHAs with least-privilege workflow permissions.
3. CODEOWNERS, dependency-update policy and a PR evidence template cover the fabric, migrations, schemas, routing and
   security-sensitive configuration.
4. A deterministic repository test rejects mutable Action references or a CI workflow that omits either harness or
   fabric gates.

## Bounded, versioned local protocol

1. One shared bounded-NDJSON module owns incremental UTF-8 framing for daemon, client and adapter-server transports. It
   rejects oversized or malformed frames before unbounded buffering.
2. Trusted limits cover frame bytes, simultaneous daemon connections, per-connection in-flight commands, client pending
   calls, adapter in-flight requests and idle/deadline behaviour. Projects may narrow but not widen global maxima.
3. Daemon connections perform an `initialize` handshake before ordinary methods. The response identifies protocol
   version, daemon version, capabilities and effective limits. Unsupported versions and pre-handshake commands fail
   closed.
4. Response writes respect stream backpressure. Overload returns typed errors; it does not start more work and hope the
   process survives.
5. Bootstrap authority remains limited after initialisation to exact-root local operator provisioning and current
   private-control discovery. It cannot create a run, is never accepted on the public operator protocol and cannot
   perform project-session, gate, Git or provider actions.

## Exact workspace trust

1. Machine-local trust lives under the private fabric state directory, never in Git. Each entry records canonical root,
   approval time, optional expiry and allowed execution profiles.
2. `workspace trust`, `inspect`, `list` and `revoke` are explicit operator CLI actions. Symlinks, ancestor broadening,
   `$HOME`-wide trust and malformed or expired entries fail closed.
3. The portable configuration defines the maximum policy. The local registry admits exact additional roots; project and
   run authority only narrow them.
4. Trust changes use private files, atomic replace and deterministic metadata; bearer capabilities are never stored in
   the registry.

## Database integrity and maintenance

1. `0001-current-baseline.sql` creates the complete current schema from an absent database path. A checked-in manifest
   binds its file digest and canonical SQLite catalogue digest.
2. Any pre-existing path is inspected read-only first. Empty, non-SQLite, earlier, future, missing-metadata or
   catalogue-mismatched state returns `SCHEMA_CUTOVER_REQUIRED` before permission, WAL, marker, socket or sidecar
   mutation.
3. Exact current state may reopen read/write. Startup runs bounded integrity/foreign-key checks after an unclean marker.
   Long-lived connections run documented `PRAGMA optimize` maintenance.
4. Query-plan tests prove the mailbox, task-owner/state, lease-expiry, event-cursor and unresolved-provider-action paths
   use intended indexes.
5. A central invariant catalogue maps every enforced invariant to tests.

## Retention and archive controls

1. `retention status` and `retention preview` classify terminal-run data and report what a project policy could archive
   or prune.
2. `archive` produces a hash-bound, non-destructive coordination snapshot.
3. This spec does not implement automatic deletion. Any future `apply` command, duration defaults or legal-hold
   semantics require a separate human-approved policy and destructive-action gate.
4. Unknown files, active/quarantined runs, provider-native sessions, capabilities and substantive project artifacts are
   never deletion candidates.

## Deterministic current receipt

1. The canonical snapshot contains committed state only and is byte-identical across repeated exports of unchanged
   state.
2. Export metadata is separate from the hashable snapshot.
3. `taskOwners` replaces the false `stageOwners` label. Deliveries are counted by explicit state; no total-row count is
   called delivered.
4. The snapshot includes an event watermark and state hash. Nested structures are closed and versioned.
5. Schema version 2 is the sole current fabric-receipt schema. The runtime has no v1 decoder, importer, projection or
   compatibility fixture. An older file is preserved as an unknown user artifact but is not protocol evidence. Immutable
   schema-v2 receipt history is never rewritten.
6. Export uses bounded two-phase publication. Phase A fixes the database watermark/owner revisions plus exact Git
   HEAD/tree/index/worktree and registered external source/evidence currency tokens and writes a private candidate.
   Phase B equality-rechecks database revisions and reruns fixed no- follow external reads before atomic publication.
   Drift discards and retries or fails; a receipt cannot publish review completion against stale bytes.

## Machine status and documentation

1. `agent-fabric status --json` reports daemon reachability, protocol, configured/active adapters, trusted roots and
   current project seat metadata without capability values.
2. `agent-fabric doctor --json` verifies configuration, compatibility pins, state permissions, database checks and
   socket ownership with typed results.
3. Repository documentation describes expected setup. Current workstation run IDs, project keys, expiry and pane IDs
   come from status output, not committed prose.

## Incremental modularity and testing

1. `Fabric` remains the current coordination façade and sole cross-domain transaction owner. This programme extracts
   only stable seams created by the changes: wire framing/negotiation, workspace trust, retention/archive, database
   maintenance and receipt snapshot projection.
2. No network microservices, parallel mutation owners or second authority store are introduced.
3. Deterministic tests cover oversized frames, connection/in-flight overload, pre-handshake methods, mixed versions,
   backpressure, trust symlink/expiry, invalid current rows, baseline atomicity and preservation, query plans, repeated
   receipt export, archive integrity and daemon restart.
4. Multiple targeted lenses and the other-primary review must report no unresolved P0–P2 findings before human acceptance; terminal pressure follows `HARNESS.md`.

## Explicit rejections

- The model router does not decide topology; the accountable chair and `orchestrate` do.
- No weighted quality/cost scoring without a calibrated evaluation proving the factors predict outcomes.
- No second canonical skill registry; the active skill-portfolio effort owns skill governance.
- No automatic evidence deletion or age-implied authority.
- No mandatory autonomous self-halt that contradicts a human `until STOP` contract; authority expiry and bounded retries
  still fail safe.
- No provider rollback, A2A gateway, remote listener, external dashboard or daemon microservices.
- No wholesale rewrite of `Fabric` or all protocol surfaces in one change.

## Implementation sequence and ownership

| Package | Owner | Depends on | Write scope |
| --- | --- | --- | --- |
| WP1 CI and repository policy | CI worker | Spec | `.github/`, CI policy tests |
| WP2 bounded wire protocol | Protocol worker | Spec limits | transport, daemon/adapter server, protocol tests |
| WP3 SQLite integrity | Persistence worker | Spec invariants | current baseline, persistence, catalogue/query-plan tests |
| WP4 trust/status/retention | Operations worker | wire status contract | CLI/application modules and focused tests |
| WP5 current receipt | Receipt worker | event watermark | exports/schemas and focused tests |
| WP6 serial integration | Chair | WP1–WP5 | shared configuration, façade wiring, docs, receipts |
| WP7 independent verification | read-only reviewers | integrated tree | findings only |

No two workers edit the same source surface. Cross-cutting application is serial through the chair.

## Defaults and hard maxima

Initial global maxima are conservative local-process limits:

```yaml
protocol:
  version: 1
  maximumFrameBytes: 1048576
  maximumConnections: 32
  maximumInFlightPerConnection: 16
  maximumTotalInFlight: 128
  maximumClientPending: 32
  maximumAdapterInFlight: 8
  idleTimeoutMs: 300000
```

These are operational safety bounds, not throughput targets. Load evidence may support a later human-approved change.

## Cutover and rollback

1. There is no in-place pre-release database migration. A pre-existing path is evidence and is inspected read-only
   against the exact current manifest.
2. Fresh initialization writes a private temporary database, installs the complete baseline transactionally, runs
   `foreign_key_check`, `quick_check` and catalogue assertions, fsyncs, then publishes without overwrite.
3. Failure leaves either no final path or one complete current database. Existing incompatible state and its sidecars,
   mode, timestamps and directory entries remain unchanged.
4. Protocol clients and daemon are deployed together locally. Version mismatch fails closed; there is no downgrade or
   translation path.
5. Workspace trust and archive metadata are independent private files and can be revoked without database mutation.
6. Receipt consumers and producers use schema version 2 together. Any other version fails closed without translation or
   import.

## Acceptance

- Full runtime and harness gates pass from a clean install-compatible state.
- CI-policy tests prove immutable Actions and complete fabric coverage.
- All resource, protocol, trust, database, retention, receipt and status acceptance tests pass.
- The existing five adapter smokes and five-seat MCP health/round-trip remain green after hardening.
- the activation contract rollback to coordination-only remains possible.
- Targeted OpenAI lenses and an independent Anthropic other-primary review are clean at P0–P2; available distinct-family pressure is recorded when used.
- Canonical delivery receipt validates at `awaiting_acceptance`.
- Human final acceptance remains pending; Git push and release remain separate gates.

## Frozen review-bundle schema

~~~sql
review_target_preparation_high_water(
  run_id PRIMARY KEY,
  preparation_generation INTEGER NOT NULL CHECK(preparation_generation >= 0),
  target_generation INTEGER NOT NULL CHECK(target_generation >= 0),
  bundle_generation INTEGER NOT NULL CHECK(bundle_generation >= 0),
  revision INTEGER NOT NULL CHECK(revision >= 1),
  CHECK(preparation_generation = target_generation),
  CHECK(target_generation = bundle_generation)
)

review_target_preparations(
  run_id, preparation_id,
  preparation_generation INTEGER NOT NULL CHECK(preparation_generation >= 1),
  owner_command_id, semantic_input_digest, full_input_digest,
  actor_principal_digest, task_id, expected_target_generation,
  delivery_manifest_artifact_id,
  delivery_manifest_artifact_revision INTEGER NOT NULL
    CHECK(delivery_manifest_artifact_revision >= 1),
  reserved_target_generation INTEGER NOT NULL
    CHECK(reserved_target_generation >= 1),
  reserved_bundle_generation INTEGER NOT NULL
    CHECK(reserved_bundle_generation >= 1),
  state, revision INTEGER NOT NULL CHECK(revision >= 1),
  worker_claim_generation INTEGER NOT NULL
    CHECK(worker_claim_generation >= 0), worker_instance_id,
  worker_lease_expires_at, captured_precondition_digest,
  progress_kind, progress_plan_digest, progress_total, progress_completed,
  built_bundle_digest, built_manifest_root_digest,
  terminal_kind, terminal_code, terminal_evidence_digest, target_generation,
  accepted_receipt_digest, created_at, updated_at,
  PRIMARY KEY(run_id, preparation_id),
  UNIQUE(run_id, preparation_generation),
  UNIQUE(run_id, reserved_target_generation),
  UNIQUE(run_id, reserved_bundle_generation),
  UNIQUE(owner_command_id),
  CHECK(preparation_generation = reserved_target_generation),
  CHECK(reserved_target_generation = reserved_bundle_generation)
)
CREATE UNIQUE INDEX one_active_review_target_preparation_per_run
  ON review_target_preparations(run_id)
  WHERE state IN ('prepared','building','built');

review_bundles(
  run_id, bundle_generation, delivery_run_id,
  review_basis_revision, review_basis_digest,
  delivery_artifact_id, delivery_artifact_revision,
  base_object_id, head_object_id, head_tree_id, index_tree_id,
  review_diff_codec_digest, review_diff_rules_digest,
  review_diff_set_digest,
  repository_source_state_digest,
  publication_lineage_digest,
  coverage_digest, manifest_body_digest, manifest_root_digest, bundle_digest,
  bundle_search_index_digest, risk_read_map_digest,
  mandatory_read_set_digest, mandatory_read_count, mandatory_read_bytes,
  changed_path_count, required_evidence_count, carried_finding_count,
  object_count, chunk_count, total_object_bytes,
  manifest_page_bytes, search_index_bytes, risk_map_bytes,
  private_manifest_body_path, private_manifest_root_path,
  private_bundle_ref_path, created_at,
  PRIMARY KEY(run_id, bundle_generation),
  UNIQUE(bundle_digest)
)

review_bundle_objects(
  bundle_digest, object_digest, media_type, byte_length, ordinal,
  PRIMARY KEY(bundle_digest, object_digest),
  UNIQUE(bundle_digest, ordinal)
)

review_bundle_chunks(
  bundle_digest, object_digest, ordinal, chunk_digest, byte_length,
  private_chunk_path,
  PRIMARY KEY(bundle_digest, object_digest, ordinal)
)

review_bundle_manifest_pages(
  bundle_digest, ordinal, page_digest, byte_length, private_page_path,
  PRIMARY KEY(bundle_digest, ordinal)
)

review_finding_sets(
  finding_set_digest PRIMARY KEY, finding_count, page_count,
  canonical_byte_length, created_at
)

review_finding_pages(
  page_digest PRIMARY KEY, member_count, canonical_byte_length,
  private_page_path, created_at
)

review_finding_set_pages(
  finding_set_digest, ordinal, page_digest, member_count,
  first_finding_digest, last_finding_digest,
  PRIMARY KEY(finding_set_digest, ordinal),
  UNIQUE(finding_set_digest, page_digest),
  FOREIGN KEY(finding_set_digest) REFERENCES review_finding_sets(finding_set_digest),
  FOREIGN KEY(page_digest) REFERENCES review_finding_pages(page_digest)
)

review_finding_members(
  page_digest, member_ordinal, finding_digest,
  finding_id, severity, safe_record_json,
  PRIMARY KEY(page_digest, member_ordinal),
  UNIQUE(page_digest, finding_digest, finding_id),
  FOREIGN KEY(page_digest) REFERENCES review_finding_pages(page_digest)
)

provider_action_pair_preflights(
  adapter_id NOT NULL, action_id NOT NULL,
  scope_kind NOT NULL CHECK(scope_kind IN ('provider-smoke','run-action')),
  run_id,
  owner_digest NOT NULL, actor_principal_digest NOT NULL, input_digest NOT NULL,
  state NOT NULL CHECK(state IN ('resolving','admitted','released')),
  created_at NOT NULL, updated_at NOT NULL,
  PRIMARY KEY(adapter_id, action_id),
  UNIQUE(adapter_id, action_id, owner_digest),
  UNIQUE(run_id, adapter_id, action_id),
  UNIQUE(run_id, adapter_id, action_id, owner_digest),
  FOREIGN KEY(run_id) REFERENCES runs(run_id),
  CHECK(
    (scope_kind = 'provider-smoke' AND run_id IS NULL) OR
    (scope_kind = 'run-action' AND run_id IS NOT NULL)
  )
)

review_finding_capacity_reservations(
  adapter_id NOT NULL, action_id NOT NULL, run_id NOT NULL,
  target_generation NOT NULL CHECK(target_generation >= 1),
  slot NOT NULL CHECK(slot IN
    ('native','other-primary','cursor-grok','agy-gemini')),
  attempt_generation CHECK(
    attempt_generation IS NULL OR attempt_generation >= 1),
  owner_digest NOT NULL,
  finding_window_mode NOT NULL, prior_open_finding_set_digest NOT NULL,
  maximum_new_findings NOT NULL, maximum_new_finding_bytes NOT NULL,
  reservation_digest NOT NULL,
  state NOT NULL CHECK(state IN ('preflight','attached','released','settled')),
  created_at NOT NULL, updated_at NOT NULL,
  PRIMARY KEY(adapter_id, action_id),
  UNIQUE(adapter_id, action_id, reservation_digest),
  UNIQUE(run_id, target_generation, slot, attempt_generation),
  UNIQUE(adapter_id, action_id, run_id, target_generation, slot,
    attempt_generation, reservation_digest),
  FOREIGN KEY(run_id, adapter_id, action_id, owner_digest)
    REFERENCES provider_action_pair_preflights(
      run_id, adapter_id, action_id, owner_digest),
  FOREIGN KEY(run_id, target_generation, slot)
    REFERENCES review_slot_heads(run_id, target_generation, slot),
  CHECK(
    (state IN ('preflight','released') AND attempt_generation IS NULL) OR
    (state IN ('attached','settled') AND attempt_generation IS NOT NULL)
  )
)

review_terminal_sequence_high_water(
  run_id PRIMARY KEY, terminal_sequence, revision
)

review_certification_cuts(
  run_id, target_generation, predecessor_binding_generation,
  predecessor_binding_digest, terminal_sequence_high_water,
  lifecycle_custody_agent_id, lifecycle_custody_id,
  lifecycle_custody_revision, lifecycle_adoption_evidence_digest,
  lifecycle_review_decision_digest, lifecycle_review_authority_receipt_digest,
  cut_digest, created_at,
  PRIMARY KEY(run_id, target_generation, lifecycle_custody_agent_id,
    lifecycle_custody_id, lifecycle_custody_revision),
  UNIQUE(cut_digest),
  UNIQUE(run_id, target_generation, lifecycle_custody_agent_id,
    lifecycle_custody_id, lifecycle_custody_revision,
    predecessor_binding_generation,lifecycle_review_decision_digest,
    lifecycle_review_authority_receipt_digest,cut_digest),
  UNIQUE(run_id,target_generation,lifecycle_custody_agent_id,
    lifecycle_custody_id,lifecycle_custody_revision,
    predecessor_binding_generation,cut_digest),
  FOREIGN KEY(run_id, lifecycle_custody_agent_id, lifecycle_custody_id,
      lifecycle_custody_revision)
    REFERENCES lifecycle_rotation_custody_revisions(
      run_id, agent_id, custody_id, revision),
  FOREIGN KEY(lifecycle_review_authority_receipt_digest,run_id,
      lifecycle_custody_agent_id,lifecycle_custody_id,
      lifecycle_custody_revision,lifecycle_review_decision_digest,cut_digest)
    REFERENCES lifecycle_review_authority_bindings(
      receipt_digest,run_id,agent_id,custody_id,custody_revision,
      review_decision_digest,certification_cut_digest)
)

review_completion_targets(
  run_id, target_generation, preparation_id, review_subject_digest,
  task_id, reviewed_artifact_id, reviewed_artifact_revision,
  publication_lineage_digest, delivery_review_basis_revision,
  delivery_review_basis_digest, repository_source_state_digest,
  bundle_generation, bundle_digest, manifest_body_digest, manifest_root_digest,
  coverage_digest, bundle_search_index_digest, risk_read_map_digest,
  mandatory_read_set_digest, mandatory_read_count, mandatory_read_bytes,
  object_count, chunk_count, total_object_bytes,
  profile_id, profile_schema_digest, resolved_profile_digest,
  initial_chair_binding_digest, state, created_at,
  PRIMARY KEY(run_id, target_generation),
  UNIQUE(run_id, target_generation, review_subject_digest),
  UNIQUE(run_id,target_generation,task_id,bundle_digest,coverage_digest,
    resolved_profile_digest),
  UNIQUE(run_id, review_subject_digest),
  UNIQUE(preparation_id),
  CHECK(state IN ('current', 'superseded'))
)

review_target_chair_bindings(
  run_id, target_generation, binding_generation,
  predecessor_binding_generation, predecessor_binding_digest,
  predecessor_certification_cut_sequence,
  predecessor_certification_cut_digest,
  predecessor_certification_cut_custody_agent_id,
  predecessor_certification_cut_custody_id,
  predecessor_certification_cut_custody_revision,
  agent_id, principal_generation,
  chair_lease_generation, provider_session_generation, bridge_generation,
  adapter_id, adapter_contract_digest, model_family, model,
  review_subject_digest,
  route_receipt_digest, profile_digest, task_id, reviewed_artifact_id,
  delivery_review_basis_digest, repository_source_state_digest, bundle_digest,
  lifecycle_custody_id, lifecycle_custody_revision, checkpoint_digest,
  lifecycle_adoption_evidence_digest,lifecycle_review_decision_digest,
  lifecycle_review_authority_receipt_digest,
  binding_digest, created_at,
  PRIMARY KEY(run_id, target_generation, binding_generation),
  UNIQUE(run_id, target_generation, binding_generation, binding_digest),
  UNIQUE(run_id,target_generation,binding_generation,binding_digest,task_id,
    bundle_digest,profile_digest),
  FOREIGN KEY(run_id, target_generation, review_subject_digest)
    REFERENCES review_completion_targets(
      run_id, target_generation, review_subject_digest),
  FOREIGN KEY(run_id, target_generation,
      predecessor_certification_cut_custody_agent_id,
      predecessor_certification_cut_custody_id,
      predecessor_certification_cut_custody_revision,
      predecessor_binding_generation,
      predecessor_certification_cut_digest)
    REFERENCES review_certification_cuts(
      run_id, target_generation, lifecycle_custody_agent_id,
      lifecycle_custody_id, lifecycle_custody_revision,
      predecessor_binding_generation, cut_digest),
  FOREIGN KEY(run_id, agent_id, lifecycle_custody_id,
      lifecycle_custody_revision)
    REFERENCES lifecycle_rotation_custody_revisions(
      run_id, agent_id, custody_id, revision),
  FOREIGN KEY(lifecycle_review_authority_receipt_digest,run_id,agent_id,
      lifecycle_custody_id,lifecycle_custody_revision,
      lifecycle_review_decision_digest,predecessor_certification_cut_digest)
    REFERENCES lifecycle_review_authority_bindings(
      receipt_digest,run_id,agent_id,custody_id,custody_revision,
      review_decision_digest,certification_cut_digest)
)

review_target_chair_binding_heads(
  run_id, target_generation, active_binding_generation, revision,
  PRIMARY KEY(run_id, target_generation),
  FOREIGN KEY(run_id, target_generation, active_binding_generation)
    REFERENCES review_target_chair_bindings(
      run_id, target_generation, binding_generation)
)

review_target_rebind_receipts(
  run_id, target_generation, lifecycle_custody_agent_id,
  lifecycle_custody_id, lifecycle_custody_revision, command_id,
  review_subject_digest, prior_binding_generation, new_binding_generation,
  prior_binding_digest, new_binding_digest, lifecycle_adoption_digest,
  lifecycle_review_decision_digest,lifecycle_certification_cut_digest,
  lifecycle_review_authority_receipt_digest,
  bundle_digest, profile_digest, slot_head_set_digest,
  open_and_repair_finding_set_digest, rebind_receipt_digest, created_at,
  PRIMARY KEY(run_id, target_generation, lifecycle_custody_agent_id,
    lifecycle_custody_id, lifecycle_custody_revision),
  UNIQUE(command_id), UNIQUE(rebind_receipt_digest),
  FOREIGN KEY(run_id, target_generation)
    REFERENCES review_completion_targets(run_id, target_generation),
  FOREIGN KEY(run_id, target_generation, review_subject_digest)
    REFERENCES review_completion_targets(
      run_id, target_generation, review_subject_digest),
  FOREIGN KEY(run_id, target_generation, prior_binding_generation)
    REFERENCES review_target_chair_bindings(
      run_id, target_generation, binding_generation),
  FOREIGN KEY(run_id, target_generation, new_binding_generation)
    REFERENCES review_target_chair_bindings(
      run_id, target_generation, binding_generation),
  FOREIGN KEY(run_id, lifecycle_custody_agent_id, lifecycle_custody_id,
      lifecycle_custody_revision)
    REFERENCES lifecycle_rotation_custody_revisions(
      run_id, agent_id, custody_id, revision),
  FOREIGN KEY(lifecycle_review_authority_receipt_digest,run_id,
      lifecycle_custody_agent_id,lifecycle_custody_id,
      lifecycle_custody_revision,lifecycle_review_decision_digest,
      lifecycle_certification_cut_digest)
    REFERENCES lifecycle_review_authority_bindings(
      receipt_digest,run_id,agent_id,custody_id,custody_revision,
      review_decision_digest,certification_cut_digest),
  CHECK(new_binding_generation = prior_binding_generation + 1)
)

review_certifying_slot_availability_revisions(
  project_session_id, profile_id, profile_schema_digest,
  target_chair_family, slot, adapter_id, adapter_contract_digest,
  provider_family, model, source_mode, runtime_identity_digest,
  platform_identity_digest, availability_revision, state, reason,
  created_at,
  PRIMARY KEY(project_session_id, profile_id, profile_schema_digest,
    target_chair_family, slot, adapter_id, adapter_contract_digest,
    provider_family, model, source_mode, runtime_identity_digest,
    platform_identity_digest, availability_revision)
)

review_certifying_slot_availability_heads(
  project_session_id, profile_id, profile_schema_digest,
  target_chair_family, slot, adapter_id, adapter_contract_digest,
  provider_family, model, source_mode, runtime_identity_digest,
  platform_identity_digest, current_availability_revision, revision,
  PRIMARY KEY(project_session_id, profile_id, profile_schema_digest,
    target_chair_family, slot, adapter_id, adapter_contract_digest,
    provider_family, model, source_mode, runtime_identity_digest,
    platform_identity_digest),
  FOREIGN KEY(project_session_id, profile_id, profile_schema_digest,
    target_chair_family, slot, adapter_id, adapter_contract_digest,
    provider_family, model, source_mode, runtime_identity_digest,
    platform_identity_digest, current_availability_revision)
    REFERENCES review_certifying_slot_availability_revisions(
      project_session_id, profile_id, profile_schema_digest,
      target_chair_family, slot, adapter_id, adapter_contract_digest,
      provider_family, model, source_mode, runtime_identity_digest,
      platform_identity_digest, availability_revision)
)

~~~

## Frozen fresh-rotation schema

~~~sql
lifecycle_fresh_rotation_preparations(
  preparation_id PRIMARY KEY, attempt_id UNIQUE, issue_id UNIQUE,
  project_session_id, run_id, agent_id,
  recovery_source_kind CHECK(
    recovery_source_kind IN ('custody','generation-loss')),
  old_custody_id, old_custody_revision,
  generation_loss_id, generation_loss_revision,
  recovery_source_ref_digest, source_journal_digest,
  provider_action_adapter_id, provider_action_id,
  checkpoint_ref, checkpoint_digest, checkpoint_validation_digest,
  checkpoint_validation_key,
  adapter_contract_digest, operation,
  reserved_provider_generation, reserved_principal_generation,
  reserved_bridge_generation, preparation_json, preparation_digest,
  created_at,
  UNIQUE(preparation_id,preparation_digest),
  UNIQUE(preparation_id,attempt_id,issue_id,project_session_id,run_id,agent_id,
    recovery_source_kind,recovery_source_ref_digest,source_journal_digest,
    preparation_digest),
  UNIQUE(preparation_id,attempt_id,issue_id,project_session_id,run_id,agent_id,
    recovery_source_kind,recovery_source_ref_digest,source_journal_digest,
    provider_action_adapter_id,provider_action_id,checkpoint_ref,
    checkpoint_digest,checkpoint_validation_digest,checkpoint_validation_key,
    adapter_contract_digest,
    operation,reserved_provider_generation,reserved_principal_generation,
    reserved_bridge_generation,preparation_digest),
  UNIQUE(preparation_id,attempt_id,issue_id,project_session_id,run_id,agent_id,
    recovery_source_kind,recovery_source_ref_digest,source_journal_digest,
    provider_action_adapter_id,provider_action_id,checkpoint_ref,
    checkpoint_digest,checkpoint_validation_key,adapter_contract_digest,operation,
    reserved_provider_generation,reserved_principal_generation,
    reserved_bridge_generation,preparation_digest),
  UNIQUE(provider_action_adapter_id,provider_action_id),
  FOREIGN KEY(issue_id,project_session_id,run_id,agent_id,
      recovery_source_kind,recovery_source_ref_digest,source_journal_digest)
    REFERENCES agent_lifecycle_recovery_capability_issues(
      issue_id,project_session_id,run_id,agent_id,recovery_source_kind,
      recovery_source_ref_digest,source_journal_digest),
  FOREIGN KEY(project_session_id,run_id,agent_id,old_custody_id,
      old_custody_revision,recovery_source_ref_digest,source_journal_digest)
    REFERENCES lifecycle_rotation_custody_revisions(
      project_session_id,run_id,agent_id,custody_id,revision,
      source_ref_digest,journal_digest),
  FOREIGN KEY(project_session_id,run_id,agent_id,generation_loss_id,
      generation_loss_revision,recovery_source_ref_digest,source_journal_digest)
    REFERENCES lifecycle_generation_loss_revisions(
      project_session_id,run_id,agent_id,generation_loss_id,
      revision,source_ref_digest,journal_digest),
  CHECK((checkpoint_validation_digest IS NULL AND
      checkpoint_validation_key='none') OR
    (checkpoint_validation_digest IS NOT NULL AND
      checkpoint_validation_key=checkpoint_validation_digest)),
  CHECK((recovery_source_kind='custody' AND old_custody_id IS NOT NULL AND
      old_custody_revision IS NOT NULL AND generation_loss_id IS NULL AND
      generation_loss_revision IS NULL) OR
    (recovery_source_kind='generation-loss' AND old_custody_id IS NULL AND
      old_custody_revision IS NULL AND generation_loss_id IS NOT NULL AND
      generation_loss_revision IS NOT NULL))
)

lifecycle_fresh_recovery_handoffs(
  handoff_id PRIMARY KEY, preparation_id UNIQUE, attempt_id UNIQUE,
  preparation_digest, issue_id NOT NULL UNIQUE, project_session_id, run_id,
  agent_id,
  source_mode CHECK(source_mode IN ('terminalize-nonfinal-custody',
    'reuse-final-custody','open-generation-loss')),
  recovery_source_kind CHECK(
    recovery_source_kind IN ('custody','generation-loss')),
  old_custody_id, old_custody_revision,
  generation_loss_id, generation_loss_revision,
  recovery_source_ref_digest, source_journal_digest,
  new_custody_id UNIQUE, planned_apply_id UNIQUE, new_custody_semantic_digest,
  new_custody_source_ref_digest,
  affected_generation_loss_id, affected_generation_loss_before_revision,
  affected_generation_loss_before_state,
  affected_generation_loss_before_source_ref_digest,
  affected_generation_loss_before_journal_digest,
  affected_generation_loss_after_revision,
  affected_generation_loss_after_semantic_digest,
  affected_generation_loss_after_source_ref_digest,
  affected_generation_loss_after_key NOT NULL,
  provider_action_adapter_id, provider_action_id,
  checkpoint_ref, checkpoint_digest, checkpoint_validation_digest,
  checkpoint_validation_key,
  adapter_contract_digest, operation,
  reserved_provider_generation, reserved_principal_generation,
  reserved_bridge_generation, admission_digest,
  fresh_apply_plan_json, fresh_apply_plan_digest,
  handoff_json, handoff_digest UNIQUE, created_at,
  UNIQUE(handoff_id,handoff_digest),
  UNIQUE(handoff_id,handoff_digest,planned_apply_id),
  UNIQUE(handoff_id,handoff_digest,planned_apply_id,source_mode),
  UNIQUE(handoff_id,handoff_digest,affected_generation_loss_after_key),
  UNIQUE(handoff_id,provider_action_adapter_id,provider_action_id),
  UNIQUE(handoff_id,admission_digest,fresh_apply_plan_digest),
  UNIQUE(handoff_id,handoff_digest,project_session_id,run_id,agent_id,
    source_mode,recovery_source_kind,old_custody_id,old_custody_revision,
    generation_loss_id,generation_loss_revision,recovery_source_ref_digest,
    source_journal_digest,new_custody_id,provider_action_adapter_id,
    provider_action_id,checkpoint_ref,checkpoint_digest,
    checkpoint_validation_digest,checkpoint_validation_key,
    adapter_contract_digest,operation,
    reserved_provider_generation,reserved_principal_generation,
    reserved_bridge_generation,admission_digest,fresh_apply_plan_digest),
  UNIQUE(handoff_id,handoff_digest,project_session_id,run_id,agent_id,
    recovery_source_kind,recovery_source_ref_digest,source_journal_digest,
    new_custody_id,provider_action_adapter_id,provider_action_id,checkpoint_ref,
    checkpoint_digest,checkpoint_validation_key,adapter_contract_digest,operation,
    reserved_provider_generation,reserved_principal_generation,
    reserved_bridge_generation,admission_digest,fresh_apply_plan_digest),
  UNIQUE(handoff_id,handoff_digest,planned_apply_id,project_session_id,run_id,
    agent_id,source_mode,new_custody_id,new_custody_semantic_digest,
    new_custody_source_ref_digest,
    fresh_apply_plan_digest,affected_generation_loss_id,
    affected_generation_loss_after_revision,
    affected_generation_loss_after_semantic_digest,
    affected_generation_loss_after_source_ref_digest),
  UNIQUE(handoff_id,planned_apply_id,affected_generation_loss_id,
    affected_generation_loss_after_revision,
    affected_generation_loss_after_semantic_digest,
    affected_generation_loss_after_source_ref_digest),
  UNIQUE(handoff_id,handoff_digest,planned_apply_id,project_session_id,run_id,
    agent_id,source_mode,new_custody_id,new_custody_semantic_digest,
    new_custody_source_ref_digest,fresh_apply_plan_digest,
    affected_generation_loss_after_key),
  UNIQUE(handoff_id,handoff_digest,planned_apply_id,project_session_id,run_id,
    agent_id,source_mode,recovery_source_kind,recovery_source_ref_digest,
    source_journal_digest,new_custody_id,new_custody_semantic_digest,
    new_custody_source_ref_digest,affected_generation_loss_after_key,
    admission_digest,fresh_apply_plan_digest),
  UNIQUE(handoff_id,handoff_digest,affected_generation_loss_id,
    affected_generation_loss_before_revision,
    affected_generation_loss_before_state,
    affected_generation_loss_before_source_ref_digest,
    affected_generation_loss_before_journal_digest,
    affected_generation_loss_after_revision,
    affected_generation_loss_after_semantic_digest,
    affected_generation_loss_after_source_ref_digest),
  UNIQUE(handoff_id,handoff_digest,planned_apply_id,project_session_id,run_id,
    agent_id,source_mode,recovery_source_kind,old_custody_id,
    old_custody_revision,generation_loss_id,generation_loss_revision,
    recovery_source_ref_digest,source_journal_digest,admission_digest,
    fresh_apply_plan_digest,
    new_custody_id,new_custody_semantic_digest,new_custody_source_ref_digest,
    affected_generation_loss_id,affected_generation_loss_before_revision,
    affected_generation_loss_before_source_ref_digest,
    affected_generation_loss_before_journal_digest,
    affected_generation_loss_after_revision,
    affected_generation_loss_after_semantic_digest,
    affected_generation_loss_after_source_ref_digest,
    affected_generation_loss_after_key),
  UNIQUE(handoff_id,preparation_id,attempt_id,issue_id,project_session_id,
    run_id,agent_id,source_mode,recovery_source_kind,recovery_source_ref_digest,
    source_journal_digest,preparation_digest,fresh_apply_plan_digest,
    handoff_digest),
  UNIQUE(provider_action_adapter_id,provider_action_id),
  FOREIGN KEY(issue_id)
    REFERENCES agent_lifecycle_recovery_source_heads(issue_id),
  FOREIGN KEY(preparation_id,attempt_id,issue_id,project_session_id,run_id,
      agent_id,recovery_source_kind,recovery_source_ref_digest,
      source_journal_digest,preparation_digest)
    REFERENCES lifecycle_fresh_rotation_preparations(
      preparation_id,attempt_id,issue_id,project_session_id,run_id,agent_id,
      recovery_source_kind,recovery_source_ref_digest,source_journal_digest,
      preparation_digest),
  FOREIGN KEY(preparation_id,attempt_id,issue_id,project_session_id,run_id,
      agent_id,recovery_source_kind,recovery_source_ref_digest,
      source_journal_digest,provider_action_adapter_id,provider_action_id,
      checkpoint_ref,checkpoint_digest,checkpoint_validation_digest,
      checkpoint_validation_key,
      adapter_contract_digest,operation,reserved_provider_generation,
      reserved_principal_generation,reserved_bridge_generation,
      preparation_digest)
    REFERENCES lifecycle_fresh_rotation_preparations(
      preparation_id,attempt_id,issue_id,project_session_id,run_id,agent_id,
      recovery_source_kind,recovery_source_ref_digest,source_journal_digest,
      provider_action_adapter_id,provider_action_id,checkpoint_ref,
      checkpoint_digest,checkpoint_validation_digest,checkpoint_validation_key,
      adapter_contract_digest,
      operation,reserved_provider_generation,reserved_principal_generation,
      reserved_bridge_generation,preparation_digest),
  FOREIGN KEY(preparation_id,attempt_id,issue_id,project_session_id,run_id,
      agent_id,recovery_source_kind,recovery_source_ref_digest,
      source_journal_digest,provider_action_adapter_id,provider_action_id,
      checkpoint_ref,checkpoint_digest,checkpoint_validation_key,
      adapter_contract_digest,operation,
      reserved_provider_generation,reserved_principal_generation,
      reserved_bridge_generation,preparation_digest)
    REFERENCES lifecycle_fresh_rotation_preparations(
      preparation_id,attempt_id,issue_id,project_session_id,run_id,agent_id,
      recovery_source_kind,recovery_source_ref_digest,source_journal_digest,
      provider_action_adapter_id,provider_action_id,checkpoint_ref,
      checkpoint_digest,checkpoint_validation_key,adapter_contract_digest,operation,
      reserved_provider_generation,reserved_principal_generation,
      reserved_bridge_generation,preparation_digest),
  FOREIGN KEY(project_session_id,run_id,agent_id,old_custody_id,
      old_custody_revision,recovery_source_ref_digest,source_journal_digest)
    REFERENCES lifecycle_rotation_custody_revisions(
      project_session_id,run_id,agent_id,custody_id,revision,
      source_ref_digest,journal_digest),
  FOREIGN KEY(project_session_id,run_id,agent_id,generation_loss_id,
      generation_loss_revision,recovery_source_ref_digest,source_journal_digest)
    REFERENCES lifecycle_generation_loss_revisions(
      project_session_id,run_id,agent_id,generation_loss_id,revision,
      source_ref_digest,journal_digest),
  FOREIGN KEY(project_session_id,run_id,agent_id,affected_generation_loss_id,
      affected_generation_loss_before_revision,
      affected_generation_loss_before_state,
      affected_generation_loss_before_source_ref_digest,
      affected_generation_loss_before_journal_digest)
    REFERENCES lifecycle_generation_loss_revisions(
      project_session_id,run_id,agent_id,generation_loss_id,revision,
      state,source_ref_digest,journal_digest),
  FOREIGN KEY(project_session_id,run_id,agent_id,affected_generation_loss_id,
      affected_generation_loss_before_revision,
      affected_generation_loss_before_state,old_custody_id,
      affected_generation_loss_before_source_ref_digest,
      affected_generation_loss_before_journal_digest)
    REFERENCES lifecycle_generation_loss_revisions(
      project_session_id,run_id,agent_id,generation_loss_id,revision,state,
      active_recovery_custody_id,source_ref_digest,journal_digest),
  CHECK((checkpoint_validation_digest IS NULL AND
      checkpoint_validation_key='none') OR
    (checkpoint_validation_digest IS NOT NULL AND
      checkpoint_validation_key=checkpoint_validation_digest)),
  CHECK((source_mode='terminalize-nonfinal-custody' AND
      recovery_source_kind='custody' AND
      old_custody_id IS NOT NULL AND old_custody_revision IS NOT NULL AND
      generation_loss_id IS NULL AND generation_loss_revision IS NULL AND
      ((affected_generation_loss_id IS NULL AND
          affected_generation_loss_before_revision IS NULL AND
          affected_generation_loss_before_state IS NULL AND
          affected_generation_loss_before_source_ref_digest IS NULL AND
          affected_generation_loss_before_journal_digest IS NULL AND
          affected_generation_loss_after_revision IS NULL AND
          affected_generation_loss_after_semantic_digest IS NULL AND
          affected_generation_loss_after_source_ref_digest IS NULL AND
          affected_generation_loss_after_key='none') OR
        (affected_generation_loss_id IS NOT NULL AND
          affected_generation_loss_before_revision IS NOT NULL AND
          affected_generation_loss_before_state='recovery-in-progress' AND
          affected_generation_loss_before_source_ref_digest IS NOT NULL AND
          affected_generation_loss_before_journal_digest IS NOT NULL AND
          affected_generation_loss_after_revision=
            affected_generation_loss_before_revision+1 AND
          affected_generation_loss_after_semantic_digest IS NOT NULL AND
          affected_generation_loss_after_source_ref_digest IS NOT NULL AND
          affected_generation_loss_after_key=
            affected_generation_loss_after_source_ref_digest))) OR
    (source_mode='reuse-final-custody' AND recovery_source_kind='custody' AND
      old_custody_id IS NOT NULL AND old_custody_revision IS NOT NULL AND
      generation_loss_id IS NULL AND generation_loss_revision IS NULL AND
      affected_generation_loss_id IS NULL AND
      affected_generation_loss_before_revision IS NULL AND
      affected_generation_loss_before_state IS NULL AND
      affected_generation_loss_before_source_ref_digest IS NULL AND
      affected_generation_loss_before_journal_digest IS NULL AND
      affected_generation_loss_after_revision IS NULL AND
      affected_generation_loss_after_semantic_digest IS NULL AND
      affected_generation_loss_after_source_ref_digest IS NULL AND
      affected_generation_loss_after_key='none') OR
    (source_mode='open-generation-loss' AND
      recovery_source_kind='generation-loss' AND old_custody_id IS NULL AND
      old_custody_revision IS NULL AND generation_loss_id IS NOT NULL AND
      generation_loss_revision IS NOT NULL AND
      affected_generation_loss_id=generation_loss_id AND
      affected_generation_loss_before_revision=generation_loss_revision AND
      affected_generation_loss_before_state='open' AND
      affected_generation_loss_before_source_ref_digest=
        recovery_source_ref_digest AND
      affected_generation_loss_before_journal_digest=source_journal_digest AND
      affected_generation_loss_after_revision=generation_loss_revision+1 AND
      affected_generation_loss_after_semantic_digest IS NOT NULL AND
      affected_generation_loss_after_source_ref_digest IS NOT NULL AND
      affected_generation_loss_after_key=
        affected_generation_loss_after_source_ref_digest))
)

lifecycle_fresh_rotation_commits(
  commit_id PRIMARY KEY, handoff_id UNIQUE, preparation_id UNIQUE,
  handoff_digest, preparation_digest, attempt_id UNIQUE, issue_id UNIQUE,
  project_session_id, run_id, agent_id,
  source_mode, recovery_source_kind, recovery_source_ref_digest,
  source_journal_digest, new_custody_id UNIQUE,
  new_custody_revision CHECK(new_custody_revision=1),
  new_custody_semantic_digest, new_custody_source_ref_digest,
  new_custody_journal_digest,
  generation_loss_after_id, generation_loss_after_revision,
  generation_loss_after_semantic_digest,
  generation_loss_after_source_ref_digest, generation_loss_after_journal_digest,
  generation_loss_after_key NOT NULL,
  provider_action_adapter_id, provider_action_id,
  checkpoint_ref, checkpoint_digest, checkpoint_validation_digest,
  checkpoint_validation_key,
  adapter_contract_digest, operation,
  reserved_provider_generation, reserved_principal_generation,
  reserved_bridge_generation,
  admission_digest, fresh_apply_plan_digest,
  apply_kind CHECK(apply_kind IN ('terminal-fresh','fresh')), fresh_apply_digest,
  source_terminal_receipt_apply_digest, apply_id UNIQUE,
  commit_json, commit_digest UNIQUE, created_at,
  UNIQUE(handoff_id,preparation_id,attempt_id,issue_id,project_session_id,
    run_id,agent_id,source_mode,recovery_source_kind,recovery_source_ref_digest,
    source_journal_digest,preparation_digest,fresh_apply_plan_digest),
  FOREIGN KEY(handoff_id,handoff_digest,generation_loss_after_key)
    REFERENCES lifecycle_fresh_recovery_handoffs(
      handoff_id,handoff_digest,affected_generation_loss_after_key),
  FOREIGN KEY(apply_id,fresh_apply_digest,generation_loss_after_key)
    REFERENCES lifecycle_transition_applies(
      apply_id,apply_digest,fresh_generation_loss_after_key)
    DEFERRABLE INITIALLY DEFERRED,
  FOREIGN KEY(handoff_id,preparation_id,attempt_id,issue_id,
      project_session_id,run_id,agent_id,source_mode,recovery_source_kind,
      recovery_source_ref_digest,source_journal_digest,preparation_digest,
      fresh_apply_plan_digest,handoff_digest)
    REFERENCES lifecycle_fresh_recovery_handoffs(
      handoff_id,preparation_id,attempt_id,issue_id,project_session_id,run_id,
      agent_id,source_mode,recovery_source_kind,recovery_source_ref_digest,
      source_journal_digest,preparation_digest,fresh_apply_plan_digest,
      handoff_digest),
  FOREIGN KEY(project_session_id,run_id,agent_id,new_custody_id,
      new_custody_revision,new_custody_semantic_digest,
      new_custody_source_ref_digest,new_custody_journal_digest,apply_id,
      fresh_apply_digest)
    REFERENCES lifecycle_rotation_custody_revisions(
      project_session_id,run_id,agent_id,custody_id,revision,semantic_digest,
      source_ref_digest,journal_digest,origin_fresh_apply_id,
      origin_fresh_apply_digest),
  FOREIGN KEY(handoff_id,handoff_digest,apply_id,project_session_id,run_id,
      agent_id,source_mode,new_custody_id,new_custody_semantic_digest,
      new_custody_source_ref_digest,fresh_apply_plan_digest,
      generation_loss_after_id,generation_loss_after_revision,
      generation_loss_after_semantic_digest,
      generation_loss_after_source_ref_digest)
    REFERENCES lifecycle_fresh_recovery_handoffs(
      handoff_id,handoff_digest,planned_apply_id,project_session_id,run_id,
      agent_id,source_mode,new_custody_id,new_custody_semantic_digest,
      new_custody_source_ref_digest,fresh_apply_plan_digest,
      affected_generation_loss_id,affected_generation_loss_after_revision,
      affected_generation_loss_after_semantic_digest,
      affected_generation_loss_after_source_ref_digest),
  FOREIGN KEY(apply_id,fresh_apply_digest,project_session_id,run_id,agent_id,
      generation_loss_after_id,generation_loss_after_revision,
      generation_loss_after_semantic_digest,
      generation_loss_after_source_ref_digest)
    REFERENCES lifecycle_transition_applies(
      apply_id,apply_digest,fresh_project_session_id,fresh_run_id,fresh_agent_id,
      fresh_generation_loss_id,fresh_generation_loss_after_revision,
      fresh_generation_loss_after_semantic_digest,
      fresh_generation_loss_after_source_ref_digest)
    DEFERRABLE INITIALLY DEFERRED,
  FOREIGN KEY(project_session_id,run_id,agent_id,generation_loss_after_id,
      generation_loss_after_revision,generation_loss_after_semantic_digest,
      generation_loss_after_source_ref_digest,
      generation_loss_after_journal_digest)
    REFERENCES lifecycle_generation_loss_revisions(
      project_session_id,run_id,agent_id,generation_loss_id,revision,
      semantic_digest,source_ref_digest,journal_digest),
  FOREIGN KEY(handoff_id,provider_action_adapter_id,provider_action_id)
    REFERENCES lifecycle_fresh_recovery_handoffs(
      handoff_id,provider_action_adapter_id,provider_action_id),
  FOREIGN KEY(handoff_id,handoff_digest,project_session_id,run_id,agent_id,
      recovery_source_kind,recovery_source_ref_digest,source_journal_digest,
      new_custody_id,provider_action_adapter_id,provider_action_id,checkpoint_ref,
      checkpoint_digest,checkpoint_validation_key,adapter_contract_digest,operation,
      reserved_provider_generation,reserved_principal_generation,
      reserved_bridge_generation,admission_digest,fresh_apply_plan_digest)
    REFERENCES lifecycle_fresh_recovery_handoffs(
      handoff_id,handoff_digest,project_session_id,run_id,agent_id,
      recovery_source_kind,recovery_source_ref_digest,source_journal_digest,
      new_custody_id,provider_action_adapter_id,provider_action_id,checkpoint_ref,
      checkpoint_digest,checkpoint_validation_key,adapter_contract_digest,operation,
      reserved_provider_generation,reserved_principal_generation,
      reserved_bridge_generation,admission_digest,fresh_apply_plan_digest),
  FOREIGN KEY(handoff_id,admission_digest,fresh_apply_plan_digest)
    REFERENCES lifecycle_fresh_recovery_handoffs(
      handoff_id,admission_digest,fresh_apply_plan_digest),
  FOREIGN KEY(apply_id,handoff_id)
    REFERENCES lifecycle_transition_applies(apply_id,fresh_handoff_id)
    DEFERRABLE INITIALLY DEFERRED,
  FOREIGN KEY(apply_id,fresh_apply_digest,handoff_id,apply_kind)
    REFERENCES lifecycle_transition_applies(
      apply_id,apply_digest,fresh_handoff_id,apply_kind)
    DEFERRABLE INITIALLY DEFERRED,
  FOREIGN KEY(apply_id,source_terminal_receipt_apply_digest,handoff_id)
    REFERENCES lifecycle_transition_applies(
      apply_id,apply_digest,fresh_handoff_id)
    DEFERRABLE INITIALLY DEFERRED,
  CHECK((checkpoint_validation_digest IS NULL AND
      checkpoint_validation_key='none') OR
    (checkpoint_validation_digest IS NOT NULL AND
      checkpoint_validation_key=checkpoint_validation_digest)),
  CHECK((source_mode='terminalize-nonfinal-custody' AND
      apply_kind='terminal-fresh' AND
      source_terminal_receipt_apply_digest=fresh_apply_digest AND
      ((generation_loss_after_id IS NULL AND
          generation_loss_after_revision IS NULL AND
          generation_loss_after_semantic_digest IS NULL AND
          generation_loss_after_source_ref_digest IS NULL AND
          generation_loss_after_journal_digest IS NULL AND
          generation_loss_after_key='none') OR
        (generation_loss_after_id IS NOT NULL AND
          generation_loss_after_revision IS NOT NULL AND
          generation_loss_after_semantic_digest IS NOT NULL AND
          generation_loss_after_source_ref_digest IS NOT NULL AND
          generation_loss_after_journal_digest IS NOT NULL AND
          generation_loss_after_key=
            generation_loss_after_source_ref_digest))) OR
    (source_mode='reuse-final-custody' AND apply_kind='fresh' AND
      source_terminal_receipt_apply_digest IS NULL AND
      generation_loss_after_id IS NULL AND
      generation_loss_after_revision IS NULL AND
      generation_loss_after_semantic_digest IS NULL AND
      generation_loss_after_source_ref_digest IS NULL AND
      generation_loss_after_journal_digest IS NULL AND
      generation_loss_after_key='none') OR
    (source_mode='open-generation-loss' AND apply_kind='fresh' AND
      source_terminal_receipt_apply_digest IS NULL AND
      generation_loss_after_id IS NOT NULL AND
      generation_loss_after_revision IS NOT NULL AND
      generation_loss_after_semantic_digest IS NOT NULL AND
      generation_loss_after_source_ref_digest IS NOT NULL AND
      generation_loss_after_journal_digest IS NOT NULL AND
      generation_loss_after_key=generation_loss_after_source_ref_digest)),
  CHECK(source_mode IN ('terminalize-nonfinal-custody',
    'reuse-final-custody','open-generation-loss')),
  CHECK(recovery_source_kind IN ('custody','generation-loss'))
)

~~~
