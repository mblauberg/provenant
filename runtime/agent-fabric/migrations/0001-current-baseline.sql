-- Generated pre-release Agent Fabric schema baseline.
-- Fresh databases only: existing non-current state is rejected before mutation.

CREATE TABLE fabric_schema (
  singleton INTEGER PRIMARY KEY CHECK (singleton = 1),
  epoch TEXT NOT NULL CHECK (epoch = 'agent-fabric-pre-release-v1'),
  baseline_sha256 TEXT NOT NULL CHECK (length(baseline_sha256) = 64),
  catalog_sha256 TEXT NOT NULL CHECK (length(catalog_sha256) = 64)
);

CREATE TABLE agent_adapter_bindings (
  run_id TEXT NOT NULL,
  agent_id TEXT NOT NULL,
  adapter_id TEXT NOT NULL,
  contract_version INTEGER NOT NULL DEFAULT 1,
  bound_at INTEGER NOT NULL,
  PRIMARY KEY (run_id, agent_id),
  UNIQUE (run_id, agent_id, adapter_id),
  FOREIGN KEY (run_id, agent_id) REFERENCES agents(run_id, agent_id)
);

CREATE TABLE agent_bridge_state (
  run_id TEXT NOT NULL,
  agent_id TEXT NOT NULL,
  adapter_id TEXT NOT NULL,
  action_id TEXT NOT NULL,
  provider_session_ref TEXT,
  provider_session_generation INTEGER CHECK (
    provider_session_generation IS NULL OR provider_session_generation >= 1
  ),
  bridge_state TEXT NOT NULL CHECK (bridge_state IN ('pending','active','none','lost')),
  bridge_generation INTEGER NOT NULL CHECK (bridge_generation >= 1),
  capability_hash TEXT,
  activation_evidence_digest TEXT,
  revision INTEGER NOT NULL CHECK (revision >= 1),
  created_at INTEGER NOT NULL,
  updated_at INTEGER NOT NULL,
  PRIMARY KEY (run_id, agent_id),
  FOREIGN KEY (run_id, agent_id) REFERENCES agents(run_id, agent_id),
  FOREIGN KEY (run_id, adapter_id, action_id)
    REFERENCES provider_agent_custody(run_id, adapter_id, action_id),
  FOREIGN KEY (capability_hash) REFERENCES capabilities(token_hash),
  CHECK ((provider_session_ref IS NULL)=(provider_session_generation IS NULL)),
  CHECK (
    (activation_evidence_digest IS NULL) OR
    (length(activation_evidence_digest)=71 AND substr(activation_evidence_digest,1,7)='sha256:')
  ),
  CHECK (
    (bridge_state='pending' AND capability_hash IS NOT NULL AND activation_evidence_digest IS NULL) OR
    (bridge_state='active' AND capability_hash IS NOT NULL AND provider_session_ref IS NOT NULL
      AND activation_evidence_digest IS NOT NULL) OR
    (bridge_state='none' AND capability_hash IS NULL AND activation_evidence_digest IS NULL) OR
    (bridge_state='lost' AND capability_hash IS NOT NULL AND provider_session_ref IS NOT NULL
      AND activation_evidence_digest IS NOT NULL)
  )
);

CREATE TABLE agents (
  run_id TEXT NOT NULL REFERENCES runs(run_id),
  agent_id TEXT NOT NULL,
  parent_agent_id TEXT,
  authority_id TEXT NOT NULL REFERENCES authorities(authority_id),
  provider_session_ref TEXT,
  lifecycle TEXT NOT NULL DEFAULT 'ready',
  classification TEXT CHECK (classification IN ('worker','reviewer')),
  PRIMARY KEY (run_id, agent_id)
);

CREATE TABLE artifact_content_cursor_keys (
  singleton INTEGER PRIMARY KEY CHECK(singleton=1),
  key_material TEXT NOT NULL CHECK(length(key_material) >= 43)
);

CREATE TABLE artifacts (
  artifact_id TEXT PRIMARY KEY,
  project_id TEXT NOT NULL REFERENCES projects(project_id),
  project_session_id TEXT,
  run_id TEXT,
  task_id TEXT,
  publisher_kind TEXT NOT NULL
    CHECK (publisher_kind IN ('agent','operator','fabric','project')),
  publisher_ref TEXT NOT NULL,
  publisher_agent_id TEXT,
  source_kind TEXT NOT NULL
    CHECK (source_kind IN ('project-file','run-file','git-private-diff')),
  evidence_kind TEXT NOT NULL
    CHECK (evidence_kind IN (
      'artifact','diff','test','review','receipt',
      'delivery-requirement-map.v1','implementation-delivery-manifest.v1',
      'coordination-gate-snapshot.v1','discovery-surface.v1',
      'adapter-effective-configuration.v1'
    )),
  relative_path TEXT NOT NULL,
  sha256 TEXT NOT NULL
    CHECK (length(sha256)=71 AND substr(sha256,1,7)='sha256:'),
  registry_state TEXT NOT NULL
    CHECK (registry_state IN ('active','quarantined')),
  quarantine_reason TEXT,
  revision INTEGER NOT NULL CHECK (revision >= 1),
  created_at INTEGER NOT NULL,
  FOREIGN KEY(project_session_id, project_id)
    REFERENCES project_sessions(project_session_id, project_id),
  FOREIGN KEY(project_session_id, run_id)
    REFERENCES runs(project_session_id, run_id),
  FOREIGN KEY(run_id, task_id) REFERENCES tasks(run_id, task_id),
  FOREIGN KEY(run_id, publisher_agent_id) REFERENCES agents(run_id, agent_id),
  CHECK ((registry_state='active' AND quarantine_reason IS NULL) OR
         (registry_state='quarantined' AND quarantine_reason IS NOT NULL)),
  CHECK (
    (source_kind='project-file') OR
    (source_kind='run-file' AND project_session_id IS NOT NULL AND run_id IS NOT NULL) OR
    (source_kind='git-private-diff' AND run_id IS NULL AND task_id IS NULL)
  ),
  CHECK (run_id IS NOT NULL OR task_id IS NULL),
  CHECK ((run_id IS NULL AND project_session_id IS NULL) OR project_session_id IS NOT NULL),
  CHECK ((publisher_kind='agent' AND publisher_agent_id=publisher_ref AND run_id IS NOT NULL) OR
         (publisher_kind<>'agent' AND publisher_agent_id IS NULL))
);

CREATE TABLE attention_items (
  item_id TEXT PRIMARY KEY,
  project_session_id TEXT NOT NULL REFERENCES project_sessions(project_session_id),
  coordination_run_id TEXT,
  kind TEXT NOT NULL,
  severity TEXT NOT NULL,
  revision INTEGER NOT NULL CHECK (revision >= 1),
  state TEXT NOT NULL CHECK (state IN ('open','acknowledged','resolved','cancelled')),
  dedupe_key TEXT NOT NULL,
  payload_json TEXT NOT NULL,
  created_at INTEGER NOT NULL,
  updated_at INTEGER NOT NULL,
  UNIQUE(project_session_id, dedupe_key)
);

CREATE TABLE authorities (
  authority_id TEXT PRIMARY KEY,
  run_id TEXT NOT NULL REFERENCES runs(run_id),
  parent_authority_id TEXT REFERENCES authorities(authority_id),
  authority_json TEXT NOT NULL,
  authority_hash TEXT NOT NULL,
  created_at INTEGER NOT NULL
);

CREATE TABLE authority_budget (
  authority_id TEXT NOT NULL REFERENCES authorities(authority_id),
  unit_key TEXT NOT NULL,
  granted INTEGER NOT NULL CHECK (granted >= 0),
  reserved INTEGER NOT NULL DEFAULT 0 CHECK (reserved >= 0 AND reserved <= granted),
  consumed INTEGER NOT NULL DEFAULT 0 CHECK (consumed >= 0 AND consumed <= granted),
  provider_reserved INTEGER NOT NULL DEFAULT 0 CHECK (provider_reserved >= 0 AND provider_reserved <= reserved),
  provider_consumed INTEGER NOT NULL DEFAULT 0 CHECK (provider_consumed >= 0 AND provider_consumed <= consumed),
  usage_unknown INTEGER NOT NULL DEFAULT 0,
  PRIMARY KEY (authority_id, unit_key),
  CHECK (reserved + consumed <= granted)
);

CREATE TABLE barriers (
  run_id TEXT NOT NULL REFERENCES runs(run_id),
  scope TEXT NOT NULL,
  stage_id TEXT NOT NULL DEFAULT '',
  state TEXT NOT NULL,
  closed_at INTEGER,
  receipt_sha256 TEXT,
  PRIMARY KEY (run_id, scope, stage_id)
);

CREATE TABLE bootstrap_audit_receipts (
  action_id TEXT PRIMARY KEY,
  election_generation INTEGER NOT NULL UNIQUE CHECK (election_generation >= 1),
  attempt_digest TEXT NOT NULL UNIQUE,
  status TEXT NOT NULL CHECK (status IN ('succeeded','failed','expired','ambiguous')),
  receipt_json TEXT NOT NULL,
  imported_instance_generation INTEGER,
  created_at INTEGER NOT NULL
);

CREATE TABLE budget_dimensions (
  run_id TEXT NOT NULL,
  budget_id TEXT NOT NULL,
  unit_key TEXT NOT NULL,
  granted INTEGER NOT NULL,
  reserved INTEGER NOT NULL DEFAULT 0,
  consumed INTEGER NOT NULL DEFAULT 0,
  direct_usage_unknown INTEGER NOT NULL DEFAULT 0,
  usage_unknown INTEGER NOT NULL DEFAULT 0,
  PRIMARY KEY (run_id, budget_id, unit_key),
  FOREIGN KEY (run_id, budget_id) REFERENCES budgets(run_id, budget_id)
);

CREATE TABLE budgets (
  run_id TEXT NOT NULL,
  budget_id TEXT NOT NULL,
  parent_budget_id TEXT,
  team_id TEXT NOT NULL,
  owner_agent_id TEXT NOT NULL,
  state TEXT NOT NULL,
  returned_json TEXT NOT NULL DEFAULT '{}',
  created_at INTEGER NOT NULL,
  PRIMARY KEY (run_id, budget_id)
);

CREATE TABLE capabilities (
  token_hash TEXT PRIMARY KEY,
  run_id TEXT NOT NULL,
  agent_id TEXT NOT NULL,
  principal_generation INTEGER NOT NULL DEFAULT 1,
  expires_at INTEGER NOT NULL,
  revoked_at INTEGER,
  FOREIGN KEY (run_id, agent_id) REFERENCES agents(run_id, agent_id)
);

CREATE TABLE mcp_seat_generations (
  generation TEXT PRIMARY KEY CHECK (
    length(generation)=64 AND generation NOT GLOB '*[^0-9a-f]*'
  ),
  project_id TEXT NOT NULL REFERENCES projects(project_id),
  project_session_id TEXT NOT NULL,
  session_revision INTEGER NOT NULL CHECK(session_revision>=1),
  session_generation INTEGER NOT NULL CHECK(session_generation>=1),
  run_id TEXT NOT NULL,
  run_revision INTEGER NOT NULL CHECK(run_revision>=1),
  chair_agent_id TEXT NOT NULL,
  chair_generation INTEGER NOT NULL CHECK(chair_generation>=1),
  chair_lease_id TEXT NOT NULL,
  previous_generation TEXT REFERENCES mcp_seat_generations(generation),
  binding_json TEXT NOT NULL CHECK (json_valid(binding_json)=1),
  binding_digest TEXT NOT NULL UNIQUE CHECK (
    length(binding_digest)=71 AND substr(binding_digest,1,7)='sha256:'
  ),
  expires_at INTEGER NOT NULL,
  created_at INTEGER NOT NULL,
  UNIQUE(project_id,generation),
  FOREIGN KEY(project_session_id,project_id) REFERENCES project_sessions(project_session_id,project_id),
  FOREIGN KEY(project_session_id,run_id) REFERENCES runs(project_session_id,run_id),
  FOREIGN KEY(run_id,chair_agent_id) REFERENCES agents(run_id,agent_id),
  FOREIGN KEY(chair_lease_id) REFERENCES run_chair_leases(lease_id),
  CHECK(previous_generation IS NULL OR previous_generation<>generation)
);

CREATE TABLE mcp_active_seat_generations (
  project_id TEXT PRIMARY KEY REFERENCES projects(project_id),
  generation TEXT NOT NULL UNIQUE,
  activated_at INTEGER NOT NULL,
  FOREIGN KEY(project_id,generation) REFERENCES mcp_seat_generations(project_id,generation)
);

CREATE TABLE mcp_seat_generation_members (
  generation TEXT NOT NULL REFERENCES mcp_seat_generations(generation),
  seat TEXT NOT NULL CHECK (length(seat) BETWEEN 1 AND 64),
  run_id TEXT NOT NULL,
  agent_id TEXT NOT NULL,
  principal_generation INTEGER NOT NULL CHECK(principal_generation>=1),
  token_hash TEXT NOT NULL UNIQUE REFERENCES capabilities(token_hash),
  expires_at INTEGER NOT NULL,
  PRIMARY KEY(generation,seat),
  UNIQUE(generation,agent_id),
  FOREIGN KEY(run_id,agent_id) REFERENCES agents(run_id,agent_id)
);

CREATE VIEW current_mcp_seat_generation_members AS
SELECT member.generation,member.seat,member.run_id,member.agent_id,
       member.principal_generation,member.token_hash,member.expires_at
  FROM mcp_seat_generation_members member
  JOIN capabilities capability
    ON capability.token_hash=member.token_hash
   AND capability.run_id=member.run_id
   AND capability.agent_id=member.agent_id
   AND capability.principal_generation=member.principal_generation
   AND capability.expires_at=member.expires_at
  JOIN mcp_seat_generations generation ON generation.generation=member.generation
  JOIN mcp_active_seat_generations active
    ON active.project_id=generation.project_id AND active.generation=generation.generation
  JOIN project_sessions session
    ON session.project_session_id=generation.project_session_id
   AND session.project_id=generation.project_id
   AND session.generation=generation.session_generation
   AND session.state IN ('active','visibility_degraded')
  JOIN runs run
    ON run.project_session_id=generation.project_session_id
   AND run.run_id=generation.run_id
   AND run.chair_agent_id=generation.chair_agent_id
   AND run.chair_generation=generation.chair_generation
   AND run.chair_lease_id=generation.chair_lease_id
   AND run.lifecycle_state IN ('active','visibility_degraded')
  JOIN run_chair_leases lease
    ON lease.project_session_id=generation.project_session_id
   AND lease.run_id=generation.run_id
   AND lease.lease_id=generation.chair_lease_id
   AND lease.holder_agent_id=generation.chair_agent_id
   AND lease.generation=generation.chair_generation
   AND lease.status='active';

CREATE TABLE chair_bridge_loss_resolutions (
  loss_id TEXT PRIMARY KEY,
  recovery_id TEXT NOT NULL UNIQUE,
  path TEXT NOT NULL CHECK (path IN ('rebind','takeover','abandon')),
  successor_agent_id TEXT,
  new_principal_generation INTEGER,
  new_bridge_generation INTEGER,
  evidence_digest TEXT NOT NULL,
  created_at INTEGER NOT NULL,
  FOREIGN KEY (loss_id) REFERENCES chair_bridge_losses(loss_id),
  FOREIGN KEY (recovery_id) REFERENCES chair_bridge_recovery_custody(recovery_id),
  CHECK (length(evidence_digest)=71 AND substr(evidence_digest,1,7)='sha256:')
);

CREATE TABLE chair_bridge_losses (
  loss_id TEXT PRIMARY KEY,
  project_session_id TEXT NOT NULL,
  coordination_run_id TEXT NOT NULL,
  chair_agent_id TEXT NOT NULL,
  provider_adapter_id TEXT NOT NULL,
  provider_action_id TEXT NOT NULL,
  provider_contract_digest TEXT NOT NULL,
  provider_session_ref TEXT NOT NULL,
  provider_session_generation INTEGER NOT NULL CHECK (provider_session_generation >= 1),
  principal_generation INTEGER NOT NULL CHECK (principal_generation >= 1),
  lost_bridge_generation INTEGER NOT NULL CHECK (lost_bridge_generation >= 1),
  next_bridge_generation INTEGER NOT NULL CHECK (next_bridge_generation >= 2),
  capability_hash TEXT NOT NULL,
  daemon_instance_generation INTEGER NOT NULL CHECK (daemon_instance_generation >= 1),
  reason TEXT NOT NULL CHECK (length(reason) BETWEEN 1 AND 160),
  evidence_digest TEXT NOT NULL,
  recovery_manifest_digest TEXT NOT NULL,
  created_at INTEGER NOT NULL,
  UNIQUE (coordination_run_id, lost_bridge_generation),
  FOREIGN KEY (project_session_id, coordination_run_id)
    REFERENCES launched_chair_bridge_state(project_session_id, coordination_run_id),
  FOREIGN KEY (coordination_run_id, chair_agent_id)
    REFERENCES agents(run_id, agent_id),
  FOREIGN KEY (provider_adapter_id, provider_action_id)
    REFERENCES provider_actions(adapter_id, action_id),
  FOREIGN KEY (capability_hash) REFERENCES capabilities(token_hash),
  CHECK (next_bridge_generation=lost_bridge_generation+1),
  CHECK (length(provider_contract_digest)=71 AND substr(provider_contract_digest,1,7)='sha256:'),
  CHECK (length(evidence_digest)=71 AND substr(evidence_digest,1,7)='sha256:'),
  CHECK (length(recovery_manifest_digest)=71 AND substr(recovery_manifest_digest,1,7)='sha256:')
);

CREATE TABLE chair_bridge_recovery_custody (
  recovery_id TEXT PRIMARY KEY,
  loss_id TEXT NOT NULL,
  operator_id TEXT NOT NULL,
  operator_command_id TEXT NOT NULL,
  path TEXT NOT NULL CHECK (path IN ('rebind','takeover','abandon')),
  intent_digest TEXT NOT NULL,
  intent_json TEXT NOT NULL CHECK (json_valid(intent_json)=1),
  recovery_manifest_digest TEXT NOT NULL,
  expected_session_revision INTEGER NOT NULL,
  expected_session_generation INTEGER NOT NULL,
  expected_run_revision INTEGER NOT NULL,
  expected_chair_generation INTEGER NOT NULL,
  expected_principal_generation INTEGER NOT NULL,
  expected_bridge_revision INTEGER NOT NULL,
  expected_lost_bridge_generation INTEGER NOT NULL,
  expected_provider_session_generation INTEGER NOT NULL,
  provider_adapter_id TEXT NOT NULL,
  provider_contract_digest TEXT NOT NULL,
  provider_action_id TEXT,
  successor_agent_id TEXT,
  expected_successor_principal_generation INTEGER,
  expected_successor_bridge_generation INTEGER,
  expected_successor_revision INTEGER,
  new_chair_agent_id TEXT,
  new_provider_action_id TEXT,
  new_provider_session_ref TEXT,
  new_provider_session_generation INTEGER,
  new_principal_generation INTEGER,
  new_bridge_generation INTEGER,
  new_capability_hash TEXT,
  new_activation_evidence_digest TEXT,
  attestation_challenge_digest TEXT,
  state TEXT NOT NULL CHECK (state IN (
    'prepared','dispatched','accepted','ambiguous','committing','terminal','no-effect'
  )),
  result_json TEXT,
  revision INTEGER NOT NULL CHECK (revision >= 1),
  created_at INTEGER NOT NULL,
  updated_at INTEGER NOT NULL,
  UNIQUE (operator_id, operator_command_id),
  UNIQUE (provider_adapter_id, provider_action_id),
  FOREIGN KEY (loss_id) REFERENCES chair_bridge_losses(loss_id),
  CHECK (length(intent_digest)=71 AND substr(intent_digest,1,7)='sha256:'),
  CHECK (length(recovery_manifest_digest)=71 AND substr(recovery_manifest_digest,1,7)='sha256:'),
  CHECK (length(provider_contract_digest)=71 AND substr(provider_contract_digest,1,7)='sha256:'),
  CHECK (new_activation_evidence_digest IS NULL OR
    (length(new_activation_evidence_digest)=71 AND substr(new_activation_evidence_digest,1,7)='sha256:')),
  CHECK (attestation_challenge_digest IS NULL OR
    (length(attestation_challenge_digest)=71 AND substr(attestation_challenge_digest,1,7)='sha256:')),
  CHECK (
    (path='rebind' AND provider_action_id IS NOT NULL AND successor_agent_id IS NULL
      AND new_capability_hash IS NOT NULL AND attestation_challenge_digest IS NOT NULL) OR
    (path='takeover' AND provider_action_id IS NULL AND successor_agent_id IS NOT NULL
      AND new_capability_hash IS NOT NULL AND attestation_challenge_digest IS NULL) OR
    (path='abandon' AND provider_action_id IS NULL AND successor_agent_id IS NULL
      AND new_capability_hash IS NULL AND attestation_challenge_digest IS NULL)
  )
);

CREATE TABLE chair_live_handoff_custody (
  custody_id TEXT PRIMARY KEY,
  operator_id TEXT NOT NULL REFERENCES operator_principals(operator_id),
  operator_command_id TEXT NOT NULL,
  project_session_id TEXT NOT NULL,
  coordination_run_id TEXT NOT NULL,
  intent_digest TEXT NOT NULL,
  intent_json TEXT NOT NULL CHECK (json_valid(intent_json)=1),
  handoff_path TEXT NOT NULL,
  handoff_digest TEXT NOT NULL,
  predecessor_agent_id TEXT NOT NULL,
  successor_agent_id TEXT NOT NULL,
  successor_authority_id TEXT NOT NULL REFERENCES authorities(authority_id),
  successor_authority_digest TEXT NOT NULL,
  expected_session_revision INTEGER NOT NULL CHECK (expected_session_revision>=1),
  expected_session_generation INTEGER NOT NULL CHECK (expected_session_generation>=1),
  expected_membership_revision INTEGER NOT NULL CHECK (expected_membership_revision>=1),
  expected_run_revision INTEGER NOT NULL CHECK (expected_run_revision>=1),
  expected_chair_generation INTEGER NOT NULL CHECK (expected_chair_generation>=1),
  expected_chair_lease_id TEXT NOT NULL,
  expected_bridge_revision INTEGER NOT NULL CHECK (expected_bridge_revision>=1),
  expected_chair_bridge_generation INTEGER NOT NULL CHECK (expected_chair_bridge_generation>=1),
  expected_predecessor_principal_generation INTEGER NOT NULL CHECK (expected_predecessor_principal_generation>=1),
  expected_successor_principal_generation INTEGER NOT NULL CHECK (expected_successor_principal_generation>=1),
  expected_successor_bridge_revision INTEGER NOT NULL CHECK (expected_successor_bridge_revision>=1),
  expected_successor_bridge_generation INTEGER NOT NULL CHECK (expected_successor_bridge_generation>=1),
  provider_adapter_id TEXT NOT NULL,
  provider_contract_digest TEXT NOT NULL,
  source_provider_action_id TEXT NOT NULL,
  promotion_action_id TEXT NOT NULL,
  provider_session_ref TEXT NOT NULL,
  provider_session_generation INTEGER NOT NULL CHECK (provider_session_generation>=1),
  new_bridge_generation INTEGER NOT NULL CHECK (new_bridge_generation>=2),
  state TEXT NOT NULL CHECK (state IN ('prepared','dispatched','committing','terminal','no-effect','ambiguous')),
  result_json TEXT,
  revision INTEGER NOT NULL CHECK (revision>=1),
  created_at INTEGER NOT NULL,
  updated_at INTEGER NOT NULL,
  UNIQUE(operator_id, operator_command_id),
  UNIQUE(provider_adapter_id, promotion_action_id),
  FOREIGN KEY(project_session_id, coordination_run_id) REFERENCES runs(project_session_id, run_id),
  FOREIGN KEY(coordination_run_id, predecessor_agent_id) REFERENCES agents(run_id, agent_id),
  FOREIGN KEY(coordination_run_id, successor_agent_id) REFERENCES agents(run_id, agent_id),
  CHECK (length(intent_digest)=71 AND substr(intent_digest,1,7)='sha256:'),
  CHECK (length(handoff_digest)=71 AND substr(handoff_digest,1,7)='sha256:'),
  CHECK (length(successor_authority_digest)=71 AND substr(successor_authority_digest,1,7)='sha256:'),
  CHECK (length(provider_contract_digest)=71 AND substr(provider_contract_digest,1,7)='sha256:')
);

CREATE TABLE chair_live_handoff_resolutions (
  custody_id TEXT PRIMARY KEY REFERENCES chair_live_handoff_custody(custody_id),
  project_session_id TEXT NOT NULL,
  coordination_run_id TEXT NOT NULL,
  predecessor_agent_id TEXT NOT NULL,
  successor_agent_id TEXT NOT NULL,
  promotion_action_id TEXT NOT NULL,
  new_chair_generation INTEGER NOT NULL CHECK (new_chair_generation>=2),
  new_bridge_generation INTEGER NOT NULL CHECK (new_bridge_generation>=2),
  evidence_digest TEXT NOT NULL,
  created_at INTEGER NOT NULL,
  CHECK (length(evidence_digest)=71 AND substr(evidence_digest,1,7)='sha256:')
);

CREATE TABLE child_bridge_losses (
  loss_id TEXT PRIMARY KEY,
  run_id TEXT NOT NULL,
  agent_id TEXT NOT NULL,
  adapter_id TEXT NOT NULL,
  action_id TEXT NOT NULL,
  provider_session_ref TEXT NOT NULL,
  provider_session_generation INTEGER NOT NULL CHECK (provider_session_generation >= 1),
  lost_bridge_generation INTEGER NOT NULL CHECK (lost_bridge_generation >= 1),
  next_bridge_generation INTEGER NOT NULL CHECK (next_bridge_generation >= 2),
  capability_hash TEXT NOT NULL,
  daemon_instance_generation INTEGER NOT NULL CHECK (daemon_instance_generation >= 1),
  reason TEXT NOT NULL CHECK (length(reason) BETWEEN 1 AND 160),
  evidence_digest TEXT NOT NULL,
  created_at INTEGER NOT NULL,
  UNIQUE (run_id, agent_id, lost_bridge_generation),
  FOREIGN KEY (run_id, agent_id) REFERENCES agents(run_id, agent_id),
  FOREIGN KEY (adapter_id, action_id) REFERENCES provider_agent_custody(adapter_id, action_id),
  FOREIGN KEY (capability_hash) REFERENCES capabilities(token_hash),
  CHECK (next_bridge_generation=lost_bridge_generation+1),
  CHECK (length(evidence_digest)=71 AND substr(evidence_digest,1,7)='sha256:')
);

CREATE TABLE commands (
  run_id TEXT NOT NULL,
  actor_agent_id TEXT NOT NULL,
  command_id TEXT NOT NULL,
  payload_hash TEXT NOT NULL,
  result_json TEXT NOT NULL,
  created_at INTEGER NOT NULL,
  PRIMARY KEY (run_id, actor_agent_id, command_id)
);

CREATE TABLE daemon_global_state (
  singleton INTEGER PRIMARY KEY CHECK (singleton = 1),
  revision INTEGER NOT NULL CHECK (revision >= 1)
);

CREATE TABLE daemon_runtime_epochs (
  instance_generation INTEGER PRIMARY KEY CHECK (instance_generation >= 1),
  instance_id TEXT NOT NULL UNIQUE,
  state TEXT NOT NULL CHECK (state IN ('starting','running','quiescing','stopped','crashed')),
  observed_global_revision INTEGER CHECK (
    observed_global_revision IS NULL OR observed_global_revision >= 1
  ),
  started_at INTEGER NOT NULL,
  heartbeat_at INTEGER NOT NULL,
  stopped_at INTEGER
);

CREATE TABLE deliveries (
  delivery_id TEXT PRIMARY KEY,
  message_id TEXT NOT NULL REFERENCES messages(message_id),
  run_id TEXT NOT NULL,
  recipient_id TEXT NOT NULL,
  mailbox_sequence INTEGER NOT NULL,
  state TEXT NOT NULL,
  attempt_count INTEGER NOT NULL DEFAULT 0,
  claim_deadline INTEGER,
  acknowledged_at INTEGER,
  resolution_reason TEXT,
  resolved_at INTEGER,
  UNIQUE (message_id, recipient_id),
  UNIQUE (run_id, recipient_id, mailbox_sequence)
);

CREATE TABLE delivery_freezes (
  run_id TEXT NOT NULL,
  agent_id TEXT NOT NULL,
  reason TEXT NOT NULL,
  created_at INTEGER NOT NULL,
  PRIMARY KEY (run_id, agent_id),
  FOREIGN KEY (run_id, agent_id) REFERENCES agents(run_id, agent_id)
);

CREATE TABLE dependency_mutation_guards (
  run_id TEXT PRIMARY KEY REFERENCES runs(run_id),
  project_session_id TEXT NOT NULL,
  target_revision INTEGER NOT NULL CHECK (target_revision >= 1),
  expected_edge_count INTEGER NOT NULL CHECK (expected_edge_count >= 0),
  expected_binding_count INTEGER NOT NULL CHECK (expected_binding_count >= 0)
);

CREATE TABLE discussion_group_members (
  run_id TEXT NOT NULL,
  group_id TEXT NOT NULL,
  agent_id TEXT NOT NULL,
  PRIMARY KEY (run_id, group_id, agent_id),
  FOREIGN KEY (run_id, group_id) REFERENCES discussion_groups(run_id, group_id),
  FOREIGN KEY (run_id, agent_id) REFERENCES agents(run_id, agent_id)
);

CREATE TABLE discussion_groups (
  run_id TEXT NOT NULL,
  group_id TEXT NOT NULL,
  team_id TEXT,
  created_by TEXT NOT NULL,
  PRIMARY KEY (run_id, group_id)
);

CREATE TABLE events (
  event_id TEXT PRIMARY KEY,
  run_id TEXT NOT NULL,
  type TEXT NOT NULL,
  actor_agent_id TEXT,
  payload_json TEXT NOT NULL,
  created_at INTEGER NOT NULL
);

CREATE TABLE git_custody_resolutions(
  resolution_id TEXT PRIMARY KEY,
  draft_id TEXT NOT NULL UNIQUE REFERENCES git_operation_drafts(draft_id),
  resolution_operation_id TEXT NOT NULL UNIQUE REFERENCES operation_admissions(operation_id),
  target_custody_id TEXT NOT NULL UNIQUE REFERENCES operator_git_effect_bindings(custody_id),
  target_operation_id TEXT NOT NULL,
  project_id TEXT NOT NULL,
  project_session_id TEXT NOT NULL,
  coordination_run_id TEXT NOT NULL,
  expected_lookup_generation INTEGER NOT NULL CHECK(expected_lookup_generation>=1),
  lookup_evidence_digest TEXT NOT NULL,
  eligibility_reason TEXT NOT NULL CHECK(eligibility_reason IN (
    'inspector-unavailable','remote-proof-permanently-unavailable','mixed-local-remote-evidence',
    'evidence-integrity-failure','conflict-state-unverifiable'
  )),
  adjudication TEXT NOT NULL CHECK(adjudication IN ('applied','no-effect','quarantine-accepted')),
  reason TEXT NOT NULL CHECK(length(reason)>0),
  gate_id TEXT NOT NULL,
  gate_revision INTEGER NOT NULL CHECK(gate_revision>=1),
  resolved_by_operator_id TEXT NOT NULL,
  operator_input_record_digest TEXT NOT NULL,
  reservation_disposition TEXT NOT NULL CHECK(reservation_disposition IN ('released','retired')),
  resolution_digest TEXT NOT NULL,
  created_at INTEGER NOT NULL,
  FOREIGN KEY(project_session_id,coordination_run_id) REFERENCES runs(project_session_id,run_id),
  FOREIGN KEY(target_operation_id) REFERENCES operation_admissions(operation_id),
  FOREIGN KEY(gate_id,resolution_operation_id) REFERENCES scoped_gate_operations(gate_id,operation_id),
  CHECK((adjudication='quarantine-accepted')=(reservation_disposition='retired'))
);

CREATE TABLE git_execution_profiles(
  profile_id TEXT NOT NULL,
  revision INTEGER NOT NULL CHECK(revision>=1),
  profile_digest TEXT NOT NULL,
  git_binary_path TEXT NOT NULL,
  git_binary_version TEXT NOT NULL,
  git_binary_digest TEXT NOT NULL,
  object_format TEXT NOT NULL CHECK(object_format IN ('sha1','sha256')),
  merge_backend_id TEXT NOT NULL,
  rebase_backend_id TEXT NOT NULL,
  environment_digest TEXT NOT NULL,
  helper_registry_digest TEXT NOT NULL,
  inspector_digest TEXT NOT NULL,
  state TEXT NOT NULL CHECK(state IN ('active','revoked')),
  created_at INTEGER NOT NULL,
  PRIMARY KEY(profile_id,revision),
  UNIQUE(profile_id,revision,profile_digest),
  CHECK(length(profile_digest)=71 AND substr(profile_digest,1,7)='sha256:'),
  CHECK(length(git_binary_digest)=71 AND substr(git_binary_digest,1,7)='sha256:'),
  CHECK(length(environment_digest)=71 AND substr(environment_digest,1,7)='sha256:'),
  CHECK(length(helper_registry_digest)=71 AND substr(helper_registry_digest,1,7)='sha256:'),
  CHECK(length(inspector_digest)=71 AND substr(inspector_digest,1,7)='sha256:')
);

CREATE TABLE git_mutation_reservations(
  custody_id TEXT NOT NULL,
  generation INTEGER NOT NULL CHECK(generation>=1),
  project_id TEXT NOT NULL,
  project_session_id TEXT NOT NULL,
  coordination_run_id TEXT NOT NULL,
  git_common_dir TEXT NOT NULL,
  common_dir_identity_digest TEXT NOT NULL,
  lock_plan_digest TEXT NOT NULL,
  state TEXT NOT NULL CHECK(state IN (
    'reserved','dispatching','conflict','ambiguous','quarantined','released','retired'
  )),
  owner_instance_id TEXT NOT NULL,
  created_at INTEGER NOT NULL,
  updated_at INTEGER NOT NULL,
  PRIMARY KEY(custody_id,generation),
  FOREIGN KEY(custody_id) REFERENCES operator_effect_custody(custody_id),
  FOREIGN KEY(project_session_id,coordination_run_id) REFERENCES runs(project_session_id,run_id)
);

CREATE TABLE git_operation_drafts(
  draft_id TEXT PRIMARY KEY,
  revision INTEGER NOT NULL CHECK(revision>=1),
  draft_request_id TEXT NOT NULL,
  request_digest TEXT NOT NULL,
  operator_id TEXT NOT NULL,
  project_id TEXT NOT NULL,
  project_session_id TEXT NOT NULL,
  observed_session_revision INTEGER NOT NULL CHECK(observed_session_revision>=1),
  session_generation INTEGER NOT NULL CHECK(session_generation>=1),
  coordination_run_id TEXT NOT NULL,
  observed_run_revision INTEGER NOT NULL CHECK(observed_run_revision>=1),
  observed_dependency_revision INTEGER NOT NULL CHECK(observed_dependency_revision>=1),
  authority_ref TEXT NOT NULL,
  authority_revision INTEGER NOT NULL CHECK(authority_revision>=1),
  git_allowlist_epoch INTEGER NOT NULL CHECK(git_allowlist_epoch>=1),
  git_allowlist_digest TEXT,
  draft_kind TEXT NOT NULL CHECK(draft_kind IN ('mutation','custody-resolution')),
  operation_id TEXT NOT NULL UNIQUE,
  operation_kind TEXT NOT NULL,
  payload_digest TEXT NOT NULL,
  binding_json TEXT NOT NULL CHECK(json_valid(binding_json)),
  draft_digest TEXT NOT NULL,
  state TEXT NOT NULL CHECK(state IN ('open','gate-bound','consumed','stale','expired','cancelled')),
  expires_at INTEGER NOT NULL,
  consumed_command_id TEXT,
  terminal_reason TEXT,
  created_at INTEGER NOT NULL,
  updated_at INTEGER NOT NULL,
  UNIQUE(operator_id,project_id,project_session_id,draft_request_id),
  UNIQUE(draft_id,operation_id),
  FOREIGN KEY(project_session_id,coordination_run_id) REFERENCES runs(project_session_id,run_id),
  FOREIGN KEY(operation_id) REFERENCES operation_admissions(operation_id),
  CHECK((state='consumed')=(consumed_command_id IS NOT NULL)),
  CHECK((state IN ('stale','expired','cancelled'))=(terminal_reason IS NOT NULL))
);

CREATE TABLE git_remote_registrations(
  registration_id TEXT NOT NULL,
  revision INTEGER NOT NULL CHECK(revision>=1),
  generation INTEGER NOT NULL CHECK(generation>=1),
  project_id TEXT NOT NULL REFERENCES projects(project_id),
  remote_name TEXT NOT NULL,
  transport_kind TEXT NOT NULL CHECK(transport_kind IN ('local','ssh','https','provider-port')),
  target_identity TEXT NOT NULL,
  target_digest TEXT NOT NULL,
  adapter_id TEXT NOT NULL,
  adapter_contract_digest TEXT NOT NULL,
  credential_selector_digest TEXT NOT NULL,
  state TEXT NOT NULL CHECK(state IN ('active','revoked')),
  created_at INTEGER NOT NULL,
  PRIMARY KEY(registration_id,revision),
  UNIQUE(registration_id,revision,generation,target_digest),
  CHECK(length(target_digest)=71 AND substr(target_digest,1,7)='sha256:'),
  CHECK(length(adapter_contract_digest)=71 AND substr(adapter_contract_digest,1,7)='sha256:'),
  CHECK(length(credential_selector_digest)=71 AND substr(credential_selector_digest,1,7)='sha256:')
);

CREATE TABLE intake_artifact_bindings (
  intake_id TEXT NOT NULL,
  intake_revision INTEGER NOT NULL,
  artifact_id TEXT NOT NULL REFERENCES artifacts(artifact_id),
  relative_path TEXT NOT NULL,
  sha256 TEXT NOT NULL,
  PRIMARY KEY(intake_id, intake_revision, artifact_id),
  UNIQUE(intake_id, intake_revision, relative_path, sha256),
  FOREIGN KEY(intake_id, intake_revision) REFERENCES intake_revisions(intake_id, revision)
);

CREATE TABLE intake_gate_bindings (
  intake_id TEXT NOT NULL,
  intake_revision INTEGER NOT NULL,
  gate_id TEXT NOT NULL,
  gate_revision INTEGER NOT NULL CHECK (gate_revision >= 1),
  PRIMARY KEY(intake_id, intake_revision, gate_id),
  FOREIGN KEY(intake_id, intake_revision) REFERENCES intake_revisions(intake_id, revision)
);

CREATE TABLE intake_revisions (
  intake_id TEXT NOT NULL REFERENCES intakes(intake_id),
  revision INTEGER NOT NULL CHECK (revision >= 1),
  state TEXT NOT NULL,
  payload_json TEXT NOT NULL,
  payload_digest TEXT NOT NULL,
  actor_ref TEXT NOT NULL,
  created_at INTEGER NOT NULL, accepted_scope_artifact_id TEXT REFERENCES artifacts(artifact_id), accepted_scope_state TEXT NOT NULL DEFAULT 'not-applicable',
  PRIMARY KEY(intake_id, revision)
);

CREATE TABLE intakes (
  intake_id TEXT PRIMARY KEY,
  project_id TEXT NOT NULL REFERENCES projects(project_id),
  project_session_id TEXT,
  coordination_run_id TEXT,
  dedupe_key TEXT NOT NULL,
  state TEXT NOT NULL CHECK (state IN ('draft','awaiting-chair','discussing','awaiting-human','accepted','deferred','cancelled')),
  revision INTEGER NOT NULL CHECK (revision >= 1),
  chair_request_id TEXT,
  chair_request_revision INTEGER,
  summary TEXT NOT NULL,
  artifact_refs_json TEXT NOT NULL,
  gate_ids_json TEXT NOT NULL,
  payload_digest TEXT NOT NULL,
  created_at INTEGER NOT NULL,
  updated_at INTEGER NOT NULL, accepted_scope_artifact_id TEXT REFERENCES artifacts(artifact_id), accepted_scope_state TEXT NOT NULL DEFAULT 'not-applicable',
  UNIQUE(project_id, dedupe_key),
  FOREIGN KEY(project_session_id, coordination_run_id) REFERENCES runs(project_session_id, run_id),
  CHECK ((state = 'draft' AND project_session_id IS NULL AND coordination_run_id IS NULL) OR
         (state <> 'draft' AND project_session_id IS NOT NULL AND coordination_run_id IS NOT NULL))
);

CREATE TABLE integration_availability (
  integration_id TEXT PRIMARY KEY,
  state TEXT NOT NULL CHECK (state IN ('available','unavailable','stale')),
  discovered_contract_json TEXT NOT NULL,
  checked_at INTEGER NOT NULL
);

CREATE TABLE launched_chair_bridge_retirements(
  project_session_id TEXT NOT NULL,
  coordination_run_id TEXT NOT NULL,
  source_kind TEXT NOT NULL CHECK(source_kind IN (
    'project-session-close','project-session-stop','chair-recovery-abandon'
  )),
  terminal_kind TEXT NOT NULL CHECK(terminal_kind IN (
    'accepted','cancelled','failed','closed','launch-failed'
  )),
  terminal_ref TEXT NOT NULL CHECK(length(terminal_ref)>0),
  owner_operator_id TEXT NOT NULL CHECK(length(owner_operator_id)>0),
  owner_ref TEXT NOT NULL CHECK(length(owner_ref)>0),
  created_at INTEGER NOT NULL,
  PRIMARY KEY(project_session_id,coordination_run_id),
  FOREIGN KEY(project_session_id,coordination_run_id)
    REFERENCES launched_chair_bridge_state(project_session_id,coordination_run_id)
);

CREATE TABLE launched_chair_bridge_state (
  project_session_id TEXT NOT NULL,
  coordination_run_id TEXT NOT NULL,
  chair_agent_id TEXT NOT NULL,
  provider_adapter_id TEXT NOT NULL,
  provider_action_id TEXT NOT NULL,
  provider_contract_digest TEXT NOT NULL,
  provider_session_ref TEXT NOT NULL,
  provider_session_generation INTEGER NOT NULL CHECK (provider_session_generation >= 1),
  principal_generation INTEGER NOT NULL CHECK (principal_generation >= 1),
  bridge_generation INTEGER NOT NULL CHECK (bridge_generation >= 1),
  capability_hash TEXT NOT NULL,
  activation_evidence_digest TEXT NOT NULL,
  state TEXT NOT NULL CHECK (state IN ('active','lost','abandoned')),
  revision INTEGER NOT NULL CHECK (revision >= 1),
  created_at INTEGER NOT NULL,
  updated_at INTEGER NOT NULL,
  PRIMARY KEY (project_session_id, coordination_run_id),
  UNIQUE (provider_adapter_id, provider_action_id),
  UNIQUE (coordination_run_id, chair_agent_id, bridge_generation),
  FOREIGN KEY (project_session_id, coordination_run_id)
    REFERENCES runs(project_session_id, run_id),
  FOREIGN KEY (coordination_run_id, chair_agent_id)
    REFERENCES agents(run_id, agent_id),
  FOREIGN KEY (provider_adapter_id, provider_action_id)
    REFERENCES provider_actions(adapter_id, action_id),
  FOREIGN KEY (capability_hash) REFERENCES capabilities(token_hash),
  CHECK (length(provider_contract_digest)=71 AND substr(provider_contract_digest,1,7)='sha256:'),
  CHECK (length(activation_evidence_digest)=71 AND substr(activation_evidence_digest,1,7)='sha256:')
);

CREATE TABLE leases (
  lease_id TEXT PRIMARY KEY,
  run_id TEXT NOT NULL,
  kind TEXT NOT NULL,
  holder_agent_id TEXT NOT NULL,
  generation INTEGER NOT NULL,
  status TEXT NOT NULL,
  expires_at INTEGER NOT NULL,
  updated_at INTEGER NOT NULL
);

CREATE TABLE lifecycle_checkpoints (
  checkpoint_id TEXT PRIMARY KEY,
  run_id TEXT NOT NULL REFERENCES runs(run_id),
  agent_id TEXT NOT NULL,
  task_id TEXT NOT NULL,
  task_revision INTEGER NOT NULL,
  relative_path TEXT NOT NULL,
  sha256 TEXT NOT NULL,
  checkpoint_json TEXT NOT NULL,
  created_at INTEGER NOT NULL,
  UNIQUE (run_id, agent_id, sha256)
);

CREATE TABLE lifecycle_operations (
  operation_id TEXT PRIMARY KEY,
  run_id TEXT NOT NULL REFERENCES runs(run_id),
  agent_id TEXT NOT NULL,
  action TEXT NOT NULL,
  task_id TEXT NOT NULL,
  task_revision INTEGER NOT NULL,
  checkpoint_sha256 TEXT NOT NULL,
  prior_resume_reference TEXT,
  replacement_resume_reference TEXT,
  created_at INTEGER NOT NULL
);

CREATE TABLE agent_lifecycle_identity_high_water (
  run_id TEXT NOT NULL,
  agent_id TEXT NOT NULL,
  provider_generation INTEGER NOT NULL CHECK(provider_generation BETWEEN 1 AND 9007199254740991),
  principal_generation INTEGER NOT NULL CHECK(principal_generation BETWEEN 1 AND 9007199254740991),
  revision INTEGER NOT NULL CHECK(revision BETWEEN 1 AND 9007199254740991),
  PRIMARY KEY(run_id, agent_id)
) STRICT;
CREATE TABLE agent_lifecycle_bridge_high_water (
  run_id TEXT NOT NULL,
  agent_id TEXT NOT NULL,
  bridge_owner_kind TEXT NOT NULL CHECK(bridge_owner_kind IN ('chair','child')),
  bridge_generation INTEGER NOT NULL CHECK(bridge_generation BETWEEN 1 AND 9007199254740991),
  revision INTEGER NOT NULL CHECK(revision BETWEEN 1 AND 9007199254740991),
  PRIMARY KEY(run_id, agent_id, bridge_owner_kind)
) STRICT;
CREATE TABLE agent_lifecycle_context_high_water (
  run_id TEXT NOT NULL,
  agent_id TEXT NOT NULL,
  provider_generation INTEGER NOT NULL CHECK(provider_generation BETWEEN 1 AND 9007199254740991),
  context_revision INTEGER NOT NULL CHECK(context_revision BETWEEN 0 AND 9007199254740991),
  revision INTEGER NOT NULL CHECK(revision BETWEEN 1 AND 9007199254740991),
  PRIMARY KEY(run_id, agent_id, provider_generation)
) STRICT;
CREATE TABLE provider_context_observation_audit (
  observation_id TEXT PRIMARY KEY,
  source_event_id TEXT,
  run_id TEXT,
  agent_id TEXT,
  provider_generation INTEGER,
  context_revision INTEGER,
  classification TEXT,
  evidence_digest TEXT,
  observed_at INTEGER,
  UNIQUE(run_id, agent_id, source_event_id),
  UNIQUE(run_id, agent_id, source_event_id, provider_generation,
      context_revision, evidence_digest)
) STRICT;
CREATE TABLE lifecycle_rotation_custodies (
  project_session_id TEXT,
  run_id TEXT,
  agent_id TEXT,
  custody_id TEXT,
  command_id TEXT,
  admission_digest TEXT,
  provider_action_adapter_id TEXT,
  provider_action_id TEXT,
  recovery_source_kind TEXT,
  recovery_from_custody_id TEXT,
  recovery_from_custody_revision INTEGER,
  recovery_from_generation_loss_id TEXT,
  recovery_from_generation_loss_revision INTEGER,
  recovery_source_ref_digest TEXT,
  recovery_source_journal_digest TEXT,
  bridge_owner_kind TEXT,
  caller_turn_lease_id TEXT,
  caller_turn_generation INTEGER,
  predecessor_turn_set_digest TEXT,
  quarantined_write_set_digest TEXT,
  delivery_cut_watermark INTEGER,
  adoption_delivery_set_digest TEXT,
  checkpoint_ref TEXT,
  checkpoint_digest TEXT,
  checkpoint_validation_revision INTEGER,
  checkpoint_validation_digest TEXT,
  checkpoint_validation_key TEXT,
  task_revision INTEGER,
  mailbox_revision INTEGER,
  child_set_digest TEXT,
  open_work_set_digest TEXT,
  source_provider_session_ref TEXT,
  source_capability_hash TEXT,
  source_custody_action_id TEXT,
  source_adapter_id TEXT,
  source_adapter_contract_digest TEXT,
  source_bridge_row_id TEXT,
  source_bridge_revision INTEGER,
  source_provider_generation INTEGER,
  source_principal_generation INTEGER,
  source_bridge_generation INTEGER,
  source_project_session_generation INTEGER,
  source_run_generation INTEGER,
  source_chair_lease_generation INTEGER,
  target_provider_generation INTEGER,
  target_principal_generation INTEGER,
  target_bridge_generation INTEGER,
  replacement_adapter_id TEXT,
  replacement_contract_digest TEXT,
  staged_capability_hash TEXT,
  launch_attest_challenge_digest TEXT,
  precondition_digest TEXT,
  origin_fresh_handoff_id TEXT,
  origin_fresh_handoff_digest TEXT,
  origin_operation TEXT,
  origin_fresh_apply_plan_digest TEXT,
  creation_json TEXT,
  creation_digest TEXT,
  created_at INTEGER,
  PRIMARY KEY(run_id,agent_id,custody_id),
  UNIQUE(project_session_id,run_id,agent_id,custody_id),
  UNIQUE(provider_action_adapter_id,provider_action_id),
  UNIQUE(creation_digest),
  CHECK((recovery_source_kind='none' AND
        recovery_from_custody_id IS NULL AND
        recovery_from_custody_revision IS NULL AND
        recovery_from_generation_loss_id IS NULL AND
        recovery_from_generation_loss_revision IS NULL AND
        recovery_source_ref_digest IS NULL AND
        recovery_source_journal_digest IS NULL AND
        origin_fresh_handoff_id IS NULL AND origin_fresh_handoff_digest IS NULL AND
        origin_operation IS NULL AND origin_fresh_apply_plan_digest IS NULL) OR
      (recovery_source_kind='custody' AND
        recovery_from_custody_id IS NOT NULL AND
        recovery_from_custody_revision IS NOT NULL AND
        recovery_from_generation_loss_id IS NULL AND
        recovery_from_generation_loss_revision IS NULL AND
        recovery_source_ref_digest IS NOT NULL AND
        recovery_source_journal_digest IS NOT NULL AND
        origin_fresh_handoff_id IS NOT NULL AND
        origin_fresh_handoff_digest IS NOT NULL AND
        origin_operation='fresh-rotate' AND
        origin_fresh_apply_plan_digest IS NOT NULL) OR
      (recovery_source_kind='generation-loss' AND
        recovery_from_custody_id IS NULL AND
        recovery_from_custody_revision IS NULL AND
        recovery_from_generation_loss_id IS NOT NULL AND
        recovery_from_generation_loss_revision IS NOT NULL AND
        recovery_source_ref_digest IS NOT NULL AND
        recovery_source_journal_digest IS NOT NULL AND
        origin_fresh_handoff_id IS NOT NULL AND
        origin_fresh_handoff_digest IS NOT NULL AND
        origin_operation='fresh-rotate' AND
        origin_fresh_apply_plan_digest IS NOT NULL)),
  CHECK((checkpoint_validation_digest IS NULL AND
        checkpoint_validation_key='none') OR
      (checkpoint_validation_digest IS NOT NULL AND
        checkpoint_validation_key=checkpoint_validation_digest)),
  CHECK(origin_fresh_handoff_id IS NULL OR
      (provider_action_adapter_id=replacement_adapter_id AND
        replacement_contract_digest IS NOT NULL)),
  FOREIGN KEY(provider_action_adapter_id,provider_action_id)
      REFERENCES provider_actions(adapter_id,action_id),
  FOREIGN KEY(source_adapter_id,source_custody_action_id)
      REFERENCES provider_actions(adapter_id,action_id),
  FOREIGN KEY(project_session_id,run_id,agent_id,recovery_from_custody_id,
        recovery_from_custody_revision,recovery_source_ref_digest)
      REFERENCES lifecycle_rotation_custody_revisions(
        project_session_id,run_id,agent_id,custody_id,revision,source_ref_digest),
  FOREIGN KEY(project_session_id,run_id,agent_id,
        recovery_from_generation_loss_id,recovery_from_generation_loss_revision,
        recovery_source_ref_digest)
      REFERENCES lifecycle_generation_loss_revisions(
        project_session_id,run_id,agent_id,generation_loss_id,revision,
        source_ref_digest),
  FOREIGN KEY(origin_fresh_handoff_id,origin_fresh_handoff_digest,
        project_session_id,run_id,agent_id,recovery_source_kind,
        recovery_source_ref_digest,recovery_source_journal_digest,custody_id,
        provider_action_adapter_id,provider_action_id,checkpoint_ref,
        checkpoint_digest,checkpoint_validation_key,replacement_contract_digest,
        origin_operation,target_provider_generation,target_principal_generation,
        target_bridge_generation,admission_digest,origin_fresh_apply_plan_digest)
      REFERENCES lifecycle_fresh_recovery_handoffs(
        handoff_id,handoff_digest,project_session_id,run_id,agent_id,
        recovery_source_kind,recovery_source_ref_digest,source_journal_digest,
        new_custody_id,provider_action_adapter_id,provider_action_id,checkpoint_ref,
        checkpoint_digest,checkpoint_validation_key,adapter_contract_digest,
        operation,reserved_provider_generation,reserved_principal_generation,
        reserved_bridge_generation,admission_digest,fresh_apply_plan_digest)
      DEFERRABLE INITIALLY DEFERRED
) STRICT;
CREATE TABLE lifecycle_rotation_custody_revisions (
  project_session_id TEXT,
  run_id TEXT,
  agent_id TEXT,
  custody_id TEXT,
  revision INTEGER CHECK(revision >= 1),
  prior_revision INTEGER,
  prior_journal_digest TEXT,
  state TEXT CHECK(state IN ('awaiting-boundary','prepared','dispatched','accepted',
      'ambiguous','provider-terminal','committing','finalized')),
  disposition_code TEXT CHECK(disposition_code IN
      ('none','adopted','no-effect','quarantined','superseded','abandoned')),
  proof_kind TEXT CHECK(proof_kind IN ('none','zero-dispatch-no-effect',
      'predispatch-superseded','postterminal-adoption-cas-superseded',
      'fresh-handoff-superseded','provider-terminal','provider-no-effect',
      'integrity-quarantine','confirmed-abandon')),
  terminal_evidence_digest TEXT,
  semantic_json TEXT,
  semantic_digest TEXT,
  source_ref_digest TEXT,
  origin_fresh_apply_id TEXT,
  origin_fresh_apply_digest TEXT,
  receipt_batch_id TEXT,
  receipt_apply_id TEXT,
  receipt_apply_digest TEXT,
  journal_json TEXT,
  journal_digest TEXT,
  recorded_at INTEGER,
  PRIMARY KEY(run_id,agent_id,custody_id,revision),
  UNIQUE(project_session_id,run_id,agent_id,custody_id,revision),
  UNIQUE(project_session_id,run_id,agent_id,custody_id,revision,
      source_ref_digest),
  UNIQUE(project_session_id,run_id,agent_id,custody_id,revision,
      journal_digest),
  UNIQUE(project_session_id,run_id,agent_id,custody_id,revision,
      source_ref_digest,journal_digest),
  UNIQUE(project_session_id,run_id,agent_id,custody_id,revision,
      semantic_digest,source_ref_digest,journal_digest),
  UNIQUE(project_session_id,run_id,agent_id,custody_id,revision,
      semantic_digest,source_ref_digest,journal_digest,origin_fresh_apply_id,
      origin_fresh_apply_digest),
  UNIQUE(project_session_id,run_id,agent_id,custody_id,revision,state,
      disposition_code,semantic_digest,source_ref_digest,journal_digest),
  UNIQUE(project_session_id,run_id,agent_id,custody_id,revision,
      disposition_code,source_ref_digest,journal_digest),
  UNIQUE(project_session_id,run_id,agent_id,custody_id,revision,
      disposition_code,terminal_evidence_digest,source_ref_digest,journal_digest),
  UNIQUE(semantic_digest),
  UNIQUE(source_ref_digest),
  UNIQUE(journal_digest),
  CHECK((revision=1 AND prior_revision IS NULL AND
        prior_journal_digest IS NULL) OR
      (revision>1 AND prior_revision=revision-1 AND
        prior_journal_digest IS NOT NULL)),
  CHECK((state='finalized')=(disposition_code<>'none')),
  CHECK((state='finalized')=
      (receipt_batch_id IS NOT NULL AND receipt_apply_id IS NOT NULL AND
        receipt_apply_digest IS NOT NULL)),
  CHECK((receipt_batch_id IS NULL)=(receipt_apply_id IS NULL)),
  CHECK((receipt_batch_id IS NULL)=(receipt_apply_digest IS NULL)),
  CHECK((origin_fresh_apply_id IS NULL)=(origin_fresh_apply_digest IS NULL)),
  CHECK(origin_fresh_apply_id IS NULL OR
      (revision=1 AND state<>'finalized' AND receipt_batch_id IS NULL)),
  CHECK((state IN ('provider-terminal','committing','finalized'))=
      (terminal_evidence_digest IS NOT NULL)),
  CHECK((state='finalized')=(proof_kind<>'none')),
  FOREIGN KEY(project_session_id,run_id,agent_id,custody_id)
      REFERENCES lifecycle_rotation_custodies(
        project_session_id,run_id,agent_id,custody_id),
  FOREIGN KEY(project_session_id,run_id,agent_id,custody_id,prior_revision,
        prior_journal_digest)
      REFERENCES lifecycle_rotation_custody_revisions(
        project_session_id,run_id,agent_id,custody_id,revision,journal_digest),
  FOREIGN KEY(receipt_batch_id,receipt_apply_id,project_session_id,run_id,agent_id,
        custody_id,revision,semantic_digest,source_ref_digest)
      REFERENCES lifecycle_receipt_custody_effects(
        batch_id,planned_apply_id,project_session_id,run_id,agent_id,custody_id,
        final_revision,final_semantic_digest,final_source_ref_digest)
      DEFERRABLE INITIALLY DEFERRED,
  FOREIGN KEY(receipt_apply_id,receipt_apply_digest,receipt_batch_id)
      REFERENCES lifecycle_transition_applies(
        apply_id,apply_digest,receipt_batch_id)
      DEFERRABLE INITIALLY DEFERRED,
  FOREIGN KEY(receipt_apply_id,receipt_batch_id)
      REFERENCES lifecycle_transition_applies(apply_id,receipt_batch_id)
      DEFERRABLE INITIALLY DEFERRED,
  FOREIGN KEY(origin_fresh_apply_id,origin_fresh_apply_digest,custody_id,
        semantic_digest,source_ref_digest)
      REFERENCES lifecycle_transition_applies(
        apply_id,apply_digest,new_custody_id,new_custody_semantic_digest,
        new_custody_source_ref_digest)
      DEFERRABLE INITIALLY DEFERRED
) STRICT;
CREATE TABLE lifecycle_rotation_custody_heads (
  project_session_id TEXT NOT NULL,
  run_id TEXT NOT NULL,
  agent_id TEXT NOT NULL,
  custody_id TEXT NOT NULL,
  current_revision INTEGER NOT NULL,
  state TEXT NOT NULL,
  disposition_code TEXT NOT NULL,
  semantic_digest TEXT NOT NULL,
  source_ref_digest TEXT NOT NULL,
  journal_digest TEXT NOT NULL,
  terminal INTEGER NOT NULL CHECK(terminal IN (0,1)),
  head_revision INTEGER NOT NULL CHECK(head_revision >= 1),
  PRIMARY KEY(run_id,agent_id,custody_id),
  UNIQUE(project_session_id,run_id,agent_id,custody_id),
  FOREIGN KEY(project_session_id,run_id,agent_id,custody_id,current_revision,
        state,disposition_code,semantic_digest,source_ref_digest,journal_digest)
      REFERENCES lifecycle_rotation_custody_revisions(
        project_session_id,run_id,agent_id,custody_id,revision,state,
        disposition_code,semantic_digest,source_ref_digest,journal_digest),
  CHECK((terminal=1)=(state='finalized')),
  CHECK((state='finalized')=(disposition_code<>'none'))
) STRICT;
CREATE UNIQUE INDEX one_nonfinal_lifecycle_custody_per_agent
  ON lifecycle_rotation_custody_heads(run_id,agent_id)
  WHERE terminal=0;
CREATE TABLE lifecycle_receipt_projects (
  project_id TEXT PRIMARY KEY,
  authority_id TEXT NOT NULL,
  registered_at INTEGER NOT NULL,
  UNIQUE(project_id,authority_id),
  FOREIGN KEY(project_id) REFERENCES projects(project_id)
) STRICT;
CREATE TABLE lifecycle_scope_admission_outbox (
  admission_request_id TEXT PRIMARY KEY,
  project_id TEXT NOT NULL,
  project_session_id TEXT NOT NULL,
  run_id TEXT NOT NULL,
  authority_id TEXT NOT NULL,
  admission_digest TEXT NOT NULL,
  admitted_at INTEGER NOT NULL,
  scope_json TEXT NOT NULL,
  scope_digest TEXT NOT NULL UNIQUE,
  created_at INTEGER NOT NULL,
  UNIQUE(project_session_id,run_id),
  UNIQUE(admission_request_id,project_id,project_session_id,run_id,authority_id,
      admission_digest,admitted_at,scope_digest),
  FOREIGN KEY(project_id,authority_id)
      REFERENCES lifecycle_receipt_projects(project_id,authority_id)
) STRICT;
CREATE TABLE lifecycle_admitted_run_scopes (
  project_id TEXT,
  project_session_id TEXT,
  run_id TEXT,
  authority_id TEXT,
  admission_digest TEXT,
  admitted_at INTEGER,
  admission_request_id TEXT UNIQUE,
  scope_digest TEXT UNIQUE,
  initial_scope_checkpoint_digest TEXT,
  scope_admission_resolution_digest TEXT UNIQUE,
  PRIMARY KEY(project_session_id,run_id),
  UNIQUE(project_id,project_session_id,run_id),
  UNIQUE(project_session_id,run_id,authority_id),
  FOREIGN KEY(admission_request_id,scope_admission_resolution_digest,
        project_id,project_session_id,run_id,authority_id,admission_digest,
        admitted_at,scope_digest,initial_scope_checkpoint_digest)
      REFERENCES lifecycle_scope_admission_resolutions(
        admission_request_id,resolution_digest,project_id,project_session_id,
        run_id,authority_id,admission_digest,admitted_at,scope_digest,
        initial_scope_checkpoint_digest)
      DEFERRABLE INITIALLY DEFERRED
) STRICT;
CREATE TABLE lifecycle_receipt_scope_checkpoints (
  project_session_id TEXT,
  run_id TEXT,
  authority_id TEXT,
  receipt_count INTEGER CHECK(receipt_count >= 0),
  head_authority_sequence INTEGER CHECK(head_authority_sequence >= 0),
  head_receipt_digest TEXT,
  ordered_record_set_digest TEXT,
  checkpoint_json TEXT,
  checkpoint_digest TEXT,
  attestation TEXT,
  verified_at INTEGER,
  PRIMARY KEY(project_session_id,run_id,receipt_count),
  UNIQUE(checkpoint_digest),
  UNIQUE(project_session_id,run_id,checkpoint_digest),
  UNIQUE(project_session_id,run_id,receipt_count,checkpoint_digest,
      head_receipt_digest),
  UNIQUE(project_session_id,run_id,authority_id,receipt_count,
      checkpoint_digest,head_receipt_digest),
  UNIQUE(project_session_id,run_id,authority_id,receipt_count,
      checkpoint_digest),
  UNIQUE(project_session_id,run_id,authority_id,receipt_count,
      head_authority_sequence,ordered_record_set_digest,checkpoint_digest),
  UNIQUE(project_session_id,run_id,authority_id,receipt_count,
      head_authority_sequence,head_receipt_digest,ordered_record_set_digest,
      checkpoint_digest),
  CHECK(receipt_count=head_authority_sequence),
  CHECK((receipt_count=0)=(head_receipt_digest IS NULL)),
  FOREIGN KEY(project_session_id,run_id,authority_id)
      REFERENCES lifecycle_admitted_run_scopes(
        project_session_id,run_id,authority_id)
) STRICT;
CREATE TABLE lifecycle_scope_admission_resolutions (
  admission_request_id TEXT PRIMARY KEY,
  project_id TEXT NOT NULL,
  project_session_id TEXT NOT NULL,
  run_id TEXT NOT NULL,
  authority_id TEXT NOT NULL,
  admission_digest TEXT NOT NULL,
  admitted_at INTEGER NOT NULL,
  scope_digest TEXT NOT NULL,
  initial_receipt_count INTEGER NOT NULL CHECK(initial_receipt_count=0),
  initial_head_authority_sequence INTEGER NOT NULL CHECK(
      initial_head_authority_sequence=0),
  initial_ordered_record_set_digest TEXT NOT NULL,
  initial_scope_checkpoint_json TEXT NOT NULL,
  initial_scope_checkpoint_digest TEXT NOT NULL,
  initial_scope_head_revision INTEGER NOT NULL CHECK(initial_scope_head_revision=1),
  namespace_checkpoint_digest TEXT NOT NULL,
  namespace_member_json TEXT NOT NULL,
  verified_at INTEGER NOT NULL,
  resolution_json TEXT NOT NULL,
  resolution_digest TEXT NOT NULL UNIQUE,
  UNIQUE(project_session_id,run_id,authority_id,
      initial_scope_checkpoint_digest),
  UNIQUE(admission_request_id,resolution_digest,project_id,project_session_id,
      run_id,authority_id,admission_digest,admitted_at,scope_digest,
      initial_scope_checkpoint_digest),
  FOREIGN KEY(admission_request_id,project_id,project_session_id,run_id,
        authority_id,admission_digest,admitted_at,scope_digest)
      REFERENCES lifecycle_scope_admission_outbox(
        admission_request_id,project_id,project_session_id,run_id,authority_id,
        admission_digest,admitted_at,scope_digest),
  FOREIGN KEY(project_session_id,run_id,authority_id,initial_receipt_count,
        initial_head_authority_sequence,initial_ordered_record_set_digest,
        initial_scope_checkpoint_digest)
      REFERENCES lifecycle_receipt_scope_checkpoints(
        project_session_id,run_id,authority_id,receipt_count,
        head_authority_sequence,ordered_record_set_digest,checkpoint_digest),
  FOREIGN KEY(project_session_id,run_id)
      REFERENCES lifecycle_receipt_scope_heads(project_session_id,run_id)
) STRICT;
CREATE TABLE lifecycle_receipt_scope_heads (
  project_session_id TEXT NOT NULL,
  run_id TEXT NOT NULL,
  checkpoint_digest TEXT NOT NULL,
  revision INTEGER NOT NULL CHECK(revision >= 1),
  PRIMARY KEY(project_session_id,run_id),
  UNIQUE(project_session_id,run_id,checkpoint_digest,revision),
  FOREIGN KEY(project_session_id,run_id,checkpoint_digest)
      REFERENCES lifecycle_receipt_scope_checkpoints(
        project_session_id,run_id,checkpoint_digest)
) STRICT;
CREATE TRIGGER lifecycle_scope_resolution_requires_initial_head
BEFORE INSERT ON lifecycle_scope_admission_resolutions
WHEN NOT EXISTS (
  SELECT 1 FROM lifecycle_receipt_scope_heads h
  WHERE h.project_session_id=NEW.project_session_id
    AND h.run_id=NEW.run_id
    AND h.checkpoint_digest=NEW.initial_scope_checkpoint_digest
    AND h.revision=NEW.initial_scope_head_revision
)
BEGIN
  SELECT RAISE(ABORT,'lifecycle-scope-resolution-head-crossed');
END;
CREATE TABLE lifecycle_receipt_namespace_checkpoints (
  project_id TEXT,
  authority_id TEXT,
  scope_count INTEGER CHECK(scope_count >= 0),
  ordered_scope_head_set_digest TEXT,
  checkpoint_json TEXT,
  checkpoint_digest TEXT,
  attestation TEXT,
  verified_at INTEGER,
  PRIMARY KEY(project_id,checkpoint_digest),
  UNIQUE(checkpoint_digest),
  UNIQUE(project_id,checkpoint_digest,authority_id),
  UNIQUE(project_id,authority_id,scope_count,ordered_scope_head_set_digest,
      checkpoint_digest)
) STRICT;
CREATE TABLE lifecycle_receipt_namespace_members (
  project_id TEXT,
  checkpoint_digest TEXT,
  ordinal INTEGER CHECK(ordinal >= 1),
  project_session_id TEXT,
  run_id TEXT,
  authority_id TEXT,
  scope_checkpoint_digest TEXT,
  receipt_count INTEGER,
  head_receipt_digest TEXT,
  PRIMARY KEY(project_id,checkpoint_digest,ordinal),
  UNIQUE(project_id,checkpoint_digest,project_session_id,run_id),
  CHECK(receipt_count >= 0),
  CHECK((receipt_count=0)=(head_receipt_digest IS NULL)),
  FOREIGN KEY(project_id,checkpoint_digest,authority_id)
      REFERENCES lifecycle_receipt_namespace_checkpoints(
        project_id,checkpoint_digest,authority_id),
  FOREIGN KEY(project_id,project_session_id,run_id)
      REFERENCES lifecycle_admitted_run_scopes(
        project_id,project_session_id,run_id),
  FOREIGN KEY(project_session_id,run_id,authority_id,receipt_count,
        scope_checkpoint_digest)
      REFERENCES lifecycle_receipt_scope_checkpoints(
        project_session_id,run_id,authority_id,receipt_count,checkpoint_digest),
  FOREIGN KEY(project_session_id,run_id,authority_id,receipt_count,
        scope_checkpoint_digest,head_receipt_digest)
      REFERENCES lifecycle_receipt_scope_checkpoints(
        project_session_id,run_id,authority_id,receipt_count,checkpoint_digest,
        head_receipt_digest)
) STRICT;
CREATE TABLE lifecycle_receipt_namespace_heads (
  project_id TEXT PRIMARY KEY,
  authority_id TEXT,
  scope_count INTEGER,
  ordered_scope_head_set_digest TEXT,
  checkpoint_digest TEXT,
  head_revision INTEGER,
  FOREIGN KEY(project_id,authority_id,scope_count,
        ordered_scope_head_set_digest,checkpoint_digest)
      REFERENCES lifecycle_receipt_namespace_checkpoints(
        project_id,authority_id,scope_count,ordered_scope_head_set_digest,
        checkpoint_digest)
) STRICT;
CREATE TABLE lifecycle_recovery_retirement_plans (
  retirement_id TEXT PRIMARY KEY,
  revision INTEGER CHECK(revision=1),
  project_session_id TEXT,
  run_id TEXT,
  agent_id TEXT,
  custody_id TEXT,
  custody_revision INTEGER,
  custody_source_ref_digest TEXT,
  custody_journal_digest TEXT,
  finalized_disposition TEXT CHECK(finalized_disposition IN
      ('no-effect','superseded','quarantined')),
  finalized_terminal_evidence_digest TEXT,
  admission_digest TEXT,
  transition_proof_json TEXT,
  transition_proof_digest TEXT,
  mutation_plan_json TEXT,
  mutation_plan_digest TEXT,
  retirement_evidence_digest TEXT,
  planned_apply_id TEXT UNIQUE,
  recorded_at INTEGER,
  plan_json TEXT,
  retirement_plan_digest TEXT UNIQUE,
  UNIQUE(retirement_id,revision,retirement_plan_digest),
  UNIQUE(retirement_id,retirement_plan_digest,planned_apply_id),
  UNIQUE(project_session_id,run_id,agent_id,custody_id,custody_revision),
  UNIQUE(retirement_id,retirement_plan_digest,planned_apply_id,
      project_session_id,run_id,agent_id,mutation_plan_digest),
  UNIQUE(retirement_id,planned_apply_id,project_session_id,run_id,agent_id,
      custody_id,custody_revision,custody_source_ref_digest,custody_journal_digest,
      finalized_disposition,retirement_plan_digest),
  UNIQUE(retirement_id,planned_apply_id,project_session_id,run_id,agent_id,
      custody_id,custody_revision,custody_source_ref_digest,custody_journal_digest,
      finalized_disposition,finalized_terminal_evidence_digest,admission_digest,
      transition_proof_digest,mutation_plan_digest,retirement_evidence_digest,
      retirement_plan_digest),
  FOREIGN KEY(project_session_id,run_id,agent_id,custody_id,custody_revision,
        finalized_disposition,finalized_terminal_evidence_digest,
        custody_source_ref_digest,custody_journal_digest)
      REFERENCES lifecycle_rotation_custody_revisions(
        project_session_id,run_id,agent_id,custody_id,revision,disposition_code,
        terminal_evidence_digest,source_ref_digest,journal_digest)
) STRICT;
CREATE TABLE lifecycle_receipt_batches (
  batch_id TEXT PRIMARY KEY,
  planned_apply_id TEXT UNIQUE,
  project_session_id TEXT,
  run_id TEXT,
  agent_id TEXT,
  transition_kind TEXT CHECK(transition_kind IN
      ('custody-terminal','generation-loss-terminal',
        'custody-recovery-retirement','fresh-origin')),
  planned_apply_kind TEXT NOT NULL CHECK(
      planned_apply_kind IN ('terminal','terminal-fresh','fresh')),
  effects_set_digest TEXT,
  mutation_plan_digest TEXT,
  transition_replay_json TEXT,
  transition_replay_digest TEXT,
  ordered_subject_set_digest TEXT,
  receipt_intent_count INTEGER CHECK(receipt_intent_count IN (1,2)),
  secondary_intent_kind TEXT NOT NULL CHECK(secondary_intent_kind IN
      ('none','fresh-origin','review-adoption-decision')),
  review_adoption_reservation_id TEXT,
  review_adoption_reservation_digest TEXT,
  review_decision_loss_effect_key TEXT NOT NULL,
  review_decision_loss_effect_role TEXT,
  review_decision_loss_effect_digest TEXT,
  review_decision_loss_after_id TEXT,
  review_decision_loss_after_revision INTEGER,
  review_decision_loss_after_semantic_digest TEXT,
  review_decision_loss_after_source_ref_digest TEXT,
  fresh_handoff_id TEXT,
  fresh_handoff_digest TEXT,
  fresh_handoff_source_mode TEXT,
  fresh_handoff_key TEXT NOT NULL,
  recovery_retirement_id TEXT,
  recovery_retirement_plan_digest TEXT,
  created_at INTEGER,
  UNIQUE(project_session_id,run_id,agent_id,transition_replay_digest),
  UNIQUE(batch_id,planned_apply_id),
  UNIQUE(batch_id,effects_set_digest),
  UNIQUE(batch_id,transition_kind,receipt_intent_count),
  UNIQUE(batch_id,transition_kind,receipt_intent_count,secondary_intent_kind),
  UNIQUE(batch_id,planned_apply_id,transition_replay_digest,
      mutation_plan_digest),
  UNIQUE(batch_id,planned_apply_id,transition_kind,planned_apply_kind,
      transition_replay_digest,mutation_plan_digest,fresh_handoff_key),
  UNIQUE(batch_id,project_session_id,run_id),
  UNIQUE(batch_id,project_session_id,run_id,agent_id),
  UNIQUE(batch_id,planned_apply_id,project_session_id,run_id,agent_id),
  UNIQUE(batch_id,planned_apply_id,project_session_id,run_id,agent_id,
      review_adoption_reservation_digest),
  UNIQUE(batch_id,planned_apply_id,project_session_id,run_id,agent_id,
      transition_kind),
  UNIQUE(batch_id,planned_apply_id,transition_replay_digest,
      mutation_plan_digest,fresh_handoff_id,fresh_handoff_digest,
      fresh_handoff_key),
  UNIQUE(batch_id,review_decision_loss_effect_role,
      review_decision_loss_effect_digest),
  UNIQUE(batch_id,review_decision_loss_effect_key),
  UNIQUE(batch_id,review_decision_loss_effect_role,
      review_decision_loss_effect_digest,project_session_id,run_id,agent_id,
      review_decision_loss_after_id,review_decision_loss_after_revision,
      review_decision_loss_after_semantic_digest,
      review_decision_loss_after_source_ref_digest),
  UNIQUE(batch_id,review_decision_loss_effect_key,
      review_decision_loss_effect_role,review_decision_loss_effect_digest,
      project_session_id,run_id,agent_id,review_decision_loss_after_id,
      review_decision_loss_after_revision,
      review_decision_loss_after_semantic_digest,
      review_decision_loss_after_source_ref_digest),
  CHECK((review_adoption_reservation_id IS NULL)=
      (review_adoption_reservation_digest IS NULL)),
  CHECK((fresh_handoff_id IS NULL)=(fresh_handoff_digest IS NULL)),
  CHECK((fresh_handoff_id IS NULL)=(fresh_handoff_source_mode IS NULL)),
  CHECK((fresh_handoff_id IS NULL AND fresh_handoff_key='none') OR
      (fresh_handoff_id IS NOT NULL AND
        fresh_handoff_key=fresh_handoff_digest)),
  CHECK((recovery_retirement_id IS NULL)=
      (recovery_retirement_plan_digest IS NULL)),
  CHECK((transition_kind='custody-recovery-retirement')=
      (recovery_retirement_id IS NOT NULL)),
  CHECK(
      (transition_kind='custody-terminal' AND planned_apply_kind='terminal' AND
        secondary_intent_kind='none' AND receipt_intent_count=1 AND
        review_adoption_reservation_id IS NULL AND fresh_handoff_id IS NULL AND
        recovery_retirement_id IS NULL) OR
      (transition_kind='custody-terminal' AND planned_apply_kind='terminal' AND
        secondary_intent_kind='review-adoption-decision' AND
        receipt_intent_count=2 AND
        review_adoption_reservation_id IS NOT NULL AND
        fresh_handoff_id IS NULL AND recovery_retirement_id IS NULL) OR
      (transition_kind='custody-terminal' AND
        planned_apply_kind='terminal-fresh' AND
        secondary_intent_kind='fresh-origin' AND receipt_intent_count=2 AND
        review_adoption_reservation_id IS NULL AND fresh_handoff_id IS NOT NULL AND
        fresh_handoff_source_mode='terminalize-nonfinal-custody' AND
        recovery_retirement_id IS NULL) OR
      (transition_kind='generation-loss-terminal' AND
        planned_apply_kind='terminal' AND secondary_intent_kind='none' AND
        receipt_intent_count=1 AND review_adoption_reservation_id IS NULL AND
        fresh_handoff_id IS NULL AND recovery_retirement_id IS NULL) OR
      (transition_kind='custody-recovery-retirement' AND
        planned_apply_kind='terminal' AND secondary_intent_kind='none' AND
        receipt_intent_count=1 AND review_adoption_reservation_id IS NULL AND
        fresh_handoff_id IS NULL AND recovery_retirement_id IS NOT NULL) OR
      (transition_kind='fresh-origin' AND planned_apply_kind='fresh' AND
        secondary_intent_kind='none' AND receipt_intent_count=1 AND
        review_adoption_reservation_id IS NULL AND fresh_handoff_id IS NOT NULL AND
        fresh_handoff_source_mode IN
          ('reuse-final-custody','open-generation-loss') AND
        recovery_retirement_id IS NULL)),
  CHECK(
      (review_adoption_reservation_id IS NULL AND
        review_decision_loss_effect_key='none' AND
        review_decision_loss_effect_role IS NULL AND
        review_decision_loss_effect_digest IS NULL AND
        review_decision_loss_after_id IS NULL AND
        review_decision_loss_after_revision IS NULL AND
        review_decision_loss_after_semantic_digest IS NULL AND
        review_decision_loss_after_source_ref_digest IS NULL) OR
      (review_adoption_reservation_id IS NOT NULL AND
        review_decision_loss_effect_key='none' AND
        review_decision_loss_effect_role IS NULL AND
        review_decision_loss_effect_digest IS NULL AND
        review_decision_loss_after_id IS NULL AND
        review_decision_loss_after_revision IS NULL AND
        review_decision_loss_after_semantic_digest IS NULL AND
        review_decision_loss_after_source_ref_digest IS NULL) OR
      (review_adoption_reservation_id IS NOT NULL AND
        review_decision_loss_effect_key<>'none' AND
        review_decision_loss_effect_role IS NOT NULL AND
        review_decision_loss_effect_role='linked' AND
        review_decision_loss_effect_digest IS NOT NULL AND
        review_decision_loss_effect_digest=review_decision_loss_effect_key AND
        review_decision_loss_after_id IS NOT NULL AND
        review_decision_loss_after_revision IS NOT NULL AND
        review_decision_loss_after_semantic_digest IS NOT NULL AND
        review_decision_loss_after_source_ref_digest IS NOT NULL)),
  FOREIGN KEY(review_adoption_reservation_id,
        review_adoption_reservation_digest,review_decision_loss_effect_key)
      REFERENCES lifecycle_review_adoption_reservations(
        reservation_id,reservation_digest,decision_loss_effect_key),
  FOREIGN KEY(review_adoption_reservation_id,
        review_adoption_reservation_digest,review_decision_loss_effect_key,
        review_decision_loss_after_id,review_decision_loss_after_revision,
        review_decision_loss_after_semantic_digest,
        review_decision_loss_after_source_ref_digest)
      REFERENCES lifecycle_review_adoption_reservations(
        reservation_id,reservation_digest,decision_loss_effect_key,
        decision_loss_after_id,decision_loss_after_revision,
        decision_loss_after_semantic_digest,decision_loss_after_source_ref_digest),
  FOREIGN KEY(fresh_handoff_id,fresh_handoff_digest,planned_apply_id,
        fresh_handoff_source_mode)
      REFERENCES lifecycle_fresh_recovery_handoffs(
        handoff_id,handoff_digest,planned_apply_id,source_mode),
  FOREIGN KEY(recovery_retirement_id,recovery_retirement_plan_digest,
        planned_apply_id,project_session_id,run_id,agent_id,mutation_plan_digest)
      REFERENCES lifecycle_recovery_retirement_plans(
        retirement_id,retirement_plan_digest,planned_apply_id,
        project_session_id,run_id,agent_id,mutation_plan_digest),
  FOREIGN KEY(batch_id,review_decision_loss_effect_role,
        review_decision_loss_effect_digest,project_session_id,run_id,agent_id,
        review_decision_loss_after_id,review_decision_loss_after_revision,
        review_decision_loss_after_semantic_digest,
        review_decision_loss_after_source_ref_digest)
      REFERENCES lifecycle_receipt_generation_loss_effects(
        batch_id,role,effect_digest,project_session_id,run_id,agent_id,
        generation_loss_id,final_revision,final_semantic_digest,
        final_source_ref_digest)
      DEFERRABLE INITIALLY DEFERRED
) STRICT;
CREATE TABLE lifecycle_receipt_custody_effects (
  batch_id TEXT,
  ordinal INTEGER CHECK(ordinal=1),
  role TEXT CHECK(role='primary'),
  transition_kind TEXT CHECK(transition_kind='custody-terminal'),
  planned_apply_id TEXT,
  project_session_id TEXT,
  run_id TEXT,
  agent_id TEXT,
  custody_id TEXT,
  pre_revision INTEGER CHECK(pre_revision >= 1),
  pre_journal_digest TEXT,
  final_revision INTEGER CHECK(final_revision >= 2),
  final_semantic_digest TEXT,
  final_source_ref_digest TEXT,
  effect_digest TEXT,
  PRIMARY KEY(batch_id,ordinal),
  UNIQUE(batch_id),
  UNIQUE(effect_digest),
  UNIQUE(batch_id,effect_digest),
  UNIQUE(batch_id,effect_digest,project_session_id,run_id,agent_id,custody_id,
      final_revision),
  UNIQUE(batch_id,planned_apply_id,project_session_id,run_id,agent_id,
      custody_id,final_revision,final_semantic_digest,final_source_ref_digest),
  FOREIGN KEY(batch_id,planned_apply_id,project_session_id,run_id,agent_id,
        transition_kind)
      REFERENCES lifecycle_receipt_batches(
        batch_id,planned_apply_id,project_session_id,run_id,agent_id,
        transition_kind),
  FOREIGN KEY(project_session_id,run_id,agent_id,custody_id,pre_revision,
        pre_journal_digest)
      REFERENCES lifecycle_rotation_custody_revisions(
        project_session_id,run_id,agent_id,custody_id,revision,journal_digest),
  CHECK(final_revision=pre_revision+1)
) STRICT;
CREATE TABLE lifecycle_receipt_generation_loss_effects (
  batch_id TEXT,
  ordinal INTEGER CHECK(ordinal IN (1,2)),
  role TEXT CHECK(role IN ('primary','linked')),
  planned_apply_id TEXT,
  batch_transition_kind TEXT,
  project_session_id TEXT,
  run_id TEXT,
  agent_id TEXT,
  generation_loss_id TEXT,
  pre_revision INTEGER CHECK(pre_revision >= 1),
  pre_journal_digest TEXT,
  final_revision INTEGER CHECK(final_revision >= 2),
  final_semantic_digest TEXT,
  final_source_ref_digest TEXT,
  effect_digest TEXT,
  PRIMARY KEY(batch_id,ordinal),
  UNIQUE(batch_id,role),
  UNIQUE(effect_digest),
  UNIQUE(batch_id,role,effect_digest),
  UNIQUE(batch_id,role,effect_digest,project_session_id,run_id,agent_id,
      generation_loss_id,final_revision),
  UNIQUE(batch_id,role,effect_digest,project_session_id,run_id,agent_id,
      generation_loss_id,final_revision,final_semantic_digest,
      final_source_ref_digest),
  UNIQUE(batch_id,planned_apply_id,project_session_id,run_id,agent_id,
      generation_loss_id,final_revision,final_semantic_digest,
      final_source_ref_digest),
  FOREIGN KEY(batch_id,planned_apply_id,project_session_id,run_id,agent_id,
        batch_transition_kind)
      REFERENCES lifecycle_receipt_batches(
        batch_id,planned_apply_id,project_session_id,run_id,agent_id,
        transition_kind),
  FOREIGN KEY(project_session_id,run_id,agent_id,generation_loss_id,
        pre_revision,pre_journal_digest)
      REFERENCES lifecycle_generation_loss_revisions(
        project_session_id,run_id,agent_id,generation_loss_id,revision,
        journal_digest),
  CHECK(final_revision=pre_revision+1),
  CHECK((role='primary' AND ordinal=1 AND
        batch_transition_kind='generation-loss-terminal') OR
      (role='linked' AND ordinal=2 AND
        batch_transition_kind='custody-terminal'))
) STRICT;
CREATE TABLE lifecycle_receipt_recovery_retirement_effects (
  batch_id TEXT PRIMARY KEY,
  ordinal INTEGER CHECK(ordinal=1),
  role TEXT CHECK(role='primary'),
  transition_kind TEXT CHECK(transition_kind='custody-recovery-retirement'),
  planned_apply_id TEXT,
  project_session_id TEXT,
  run_id TEXT,
  agent_id TEXT,
  retirement_id TEXT UNIQUE,
  retirement_revision INTEGER CHECK(retirement_revision=1),
  retirement_plan_digest TEXT,
  custody_id TEXT,
  custody_revision INTEGER,
  custody_source_ref_digest TEXT,
  custody_journal_digest TEXT,
  finalized_disposition TEXT,
  finalized_terminal_evidence_digest TEXT,
  admission_digest TEXT,
  transition_proof_digest TEXT,
  mutation_plan_digest TEXT,
  retirement_evidence_digest TEXT,
  effect_digest TEXT UNIQUE,
  UNIQUE(batch_id,effect_digest),
  UNIQUE(batch_id,effect_digest,project_session_id,run_id,agent_id,
      retirement_id,retirement_revision),
  UNIQUE(batch_id,planned_apply_id,project_session_id,run_id,agent_id,
      retirement_id,retirement_plan_digest,custody_id,custody_revision,
      custody_source_ref_digest,custody_journal_digest,finalized_disposition,
      finalized_terminal_evidence_digest,admission_digest,
      transition_proof_digest,mutation_plan_digest,retirement_evidence_digest,
      effect_digest),
  FOREIGN KEY(batch_id,planned_apply_id,project_session_id,run_id,agent_id,
        transition_kind)
      REFERENCES lifecycle_receipt_batches(
        batch_id,planned_apply_id,project_session_id,run_id,agent_id,
        transition_kind),
  FOREIGN KEY(retirement_id,planned_apply_id,project_session_id,run_id,agent_id,
        custody_id,custody_revision,custody_source_ref_digest,
        custody_journal_digest,finalized_disposition,
        finalized_terminal_evidence_digest,admission_digest,
        transition_proof_digest,mutation_plan_digest,retirement_evidence_digest,
        retirement_plan_digest)
      REFERENCES lifecycle_recovery_retirement_plans(
        retirement_id,planned_apply_id,project_session_id,run_id,agent_id,
        custody_id,custody_revision,custody_source_ref_digest,
        custody_journal_digest,finalized_disposition,
        finalized_terminal_evidence_digest,admission_digest,
        transition_proof_digest,mutation_plan_digest,retirement_evidence_digest,
        retirement_plan_digest)
) STRICT;
CREATE TABLE lifecycle_receipt_fresh_origin_effects (
  batch_id TEXT,
  receipt_ordinal INTEGER CHECK(receipt_ordinal IN (1,2)),
  batch_transition_kind TEXT CHECK(
      batch_transition_kind IN ('custody-terminal','fresh-origin')),
  effect_role TEXT CHECK(effect_role IN ('primary','secondary')),
  planned_apply_id TEXT,
  project_session_id TEXT,
  run_id TEXT,
  agent_id TEXT,
  source_mode TEXT CHECK(source_mode IN ('terminalize-nonfinal-custody',
      'reuse-final-custody','open-generation-loss')),
  recovery_source_kind TEXT CHECK(
      recovery_source_kind IN ('custody','generation-loss')),
  recovery_source_ref_digest TEXT,
  source_journal_digest TEXT,
  handoff_id TEXT UNIQUE,
  handoff_digest TEXT,
  admission_digest TEXT,
  fresh_apply_plan_digest TEXT,
  new_custody_id TEXT UNIQUE,
  new_custody_revision INTEGER CHECK(new_custody_revision=1),
  new_custody_semantic_digest TEXT,
  new_custody_source_ref_digest TEXT,
  affected_generation_loss_id TEXT,
  affected_generation_loss_before_revision INTEGER,
  affected_generation_loss_before_state TEXT,
  affected_generation_loss_before_source_ref_digest TEXT,
  affected_generation_loss_before_journal_digest TEXT,
  affected_generation_loss_after_revision INTEGER,
  affected_generation_loss_after_semantic_digest TEXT,
  affected_generation_loss_after_source_ref_digest TEXT,
  affected_generation_loss_after_key TEXT NOT NULL,
  effect_json TEXT,
  effect_digest TEXT UNIQUE,
  PRIMARY KEY(batch_id,receipt_ordinal),
  UNIQUE(batch_id,effect_role,effect_digest),
  UNIQUE(batch_id,receipt_ordinal,effect_digest,project_session_id,run_id,
      agent_id,new_custody_id,new_custody_revision),
  UNIQUE(batch_id,effect_role,effect_digest,project_session_id,run_id,agent_id,
      handoff_id,handoff_digest,new_custody_id,new_custody_revision,
      new_custody_semantic_digest,new_custody_source_ref_digest,
      affected_generation_loss_after_key),
  FOREIGN KEY(batch_id,planned_apply_id,project_session_id,run_id,agent_id,
        batch_transition_kind)
      REFERENCES lifecycle_receipt_batches(
        batch_id,planned_apply_id,project_session_id,run_id,agent_id,
        transition_kind),
  FOREIGN KEY(handoff_id,handoff_digest,planned_apply_id,project_session_id,
        run_id,agent_id,source_mode,recovery_source_kind,
        recovery_source_ref_digest,source_journal_digest,new_custody_id,
        new_custody_semantic_digest,new_custody_source_ref_digest,
        affected_generation_loss_after_key,admission_digest,
        fresh_apply_plan_digest)
      REFERENCES lifecycle_fresh_recovery_handoffs(
        handoff_id,handoff_digest,planned_apply_id,project_session_id,run_id,
        agent_id,source_mode,recovery_source_kind,recovery_source_ref_digest,
        source_journal_digest,new_custody_id,new_custody_semantic_digest,
        new_custody_source_ref_digest,affected_generation_loss_after_key,
        admission_digest,fresh_apply_plan_digest),
  FOREIGN KEY(handoff_id,handoff_digest,affected_generation_loss_id,
        affected_generation_loss_before_revision,
        affected_generation_loss_before_state,
        affected_generation_loss_before_source_ref_digest,
        affected_generation_loss_before_journal_digest,
        affected_generation_loss_after_revision,
        affected_generation_loss_after_semantic_digest,
        affected_generation_loss_after_source_ref_digest)
      REFERENCES lifecycle_fresh_recovery_handoffs(
        handoff_id,handoff_digest,affected_generation_loss_id,
        affected_generation_loss_before_revision,
        affected_generation_loss_before_state,
        affected_generation_loss_before_source_ref_digest,
        affected_generation_loss_before_journal_digest,
        affected_generation_loss_after_revision,
        affected_generation_loss_after_semantic_digest,
        affected_generation_loss_after_source_ref_digest),
  CHECK((batch_transition_kind='fresh-origin' AND receipt_ordinal=1 AND
        effect_role='primary') OR
      (batch_transition_kind='custody-terminal' AND receipt_ordinal=2 AND
        effect_role='secondary')),
  CHECK((affected_generation_loss_after_key='none' AND
        affected_generation_loss_id IS NULL AND
        affected_generation_loss_before_revision IS NULL AND
        affected_generation_loss_before_state IS NULL AND
        affected_generation_loss_before_source_ref_digest IS NULL AND
        affected_generation_loss_before_journal_digest IS NULL AND
        affected_generation_loss_after_revision IS NULL AND
        affected_generation_loss_after_semantic_digest IS NULL AND
        affected_generation_loss_after_source_ref_digest IS NULL) OR
      (affected_generation_loss_after_key<>'none' AND
        affected_generation_loss_id IS NOT NULL AND
        affected_generation_loss_before_revision IS NOT NULL AND
        affected_generation_loss_before_state IS NOT NULL AND
        affected_generation_loss_before_source_ref_digest IS NOT NULL AND
        affected_generation_loss_before_journal_digest IS NOT NULL AND
        affected_generation_loss_after_revision=
          affected_generation_loss_before_revision+1 AND
        affected_generation_loss_after_semantic_digest IS NOT NULL AND
        affected_generation_loss_after_source_ref_digest=
          affected_generation_loss_after_key)),
  CHECK((source_mode='reuse-final-custody' AND
        recovery_source_kind='custody' AND
        affected_generation_loss_after_key='none') OR
      (source_mode='open-generation-loss' AND
        recovery_source_kind='generation-loss' AND
        affected_generation_loss_id IS NOT NULL AND
        affected_generation_loss_before_source_ref_digest=
          recovery_source_ref_digest AND
        affected_generation_loss_before_journal_digest=source_journal_digest) OR
      (source_mode='terminalize-nonfinal-custody' AND
        recovery_source_kind='custody'))
) STRICT;
CREATE TABLE lifecycle_receipt_intents (
  batch_id TEXT,
  ordinal INTEGER CHECK(ordinal IN (1,2)),
  batch_transition_kind TEXT,
  batch_intent_count INTEGER,
  batch_secondary_intent_kind TEXT,
  kind TEXT CHECK(kind IN ('custody-terminal','generation-loss-terminal',
      'custody-recovery-retirement','review-adoption-decision','fresh-origin')),
  project_session_id TEXT,
  run_id TEXT,
  agent_id TEXT,
  subject_owner_kind TEXT CHECK(subject_owner_kind IN
      ('custody','generation-loss','recovery-retirement')),
  subject_owner_id TEXT,
  subject_owner_revision INTEGER CHECK(subject_owner_revision >= 1),
  custody_effect_digest TEXT,
  generation_loss_effect_role TEXT,
  generation_loss_effect_digest TEXT,
  recovery_retirement_effect_digest TEXT,
  fresh_origin_effect_digest TEXT,
  subject_json TEXT,
  subject_digest TEXT,
  intent_digest TEXT,
  created_at INTEGER,
  PRIMARY KEY(batch_id,ordinal),
  UNIQUE(intent_digest),
  UNIQUE(intent_digest,batch_id,ordinal,project_session_id,run_id,agent_id,
      kind,subject_owner_kind,subject_owner_id,subject_owner_revision,
      subject_digest),
  UNIQUE(kind,project_session_id,run_id,agent_id,subject_owner_kind,
      subject_owner_id,subject_owner_revision),
  FOREIGN KEY(batch_id,project_session_id,run_id,agent_id)
      REFERENCES lifecycle_receipt_batches(
        batch_id,project_session_id,run_id,agent_id),
  FOREIGN KEY(batch_id,batch_transition_kind,batch_intent_count,
        batch_secondary_intent_kind)
      REFERENCES lifecycle_receipt_batches(
        batch_id,transition_kind,receipt_intent_count,secondary_intent_kind),
  FOREIGN KEY(batch_id,custody_effect_digest,project_session_id,run_id,
        agent_id,subject_owner_id,subject_owner_revision)
      REFERENCES lifecycle_receipt_custody_effects(
        batch_id,effect_digest,project_session_id,run_id,agent_id,custody_id,
        final_revision),
  FOREIGN KEY(batch_id,generation_loss_effect_role,
        generation_loss_effect_digest,project_session_id,run_id,agent_id,
        subject_owner_id,subject_owner_revision)
      REFERENCES lifecycle_receipt_generation_loss_effects(
        batch_id,role,effect_digest,project_session_id,run_id,agent_id,
        generation_loss_id,final_revision),
  FOREIGN KEY(batch_id,recovery_retirement_effect_digest,project_session_id,
        run_id,agent_id,subject_owner_id,subject_owner_revision)
      REFERENCES lifecycle_receipt_recovery_retirement_effects(
        batch_id,effect_digest,project_session_id,run_id,agent_id,retirement_id,
        retirement_revision),
  FOREIGN KEY(batch_id,ordinal,fresh_origin_effect_digest,project_session_id,
        run_id,agent_id,subject_owner_id,subject_owner_revision)
      REFERENCES lifecycle_receipt_fresh_origin_effects(
        batch_id,receipt_ordinal,effect_digest,project_session_id,run_id,agent_id,
        new_custody_id,new_custody_revision),
  CHECK((ordinal=1 AND kind=batch_transition_kind) OR
      (ordinal=2 AND batch_intent_count=2 AND
        batch_secondary_intent_kind<>'none' AND
        kind=batch_secondary_intent_kind)),
  CHECK(
      (kind IN ('custody-terminal','review-adoption-decision') AND
        subject_owner_kind='custody' AND custody_effect_digest IS NOT NULL AND
        generation_loss_effect_role IS NULL AND
        generation_loss_effect_digest IS NULL AND
        recovery_retirement_effect_digest IS NULL AND
        fresh_origin_effect_digest IS NULL) OR
      (kind='generation-loss-terminal' AND
        subject_owner_kind='generation-loss' AND
        custody_effect_digest IS NULL AND
        generation_loss_effect_role='primary' AND
        generation_loss_effect_digest IS NOT NULL AND
        recovery_retirement_effect_digest IS NULL AND
        fresh_origin_effect_digest IS NULL) OR
      (kind='custody-recovery-retirement' AND
        subject_owner_kind='recovery-retirement' AND
        custody_effect_digest IS NULL AND
        generation_loss_effect_role IS NULL AND
        generation_loss_effect_digest IS NULL AND
        recovery_retirement_effect_digest IS NOT NULL AND
        fresh_origin_effect_digest IS NULL) OR
      (kind='fresh-origin' AND subject_owner_kind='custody' AND
        custody_effect_digest IS NULL AND
        generation_loss_effect_role IS NULL AND
        generation_loss_effect_digest IS NULL AND
        recovery_retirement_effect_digest IS NULL AND
        fresh_origin_effect_digest IS NOT NULL))
) STRICT;
CREATE TABLE lifecycle_authority_receipts (
  intent_digest TEXT PRIMARY KEY,
  batch_id TEXT,
  ordinal INTEGER,
  project_session_id TEXT,
  run_id TEXT,
  agent_id TEXT,
  kind TEXT,
  subject_owner_kind TEXT,
  subject_owner_id TEXT,
  subject_owner_revision INTEGER,
  subject_digest TEXT,
  authority_id TEXT,
  authority_sequence INTEGER CHECK(authority_sequence >= 1),
  previous_authority_sequence INTEGER,
  previous_receipt_digest TEXT,
  receipt_json TEXT,
  receipt_digest TEXT UNIQUE,
  attestation TEXT,
  verified_at INTEGER,
  UNIQUE(project_session_id,run_id,authority_id,authority_sequence),
  UNIQUE(project_session_id,run_id,authority_id,authority_sequence,
      receipt_digest),
  UNIQUE(batch_id,ordinal,intent_digest,subject_digest,receipt_digest),
  UNIQUE(receipt_digest,kind,project_session_id,run_id,agent_id,
      subject_owner_kind,subject_owner_id,subject_owner_revision),
  UNIQUE(receipt_digest,intent_digest,batch_id,ordinal,kind,project_session_id,
      run_id,agent_id,subject_owner_kind,subject_owner_id,subject_owner_revision,
      subject_digest),
  UNIQUE(kind,project_session_id,run_id,agent_id,subject_owner_kind,
      subject_owner_id,subject_owner_revision),
  FOREIGN KEY(intent_digest,batch_id,ordinal,project_session_id,run_id,agent_id,
        kind,subject_owner_kind,subject_owner_id,subject_owner_revision,
        subject_digest)
      REFERENCES lifecycle_receipt_intents(
        intent_digest,batch_id,ordinal,project_session_id,run_id,agent_id,kind,
        subject_owner_kind,subject_owner_id,subject_owner_revision,subject_digest),
  FOREIGN KEY(project_session_id,run_id,authority_id)
      REFERENCES lifecycle_admitted_run_scopes(
        project_session_id,run_id,authority_id),
  FOREIGN KEY(project_session_id,run_id,authority_id,
        previous_authority_sequence,previous_receipt_digest)
      REFERENCES lifecycle_authority_receipts(
        project_session_id,run_id,authority_id,authority_sequence,receipt_digest),
  CHECK((authority_sequence=1 AND previous_authority_sequence IS NULL AND
        previous_receipt_digest IS NULL) OR
      (authority_sequence>1 AND
        previous_authority_sequence=authority_sequence-1 AND
        previous_receipt_digest IS NOT NULL))
) STRICT;
CREATE TABLE lifecycle_receipt_batch_completions (
  batch_id TEXT PRIMARY KEY,
  transition_kind TEXT,
  receipt_intent_count INTEGER,
  secondary_intent_kind TEXT,
  ordinal_one INTEGER CHECK(ordinal_one=1),
  ordinal_one_intent_digest TEXT,
  ordinal_one_subject_digest TEXT,
  ordinal_one_receipt_digest TEXT,
  ordinal_two INTEGER CHECK(ordinal_two IS NULL OR ordinal_two=2),
  ordinal_two_intent_digest TEXT,
  ordinal_two_subject_digest TEXT,
  ordinal_two_receipt_digest TEXT,
  effects_set_digest TEXT,
  primary_custody_effect_digest TEXT,
  primary_loss_effect_role TEXT CHECK(
      primary_loss_effect_role IS NULL OR primary_loss_effect_role='primary'),
  primary_loss_effect_digest TEXT,
  primary_retirement_effect_digest TEXT,
  primary_fresh_origin_effect_role TEXT CHECK(
      primary_fresh_origin_effect_role IS NULL OR
        primary_fresh_origin_effect_role='primary'),
  primary_fresh_origin_effect_digest TEXT,
  linked_loss_effect_role TEXT CHECK(
      linked_loss_effect_role IS NULL OR linked_loss_effect_role='linked'),
  linked_loss_effect_digest TEXT,
  secondary_fresh_origin_effect_role TEXT CHECK(
      secondary_fresh_origin_effect_role IS NULL OR
        secondary_fresh_origin_effect_role='secondary'),
  secondary_fresh_origin_effect_digest TEXT,
  ordered_authority_receipt_set_digest TEXT,
  completion_json TEXT,
  completion_digest TEXT UNIQUE,
  completed_at INTEGER,
  UNIQUE(batch_id,completion_digest,ordered_authority_receipt_set_digest),
  UNIQUE(batch_id,effects_set_digest),
  FOREIGN KEY(batch_id,transition_kind,receipt_intent_count,
        secondary_intent_kind)
      REFERENCES lifecycle_receipt_batches(
        batch_id,transition_kind,receipt_intent_count,secondary_intent_kind),
  FOREIGN KEY(batch_id,effects_set_digest)
      REFERENCES lifecycle_receipt_batches(batch_id,effects_set_digest),
  FOREIGN KEY(batch_id,ordinal_one,ordinal_one_intent_digest,
        ordinal_one_subject_digest,ordinal_one_receipt_digest)
      REFERENCES lifecycle_authority_receipts(
        batch_id,ordinal,intent_digest,subject_digest,receipt_digest),
  FOREIGN KEY(batch_id,ordinal_two,ordinal_two_intent_digest,
        ordinal_two_subject_digest,ordinal_two_receipt_digest)
      REFERENCES lifecycle_authority_receipts(
        batch_id,ordinal,intent_digest,subject_digest,receipt_digest),
  FOREIGN KEY(batch_id,primary_custody_effect_digest)
      REFERENCES lifecycle_receipt_custody_effects(batch_id,effect_digest),
  FOREIGN KEY(batch_id,primary_loss_effect_role,primary_loss_effect_digest)
      REFERENCES lifecycle_receipt_generation_loss_effects(
        batch_id,role,effect_digest),
  FOREIGN KEY(batch_id,primary_retirement_effect_digest)
      REFERENCES lifecycle_receipt_recovery_retirement_effects(
        batch_id,effect_digest),
  FOREIGN KEY(batch_id,linked_loss_effect_role,linked_loss_effect_digest)
      REFERENCES lifecycle_receipt_generation_loss_effects(
        batch_id,role,effect_digest),
  FOREIGN KEY(batch_id,primary_fresh_origin_effect_role,
        primary_fresh_origin_effect_digest)
      REFERENCES lifecycle_receipt_fresh_origin_effects(
        batch_id,effect_role,effect_digest),
  FOREIGN KEY(batch_id,secondary_fresh_origin_effect_role,
        secondary_fresh_origin_effect_digest)
      REFERENCES lifecycle_receipt_fresh_origin_effects(
        batch_id,effect_role,effect_digest),
  CHECK((primary_fresh_origin_effect_role IS NULL)=
      (primary_fresh_origin_effect_digest IS NULL)),
  CHECK((secondary_fresh_origin_effect_role IS NULL)=
      (secondary_fresh_origin_effect_digest IS NULL)),
  CHECK((secondary_intent_kind='none' AND receipt_intent_count=1 AND
        ordinal_two IS NULL AND
        ordinal_two_intent_digest IS NULL AND
        ordinal_two_subject_digest IS NULL AND ordinal_two_receipt_digest IS NULL) OR
      (secondary_intent_kind<>'none' AND receipt_intent_count=2 AND ordinal_two=2 AND
        ordinal_two_intent_digest IS NOT NULL AND
        ordinal_two_subject_digest IS NOT NULL AND
        ordinal_two_receipt_digest IS NOT NULL)),
  CHECK((linked_loss_effect_role IS NULL)=(linked_loss_effect_digest IS NULL)),
  CHECK((transition_kind='custody-terminal' AND
        secondary_intent_kind IN ('none','review-adoption-decision') AND
        primary_custody_effect_digest IS NOT NULL AND
        primary_loss_effect_role IS NULL AND primary_loss_effect_digest IS NULL AND
        primary_retirement_effect_digest IS NULL AND
        primary_fresh_origin_effect_role IS NULL AND
        primary_fresh_origin_effect_digest IS NULL AND
        secondary_fresh_origin_effect_role IS NULL AND
        secondary_fresh_origin_effect_digest IS NULL) OR
      (transition_kind='custody-terminal' AND
        secondary_intent_kind='fresh-origin' AND
        primary_custody_effect_digest IS NOT NULL AND
        primary_loss_effect_role IS NULL AND primary_loss_effect_digest IS NULL AND
        primary_retirement_effect_digest IS NULL AND
        primary_fresh_origin_effect_role IS NULL AND
        primary_fresh_origin_effect_digest IS NULL AND
        secondary_fresh_origin_effect_role='secondary' AND
        secondary_fresh_origin_effect_digest IS NOT NULL) OR
      (transition_kind='generation-loss-terminal' AND
        secondary_intent_kind='none' AND
        primary_custody_effect_digest IS NULL AND
        primary_loss_effect_role='primary' AND
        primary_loss_effect_digest IS NOT NULL AND
        primary_retirement_effect_digest IS NULL AND
        primary_fresh_origin_effect_role IS NULL AND
        primary_fresh_origin_effect_digest IS NULL AND
        linked_loss_effect_digest IS NULL AND
        secondary_fresh_origin_effect_role IS NULL AND
        secondary_fresh_origin_effect_digest IS NULL) OR
      (transition_kind='custody-recovery-retirement' AND
        secondary_intent_kind='none' AND
        primary_custody_effect_digest IS NULL AND
        primary_loss_effect_role IS NULL AND primary_loss_effect_digest IS NULL AND
        primary_retirement_effect_digest IS NOT NULL AND
        primary_fresh_origin_effect_role IS NULL AND
        primary_fresh_origin_effect_digest IS NULL AND
        linked_loss_effect_digest IS NULL AND
        secondary_fresh_origin_effect_role IS NULL AND
        secondary_fresh_origin_effect_digest IS NULL) OR
      (transition_kind='fresh-origin' AND secondary_intent_kind='none' AND
        primary_custody_effect_digest IS NULL AND
        primary_loss_effect_role IS NULL AND primary_loss_effect_digest IS NULL AND
        primary_retirement_effect_digest IS NULL AND
        primary_fresh_origin_effect_role='primary' AND
        primary_fresh_origin_effect_digest IS NOT NULL AND
        linked_loss_effect_digest IS NULL AND
        secondary_fresh_origin_effect_role IS NULL AND
        secondary_fresh_origin_effect_digest IS NULL))
) STRICT;
CREATE TABLE lifecycle_review_authority_bindings (
  receipt_digest TEXT PRIMARY KEY,
  intent_digest TEXT UNIQUE,
  batch_id TEXT UNIQUE,
  ordinal INTEGER CHECK(ordinal=2),
  subject_digest TEXT,
  kind TEXT CHECK(kind='review-adoption-decision'),
  subject_owner_kind TEXT CHECK(subject_owner_kind='custody'),
  project_session_id TEXT,
  run_id TEXT,
  agent_id TEXT,
  custody_id TEXT,
  custody_revision INTEGER,
  review_reservation_digest TEXT,
  review_decision_digest TEXT,
  certification_cut_digest TEXT,
  certification_cut_key TEXT,
  decision_loss_after_id TEXT,
  decision_loss_after_revision INTEGER,
  decision_loss_after_semantic_digest TEXT,
  decision_loss_after_source_ref_digest TEXT,
  decision_loss_after_key TEXT,
  decision_loss_effect_key TEXT NOT NULL,
  decision_loss_effect_role TEXT,
  decision_loss_effect_digest TEXT,
  apply_id TEXT UNIQUE,
  UNIQUE(receipt_digest,run_id,agent_id,custody_id,custody_revision,
      review_decision_digest,certification_cut_digest),
  FOREIGN KEY(receipt_digest,intent_digest,batch_id,ordinal,kind,
        project_session_id,run_id,agent_id,subject_owner_kind,custody_id,
        custody_revision,subject_digest)
      REFERENCES lifecycle_authority_receipts(
        receipt_digest,intent_digest,batch_id,ordinal,kind,project_session_id,run_id,
        agent_id,subject_owner_kind,subject_owner_id,subject_owner_revision,
        subject_digest),
  FOREIGN KEY(batch_id,apply_id,project_session_id,run_id,agent_id,
        review_reservation_digest)
      REFERENCES lifecycle_receipt_batches(
        batch_id,planned_apply_id,project_session_id,run_id,agent_id,
        review_adoption_reservation_digest),
  FOREIGN KEY(run_id,agent_id,custody_id,custody_revision)
      REFERENCES lifecycle_rotation_custody_revisions(
        run_id,agent_id,custody_id,revision),
  FOREIGN KEY(review_reservation_digest,project_session_id,run_id,agent_id,
        custody_id,custody_revision,review_decision_digest,certification_cut_key,
        decision_loss_after_key)
      REFERENCES lifecycle_review_adoption_reservations(
        reservation_digest,project_session_id,run_id,agent_id,custody_id,
        finalized_custody_revision,review_decision_digest,certification_cut_key,
        decision_loss_after_key),
  FOREIGN KEY(review_reservation_digest,decision_loss_after_id,
        decision_loss_after_revision,decision_loss_after_semantic_digest,
        decision_loss_after_source_ref_digest)
      REFERENCES lifecycle_review_adoption_reservations(
        reservation_digest,decision_loss_after_id,decision_loss_after_revision,
        decision_loss_after_semantic_digest,decision_loss_after_source_ref_digest),
  FOREIGN KEY(batch_id,decision_loss_effect_key)
      REFERENCES lifecycle_receipt_batches(
        batch_id,review_decision_loss_effect_key),
  FOREIGN KEY(batch_id,decision_loss_effect_key,decision_loss_effect_role,
        decision_loss_effect_digest,project_session_id,run_id,agent_id,
        decision_loss_after_id,decision_loss_after_revision,
        decision_loss_after_semantic_digest,
        decision_loss_after_source_ref_digest)
      REFERENCES lifecycle_receipt_batches(
        batch_id,review_decision_loss_effect_key,
        review_decision_loss_effect_role,review_decision_loss_effect_digest,
        project_session_id,run_id,agent_id,review_decision_loss_after_id,
        review_decision_loss_after_revision,
        review_decision_loss_after_semantic_digest,
        review_decision_loss_after_source_ref_digest),
  FOREIGN KEY(apply_id,batch_id)
      REFERENCES lifecycle_transition_applies(apply_id,receipt_batch_id)
      DEFERRABLE INITIALLY DEFERRED,
  CHECK(certification_cut_key IS NOT NULL AND
      decision_loss_after_key IS NOT NULL),
  CHECK((certification_cut_digest IS NULL AND certification_cut_key='none') OR
      (certification_cut_digest IS NOT NULL AND
        certification_cut_key=certification_cut_digest)),
  CHECK((decision_loss_after_key='none' AND decision_loss_after_id IS NULL AND
        decision_loss_after_revision IS NULL AND
        decision_loss_after_semantic_digest IS NULL AND
        decision_loss_after_source_ref_digest IS NULL) OR
      (decision_loss_after_key<>'none' AND decision_loss_after_id IS NOT NULL AND
        decision_loss_after_revision IS NOT NULL AND
        decision_loss_after_semantic_digest IS NOT NULL AND
        decision_loss_after_source_ref_digest=decision_loss_after_key)),
  CHECK((decision_loss_effect_key='none' AND
        decision_loss_effect_role IS NULL AND
        decision_loss_effect_digest IS NULL) OR
      (decision_loss_effect_key<>'none' AND
        decision_loss_effect_role IS NOT NULL AND
        decision_loss_effect_role='linked' AND
        decision_loss_effect_digest IS NOT NULL AND
        decision_loss_effect_digest=decision_loss_effect_key))
) STRICT;
CREATE TABLE lifecycle_receipt_batch_authorizations (
  batch_id TEXT PRIMARY KEY,
  project_session_id TEXT,
  run_id TEXT,
  batch_completion_digest TEXT,
  ordered_authority_receipt_set_digest TEXT,
  verified_scope_checkpoint_digest TEXT,
  authorized_at INTEGER,
  authorization_digest TEXT UNIQUE,
  UNIQUE(batch_id,verified_scope_checkpoint_digest),
  UNIQUE(batch_id,ordered_authority_receipt_set_digest,
      verified_scope_checkpoint_digest),
  UNIQUE(batch_id,batch_completion_digest,
      ordered_authority_receipt_set_digest,verified_scope_checkpoint_digest),
  FOREIGN KEY(batch_id,project_session_id,run_id)
      REFERENCES lifecycle_receipt_batches(batch_id,project_session_id,run_id),
  FOREIGN KEY(batch_id,batch_completion_digest,
        ordered_authority_receipt_set_digest)
      REFERENCES lifecycle_receipt_batch_completions(
        batch_id,completion_digest,ordered_authority_receipt_set_digest),
  FOREIGN KEY(project_session_id,run_id,verified_scope_checkpoint_digest)
      REFERENCES lifecycle_receipt_scope_checkpoints(
        project_session_id,run_id,checkpoint_digest)
) STRICT;
CREATE TABLE lifecycle_transition_applies (
  apply_id TEXT PRIMARY KEY,
  apply_kind TEXT CHECK(apply_kind IN ('terminal','terminal-fresh','fresh')),
  batch_transition_kind TEXT NOT NULL CHECK(batch_transition_kind IN
      ('custody-terminal','generation-loss-terminal',
        'custody-recovery-retirement','fresh-origin')),
  receipt_batch_id TEXT UNIQUE,
  batch_completion_digest TEXT,
  transition_replay_digest TEXT,
  ordered_authority_receipt_set_digest TEXT,
  verified_scope_checkpoint_digest TEXT,
  applied_mutation_plan_digest TEXT,
  fresh_handoff_id TEXT UNIQUE,
  fresh_handoff_digest TEXT,
  fresh_handoff_key TEXT NOT NULL,
  fresh_project_session_id TEXT,
  fresh_run_id TEXT,
  fresh_agent_id TEXT,
  fresh_source_mode TEXT,
  fresh_apply_plan_digest TEXT,
  new_custody_id TEXT UNIQUE,
  new_custody_revision INTEGER CHECK(new_custody_revision=1),
  new_custody_semantic_digest TEXT,
  new_custody_source_ref_digest TEXT,
  fresh_generation_loss_id TEXT,
  fresh_generation_loss_after_revision INTEGER,
  fresh_generation_loss_after_semantic_digest TEXT,
  fresh_generation_loss_after_source_ref_digest TEXT,
  fresh_generation_loss_after_key TEXT NOT NULL,
  fresh_origin_effect_role TEXT CHECK(
      fresh_origin_effect_role IS NULL OR
        fresh_origin_effect_role IN ('primary','secondary')),
  fresh_origin_effect_digest TEXT,
  local_write_set_digest TEXT,
  apply_json TEXT,
  apply_digest TEXT UNIQUE,
  applied_at INTEGER,
  UNIQUE(apply_id,apply_digest),
  UNIQUE(apply_id,apply_digest,apply_kind),
  UNIQUE(apply_id,receipt_batch_id),
  UNIQUE(apply_id,apply_digest,receipt_batch_id),
  UNIQUE(apply_id,fresh_handoff_id),
  UNIQUE(apply_id,apply_digest,fresh_handoff_id),
  UNIQUE(apply_id,apply_digest,fresh_handoff_id,apply_kind),
  UNIQUE(apply_id,apply_digest,new_custody_id,new_custody_semantic_digest,
      new_custody_source_ref_digest),
  UNIQUE(apply_id,apply_digest,fresh_generation_loss_after_key),
  UNIQUE(apply_id,apply_digest,fresh_project_session_id,fresh_run_id,
      fresh_agent_id,fresh_generation_loss_id,
      fresh_generation_loss_after_revision,
      fresh_generation_loss_after_semantic_digest,
      fresh_generation_loss_after_source_ref_digest),
  FOREIGN KEY(receipt_batch_id,apply_id,batch_transition_kind,apply_kind,
        transition_replay_digest,applied_mutation_plan_digest,fresh_handoff_key)
      REFERENCES lifecycle_receipt_batches(
        batch_id,planned_apply_id,transition_kind,planned_apply_kind,
        transition_replay_digest,mutation_plan_digest,fresh_handoff_key),
  FOREIGN KEY(receipt_batch_id,batch_completion_digest,
        ordered_authority_receipt_set_digest,verified_scope_checkpoint_digest)
      REFERENCES lifecycle_receipt_batch_authorizations(
        batch_id,batch_completion_digest,ordered_authority_receipt_set_digest,
        verified_scope_checkpoint_digest),
  FOREIGN KEY(fresh_handoff_id,fresh_handoff_digest,apply_id,
        fresh_project_session_id,fresh_run_id,fresh_agent_id,fresh_source_mode,
        new_custody_id,
        new_custody_semantic_digest,new_custody_source_ref_digest,
        fresh_apply_plan_digest,fresh_generation_loss_after_key)
      REFERENCES lifecycle_fresh_recovery_handoffs(
        handoff_id,handoff_digest,planned_apply_id,project_session_id,run_id,
        agent_id,source_mode,new_custody_id,new_custody_semantic_digest,
        new_custody_source_ref_digest,fresh_apply_plan_digest,
        affected_generation_loss_after_key),
  FOREIGN KEY(fresh_handoff_id,apply_id,fresh_generation_loss_id,
        fresh_generation_loss_after_revision,
        fresh_generation_loss_after_semantic_digest,
        fresh_generation_loss_after_source_ref_digest)
      REFERENCES lifecycle_fresh_recovery_handoffs(
        handoff_id,planned_apply_id,affected_generation_loss_id,
        affected_generation_loss_after_revision,
        affected_generation_loss_after_semantic_digest,
        affected_generation_loss_after_source_ref_digest),
  FOREIGN KEY(receipt_batch_id,fresh_origin_effect_role,
        fresh_origin_effect_digest,fresh_project_session_id,fresh_run_id,
        fresh_agent_id,fresh_handoff_id,fresh_handoff_digest,new_custody_id,
        new_custody_revision,new_custody_semantic_digest,
        new_custody_source_ref_digest,fresh_generation_loss_after_key)
      REFERENCES lifecycle_receipt_fresh_origin_effects(
        batch_id,effect_role,effect_digest,project_session_id,run_id,agent_id,
        handoff_id,handoff_digest,new_custody_id,new_custody_revision,
        new_custody_semantic_digest,new_custody_source_ref_digest,
        affected_generation_loss_after_key),
  CHECK((apply_kind='terminal' AND
        batch_transition_kind IN ('custody-terminal','generation-loss-terminal',
          'custody-recovery-retirement') AND receipt_batch_id IS NOT NULL AND
        batch_completion_digest IS NOT NULL AND
        transition_replay_digest IS NOT NULL AND
        ordered_authority_receipt_set_digest IS NOT NULL AND
        verified_scope_checkpoint_digest IS NOT NULL AND
        applied_mutation_plan_digest IS NOT NULL AND fresh_handoff_id IS NULL AND
        fresh_handoff_digest IS NULL AND fresh_handoff_key='none' AND
        fresh_project_session_id IS NULL AND
        fresh_run_id IS NULL AND fresh_agent_id IS NULL AND
        fresh_source_mode IS NULL AND fresh_apply_plan_digest IS NULL AND
        new_custody_id IS NULL AND new_custody_revision IS NULL AND
        new_custody_semantic_digest IS NULL AND
        new_custody_source_ref_digest IS NULL AND
        fresh_generation_loss_id IS NULL AND
        fresh_generation_loss_after_revision IS NULL AND
        fresh_generation_loss_after_semantic_digest IS NULL AND
        fresh_generation_loss_after_source_ref_digest IS NULL AND
        fresh_generation_loss_after_key='none' AND
        fresh_origin_effect_role IS NULL AND
        fresh_origin_effect_digest IS NULL) OR
      (apply_kind='terminal-fresh' AND
        batch_transition_kind='custody-terminal' AND receipt_batch_id IS NOT NULL AND
        batch_completion_digest IS NOT NULL AND
        transition_replay_digest IS NOT NULL AND
        ordered_authority_receipt_set_digest IS NOT NULL AND
        verified_scope_checkpoint_digest IS NOT NULL AND
        applied_mutation_plan_digest IS NOT NULL AND fresh_handoff_id IS NOT NULL AND
        fresh_handoff_digest IS NOT NULL AND
        fresh_handoff_key=fresh_handoff_digest AND
        fresh_project_session_id IS NOT NULL AND
        fresh_run_id IS NOT NULL AND fresh_agent_id IS NOT NULL AND
        fresh_source_mode='terminalize-nonfinal-custody' AND
        fresh_apply_plan_digest IS NOT NULL AND
        new_custody_id IS NOT NULL AND new_custody_revision=1 AND
        new_custody_semantic_digest IS NOT NULL AND
        new_custody_source_ref_digest IS NOT NULL AND
        fresh_origin_effect_role='secondary' AND
        fresh_origin_effect_digest IS NOT NULL AND
        ((fresh_generation_loss_after_key='none' AND
            fresh_generation_loss_id IS NULL AND
            fresh_generation_loss_after_revision IS NULL AND
            fresh_generation_loss_after_semantic_digest IS NULL AND
            fresh_generation_loss_after_source_ref_digest IS NULL) OR
          (fresh_generation_loss_after_key<>'none' AND
            fresh_generation_loss_id IS NOT NULL AND
            fresh_generation_loss_after_revision IS NOT NULL AND
            fresh_generation_loss_after_semantic_digest IS NOT NULL AND
            fresh_generation_loss_after_source_ref_digest=
              fresh_generation_loss_after_key))) OR
      (apply_kind='fresh' AND batch_transition_kind='fresh-origin' AND
        receipt_batch_id IS NOT NULL AND batch_completion_digest IS NOT NULL AND
        transition_replay_digest IS NOT NULL AND
        ordered_authority_receipt_set_digest IS NOT NULL AND
        verified_scope_checkpoint_digest IS NOT NULL AND
        applied_mutation_plan_digest IS NOT NULL AND fresh_handoff_id IS NOT NULL AND
        fresh_handoff_digest IS NOT NULL AND
        fresh_handoff_key=fresh_handoff_digest AND
        fresh_project_session_id IS NOT NULL AND
        fresh_run_id IS NOT NULL AND fresh_agent_id IS NOT NULL AND
        fresh_source_mode IN ('reuse-final-custody','open-generation-loss') AND
        fresh_apply_plan_digest IS NOT NULL AND
        new_custody_id IS NOT NULL AND new_custody_revision=1 AND
        new_custody_semantic_digest IS NOT NULL AND
        new_custody_source_ref_digest IS NOT NULL AND
        applied_mutation_plan_digest=fresh_apply_plan_digest AND
        fresh_origin_effect_role='primary' AND
        fresh_origin_effect_digest IS NOT NULL AND
        ((fresh_source_mode='reuse-final-custody' AND
            fresh_generation_loss_after_key='none' AND
            fresh_generation_loss_id IS NULL AND
            fresh_generation_loss_after_revision IS NULL AND
            fresh_generation_loss_after_semantic_digest IS NULL AND
            fresh_generation_loss_after_source_ref_digest IS NULL) OR
          (fresh_source_mode='open-generation-loss' AND
            fresh_generation_loss_after_key<>'none' AND
            fresh_generation_loss_id IS NOT NULL AND
            fresh_generation_loss_after_revision IS NOT NULL AND
            fresh_generation_loss_after_semantic_digest IS NOT NULL AND
            fresh_generation_loss_after_source_ref_digest=
              fresh_generation_loss_after_key))))
) STRICT;
CREATE TABLE lifecycle_review_adoption_reservations (
  reservation_id TEXT PRIMARY KEY,
  reservation_digest TEXT UNIQUE,
  project_session_id TEXT,
  run_id TEXT,
  agent_id TEXT,
  custody_id TEXT,
  finalized_custody_revision INTEGER,
  target_generation INTEGER,
  predecessor_binding_generation INTEGER,
  predecessor_binding_digest TEXT,
  terminal_sequence_high_water TEXT,
  lifecycle_adoption_evidence_digest TEXT,
  review_decision_json TEXT,
  review_decision_digest TEXT,
  certification_cut_json TEXT,
  certification_cut_digest TEXT,
  certification_cut_key TEXT,
  recovery_source_kind TEXT CHECK(
      recovery_source_kind IN ('none','custody','generation-loss')),
  recovery_from_custody_id TEXT,
  recovery_from_custody_revision INTEGER,
  recovery_from_generation_loss_id TEXT,
  recovery_from_generation_loss_revision INTEGER,
  recovery_source_ref_digest TEXT,
  decision_loss_after_id TEXT,
  decision_loss_after_revision INTEGER,
  decision_loss_after_semantic_digest TEXT,
  decision_loss_after_source_ref_digest TEXT,
  decision_loss_after_key TEXT,
  decision_loss_effect_key TEXT NOT NULL,
  recovery_source_decision_json TEXT,
  recovery_source_decision_digest TEXT,
  local_write_set_digest TEXT,
  reservation_json TEXT,
  created_at INTEGER,
  UNIQUE(reservation_id,reservation_digest),
  UNIQUE(reservation_id,reservation_digest,decision_loss_effect_key),
  UNIQUE(reservation_id,reservation_digest,decision_loss_effect_key,
      decision_loss_after_id,decision_loss_after_revision,
      decision_loss_after_semantic_digest,decision_loss_after_source_ref_digest),
  UNIQUE(reservation_digest,project_session_id,run_id,agent_id,custody_id,
      finalized_custody_revision,review_decision_digest,certification_cut_key,
      decision_loss_after_key),
  UNIQUE(reservation_digest,decision_loss_after_id,decision_loss_after_revision,
      decision_loss_after_semantic_digest,decision_loss_after_source_ref_digest),
  UNIQUE(project_session_id,run_id,agent_id,custody_id,
      finalized_custody_revision),
  CHECK(certification_cut_key IS NOT NULL AND
      decision_loss_after_key IS NOT NULL),
  FOREIGN KEY(project_session_id,run_id,agent_id,custody_id)
      REFERENCES lifecycle_rotation_custodies(
        project_session_id,run_id,agent_id,custody_id),
  CHECK((certification_cut_digest IS NULL AND certification_cut_key='none') OR
      (certification_cut_digest IS NOT NULL AND
        certification_cut_key=certification_cut_digest)),
  CHECK((recovery_source_kind='none' AND
        recovery_from_custody_id IS NULL AND
        recovery_from_custody_revision IS NULL AND
        recovery_from_generation_loss_id IS NULL AND
        recovery_from_generation_loss_revision IS NULL AND
        recovery_source_ref_digest IS NULL AND
        decision_loss_after_id IS NULL AND decision_loss_after_revision IS NULL AND
        decision_loss_after_semantic_digest IS NULL AND
        decision_loss_after_source_ref_digest IS NULL AND
        decision_loss_after_key='none' AND
        decision_loss_effect_key='none' AND
        recovery_source_decision_json IS NULL AND
        recovery_source_decision_digest IS NULL) OR
      (recovery_source_kind='custody' AND
        recovery_from_custody_id IS NOT NULL AND
        recovery_from_custody_revision IS NOT NULL AND
        recovery_from_generation_loss_id IS NULL AND
        recovery_from_generation_loss_revision IS NULL AND
        recovery_source_ref_digest IS NOT NULL AND
        decision_loss_after_id IS NULL AND decision_loss_after_revision IS NULL AND
        decision_loss_after_semantic_digest IS NULL AND
        decision_loss_after_source_ref_digest IS NULL AND
        decision_loss_after_key='none' AND
        decision_loss_effect_key='none' AND
        recovery_source_decision_json IS NOT NULL AND
        recovery_source_decision_digest IS NOT NULL) OR
      (recovery_source_kind='generation-loss' AND
        recovery_from_custody_id IS NULL AND
        recovery_from_custody_revision IS NULL AND
        recovery_from_generation_loss_id IS NOT NULL AND
        recovery_from_generation_loss_revision IS NOT NULL AND
        recovery_source_ref_digest IS NOT NULL AND
        decision_loss_after_id=recovery_from_generation_loss_id AND
        decision_loss_after_revision IS NOT NULL AND
        decision_loss_after_semantic_digest IS NOT NULL AND
        decision_loss_after_source_ref_digest IS NOT NULL AND
        decision_loss_after_key=decision_loss_after_source_ref_digest AND
        decision_loss_effect_key<>'none' AND
        recovery_source_decision_json IS NOT NULL AND
        recovery_source_decision_digest IS NOT NULL)),
  FOREIGN KEY(project_session_id,run_id,agent_id,recovery_from_custody_id,
        recovery_from_custody_revision,recovery_source_ref_digest)
      REFERENCES lifecycle_rotation_custody_revisions(
        project_session_id,run_id,agent_id,custody_id,revision,
        source_ref_digest),
  FOREIGN KEY(project_session_id,run_id,agent_id,
        recovery_from_generation_loss_id,recovery_from_generation_loss_revision,
        recovery_source_ref_digest)
      REFERENCES lifecycle_generation_loss_revisions(
        project_session_id,run_id,agent_id,generation_loss_id,
        revision,source_ref_digest),
  CHECK((decision_loss_effect_key='none')=
      (decision_loss_after_id IS NULL))
) STRICT;
CREATE TABLE lifecycle_fresh_rotation_preparations (
  preparation_id TEXT PRIMARY KEY,
  attempt_id TEXT UNIQUE,
  issue_id TEXT UNIQUE,
  project_session_id TEXT,
  run_id TEXT,
  agent_id TEXT,
  recovery_source_kind TEXT CHECK(
      recovery_source_kind IN ('custody','generation-loss')),
  old_custody_id TEXT,
  old_custody_revision INTEGER,
  generation_loss_id TEXT,
  generation_loss_revision INTEGER,
  recovery_source_ref_digest TEXT,
  source_journal_digest TEXT,
  provider_action_adapter_id TEXT,
  provider_action_id TEXT,
  checkpoint_ref TEXT,
  checkpoint_digest TEXT,
  checkpoint_validation_digest TEXT,
  checkpoint_validation_key TEXT,
  adapter_contract_digest TEXT,
  operation TEXT,
  reserved_provider_generation INTEGER,
  reserved_principal_generation INTEGER,
  reserved_bridge_generation INTEGER,
  preparation_json TEXT,
  preparation_digest TEXT,
  created_at INTEGER,
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
) STRICT;
CREATE TABLE lifecycle_fresh_recovery_handoffs (
  handoff_id TEXT PRIMARY KEY,
  preparation_id TEXT UNIQUE,
  attempt_id TEXT UNIQUE,
  preparation_digest TEXT,
  issue_id TEXT UNIQUE,
  project_session_id TEXT,
  run_id TEXT,
  agent_id TEXT,
  source_mode TEXT CHECK(source_mode IN ('terminalize-nonfinal-custody',
      'reuse-final-custody','open-generation-loss')),
  recovery_source_kind TEXT CHECK(
      recovery_source_kind IN ('custody','generation-loss')),
  old_custody_id TEXT,
  old_custody_revision INTEGER,
  generation_loss_id TEXT,
  generation_loss_revision INTEGER,
  recovery_source_ref_digest TEXT,
  source_journal_digest TEXT,
  new_custody_id TEXT UNIQUE,
  planned_apply_id TEXT UNIQUE,
  new_custody_semantic_digest TEXT,
  new_custody_source_ref_digest TEXT,
  affected_generation_loss_id TEXT,
  affected_generation_loss_before_revision INTEGER,
  affected_generation_loss_before_state TEXT,
  affected_generation_loss_before_source_ref_digest TEXT,
  affected_generation_loss_before_journal_digest TEXT,
  affected_generation_loss_after_revision INTEGER,
  affected_generation_loss_after_semantic_digest TEXT,
  affected_generation_loss_after_source_ref_digest TEXT,
  affected_generation_loss_after_key TEXT NOT NULL,
  provider_action_adapter_id TEXT,
  provider_action_id TEXT,
  checkpoint_ref TEXT,
  checkpoint_digest TEXT,
  checkpoint_validation_digest TEXT,
  checkpoint_validation_key TEXT,
  adapter_contract_digest TEXT,
  operation TEXT,
  reserved_provider_generation INTEGER,
  reserved_principal_generation INTEGER,
  reserved_bridge_generation INTEGER,
  admission_digest TEXT,
  fresh_apply_plan_json TEXT,
  fresh_apply_plan_digest TEXT,
  handoff_json TEXT,
  handoff_digest TEXT UNIQUE,
  created_at INTEGER,
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
) STRICT;
CREATE TABLE lifecycle_fresh_rotation_commits (
  commit_id TEXT PRIMARY KEY,
  handoff_id TEXT UNIQUE,
  preparation_id TEXT UNIQUE,
  handoff_digest TEXT,
  preparation_digest TEXT,
  attempt_id TEXT UNIQUE,
  issue_id TEXT UNIQUE,
  project_session_id TEXT,
  run_id TEXT,
  agent_id TEXT,
  source_mode TEXT,
  recovery_source_kind TEXT,
  recovery_source_ref_digest TEXT,
  source_journal_digest TEXT,
  new_custody_id TEXT UNIQUE,
  new_custody_revision INTEGER CHECK(new_custody_revision=1),
  new_custody_semantic_digest TEXT,
  new_custody_source_ref_digest TEXT,
  new_custody_journal_digest TEXT,
  generation_loss_after_id TEXT,
  generation_loss_after_revision INTEGER,
  generation_loss_after_semantic_digest TEXT,
  generation_loss_after_source_ref_digest TEXT,
  generation_loss_after_journal_digest TEXT,
  generation_loss_after_key TEXT NOT NULL,
  provider_action_adapter_id TEXT,
  provider_action_id TEXT,
  checkpoint_ref TEXT,
  checkpoint_digest TEXT,
  checkpoint_validation_digest TEXT,
  checkpoint_validation_key TEXT,
  adapter_contract_digest TEXT,
  operation TEXT,
  reserved_provider_generation INTEGER,
  reserved_principal_generation INTEGER,
  reserved_bridge_generation INTEGER,
  admission_digest TEXT,
  fresh_apply_plan_digest TEXT,
  apply_kind TEXT CHECK(apply_kind IN ('terminal-fresh','fresh')),
  fresh_apply_digest TEXT,
  source_terminal_receipt_apply_digest TEXT,
  apply_id TEXT UNIQUE,
  commit_json TEXT,
  commit_digest TEXT UNIQUE,
  created_at INTEGER,
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
) STRICT;
CREATE TABLE lifecycle_generation_losses (
  project_session_id TEXT,
  run_id TEXT,
  agent_id TEXT,
  generation_loss_id TEXT,
  loss_kind TEXT,
  old_provider_session_ref TEXT,
  new_provider_session_ref TEXT,
  old_provider_generation INTEGER,
  new_provider_generation INTEGER,
  old_context_revision INTEGER,
  new_context_revision INTEGER,
  source_custody_action_id TEXT,
  source_adapter_id TEXT,
  source_adapter_contract_digest TEXT,
  source_principal_generation INTEGER,
  source_bridge_generation INTEGER,
  bridge_owner_kind TEXT,
  source_bridge_row_id TEXT,
  source_bridge_revision INTEGER,
  source_capability_hash TEXT,
  source_project_session_generation INTEGER,
  source_run_generation INTEGER,
  source_chair_lease_generation INTEGER,
  checkpoint_state TEXT,
  checkpoint_ref TEXT,
  checkpoint_digest TEXT,
  loss_evidence_digest TEXT,
  creation_json TEXT,
  creation_digest TEXT,
  created_at INTEGER,
  PRIMARY KEY(run_id,agent_id,generation_loss_id),
  UNIQUE(project_session_id,run_id,agent_id,generation_loss_id),
  UNIQUE(creation_digest),
  CHECK(loss_kind IN ('generation-advance','context-advance')),
  FOREIGN KEY(source_adapter_id, source_custody_action_id)
      REFERENCES provider_actions(adapter_id,action_id)
) STRICT;
CREATE TABLE lifecycle_generation_loss_revisions (
  project_session_id TEXT,
  run_id TEXT,
  agent_id TEXT,
  generation_loss_id TEXT,
  revision INTEGER CHECK(revision >= 1),
  prior_revision INTEGER,
  prior_journal_digest TEXT,
  state TEXT CHECK(state IN
      ('open','recovery-in-progress','recovered-adopted','abandoned')),
  abandon_kind_code TEXT CHECK(
      abandon_kind_code IN ('none','direct-open','recovery-attempt')),
  recovery_action_adapter_id TEXT,
  recovery_action_id TEXT,
  active_recovery_custody_id TEXT,
  terminal_evidence_digest TEXT,
  semantic_json TEXT,
  semantic_digest TEXT,
  source_ref_digest TEXT,
  origin_fresh_apply_id TEXT,
  origin_fresh_apply_digest TEXT,
  receipt_batch_id TEXT,
  receipt_apply_id TEXT,
  receipt_apply_digest TEXT,
  journal_json TEXT,
  journal_digest TEXT,
  recorded_at INTEGER,
  PRIMARY KEY(run_id,agent_id,generation_loss_id,revision),
  UNIQUE(project_session_id,run_id,agent_id,generation_loss_id,revision),
  UNIQUE(project_session_id,run_id,agent_id,generation_loss_id,revision,
      source_ref_digest),
  UNIQUE(project_session_id,run_id,agent_id,generation_loss_id,revision,
      journal_digest),
  UNIQUE(project_session_id,run_id,agent_id,generation_loss_id,revision,
      source_ref_digest,journal_digest),
  UNIQUE(project_session_id,run_id,agent_id,generation_loss_id,revision,
      semantic_digest,source_ref_digest,journal_digest),
  UNIQUE(project_session_id,run_id,agent_id,generation_loss_id,revision,
      semantic_digest,source_ref_digest),
  UNIQUE(project_session_id,run_id,agent_id,generation_loss_id,revision,state,
      source_ref_digest,journal_digest),
  UNIQUE(project_session_id,run_id,agent_id,generation_loss_id,revision,state,
      active_recovery_custody_id,source_ref_digest,journal_digest),
  UNIQUE(project_session_id,run_id,agent_id,generation_loss_id,revision,
      state,abandon_kind_code,semantic_digest,source_ref_digest,journal_digest),
  UNIQUE(project_session_id,run_id,agent_id,generation_loss_id,revision,
      semantic_digest,source_ref_digest,journal_digest,origin_fresh_apply_id,
      origin_fresh_apply_digest),
  UNIQUE(project_session_id,run_id,agent_id,generation_loss_id,revision,state,
      abandon_kind_code,recovery_action_adapter_id,recovery_action_id,
      active_recovery_custody_id,semantic_digest,source_ref_digest,journal_digest),
  UNIQUE(semantic_digest),
  UNIQUE(source_ref_digest),
  UNIQUE(journal_digest),
  CHECK((revision=1 AND prior_revision IS NULL AND
        prior_journal_digest IS NULL) OR
      (revision>1 AND prior_revision=revision-1 AND
        prior_journal_digest IS NOT NULL)),
  CHECK((receipt_batch_id IS NULL)=(receipt_apply_id IS NULL)),
  CHECK((receipt_batch_id IS NULL)=(receipt_apply_digest IS NULL)),
  CHECK((origin_fresh_apply_id IS NULL)=(origin_fresh_apply_digest IS NULL)),
  CHECK((revision=1 AND state='open' AND receipt_batch_id IS NULL AND
        origin_fresh_apply_id IS NULL) OR
      (revision>1 AND
        ((receipt_batch_id IS NOT NULL AND origin_fresh_apply_id IS NULL) OR
          (receipt_batch_id IS NULL AND origin_fresh_apply_id IS NOT NULL)))),
  CHECK(origin_fresh_apply_id IS NULL OR state='recovery-in-progress'),
  CHECK(state NOT IN ('recovered-adopted','abandoned') OR
      receipt_batch_id IS NOT NULL),
  CHECK((state='open' AND abandon_kind_code='none' AND
        recovery_action_id IS NULL AND active_recovery_custody_id IS NULL AND
        terminal_evidence_digest IS NULL) OR
      (state='recovery-in-progress' AND abandon_kind_code='none' AND
        recovery_action_id IS NOT NULL AND
        active_recovery_custody_id IS NOT NULL AND
        terminal_evidence_digest IS NULL) OR
      (state='recovered-adopted' AND abandon_kind_code='none' AND
        recovery_action_id IS NOT NULL AND
        active_recovery_custody_id IS NOT NULL AND
        terminal_evidence_digest IS NOT NULL) OR
      (state='abandoned' AND abandon_kind_code='direct-open' AND
        recovery_action_id IS NULL AND active_recovery_custody_id IS NULL AND
        terminal_evidence_digest IS NOT NULL) OR
      (state='abandoned' AND abandon_kind_code='recovery-attempt' AND
        recovery_action_id IS NOT NULL AND
        active_recovery_custody_id IS NOT NULL AND
        terminal_evidence_digest IS NOT NULL)),
  CHECK((recovery_action_adapter_id IS NULL)=(recovery_action_id IS NULL)),
  FOREIGN KEY(project_session_id,run_id,agent_id,generation_loss_id)
      REFERENCES lifecycle_generation_losses(
        project_session_id,run_id,agent_id,generation_loss_id),
  FOREIGN KEY(project_session_id,run_id,agent_id,generation_loss_id,
        prior_revision,prior_journal_digest)
      REFERENCES lifecycle_generation_loss_revisions(
        project_session_id,run_id,agent_id,generation_loss_id,revision,
        journal_digest),
  FOREIGN KEY(recovery_action_adapter_id,recovery_action_id)
      REFERENCES lifecycle_rotation_custodies(
        provider_action_adapter_id,provider_action_id),
  FOREIGN KEY(run_id,agent_id,active_recovery_custody_id)
      REFERENCES lifecycle_rotation_custodies(run_id,agent_id,custody_id),
  FOREIGN KEY(receipt_batch_id,receipt_apply_id,project_session_id,run_id,agent_id,
        generation_loss_id,revision,semantic_digest,source_ref_digest)
      REFERENCES lifecycle_receipt_generation_loss_effects(
        batch_id,planned_apply_id,project_session_id,run_id,agent_id,
        generation_loss_id,final_revision,final_semantic_digest,
        final_source_ref_digest)
      DEFERRABLE INITIALLY DEFERRED,
  FOREIGN KEY(receipt_apply_id,receipt_apply_digest,receipt_batch_id)
      REFERENCES lifecycle_transition_applies(
        apply_id,apply_digest,receipt_batch_id)
      DEFERRABLE INITIALLY DEFERRED,
  FOREIGN KEY(receipt_apply_id,receipt_batch_id)
      REFERENCES lifecycle_transition_applies(apply_id,receipt_batch_id)
      DEFERRABLE INITIALLY DEFERRED,
  FOREIGN KEY(origin_fresh_apply_id,origin_fresh_apply_digest,
        project_session_id,run_id,agent_id,generation_loss_id,revision,
        semantic_digest,source_ref_digest)
      REFERENCES lifecycle_transition_applies(
        apply_id,apply_digest,fresh_project_session_id,fresh_run_id,fresh_agent_id,
        fresh_generation_loss_id,fresh_generation_loss_after_revision,
        fresh_generation_loss_after_semantic_digest,
        fresh_generation_loss_after_source_ref_digest)
      DEFERRABLE INITIALLY DEFERRED
) STRICT;
CREATE TABLE lifecycle_generation_loss_heads (
  project_session_id TEXT NOT NULL,
  run_id TEXT NOT NULL,
  agent_id TEXT NOT NULL,
  generation_loss_id TEXT NOT NULL,
  current_revision INTEGER NOT NULL,
  state TEXT NOT NULL,
  abandon_kind_code TEXT NOT NULL,
  semantic_digest TEXT NOT NULL,
  source_ref_digest TEXT NOT NULL,
  journal_digest TEXT NOT NULL,
  terminal INTEGER NOT NULL CHECK(terminal IN (0,1)),
  head_revision INTEGER NOT NULL CHECK(head_revision >= 1),
  PRIMARY KEY(run_id,agent_id,generation_loss_id),
  UNIQUE(project_session_id,run_id,agent_id,generation_loss_id),
  FOREIGN KEY(project_session_id,run_id,agent_id,generation_loss_id,
        current_revision,state,abandon_kind_code,semantic_digest,
        source_ref_digest,journal_digest)
      REFERENCES lifecycle_generation_loss_revisions(
        project_session_id,run_id,agent_id,generation_loss_id,revision,
        state,abandon_kind_code,semantic_digest,source_ref_digest,journal_digest),
  CHECK((terminal=1)=(state IN ('recovered-adopted','abandoned'))),
  CHECK((state='abandoned')=(abandon_kind_code<>'none'))
) STRICT;
CREATE UNIQUE INDEX one_nonterminal_generation_loss_per_agent
  ON lifecycle_generation_loss_heads(run_id,agent_id)
  WHERE terminal=0;
CREATE TABLE lifecycle_custody_adoption_deliveries (
  run_id TEXT NOT NULL,
  agent_id TEXT NOT NULL,
  custody_id TEXT NOT NULL,
  ordinal INTEGER NOT NULL CHECK(ordinal>=1),
  delivery_id TEXT NOT NULL,
  delivery_generation INTEGER NOT NULL CHECK(delivery_generation>=1),
  recipient_agent_id TEXT NOT NULL,
  source_state TEXT NOT NULL CHECK(source_state IN ('claimed','provider-accepted')),
  active_owner INTEGER NOT NULL CHECK(active_owner IN (0,1)),
  PRIMARY KEY(run_id, agent_id, custody_id, ordinal),
  UNIQUE(run_id, agent_id, custody_id, delivery_id, delivery_generation),
  FOREIGN KEY(run_id, agent_id, custody_id)
      REFERENCES lifecycle_rotation_custodies(run_id, agent_id, custody_id)
) STRICT;
CREATE UNIQUE INDEX one_nonfinal_custody_per_delivery_generation
  ON lifecycle_custody_adoption_deliveries(run_id, delivery_id,
    delivery_generation)
  WHERE active_owner = 1;
CREATE TRIGGER lifecycle_custody_adoption_delivery_owner_update_guard
BEFORE UPDATE ON lifecycle_custody_adoption_deliveries
WHEN NOT (
  OLD.active_owner=1 AND NEW.active_owner=0
  AND NEW.run_id=OLD.run_id AND NEW.agent_id=OLD.agent_id
  AND NEW.custody_id=OLD.custody_id AND NEW.ordinal=OLD.ordinal
  AND NEW.delivery_id=OLD.delivery_id
  AND NEW.delivery_generation=OLD.delivery_generation
  AND NEW.recipient_agent_id=OLD.recipient_agent_id
  AND NEW.source_state=OLD.source_state
  AND EXISTS (
    SELECT 1 FROM lifecycle_rotation_custody_heads head
     WHERE head.run_id=OLD.run_id AND head.agent_id=OLD.agent_id
       AND head.custody_id=OLD.custody_id AND head.terminal=1
       AND head.disposition_code IN ('adopted','no-effect','superseded')
  )
)
BEGIN SELECT RAISE(ABORT,'INVARIANT_lifecycle_custody_adoption_delivery_owner'); END;
CREATE TRIGGER lifecycle_custody_adoption_delivery_owner_delete_guard
BEFORE DELETE ON lifecycle_custody_adoption_deliveries
BEGIN SELECT RAISE(ABORT,'INVARIANT_lifecycle_custody_adoption_delivery_owner'); END;
CREATE TABLE lifecycle_custody_write_leases (
  run_id TEXT NOT NULL,
  agent_id TEXT NOT NULL,
  custody_id TEXT NOT NULL,
  ordinal INTEGER NOT NULL CHECK(ordinal>=1),
  lease_id TEXT NOT NULL,
  lease_generation INTEGER NOT NULL CHECK(lease_generation>=1),
  source_status TEXT NOT NULL CHECK(source_status='active'),
  active_owner INTEGER NOT NULL CHECK(active_owner IN (0,1)),
  PRIMARY KEY(run_id,agent_id,custody_id,ordinal),
  UNIQUE(run_id,agent_id,custody_id,lease_id,lease_generation),
  FOREIGN KEY(run_id,agent_id,custody_id)
    REFERENCES lifecycle_rotation_custodies(run_id,agent_id,custody_id)
) STRICT;
CREATE UNIQUE INDEX one_lifecycle_custody_per_write_lease_generation
  ON lifecycle_custody_write_leases(run_id,lease_id,lease_generation)
  WHERE active_owner=1;
CREATE TRIGGER lifecycle_custody_write_lease_owner_update_guard
BEFORE UPDATE ON lifecycle_custody_write_leases
WHEN NOT (
  OLD.active_owner=1 AND NEW.active_owner=0
  AND NEW.run_id=OLD.run_id AND NEW.agent_id=OLD.agent_id
  AND NEW.custody_id=OLD.custody_id AND NEW.ordinal=OLD.ordinal
  AND NEW.lease_id=OLD.lease_id AND NEW.lease_generation=OLD.lease_generation
  AND NEW.source_status=OLD.source_status
  AND EXISTS (
    SELECT 1
      FROM lifecycle_rotation_custody_heads head
      JOIN leases lease
        ON lease.run_id=OLD.run_id AND lease.lease_id=OLD.lease_id
       AND lease.generation=OLD.lease_generation
     WHERE head.run_id=OLD.run_id AND head.agent_id=OLD.agent_id
       AND head.custody_id=OLD.custody_id AND head.terminal=1
       AND ((head.disposition_code IN ('no-effect','superseded') AND lease.status='active')
         OR (head.disposition_code IN ('adopted','quarantined') AND lease.status='quarantined'))
  )
)
BEGIN SELECT RAISE(ABORT,'INVARIANT_lifecycle_custody_write_lease_owner'); END;
CREATE TRIGGER lifecycle_custody_write_lease_owner_delete_guard
BEFORE DELETE ON lifecycle_custody_write_leases
BEGIN SELECT RAISE(ABORT,'INVARIANT_lifecycle_custody_write_lease_owner'); END;
CREATE TABLE agent_lifecycle_recovery_capability_issues (
  issue_id TEXT,
  capability_hash TEXT,
  operator_id TEXT,
  project_id TEXT,
  project_session_id TEXT,
  run_id TEXT,
  agent_id TEXT,
  session_revision INTEGER,
  session_generation INTEGER,
  run_revision INTEGER,
  recovery_source_kind TEXT,
  old_custody_id TEXT,
  old_action_adapter_id TEXT,
  old_action_id TEXT,
  old_custody_revision INTEGER,
  generation_loss_id TEXT,
  generation_loss_revision INTEGER,
  recovery_source_ref_digest TEXT,
  source_journal_digest TEXT,
  checkpoint_digest TEXT,
  source_provider_session_ref TEXT,
  source_capability_hash TEXT,
  source_custody_action_id TEXT,
  source_adapter_id TEXT,
  source_adapter_contract_digest TEXT,
  source_bridge_row_id TEXT,
  source_bridge_revision INTEGER,
  source_provider_generation INTEGER,
  source_principal_generation INTEGER,
  source_bridge_generation INTEGER,
  source_project_session_generation INTEGER,
  source_run_generation INTEGER,
  source_chair_lease_generation INTEGER,
  bridge_owner_kind TEXT,
  parent_capability_id TEXT,
  consequential_gate_id TEXT,
  path TEXT CHECK(path='fresh-rotate'),
  issuance_json TEXT,
  issuance_digest TEXT,
  issued_at INTEGER,
  expires_at INTEGER,
  PRIMARY KEY(issue_id),
  UNIQUE(capability_hash),
  UNIQUE(issuance_digest),
  UNIQUE(issue_id,project_session_id,run_id,agent_id,
      recovery_source_kind,recovery_source_ref_digest,source_journal_digest),
  CHECK((recovery_source_kind='custody' AND old_custody_id IS NOT NULL AND
        old_custody_revision IS NOT NULL AND generation_loss_id IS NULL AND
        generation_loss_revision IS NULL) OR
      (recovery_source_kind='generation-loss' AND old_custody_id IS NULL AND
        old_custody_revision IS NULL AND generation_loss_id IS NOT NULL AND
        generation_loss_revision IS NOT NULL)),
  FOREIGN KEY(project_session_id,run_id,agent_id,old_custody_id,
        old_custody_revision,recovery_source_ref_digest,source_journal_digest)
      REFERENCES lifecycle_rotation_custody_revisions(
        project_session_id,run_id,agent_id,custody_id,revision,
        source_ref_digest,journal_digest),
  FOREIGN KEY(project_session_id,run_id,agent_id,generation_loss_id,
        generation_loss_revision,recovery_source_ref_digest,source_journal_digest)
      REFERENCES lifecycle_generation_loss_revisions(
        project_session_id,run_id,agent_id,generation_loss_id,revision,
        source_ref_digest,journal_digest)
) STRICT;
CREATE TABLE agent_lifecycle_recovery_source_heads (
  project_session_id TEXT NOT NULL,
  run_id TEXT NOT NULL,
  agent_id TEXT NOT NULL,
  recovery_source_kind TEXT NOT NULL CHECK(
      recovery_source_kind IN ('custody','generation-loss')),
  recovery_source_ref_digest TEXT NOT NULL,
  issue_id TEXT NOT NULL UNIQUE,
  source_journal_digest TEXT NOT NULL,
  head_revision INTEGER NOT NULL CHECK(head_revision >= 1),
  PRIMARY KEY(project_session_id,run_id,agent_id,recovery_source_kind,
      recovery_source_ref_digest),
  UNIQUE(issue_id,project_session_id,run_id,agent_id,recovery_source_kind,
      recovery_source_ref_digest,source_journal_digest),
  FOREIGN KEY(issue_id,project_session_id,run_id,agent_id,recovery_source_kind,
        recovery_source_ref_digest,source_journal_digest)
      REFERENCES agent_lifecycle_recovery_capability_issues(
        issue_id,project_session_id,run_id,agent_id,recovery_source_kind,
        recovery_source_ref_digest,source_journal_digest)
) STRICT;
CREATE TABLE agent_lifecycle_recovery_issue_revocations (
  issue_id TEXT PRIMARY KEY,
  revocation_kind TEXT CHECK(
      revocation_kind IN ('operator-revoked','source-stale')),
  evidence_digest TEXT,
  revoked_at INTEGER,
  FOREIGN KEY(issue_id)
      REFERENCES agent_lifecycle_recovery_capability_issues(issue_id)
) STRICT;
CREATE TABLE agent_lifecycle_recovery_retirements (
  retirement_id TEXT PRIMARY KEY,
  project_session_id TEXT,
  run_id TEXT,
  agent_id TEXT,
  retirement_plan_digest TEXT,
  custody_id TEXT,
  custody_revision INTEGER,
  custody_source_ref_digest TEXT,
  custody_journal_digest TEXT,
  finalized_disposition TEXT,
  finalized_terminal_evidence_digest TEXT,
  admission_digest TEXT,
  transition_proof_digest TEXT,
  mutation_plan_digest TEXT,
  retirement_evidence_digest TEXT,
  retirement_effect_digest TEXT,
  receipt_batch_id TEXT UNIQUE,
  receipt_apply_id TEXT UNIQUE,
  receipt_apply_digest TEXT,
  retirement_json TEXT,
  retirement_digest TEXT UNIQUE,
  created_at INTEGER,
  UNIQUE(retirement_id,receipt_batch_id,receipt_apply_id,receipt_apply_digest),
  UNIQUE(retirement_digest),
  FOREIGN KEY(retirement_id,receipt_apply_id,project_session_id,run_id,agent_id,
        custody_id,custody_revision,custody_source_ref_digest,
        custody_journal_digest,finalized_disposition,
        finalized_terminal_evidence_digest,admission_digest,
        transition_proof_digest,mutation_plan_digest,retirement_evidence_digest,
        retirement_plan_digest)
      REFERENCES lifecycle_recovery_retirement_plans(
        retirement_id,planned_apply_id,project_session_id,run_id,agent_id,
        custody_id,custody_revision,custody_source_ref_digest,
        custody_journal_digest,finalized_disposition,
        finalized_terminal_evidence_digest,admission_digest,
        transition_proof_digest,mutation_plan_digest,retirement_evidence_digest,
        retirement_plan_digest),
  FOREIGN KEY(receipt_batch_id,receipt_apply_id,project_session_id,run_id,agent_id,
        retirement_id,retirement_plan_digest,custody_id,custody_revision,
        custody_source_ref_digest,custody_journal_digest,finalized_disposition,
        finalized_terminal_evidence_digest,admission_digest,
        transition_proof_digest,mutation_plan_digest,retirement_evidence_digest,
        retirement_effect_digest)
      REFERENCES lifecycle_receipt_recovery_retirement_effects(
        batch_id,planned_apply_id,project_session_id,run_id,agent_id,retirement_id,
        retirement_plan_digest,custody_id,custody_revision,
        custody_source_ref_digest,custody_journal_digest,finalized_disposition,
        finalized_terminal_evidence_digest,admission_digest,
        transition_proof_digest,mutation_plan_digest,retirement_evidence_digest,
        effect_digest),
  FOREIGN KEY(receipt_apply_id,receipt_apply_digest,receipt_batch_id)
      REFERENCES lifecycle_transition_applies(
        apply_id,apply_digest,receipt_batch_id)
      DEFERRABLE INITIALLY DEFERRED
) STRICT;
CREATE TRIGGER agent_lifecycle_recovery_capability_issues_immutable_update
BEFORE UPDATE ON agent_lifecycle_recovery_capability_issues
BEGIN
  SELECT RAISE(ABORT,'lifecycle-immutable-row');
END;
CREATE TRIGGER agent_lifecycle_recovery_capability_issues_immutable_delete
BEFORE DELETE ON agent_lifecycle_recovery_capability_issues
BEGIN
  SELECT RAISE(ABORT,'lifecycle-immutable-row');
END;
CREATE TRIGGER agent_lifecycle_recovery_issue_revocations_immutable_update
BEFORE UPDATE ON agent_lifecycle_recovery_issue_revocations
BEGIN
  SELECT RAISE(ABORT,'lifecycle-immutable-row');
END;
CREATE TRIGGER agent_lifecycle_recovery_issue_revocations_immutable_delete
BEFORE DELETE ON agent_lifecycle_recovery_issue_revocations
BEGIN
  SELECT RAISE(ABORT,'lifecycle-immutable-row');
END;
CREATE TRIGGER agent_lifecycle_recovery_retirements_immutable_update
BEFORE UPDATE ON agent_lifecycle_recovery_retirements
BEGIN
  SELECT RAISE(ABORT,'lifecycle-immutable-row');
END;
CREATE TRIGGER agent_lifecycle_recovery_retirements_immutable_delete
BEFORE DELETE ON agent_lifecycle_recovery_retirements
BEGIN
  SELECT RAISE(ABORT,'lifecycle-immutable-row');
END;
CREATE TRIGGER lifecycle_admitted_run_scopes_immutable_update
BEFORE UPDATE ON lifecycle_admitted_run_scopes
BEGIN
  SELECT RAISE(ABORT,'lifecycle-immutable-row');
END;
CREATE TRIGGER lifecycle_admitted_run_scopes_immutable_delete
BEFORE DELETE ON lifecycle_admitted_run_scopes
BEGIN
  SELECT RAISE(ABORT,'lifecycle-immutable-row');
END;
CREATE TRIGGER lifecycle_authority_receipts_immutable_update
BEFORE UPDATE ON lifecycle_authority_receipts
BEGIN
  SELECT RAISE(ABORT,'lifecycle-immutable-row');
END;
CREATE TRIGGER lifecycle_authority_receipts_immutable_delete
BEFORE DELETE ON lifecycle_authority_receipts
BEGIN
  SELECT RAISE(ABORT,'lifecycle-immutable-row');
END;
CREATE TRIGGER lifecycle_fresh_recovery_handoffs_immutable_update
BEFORE UPDATE ON lifecycle_fresh_recovery_handoffs
BEGIN
  SELECT RAISE(ABORT,'lifecycle-immutable-row');
END;
CREATE TRIGGER lifecycle_fresh_recovery_handoffs_immutable_delete
BEFORE DELETE ON lifecycle_fresh_recovery_handoffs
BEGIN
  SELECT RAISE(ABORT,'lifecycle-immutable-row');
END;
CREATE TRIGGER lifecycle_fresh_rotation_commits_immutable_update
BEFORE UPDATE ON lifecycle_fresh_rotation_commits
BEGIN
  SELECT RAISE(ABORT,'lifecycle-immutable-row');
END;
CREATE TRIGGER lifecycle_fresh_rotation_commits_immutable_delete
BEFORE DELETE ON lifecycle_fresh_rotation_commits
BEGIN
  SELECT RAISE(ABORT,'lifecycle-immutable-row');
END;
CREATE TRIGGER lifecycle_fresh_rotation_preparations_immutable_update
BEFORE UPDATE ON lifecycle_fresh_rotation_preparations
BEGIN
  SELECT RAISE(ABORT,'lifecycle-immutable-row');
END;
CREATE TRIGGER lifecycle_fresh_rotation_preparations_immutable_delete
BEFORE DELETE ON lifecycle_fresh_rotation_preparations
BEGIN
  SELECT RAISE(ABORT,'lifecycle-immutable-row');
END;
CREATE TRIGGER lifecycle_generation_loss_revisions_immutable_update
BEFORE UPDATE ON lifecycle_generation_loss_revisions
BEGIN
  SELECT RAISE(ABORT,'lifecycle-immutable-row');
END;
CREATE TRIGGER lifecycle_generation_loss_revisions_immutable_delete
BEFORE DELETE ON lifecycle_generation_loss_revisions
BEGIN
  SELECT RAISE(ABORT,'lifecycle-immutable-row');
END;
CREATE TRIGGER lifecycle_generation_losses_immutable_update
BEFORE UPDATE ON lifecycle_generation_losses
BEGIN
  SELECT RAISE(ABORT,'lifecycle-immutable-row');
END;
CREATE TRIGGER lifecycle_generation_losses_immutable_delete
BEFORE DELETE ON lifecycle_generation_losses
BEGIN
  SELECT RAISE(ABORT,'lifecycle-immutable-row');
END;
CREATE TRIGGER lifecycle_receipt_batch_authorizations_immutable_update
BEFORE UPDATE ON lifecycle_receipt_batch_authorizations
BEGIN
  SELECT RAISE(ABORT,'lifecycle-immutable-row');
END;
CREATE TRIGGER lifecycle_receipt_batch_authorizations_immutable_delete
BEFORE DELETE ON lifecycle_receipt_batch_authorizations
BEGIN
  SELECT RAISE(ABORT,'lifecycle-immutable-row');
END;
CREATE TRIGGER lifecycle_receipt_batch_completions_immutable_update
BEFORE UPDATE ON lifecycle_receipt_batch_completions
BEGIN
  SELECT RAISE(ABORT,'lifecycle-immutable-row');
END;
CREATE TRIGGER lifecycle_receipt_batch_completions_immutable_delete
BEFORE DELETE ON lifecycle_receipt_batch_completions
BEGIN
  SELECT RAISE(ABORT,'lifecycle-immutable-row');
END;
CREATE TRIGGER lifecycle_receipt_batches_immutable_update
BEFORE UPDATE ON lifecycle_receipt_batches
BEGIN
  SELECT RAISE(ABORT,'lifecycle-immutable-row');
END;
CREATE TRIGGER lifecycle_receipt_batches_immutable_delete
BEFORE DELETE ON lifecycle_receipt_batches
BEGIN
  SELECT RAISE(ABORT,'lifecycle-immutable-row');
END;
CREATE TRIGGER lifecycle_receipt_custody_effects_immutable_update
BEFORE UPDATE ON lifecycle_receipt_custody_effects
BEGIN
  SELECT RAISE(ABORT,'lifecycle-immutable-row');
END;
CREATE TRIGGER lifecycle_receipt_custody_effects_immutable_delete
BEFORE DELETE ON lifecycle_receipt_custody_effects
BEGIN
  SELECT RAISE(ABORT,'lifecycle-immutable-row');
END;
CREATE TRIGGER lifecycle_receipt_fresh_origin_effects_immutable_update
BEFORE UPDATE ON lifecycle_receipt_fresh_origin_effects
BEGIN
  SELECT RAISE(ABORT,'lifecycle-immutable-row');
END;
CREATE TRIGGER lifecycle_receipt_fresh_origin_effects_immutable_delete
BEFORE DELETE ON lifecycle_receipt_fresh_origin_effects
BEGIN
  SELECT RAISE(ABORT,'lifecycle-immutable-row');
END;
CREATE TRIGGER lifecycle_receipt_generation_loss_effects_immutable_update
BEFORE UPDATE ON lifecycle_receipt_generation_loss_effects
BEGIN
  SELECT RAISE(ABORT,'lifecycle-immutable-row');
END;
CREATE TRIGGER lifecycle_receipt_generation_loss_effects_immutable_delete
BEFORE DELETE ON lifecycle_receipt_generation_loss_effects
BEGIN
  SELECT RAISE(ABORT,'lifecycle-immutable-row');
END;
CREATE TRIGGER lifecycle_receipt_intents_immutable_update
BEFORE UPDATE ON lifecycle_receipt_intents
BEGIN
  SELECT RAISE(ABORT,'lifecycle-immutable-row');
END;
CREATE TRIGGER lifecycle_receipt_intents_immutable_delete
BEFORE DELETE ON lifecycle_receipt_intents
BEGIN
  SELECT RAISE(ABORT,'lifecycle-immutable-row');
END;
CREATE TRIGGER lifecycle_receipt_namespace_checkpoints_immutable_update
BEFORE UPDATE ON lifecycle_receipt_namespace_checkpoints
BEGIN
  SELECT RAISE(ABORT,'lifecycle-immutable-row');
END;
CREATE TRIGGER lifecycle_receipt_namespace_checkpoints_immutable_delete
BEFORE DELETE ON lifecycle_receipt_namespace_checkpoints
BEGIN
  SELECT RAISE(ABORT,'lifecycle-immutable-row');
END;
CREATE TRIGGER lifecycle_receipt_namespace_members_immutable_update
BEFORE UPDATE ON lifecycle_receipt_namespace_members
BEGIN
  SELECT RAISE(ABORT,'lifecycle-immutable-row');
END;
CREATE TRIGGER lifecycle_receipt_namespace_members_immutable_delete
BEFORE DELETE ON lifecycle_receipt_namespace_members
BEGIN
  SELECT RAISE(ABORT,'lifecycle-immutable-row');
END;
CREATE TRIGGER lifecycle_receipt_projects_immutable_update
BEFORE UPDATE ON lifecycle_receipt_projects
BEGIN
  SELECT RAISE(ABORT,'lifecycle-immutable-row');
END;
CREATE TRIGGER lifecycle_receipt_projects_immutable_delete
BEFORE DELETE ON lifecycle_receipt_projects
BEGIN
  SELECT RAISE(ABORT,'lifecycle-immutable-row');
END;
CREATE TRIGGER lifecycle_receipt_recovery_retirement_effects_immutable_update
BEFORE UPDATE ON lifecycle_receipt_recovery_retirement_effects
BEGIN
  SELECT RAISE(ABORT,'lifecycle-immutable-row');
END;
CREATE TRIGGER lifecycle_receipt_recovery_retirement_effects_immutable_delete
BEFORE DELETE ON lifecycle_receipt_recovery_retirement_effects
BEGIN
  SELECT RAISE(ABORT,'lifecycle-immutable-row');
END;
CREATE TRIGGER lifecycle_receipt_scope_checkpoints_immutable_update
BEFORE UPDATE ON lifecycle_receipt_scope_checkpoints
BEGIN
  SELECT RAISE(ABORT,'lifecycle-immutable-row');
END;
CREATE TRIGGER lifecycle_receipt_scope_checkpoints_immutable_delete
BEFORE DELETE ON lifecycle_receipt_scope_checkpoints
BEGIN
  SELECT RAISE(ABORT,'lifecycle-immutable-row');
END;
CREATE TRIGGER lifecycle_recovery_retirement_plans_immutable_update
BEFORE UPDATE ON lifecycle_recovery_retirement_plans
BEGIN
  SELECT RAISE(ABORT,'lifecycle-immutable-row');
END;
CREATE TRIGGER lifecycle_recovery_retirement_plans_immutable_delete
BEFORE DELETE ON lifecycle_recovery_retirement_plans
BEGIN
  SELECT RAISE(ABORT,'lifecycle-immutable-row');
END;
CREATE TRIGGER lifecycle_review_adoption_reservations_immutable_update
BEFORE UPDATE ON lifecycle_review_adoption_reservations
BEGIN
  SELECT RAISE(ABORT,'lifecycle-immutable-row');
END;
CREATE TRIGGER lifecycle_review_adoption_reservations_immutable_delete
BEFORE DELETE ON lifecycle_review_adoption_reservations
BEGIN
  SELECT RAISE(ABORT,'lifecycle-immutable-row');
END;
CREATE TRIGGER lifecycle_review_authority_bindings_immutable_update
BEFORE UPDATE ON lifecycle_review_authority_bindings
BEGIN
  SELECT RAISE(ABORT,'lifecycle-immutable-row');
END;
CREATE TRIGGER lifecycle_review_authority_bindings_immutable_delete
BEFORE DELETE ON lifecycle_review_authority_bindings
BEGIN
  SELECT RAISE(ABORT,'lifecycle-immutable-row');
END;
CREATE TRIGGER lifecycle_rotation_custodies_immutable_update
BEFORE UPDATE ON lifecycle_rotation_custodies
BEGIN
  SELECT RAISE(ABORT,'lifecycle-immutable-row');
END;
CREATE TRIGGER lifecycle_rotation_custodies_immutable_delete
BEFORE DELETE ON lifecycle_rotation_custodies
BEGIN
  SELECT RAISE(ABORT,'lifecycle-immutable-row');
END;
CREATE TRIGGER lifecycle_rotation_custody_revisions_immutable_update
BEFORE UPDATE ON lifecycle_rotation_custody_revisions
BEGIN
  SELECT RAISE(ABORT,'lifecycle-immutable-row');
END;
CREATE TRIGGER lifecycle_rotation_custody_revisions_immutable_delete
BEFORE DELETE ON lifecycle_rotation_custody_revisions
BEGIN
  SELECT RAISE(ABORT,'lifecycle-immutable-row');
END;
CREATE TRIGGER lifecycle_scope_admission_outbox_immutable_update
BEFORE UPDATE ON lifecycle_scope_admission_outbox
BEGIN
  SELECT RAISE(ABORT,'lifecycle-immutable-row');
END;
CREATE TRIGGER lifecycle_scope_admission_outbox_immutable_delete
BEFORE DELETE ON lifecycle_scope_admission_outbox
BEGIN
  SELECT RAISE(ABORT,'lifecycle-immutable-row');
END;
CREATE TRIGGER lifecycle_scope_admission_resolutions_immutable_update
BEFORE UPDATE ON lifecycle_scope_admission_resolutions
BEGIN
  SELECT RAISE(ABORT,'lifecycle-immutable-row');
END;
CREATE TRIGGER lifecycle_scope_admission_resolutions_immutable_delete
BEFORE DELETE ON lifecycle_scope_admission_resolutions
BEGIN
  SELECT RAISE(ABORT,'lifecycle-immutable-row');
END;
CREATE TRIGGER lifecycle_transition_applies_immutable_update
BEFORE UPDATE ON lifecycle_transition_applies
BEGIN
  SELECT RAISE(ABORT,'lifecycle-immutable-row');
END;
CREATE TRIGGER lifecycle_transition_applies_immutable_delete
BEFORE DELETE ON lifecycle_transition_applies
BEGIN
  SELECT RAISE(ABORT,'lifecycle-immutable-row');
END;
CREATE TRIGGER lifecycle_rotation_custody_revision_append_guard
BEFORE INSERT ON lifecycle_rotation_custody_revisions
BEGIN
  SELECT CASE
    WHEN NEW.revision=1 AND (
      NEW.prior_revision IS NOT NULL OR NEW.prior_journal_digest IS NOT NULL OR
      NEW.state NOT IN ('awaiting-boundary','prepared')
    ) THEN RAISE(ABORT,'lifecycle-custody-revision-one-invalid')
    WHEN NEW.revision>1 AND NOT EXISTS (
      SELECT 1 FROM lifecycle_rotation_custody_heads h
      JOIN lifecycle_rotation_custody_revisions p
        ON p.project_session_id=h.project_session_id
       AND p.run_id=h.run_id AND p.agent_id=h.agent_id
       AND p.custody_id=h.custody_id AND p.revision=h.current_revision
       AND p.journal_digest=h.journal_digest
      WHERE h.project_session_id=NEW.project_session_id
        AND h.run_id=NEW.run_id AND h.agent_id=NEW.agent_id
        AND h.custody_id=NEW.custody_id AND h.terminal=0
        AND h.current_revision=NEW.prior_revision
        AND h.journal_digest=NEW.prior_journal_digest
        AND NEW.revision=h.current_revision+1
        AND (
          (p.state='awaiting-boundary' AND NEW.state='prepared' AND NEW.disposition_code='none') OR
          (p.state='prepared' AND NEW.state='dispatched' AND NEW.disposition_code='none') OR
          (p.state='dispatched' AND NEW.state IN ('accepted','ambiguous','provider-terminal') AND NEW.disposition_code='none') OR
          (p.state='accepted' AND NEW.state IN ('ambiguous','provider-terminal') AND NEW.disposition_code='none') OR
          (p.state='ambiguous' AND NEW.state='provider-terminal' AND NEW.disposition_code='none') OR
          (p.state='provider-terminal' AND NEW.state='committing' AND NEW.disposition_code='none') OR
          (p.state='committing' AND NEW.state='committing' AND NEW.disposition_code='none'
            AND NEW.terminal_evidence_digest=p.terminal_evidence_digest
            AND EXISTS (
              SELECT 1
                FROM lifecycle_receipt_custody_effects effect
                JOIN lifecycle_receipt_batches batch ON batch.batch_id=effect.batch_id
                JOIN lifecycle_receipt_intents intent
                  ON intent.batch_id=batch.batch_id AND intent.ordinal=1
                 AND intent.kind='custody-terminal'
                 AND intent.custody_effect_digest=effect.effect_digest
                JOIN lifecycle_authority_receipts receipt
                  ON receipt.intent_digest=intent.intent_digest
                 AND receipt.batch_id=intent.batch_id
                 AND receipt.ordinal=intent.ordinal
                 AND receipt.project_session_id=intent.project_session_id
                 AND receipt.run_id=intent.run_id AND receipt.agent_id=intent.agent_id
                 AND receipt.kind=intent.kind
                 AND receipt.subject_owner_kind=intent.subject_owner_kind
                 AND receipt.subject_owner_id=intent.subject_owner_id
                 AND receipt.subject_owner_revision=intent.subject_owner_revision
                 AND receipt.subject_digest=intent.subject_digest
               WHERE effect.project_session_id=NEW.project_session_id
                 AND effect.run_id=NEW.run_id AND effect.agent_id=NEW.agent_id
                 AND effect.custody_id=NEW.custody_id
                 AND effect.pre_revision=p.revision
                 AND effect.pre_journal_digest=p.journal_digest
                 AND effect.final_revision=NEW.revision
                 AND batch.planned_apply_id=NEW.custody_id || ':apply'
                 AND batch.transition_kind='custody-terminal'
                 AND batch.planned_apply_kind='terminal'
                 AND json_extract(batch.transition_replay_json,
                       '$.terminalDisposition')='adopted'
                 AND receipt.verified_at IS NOT NULL
                 AND NOT EXISTS (
                   SELECT 1 FROM lifecycle_transition_applies applied
                    WHERE applied.receipt_batch_id=batch.batch_id
                       OR applied.apply_id=batch.planned_apply_id
                 )
            )) OR
          (NEW.state='finalized' AND NEW.disposition_code IN ('quarantined','abandoned')) OR
          (p.state IN ('awaiting-boundary','prepared') AND NEW.state='finalized' AND NEW.disposition_code IN ('no-effect','superseded')) OR
          (p.state='provider-terminal' AND NEW.state='finalized' AND NEW.disposition_code IN ('no-effect','superseded')) OR
          (p.state='committing' AND NEW.state='finalized' AND NEW.disposition_code IN ('adopted','superseded'))
        )
    ) THEN RAISE(ABORT,'lifecycle-custody-revision-not-contiguous')
  END;
END;
CREATE TRIGGER lifecycle_rotation_custody_head_insert_guard
BEFORE INSERT ON lifecycle_rotation_custody_heads
WHEN NEW.current_revision<>1 OR NEW.head_revision<>1 OR NOT EXISTS (
  SELECT 1 FROM lifecycle_rotation_custody_revisions r
  WHERE r.project_session_id=NEW.project_session_id
    AND r.run_id=NEW.run_id AND r.agent_id=NEW.agent_id
    AND r.custody_id=NEW.custody_id AND r.revision=1
    AND r.state=NEW.state AND r.disposition_code=NEW.disposition_code
    AND r.semantic_digest=NEW.semantic_digest
    AND r.source_ref_digest=NEW.source_ref_digest
    AND r.journal_digest=NEW.journal_digest
)
BEGIN
  SELECT RAISE(ABORT,'lifecycle-custody-head-insert-invalid');
END;
CREATE TRIGGER lifecycle_rotation_custody_head_update_guard
BEFORE UPDATE ON lifecycle_rotation_custody_heads
WHEN NEW.project_session_id IS NOT OLD.project_session_id
  OR NEW.run_id IS NOT OLD.run_id OR NEW.agent_id IS NOT OLD.agent_id
  OR NEW.custody_id IS NOT OLD.custody_id OR OLD.terminal<>0
  OR NEW.head_revision<>OLD.head_revision+1
  OR NEW.current_revision<>OLD.current_revision+1
  OR NOT EXISTS (
    SELECT 1 FROM lifecycle_rotation_custody_revisions r
    WHERE r.project_session_id=NEW.project_session_id
      AND r.run_id=NEW.run_id AND r.agent_id=NEW.agent_id
      AND r.custody_id=NEW.custody_id AND r.revision=NEW.current_revision
      AND r.prior_revision=OLD.current_revision
      AND r.prior_journal_digest=OLD.journal_digest
      AND r.state=NEW.state AND r.disposition_code=NEW.disposition_code
      AND r.semantic_digest=NEW.semantic_digest
      AND r.source_ref_digest=NEW.source_ref_digest
      AND r.journal_digest=NEW.journal_digest
  )
BEGIN
  SELECT RAISE(ABORT,'lifecycle-custody-head-cas-failed');
END;
CREATE TRIGGER lifecycle_rotation_custody_head_delete_guard
BEFORE DELETE ON lifecycle_rotation_custody_heads
BEGIN
  SELECT RAISE(ABORT,'lifecycle-custody-head-delete-denied');
END;
CREATE TRIGGER lifecycle_generation_loss_revision_append_guard
BEFORE INSERT ON lifecycle_generation_loss_revisions
BEGIN
  SELECT CASE
    WHEN NEW.revision=1 AND (
      NEW.prior_revision IS NOT NULL OR NEW.prior_journal_digest IS NOT NULL OR
      NEW.state<>'open'
    ) THEN RAISE(ABORT,'lifecycle-loss-revision-one-invalid')
    WHEN NEW.revision>1 AND NOT EXISTS (
      SELECT 1 FROM lifecycle_generation_loss_heads h
      JOIN lifecycle_generation_loss_revisions p
        ON p.project_session_id=h.project_session_id
       AND p.run_id=h.run_id AND p.agent_id=h.agent_id
       AND p.generation_loss_id=h.generation_loss_id
       AND p.revision=h.current_revision AND p.journal_digest=h.journal_digest
      WHERE h.project_session_id=NEW.project_session_id
        AND h.run_id=NEW.run_id AND h.agent_id=NEW.agent_id
        AND h.generation_loss_id=NEW.generation_loss_id AND h.terminal=0
        AND h.current_revision=NEW.prior_revision
        AND h.journal_digest=NEW.prior_journal_digest
        AND NEW.revision=h.current_revision+1
        AND (
          (p.state='open' AND NEW.state IN ('recovery-in-progress','abandoned')) OR
          (p.state='recovery-in-progress' AND NEW.state IN ('open','recovered-adopted','abandoned'))
        )
    ) THEN RAISE(ABORT,'lifecycle-loss-revision-not-contiguous')
  END;
END;
CREATE TRIGGER lifecycle_generation_loss_head_insert_guard
BEFORE INSERT ON lifecycle_generation_loss_heads
WHEN NEW.current_revision<>1 OR NEW.head_revision<>1 OR NOT EXISTS (
  SELECT 1 FROM lifecycle_generation_loss_revisions r
  WHERE r.project_session_id=NEW.project_session_id
    AND r.run_id=NEW.run_id AND r.agent_id=NEW.agent_id
    AND r.generation_loss_id=NEW.generation_loss_id AND r.revision=1
    AND r.state=NEW.state AND r.abandon_kind_code=NEW.abandon_kind_code
    AND r.semantic_digest=NEW.semantic_digest
    AND r.source_ref_digest=NEW.source_ref_digest
    AND r.journal_digest=NEW.journal_digest
)
BEGIN
  SELECT RAISE(ABORT,'lifecycle-loss-head-insert-invalid');
END;
CREATE TRIGGER lifecycle_generation_loss_head_update_guard
BEFORE UPDATE ON lifecycle_generation_loss_heads
WHEN NEW.project_session_id IS NOT OLD.project_session_id
  OR NEW.run_id IS NOT OLD.run_id OR NEW.agent_id IS NOT OLD.agent_id
  OR NEW.generation_loss_id IS NOT OLD.generation_loss_id OR OLD.terminal<>0
  OR NEW.head_revision<>OLD.head_revision+1
  OR NEW.current_revision<>OLD.current_revision+1
  OR NOT EXISTS (
    SELECT 1 FROM lifecycle_generation_loss_revisions r
    WHERE r.project_session_id=NEW.project_session_id
      AND r.run_id=NEW.run_id AND r.agent_id=NEW.agent_id
      AND r.generation_loss_id=NEW.generation_loss_id
      AND r.revision=NEW.current_revision
      AND r.prior_revision=OLD.current_revision
      AND r.prior_journal_digest=OLD.journal_digest
      AND r.state=NEW.state AND r.abandon_kind_code=NEW.abandon_kind_code
      AND r.semantic_digest=NEW.semantic_digest
      AND r.source_ref_digest=NEW.source_ref_digest
      AND r.journal_digest=NEW.journal_digest
  )
BEGIN
  SELECT RAISE(ABORT,'lifecycle-loss-head-cas-failed');
END;
CREATE TRIGGER lifecycle_generation_loss_head_delete_guard
BEFORE DELETE ON lifecycle_generation_loss_heads
BEGIN
  SELECT RAISE(ABORT,'lifecycle-loss-head-delete-denied');
END;


-- Direct-SQL lifecycle guards: immutable histories advance through exact CAS heads.

CREATE TRIGGER agent_lifecycle_identity_high_water_insert
BEFORE INSERT ON agent_lifecycle_identity_high_water
WHEN NEW.revision IS NULL OR NEW.revision<>1
  OR NEW.provider_generation IS NULL
  OR NEW.provider_generation NOT BETWEEN 1 AND 9007199254740991
  OR NEW.principal_generation IS NULL
  OR NEW.principal_generation NOT BETWEEN 1 AND 9007199254740991
BEGIN
  SELECT RAISE(ABORT,'INVARIANT_agent_lifecycle_identity_high_water_cas');
END;
CREATE TRIGGER agent_lifecycle_identity_high_water_update
BEFORE UPDATE ON agent_lifecycle_identity_high_water
WHEN NEW.run_id IS NOT OLD.run_id OR NEW.agent_id IS NOT OLD.agent_id
  OR NEW.revision IS NULL OR NEW.revision<>OLD.revision+1
  OR NEW.provider_generation IS NULL OR NEW.principal_generation IS NULL
  OR NEW.revision NOT BETWEEN 1 AND 9007199254740991
  OR NEW.provider_generation NOT BETWEEN 1 AND 9007199254740991
  OR NEW.principal_generation NOT BETWEEN 1 AND 9007199254740991
  OR NEW.provider_generation<OLD.provider_generation
  OR NEW.principal_generation NOT IN (OLD.principal_generation,OLD.principal_generation+1)
  OR (NEW.provider_generation=OLD.provider_generation AND
      NEW.principal_generation=OLD.principal_generation)
BEGIN
  SELECT RAISE(ABORT,'INVARIANT_agent_lifecycle_identity_high_water_cas');
END;
CREATE TRIGGER agent_lifecycle_identity_high_water_delete
BEFORE DELETE ON agent_lifecycle_identity_high_water
BEGIN
  SELECT RAISE(ABORT,'INVARIANT_agent_lifecycle_identity_high_water_cas');
END;

CREATE TRIGGER agent_lifecycle_bridge_high_water_insert
BEFORE INSERT ON agent_lifecycle_bridge_high_water
WHEN NEW.revision IS NULL OR NEW.revision<>1
  OR NEW.bridge_generation IS NULL
  OR NEW.bridge_generation NOT BETWEEN 1 AND 9007199254740991
BEGIN
  SELECT RAISE(ABORT,'INVARIANT_agent_lifecycle_bridge_high_water_cas');
END;
CREATE TRIGGER agent_lifecycle_bridge_high_water_update
BEFORE UPDATE ON agent_lifecycle_bridge_high_water
WHEN NEW.run_id IS NOT OLD.run_id OR NEW.agent_id IS NOT OLD.agent_id
  OR NEW.bridge_owner_kind IS NOT OLD.bridge_owner_kind
  OR NEW.revision IS NULL OR NEW.revision<>OLD.revision+1
  OR NEW.bridge_generation IS NULL OR NEW.bridge_generation<>OLD.bridge_generation+1
  OR NEW.revision NOT BETWEEN 1 AND 9007199254740991
  OR NEW.bridge_generation NOT BETWEEN 1 AND 9007199254740991
BEGIN
  SELECT RAISE(ABORT,'INVARIANT_agent_lifecycle_bridge_high_water_cas');
END;
CREATE TRIGGER agent_lifecycle_bridge_high_water_delete
BEFORE DELETE ON agent_lifecycle_bridge_high_water
BEGIN
  SELECT RAISE(ABORT,'INVARIANT_agent_lifecycle_bridge_high_water_cas');
END;

CREATE TRIGGER agent_lifecycle_context_high_water_insert
BEFORE INSERT ON agent_lifecycle_context_high_water
WHEN NEW.revision IS NULL OR NEW.revision<>1
  OR NEW.provider_generation IS NULL
  OR NEW.provider_generation NOT BETWEEN 1 AND 9007199254740991
  OR NEW.context_revision IS NULL
  OR NEW.context_revision NOT BETWEEN 0 AND 9007199254740991
BEGIN
  SELECT RAISE(ABORT,'INVARIANT_agent_lifecycle_context_high_water_cas');
END;
CREATE TRIGGER agent_lifecycle_context_high_water_update
BEFORE UPDATE ON agent_lifecycle_context_high_water
WHEN NEW.run_id IS NOT OLD.run_id OR NEW.agent_id IS NOT OLD.agent_id
  OR NEW.provider_generation IS NOT OLD.provider_generation
  OR NEW.revision IS NULL OR NEW.revision<>OLD.revision+1
  OR NEW.context_revision IS NULL OR NEW.context_revision<=OLD.context_revision
  OR NEW.revision NOT BETWEEN 1 AND 9007199254740991
  OR NEW.provider_generation NOT BETWEEN 1 AND 9007199254740991
  OR NEW.context_revision NOT BETWEEN 0 AND 9007199254740991
BEGIN
  SELECT RAISE(ABORT,'INVARIANT_agent_lifecycle_context_high_water_cas');
END;
CREATE TRIGGER agent_lifecycle_context_high_water_delete
BEFORE DELETE ON agent_lifecycle_context_high_water
BEGIN
  SELECT RAISE(ABORT,'INVARIANT_agent_lifecycle_context_high_water_cas');
END;

CREATE TRIGGER lifecycle_receipt_scope_head_insert_guard
BEFORE INSERT ON lifecycle_receipt_scope_heads
WHEN NEW.revision IS NULL OR NEW.revision<>1 OR NOT EXISTS (
  SELECT 1 FROM lifecycle_receipt_scope_checkpoints checkpoint
   WHERE checkpoint.project_session_id=NEW.project_session_id
     AND checkpoint.run_id=NEW.run_id
     AND checkpoint.checkpoint_digest=NEW.checkpoint_digest
)
BEGIN
  SELECT RAISE(ABORT,'lifecycle-scope-head-insert-invalid');
END;
CREATE TRIGGER lifecycle_receipt_scope_head_update_guard
BEFORE UPDATE ON lifecycle_receipt_scope_heads
WHEN NEW.project_session_id IS NOT OLD.project_session_id
  OR NEW.run_id IS NOT OLD.run_id
  OR NEW.revision IS NULL OR NEW.revision<>OLD.revision+1
  OR NOT EXISTS (
    SELECT 1 FROM lifecycle_receipt_scope_checkpoints checkpoint
     WHERE checkpoint.project_session_id=NEW.project_session_id
       AND checkpoint.run_id=NEW.run_id
       AND checkpoint.checkpoint_digest=NEW.checkpoint_digest
  )
BEGIN
  SELECT RAISE(ABORT,'lifecycle-scope-head-cas-failed');
END;
CREATE TRIGGER lifecycle_receipt_scope_head_delete_guard
BEFORE DELETE ON lifecycle_receipt_scope_heads
BEGIN
  SELECT RAISE(ABORT,'lifecycle-scope-head-delete-denied');
END;

CREATE TRIGGER lifecycle_receipt_namespace_head_insert_guard
BEFORE INSERT ON lifecycle_receipt_namespace_heads
WHEN NEW.head_revision IS NULL OR NEW.head_revision<>1 OR NOT EXISTS (
  SELECT 1 FROM lifecycle_receipt_namespace_checkpoints checkpoint
   WHERE checkpoint.project_id=NEW.project_id
     AND checkpoint.authority_id=NEW.authority_id
     AND checkpoint.scope_count=NEW.scope_count
     AND checkpoint.ordered_scope_head_set_digest=NEW.ordered_scope_head_set_digest
     AND checkpoint.checkpoint_digest=NEW.checkpoint_digest
)
BEGIN
  SELECT RAISE(ABORT,'lifecycle-namespace-head-insert-invalid');
END;
CREATE TRIGGER lifecycle_receipt_namespace_head_update_guard
BEFORE UPDATE ON lifecycle_receipt_namespace_heads
WHEN NEW.project_id IS NOT OLD.project_id
  OR NEW.head_revision IS NULL OR NEW.head_revision<>OLD.head_revision+1
  OR NOT EXISTS (
    SELECT 1 FROM lifecycle_receipt_namespace_checkpoints checkpoint
     WHERE checkpoint.project_id=NEW.project_id
       AND checkpoint.authority_id=NEW.authority_id
       AND checkpoint.scope_count=NEW.scope_count
       AND checkpoint.ordered_scope_head_set_digest=NEW.ordered_scope_head_set_digest
       AND checkpoint.checkpoint_digest=NEW.checkpoint_digest
  )
BEGIN
  SELECT RAISE(ABORT,'lifecycle-namespace-head-cas-failed');
END;
CREATE TRIGGER lifecycle_receipt_namespace_head_delete_guard
BEFORE DELETE ON lifecycle_receipt_namespace_heads
BEGIN
  SELECT RAISE(ABORT,'lifecycle-namespace-head-delete-denied');
END;

CREATE TRIGGER agent_lifecycle_recovery_source_head_insert_guard
BEFORE INSERT ON agent_lifecycle_recovery_source_heads
WHEN NEW.head_revision IS NULL OR NEW.head_revision<>1 OR NOT EXISTS (
  SELECT 1 FROM agent_lifecycle_recovery_capability_issues issue
   WHERE issue.issue_id=NEW.issue_id
     AND issue.project_session_id=NEW.project_session_id
     AND issue.run_id=NEW.run_id
     AND issue.agent_id=NEW.agent_id
     AND issue.recovery_source_kind=NEW.recovery_source_kind
     AND issue.recovery_source_ref_digest=NEW.recovery_source_ref_digest
     AND issue.source_journal_digest=NEW.source_journal_digest
)
BEGIN
  SELECT RAISE(ABORT,'LIFECYCLE_RECOVERY_SOURCE_HEAD_INVALID');
END;
CREATE TRIGGER agent_lifecycle_recovery_source_head_update_guard
BEFORE UPDATE ON agent_lifecycle_recovery_source_heads
WHEN NEW.project_session_id IS NOT OLD.project_session_id
  OR NEW.run_id IS NOT OLD.run_id
  OR NEW.agent_id IS NOT OLD.agent_id
  OR NEW.recovery_source_kind IS NOT OLD.recovery_source_kind
  OR NEW.recovery_source_ref_digest IS NOT OLD.recovery_source_ref_digest
  OR NEW.head_revision IS NULL OR NEW.head_revision<>OLD.head_revision+1
  OR NOT EXISTS (
    SELECT 1 FROM agent_lifecycle_recovery_capability_issues issue
     WHERE issue.issue_id=NEW.issue_id
       AND issue.project_session_id=NEW.project_session_id
       AND issue.run_id=NEW.run_id
       AND issue.agent_id=NEW.agent_id
       AND issue.recovery_source_kind=NEW.recovery_source_kind
       AND issue.recovery_source_ref_digest=NEW.recovery_source_ref_digest
       AND issue.source_journal_digest=NEW.source_journal_digest
  )
BEGIN
  SELECT RAISE(ABORT,'LIFECYCLE_RECOVERY_SOURCE_HEAD_CAS');
END;
CREATE TRIGGER agent_lifecycle_recovery_source_head_delete_guard
BEFORE DELETE ON agent_lifecycle_recovery_source_heads
BEGIN
  SELECT RAISE(ABORT,'LIFECYCLE_RECOVERY_SOURCE_HEAD_DELETE');
END;

CREATE TRIGGER agent_lifecycle_recovery_capability_issue_source_head_after_insert
AFTER INSERT ON agent_lifecycle_recovery_capability_issues
BEGIN
  INSERT INTO agent_lifecycle_recovery_source_heads(
    project_session_id,run_id,agent_id,recovery_source_kind,
    recovery_source_ref_digest,issue_id,source_journal_digest,head_revision)
  SELECT NEW.project_session_id,NEW.run_id,NEW.agent_id,NEW.recovery_source_kind,
    NEW.recovery_source_ref_digest,NEW.issue_id,NEW.source_journal_digest,1
  WHERE NOT EXISTS (
    SELECT 1 FROM agent_lifecycle_recovery_source_heads head
     WHERE head.project_session_id=NEW.project_session_id
       AND head.run_id=NEW.run_id
       AND head.agent_id=NEW.agent_id
       AND head.recovery_source_kind=NEW.recovery_source_kind
       AND head.recovery_source_ref_digest=NEW.recovery_source_ref_digest
  );
  UPDATE agent_lifecycle_recovery_source_heads
     SET issue_id=NEW.issue_id,
         source_journal_digest=NEW.source_journal_digest,
         head_revision=head_revision+1
   WHERE project_session_id=NEW.project_session_id
     AND run_id=NEW.run_id
     AND agent_id=NEW.agent_id
     AND recovery_source_kind=NEW.recovery_source_kind
     AND recovery_source_ref_digest=NEW.recovery_source_ref_digest
     AND issue_id<>NEW.issue_id
     AND EXISTS (
       SELECT 1 FROM agent_lifecycle_recovery_capability_issues old_issue
        WHERE old_issue.issue_id=agent_lifecycle_recovery_source_heads.issue_id
          AND (EXISTS (
            SELECT 1 FROM agent_lifecycle_recovery_issue_revocations revocation
             WHERE revocation.issue_id=old_issue.issue_id
          ) OR old_issue.expires_at<=NEW.issued_at)
          AND NOT EXISTS (
            SELECT 1 FROM lifecycle_fresh_recovery_handoffs handoff
             WHERE handoff.issue_id=old_issue.issue_id
          )
     );
  SELECT RAISE(ABORT,'LIFECYCLE_RECOVERY_SOURCE_BUSY')
   WHERE NOT EXISTS (
    SELECT 1 FROM agent_lifecycle_recovery_source_heads head
     WHERE head.project_session_id=NEW.project_session_id
       AND head.run_id=NEW.run_id
       AND head.agent_id=NEW.agent_id
       AND head.recovery_source_kind=NEW.recovery_source_kind
       AND head.recovery_source_ref_digest=NEW.recovery_source_ref_digest
       AND head.issue_id=NEW.issue_id
       AND head.source_journal_digest=NEW.source_journal_digest
   );
END;

CREATE TRIGGER lifecycle_recovery_issue_revocation_guard
BEFORE INSERT ON agent_lifecycle_recovery_issue_revocations
WHEN EXISTS (
  SELECT 1 FROM lifecycle_fresh_recovery_handoffs handoff
   WHERE handoff.issue_id=NEW.issue_id
)
BEGIN
  SELECT RAISE(ABORT,'LIFECYCLE_RECOVERY_ISSUE_COMMIT_PENDING');
END;
CREATE TRIGGER lifecycle_recovery_handoff_source_guard
BEFORE INSERT ON lifecycle_fresh_recovery_handoffs
WHEN EXISTS (
  SELECT 1 FROM agent_lifecycle_recovery_issue_revocations revocation
   WHERE revocation.issue_id=NEW.issue_id
) OR EXISTS (
  SELECT 1 FROM agent_lifecycle_recovery_capability_issues issue
   WHERE issue.issue_id=NEW.issue_id AND issue.expires_at<=NEW.created_at
)
BEGIN
  SELECT RAISE(ABORT,'LIFECYCLE_RECOVERY_ISSUE_NOT_CURRENT');
END;

CREATE TRIGGER lifecycle_completion_effect_set_exact
BEFORE INSERT ON lifecycle_receipt_batch_completions
BEGIN
  SELECT RAISE(ABORT,'lifecycle-effect-set-incomplete')
   WHERE NOT (
    (NEW.transition_kind='custody-terminal' AND NEW.secondary_intent_kind='none' AND
      NEW.primary_custody_effect_digest IS NOT NULL AND
      NEW.primary_loss_effect_digest IS NULL AND NEW.primary_retirement_effect_digest IS NULL AND
      NEW.primary_fresh_origin_effect_digest IS NULL AND
      (SELECT count(*) FROM lifecycle_receipt_custody_effects WHERE batch_id=NEW.batch_id)=1 AND
      (SELECT count(*) FROM lifecycle_receipt_generation_loss_effects WHERE batch_id=NEW.batch_id)=
        CASE WHEN NEW.linked_loss_effect_digest IS NULL THEN 0 ELSE 1 END AND
      (SELECT count(*) FROM lifecycle_receipt_recovery_retirement_effects WHERE batch_id=NEW.batch_id)=0 AND
      (SELECT count(*) FROM lifecycle_receipt_fresh_origin_effects WHERE batch_id=NEW.batch_id)=0) OR
    (NEW.transition_kind='custody-terminal' AND NEW.secondary_intent_kind='review-adoption-decision' AND
      NEW.primary_custody_effect_digest IS NOT NULL AND NEW.primary_loss_effect_digest IS NULL AND
      NEW.primary_retirement_effect_digest IS NULL AND NEW.primary_fresh_origin_effect_digest IS NULL AND
      (SELECT count(*) FROM lifecycle_receipt_custody_effects WHERE batch_id=NEW.batch_id)=1 AND
      (SELECT count(*) FROM lifecycle_receipt_generation_loss_effects WHERE batch_id=NEW.batch_id)=
        CASE WHEN NEW.linked_loss_effect_digest IS NULL THEN 0 ELSE 1 END AND
      (SELECT count(*) FROM lifecycle_receipt_recovery_retirement_effects WHERE batch_id=NEW.batch_id)=0 AND
      (SELECT count(*) FROM lifecycle_receipt_fresh_origin_effects WHERE batch_id=NEW.batch_id)=0) OR
    (NEW.transition_kind='custody-terminal' AND NEW.secondary_intent_kind='fresh-origin' AND
      NEW.primary_custody_effect_digest IS NOT NULL AND NEW.primary_loss_effect_digest IS NULL AND
      NEW.primary_retirement_effect_digest IS NULL AND NEW.secondary_fresh_origin_effect_role='secondary' AND
      NEW.secondary_fresh_origin_effect_digest IS NOT NULL AND
      (SELECT count(*) FROM lifecycle_receipt_custody_effects WHERE batch_id=NEW.batch_id)=1 AND
      (SELECT count(*) FROM lifecycle_receipt_generation_loss_effects WHERE batch_id=NEW.batch_id)=
        CASE WHEN NEW.linked_loss_effect_digest IS NULL THEN 0 ELSE 1 END AND
      (SELECT count(*) FROM lifecycle_receipt_recovery_retirement_effects WHERE batch_id=NEW.batch_id)=0 AND
      (SELECT count(*) FROM lifecycle_receipt_fresh_origin_effects WHERE batch_id=NEW.batch_id)=1) OR
    (NEW.transition_kind='generation-loss-terminal' AND NEW.secondary_intent_kind='none' AND
      NEW.primary_custody_effect_digest IS NULL AND NEW.primary_loss_effect_role='primary' AND
      NEW.primary_loss_effect_digest IS NOT NULL AND NEW.linked_loss_effect_digest IS NULL AND
      NEW.primary_retirement_effect_digest IS NULL AND NEW.primary_fresh_origin_effect_digest IS NULL AND
      (SELECT count(*) FROM lifecycle_receipt_custody_effects WHERE batch_id=NEW.batch_id)=0 AND
      (SELECT count(*) FROM lifecycle_receipt_generation_loss_effects WHERE batch_id=NEW.batch_id)=1 AND
      (SELECT count(*) FROM lifecycle_receipt_recovery_retirement_effects WHERE batch_id=NEW.batch_id)=0 AND
      (SELECT count(*) FROM lifecycle_receipt_fresh_origin_effects WHERE batch_id=NEW.batch_id)=0) OR
    (NEW.transition_kind='custody-recovery-retirement' AND NEW.secondary_intent_kind='none' AND
      NEW.primary_custody_effect_digest IS NULL AND NEW.primary_loss_effect_digest IS NULL AND
      NEW.primary_retirement_effect_digest IS NOT NULL AND NEW.linked_loss_effect_digest IS NULL AND
      NEW.primary_fresh_origin_effect_digest IS NULL AND
      (SELECT count(*) FROM lifecycle_receipt_custody_effects WHERE batch_id=NEW.batch_id)=0 AND
      (SELECT count(*) FROM lifecycle_receipt_generation_loss_effects WHERE batch_id=NEW.batch_id)=0 AND
      (SELECT count(*) FROM lifecycle_receipt_recovery_retirement_effects WHERE batch_id=NEW.batch_id)=1 AND
      (SELECT count(*) FROM lifecycle_receipt_fresh_origin_effects WHERE batch_id=NEW.batch_id)=0) OR
    (NEW.transition_kind='fresh-origin' AND NEW.secondary_intent_kind='none' AND
      NEW.primary_custody_effect_digest IS NULL AND NEW.primary_loss_effect_digest IS NULL AND
      NEW.primary_retirement_effect_digest IS NULL AND NEW.primary_fresh_origin_effect_role='primary' AND
      NEW.primary_fresh_origin_effect_digest IS NOT NULL AND NEW.linked_loss_effect_digest IS NULL AND
      (SELECT count(*) FROM lifecycle_receipt_custody_effects WHERE batch_id=NEW.batch_id)=0 AND
      (SELECT count(*) FROM lifecycle_receipt_generation_loss_effects WHERE batch_id=NEW.batch_id)=0 AND
      (SELECT count(*) FROM lifecycle_receipt_recovery_retirement_effects WHERE batch_id=NEW.batch_id)=0 AND
      (SELECT count(*) FROM lifecycle_receipt_fresh_origin_effects WHERE batch_id=NEW.batch_id)=1)
   );
END;

CREATE TRIGGER lifecycle_custody_effect_set_closed
BEFORE INSERT ON lifecycle_receipt_custody_effects
WHEN EXISTS (SELECT 1 FROM lifecycle_receipt_batch_completions WHERE batch_id=NEW.batch_id)
BEGIN SELECT RAISE(ABORT,'lifecycle-effect-set-closed'); END;
CREATE TRIGGER lifecycle_loss_effect_set_closed
BEFORE INSERT ON lifecycle_receipt_generation_loss_effects
WHEN EXISTS (SELECT 1 FROM lifecycle_receipt_batch_completions WHERE batch_id=NEW.batch_id)
BEGIN SELECT RAISE(ABORT,'lifecycle-effect-set-closed'); END;
CREATE TRIGGER lifecycle_retirement_effect_set_closed
BEFORE INSERT ON lifecycle_receipt_recovery_retirement_effects
WHEN EXISTS (SELECT 1 FROM lifecycle_receipt_batch_completions WHERE batch_id=NEW.batch_id)
BEGIN SELECT RAISE(ABORT,'lifecycle-effect-set-closed'); END;
CREATE TRIGGER lifecycle_fresh_effect_set_closed
BEFORE INSERT ON lifecycle_receipt_fresh_origin_effects
WHEN EXISTS (SELECT 1 FROM lifecycle_receipt_batch_completions WHERE batch_id=NEW.batch_id)
BEGIN SELECT RAISE(ABORT,'lifecycle-effect-set-closed'); END;

CREATE TRIGGER lifecycle_apply_post_state_complete
BEFORE INSERT ON lifecycle_transition_applies
WHEN NEW.batch_transition_kind='custody-terminal'
BEGIN
  SELECT RAISE(ABORT,'lifecycle-apply-post-state-incomplete')
   WHERE NOT EXISTS (
    SELECT 1
      FROM lifecycle_receipt_batch_completions completion
      JOIN lifecycle_receipt_custody_effects effect
        ON effect.batch_id=completion.batch_id
       AND effect.effect_digest=completion.primary_custody_effect_digest
      JOIN lifecycle_rotation_custody_revisions revision
        ON revision.project_session_id=effect.project_session_id
       AND revision.run_id=effect.run_id
       AND revision.agent_id=effect.agent_id
       AND revision.custody_id=effect.custody_id
       AND revision.revision=effect.final_revision
       AND revision.semantic_digest=effect.final_semantic_digest
       AND revision.source_ref_digest=effect.final_source_ref_digest
       AND revision.receipt_batch_id=effect.batch_id
       AND revision.receipt_apply_id=NEW.apply_id
       AND revision.receipt_apply_digest=NEW.apply_digest
      JOIN lifecycle_rotation_custody_heads head
        ON head.project_session_id=revision.project_session_id
       AND head.run_id=revision.run_id
       AND head.agent_id=revision.agent_id
       AND head.custody_id=revision.custody_id
       AND head.current_revision=revision.revision
       AND head.state=revision.state
       AND head.disposition_code=revision.disposition_code
       AND head.semantic_digest=revision.semantic_digest
       AND head.source_ref_digest=revision.source_ref_digest
       AND head.journal_digest=revision.journal_digest
     WHERE completion.batch_id=NEW.receipt_batch_id
   );
END;

CREATE TRIGGER lifecycle_apply_loss_post_state_required
BEFORE INSERT ON lifecycle_transition_applies
WHEN NEW.batch_transition_kind='generation-loss-terminal'
BEGIN
  SELECT RAISE(ABORT,'lifecycle-apply-post-state-incomplete')
   WHERE NOT EXISTS (
    SELECT 1
      FROM lifecycle_receipt_batch_completions completion
      JOIN lifecycle_receipt_generation_loss_effects effect
        ON effect.batch_id=completion.batch_id
       AND effect.role=completion.primary_loss_effect_role
       AND effect.effect_digest=completion.primary_loss_effect_digest
      JOIN lifecycle_generation_loss_revisions revision
        ON revision.project_session_id=effect.project_session_id
       AND revision.run_id=effect.run_id
       AND revision.agent_id=effect.agent_id
       AND revision.generation_loss_id=effect.generation_loss_id
       AND revision.revision=effect.final_revision
       AND revision.semantic_digest=effect.final_semantic_digest
       AND revision.source_ref_digest=effect.final_source_ref_digest
       AND revision.receipt_batch_id=effect.batch_id
       AND revision.receipt_apply_id=NEW.apply_id
       AND revision.receipt_apply_digest=NEW.apply_digest
      JOIN lifecycle_generation_loss_heads head
        ON head.project_session_id=revision.project_session_id
       AND head.run_id=revision.run_id
       AND head.agent_id=revision.agent_id
       AND head.generation_loss_id=revision.generation_loss_id
       AND head.current_revision=revision.revision
       AND head.state=revision.state
       AND head.abandon_kind_code=revision.abandon_kind_code
       AND head.semantic_digest=revision.semantic_digest
       AND head.source_ref_digest=revision.source_ref_digest
       AND head.journal_digest=revision.journal_digest
     WHERE completion.batch_id=NEW.receipt_batch_id
   );
END;

CREATE TRIGGER lifecycle_apply_retirement_post_state_required
BEFORE INSERT ON lifecycle_transition_applies
WHEN NEW.batch_transition_kind='custody-recovery-retirement'
BEGIN
  SELECT RAISE(ABORT,'lifecycle-apply-post-state-incomplete')
   WHERE NOT EXISTS (
    SELECT 1
      FROM lifecycle_receipt_batch_completions completion
      JOIN lifecycle_receipt_recovery_retirement_effects effect
        ON effect.batch_id=completion.batch_id
       AND effect.effect_digest=completion.primary_retirement_effect_digest
      JOIN agent_lifecycle_recovery_retirements retirement
        ON retirement.retirement_id=effect.retirement_id
       AND retirement.receipt_batch_id=effect.batch_id
       AND retirement.receipt_apply_id=NEW.apply_id
       AND retirement.receipt_apply_digest=NEW.apply_digest
     WHERE completion.batch_id=NEW.receipt_batch_id
   );
END;

CREATE TRIGGER lifecycle_apply_fresh_post_state_required
BEFORE INSERT ON lifecycle_transition_applies
WHEN NEW.apply_kind IN ('terminal-fresh','fresh')
BEGIN
  SELECT RAISE(ABORT,'lifecycle-apply-post-state-incomplete')
   WHERE NOT EXISTS (
    SELECT 1
      FROM lifecycle_receipt_batch_completions completion
      JOIN lifecycle_receipt_fresh_origin_effects effect
        ON effect.batch_id=completion.batch_id
       AND effect.effect_role=NEW.fresh_origin_effect_role
       AND effect.effect_digest=NEW.fresh_origin_effect_digest
      JOIN lifecycle_rotation_custody_revisions revision
        ON revision.project_session_id=effect.project_session_id
       AND revision.run_id=effect.run_id
       AND revision.agent_id=effect.agent_id
       AND revision.custody_id=effect.new_custody_id
       AND revision.revision=effect.new_custody_revision
       AND revision.semantic_digest=effect.new_custody_semantic_digest
       AND revision.source_ref_digest=effect.new_custody_source_ref_digest
       AND revision.origin_fresh_apply_id=NEW.apply_id
       AND revision.origin_fresh_apply_digest=NEW.apply_digest
      JOIN lifecycle_rotation_custody_heads head
        ON head.project_session_id=revision.project_session_id
       AND head.run_id=revision.run_id
       AND head.agent_id=revision.agent_id
       AND head.custody_id=revision.custody_id
       AND head.current_revision=revision.revision
       AND head.state=revision.state
       AND head.disposition_code=revision.disposition_code
       AND head.semantic_digest=revision.semantic_digest
       AND head.source_ref_digest=revision.source_ref_digest
       AND head.journal_digest=revision.journal_digest
     WHERE completion.batch_id=NEW.receipt_batch_id
       AND effect.new_custody_id=NEW.new_custody_id
       AND effect.new_custody_revision=NEW.new_custody_revision
       AND effect.new_custody_semantic_digest=NEW.new_custody_semantic_digest
       AND effect.new_custody_source_ref_digest=NEW.new_custody_source_ref_digest
   );
END;

CREATE TRIGGER lifecycle_apply_linked_loss_post_state_required
BEFORE INSERT ON lifecycle_transition_applies
WHEN EXISTS (
  SELECT 1 FROM lifecycle_receipt_batch_completions completion
   WHERE completion.batch_id=NEW.receipt_batch_id
     AND completion.linked_loss_effect_digest IS NOT NULL
)
BEGIN
  SELECT RAISE(ABORT,'lifecycle-apply-post-state-incomplete')
   WHERE NOT EXISTS (
    SELECT 1
      FROM lifecycle_receipt_batch_completions completion
      JOIN lifecycle_receipt_generation_loss_effects effect
        ON effect.batch_id=completion.batch_id
       AND effect.role=completion.linked_loss_effect_role
       AND effect.effect_digest=completion.linked_loss_effect_digest
      JOIN lifecycle_generation_loss_revisions revision
        ON revision.project_session_id=effect.project_session_id
       AND revision.run_id=effect.run_id
       AND revision.agent_id=effect.agent_id
       AND revision.generation_loss_id=effect.generation_loss_id
       AND revision.revision=effect.final_revision
       AND revision.semantic_digest=effect.final_semantic_digest
       AND revision.source_ref_digest=effect.final_source_ref_digest
       AND revision.receipt_batch_id=effect.batch_id
       AND revision.receipt_apply_id=NEW.apply_id
       AND revision.receipt_apply_digest=NEW.apply_digest
      JOIN lifecycle_generation_loss_heads head
        ON head.project_session_id=revision.project_session_id
       AND head.run_id=revision.run_id
       AND head.agent_id=revision.agent_id
       AND head.generation_loss_id=revision.generation_loss_id
       AND head.current_revision=revision.revision
       AND head.state=revision.state
       AND head.abandon_kind_code=revision.abandon_kind_code
       AND head.semantic_digest=revision.semantic_digest
       AND head.source_ref_digest=revision.source_ref_digest
       AND head.journal_digest=revision.journal_digest
     WHERE completion.batch_id=NEW.receipt_batch_id
   );
END;

CREATE TRIGGER lifecycle_terminal_fresh_commit_required
BEFORE INSERT ON lifecycle_transition_applies
WHEN NEW.apply_kind='terminal-fresh'
BEGIN
  SELECT RAISE(ABORT,'lifecycle-apply-post-state-incomplete')
   WHERE NOT EXISTS (
    SELECT 1 FROM lifecycle_fresh_rotation_commits fresh_commit
     WHERE fresh_commit.handoff_id=NEW.fresh_handoff_id
       AND fresh_commit.handoff_digest=NEW.fresh_handoff_digest
       AND fresh_commit.apply_id=NEW.apply_id
       AND fresh_commit.fresh_apply_digest=NEW.apply_digest
       AND fresh_commit.generation_loss_after_key=NEW.fresh_generation_loss_after_key
       AND fresh_commit.generation_loss_after_id IS NEW.fresh_generation_loss_id
       AND fresh_commit.generation_loss_after_revision IS NEW.fresh_generation_loss_after_revision
       AND fresh_commit.generation_loss_after_semantic_digest IS NEW.fresh_generation_loss_after_semantic_digest
       AND fresh_commit.generation_loss_after_source_ref_digest IS NEW.fresh_generation_loss_after_source_ref_digest
   );
END;

CREATE TABLE mailbox_state (
  run_id TEXT NOT NULL,
  recipient_id TEXT NOT NULL,
  next_sequence INTEGER NOT NULL DEFAULT 1,
  contiguous_watermark INTEGER NOT NULL DEFAULT 0,
  PRIMARY KEY (run_id, recipient_id),
  FOREIGN KEY (run_id, recipient_id) REFERENCES agents(run_id, agent_id)
);

CREATE TABLE message_contexts (
  message_id TEXT PRIMARY KEY REFERENCES messages(message_id),
  context_json TEXT NOT NULL
);

CREATE TABLE messages (
  message_id TEXT PRIMARY KEY,
  run_id TEXT NOT NULL,
  sender_id TEXT NOT NULL,
  dedupe_key TEXT NOT NULL,
  payload_hash TEXT NOT NULL,
  audience_json TEXT NOT NULL,
  kind TEXT NOT NULL,
  body TEXT NOT NULL,
  requires_ack INTEGER NOT NULL,
  conversation_id TEXT NOT NULL,
  reply_to_message_id TEXT,
  task_revision INTEGER,
  hop_count INTEGER NOT NULL DEFAULT 0 CHECK (hop_count >= 0 AND hop_count <= 4),
  expires_at INTEGER,
  created_at INTEGER NOT NULL,
  UNIQUE (run_id, sender_id, dedupe_key)
);

CREATE TABLE notification_attempts (
  notification_id TEXT NOT NULL REFERENCES notification_deliveries(notification_id),
  attempt INTEGER NOT NULL CHECK (attempt >= 1),
  state TEXT NOT NULL,
  detail_json TEXT NOT NULL,
  created_at INTEGER NOT NULL,
  PRIMARY KEY(notification_id, attempt)
);

CREATE TABLE notification_deliveries (
  notification_id TEXT PRIMARY KEY,
  item_id TEXT NOT NULL REFERENCES attention_items(item_id),
  item_revision INTEGER NOT NULL CHECK (item_revision >= 1),
  target_integration TEXT NOT NULL,
  dedupe_key TEXT NOT NULL UNIQUE,
  state TEXT NOT NULL CHECK (state IN ('pending','claimed','sent','failed','deduplicated','ambiguous')),
  claim_generation INTEGER NOT NULL CHECK (claim_generation >= 0),
  claim_deadline INTEGER,
  effect_identity_hash TEXT,
  updated_at INTEGER NOT NULL,
  UNIQUE(item_id, item_revision, target_integration)
);

CREATE TABLE observer_event_sequence (
  sequence INTEGER PRIMARY KEY AUTOINCREMENT,
  event_id TEXT NOT NULL UNIQUE REFERENCES events(event_id)
);

CREATE TABLE operation_admissions(
  operation_id TEXT PRIMARY KEY,
  project_session_id TEXT NOT NULL,
  coordination_run_id TEXT NOT NULL,
  operation_kind TEXT NOT NULL,
  state TEXT NOT NULL CHECK(state IN (
    'prepared','authorised','executing','conflict','ambiguous','quarantined','terminal','cancelled'
  )),
  revision INTEGER NOT NULL CHECK(revision>=1),
  payload_digest TEXT NOT NULL,
  FOREIGN KEY(project_session_id,coordination_run_id) REFERENCES runs(project_session_id,run_id)
);

CREATE TABLE operator_capabilities (
  capability_id TEXT PRIMARY KEY,
  token_hash TEXT NOT NULL UNIQUE,
  operator_id TEXT NOT NULL REFERENCES operator_principals(operator_id),
  project_id TEXT NOT NULL REFERENCES projects(project_id),
  project_session_id TEXT,
  project_authority_generation INTEGER NOT NULL CHECK (project_authority_generation >= 1),
  session_generation INTEGER CHECK (session_generation IS NULL OR session_generation >= 1),
  principal_generation INTEGER NOT NULL CHECK (principal_generation >= 1),
  kind TEXT NOT NULL CHECK (kind IN ('project-launch','session','takeover')),
  operations_json TEXT NOT NULL,
  issued_at INTEGER NOT NULL,
  expires_at INTEGER NOT NULL,
  revoked_at INTEGER,
  handoff_digest TEXT,
  old_chair_generation INTEGER,
  expected_run_id TEXT,
  expected_run_revision INTEGER,
  expected_session_revision INTEGER,
  cas_target_revision INTEGER,
  FOREIGN KEY(project_session_id, project_id) REFERENCES project_sessions(project_session_id, project_id)
);

CREATE TABLE operator_client_attachments (
  attachment_id TEXT PRIMARY KEY,
  operator_id TEXT NOT NULL REFERENCES operator_principals(operator_id),
  project_id TEXT NOT NULL REFERENCES projects(project_id),
  project_authority_generation INTEGER NOT NULL CHECK (project_authority_generation >= 1),
  project_session_id TEXT,
  session_generation INTEGER,
  daemon_instance_generation INTEGER NOT NULL CHECK (daemon_instance_generation >= 1),
  lease_generation INTEGER NOT NULL CHECK (lease_generation >= 1),
  state TEXT NOT NULL CHECK (state IN ('active','detached','expired')),
  expires_at INTEGER NOT NULL,
  revision INTEGER NOT NULL CHECK (revision >= 1),
  created_at INTEGER NOT NULL,
  updated_at INTEGER NOT NULL,
  FOREIGN KEY(project_session_id, project_id) REFERENCES project_sessions(project_session_id, project_id)
);

CREATE TABLE operator_commands (
  operator_id TEXT NOT NULL REFERENCES operator_principals(operator_id),
  command_id TEXT NOT NULL,
  capability_id TEXT NOT NULL REFERENCES operator_capabilities(capability_id),
  project_id TEXT NOT NULL REFERENCES projects(project_id),
  project_session_id TEXT,
  operation TEXT NOT NULL,
  expected_revision INTEGER NOT NULL CHECK (expected_revision >= 0),
  payload_hash TEXT NOT NULL,
  provenance_json TEXT NOT NULL,
  before_json TEXT NOT NULL,
  after_json TEXT NOT NULL,
  evidence_json TEXT NOT NULL,
  result_json TEXT NOT NULL,
  status TEXT NOT NULL CHECK (status IN ('committed','rejected')),
  created_at INTEGER NOT NULL,
  PRIMARY KEY(operator_id, command_id)
);

CREATE TABLE project_session_launch_preparations (
  operator_id TEXT NOT NULL REFERENCES operator_principals(operator_id),
  command_id TEXT NOT NULL,
  capability_id TEXT NOT NULL REFERENCES operator_capabilities(capability_id),
  project_id TEXT NOT NULL REFERENCES projects(project_id),
  project_session_id TEXT NOT NULL,
  session_generation INTEGER NOT NULL CHECK (session_generation >= 1),
  payload_hash TEXT NOT NULL,
  status TEXT NOT NULL CHECK (status IN ('claimed','staged','committed')),
  launch_packet_path TEXT,
  launch_packet_digest TEXT,
  resource_plan_path TEXT,
  resource_plan_digest TEXT,
  staged_launch_packet_path TEXT,
  staged_resource_plan_path TEXT,
  staged_launch_packet_device TEXT,
  staged_launch_packet_inode TEXT,
  staged_resource_plan_device TEXT,
  staged_resource_plan_inode TEXT,
  result_json TEXT,
  created_at INTEGER NOT NULL,
  updated_at INTEGER NOT NULL,
  PRIMARY KEY(operator_id, command_id),
  FOREIGN KEY(project_session_id, project_id)
    REFERENCES project_sessions(project_session_id, project_id)
);

CREATE TABLE operator_control_fences (
  fence_id TEXT PRIMARY KEY,
  project_session_id TEXT NOT NULL,
  coordination_run_id TEXT NOT NULL,
  task_id TEXT NOT NULL,
  scope_kind TEXT NOT NULL CHECK (scope_kind IN ('task','subtree','run','session')),
  target_revision INTEGER NOT NULL CHECK (target_revision >= 1),
  session_generation INTEGER NOT NULL CHECK (session_generation >= 1),
  command_id TEXT NOT NULL,
  state TEXT NOT NULL CHECK (state IN ('paused','released','cancelled')),
  created_at INTEGER NOT NULL,
  released_at INTEGER,
  FOREIGN KEY (project_session_id, coordination_run_id)
    REFERENCES runs(project_session_id, run_id),
  FOREIGN KEY (coordination_run_id, task_id)
    REFERENCES tasks(run_id, task_id),
  CHECK ((state='paused' AND released_at IS NULL) OR (state<>'paused' AND released_at IS NOT NULL))
);

CREATE TABLE operator_daemon_stop_custody(
  daemon_instance_generation INTEGER NOT NULL CHECK(daemon_instance_generation>=1),
  observed_global_revision INTEGER NOT NULL CHECK(observed_global_revision>=1),
  custody_id TEXT PRIMARY KEY REFERENCES operator_effect_custody(custody_id),
  operator_id TEXT NOT NULL,
  project_id TEXT NOT NULL,
  project_session_id TEXT NOT NULL,
  principal_generation INTEGER NOT NULL CHECK(principal_generation>=1),
  command_id TEXT NOT NULL,
  operation TEXT NOT NULL CHECK(operation='daemon-stop'),
  result_correlation_digest TEXT NOT NULL,
  state TEXT NOT NULL CHECK(state IN ('prepared','scheduled','stopped','failed','rejected','no-effect')),
  result_json TEXT,
  updated_at INTEGER NOT NULL,
  UNIQUE(operator_id,project_id,project_session_id,command_id),
  FOREIGN KEY(project_session_id,project_id) REFERENCES project_sessions(project_session_id,project_id)
);

CREATE TABLE operator_effect_custody(
  custody_id TEXT PRIMARY KEY,
  operator_id TEXT NOT NULL,
  project_id TEXT NOT NULL,
  project_session_id TEXT NOT NULL,
  principal_generation INTEGER NOT NULL CHECK(principal_generation>=1),
  command_id TEXT NOT NULL,
  operation TEXT NOT NULL,
  intent_digest TEXT NOT NULL,
  before_state_digest TEXT NOT NULL,
  intent_json TEXT NOT NULL,
  state TEXT NOT NULL CHECK(state IN (
    'prepared','dispatching','conflict','ambiguous','quarantined','terminal','no-effect','rejected','failed'
  )),
  effect_path TEXT,
  effect_digest TEXT,
  outcome_json TEXT,
  created_at INTEGER NOT NULL,
  updated_at INTEGER NOT NULL,
  UNIQUE(operator_id,project_id,project_session_id,command_id),
  FOREIGN KEY(project_session_id,project_id) REFERENCES project_sessions(project_session_id,project_id),
  CHECK((effect_path IS NULL)=(effect_digest IS NULL)),
  CHECK(effect_digest IS NULL OR (length(effect_digest)=71 AND substr(effect_digest,1,7)='sha256:')),
  CHECK(length(intent_digest)=71 AND substr(intent_digest,1,7)='sha256:'),
  CHECK(length(before_state_digest)=71 AND substr(before_state_digest,1,7)='sha256:')
);

CREATE TABLE operator_external_effect_bindings(
  custody_id TEXT PRIMARY KEY REFERENCES operator_effect_custody(custody_id),
  effect_kind TEXT NOT NULL CHECK(effect_kind IN ('registered-external-effect','promotion')),
  integration_id TEXT NOT NULL CHECK(length(integration_id) BETWEEN 1 AND 256),
  integration_generation INTEGER NOT NULL CHECK(integration_generation>=1),
  operation_id TEXT NOT NULL CHECK(length(operation_id) BETWEEN 1 AND 256),
  contract_digest TEXT NOT NULL CHECK(length(contract_digest)=71 AND substr(contract_digest,1,7)='sha256:'),
  target_id TEXT NOT NULL CHECK(length(target_id) BETWEEN 1 AND 512),
  target_revision INTEGER NOT NULL CHECK(target_revision>=1),
  request_artifact_path TEXT NOT NULL CHECK(length(request_artifact_path) BETWEEN 1 AND 4096),
  request_artifact_digest TEXT NOT NULL CHECK(
    length(request_artifact_digest)=71 AND substr(request_artifact_digest,1,7)='sha256:'
  ),
  idempotency_key TEXT NOT NULL CHECK(length(idempotency_key) BETWEEN 1 AND 512),
  release_gate_id TEXT REFERENCES scoped_gates(gate_id),
  release_gate_revision INTEGER CHECK(release_gate_revision IS NULL OR release_gate_revision>=1),
  release_binding_digest TEXT CHECK(
    release_binding_digest IS NULL OR
    (length(release_binding_digest)=71 AND substr(release_binding_digest,1,7)='sha256:')
  ),
  lookup_generation INTEGER NOT NULL DEFAULT 0 CHECK(lookup_generation>=0),
  lookup_evidence_digest TEXT,
  created_at INTEGER NOT NULL,
  UNIQUE(integration_id,idempotency_key),
  CHECK(
    (effect_kind='registered-external-effect'
      AND release_gate_id IS NULL
      AND release_gate_revision IS NULL
      AND release_binding_digest IS NULL) OR
    (effect_kind='promotion'
      AND release_gate_id IS NOT NULL
      AND release_gate_revision IS NOT NULL
      AND release_binding_digest IS NOT NULL)
  ),
  CHECK(
    (lookup_generation=0 AND lookup_evidence_digest IS NULL) OR
    (lookup_generation>0 AND lookup_evidence_digest IS NOT NULL
      AND length(lookup_evidence_digest)=71
      AND substr(lookup_evidence_digest,1,7)='sha256:')
  )
);

CREATE TABLE operator_git_effect_bindings(
  custody_id TEXT PRIMARY KEY REFERENCES operator_effect_custody(custody_id),
  project_id TEXT NOT NULL,
  project_session_id TEXT NOT NULL,
  prepared_session_revision INTEGER NOT NULL CHECK(prepared_session_revision>=1),
  session_generation INTEGER NOT NULL CHECK(session_generation>=1),
  coordination_run_id TEXT NOT NULL,
  prepared_run_revision INTEGER NOT NULL CHECK(prepared_run_revision>=1),
  prepared_dependency_revision INTEGER NOT NULL CHECK(prepared_dependency_revision>=1),
  authority_ref TEXT NOT NULL,
  authority_revision INTEGER NOT NULL CHECK(authority_revision>=1),
  git_allowlist_epoch INTEGER NOT NULL CHECK(git_allowlist_epoch>=1),
  git_allowlist_digest TEXT,
  grant_id TEXT,
  grant_revision INTEGER,
  draft_id TEXT,
  draft_revision INTEGER,
  gate_id TEXT,
  gate_revision INTEGER,
  repository_root TEXT NOT NULL,
  worktree_path TEXT NOT NULL,
  repository_state_digest TEXT NOT NULL,
  execution_profile_id TEXT NOT NULL,
  execution_profile_revision INTEGER NOT NULL CHECK(execution_profile_revision>=1),
  execution_profile_digest TEXT NOT NULL,
  remote_registration_id TEXT,
  remote_registration_revision INTEGER,
  remote_generation INTEGER,
  remote_target_digest TEXT,
  operation_id TEXT NOT NULL UNIQUE,
  operation_variant TEXT NOT NULL,
  effect_binding_digest TEXT NOT NULL,
  result_recipe_digest TEXT NOT NULL,
  decision_digest TEXT NOT NULL,
  before_git_state_json TEXT NOT NULL CHECK(json_valid(before_git_state_json)),
  expected_terminal_state_json TEXT NOT NULL CHECK(json_valid(expected_terminal_state_json)),
  state TEXT NOT NULL CHECK(state IN (
    'prepared','dispatching','conflict','conflict-transferred','ambiguous','quarantined',
    'applied','no-effect','rejected','failed','human-resolved'
  )),
  state_revision INTEGER NOT NULL CHECK(state_revision>=1),
  terminal_basis TEXT CHECK(terminal_basis IS NULL OR terminal_basis IN ('machine-proof','conflict-transfer','human-adjudication')),
  predecessor_custody_id TEXT,
  predecessor_conflict_generation INTEGER,
  owned_conflict_generation INTEGER,
  mutation_reservation_generation INTEGER NOT NULL CHECK(mutation_reservation_generation>=1),
  lock_plan_digest TEXT NOT NULL,
  lookup_generation INTEGER NOT NULL DEFAULT 0 CHECK(lookup_generation>=0),
  lookup_evidence_digest TEXT,
  lookup_outcome TEXT CHECK(lookup_outcome IS NULL OR lookup_outcome IN (
    'exact-conflict','exact-applied','exact-no-effect','incomplete','unavailable','inconsistent',
    'inspector-unavailable','remote-proof-permanently-unavailable','mixed-local-remote-evidence',
    'evidence-integrity-failure','conflict-state-unverifiable'
  )),
  lookup_failure_signature_digest TEXT,
  lookup_observed_at INTEGER,
  resolution_eligible INTEGER NOT NULL DEFAULT 0 CHECK(resolution_eligible IN (0,1)),
  resolution_eligible_lookup_generation INTEGER,
  resolution_eligible_evidence_digest TEXT,
  resolution_eligibility_reason TEXT,
  created_at INTEGER NOT NULL,
  updated_at INTEGER NOT NULL,
  FOREIGN KEY(project_session_id,coordination_run_id) REFERENCES runs(project_session_id,run_id),
  FOREIGN KEY(project_session_id,coordination_run_id,authority_revision,authority_ref,
              git_allowlist_epoch,git_allowlist_digest)
    REFERENCES run_authority_revisions(project_session_id,coordination_run_id,authority_revision,
                                       authority_ref,git_allowlist_epoch,git_allowlist_digest),
  FOREIGN KEY(grant_id,grant_revision) REFERENCES operator_git_grants(grant_id,revision),
  FOREIGN KEY(draft_id,operation_id) REFERENCES git_operation_drafts(draft_id,operation_id),
  FOREIGN KEY(gate_id,operation_id) REFERENCES scoped_gate_operations(gate_id,operation_id),
  FOREIGN KEY(operation_id) REFERENCES operation_admissions(operation_id),
  FOREIGN KEY(execution_profile_id,execution_profile_revision) REFERENCES git_execution_profiles(profile_id,revision),
  FOREIGN KEY(remote_registration_id,remote_registration_revision) REFERENCES git_remote_registrations(registration_id,revision),
  FOREIGN KEY(predecessor_custody_id) REFERENCES operator_git_effect_bindings(custody_id),
  FOREIGN KEY(custody_id,mutation_reservation_generation) REFERENCES git_mutation_reservations(custody_id,generation),
  CHECK((grant_id IS NULL)=(grant_revision IS NULL)),
  CHECK((draft_id IS NULL)=(draft_revision IS NULL)),
  CHECK((gate_id IS NULL)=(gate_revision IS NULL)),
  CHECK((draft_id IS NULL)=(gate_id IS NULL)),
  CHECK((grant_id IS NULL)<>(gate_id IS NULL)),
  CHECK((remote_registration_id IS NULL)=(remote_registration_revision IS NULL)),
  CHECK((remote_registration_id IS NULL)=(remote_generation IS NULL)),
  CHECK((remote_registration_id IS NULL)=(remote_target_digest IS NULL)),
  CHECK((predecessor_custody_id IS NULL)=(predecessor_conflict_generation IS NULL)),
  CHECK((lookup_generation=0)=(lookup_evidence_digest IS NULL)),
  CHECK((lookup_generation=0)=(lookup_outcome IS NULL)),
  CHECK((lookup_generation=0)=(lookup_observed_at IS NULL)),
  CHECK(
    (lookup_outcome IN ('incomplete','unavailable','inconsistent','inspector-unavailable',
      'remote-proof-permanently-unavailable','mixed-local-remote-evidence','evidence-integrity-failure')
      AND lookup_failure_signature_digest IS NOT NULL)
    OR
    ((lookup_outcome IS NULL OR lookup_outcome NOT IN ('incomplete','unavailable','inconsistent','inspector-unavailable',
      'remote-proof-permanently-unavailable','mixed-local-remote-evidence','evidence-integrity-failure'))
      AND lookup_failure_signature_digest IS NULL)
  ),
  CHECK(state<>'conflict' OR owned_conflict_generation IS NOT NULL),
  CHECK((resolution_eligible=0)=(resolution_eligible_lookup_generation IS NULL)),
  CHECK((resolution_eligible=0)=(resolution_eligible_evidence_digest IS NULL)),
  CHECK((resolution_eligible=0)=(resolution_eligibility_reason IS NULL)),
  CHECK(resolution_eligible=0 OR resolution_eligible_lookup_generation=lookup_generation),
  CHECK(resolution_eligible=0 OR resolution_eligible_evidence_digest=lookup_evidence_digest),
  CHECK(resolution_eligible=0 OR resolution_eligibility_reason=lookup_outcome),
  CHECK(resolution_eligible=0 OR state IN ('ambiguous','quarantined')),
  CHECK(resolution_eligibility_reason IS NULL OR resolution_eligibility_reason IN (
    'inspector-unavailable','remote-proof-permanently-unavailable','mixed-local-remote-evidence',
    'evidence-integrity-failure','conflict-state-unverifiable'
  )),
  CHECK(resolution_eligibility_reason<>'conflict-state-unverifiable' OR
        (state='quarantined' AND (owned_conflict_generation IS NOT NULL OR predecessor_conflict_generation IS NOT NULL)))
);

CREATE TABLE operator_git_grant_paths(
  grant_id TEXT NOT NULL,
  grant_revision INTEGER NOT NULL,
  canonical_prefix TEXT NOT NULL,
  PRIMARY KEY(grant_id,grant_revision,canonical_prefix),
  FOREIGN KEY(grant_id,grant_revision) REFERENCES operator_git_grants(grant_id,revision)
);

CREATE TABLE operator_git_grant_refs(
  grant_id TEXT NOT NULL,
  grant_revision INTEGER NOT NULL,
  ref_name TEXT NOT NULL CHECK(substr(ref_name,1,5)='refs/'),
  PRIMARY KEY(grant_id,grant_revision,ref_name),
  FOREIGN KEY(grant_id,grant_revision) REFERENCES operator_git_grants(grant_id,revision)
);

CREATE TABLE operator_git_grant_remotes(
  grant_id TEXT NOT NULL,
  grant_revision INTEGER NOT NULL,
  registration_id TEXT NOT NULL,
  registration_revision INTEGER NOT NULL,
  generation INTEGER NOT NULL,
  target_digest TEXT NOT NULL,
  PRIMARY KEY(grant_id,grant_revision,registration_id,registration_revision),
  FOREIGN KEY(grant_id,grant_revision) REFERENCES operator_git_grants(grant_id,revision),
  FOREIGN KEY(registration_id,registration_revision,generation,target_digest)
    REFERENCES git_remote_registrations(registration_id,revision,generation,target_digest)
);

CREATE TABLE operator_git_grant_variants(
  grant_id TEXT NOT NULL,
  grant_revision INTEGER NOT NULL,
  operation_variant TEXT NOT NULL CHECK(operation_variant IN (
    'fetch','pull-fast-forward-only','stage','unstage','commit','push-fast-forward-only',
    'branch-create','branch-rename','branch-delete-merged-only','worktree-create-detached',
    'worktree-create-new-branch','worktree-create-existing-branch','worktree-move',
    'worktree-remove-clean','upstream-set','upstream-unset'
  )),
  PRIMARY KEY(grant_id,grant_revision,operation_variant),
  FOREIGN KEY(grant_id,grant_revision) REFERENCES operator_git_grants(grant_id,revision)
);

CREATE TABLE operator_git_grants(
  grant_id TEXT NOT NULL,
  revision INTEGER NOT NULL CHECK(revision>=1),
  project_id TEXT NOT NULL,
  project_session_id TEXT NOT NULL,
  session_generation INTEGER NOT NULL CHECK(session_generation>=1),
  issuing_session_revision INTEGER NOT NULL CHECK(issuing_session_revision>=1),
  coordination_run_id TEXT NOT NULL,
  issuing_run_revision INTEGER NOT NULL CHECK(issuing_run_revision>=1),
  issuing_dependency_revision INTEGER NOT NULL CHECK(issuing_dependency_revision>=1),
  authority_ref TEXT NOT NULL,
  authority_revision INTEGER NOT NULL CHECK(authority_revision>=1),
  git_allowlist_epoch INTEGER NOT NULL CHECK(git_allowlist_epoch>=1),
  git_allowlist_digest TEXT NOT NULL,
  repository_root TEXT NOT NULL,
  worktree_path TEXT NOT NULL,
  execution_profile_id TEXT NOT NULL,
  execution_profile_revision INTEGER NOT NULL CHECK(execution_profile_revision>=1),
  execution_profile_digest TEXT NOT NULL,
  allow_worktree_creation INTEGER NOT NULL CHECK(allow_worktree_creation IN (0,1)),
  source_kind TEXT NOT NULL CHECK(source_kind IN ('launch-envelope','operator-command')),
  source_digest TEXT NOT NULL,
  constraints_json TEXT NOT NULL CHECK(json_valid(constraints_json)),
  grant_digest TEXT NOT NULL,
  state TEXT NOT NULL CHECK(state IN ('active','revoked')),
  expires_at INTEGER NOT NULL,
  created_at INTEGER NOT NULL,
  revoked_at INTEGER,
  PRIMARY KEY(grant_id,revision),
  FOREIGN KEY(project_session_id,coordination_run_id) REFERENCES runs(project_session_id,run_id),
  FOREIGN KEY(project_session_id,coordination_run_id,authority_revision,authority_ref,
              git_allowlist_epoch,git_allowlist_digest)
    REFERENCES run_authority_revisions(project_session_id,coordination_run_id,authority_revision,
                                       authority_ref,git_allowlist_epoch,git_allowlist_digest),
  FOREIGN KEY(execution_profile_id,execution_profile_revision)
    REFERENCES git_execution_profiles(profile_id,revision),
  CHECK(length(authority_ref)=71 AND substr(authority_ref,1,7)='sha256:'),
  CHECK(length(git_allowlist_digest)=71 AND substr(git_allowlist_digest,1,7)='sha256:'),
  CHECK(length(execution_profile_digest)=71 AND substr(execution_profile_digest,1,7)='sha256:'),
  CHECK(length(source_digest)=71 AND substr(source_digest,1,7)='sha256:'),
  CHECK(length(grant_digest)=71 AND substr(grant_digest,1,7)='sha256:'),
  CHECK((state='active' AND revoked_at IS NULL) OR (state='revoked' AND revoked_at IS NOT NULL))
);

CREATE TABLE operator_input_attestations (
  attestation_id TEXT PRIMARY KEY,
  integration_id TEXT NOT NULL,
  integration_generation INTEGER NOT NULL CHECK (integration_generation >= 1),
  operator_id TEXT NOT NULL REFERENCES operator_principals(operator_id),
  project_id TEXT NOT NULL REFERENCES projects(project_id),
  project_session_id TEXT NOT NULL,
  coordination_run_id TEXT NOT NULL,
  gate_id TEXT NOT NULL,
  provider_message_id TEXT NOT NULL,
  exact_utterance TEXT NOT NULL,
  provider_event_json TEXT NOT NULL,
  expected_gate_revision INTEGER NOT NULL CHECK (expected_gate_revision >= 1),
  artifact_digests_json TEXT NOT NULL,
  interpreted_decision TEXT NOT NULL,
  recorded_at INTEGER NOT NULL,
  UNIQUE(project_session_id, provider_message_id),
  FOREIGN KEY(project_session_id, coordination_run_id) REFERENCES runs(project_session_id, run_id)
);

CREATE TABLE agent_lifecycle_recovery_custody (
  recovery_id TEXT PRIMARY KEY,
  operator_id TEXT NOT NULL REFERENCES operator_principals(operator_id),
  operator_command_id TEXT NOT NULL,
  path TEXT NOT NULL CHECK (path IN ('abandon')),
  project_session_id TEXT NOT NULL,
  coordination_run_id TEXT NOT NULL,
  agent_id TEXT NOT NULL,
  generation_loss_id TEXT NOT NULL,
  apply_id TEXT NOT NULL,
  expected_source_revision INTEGER NOT NULL CHECK (expected_source_revision >= 1),
  gate_id TEXT NOT NULL REFERENCES scoped_gates(gate_id),
  expected_gate_revision INTEGER NOT NULL CHECK (expected_gate_revision >= 1),
  direct_input_attestation_id TEXT NOT NULL REFERENCES operator_input_attestations(attestation_id),
  destructive_confirmation_digest TEXT NOT NULL,
  operator_decision_digest TEXT NOT NULL,
  terminal_evidence_digest TEXT NOT NULL,
  intent_digest TEXT NOT NULL,
  intent_json TEXT NOT NULL CHECK (json_valid(intent_json)=1),
  state TEXT NOT NULL CHECK (state IN ('prepared','ambiguous','terminal')),
  result_json TEXT,
  revision INTEGER NOT NULL CHECK (revision >= 1),
  created_at INTEGER NOT NULL,
  updated_at INTEGER NOT NULL,
  UNIQUE (operator_id, operator_command_id),
  UNIQUE (apply_id),
  FOREIGN KEY (coordination_run_id, agent_id, generation_loss_id)
    REFERENCES lifecycle_generation_loss_heads(run_id, agent_id, generation_loss_id),
  CHECK (length(destructive_confirmation_digest)=71 AND substr(destructive_confirmation_digest,1,7)='sha256:'),
  CHECK (length(operator_decision_digest)=71 AND substr(operator_decision_digest,1,7)='sha256:'),
  CHECK (length(terminal_evidence_digest)=71 AND substr(terminal_evidence_digest,1,7)='sha256:'),
  CHECK (length(intent_digest)=71 AND substr(intent_digest,1,7)='sha256:'),
  CHECK ((state='terminal')=(result_json IS NOT NULL))
);

CREATE TABLE operator_interventions (
  intervention_id TEXT PRIMARY KEY,
  run_id TEXT NOT NULL REFERENCES runs(run_id),
  actor_agent_id TEXT NOT NULL,
  source TEXT NOT NULL,
  direct_input_provenance TEXT NOT NULL,
  task_revision INTEGER NOT NULL,
  summary TEXT NOT NULL,
  created_at INTEGER NOT NULL
);

CREATE TABLE operator_lifecycle_receipts (
  relative_path TEXT NOT NULL,
  sha256 TEXT NOT NULL,
  kind TEXT NOT NULL CHECK (kind IN ('project-session-drain','daemon-drain')),
  operator_id TEXT NOT NULL,
  project_id TEXT NOT NULL,
  authority_session_id TEXT NOT NULL,
  project_session_id TEXT,
  daemon_instance_generation INTEGER,
  session_revision INTEGER,
  session_generation INTEGER,
  global_state_revision INTEGER NOT NULL CHECK (global_state_revision >= 1),
  command_id TEXT NOT NULL,
  receipt_json TEXT NOT NULL,
  created_at INTEGER NOT NULL,
  PRIMARY KEY (relative_path, sha256),
  UNIQUE (kind, operator_id, project_id, authority_session_id, command_id),
  FOREIGN KEY (project_session_id) REFERENCES project_sessions(project_session_id),
  CHECK (length(sha256)=71 AND substr(sha256,1,7)='sha256:'),
  CHECK (
    (kind='project-session-drain' AND project_session_id IS NOT NULL
      AND daemon_instance_generation IS NULL
      AND session_revision IS NOT NULL AND session_revision >= 1
      AND session_generation IS NOT NULL AND session_generation >= 1) OR
    (kind='daemon-drain' AND project_session_id IS NULL
      AND daemon_instance_generation IS NOT NULL AND daemon_instance_generation >= 1
      AND session_revision IS NULL AND session_generation IS NULL)
  )
);

CREATE TABLE operator_previews (
  preview_id TEXT PRIMARY KEY,
  operator_id TEXT NOT NULL REFERENCES operator_principals(operator_id),
  project_session_id TEXT NOT NULL REFERENCES project_sessions(project_session_id),
  operation TEXT NOT NULL,
  payload_digest TEXT NOT NULL,
  preview_json TEXT NOT NULL,
  revision INTEGER NOT NULL CHECK (revision >= 1),
  expires_at INTEGER NOT NULL,
  confirmed_command_id TEXT UNIQUE,
  created_at INTEGER NOT NULL
);

CREATE TABLE operator_principals (
  operator_id TEXT PRIMARY KEY,
  project_id TEXT NOT NULL REFERENCES projects(project_id),
  project_session_id TEXT,
  authenticated_subject_hash TEXT NOT NULL,
  project_authority_generation INTEGER NOT NULL CHECK (project_authority_generation >= 1),
  principal_generation INTEGER NOT NULL CHECK (principal_generation >= 1),
  state TEXT NOT NULL CHECK (state IN ('active','revoked','expired')),
  created_at INTEGER NOT NULL,
  updated_at INTEGER NOT NULL,
  FOREIGN KEY(project_session_id, project_id) REFERENCES project_sessions(project_session_id, project_id)
);

CREATE TABLE operator_projection_cursors (
  project_session_id TEXT NOT NULL REFERENCES project_sessions(project_session_id),
  schema_version INTEGER NOT NULL CHECK (schema_version >= 1),
  projection_name TEXT NOT NULL,
  cursor INTEGER NOT NULL CHECK (cursor >= 0),
  snapshot_digest TEXT,
  updated_at INTEGER NOT NULL,
  PRIMARY KEY(project_session_id, schema_version, projection_name)
);

CREATE TABLE project_session_launch_custody (
  project_session_id TEXT NOT NULL,
  custody_attempt_generation INTEGER NOT NULL CHECK (custody_attempt_generation >= 1),
  coordination_run_id TEXT NOT NULL UNIQUE,
  chair_agent_id TEXT NOT NULL,
  chair_lease_id TEXT NOT NULL UNIQUE,
  operator_id TEXT NOT NULL,
  operator_command_id TEXT NOT NULL,
  provider_adapter_id TEXT NOT NULL,
  provider_action_id TEXT NOT NULL,
  capability_hash TEXT NOT NULL UNIQUE,
  capability_expires_at INTEGER NOT NULL,
  reservation_id TEXT NOT NULL UNIQUE,
  launch_packet_path TEXT NOT NULL,
  launch_packet_digest TEXT NOT NULL,
  authority_ref TEXT NOT NULL,
  budget_ref TEXT NOT NULL,
  resource_plan_path TEXT NOT NULL,
  resource_plan_digest TEXT NOT NULL,
  expected_project_revision INTEGER NOT NULL CHECK (expected_project_revision >= 1),
  expected_session_revision INTEGER NOT NULL CHECK (expected_session_revision >= 1),
  expected_session_generation INTEGER NOT NULL CHECK (expected_session_generation >= 1),
  trust_record_digest TEXT NOT NULL,
  provider_contract_digest TEXT NOT NULL,
  resource_state_digest TEXT NOT NULL,
  launch_binding_digest TEXT NOT NULL,
  retry_of_provider_adapter_id TEXT,
  retry_of_provider_action_id TEXT,
  created_at INTEGER NOT NULL, attestation_challenge_digest TEXT,
  PRIMARY KEY (project_session_id, custody_attempt_generation),
  UNIQUE (operator_id, operator_command_id),
  UNIQUE (provider_adapter_id, provider_action_id),
  FOREIGN KEY (project_session_id, coordination_run_id)
    REFERENCES runs(project_session_id, run_id),
  FOREIGN KEY (coordination_run_id, chair_agent_id)
    REFERENCES agents(run_id, agent_id),
  FOREIGN KEY (chair_lease_id) REFERENCES run_chair_leases(lease_id),
  FOREIGN KEY (operator_id, operator_command_id)
    REFERENCES operator_commands(operator_id, command_id),
  FOREIGN KEY (capability_hash) REFERENCES capabilities(token_hash),
  FOREIGN KEY (reservation_id) REFERENCES resource_reservations(reservation_id),
  FOREIGN KEY (coordination_run_id, provider_adapter_id, provider_action_id)
    REFERENCES provider_actions(run_id, adapter_id, action_id),
  FOREIGN KEY (retry_of_provider_adapter_id, retry_of_provider_action_id)
    REFERENCES provider_actions(adapter_id, action_id),
  CHECK (
    (retry_of_provider_adapter_id IS NULL AND retry_of_provider_action_id IS NULL) OR
    (retry_of_provider_adapter_id IS NOT NULL AND retry_of_provider_action_id IS NOT NULL)
  ),
  CHECK (length(launch_packet_digest)=71 AND substr(launch_packet_digest,1,7)='sha256:'),
  CHECK (length(authority_ref)=71 AND substr(authority_ref,1,7)='sha256:'),
  CHECK (length(resource_plan_digest)=71 AND substr(resource_plan_digest,1,7)='sha256:'),
  CHECK (length(trust_record_digest)=71 AND substr(trust_record_digest,1,7)='sha256:'),
  CHECK (length(provider_contract_digest)=71 AND substr(provider_contract_digest,1,7)='sha256:'),
  CHECK (length(resource_state_digest)=71 AND substr(resource_state_digest,1,7)='sha256:'),
  CHECK (length(launch_binding_digest)=71 AND substr(launch_binding_digest,1,7)='sha256:')
);

CREATE TABLE project_session_memberships (
  project_session_id TEXT NOT NULL,
  coordination_run_id TEXT NOT NULL,
  member_kind TEXT NOT NULL CHECK (member_kind IN (
    'coordination-run','workstream','task','lease','provider-action','required-message',
    'artifact-obligation','gate','scoped-barrier'
  )),
  member_id TEXT NOT NULL,
  member_adapter_id TEXT NOT NULL DEFAULT '',
  required INTEGER NOT NULL CHECK (required IN (0,1)),
  state TEXT NOT NULL CHECK (state IN ('active','reconciled','abandoned')),
  revision INTEGER NOT NULL CHECK (revision >= 1),
  abandoned_reason TEXT,
  created_at INTEGER NOT NULL,
  updated_at INTEGER NOT NULL,
  PRIMARY KEY(project_session_id, coordination_run_id, member_kind, member_id, member_adapter_id),
  CHECK (
    (member_kind='provider-action' AND length(member_adapter_id)>0) OR
    (member_kind<>'provider-action' AND member_adapter_id='')
  ),
  FOREIGN KEY(project_session_id, coordination_run_id) REFERENCES runs(project_session_id, run_id)
);

CREATE TABLE project_sessions (
  project_session_id TEXT PRIMARY KEY,
  project_id TEXT NOT NULL REFERENCES projects(project_id),
  mode TEXT NOT NULL CHECK (mode IN ('coordinated','independent')),
  state TEXT NOT NULL CHECK (state IN (
    'draft','awaiting_launch','launching','active','quiescing','awaiting_acceptance','closed',
    'launch_failed','launch_ambiguous','reconciling','visibility_degraded','recovery_required',
    'quarantined','cancelled'
  )),
  revision INTEGER NOT NULL CHECK (revision >= 1),
  generation INTEGER NOT NULL CHECK (generation >= 1),
  authority_ref TEXT NOT NULL,
  budget_ref TEXT NOT NULL,
  launch_packet_path TEXT NOT NULL,
  launch_packet_digest TEXT NOT NULL,
  membership_revision INTEGER NOT NULL CHECK (membership_revision >= 1),
  origin_kind TEXT NOT NULL CHECK (origin_kind='operator-launch'),
  origin_operator_id TEXT NOT NULL,
  terminal_path_json TEXT,
  created_at INTEGER NOT NULL,
  updated_at INTEGER NOT NULL,
  UNIQUE(project_session_id, project_id)
);

CREATE TABLE projects (
  project_id TEXT PRIMARY KEY,
  canonical_root TEXT NOT NULL UNIQUE,
  trust_record_digest TEXT,
  revision INTEGER NOT NULL CHECK (revision >= 1),
  authority_generation INTEGER NOT NULL CHECK (authority_generation >= 1),
  created_at INTEGER NOT NULL,
  updated_at INTEGER NOT NULL
);

CREATE TABLE provider_actions (
  run_id TEXT NOT NULL,
  action_id TEXT NOT NULL,
  adapter_id TEXT NOT NULL,
  operation TEXT NOT NULL,
  target_agent_id TEXT,
  provider_session_generation INTEGER,
  turn_lease_generation INTEGER,
  identity_hash TEXT NOT NULL,
  payload_hash TEXT NOT NULL,
  payload_json TEXT NOT NULL,
  status TEXT NOT NULL,
  history_json TEXT NOT NULL,
  execution_count INTEGER NOT NULL,
  effect_count INTEGER NOT NULL,
  idempotency_proven INTEGER NOT NULL DEFAULT 0,
  result_json TEXT,
  updated_at INTEGER NOT NULL,
  journal_revision INTEGER NOT NULL DEFAULT 1 CHECK (journal_revision >= 1),
  task_id TEXT,
  budget_authority_id TEXT REFERENCES authorities(authority_id),
  budget_reservation_json TEXT CHECK (budget_reservation_json IS NULL OR json_valid(budget_reservation_json)),
  budget_settlement_json TEXT CHECK (budget_settlement_json IS NULL OR json_valid(budget_settlement_json)),
  budget_state TEXT CHECK (budget_state IS NULL OR budget_state IN ('reserved','settled','usage-unknown')),
  budget_started_at INTEGER CHECK (budget_started_at IS NULL OR budget_started_at >= 0),
  finding_capacity_reservation_digest TEXT CHECK (
    finding_capacity_reservation_digest IS NULL OR (
      length(finding_capacity_reservation_digest) = 71 AND
      substr(finding_capacity_reservation_digest, 1, 7) = 'sha256:' AND
      substr(finding_capacity_reservation_digest, 8) NOT GLOB '*[^0-9a-f]*'
    )
  ),
  PRIMARY KEY (adapter_id, action_id),
  UNIQUE (run_id, adapter_id, action_id),
  CHECK (
    (task_id IS NULL AND budget_authority_id IS NULL AND budget_reservation_json IS NULL
      AND budget_settlement_json IS NULL AND budget_state IS NULL AND budget_started_at IS NULL) OR
    (task_id IS NOT NULL AND budget_authority_id IS NOT NULL AND budget_reservation_json IS NOT NULL
      AND budget_state IS NOT NULL AND budget_started_at IS NOT NULL
      AND operation='spawn' AND target_agent_id IS NULL)
  ),
  CHECK (budget_state<>'reserved' OR budget_settlement_json IS NULL),
  CHECK (budget_state NOT IN ('settled','usage-unknown') OR budget_settlement_json IS NOT NULL),
  CHECK (budget_state<>'settled' OR status='terminal'),
  CHECK (budget_state<>'usage-unknown' OR status IN ('ambiguous','terminal','quarantined')),
  CHECK (budget_state IS NULL OR status NOT IN ('terminal','quarantined') OR budget_state IN ('settled','usage-unknown')),
  CHECK (budget_state IS NULL OR status<>'quarantined' OR budget_state='usage-unknown'),
  FOREIGN KEY (run_id, task_id) REFERENCES tasks(run_id, task_id),
  FOREIGN KEY (run_id, adapter_id, action_id)
    REFERENCES provider_action_pair_preflights(run_id, adapter_id, action_id),
  FOREIGN KEY (run_id, adapter_id, action_id, finding_capacity_reservation_digest)
    REFERENCES review_finding_capacity_reservations(
      run_id, adapter_id, action_id, reservation_digest
    )
);

CREATE TABLE provider_agent_custody (
  run_id TEXT NOT NULL,
  action_id TEXT NOT NULL,
  operation TEXT NOT NULL CHECK (operation IN ('spawn','attach')),
  actor_agent_id TEXT NOT NULL,
  target_agent_id TEXT NOT NULL,
  authority_id TEXT NOT NULL,
  adapter_id TEXT NOT NULL,
  bridge_contract_digest TEXT NOT NULL,
  bridge_capable INTEGER NOT NULL CHECK (bridge_capable IN (0,1)),
  capability_hash TEXT UNIQUE,
  capability_expires_at INTEGER,
  principal_generation INTEGER CHECK (principal_generation IS NULL OR principal_generation >= 1),
  requested_provider_session_ref TEXT,
  intent_digest TEXT NOT NULL,
  created_at INTEGER NOT NULL,
  PRIMARY KEY (adapter_id, action_id),
  UNIQUE (run_id, adapter_id, action_id),
  FOREIGN KEY (run_id, adapter_id, action_id)
    REFERENCES provider_actions(run_id, adapter_id, action_id),
  FOREIGN KEY (run_id, actor_agent_id) REFERENCES agents(run_id, agent_id),
  FOREIGN KEY (run_id, target_agent_id) REFERENCES agents(run_id, agent_id),
  FOREIGN KEY (authority_id) REFERENCES authorities(authority_id),
  FOREIGN KEY (capability_hash) REFERENCES capabilities(token_hash),
  CHECK (length(bridge_contract_digest)=71 AND substr(bridge_contract_digest,1,7)='sha256:'),
  CHECK (length(intent_digest)=71 AND substr(intent_digest,1,7)='sha256:'),
  CHECK (operation <> 'spawn' OR bridge_capable=1),
  CHECK (
    (bridge_capable=1 AND capability_hash IS NOT NULL
      AND capability_expires_at IS NOT NULL AND principal_generation IS NOT NULL) OR
    (bridge_capable=0 AND capability_hash IS NULL
      AND capability_expires_at IS NULL AND principal_generation IS NULL)
  )
);

CREATE TABLE provider_lifecycle_intents (
  run_id TEXT NOT NULL REFERENCES runs(run_id),
  action_id TEXT NOT NULL,
  operation TEXT NOT NULL CHECK (operation IN ('spawn', 'attach')),
  actor_agent_id TEXT NOT NULL,
  target_agent_id TEXT NOT NULL,
  authority_id TEXT NOT NULL REFERENCES authorities(authority_id),
  adapter_id TEXT NOT NULL,
  requested_resume_reference TEXT,
  intent_hash TEXT NOT NULL,
  status TEXT NOT NULL CHECK (status IN ('prepared', 'provider-terminal', 'finalized', 'quarantined')),
  provider_resume_reference TEXT,
  created_at INTEGER NOT NULL,
  updated_at INTEGER NOT NULL,
  PRIMARY KEY (adapter_id, action_id),
  UNIQUE (run_id, adapter_id, action_id),
  FOREIGN KEY (run_id, adapter_id, action_id)
    REFERENCES provider_actions(run_id, adapter_id, action_id)
);

CREATE TABLE provider_session_turn_leases (
  run_id TEXT NOT NULL REFERENCES runs(run_id),
  agent_id TEXT NOT NULL,
  provider_session_generation INTEGER NOT NULL CHECK (provider_session_generation >= 1),
  turn_lease_generation INTEGER NOT NULL CHECK (turn_lease_generation >= 1),
  adapter_id TEXT NOT NULL,
  action_id TEXT NOT NULL,
  status TEXT NOT NULL CHECK (status IN ('active', 'quarantined', 'released')),
  created_at INTEGER NOT NULL,
  updated_at INTEGER NOT NULL,
  PRIMARY KEY (run_id, agent_id, turn_lease_generation),
  UNIQUE (adapter_id, action_id),
  FOREIGN KEY (run_id, agent_id) REFERENCES agents(run_id, agent_id),
  FOREIGN KEY (run_id, adapter_id, action_id)
    REFERENCES provider_actions(run_id, adapter_id, action_id)
);

CREATE TABLE operator_control_provider_action_bindings (
  custody_id TEXT NOT NULL REFERENCES operator_effect_custody(custody_id),
  run_id TEXT NOT NULL,
  adapter_id TEXT NOT NULL,
  action_id TEXT NOT NULL,
  source_adapter_id TEXT NOT NULL,
  source_action_id TEXT NOT NULL,
  source_payload_hash TEXT NOT NULL,
  operation TEXT NOT NULL CHECK (operation IN ('interrupt','steer')),
  target_agent_id TEXT NOT NULL,
  provider_session_ref TEXT NOT NULL,
  provider_session_generation INTEGER NOT NULL CHECK (provider_session_generation >= 1),
  turn_lease_generation INTEGER NOT NULL CHECK (turn_lease_generation >= 1),
  turn_id TEXT NOT NULL,
  created_at INTEGER NOT NULL,
  PRIMARY KEY (adapter_id, action_id),
  UNIQUE (run_id, adapter_id, action_id),
  FOREIGN KEY (run_id, adapter_id, action_id)
    REFERENCES provider_actions(run_id, adapter_id, action_id),
  FOREIGN KEY (run_id, source_adapter_id, source_action_id)
    REFERENCES provider_actions(run_id, adapter_id, action_id)
);

CREATE TRIGGER operator_control_provider_action_bindings_exact_identity
BEFORE INSERT ON operator_control_provider_action_bindings
WHEN NOT EXISTS (
  SELECT 1
    FROM operator_effect_custody custody
    JOIN runs run
      ON run.run_id=NEW.run_id
     AND run.project_session_id=custody.project_session_id
    JOIN provider_actions action
      ON action.run_id=NEW.run_id
     AND action.adapter_id=NEW.adapter_id
     AND action.action_id=NEW.action_id
    JOIN provider_actions source
      ON source.run_id=NEW.run_id
     AND source.adapter_id=NEW.source_adapter_id
     AND source.action_id=NEW.source_action_id
    JOIN provider_session_turn_leases lease
      ON lease.run_id=NEW.run_id
     AND lease.agent_id=NEW.target_agent_id
     AND lease.adapter_id=NEW.source_adapter_id
     AND lease.action_id=NEW.source_action_id
     AND lease.provider_session_generation=NEW.provider_session_generation
     AND lease.turn_lease_generation=NEW.turn_lease_generation
    JOIN agents agent
      ON agent.run_id=NEW.run_id AND agent.agent_id=NEW.target_agent_id
   WHERE custody.custody_id=NEW.custody_id
     AND json_extract(custody.intent_json,'$.kind')='control'
     AND json_extract(custody.intent_json,'$.action')=custody.operation
     AND (
       (NEW.operation='interrupt' AND custody.operation IN ('pause','cancel')) OR
       (NEW.operation='steer' AND custody.operation='steer')
     )
     AND action.operation=NEW.operation
     AND action.target_agent_id=NEW.target_agent_id
     AND action.provider_session_generation=NEW.provider_session_generation
     AND action.turn_lease_generation=NEW.turn_lease_generation
     AND source.payload_hash=NEW.source_payload_hash
     AND agent.provider_session_ref=NEW.provider_session_ref
     AND json_extract(source.result_json,'$.turnId')=NEW.turn_id
)
BEGIN SELECT RAISE(ABORT, 'INVARIANT_operator_control_provider_action_binding_identity'); END;

CREATE TRIGGER operator_control_provider_action_bindings_immutable_update
BEFORE UPDATE ON operator_control_provider_action_bindings
BEGIN SELECT RAISE(ABORT, 'INVARIANT_operator_control_provider_action_binding_immutable'); END;

CREATE TRIGGER operator_control_provider_action_bindings_immutable_delete
BEFORE DELETE ON operator_control_provider_action_bindings
BEGIN SELECT RAISE(ABORT, 'INVARIANT_operator_control_provider_action_binding_immutable'); END;

CREATE TABLE provider_state (
  run_id TEXT NOT NULL,
  agent_id TEXT NOT NULL,
  provider_session_generation INTEGER NOT NULL DEFAULT 1,
  context_revision TEXT,
  reconciled_checkpoint_sha256 TEXT,
  PRIMARY KEY (run_id, agent_id),
  FOREIGN KEY (run_id, agent_id) REFERENCES agents(run_id, agent_id)
);

CREATE TABLE receipt_exports (
  run_id TEXT NOT NULL REFERENCES runs(run_id),
  relative_path TEXT NOT NULL,
  sha256 TEXT NOT NULL,
  exported_at INTEGER NOT NULL,
  PRIMARY KEY (run_id, relative_path, sha256)
);

CREATE TABLE resource_dimensions (
  scope_id TEXT NOT NULL REFERENCES resource_scopes(scope_id),
  unit_key TEXT NOT NULL,
  limit_value INTEGER NOT NULL CHECK (limit_value >= 0),
  used INTEGER NOT NULL CHECK (used >= 0),
  reserved INTEGER NOT NULL CHECK (reserved >= 0),
  usage_unknown INTEGER NOT NULL CHECK (usage_unknown IN (0,1)),
  PRIMARY KEY(scope_id, unit_key),
  CHECK (used + reserved <= limit_value)
);

CREATE TABLE resource_reservation_dimensions (
  reservation_id TEXT NOT NULL REFERENCES resource_reservations(reservation_id),
  scope_id TEXT NOT NULL REFERENCES resource_scopes(scope_id),
  unit_key TEXT NOT NULL,
  amount INTEGER NOT NULL CHECK (amount >= 0),
  consumed INTEGER NOT NULL CHECK (consumed >= 0),
  released INTEGER NOT NULL CHECK (released >= 0),
  usage_unknown INTEGER NOT NULL CHECK (usage_unknown IN (0,1)),
  PRIMARY KEY(reservation_id, scope_id, unit_key),
  CHECK (consumed + released <= amount)
);

CREATE TABLE resource_reservations (
  reservation_id TEXT PRIMARY KEY,
  project_session_id TEXT NOT NULL REFERENCES project_sessions(project_session_id),
  coordination_run_id TEXT,
  leaf_scope_id TEXT NOT NULL REFERENCES resource_scopes(scope_id),
  operation_id TEXT,
  actor_agent_id TEXT NOT NULL,
  state TEXT NOT NULL CHECK (state IN ('reserved','partially-consumed','consumed','released','ambiguous','reconciled')),
  revision INTEGER NOT NULL CHECK (revision >= 1),
  generation INTEGER NOT NULL CHECK (generation >= 1),
  identity_hash TEXT NOT NULL UNIQUE,
  path_json TEXT NOT NULL,
  amounts_json TEXT NOT NULL,
  created_at INTEGER NOT NULL,
  updated_at INTEGER NOT NULL
);

CREATE TABLE resource_scopes (
  scope_id TEXT PRIMARY KEY,
  project_id TEXT NOT NULL REFERENCES projects(project_id),
  project_session_id TEXT,
  coordination_run_id TEXT,
  parent_scope_id TEXT REFERENCES resource_scopes(scope_id),
  scope_kind TEXT NOT NULL CHECK (scope_kind IN ('project','project-session','coordination-run','team','agent')),
  owner_ref TEXT NOT NULL,
  state TEXT NOT NULL CHECK (state IN ('active','usage-unknown','released')),
  revision INTEGER NOT NULL CHECK (revision >= 1),
  UNIQUE(project_id, scope_kind, owner_ref),
  FOREIGN KEY(project_session_id, coordination_run_id) REFERENCES runs(project_session_id, run_id)
);

CREATE TABLE result_deadline_sweep_state(
  singleton INTEGER PRIMARY KEY CHECK(singleton=1),
  daemon_instance_generation INTEGER NOT NULL CHECK(daemon_instance_generation>=1),
  pass_generation INTEGER NOT NULL CHECK(pass_generation>=1),
  result_json TEXT NOT NULL,
  completed_at INTEGER NOT NULL,
  FOREIGN KEY(daemon_instance_generation)
    REFERENCES daemon_runtime_epochs(instance_generation)
);

CREATE TABLE result_deliveries (
  result_delivery_id TEXT PRIMARY KEY,
  callback_id TEXT NOT NULL UNIQUE,
  request_id TEXT NOT NULL UNIQUE REFERENCES task_requests(request_id),
  result_id TEXT NOT NULL UNIQUE REFERENCES task_results(result_id),
  project_session_id TEXT NOT NULL,
  run_id TEXT NOT NULL,
  task_id TEXT NOT NULL,
  requester_agent_id TEXT NOT NULL,
  target_provider_session TEXT NOT NULL,
  state TEXT NOT NULL CHECK (state IN ('pending','claimed','provider-accepted','consumed','overdue','abandoned')),
  required INTEGER NOT NULL CHECK (required IN (0,1)),
  revision INTEGER NOT NULL CHECK (revision >= 1),
  claim_generation INTEGER NOT NULL CHECK (claim_generation >= 0),
  assignment_generation INTEGER NOT NULL CHECK (assignment_generation >= 1),
  claimed_by TEXT,
  claim_deadline INTEGER,
  response_deadline INTEGER NOT NULL,
  provider_adapter_id TEXT,
  provider_action_id TEXT,
  request_revision INTEGER NOT NULL CHECK (request_revision >= 1),
  reply_revision INTEGER NOT NULL CHECK (reply_revision >= 1),
  task_revision INTEGER NOT NULL CHECK (task_revision >= 1),
  payload_digest TEXT NOT NULL,
  retry_of_callback_id TEXT,
  reassignment_of_callback_id TEXT,
  abandoned_reason TEXT,
  provider_accepted_at INTEGER,
  consumed_at INTEGER,
  overdue_at INTEGER,
  abandoned_at INTEGER,
  updated_at INTEGER NOT NULL,
  FOREIGN KEY(project_session_id, run_id) REFERENCES runs(project_session_id, run_id),
  FOREIGN KEY(run_id, task_id) REFERENCES tasks(run_id, task_id),
  FOREIGN KEY(run_id, requester_agent_id) REFERENCES agents(run_id, agent_id),
  FOREIGN KEY(run_id, provider_adapter_id, provider_action_id)
    REFERENCES provider_actions(run_id, adapter_id, action_id),
  CHECK ((provider_adapter_id IS NULL) = (provider_action_id IS NULL))
);
CREATE TRIGGER result_delivery_lifecycle_receipt_owner_guard
BEFORE UPDATE ON result_deliveries
WHEN EXISTS (
  SELECT 1
    FROM lifecycle_custody_adoption_deliveries ownership
    JOIN lifecycle_rotation_custody_heads head
      ON head.run_id=ownership.run_id AND head.agent_id=ownership.agent_id
     AND head.custody_id=ownership.custody_id
    JOIN lifecycle_receipt_custody_effects effect
      ON effect.run_id=ownership.run_id AND effect.agent_id=ownership.agent_id
     AND effect.custody_id=ownership.custody_id
   WHERE ownership.delivery_id=OLD.result_delivery_id
     AND ownership.active_owner=1 AND head.terminal=0
)
BEGIN SELECT RAISE(ABORT,'INVARIANT_result_delivery_lifecycle_receipt_owner'); END;
CREATE TRIGGER result_delivery_lifecycle_receipt_owner_delete_guard
BEFORE DELETE ON result_deliveries
WHEN EXISTS (
  SELECT 1
    FROM lifecycle_custody_adoption_deliveries ownership
    JOIN lifecycle_rotation_custody_heads head
      ON head.run_id=ownership.run_id AND head.agent_id=ownership.agent_id
     AND head.custody_id=ownership.custody_id
    JOIN lifecycle_receipt_custody_effects effect
      ON effect.run_id=ownership.run_id AND effect.agent_id=ownership.agent_id
     AND effect.custody_id=ownership.custody_id
   WHERE ownership.delivery_id=OLD.result_delivery_id
     AND ownership.active_owner=1 AND head.terminal=0
)
BEGIN SELECT RAISE(ABORT,'INVARIANT_result_delivery_lifecycle_receipt_owner'); END;

CREATE TABLE result_delivery_attempts (
  result_delivery_id TEXT NOT NULL REFERENCES result_deliveries(result_delivery_id),
  command_id TEXT NOT NULL,
  claim_generation INTEGER NOT NULL CHECK (claim_generation >= 0),
  transition TEXT NOT NULL,
  identity_hash TEXT NOT NULL,
  detail_json TEXT NOT NULL,
  created_at INTEGER NOT NULL,
  PRIMARY KEY(result_delivery_id, command_id)
);

CREATE TABLE revocation_proofs (
  proof_id TEXT PRIMARY KEY,
  lease_id TEXT NOT NULL REFERENCES leases(lease_id),
  generation INTEGER NOT NULL,
  kind TEXT NOT NULL,
  evidence_json TEXT NOT NULL,
  created_at INTEGER NOT NULL
);

CREATE TABLE run_authority_revisions(
  project_session_id TEXT NOT NULL,
  coordination_run_id TEXT NOT NULL,
  authority_revision INTEGER NOT NULL CHECK(authority_revision>=1),
  authority_ref TEXT NOT NULL,
  git_allowlist_epoch INTEGER NOT NULL CHECK(git_allowlist_epoch>=1),
  git_allowlist_digest TEXT,
  activated_at_run_revision INTEGER NOT NULL CHECK(activated_at_run_revision>=1),
  created_at INTEGER NOT NULL,
  PRIMARY KEY(project_session_id,coordination_run_id,authority_revision),
  UNIQUE(project_session_id,coordination_run_id,authority_revision,authority_ref,git_allowlist_epoch,git_allowlist_digest),
  FOREIGN KEY(project_session_id,coordination_run_id) REFERENCES runs(project_session_id,run_id),
  CHECK(length(authority_ref)=71 AND substr(authority_ref,1,7)='sha256:'),
  CHECK(git_allowlist_digest IS NULL OR
    (length(git_allowlist_digest)=71 AND substr(git_allowlist_digest,1,7)='sha256:'))
);

CREATE TABLE run_chair_leases (
  project_session_id TEXT NOT NULL,
  run_id TEXT NOT NULL,
  lease_id TEXT NOT NULL UNIQUE,
  holder_agent_id TEXT NOT NULL,
  generation INTEGER NOT NULL CHECK (generation >= 1),
  status TEXT NOT NULL CHECK (status IN ('active','frozen','revoked')),
  handoff_digest TEXT,
  updated_at INTEGER NOT NULL,
  PRIMARY KEY(project_session_id, run_id, generation),
  FOREIGN KEY(project_session_id, run_id) REFERENCES runs(project_session_id, run_id),
  FOREIGN KEY(run_id, holder_agent_id) REFERENCES agents(run_id, agent_id)
);

CREATE TABLE run_git_allowlist_paths(
  project_session_id TEXT NOT NULL,coordination_run_id TEXT NOT NULL,authority_revision INTEGER NOT NULL,
  git_allowlist_epoch INTEGER NOT NULL,repository_root TEXT NOT NULL,worktree_path TEXT NOT NULL,canonical_prefix TEXT NOT NULL,
  PRIMARY KEY(project_session_id,coordination_run_id,authority_revision,git_allowlist_epoch,repository_root,worktree_path,canonical_prefix),
  FOREIGN KEY(project_session_id,coordination_run_id,authority_revision,git_allowlist_epoch)
    REFERENCES run_git_allowlists(project_session_id,coordination_run_id,authority_revision,git_allowlist_epoch)
);

CREATE TABLE run_git_allowlist_profiles(
  project_session_id TEXT NOT NULL,coordination_run_id TEXT NOT NULL,authority_revision INTEGER NOT NULL,
  git_allowlist_epoch INTEGER NOT NULL,profile_id TEXT NOT NULL,profile_revision INTEGER NOT NULL,profile_digest TEXT NOT NULL,
  PRIMARY KEY(project_session_id,coordination_run_id,authority_revision,git_allowlist_epoch,profile_id,profile_revision),
  FOREIGN KEY(project_session_id,coordination_run_id,authority_revision,git_allowlist_epoch)
    REFERENCES run_git_allowlists(project_session_id,coordination_run_id,authority_revision,git_allowlist_epoch),
  FOREIGN KEY(profile_id,profile_revision) REFERENCES git_execution_profiles(profile_id,revision)
);

CREATE TABLE run_git_allowlist_refs(
  project_session_id TEXT NOT NULL,coordination_run_id TEXT NOT NULL,authority_revision INTEGER NOT NULL,
  git_allowlist_epoch INTEGER NOT NULL,ref_name TEXT NOT NULL CHECK(substr(ref_name,1,5)='refs/'),
  PRIMARY KEY(project_session_id,coordination_run_id,authority_revision,git_allowlist_epoch,ref_name),
  FOREIGN KEY(project_session_id,coordination_run_id,authority_revision,git_allowlist_epoch)
    REFERENCES run_git_allowlists(project_session_id,coordination_run_id,authority_revision,git_allowlist_epoch)
);

CREATE TABLE run_git_allowlist_remotes(
  project_session_id TEXT NOT NULL,coordination_run_id TEXT NOT NULL,authority_revision INTEGER NOT NULL,
  git_allowlist_epoch INTEGER NOT NULL,registration_id TEXT NOT NULL,registration_revision INTEGER NOT NULL,
  generation INTEGER NOT NULL,target_digest TEXT NOT NULL,
  PRIMARY KEY(project_session_id,coordination_run_id,authority_revision,git_allowlist_epoch,registration_id,registration_revision),
  FOREIGN KEY(project_session_id,coordination_run_id,authority_revision,git_allowlist_epoch)
    REFERENCES run_git_allowlists(project_session_id,coordination_run_id,authority_revision,git_allowlist_epoch),
  FOREIGN KEY(registration_id,registration_revision,generation,target_digest)
    REFERENCES git_remote_registrations(registration_id,revision,generation,target_digest)
);

CREATE TABLE run_git_allowlist_variants(
  project_session_id TEXT NOT NULL,coordination_run_id TEXT NOT NULL,authority_revision INTEGER NOT NULL,
  git_allowlist_epoch INTEGER NOT NULL,operation_variant TEXT NOT NULL,
  PRIMARY KEY(project_session_id,coordination_run_id,authority_revision,git_allowlist_epoch,operation_variant),
  FOREIGN KEY(project_session_id,coordination_run_id,authority_revision,git_allowlist_epoch)
    REFERENCES run_git_allowlists(project_session_id,coordination_run_id,authority_revision,git_allowlist_epoch),
  CHECK(operation_variant IN (
    'fetch','pull-fast-forward-only','stage','unstage','commit','push-fast-forward-only',
    'branch-create','branch-rename','branch-delete-merged-only','worktree-create-detached',
    'worktree-create-new-branch','worktree-create-existing-branch','worktree-move','worktree-remove-clean',
    'upstream-set','upstream-unset'
  ))
);

CREATE TABLE run_git_allowlists(
  project_session_id TEXT NOT NULL,
  coordination_run_id TEXT NOT NULL,
  authority_revision INTEGER NOT NULL CHECK(authority_revision>=1),
  git_allowlist_epoch INTEGER NOT NULL CHECK(git_allowlist_epoch>=1),
  git_allowlist_digest TEXT NOT NULL,
  allow_worktree_creation INTEGER NOT NULL CHECK(allow_worktree_creation IN (0,1)),
  maximum_expiry INTEGER NOT NULL,
  constraints_json TEXT NOT NULL CHECK(json_valid(constraints_json)),
  created_at INTEGER NOT NULL,
  PRIMARY KEY(project_session_id,coordination_run_id,authority_revision,git_allowlist_epoch),
  UNIQUE(project_session_id,coordination_run_id,authority_revision,git_allowlist_epoch,git_allowlist_digest),
  FOREIGN KEY(project_session_id,coordination_run_id,authority_revision)
    REFERENCES run_authority_revisions(project_session_id,coordination_run_id,authority_revision),
  CHECK(length(git_allowlist_digest)=71 AND substr(git_allowlist_digest,1,7)='sha256:')
);

CREATE TABLE run_metadata (
  run_id TEXT PRIMARY KEY REFERENCES runs(run_id),
  execution_profile TEXT NOT NULL DEFAULT 'unconfigured'
);

CREATE TABLE runs (
  run_id TEXT PRIMARY KEY,
  chair_agent_id TEXT NOT NULL,
  workspace_root TEXT NOT NULL,
  project_run_directory TEXT,
  created_at INTEGER NOT NULL
, project_session_id TEXT REFERENCES project_sessions(project_session_id), lifecycle_state TEXT CHECK (lifecycle_state IN (
  'draft','awaiting_launch','launching','active','quiescing','awaiting_acceptance','closed',
  'launch_failed','launch_ambiguous','reconciling','visibility_degraded','recovery_required',
  'quarantined','cancelled'
)), revision INTEGER CHECK (revision >= 1), chair_generation INTEGER CHECK (chair_generation >= 1), chair_lease_id TEXT, authority_ref TEXT, budget_ref TEXT, dependency_revision INTEGER CHECK (dependency_revision >= 1), topology_slot INTEGER CHECK (topology_slot IS NULL OR topology_slot = 1), project_run_directory_basis TEXT NOT NULL DEFAULT 'none'
  CHECK (project_run_directory_basis IN ('project-relative','none')), authority_revision INTEGER NOT NULL DEFAULT 1 CHECK(authority_revision>=1), git_allowlist_epoch INTEGER NOT NULL DEFAULT 1 CHECK(git_allowlist_epoch>=1), git_allowlist_digest TEXT CHECK(
  git_allowlist_digest IS NULL OR
  (length(git_allowlist_digest)=71 AND substr(git_allowlist_digest,1,7)='sha256:')
));

CREATE TABLE run_plan_declarations (
  run_id TEXT NOT NULL REFERENCES runs(run_id),
  plan_revision INTEGER NOT NULL CHECK(plan_revision>=1),
  plan_path TEXT NOT NULL CHECK(length(plan_path)>0),
  plan_digest TEXT NOT NULL CHECK(length(plan_digest)=71 AND substr(plan_digest,1,7)='sha256:'),
  accepted_scope_artifact_id TEXT NOT NULL REFERENCES artifacts(artifact_id),
  accepted_scope_revision INTEGER NOT NULL CHECK(accepted_scope_revision>=1),
  accepted_scope_path TEXT NOT NULL CHECK(length(accepted_scope_path)>0),
  accepted_scope_digest TEXT NOT NULL CHECK(length(accepted_scope_digest)=71 AND substr(accepted_scope_digest,1,7)='sha256:'),
  declared_task_denominator INTEGER CHECK(declared_task_denominator IS NULL OR declared_task_denominator>=1),
  declared_by_agent_id TEXT NOT NULL,
  declared_at INTEGER NOT NULL,
  PRIMARY KEY(run_id,plan_revision),
  FOREIGN KEY(run_id,declared_by_agent_id) REFERENCES agents(run_id,agent_id)
);

CREATE TABLE scoped_gate_barriers (
  gate_id TEXT NOT NULL REFERENCES scoped_gates(gate_id),
  barrier_id TEXT NOT NULL,
  PRIMARY KEY(gate_id, barrier_id)
);

CREATE TABLE scoped_gate_operations (
  gate_id TEXT NOT NULL REFERENCES scoped_gates(gate_id),
  operation_id TEXT NOT NULL,
  PRIMARY KEY(gate_id, operation_id)
);

CREATE TABLE scoped_gate_tasks (
  gate_id TEXT NOT NULL REFERENCES scoped_gates(gate_id),
  project_session_id TEXT NOT NULL,
  run_id TEXT NOT NULL,
  task_id TEXT NOT NULL,
  binding_kind TEXT NOT NULL CHECK (binding_kind IN ('direct','descendant')),
  bound_dependency_revision INTEGER NOT NULL CHECK (bound_dependency_revision >= 1),
  PRIMARY KEY(gate_id, run_id, task_id),
  FOREIGN KEY(project_session_id, run_id) REFERENCES runs(project_session_id, run_id),
  FOREIGN KEY(run_id, task_id) REFERENCES tasks(run_id, task_id)
);

CREATE TABLE scoped_gates (
  gate_id TEXT PRIMARY KEY,
  project_session_id TEXT NOT NULL,
  coordination_run_id TEXT NOT NULL,
  dedupe_key TEXT NOT NULL,
  scope_kind TEXT NOT NULL CHECK (scope_kind IN ('task','subtree','run','release')),
  scope_task_id TEXT,
  dependency_revision INTEGER NOT NULL CHECK (dependency_revision >= 1),
  blocked_operation_ids_json TEXT NOT NULL,
  enforcement_points_json TEXT NOT NULL,
  question TEXT NOT NULL,
  reason TEXT NOT NULL,
  options_json TEXT NOT NULL,
  recommendation TEXT NOT NULL,
  consequences_json TEXT NOT NULL,
  evidence_refs_json TEXT NOT NULL,
  created_by_ref TEXT NOT NULL,
  expected_approver_ref TEXT NOT NULL,
  resolved_by_operator_id TEXT,
  resolution_json TEXT,
  deadline INTEGER,
  default_action TEXT,
  status TEXT NOT NULL CHECK (status IN ('pending','approved','rejected','deferred','cancelled','superseded')),
  human_required INTEGER NOT NULL CHECK (human_required IN (0,1)),
  release_binding_json TEXT,
  revision INTEGER NOT NULL CHECK (revision >= 1),
  created_at INTEGER NOT NULL,
  updated_at INTEGER NOT NULL,
  UNIQUE(project_session_id, coordination_run_id, dedupe_key),
  FOREIGN KEY(project_session_id, coordination_run_id) REFERENCES runs(project_session_id, run_id)
);

CREATE TABLE subtree_barriers (
  run_id TEXT NOT NULL,
  team_id TEXT NOT NULL,
  generation INTEGER NOT NULL,
  closed_at INTEGER NOT NULL,
  PRIMARY KEY (run_id, team_id, generation)
);

CREATE TABLE task_dependencies (
  run_id TEXT NOT NULL,
  task_id TEXT NOT NULL,
  dependency_task_id TEXT NOT NULL, project_session_id TEXT REFERENCES project_sessions(project_session_id), dependency_revision INTEGER CHECK (dependency_revision >= 1),
  PRIMARY KEY (run_id, task_id, dependency_task_id),
  FOREIGN KEY (run_id, task_id) REFERENCES tasks(run_id, task_id),
  FOREIGN KEY (run_id, dependency_task_id) REFERENCES tasks(run_id, task_id)
);

CREATE TABLE task_eligible_agents (
  run_id TEXT NOT NULL,
  task_id TEXT NOT NULL,
  agent_id TEXT NOT NULL,
  PRIMARY KEY (run_id, task_id, agent_id),
  FOREIGN KEY (run_id, task_id) REFERENCES tasks(run_id, task_id),
  FOREIGN KEY (run_id, agent_id) REFERENCES agents(run_id, agent_id)
);

CREATE TABLE task_expected_artifacts (
  run_id TEXT NOT NULL,
  task_id TEXT NOT NULL,
  relative_path TEXT NOT NULL,
  PRIMARY KEY (run_id, task_id, relative_path),
  FOREIGN KEY (run_id, task_id) REFERENCES tasks(run_id, task_id)
);

CREATE TABLE task_handoff_acknowledgements (
  run_id TEXT NOT NULL,
  task_id TEXT NOT NULL,
  task_revision INTEGER NOT NULL,
  owner_lease_generation INTEGER NOT NULL,
  intended_next_owner_agent_id TEXT NOT NULL,
  acknowledged_by TEXT NOT NULL,
  acknowledged_at INTEGER NOT NULL,
  PRIMARY KEY (run_id, task_id, task_revision, owner_lease_generation)
);

CREATE TABLE task_objective_checks (
  run_id TEXT NOT NULL,
  task_id TEXT NOT NULL,
  check_id TEXT NOT NULL,
  status TEXT NOT NULL DEFAULT 'pending',
  evidence TEXT,
  PRIMARY KEY (run_id, task_id, check_id),
  FOREIGN KEY (run_id, task_id) REFERENCES tasks(run_id, task_id)
);

CREATE TABLE task_obligation_bindings (
  coordination_run_id TEXT NOT NULL,
  task_id TEXT NOT NULL,
  obligation_kind TEXT NOT NULL CHECK (obligation_kind IN ('write-lease','resource-reservation')),
  obligation_id TEXT NOT NULL,
  state TEXT NOT NULL CHECK (state IN ('active','reconciled','abandoned')),
  created_at INTEGER NOT NULL,
  updated_at INTEGER NOT NULL,
  PRIMARY KEY (coordination_run_id, task_id, obligation_kind, obligation_id),
  FOREIGN KEY (coordination_run_id, task_id) REFERENCES tasks(run_id, task_id)
);

CREATE TABLE task_owner_leases (
  project_session_id TEXT NOT NULL,
  run_id TEXT NOT NULL,
  task_id TEXT NOT NULL,
  lease_id TEXT NOT NULL UNIQUE,
  holder_agent_id TEXT NOT NULL,
  generation INTEGER NOT NULL CHECK (generation >= 1),
  status TEXT NOT NULL CHECK (status IN ('active','frozen','released','revoked')),
  updated_at INTEGER NOT NULL,
  PRIMARY KEY(run_id, task_id, generation),
  FOREIGN KEY(project_session_id, run_id) REFERENCES runs(project_session_id, run_id),
  FOREIGN KEY(run_id, task_id) REFERENCES tasks(run_id, task_id),
  FOREIGN KEY(run_id, holder_agent_id) REFERENCES agents(run_id, agent_id)
);

CREATE TABLE task_owner_recoveries (
  recovery_id TEXT PRIMARY KEY,
  run_id TEXT NOT NULL,
  task_id TEXT NOT NULL,
  predecessor_agent_id TEXT NOT NULL,
  successor_agent_id TEXT NOT NULL,
  prior_generation INTEGER NOT NULL,
  new_generation INTEGER NOT NULL,
  evidence_json TEXT NOT NULL,
  created_at INTEGER NOT NULL
);

CREATE TABLE task_owner_recovery_proofs (
  proof_id TEXT PRIMARY KEY,
  run_id TEXT NOT NULL,
  task_id TEXT NOT NULL,
  owner_lease_generation INTEGER NOT NULL,
  predecessor_agent_id TEXT NOT NULL,
  kind TEXT NOT NULL,
  evidence_json TEXT NOT NULL,
  created_at INTEGER NOT NULL
);

CREATE TABLE task_participants (
  run_id TEXT NOT NULL,
  task_id TEXT NOT NULL,
  agent_id TEXT NOT NULL,
  PRIMARY KEY (run_id, task_id, agent_id),
  FOREIGN KEY (run_id, task_id) REFERENCES tasks(run_id, task_id),
  FOREIGN KEY (run_id, agent_id) REFERENCES agents(run_id, agent_id)
);

CREATE TABLE task_proposals (
  run_id TEXT NOT NULL,
  task_id TEXT NOT NULL,
  proposed_owner_agent_id TEXT,
  PRIMARY KEY (run_id, task_id),
  FOREIGN KEY (run_id, task_id) REFERENCES tasks(run_id, task_id)
);

CREATE TABLE task_request_barriers (
  request_id TEXT PRIMARY KEY REFERENCES task_requests(request_id),
  barrier_id TEXT NOT NULL,
  state TEXT NOT NULL CHECK (state IN ('blocked','released','abandoned'))
);

CREATE TABLE task_request_recipients (
  request_id TEXT NOT NULL REFERENCES task_requests(request_id),
  delivery_id TEXT NOT NULL REFERENCES deliveries(delivery_id),
  PRIMARY KEY(request_id, delivery_id)
);

CREATE TABLE task_requests (
  request_id TEXT PRIMARY KEY,
  project_session_id TEXT NOT NULL,
  run_id TEXT NOT NULL,
  task_id TEXT NOT NULL,
  requester_agent_id TEXT NOT NULL,
  request_revision INTEGER NOT NULL CHECK (request_revision >= 1),
  conversation_id TEXT NOT NULL,
  request_message_id TEXT NOT NULL UNIQUE REFERENCES messages(message_id),
  target_agent_id TEXT NOT NULL,
  target_provider_session TEXT NOT NULL,
  expected_artifacts_json TEXT NOT NULL,
  acknowledgement_required INTEGER NOT NULL CHECK (acknowledgement_required IN (0,1)),
  dedupe_key TEXT NOT NULL,
  response_deadline INTEGER NOT NULL,
  callback_id TEXT NOT NULL UNIQUE,
  callback_generation INTEGER NOT NULL CHECK (callback_generation >= 1),
  dependent_barrier_id TEXT NOT NULL,
  state TEXT NOT NULL CHECK (state IN ('pending','answered','overdue','reassigned','abandoned')),
  payload_digest TEXT NOT NULL,
  created_at INTEGER NOT NULL,
  updated_at INTEGER NOT NULL,
  UNIQUE(run_id, requester_agent_id, dedupe_key),
  FOREIGN KEY(project_session_id, run_id) REFERENCES runs(project_session_id, run_id),
  FOREIGN KEY(run_id, task_id) REFERENCES tasks(run_id, task_id),
  FOREIGN KEY(run_id, requester_agent_id) REFERENCES agents(run_id, agent_id),
  FOREIGN KEY(run_id, target_agent_id) REFERENCES agents(run_id, agent_id)
);

CREATE TABLE task_results (
  result_id TEXT PRIMARY KEY,
  request_id TEXT NOT NULL UNIQUE REFERENCES task_requests(request_id),
  project_session_id TEXT NOT NULL,
  run_id TEXT NOT NULL,
  task_id TEXT NOT NULL,
  task_revision INTEGER NOT NULL CHECK (task_revision >= 1),
  reply_message_id TEXT NOT NULL UNIQUE REFERENCES messages(message_id),
  reply_revision INTEGER NOT NULL CHECK (reply_revision >= 1),
  payload_digest TEXT NOT NULL,
  artifacts_json TEXT NOT NULL,
  terminal_state TEXT NOT NULL CHECK (terminal_state IN ('complete')),
  summary TEXT NOT NULL,
  created_at INTEGER NOT NULL,
  FOREIGN KEY(project_session_id, run_id) REFERENCES runs(project_session_id, run_id),
  FOREIGN KEY(run_id, task_id) REFERENCES tasks(run_id, task_id)
);

CREATE TABLE tasks (
  run_id TEXT NOT NULL REFERENCES runs(run_id),
  task_id TEXT NOT NULL,
  authority_id TEXT NOT NULL REFERENCES authorities(authority_id),
  objective TEXT NOT NULL,
  base_revision TEXT NOT NULL,
  state TEXT NOT NULL,
  owner_agent_id TEXT,
  revision INTEGER NOT NULL,
  owner_lease_generation INTEGER NOT NULL DEFAULT 0,
  created_by TEXT NOT NULL,
  PRIMARY KEY (run_id, task_id)
);

CREATE TABLE team_members (
  run_id TEXT NOT NULL,
  team_id TEXT NOT NULL,
  agent_id TEXT NOT NULL,
  PRIMARY KEY (run_id, team_id, agent_id),
  FOREIGN KEY (run_id, team_id) REFERENCES teams(run_id, team_id),
  FOREIGN KEY (run_id, agent_id) REFERENCES agents(run_id, agent_id)
);

CREATE TABLE team_owned_tasks (
  run_id TEXT NOT NULL,
  team_id TEXT NOT NULL,
  task_id TEXT NOT NULL,
  PRIMARY KEY (run_id, team_id, task_id),
  UNIQUE (run_id, task_id),
  FOREIGN KEY (run_id, team_id) REFERENCES teams(run_id, team_id),
  FOREIGN KEY (run_id, task_id) REFERENCES tasks(run_id, task_id)
);

CREATE TABLE teams (
  run_id TEXT NOT NULL,
  team_id TEXT NOT NULL,
  parent_team_id TEXT,
  depth INTEGER NOT NULL,
  leader_agent_id TEXT NOT NULL,
  original_leader_agent_id TEXT NOT NULL,
  successor_agent_id TEXT,
  root_task_id TEXT NOT NULL,
  authority_id TEXT NOT NULL,
  budget_id TEXT NOT NULL,
  state TEXT NOT NULL,
  generation INTEGER NOT NULL,
  handoff_evidence TEXT,
  created_at INTEGER NOT NULL,
  PRIMARY KEY (run_id, team_id)
);

CREATE TABLE workstream_custody (
  workstream_id TEXT PRIMARY KEY REFERENCES workstreams(workstream_id),
  input_digest TEXT NOT NULL,
  launch_packet_artifact_id TEXT NOT NULL REFERENCES artifacts(artifact_id),
  launch_packet_path TEXT NOT NULL,
  launch_packet_digest TEXT NOT NULL,
  team_id TEXT NOT NULL,
  root_task_id TEXT NOT NULL,
  authority_id TEXT NOT NULL REFERENCES authorities(authority_id),
  budget_id TEXT NOT NULL,
  run_scope_id TEXT NOT NULL REFERENCES resource_scopes(scope_id),
  team_scope_id TEXT NOT NULL REFERENCES resource_scopes(scope_id),
  created_at INTEGER NOT NULL,
  CHECK (length(input_digest)=71 AND substr(input_digest,1,7)='sha256:'),
  CHECK (length(launch_packet_digest)=71 AND substr(launch_packet_digest,1,7)='sha256:')
);

CREATE TABLE workstreams (
  workstream_id TEXT PRIMARY KEY,
  project_session_id TEXT NOT NULL,
  coordination_run_id TEXT NOT NULL,
  fabric_task_id TEXT NOT NULL,
  lead_agent_id TEXT NOT NULL,
  delivery_run_id TEXT NOT NULL UNIQUE,
  revision INTEGER NOT NULL CHECK (revision >= 1),
  state TEXT NOT NULL CHECK (state IN ('active','complete','cancelled','degraded','abandoned')),
  created_at INTEGER NOT NULL,
  updated_at INTEGER NOT NULL,
  FOREIGN KEY(project_session_id, coordination_run_id) REFERENCES runs(project_session_id, run_id),
  FOREIGN KEY(coordination_run_id, fabric_task_id) REFERENCES tasks(run_id, task_id),
  FOREIGN KEY(coordination_run_id, lead_agent_id) REFERENCES agents(run_id, agent_id)
);

CREATE TABLE write_scope_entries (
  lease_id TEXT NOT NULL REFERENCES leases(lease_id),
  canonical_path TEXT NOT NULL,
  PRIMARY KEY (lease_id, canonical_path)
);

CREATE TABLE writer_admissions (
  writer_admission_id TEXT PRIMARY KEY,
  reservation_id TEXT NOT NULL UNIQUE REFERENCES resource_reservations(reservation_id),
  repository_root TEXT NOT NULL,
  worktree_path TEXT NOT NULL,
  writer_generation INTEGER NOT NULL CHECK (writer_generation >= 1),
  state TEXT NOT NULL CHECK (state IN ('active','released','revoked'))
);

CREATE TABLE writer_prefixes (
  writer_admission_id TEXT NOT NULL REFERENCES writer_admissions(writer_admission_id),
  canonical_prefix TEXT NOT NULL,
  PRIMARY KEY(writer_admission_id, canonical_prefix)
);

CREATE INDEX agent_bridge_state_by_supervision
  ON agent_bridge_state(bridge_state, adapter_id, updated_at, run_id, agent_id);

CREATE UNIQUE INDEX artifact_project_identity
  ON artifacts(project_id, source_kind, relative_path, sha256)
  WHERE project_session_id IS NULL AND run_id IS NULL AND registry_state='active';

CREATE UNIQUE INDEX artifact_run_identity
  ON artifacts(project_id, run_id, source_kind, relative_path, sha256)
  WHERE run_id IS NOT NULL AND registry_state='active';

CREATE UNIQUE INDEX artifact_session_identity
  ON artifacts(project_id, project_session_id, source_kind, relative_path, sha256)
  WHERE project_session_id IS NOT NULL AND run_id IS NULL AND registry_state='active';

CREATE INDEX artifacts_projection
  ON artifacts(project_id, project_session_id, registry_state, created_at, artifact_id);

CREATE INDEX attachment_liveness ON operator_client_attachments(state, expires_at, daemon_instance_generation, project_authority_generation);

CREATE UNIQUE INDEX chair_bridge_one_open_recovery_per_loss
  ON chair_bridge_recovery_custody(loss_id)
  WHERE state NOT IN ('terminal','no-effect');

CREATE INDEX chair_bridge_recovery_obligations
  ON chair_bridge_recovery_custody(state, provider_adapter_id, updated_at, recovery_id);

CREATE UNIQUE INDEX chair_live_handoff_one_open_per_run
  ON chair_live_handoff_custody(coordination_run_id)
  WHERE state NOT IN ('terminal','no-effect');

CREATE INDEX deliveries_ready_mailbox
  ON deliveries(run_id, recipient_id, mailbox_sequence) WHERE state = 'ready';

CREATE INDEX events_by_run_cursor
  ON events(run_id, event_id);

CREATE INDEX gate_barrier_lookup ON scoped_gate_barriers(barrier_id, gate_id);

CREATE INDEX gate_operation_lookup ON scoped_gate_operations(operation_id, gate_id);

CREATE INDEX gate_status_scope ON scoped_gates(project_session_id, coordination_run_id, status, scope_kind);

CREATE INDEX gate_task_lookup ON scoped_gate_tasks(run_id, task_id, gate_id);

CREATE INDEX intake_revision_latest ON intake_revisions(intake_id, revision DESC);

CREATE INDEX launched_chair_bridge_state_supervision
  ON launched_chair_bridge_state(state, provider_adapter_id, updated_at, coordination_run_id);

CREATE INDEX leases_by_expiry
  ON leases(status, expires_at, run_id);

CREATE INDEX membership_active ON project_session_memberships(project_session_id, coordination_run_id, member_kind, state);

CREATE INDEX notification_claim ON notification_deliveries(state, claim_deadline);

CREATE UNIQUE INDEX one_active_chair_lease_per_run
  ON run_chair_leases(project_session_id,run_id)
  WHERE status='active';

CREATE UNIQUE INDEX one_active_git_execution_profile
  ON git_execution_profiles(profile_id) WHERE state='active';

CREATE UNIQUE INDEX one_active_git_grant_revision
  ON operator_git_grants(grant_id) WHERE state='active';

CREATE UNIQUE INDEX one_active_git_mutation_per_common_dir
  ON git_mutation_reservations(git_common_dir)
  WHERE state IN ('reserved','dispatching','conflict','ambiguous','quarantined');

CREATE UNIQUE INDEX one_active_git_remote_name
  ON git_remote_registrations(project_id,remote_name) WHERE state='active';

CREATE UNIQUE INDEX one_active_operator_task_fence
  ON operator_control_fences(coordination_run_id, task_id)
  WHERE state='paused';

CREATE UNIQUE INDEX one_coordinated_nonterminal_run
  ON runs(project_session_id, topology_slot)
  WHERE topology_slot=1 AND lifecycle_state NOT IN ('closed','cancelled','launch_failed');

CREATE UNIQUE INDEX one_live_operator_daemon_stop
  ON operator_daemon_stop_custody(daemon_instance_generation)
  WHERE state IN ('prepared','scheduled','failed');

CREATE UNIQUE INDEX one_nonterminal_run_per_project_session
  ON runs(project_session_id)
  WHERE lifecycle_state NOT IN ('closed','cancelled','launch_failed');

CREATE UNIQUE INDEX one_unresolved_provider_turn_per_session
  ON provider_session_turn_leases(run_id, agent_id)
  WHERE status IN ('active', 'quarantined');

CREATE INDEX operator_control_fences_by_session
  ON operator_control_fences(project_session_id, state, coordination_run_id, task_id);

CREATE INDEX operator_external_effect_bindings_recovery
  ON operator_external_effect_bindings(lookup_generation,custody_id);

CREATE INDEX operator_git_effect_recovery
  ON operator_git_effect_bindings(state,lookup_generation,custody_id);

CREATE INDEX operator_git_grants_point_of_use
  ON operator_git_grants(project_session_id,coordination_run_id,state,expires_at,
    session_generation,authority_revision,git_allowlist_epoch,execution_profile_id,execution_profile_revision);

CREATE INDEX operator_lifecycle_receipts_by_authority
  ON operator_lifecycle_receipts(
    operator_id, project_id, authority_session_id, kind, created_at DESC
  );

CREATE INDEX projection_cursor ON operator_projection_cursors(project_session_id, schema_version, cursor);

CREATE INDEX provider_actions_unresolved
  ON provider_actions(run_id, updated_at, adapter_id, action_id)
  WHERE status IN ('prepared', 'dispatched', 'ambiguous');

CREATE INDEX provider_agent_custody_by_target
  ON provider_agent_custody(run_id, target_agent_id, created_at DESC);

CREATE INDEX resource_reservation_active ON resource_reservation_dimensions(scope_id, reservation_id);

CREATE INDEX resource_scope_parent ON resource_scopes(parent_scope_id, state);

CREATE INDEX result_claim ON result_deliveries(state, claim_deadline, claim_generation);

CREATE INDEX result_due ON result_deliveries(state, response_deadline);

CREATE INDEX runs_by_lifecycle ON runs(lifecycle_state, project_session_id, run_id);

CREATE UNIQUE INDEX runs_by_project_session_identity ON runs(project_session_id, run_id);

CREATE INDEX session_liveness ON project_sessions(state, project_session_id);

CREATE INDEX tasks_by_owner
  ON tasks(run_id, owner_agent_id, state, task_id) WHERE owner_agent_id IS NOT NULL;

CREATE INDEX tasks_by_state
  ON tasks(run_id, state, task_id);

CREATE INDEX writer_prefix_active ON writer_prefixes(canonical_prefix, writer_admission_id);

CREATE TRIGGER agent_bridge_active_retirement_guard
BEFORE UPDATE OF bridge_state,provider_session_ref,provider_session_generation,
  capability_hash,activation_evidence_digest ON agent_bridge_state
WHEN OLD.bridge_state='active' AND NEW.bridge_state='none' AND NOT (
  EXISTS (
    SELECT 1 FROM runs run
    JOIN project_sessions session ON session.project_session_id=run.project_session_id
    JOIN capabilities capability ON capability.token_hash=OLD.capability_hash
    JOIN agents agent ON agent.run_id=OLD.run_id AND agent.agent_id=OLD.agent_id
    LEFT JOIN launched_chair_bridge_retirements retirement
      ON retirement.project_session_id=run.project_session_id
     AND retirement.coordination_run_id=run.run_id
    WHERE run.run_id=OLD.run_id
      AND run.lifecycle_state IN ('closed','cancelled','launch_failed')
      AND session.state IN ('closed','cancelled')
      AND capability.revoked_at IS NOT NULL AND agent.lifecycle='archived'
      AND retirement.coordination_run_id IS NOT NULL
  ) OR EXISTS (
    SELECT 1 FROM provider_actions action
    JOIN capabilities capability ON capability.token_hash=OLD.capability_hash
    JOIN agents agent ON agent.run_id=OLD.run_id AND agent.agent_id=OLD.agent_id
    WHERE action.run_id=OLD.run_id AND action.target_agent_id=OLD.agent_id
      AND action.operation='release' AND action.status='terminal'
      AND action.provider_session_generation=OLD.provider_session_generation
      AND json_valid(action.payload_json)=1
      AND json_extract(action.payload_json,'$.resumeReference')=OLD.provider_session_ref
      AND capability.revoked_at IS NOT NULL AND agent.lifecycle='archived'
  ) OR EXISTS (
    SELECT 1 FROM chair_bridge_recovery_custody recovery
    JOIN chair_bridge_losses loss ON loss.loss_id=recovery.loss_id
    WHERE recovery.path='takeover' AND recovery.state='committing'
      AND loss.coordination_run_id=OLD.run_id
      AND recovery.successor_agent_id=OLD.agent_id
  ) OR EXISTS (
    SELECT 1 FROM chair_live_handoff_custody custody
     WHERE custody.state='committing'
       AND custody.coordination_run_id=OLD.run_id
       AND custody.successor_agent_id=OLD.agent_id
       AND custody.expected_successor_bridge_revision=OLD.revision
  )
)
BEGIN SELECT RAISE(ABORT,'INVARIANT_agent_bridge_active_retirement_proof'); END;

CREATE TRIGGER agent_bridge_live_delete_forbidden
BEFORE DELETE ON agent_bridge_state
WHEN OLD.bridge_state<>'none'
BEGIN SELECT RAISE(ABORT,'INVARIANT_agent_bridge_active_retirement_proof'); END;

CREATE TRIGGER agent_bridge_lifecycle_rotation_identity_guard
BEFORE UPDATE OF adapter_id,action_id,provider_session_ref,provider_session_generation,
  bridge_generation,capability_hash,activation_evidence_digest,revision ON agent_bridge_state
WHEN OLD.bridge_state='active' AND NEW.bridge_state='active'
  AND EXISTS (
    SELECT 1
      FROM lifecycle_rotation_custodies custody
      JOIN lifecycle_rotation_custody_heads head
        ON head.run_id=custody.run_id AND head.agent_id=custody.agent_id
       AND head.custody_id=custody.custody_id
     WHERE custody.bridge_owner_kind='child'
       AND ((head.state='committing' AND head.terminal=0)
         OR (head.state='finalized' AND head.disposition_code='adopted' AND head.terminal=1))
       AND custody.run_id=OLD.run_id AND custody.agent_id=OLD.agent_id
       AND custody.source_adapter_id=OLD.adapter_id
       AND custody.source_custody_action_id=OLD.action_id
       AND custody.source_provider_session_ref=OLD.provider_session_ref
       AND custody.source_provider_generation=OLD.provider_session_generation
       AND custody.source_bridge_generation=OLD.bridge_generation
       AND custody.source_capability_hash=OLD.capability_hash
       AND custody.source_bridge_revision=OLD.revision
  )
  AND NOT EXISTS (
    SELECT 1
      FROM lifecycle_rotation_custodies custody
      JOIN lifecycle_rotation_custody_heads head
        ON head.run_id=custody.run_id AND head.agent_id=custody.agent_id
       AND head.custody_id=custody.custody_id
      JOIN provider_actions action
        ON action.run_id=custody.run_id
       AND action.adapter_id=custody.replacement_adapter_id
       AND action.action_id=custody.provider_action_id
     WHERE custody.bridge_owner_kind='child'
       AND ((head.state='committing' AND head.terminal=0)
         OR (head.state='finalized' AND head.disposition_code='adopted' AND head.terminal=1))
       AND custody.run_id=OLD.run_id AND custody.agent_id=OLD.agent_id
       AND custody.source_adapter_id=OLD.adapter_id
       AND custody.source_custody_action_id=OLD.action_id
       AND custody.source_provider_session_ref=OLD.provider_session_ref
       AND custody.source_provider_generation=OLD.provider_session_generation
       AND custody.source_bridge_generation=OLD.bridge_generation
       AND custody.source_capability_hash=OLD.capability_hash
       AND custody.source_bridge_revision=OLD.revision
       AND NEW.adapter_id=custody.replacement_adapter_id
       AND NEW.action_id=custody.provider_action_id
       AND action.status='terminal' AND action.idempotency_proven=1
       AND action.execution_count=1 AND action.effect_count=1
       AND action.provider_session_generation=custody.target_provider_generation
       AND json_valid(action.result_json)=1
       AND NEW.provider_session_ref=json_extract(action.result_json,'$.providerSessionRef')
       AND json_extract(action.result_json,'$.providerSessionGeneration')=custody.target_provider_generation
       AND json_extract(action.result_json,'$.bridgeGeneration')=custody.target_bridge_generation
       AND NEW.provider_session_generation=custody.target_provider_generation
       AND NEW.bridge_generation=custody.target_bridge_generation
       AND NEW.capability_hash=custody.staged_capability_hash
       AND NEW.activation_evidence_digest=json_extract(action.result_json,'$.activationEvidenceDigest')
       AND NEW.revision=OLD.revision+1
  )
BEGIN SELECT RAISE(ABORT,'INVARIANT_agent_bridge_lifecycle_rotation_target'); END;

CREATE TRIGGER agents_values_insert BEFORE INSERT ON agents BEGIN
  SELECT CASE WHEN NEW.lifecycle NOT IN ('ready','completion-ready','suspended','context-unreconciled','archived')
    THEN RAISE(ABORT, 'INVARIANT_agents_lifecycle') END;
  SELECT CASE WHEN NOT EXISTS (SELECT 1 FROM authorities a WHERE a.authority_id=NEW.authority_id AND a.run_id=NEW.run_id)
    THEN RAISE(ABORT, 'INVARIANT_agents_authority_same_run') END;
  SELECT CASE WHEN NEW.parent_agent_id IS NOT NULL AND NOT EXISTS (SELECT 1 FROM agents a WHERE a.agent_id=NEW.parent_agent_id AND a.run_id=NEW.run_id)
    THEN RAISE(ABORT, 'INVARIANT_agents_parent_same_run') END;
END;

CREATE TRIGGER agents_values_update BEFORE UPDATE OF lifecycle,authority_id,parent_agent_id,run_id ON agents BEGIN
  SELECT CASE WHEN NEW.lifecycle NOT IN ('ready','completion-ready','suspended','context-unreconciled','archived')
    THEN RAISE(ABORT, 'INVARIANT_agents_lifecycle') END;
  SELECT CASE WHEN NOT EXISTS (SELECT 1 FROM authorities a WHERE a.authority_id=NEW.authority_id AND a.run_id=NEW.run_id)
    THEN RAISE(ABORT, 'INVARIANT_agents_authority_same_run') END;
  SELECT CASE WHEN NEW.parent_agent_id IS NOT NULL AND NOT EXISTS (SELECT 1 FROM agents a WHERE a.agent_id=NEW.parent_agent_id AND a.run_id=NEW.run_id)
    THEN RAISE(ABORT, 'INVARIANT_agents_parent_same_run') END;
END;

CREATE TRIGGER artifact_active_identity_immutable
BEFORE UPDATE OF project_id, project_session_id, run_id, task_id, source_kind, relative_path, sha256,
                 publisher_kind, publisher_ref, publisher_agent_id
ON artifacts
WHEN OLD.registry_state='active'
BEGIN
  SELECT RAISE(ABORT, 'active artifact identity is immutable');
END;

CREATE TRIGGER artifact_active_source_shape_insert
BEFORE INSERT ON artifacts
WHEN NEW.registry_state='active' AND (
  (NEW.source_kind='git-private-diff' AND NOT (
    NEW.run_id IS NULL AND NEW.task_id IS NULL AND
    NEW.publisher_kind='fabric' AND NEW.publisher_ref='fabric-git-private-diff' AND
    NEW.publisher_agent_id IS NULL AND NEW.evidence_kind='diff' AND
    NEW.relative_path='private/git-diffs/' || substr(NEW.sha256, 8) || '.patch'
  )) OR
  (NEW.source_kind<>'git-private-diff' AND NEW.relative_path GLOB 'private/git-diffs/*')
)
BEGIN SELECT RAISE(ABORT, 'active artifact violates the private Git diff namespace'); END;

CREATE TRIGGER artifact_active_source_shape_update
BEFORE UPDATE ON artifacts
WHEN NEW.registry_state='active' AND (
  (NEW.source_kind='git-private-diff' AND NOT (
    NEW.run_id IS NULL AND NEW.task_id IS NULL AND
    NEW.publisher_kind='fabric' AND NEW.publisher_ref='fabric-git-private-diff' AND
    NEW.publisher_agent_id IS NULL AND NEW.evidence_kind='diff' AND
    NEW.relative_path='private/git-diffs/' || substr(NEW.sha256, 8) || '.patch'
  )) OR
  (NEW.source_kind<>'git-private-diff' AND NEW.relative_path GLOB 'private/git-diffs/*')
)
BEGIN SELECT RAISE(ABORT, 'active artifact violates the private Git diff namespace'); END;

CREATE TRIGGER artifact_run_file_requires_descendant_insert
BEFORE INSERT ON artifacts
WHEN NEW.registry_state='active' AND NEW.source_kind='run-file'
 AND NOT EXISTS (
   SELECT 1 FROM runs run
    WHERE run.run_id=NEW.run_id AND run.project_session_id=NEW.project_session_id
      AND run.project_run_directory_basis='project-relative'
      AND run.project_run_directory IS NOT NULL AND run.project_run_directory<>'.'
 )
BEGIN SELECT RAISE(ABORT, 'active run-file requires a strict-descendant run root'); END;

CREATE TRIGGER artifact_run_file_requires_descendant_update
BEFORE UPDATE ON artifacts
WHEN NEW.registry_state='active' AND NEW.source_kind='run-file'
 AND NOT EXISTS (
   SELECT 1 FROM runs run
    WHERE run.run_id=NEW.run_id AND run.project_session_id=NEW.project_session_id
      AND run.project_run_directory_basis='project-relative'
      AND run.project_run_directory IS NOT NULL AND run.project_run_directory<>'.'
 )
BEGIN SELECT RAISE(ABORT, 'active run-file requires a strict-descendant run root'); END;

CREATE TRIGGER attention_revision_step BEFORE UPDATE OF revision ON attention_items
WHEN NEW.revision<>OLD.revision+1
BEGIN SELECT RAISE(ABORT, 'AFAB_0004_ATTENTION_REVISION'); END;

CREATE TRIGGER authorities_parent_insert BEFORE INSERT ON authorities
WHEN NEW.parent_authority_id IS NOT NULL AND NOT EXISTS (SELECT 1 FROM authorities a WHERE a.authority_id=NEW.parent_authority_id AND a.run_id=NEW.run_id)
BEGIN SELECT RAISE(ABORT, 'INVARIANT_authorities_parent_same_run'); END;

CREATE TRIGGER authorities_parent_update BEFORE UPDATE OF parent_authority_id,run_id ON authorities
WHEN NEW.parent_authority_id IS NOT NULL AND NOT EXISTS (SELECT 1 FROM authorities a WHERE a.authority_id=NEW.parent_authority_id AND a.run_id=NEW.run_id)
BEGIN SELECT RAISE(ABORT, 'INVARIANT_authorities_parent_same_run'); END;

CREATE TRIGGER authority_budget_boolean_insert BEFORE INSERT ON authority_budget
WHEN NEW.usage_unknown NOT IN (0,1) BEGIN SELECT RAISE(ABORT, 'INVARIANT_authority_budget_boolean'); END;

CREATE TRIGGER authority_budget_boolean_update BEFORE UPDATE OF usage_unknown ON authority_budget
WHEN NEW.usage_unknown NOT IN (0,1) BEGIN SELECT RAISE(ABORT, 'INVARIANT_authority_budget_boolean'); END;

CREATE TRIGGER barrier_gate_block BEFORE INSERT ON barriers
WHEN EXISTS (
  SELECT 1 FROM scoped_gates g JOIN scoped_gate_barriers gb ON gb.gate_id=g.gate_id
   WHERE gb.barrier_id=NEW.run_id || ':' || NEW.scope || ':' || NEW.stage_id
     AND g.status IN ('pending','deferred')
)
BEGIN SELECT RAISE(ABORT, 'AFAB_0004_GATE_BLOCKED'); END;

CREATE TRIGGER barriers_state_insert BEFORE INSERT ON barriers
WHEN NEW.state <> 'closed' BEGIN SELECT RAISE(ABORT, 'INVARIANT_barriers_state'); END;

CREATE TRIGGER barriers_state_update BEFORE UPDATE OF state ON barriers
WHEN NEW.state <> 'closed' BEGIN SELECT RAISE(ABORT, 'INVARIANT_barriers_state'); END;

CREATE TRIGGER budget_dimensions_values_insert BEFORE INSERT ON budget_dimensions
WHEN NEW.direct_usage_unknown NOT IN (0,1) OR NEW.usage_unknown NOT IN (0,1) OR NEW.granted < 0 OR NEW.reserved < 0 OR NEW.consumed < 0 OR NEW.reserved > NEW.granted OR NEW.consumed > NEW.granted
BEGIN SELECT RAISE(ABORT, 'INVARIANT_budget_dimensions_values'); END;

CREATE TRIGGER budget_dimensions_values_update BEFORE UPDATE OF direct_usage_unknown,usage_unknown,granted,reserved,consumed ON budget_dimensions
WHEN NEW.direct_usage_unknown NOT IN (0,1) OR NEW.usage_unknown NOT IN (0,1) OR NEW.granted < 0 OR NEW.reserved < 0 OR NEW.consumed < 0 OR NEW.reserved > NEW.granted OR NEW.consumed > NEW.granted
BEGIN SELECT RAISE(ABORT, 'INVARIANT_budget_dimensions_values'); END;

CREATE TRIGGER budgets_state_insert BEFORE INSERT ON budgets
WHEN NEW.state NOT IN ('active','usage-unknown','released') BEGIN SELECT RAISE(ABORT, 'INVARIANT_budgets_state'); END;

CREATE TRIGGER budgets_state_update BEFORE UPDATE OF state ON budgets
WHEN NEW.state NOT IN ('active','usage-unknown','released') BEGIN SELECT RAISE(ABORT, 'INVARIANT_budgets_state'); END;

CREATE TRIGGER capabilities_generation_insert BEFORE INSERT ON capabilities
WHEN NEW.principal_generation < 1 BEGIN SELECT RAISE(ABORT, 'INVARIANT_capabilities_generation'); END;

CREATE TRIGGER capabilities_generation_update BEFORE UPDATE OF principal_generation ON capabilities
WHEN NEW.principal_generation < 1 BEGIN SELECT RAISE(ABORT, 'INVARIANT_capabilities_generation'); END;

CREATE TRIGGER mcp_seat_generation_previous_project_insert
BEFORE INSERT ON mcp_seat_generations
WHEN NEW.previous_generation IS NOT NULL AND NOT EXISTS (
  SELECT 1 FROM mcp_seat_generations previous
   WHERE previous.generation=NEW.previous_generation AND previous.project_id=NEW.project_id
)
BEGIN SELECT RAISE(ABORT, 'INVARIANT_mcp_seat_generation_previous_project'); END;

CREATE TRIGGER mcp_seat_generations_update_forbidden
BEFORE UPDATE ON mcp_seat_generations
BEGIN SELECT RAISE(ABORT, 'INVARIANT_mcp_seat_generations_immutable'); END;

CREATE TRIGGER mcp_seat_generations_delete_forbidden
BEFORE DELETE ON mcp_seat_generations
BEGIN SELECT RAISE(ABORT, 'INVARIANT_mcp_seat_generations_immutable'); END;

CREATE TRIGGER mcp_seat_generation_members_update_forbidden
BEFORE UPDATE ON mcp_seat_generation_members
BEGIN SELECT RAISE(ABORT, 'INVARIANT_mcp_seat_generation_members_immutable'); END;

CREATE TRIGGER mcp_seat_generation_members_delete_forbidden
BEFORE DELETE ON mcp_seat_generation_members
BEGIN SELECT RAISE(ABORT, 'INVARIANT_mcp_seat_generation_members_immutable'); END;

CREATE TRIGGER mcp_active_seat_generation_forward_only
BEFORE UPDATE OF generation ON mcp_active_seat_generations
WHEN NOT EXISTS (
  SELECT 1 FROM mcp_seat_generations next
   WHERE next.generation=NEW.generation
     AND next.project_id=OLD.project_id
     AND next.previous_generation=OLD.generation
)
BEGIN SELECT RAISE(ABORT, 'INVARIANT_mcp_active_seat_generation_forward_only'); END;

CREATE TRIGGER mcp_active_seat_generation_delete_forbidden
BEFORE DELETE ON mcp_active_seat_generations
BEGIN SELECT RAISE(ABORT, 'INVARIANT_mcp_active_seat_generation_immutable'); END;

CREATE TRIGGER chair_bridge_loss_blocks_run_reactivation
BEFORE UPDATE OF lifecycle_state ON runs
WHEN NEW.lifecycle_state='active' AND EXISTS (
  SELECT 1 FROM launched_chair_bridge_state bridge
   WHERE bridge.coordination_run_id=NEW.run_id AND bridge.state='lost'
) AND NOT EXISTS (
  SELECT 1 FROM chair_bridge_recovery_custody recovery
  JOIN chair_bridge_losses loss ON loss.loss_id=recovery.loss_id
  WHERE recovery.state='committing' AND recovery.path IN ('rebind','takeover')
    AND loss.coordination_run_id=NEW.run_id
)
BEGIN SELECT RAISE(ABORT, 'INVARIANT_chair_bridge_loss_requires_explicit_recovery'); END;

CREATE TRIGGER chair_bridge_loss_blocks_session_reactivation
BEFORE UPDATE OF state ON project_sessions
WHEN NEW.state='active' AND EXISTS (
  SELECT 1 FROM launched_chair_bridge_state bridge
   WHERE bridge.project_session_id=NEW.project_session_id AND bridge.state='lost'
) AND NOT EXISTS (
  SELECT 1 FROM chair_bridge_recovery_custody recovery
  JOIN chair_bridge_losses loss ON loss.loss_id=recovery.loss_id
  WHERE recovery.state='committing' AND recovery.path IN ('rebind','takeover')
    AND loss.project_session_id=NEW.project_session_id
)
BEGIN SELECT RAISE(ABORT, 'INVARIANT_chair_bridge_loss_requires_explicit_recovery'); END;

CREATE TRIGGER chair_bridge_loss_freezes_authority_grants
BEFORE INSERT ON authorities
WHEN EXISTS (
  SELECT 1 FROM launched_chair_bridge_state bridge
   WHERE bridge.coordination_run_id=NEW.run_id AND bridge.state='lost'
)
BEGIN SELECT RAISE(ABORT, 'INVARIANT_chair_bridge_loss_freezes_grants'); END;

CREATE TRIGGER chair_bridge_loss_freezes_capability_grants
BEFORE INSERT ON capabilities
WHEN (
  EXISTS (
    SELECT 1 FROM launched_chair_bridge_state bridge
     WHERE bridge.coordination_run_id=NEW.run_id AND bridge.state='lost'
  ) OR EXISTS (
    SELECT 1 FROM chair_bridge_recovery_custody recovery
     WHERE recovery.path='rebind'
       AND recovery.new_capability_hash=NEW.token_hash
  )
) AND NOT EXISTS (
  SELECT 1 FROM chair_bridge_recovery_custody recovery
  JOIN chair_bridge_losses loss ON loss.loss_id=recovery.loss_id
  WHERE recovery.path='rebind' AND recovery.state='prepared'
    AND loss.coordination_run_id=NEW.run_id
    AND loss.chair_agent_id=NEW.agent_id
    AND recovery.new_capability_hash=NEW.token_hash
    AND recovery.new_principal_generation=NEW.principal_generation
)
BEGIN SELECT RAISE(ABORT, 'INVARIANT_chair_bridge_loss_freezes_grants'); END;

CREATE TRIGGER chair_bridge_loss_resolutions_immutable_delete
BEFORE DELETE ON chair_bridge_loss_resolutions
BEGIN SELECT RAISE(ABORT, 'INVARIANT_chair_bridge_loss_resolutions_immutable'); END;

CREATE TRIGGER chair_bridge_loss_resolutions_immutable_update
BEFORE UPDATE ON chair_bridge_loss_resolutions
BEGIN SELECT RAISE(ABORT, 'INVARIANT_chair_bridge_loss_resolutions_immutable'); END;

CREATE TRIGGER chair_bridge_losses_immutable_delete
BEFORE DELETE ON chair_bridge_losses
BEGIN SELECT RAISE(ABORT, 'INVARIANT_chair_bridge_losses_immutable'); END;

CREATE TRIGGER chair_bridge_losses_immutable_update
BEFORE UPDATE ON chair_bridge_losses
BEGIN SELECT RAISE(ABORT, 'INVARIANT_chair_bridge_losses_immutable'); END;

CREATE TRIGGER chair_bridge_recovery_capability_delete_forbidden
BEFORE DELETE ON capabilities
WHEN EXISTS (
  SELECT 1 FROM chair_bridge_recovery_custody recovery
   WHERE recovery.path='rebind' AND recovery.new_capability_hash=OLD.token_hash
)
BEGIN SELECT RAISE(ABORT, 'INVARIANT_chair_bridge_loss_freezes_grants'); END;

CREATE TRIGGER chair_bridge_recovery_capability_identity_immutable
BEFORE UPDATE OF token_hash, run_id, agent_id, principal_generation ON capabilities
WHEN (
  OLD.token_hash<>NEW.token_hash OR OLD.run_id<>NEW.run_id OR
  OLD.agent_id<>NEW.agent_id OR OLD.principal_generation<>NEW.principal_generation
) AND NOT (
  OLD.token_hash=NEW.token_hash AND OLD.run_id=NEW.run_id AND OLD.agent_id=NEW.agent_id AND
  NEW.principal_generation=OLD.principal_generation+1 AND
  OLD.revoked_at IS NULL AND NEW.revoked_at IS NOT NULL AND EXISTS (
    SELECT 1 FROM chair_bridge_recovery_custody terminal_recovery
     WHERE terminal_recovery.path='rebind' AND terminal_recovery.state='terminal'
       AND terminal_recovery.new_capability_hash=OLD.token_hash
  )
) AND EXISTS (
  SELECT 1 FROM chair_bridge_recovery_custody recovery
   WHERE recovery.path='rebind'
     AND recovery.new_capability_hash IN (OLD.token_hash, NEW.token_hash)
)
BEGIN SELECT RAISE(ABORT, 'INVARIANT_chair_bridge_loss_freezes_grants'); END;

CREATE TRIGGER chair_live_handoff_delete_forbidden BEFORE DELETE ON chair_live_handoff_custody
BEGIN SELECT RAISE(ABORT, 'INVARIANT_chair_live_handoff_identity_immutable'); END;

CREATE TRIGGER chair_live_handoff_freezes_authority_grants BEFORE INSERT ON authorities
WHEN EXISTS (
  SELECT 1 FROM chair_live_handoff_custody custody
   WHERE custody.coordination_run_id=NEW.run_id
     AND custody.state NOT IN ('terminal','no-effect')
)
BEGIN SELECT RAISE(ABORT, 'INVARIANT_chair_live_handoff_freezes_grants'); END;

CREATE TRIGGER chair_live_handoff_freezes_capability_grants BEFORE INSERT ON capabilities
WHEN EXISTS (
  SELECT 1 FROM chair_live_handoff_custody custody
   WHERE custody.coordination_run_id=NEW.run_id
     AND custody.state NOT IN ('terminal','no-effect')
)
BEGIN SELECT RAISE(ABORT, 'INVARIANT_chair_live_handoff_freezes_grants'); END;

CREATE TRIGGER chair_live_handoff_freezes_provider_actions BEFORE INSERT ON provider_actions
WHEN EXISTS (
  SELECT 1 FROM chair_live_handoff_custody custody
   WHERE custody.coordination_run_id=NEW.run_id
     AND custody.state NOT IN ('terminal','no-effect')
     AND custody.promotion_action_id<>NEW.action_id
)
BEGIN SELECT RAISE(ABORT, 'INVARIANT_chair_live_handoff_freezes_grants'); END;

CREATE TRIGGER chair_live_handoff_identity_immutable
BEFORE UPDATE OF
  custody_id,operator_id,operator_command_id,project_session_id,coordination_run_id,
  intent_digest,intent_json,handoff_path,handoff_digest,predecessor_agent_id,
  successor_agent_id,successor_authority_id,successor_authority_digest,
  expected_session_revision,expected_session_generation,expected_membership_revision,
  expected_run_revision,expected_chair_generation,expected_chair_lease_id,
  expected_bridge_revision,expected_chair_bridge_generation,
  expected_predecessor_principal_generation,expected_successor_principal_generation,
  expected_successor_bridge_revision,expected_successor_bridge_generation,
  provider_adapter_id,provider_contract_digest,source_provider_action_id,
  promotion_action_id,provider_session_ref,provider_session_generation,
  new_bridge_generation,created_at
ON chair_live_handoff_custody
BEGIN SELECT RAISE(ABORT, 'INVARIANT_chair_live_handoff_identity_immutable'); END;

CREATE TRIGGER chair_live_handoff_resolution_immutable_delete BEFORE DELETE ON chair_live_handoff_resolutions
BEGIN SELECT RAISE(ABORT, 'INVARIANT_chair_live_handoff_resolution_immutable'); END;

CREATE TRIGGER chair_live_handoff_resolution_immutable_update BEFORE UPDATE ON chair_live_handoff_resolutions
BEGIN SELECT RAISE(ABORT, 'INVARIANT_chair_live_handoff_resolution_immutable'); END;

CREATE TRIGGER chair_live_handoff_state_transition
BEFORE UPDATE OF state,revision ON chair_live_handoff_custody
WHEN NOT (
  (OLD.state='prepared' AND NEW.state='dispatched' AND NEW.revision=OLD.revision+1) OR
  (OLD.state='prepared' AND NEW.state='no-effect' AND NEW.revision=OLD.revision+1) OR
  (OLD.state IN ('dispatched','ambiguous') AND NEW.state IN ('committing','no-effect','ambiguous')
    AND NEW.revision=OLD.revision+1) OR
  (OLD.state='committing' AND NEW.state='terminal' AND NEW.revision=OLD.revision+1)
)
BEGIN SELECT RAISE(ABORT, 'INVARIANT_chair_live_handoff_transition'); END;

CREATE TRIGGER child_bridge_losses_immutable_delete
BEFORE DELETE ON child_bridge_losses
BEGIN
  SELECT RAISE(ABORT, 'INVARIANT_child_bridge_losses_immutable');
END;

CREATE TRIGGER child_bridge_losses_immutable_update
BEFORE UPDATE ON child_bridge_losses
BEGIN
  SELECT RAISE(ABORT, 'INVARIANT_child_bridge_losses_immutable');
END;

CREATE TRIGGER deliveries_values_insert BEFORE INSERT ON deliveries BEGIN
  SELECT CASE WHEN NEW.state NOT IN ('ready','claimed','acknowledged','abandoned','expired') OR NEW.attempt_count < 0 OR NEW.mailbox_sequence < 1
    THEN RAISE(ABORT, 'INVARIANT_deliveries_values') END;
  SELECT CASE WHEN NOT EXISTS (SELECT 1 FROM messages m WHERE m.message_id=NEW.message_id AND m.run_id=NEW.run_id)
    THEN RAISE(ABORT, 'INVARIANT_deliveries_message_same_run') END;
  SELECT CASE WHEN NOT EXISTS (SELECT 1 FROM agents a WHERE a.agent_id=NEW.recipient_id AND a.run_id=NEW.run_id)
    THEN RAISE(ABORT, 'INVARIANT_deliveries_recipient_same_run') END;
END;

CREATE TRIGGER deliveries_values_update BEFORE UPDATE OF state,attempt_count,mailbox_sequence,message_id,recipient_id,run_id ON deliveries BEGIN
  SELECT CASE WHEN NEW.state NOT IN ('ready','claimed','acknowledged','abandoned','expired') OR NEW.attempt_count < 0 OR NEW.mailbox_sequence < 1
    THEN RAISE(ABORT, 'INVARIANT_deliveries_values') END;
  SELECT CASE WHEN NOT EXISTS (SELECT 1 FROM messages m WHERE m.message_id=NEW.message_id AND m.run_id=NEW.run_id)
    THEN RAISE(ABORT, 'INVARIANT_deliveries_message_same_run') END;
  SELECT CASE WHEN NOT EXISTS (SELECT 1 FROM agents a WHERE a.agent_id=NEW.recipient_id AND a.run_id=NEW.run_id)
    THEN RAISE(ABORT, 'INVARIANT_deliveries_recipient_same_run') END;
END;

CREATE TRIGGER dependency_guard_complete BEFORE DELETE ON dependency_mutation_guards BEGIN
  SELECT CASE WHEN (SELECT dependency_revision FROM runs WHERE run_id=OLD.run_id)<>OLD.target_revision
    OR (SELECT count(*) FROM task_dependencies WHERE run_id=OLD.run_id)<>OLD.expected_edge_count
    OR EXISTS (SELECT 1 FROM task_dependencies WHERE run_id=OLD.run_id AND dependency_revision<>OLD.target_revision)
    OR (SELECT count(*) FROM scoped_gate_tasks gt JOIN scoped_gates g ON g.gate_id=gt.gate_id
         WHERE g.coordination_run_id=OLD.run_id AND g.status IN ('pending','deferred'))<>OLD.expected_binding_count
    OR EXISTS (SELECT 1 FROM scoped_gate_tasks gt JOIN scoped_gates g ON g.gate_id=gt.gate_id
         WHERE g.coordination_run_id=OLD.run_id AND g.status IN ('pending','deferred')
           AND gt.bound_dependency_revision<>OLD.target_revision)
    THEN RAISE(ABORT, 'AFAB_0004_DEPENDENCY_REBIND_INCOMPLETE') END;
END;

CREATE TRIGGER dependency_guard_delete BEFORE DELETE ON task_dependencies
WHEN NOT EXISTS (SELECT 1 FROM dependency_mutation_guards g WHERE g.run_id=OLD.run_id AND g.project_session_id=OLD.project_session_id)
BEGIN SELECT RAISE(ABORT, 'AFAB_0004_DEPENDENCY_MUTATOR'); END;

CREATE TRIGGER dependency_guard_insert BEFORE INSERT ON task_dependencies
WHEN NOT EXISTS (SELECT 1 FROM dependency_mutation_guards g WHERE g.run_id=NEW.run_id AND g.project_session_id=NEW.project_session_id AND g.target_revision=NEW.dependency_revision)
BEGIN SELECT RAISE(ABORT, 'AFAB_0004_DEPENDENCY_MUTATOR'); END;

CREATE TRIGGER dependency_guard_update BEFORE UPDATE ON task_dependencies
WHEN NOT EXISTS (SELECT 1 FROM dependency_mutation_guards g WHERE g.run_id=NEW.run_id AND g.project_session_id=NEW.project_session_id AND g.target_revision=NEW.dependency_revision)
BEGIN SELECT RAISE(ABORT, 'AFAB_0004_DEPENDENCY_MUTATOR'); END;

CREATE TRIGGER events_actor_insert BEFORE INSERT ON events
WHEN NEW.actor_agent_id IS NOT NULL AND NOT EXISTS (SELECT 1 FROM agents a WHERE a.agent_id=NEW.actor_agent_id AND a.run_id=NEW.run_id)
BEGIN SELECT RAISE(ABORT, 'INVARIANT_events_actor_same_run'); END;

CREATE TRIGGER events_actor_update BEFORE UPDATE OF actor_agent_id,run_id ON events
WHEN NEW.actor_agent_id IS NOT NULL AND NOT EXISTS (SELECT 1 FROM agents a WHERE a.agent_id=NEW.actor_agent_id AND a.run_id=NEW.run_id)
BEGIN SELECT RAISE(ABORT, 'INVARIANT_events_actor_same_run'); END;

CREATE TRIGGER gate_human_resolution BEFORE UPDATE OF status ON scoped_gates
WHEN NEW.status='approved' AND OLD.human_required=1 AND NEW.resolved_by_operator_id IS NULL
BEGIN SELECT RAISE(ABORT, 'AFAB_0004_GATE_HUMAN_REQUIRED'); END;

CREATE TRIGGER gate_release_binding BEFORE INSERT ON scoped_gates
WHEN NEW.scope_kind='release' AND NEW.release_binding_json IS NULL
BEGIN SELECT RAISE(ABORT, 'AFAB_0004_GATE_RELEASE_BINDING'); END;

CREATE TRIGGER git_binding_identity_immutable
BEFORE UPDATE OF custody_id,project_id,project_session_id,prepared_session_revision,session_generation,
  coordination_run_id,prepared_run_revision,prepared_dependency_revision,authority_ref,
  authority_revision,git_allowlist_epoch,git_allowlist_digest,grant_id,grant_revision,draft_id,
  draft_revision,gate_id,gate_revision,repository_root,worktree_path,repository_state_digest,
  execution_profile_id,execution_profile_revision,execution_profile_digest,remote_registration_id,
  remote_registration_revision,remote_generation,remote_target_digest,operation_id,operation_variant,
  effect_binding_digest,result_recipe_digest,decision_digest,before_git_state_json,
  expected_terminal_state_json,mutation_reservation_generation,lock_plan_digest,created_at
ON operator_git_effect_bindings
BEGIN SELECT RAISE(ABORT,'INVARIANT_git_binding_identity_immutable'); END;

CREATE TRIGGER git_draft_gate_association
AFTER INSERT ON scoped_gate_operations
WHEN EXISTS (
  SELECT 1 FROM git_operation_drafts draft
   WHERE draft.operation_id=NEW.operation_id AND draft.state='open'
)
BEGIN
  UPDATE git_operation_drafts
     SET state='gate-bound',revision=revision+1,updated_at=(
       SELECT updated_at FROM scoped_gates WHERE gate_id=NEW.gate_id
     )
   WHERE operation_id=NEW.operation_id AND state='open';
END;

CREATE TRIGGER git_draft_gate_association_guard
BEFORE INSERT ON scoped_gate_operations
WHEN EXISTS (SELECT 1 FROM git_operation_drafts WHERE operation_id=NEW.operation_id)
BEGIN
  SELECT CASE WHEN EXISTS (
    SELECT 1 FROM scoped_gate_operations WHERE operation_id=NEW.operation_id
  ) THEN RAISE(ABORT,'INVARIANT_git_draft_has_one_gate') END;
  SELECT CASE WHEN NOT EXISTS (
    SELECT 1 FROM git_operation_drafts draft JOIN scoped_gates gate ON gate.gate_id=NEW.gate_id
     WHERE draft.operation_id=NEW.operation_id
       AND gate.project_session_id=draft.project_session_id
       AND gate.coordination_run_id=draft.coordination_run_id
       AND gate.dependency_revision=draft.observed_dependency_revision
       AND instr(gate.enforcement_points_json,'operation')>0
  ) THEN RAISE(ABORT,'INVARIANT_git_draft_gate_scope') END;
END;

CREATE TRIGGER git_draft_identity_immutable
BEFORE UPDATE OF draft_id,draft_request_id,request_digest,operator_id,project_id,project_session_id,
  observed_session_revision,session_generation,coordination_run_id,observed_run_revision,
  observed_dependency_revision,authority_ref,authority_revision,git_allowlist_epoch,
  git_allowlist_digest,draft_kind,operation_id,operation_kind,payload_digest,binding_json,
  draft_digest,expires_at,created_at ON git_operation_drafts
BEGIN SELECT RAISE(ABORT,'INVARIANT_git_draft_identity_immutable'); END;

CREATE TRIGGER git_draft_terminal_gate
AFTER UPDATE OF status ON scoped_gates
WHEN NEW.status IN ('rejected','cancelled','superseded')
BEGIN
  UPDATE git_operation_drafts
     SET state='cancelled',revision=revision+1,terminal_reason='gate-' || NEW.status,updated_at=NEW.updated_at
   WHERE operation_id IN (SELECT operation_id FROM scoped_gate_operations WHERE gate_id=NEW.gate_id)
     AND state IN ('open','gate-bound');
  UPDATE operation_admissions
     SET state='cancelled',revision=revision+1
   WHERE operation_id IN (SELECT operation_id FROM scoped_gate_operations WHERE gate_id=NEW.gate_id)
     AND state='prepared';
END;

CREATE TRIGGER git_execution_profile_delete_forbidden
BEFORE DELETE ON git_execution_profiles
BEGIN SELECT RAISE(ABORT,'INVARIANT_git_profile_immutable'); END;

CREATE TRIGGER git_execution_profile_identity_immutable
BEFORE UPDATE OF profile_id,revision,profile_digest,git_binary_path,git_binary_version,
  git_binary_digest,object_format,merge_backend_id,rebase_backend_id,environment_digest,
  helper_registry_digest,inspector_digest,created_at ON git_execution_profiles
BEGIN SELECT RAISE(ABORT,'INVARIANT_git_profile_immutable'); END;

CREATE TRIGGER git_execution_profile_state_monotonic
BEFORE UPDATE OF state ON git_execution_profiles
WHEN NOT (OLD.state='active' AND NEW.state='revoked')
BEGIN SELECT RAISE(ABORT,'INVARIANT_git_profile_state'); END;

CREATE TRIGGER git_grant_delete_forbidden BEFORE DELETE ON operator_git_grants
BEGIN SELECT RAISE(ABORT,'INVARIANT_git_grant_delete_forbidden'); END;

CREATE TRIGGER git_grant_identity_immutable
BEFORE UPDATE OF grant_id,revision,project_id,project_session_id,session_generation,issuing_session_revision,
  coordination_run_id,issuing_run_revision,issuing_dependency_revision,authority_ref,authority_revision,
  git_allowlist_epoch,git_allowlist_digest,repository_root,worktree_path,execution_profile_id,
  execution_profile_revision,execution_profile_digest,allow_worktree_creation,source_kind,source_digest,
  constraints_json,grant_digest,expires_at,created_at ON operator_git_grants
BEGIN SELECT RAISE(ABORT,'INVARIANT_git_grant_identity_immutable'); END;

CREATE TRIGGER git_remote_registration_delete_forbidden
BEFORE DELETE ON git_remote_registrations
BEGIN SELECT RAISE(ABORT,'INVARIANT_git_remote_immutable'); END;

CREATE TRIGGER git_remote_registration_identity_immutable
BEFORE UPDATE OF registration_id,revision,generation,project_id,remote_name,transport_kind,
  target_identity,target_digest,adapter_id,adapter_contract_digest,credential_selector_digest,created_at
ON git_remote_registrations
BEGIN SELECT RAISE(ABORT,'INVARIANT_git_remote_immutable'); END;

CREATE TRIGGER git_remote_registration_state_monotonic
BEFORE UPDATE OF state ON git_remote_registrations
WHEN NOT (OLD.state='active' AND NEW.state='revoked')
BEGIN SELECT RAISE(ABORT,'INVARIANT_git_remote_state'); END;

CREATE TRIGGER git_resolution_delete_forbidden
BEFORE DELETE ON git_custody_resolutions
BEGIN SELECT RAISE(ABORT,'INVARIANT_git_resolution_immutable'); END;

CREATE TRIGGER git_resolution_immutable
BEFORE UPDATE ON git_custody_resolutions
BEGIN SELECT RAISE(ABORT,'INVARIANT_git_resolution_immutable'); END;

CREATE TRIGGER global_revision_agent_adapter_bindings_delete AFTER DELETE ON agent_adapter_bindings BEGIN UPDATE daemon_global_state SET revision=revision+1 WHERE singleton=1; END;

CREATE TRIGGER global_revision_agent_adapter_bindings_insert AFTER INSERT ON agent_adapter_bindings BEGIN UPDATE daemon_global_state SET revision=revision+1 WHERE singleton=1; END;

CREATE TRIGGER global_revision_agent_adapter_bindings_update AFTER UPDATE ON agent_adapter_bindings BEGIN UPDATE daemon_global_state SET revision=revision+1 WHERE singleton=1; END;

CREATE TRIGGER global_revision_agent_bridge_state_insert
AFTER INSERT ON agent_bridge_state
BEGIN
  UPDATE daemon_global_state SET revision=revision+1 WHERE singleton=1;
END;

CREATE TRIGGER global_revision_agent_bridge_state_update
AFTER UPDATE ON agent_bridge_state
BEGIN
  UPDATE daemon_global_state SET revision=revision+1 WHERE singleton=1;
END;

CREATE TRIGGER global_revision_agents_delete AFTER DELETE ON agents BEGIN UPDATE daemon_global_state SET revision=revision+1 WHERE singleton=1; END;

CREATE TRIGGER global_revision_agents_insert AFTER INSERT ON agents BEGIN UPDATE daemon_global_state SET revision=revision+1 WHERE singleton=1; END;

CREATE TRIGGER global_revision_agents_update AFTER UPDATE ON agents BEGIN UPDATE daemon_global_state SET revision=revision+1 WHERE singleton=1; END;

CREATE TRIGGER global_revision_artifacts_delete AFTER DELETE ON artifacts
BEGIN UPDATE daemon_global_state SET revision=revision+1 WHERE singleton=1; END;

CREATE TRIGGER global_revision_artifacts_insert AFTER INSERT ON artifacts
BEGIN UPDATE daemon_global_state SET revision=revision+1 WHERE singleton=1; END;

CREATE TRIGGER global_revision_artifacts_update AFTER UPDATE ON artifacts
BEGIN UPDATE daemon_global_state SET revision=revision+1 WHERE singleton=1; END;

CREATE TRIGGER global_revision_attention_items_delete AFTER DELETE ON attention_items BEGIN UPDATE daemon_global_state SET revision=revision+1 WHERE singleton=1; END;

CREATE TRIGGER global_revision_attention_items_insert AFTER INSERT ON attention_items BEGIN UPDATE daemon_global_state SET revision=revision+1 WHERE singleton=1; END;

CREATE TRIGGER global_revision_attention_items_update AFTER UPDATE ON attention_items BEGIN UPDATE daemon_global_state SET revision=revision+1 WHERE singleton=1; END;

CREATE TRIGGER global_revision_chair_bridge_loss_insert
AFTER INSERT ON chair_bridge_losses
BEGIN UPDATE daemon_global_state SET revision=revision+1 WHERE singleton=1; END;

CREATE TRIGGER global_revision_chair_bridge_recovery_insert
AFTER INSERT ON chair_bridge_recovery_custody
BEGIN UPDATE daemon_global_state SET revision=revision+1 WHERE singleton=1; END;

CREATE TRIGGER global_revision_chair_bridge_recovery_update
AFTER UPDATE ON chair_bridge_recovery_custody
BEGIN UPDATE daemon_global_state SET revision=revision+1 WHERE singleton=1; END;

CREATE TRIGGER global_revision_chair_bridge_resolution_insert
AFTER INSERT ON chair_bridge_loss_resolutions
BEGIN UPDATE daemon_global_state SET revision=revision+1 WHERE singleton=1; END;

CREATE TRIGGER global_revision_chair_live_handoff_insert AFTER INSERT ON chair_live_handoff_custody
BEGIN UPDATE daemon_global_state SET revision=revision+1 WHERE singleton=1; END;

CREATE TRIGGER global_revision_chair_live_handoff_resolution_insert AFTER INSERT ON chair_live_handoff_resolutions
BEGIN UPDATE daemon_global_state SET revision=revision+1 WHERE singleton=1; END;

CREATE TRIGGER global_revision_chair_live_handoff_update AFTER UPDATE ON chair_live_handoff_custody
BEGIN UPDATE daemon_global_state SET revision=revision+1 WHERE singleton=1; END;

CREATE TRIGGER global_revision_child_bridge_losses_insert
AFTER INSERT ON child_bridge_losses
BEGIN
  UPDATE daemon_global_state SET revision=revision+1 WHERE singleton=1;
END;

CREATE TRIGGER global_revision_events_delete AFTER DELETE ON events BEGIN UPDATE daemon_global_state SET revision=revision+1 WHERE singleton=1; END;

CREATE TRIGGER global_revision_events_insert AFTER INSERT ON events BEGIN UPDATE daemon_global_state SET revision=revision+1 WHERE singleton=1; END;

CREATE TRIGGER global_revision_events_update AFTER UPDATE ON events BEGIN UPDATE daemon_global_state SET revision=revision+1 WHERE singleton=1; END;

CREATE TRIGGER global_revision_git_binding_insert AFTER INSERT ON operator_git_effect_bindings
BEGIN UPDATE daemon_global_state SET revision=revision+1 WHERE singleton=1; END;

CREATE TRIGGER global_revision_git_binding_update AFTER UPDATE ON operator_git_effect_bindings
BEGIN UPDATE daemon_global_state SET revision=revision+1 WHERE singleton=1; END;

CREATE TRIGGER global_revision_git_draft_insert AFTER INSERT ON git_operation_drafts
BEGIN UPDATE daemon_global_state SET revision=revision+1 WHERE singleton=1; END;

CREATE TRIGGER global_revision_git_draft_update AFTER UPDATE ON git_operation_drafts
BEGIN UPDATE daemon_global_state SET revision=revision+1 WHERE singleton=1; END;

CREATE TRIGGER global_revision_git_execution_profile_insert AFTER INSERT ON git_execution_profiles
BEGIN UPDATE daemon_global_state SET revision=revision+1 WHERE singleton=1; END;

CREATE TRIGGER global_revision_git_execution_profile_update AFTER UPDATE ON git_execution_profiles
BEGIN UPDATE daemon_global_state SET revision=revision+1 WHERE singleton=1; END;

CREATE TRIGGER global_revision_git_grant_insert AFTER INSERT ON operator_git_grants
BEGIN UPDATE daemon_global_state SET revision=revision+1 WHERE singleton=1; END;

CREATE TRIGGER global_revision_git_grant_update AFTER UPDATE ON operator_git_grants
BEGIN UPDATE daemon_global_state SET revision=revision+1 WHERE singleton=1; END;

CREATE TRIGGER global_revision_git_remote_insert AFTER INSERT ON git_remote_registrations
BEGIN UPDATE daemon_global_state SET revision=revision+1 WHERE singleton=1; END;

CREATE TRIGGER global_revision_git_remote_update AFTER UPDATE ON git_remote_registrations
BEGIN UPDATE daemon_global_state SET revision=revision+1 WHERE singleton=1; END;

CREATE TRIGGER global_revision_intakes_delete AFTER DELETE ON intakes BEGIN UPDATE daemon_global_state SET revision=revision+1 WHERE singleton=1; END;

CREATE TRIGGER global_revision_intakes_insert AFTER INSERT ON intakes BEGIN UPDATE daemon_global_state SET revision=revision+1 WHERE singleton=1; END;

CREATE TRIGGER global_revision_intakes_update AFTER UPDATE ON intakes BEGIN UPDATE daemon_global_state SET revision=revision+1 WHERE singleton=1; END;

CREATE TRIGGER global_revision_integration_availability_delete AFTER DELETE ON integration_availability BEGIN UPDATE daemon_global_state SET revision=revision+1 WHERE singleton=1; END;

CREATE TRIGGER global_revision_integration_availability_insert AFTER INSERT ON integration_availability BEGIN UPDATE daemon_global_state SET revision=revision+1 WHERE singleton=1; END;

CREATE TRIGGER global_revision_integration_availability_update
AFTER UPDATE ON integration_availability
BEGIN UPDATE daemon_global_state SET revision=revision+1 WHERE singleton=1; END;

CREATE TRIGGER global_revision_launched_chair_bridge_insert
AFTER INSERT ON launched_chair_bridge_state
BEGIN UPDATE daemon_global_state SET revision=revision+1 WHERE singleton=1; END;

CREATE TRIGGER global_revision_launched_chair_bridge_retirement_insert
AFTER INSERT ON launched_chair_bridge_retirements
BEGIN UPDATE daemon_global_state SET revision=revision+1 WHERE singleton=1; END;

CREATE TRIGGER global_revision_launched_chair_bridge_update
AFTER UPDATE ON launched_chair_bridge_state
BEGIN UPDATE daemon_global_state SET revision=revision+1 WHERE singleton=1; END;

CREATE TRIGGER global_revision_leases_delete AFTER DELETE ON leases BEGIN
  UPDATE daemon_global_state SET revision=revision+1 WHERE singleton=1;
END;

CREATE TRIGGER global_revision_leases_insert AFTER INSERT ON leases BEGIN
  UPDATE daemon_global_state SET revision=revision+1 WHERE singleton=1;
END;

CREATE TRIGGER global_revision_leases_update AFTER UPDATE ON leases BEGIN
  UPDATE daemon_global_state SET revision=revision+1 WHERE singleton=1;
END;

CREATE TRIGGER global_revision_message_contexts_delete AFTER DELETE ON message_contexts BEGIN UPDATE daemon_global_state SET revision=revision+1 WHERE singleton=1; END;

CREATE TRIGGER global_revision_message_contexts_insert AFTER INSERT ON message_contexts BEGIN UPDATE daemon_global_state SET revision=revision+1 WHERE singleton=1; END;

CREATE TRIGGER global_revision_message_contexts_update AFTER UPDATE ON message_contexts BEGIN UPDATE daemon_global_state SET revision=revision+1 WHERE singleton=1; END;

CREATE TRIGGER global_revision_messages_delete AFTER DELETE ON messages BEGIN UPDATE daemon_global_state SET revision=revision+1 WHERE singleton=1; END;

CREATE TRIGGER global_revision_messages_insert AFTER INSERT ON messages BEGIN UPDATE daemon_global_state SET revision=revision+1 WHERE singleton=1; END;

CREATE TRIGGER global_revision_messages_update AFTER UPDATE ON messages BEGIN UPDATE daemon_global_state SET revision=revision+1 WHERE singleton=1; END;

CREATE TRIGGER global_revision_notification_deliveries_delete AFTER DELETE ON notification_deliveries
BEGIN UPDATE daemon_global_state SET revision=revision+1 WHERE singleton=1; END;

CREATE TRIGGER global_revision_notification_deliveries_insert AFTER INSERT ON notification_deliveries
BEGIN UPDATE daemon_global_state SET revision=revision+1 WHERE singleton=1; END;

CREATE TRIGGER global_revision_notification_deliveries_update AFTER UPDATE ON notification_deliveries
BEGIN UPDATE daemon_global_state SET revision=revision+1 WHERE singleton=1; END;

CREATE TRIGGER global_revision_observer_event_sequence_delete AFTER DELETE ON observer_event_sequence BEGIN UPDATE daemon_global_state SET revision=revision+1 WHERE singleton=1; END;

CREATE TRIGGER global_revision_observer_event_sequence_insert AFTER INSERT ON observer_event_sequence BEGIN UPDATE daemon_global_state SET revision=revision+1 WHERE singleton=1; END;

CREATE TRIGGER global_revision_observer_event_sequence_update AFTER UPDATE ON observer_event_sequence BEGIN UPDATE daemon_global_state SET revision=revision+1 WHERE singleton=1; END;

CREATE TRIGGER global_revision_operator_client_attachments_delete AFTER DELETE ON operator_client_attachments BEGIN
  UPDATE daemon_global_state SET revision=revision+1 WHERE singleton=1;
END;

CREATE TRIGGER global_revision_operator_client_attachments_insert AFTER INSERT ON operator_client_attachments BEGIN
  UPDATE daemon_global_state SET revision=revision+1 WHERE singleton=1;
END;

CREATE TRIGGER global_revision_operator_client_attachments_update AFTER UPDATE ON operator_client_attachments BEGIN
  UPDATE daemon_global_state SET revision=revision+1 WHERE singleton=1;
END;

CREATE TRIGGER global_revision_operator_control_fences_insert
AFTER INSERT ON operator_control_fences
BEGIN UPDATE daemon_global_state SET revision=revision+1 WHERE singleton=1; END;

CREATE TRIGGER global_revision_operator_control_fences_update
AFTER UPDATE ON operator_control_fences
BEGIN UPDATE daemon_global_state SET revision=revision+1 WHERE singleton=1; END;

CREATE TRIGGER global_revision_operator_external_effect_binding_insert
AFTER INSERT ON operator_external_effect_bindings
BEGIN UPDATE daemon_global_state SET revision=revision+1 WHERE singleton=1; END;

CREATE TRIGGER global_revision_operator_external_effect_binding_update
AFTER UPDATE ON operator_external_effect_bindings
BEGIN UPDATE daemon_global_state SET revision=revision+1 WHERE singleton=1; END;

CREATE TRIGGER global_revision_project_session_launch_custody_insert
AFTER INSERT ON project_session_launch_custody
BEGIN
  UPDATE daemon_global_state SET revision=revision+1 WHERE singleton=1;
END;

CREATE TRIGGER global_revision_project_sessions_delete AFTER DELETE ON project_sessions BEGIN
  UPDATE daemon_global_state SET revision=revision+1 WHERE singleton=1;
END;

CREATE TRIGGER global_revision_project_sessions_insert AFTER INSERT ON project_sessions BEGIN
  UPDATE daemon_global_state SET revision=revision+1 WHERE singleton=1;
END;

CREATE TRIGGER global_revision_project_sessions_update AFTER UPDATE ON project_sessions BEGIN
  UPDATE daemon_global_state SET revision=revision+1 WHERE singleton=1;
END;

CREATE TRIGGER global_revision_projects_delete AFTER DELETE ON projects BEGIN UPDATE daemon_global_state SET revision=revision+1 WHERE singleton=1; END;

CREATE TRIGGER global_revision_projects_insert AFTER INSERT ON projects BEGIN UPDATE daemon_global_state SET revision=revision+1 WHERE singleton=1; END;

CREATE TRIGGER global_revision_projects_update AFTER UPDATE ON projects BEGIN UPDATE daemon_global_state SET revision=revision+1 WHERE singleton=1; END;

CREATE TRIGGER global_revision_run_plan_declarations_insert
AFTER INSERT ON run_plan_declarations
BEGIN UPDATE daemon_global_state SET revision=revision+1 WHERE singleton=1; END;

CREATE TRIGGER global_revision_provider_actions_delete AFTER DELETE ON provider_actions BEGIN
  UPDATE daemon_global_state SET revision=revision+1 WHERE singleton=1;
END;

CREATE TRIGGER global_revision_provider_actions_insert AFTER INSERT ON provider_actions BEGIN
  UPDATE daemon_global_state SET revision=revision+1 WHERE singleton=1;
END;

CREATE TRIGGER global_revision_provider_actions_update AFTER UPDATE ON provider_actions BEGIN
  UPDATE daemon_global_state SET revision=revision+1 WHERE singleton=1;
END;

CREATE TRIGGER global_revision_provider_agent_custody_insert
AFTER INSERT ON provider_agent_custody
BEGIN
  UPDATE daemon_global_state SET revision=revision+1 WHERE singleton=1;
END;

CREATE TRIGGER global_revision_provider_state_delete AFTER DELETE ON provider_state BEGIN UPDATE daemon_global_state SET revision=revision+1 WHERE singleton=1; END;

CREATE TRIGGER global_revision_provider_state_insert AFTER INSERT ON provider_state BEGIN UPDATE daemon_global_state SET revision=revision+1 WHERE singleton=1; END;

CREATE TRIGGER global_revision_provider_state_update AFTER UPDATE ON provider_state BEGIN UPDATE daemon_global_state SET revision=revision+1 WHERE singleton=1; END;

CREATE TRIGGER global_revision_resource_dimensions_delete AFTER DELETE ON resource_dimensions BEGIN UPDATE daemon_global_state SET revision=revision+1 WHERE singleton=1; END;

CREATE TRIGGER global_revision_resource_dimensions_insert AFTER INSERT ON resource_dimensions BEGIN UPDATE daemon_global_state SET revision=revision+1 WHERE singleton=1; END;

CREATE TRIGGER global_revision_resource_dimensions_update AFTER UPDATE ON resource_dimensions BEGIN UPDATE daemon_global_state SET revision=revision+1 WHERE singleton=1; END;

CREATE TRIGGER global_revision_resource_scopes_delete AFTER DELETE ON resource_scopes BEGIN UPDATE daemon_global_state SET revision=revision+1 WHERE singleton=1; END;

CREATE TRIGGER global_revision_resource_scopes_insert AFTER INSERT ON resource_scopes BEGIN UPDATE daemon_global_state SET revision=revision+1 WHERE singleton=1; END;

CREATE TRIGGER global_revision_resource_scopes_update AFTER UPDATE ON resource_scopes BEGIN UPDATE daemon_global_state SET revision=revision+1 WHERE singleton=1; END;

CREATE TRIGGER global_revision_result_deliveries_insert AFTER INSERT ON result_deliveries
WHEN NEW.required=1 BEGIN UPDATE daemon_global_state SET revision=revision+1 WHERE singleton=1; END;

CREATE TRIGGER global_revision_result_deliveries_update AFTER UPDATE ON result_deliveries
WHEN NEW.required=1 BEGIN UPDATE daemon_global_state SET revision=revision+1 WHERE singleton=1; END;

CREATE TRIGGER global_revision_run_git_allowlist_insert AFTER INSERT ON run_git_allowlists
BEGIN UPDATE daemon_global_state SET revision=revision+1 WHERE singleton=1; END;

CREATE TRIGGER global_revision_runs_delete AFTER DELETE ON runs BEGIN
  UPDATE daemon_global_state SET revision=revision+1 WHERE singleton=1;
END;

CREATE TRIGGER global_revision_runs_insert AFTER INSERT ON runs BEGIN
  UPDATE daemon_global_state SET revision=revision+1 WHERE singleton=1;
END;

CREATE TRIGGER global_revision_runs_update AFTER UPDATE ON runs BEGIN
  UPDATE daemon_global_state SET revision=revision+1 WHERE singleton=1;
END;

CREATE TRIGGER global_revision_task_objective_checks_delete AFTER DELETE ON task_objective_checks BEGIN UPDATE daemon_global_state SET revision=revision+1 WHERE singleton=1; END;

CREATE TRIGGER global_revision_task_objective_checks_insert AFTER INSERT ON task_objective_checks BEGIN UPDATE daemon_global_state SET revision=revision+1 WHERE singleton=1; END;

CREATE TRIGGER global_revision_task_objective_checks_update AFTER UPDATE ON task_objective_checks BEGIN UPDATE daemon_global_state SET revision=revision+1 WHERE singleton=1; END;

CREATE TRIGGER global_revision_tasks_delete AFTER DELETE ON tasks BEGIN
  UPDATE daemon_global_state SET revision=revision+1 WHERE singleton=1;
END;

CREATE TRIGGER global_revision_tasks_insert AFTER INSERT ON tasks BEGIN
  UPDATE daemon_global_state SET revision=revision+1 WHERE singleton=1;
END;

CREATE TRIGGER global_revision_tasks_update AFTER UPDATE ON tasks BEGIN
  UPDATE daemon_global_state SET revision=revision+1 WHERE singleton=1;
END;

CREATE TRIGGER global_revision_workstream_custody_insert AFTER INSERT ON workstream_custody
BEGIN UPDATE daemon_global_state SET revision=revision+1 WHERE singleton=1; END;

CREATE TRIGGER global_revision_workstreams_delete AFTER DELETE ON workstreams BEGIN UPDATE daemon_global_state SET revision=revision+1 WHERE singleton=1; END;

CREATE TRIGGER global_revision_workstreams_insert AFTER INSERT ON workstreams BEGIN UPDATE daemon_global_state SET revision=revision+1 WHERE singleton=1; END;

CREATE TRIGGER global_revision_workstreams_update AFTER UPDATE ON workstreams BEGIN UPDATE daemon_global_state SET revision=revision+1 WHERE singleton=1; END;

CREATE TRIGGER intake_accepted_scope_insert
BEFORE INSERT ON intakes
WHEN NOT (
  (NEW.state='accepted' AND NEW.accepted_scope_state='bound' AND NEW.accepted_scope_artifact_id IS NOT NULL) OR
  (NEW.state<>'accepted' AND NEW.accepted_scope_state='not-applicable' AND NEW.accepted_scope_artifact_id IS NULL)
)
BEGIN SELECT RAISE(ABORT, 'intake accepted scope state is invalid'); END;

CREATE TRIGGER intake_accepted_scope_registry_insert
BEFORE INSERT ON intakes
WHEN NEW.state='accepted' AND NEW.accepted_scope_state='bound' AND NOT EXISTS (
  SELECT 1 FROM artifacts artifact
   WHERE artifact.artifact_id=NEW.accepted_scope_artifact_id
     AND artifact.registry_state='active' AND artifact.project_id=NEW.project_id
     AND (artifact.project_session_id IS NULL OR artifact.project_session_id=NEW.project_session_id)
     AND (artifact.run_id IS NULL OR artifact.run_id=NEW.coordination_run_id)
     AND (
       SELECT COUNT(*) FROM json_each(NEW.artifact_refs_json) ref
        WHERE json_extract(ref.value, '$.path')=artifact.relative_path
          AND json_extract(ref.value, '$.digest')=artifact.sha256
     )=1
)
BEGIN SELECT RAISE(ABORT, 'accepted scope must reference one active exact-scope registry row'); END;

CREATE TRIGGER intake_accepted_scope_registry_update
BEFORE UPDATE ON intakes
WHEN NEW.state='accepted' AND NEW.accepted_scope_state='bound' AND NOT EXISTS (
  SELECT 1 FROM artifacts artifact
   WHERE artifact.artifact_id=NEW.accepted_scope_artifact_id
     AND artifact.registry_state='active' AND artifact.project_id=NEW.project_id
     AND (artifact.project_session_id IS NULL OR artifact.project_session_id=NEW.project_session_id)
     AND (artifact.run_id IS NULL OR artifact.run_id=NEW.coordination_run_id)
     AND (
       SELECT COUNT(*) FROM json_each(NEW.artifact_refs_json) ref
        WHERE json_extract(ref.value, '$.path')=artifact.relative_path
          AND json_extract(ref.value, '$.digest')=artifact.sha256
     )=1
)
BEGIN SELECT RAISE(ABORT, 'accepted scope must reference one active exact-scope registry row'); END;

CREATE TRIGGER intake_accepted_scope_update
BEFORE UPDATE ON intakes
WHEN NOT (
  (NEW.state='accepted' AND NEW.accepted_scope_state IN ('bound','recovery-required') AND
    ((NEW.accepted_scope_state='bound' AND NEW.accepted_scope_artifact_id IS NOT NULL) OR
     (NEW.accepted_scope_state='recovery-required' AND NEW.accepted_scope_artifact_id IS NULL))) OR
  (NEW.state<>'accepted' AND NEW.accepted_scope_state='not-applicable' AND NEW.accepted_scope_artifact_id IS NULL)
)
BEGIN SELECT RAISE(ABORT, 'intake accepted scope state is invalid'); END;

CREATE TRIGGER intake_artifact_binding_exact_insert
BEFORE INSERT ON intake_artifact_bindings
WHEN NOT EXISTS (
  SELECT 1
    FROM artifacts artifact JOIN intakes intake ON intake.intake_id=NEW.intake_id
   WHERE artifact.artifact_id=NEW.artifact_id AND artifact.registry_state='active'
     AND artifact.relative_path=NEW.relative_path AND artifact.sha256=NEW.sha256
     AND artifact.project_id=intake.project_id
     AND (artifact.project_session_id IS NULL OR artifact.project_session_id=intake.project_session_id)
     AND (artifact.run_id IS NULL OR artifact.run_id=intake.coordination_run_id)
)
BEGIN SELECT RAISE(ABORT, 'intake artifact binding must reference one active exact-scope registry row'); END;

CREATE TRIGGER intake_artifact_binding_exact_update
BEFORE UPDATE ON intake_artifact_bindings
WHEN NOT EXISTS (
  SELECT 1
    FROM artifacts artifact JOIN intakes intake ON intake.intake_id=NEW.intake_id
   WHERE artifact.artifact_id=NEW.artifact_id AND artifact.registry_state='active'
     AND artifact.relative_path=NEW.relative_path AND artifact.sha256=NEW.sha256
     AND artifact.project_id=intake.project_id
     AND (artifact.project_session_id IS NULL OR artifact.project_session_id=intake.project_session_id)
     AND (artifact.run_id IS NULL OR artifact.run_id=intake.coordination_run_id)
)
BEGIN SELECT RAISE(ABORT, 'intake artifact binding must reference one active exact-scope registry row'); END;

CREATE TRIGGER intake_revision_accepted_scope_insert
BEFORE INSERT ON intake_revisions
WHEN NOT (
  (NEW.state='accepted' AND NEW.accepted_scope_state='bound' AND NEW.accepted_scope_artifact_id IS NOT NULL) OR
  (NEW.state<>'accepted' AND NEW.accepted_scope_state='not-applicable' AND NEW.accepted_scope_artifact_id IS NULL)
)
BEGIN SELECT RAISE(ABORT, 'intake revision accepted scope state is invalid'); END;

CREATE TRIGGER intake_revision_accepted_scope_registry_insert
BEFORE INSERT ON intake_revisions
WHEN NEW.state='accepted' AND NEW.accepted_scope_state='bound' AND NOT EXISTS (
  SELECT 1
    FROM intakes intake JOIN artifacts artifact
      ON artifact.artifact_id=NEW.accepted_scope_artifact_id
   WHERE intake.intake_id=NEW.intake_id
     AND artifact.registry_state='active' AND artifact.project_id=intake.project_id
     AND (artifact.project_session_id IS NULL OR artifact.project_session_id=intake.project_session_id)
     AND (artifact.run_id IS NULL OR artifact.run_id=intake.coordination_run_id)
     AND (
       SELECT COUNT(*) FROM json_each(NEW.payload_json, '$.artifactRefs') ref
        WHERE json_extract(ref.value, '$.path')=artifact.relative_path
          AND json_extract(ref.value, '$.digest')=artifact.sha256
     )=1
)
BEGIN SELECT RAISE(ABORT, 'accepted scope must reference one active exact-scope registry row'); END;

CREATE TRIGGER intake_revision_accepted_scope_registry_update
BEFORE UPDATE ON intake_revisions
WHEN NEW.state='accepted' AND NEW.accepted_scope_state='bound' AND NOT EXISTS (
  SELECT 1
    FROM intakes intake JOIN artifacts artifact
      ON artifact.artifact_id=NEW.accepted_scope_artifact_id
   WHERE intake.intake_id=NEW.intake_id
     AND artifact.registry_state='active' AND artifact.project_id=intake.project_id
     AND (artifact.project_session_id IS NULL OR artifact.project_session_id=intake.project_session_id)
     AND (artifact.run_id IS NULL OR artifact.run_id=intake.coordination_run_id)
     AND (
       SELECT COUNT(*) FROM json_each(NEW.payload_json, '$.artifactRefs') ref
        WHERE json_extract(ref.value, '$.path')=artifact.relative_path
          AND json_extract(ref.value, '$.digest')=artifact.sha256
     )=1
)
BEGIN SELECT RAISE(ABORT, 'accepted scope must reference one active exact-scope registry row'); END;

CREATE TRIGGER intake_revision_accepted_scope_update
BEFORE UPDATE ON intake_revisions
WHEN NOT (
  (NEW.state='accepted' AND NEW.accepted_scope_state IN ('bound','recovery-required') AND
    ((NEW.accepted_scope_state='bound' AND NEW.accepted_scope_artifact_id IS NOT NULL) OR
     (NEW.accepted_scope_state='recovery-required' AND NEW.accepted_scope_artifact_id IS NULL))) OR
  (NEW.state<>'accepted' AND NEW.accepted_scope_state='not-applicable' AND NEW.accepted_scope_artifact_id IS NULL)
)
BEGIN SELECT RAISE(ABORT, 'intake revision accepted scope state is invalid'); END;

CREATE TRIGGER launch_custody_immutable_delete
BEFORE DELETE ON project_session_launch_custody
BEGIN
  SELECT RAISE(ABORT, 'INVARIANT_launch_custody_immutable');
END;

CREATE TRIGGER launch_custody_immutable_update
BEFORE UPDATE ON project_session_launch_custody
BEGIN
  SELECT RAISE(ABORT, 'INVARIANT_launch_custody_immutable');
END;

CREATE TRIGGER launch_custody_requires_attestation_challenge
BEFORE INSERT ON project_session_launch_custody
WHEN NEW.attestation_challenge_digest IS NULL
  OR length(NEW.attestation_challenge_digest) <> 71
  OR substr(NEW.attestation_challenge_digest, 1, 7) <> 'sha256:'
BEGIN
  SELECT RAISE(ABORT, 'INVARIANT_launch_attestation_challenge_digest');
END;

CREATE TRIGGER launched_chair_bridge_delete_forbidden
BEFORE DELETE ON launched_chair_bridge_state
BEGIN SELECT RAISE(ABORT, 'INVARIANT_launched_chair_bridge_identity_immutable'); END;

CREATE TRIGGER launched_chair_bridge_identity_immutable
BEFORE UPDATE OF
  project_session_id, coordination_run_id, chair_agent_id,
  provider_adapter_id, provider_action_id, provider_contract_digest, provider_session_ref,
  provider_session_generation, principal_generation, bridge_generation,
  capability_hash, activation_evidence_digest, created_at
ON launched_chair_bridge_state
WHEN NOT EXISTS (
  SELECT 1
    FROM chair_bridge_recovery_custody recovery
    JOIN chair_bridge_losses loss ON loss.loss_id=recovery.loss_id
   WHERE recovery.state='committing' AND recovery.path IN ('rebind','takeover')
     AND OLD.state='lost' AND loss.project_session_id=OLD.project_session_id
     AND loss.coordination_run_id=OLD.coordination_run_id
     AND recovery.new_chair_agent_id=NEW.chair_agent_id
     AND recovery.provider_adapter_id=NEW.provider_adapter_id
     AND recovery.new_provider_action_id=NEW.provider_action_id
     AND recovery.provider_contract_digest=NEW.provider_contract_digest
     AND recovery.new_provider_session_ref=NEW.provider_session_ref
     AND recovery.new_provider_session_generation=NEW.provider_session_generation
     AND recovery.new_principal_generation=NEW.principal_generation
     AND recovery.new_bridge_generation=NEW.bridge_generation
     AND recovery.new_capability_hash=NEW.capability_hash
     AND recovery.new_activation_evidence_digest=NEW.activation_evidence_digest
) AND NOT EXISTS (
  SELECT 1 FROM chair_live_handoff_custody custody
   WHERE custody.state='committing'
     AND OLD.state='active' AND NEW.state='active'
     AND custody.project_session_id=OLD.project_session_id
     AND custody.coordination_run_id=OLD.coordination_run_id
     AND custody.successor_agent_id=NEW.chair_agent_id
     AND custody.provider_adapter_id=NEW.provider_adapter_id
     AND custody.promotion_action_id=NEW.provider_action_id
     AND custody.provider_contract_digest=NEW.provider_contract_digest
     AND custody.provider_session_ref=NEW.provider_session_ref
     AND custody.provider_session_generation=NEW.provider_session_generation
     AND custody.expected_successor_principal_generation=NEW.principal_generation
     AND custody.new_bridge_generation=NEW.bridge_generation
) AND NOT EXISTS (
  SELECT 1
    FROM lifecycle_rotation_custodies custody
    JOIN lifecycle_rotation_custody_heads head
      ON head.run_id=custody.run_id AND head.agent_id=custody.agent_id
     AND head.custody_id=custody.custody_id
   WHERE custody.bridge_owner_kind='chair' AND head.state='committing' AND head.terminal=0
     AND custody.project_session_id=OLD.project_session_id
     AND custody.run_id=OLD.coordination_run_id AND custody.agent_id=OLD.chair_agent_id
     AND custody.source_adapter_id=OLD.provider_adapter_id
     AND custody.source_custody_action_id=OLD.provider_action_id
     AND custody.source_adapter_contract_digest=OLD.provider_contract_digest
     AND custody.source_provider_session_ref=OLD.provider_session_ref
     AND custody.source_provider_generation=OLD.provider_session_generation
     AND custody.source_principal_generation=OLD.principal_generation
     AND custody.source_bridge_generation=OLD.bridge_generation
     AND custody.source_bridge_revision=OLD.revision
     AND NEW.state='active'
     AND NEW.chair_agent_id=custody.agent_id
     AND NEW.provider_adapter_id=custody.replacement_adapter_id
     AND NEW.provider_action_id=custody.provider_action_id
     AND NEW.provider_contract_digest=custody.replacement_contract_digest
     AND NEW.provider_session_generation=custody.target_provider_generation
     AND NEW.principal_generation=custody.target_principal_generation
     AND NEW.bridge_generation=custody.target_bridge_generation
     AND NEW.capability_hash=custody.staged_capability_hash
)
BEGIN SELECT RAISE(ABORT, 'INVARIANT_launched_chair_bridge_identity_immutable'); END;

CREATE TRIGGER launched_chair_bridge_retirement_immutable_delete
BEFORE DELETE ON launched_chair_bridge_retirements
BEGIN SELECT RAISE(ABORT,'INVARIANT_launched_chair_bridge_retirement_immutable'); END;

CREATE TRIGGER launched_chair_bridge_retirement_immutable_update
BEFORE UPDATE ON launched_chair_bridge_retirements
BEGIN SELECT RAISE(ABORT,'INVARIANT_launched_chair_bridge_retirement_immutable'); END;

CREATE TRIGGER launched_chair_bridge_retirement_insert_guard
BEFORE INSERT ON launched_chair_bridge_retirements
WHEN NOT EXISTS (
  SELECT 1
    FROM launched_chair_bridge_state bridge
    JOIN runs run ON run.project_session_id=bridge.project_session_id
                 AND run.run_id=bridge.coordination_run_id
    JOIN project_sessions session ON session.project_session_id=bridge.project_session_id
    JOIN run_chair_leases lease
      ON lease.project_session_id=run.project_session_id
     AND lease.run_id=run.run_id
     AND lease.lease_id=run.chair_lease_id
     AND lease.generation=run.chair_generation
    JOIN capabilities capability ON capability.token_hash=bridge.capability_hash
    JOIN agents agent ON agent.run_id=bridge.coordination_run_id
                     AND agent.agent_id=bridge.chair_agent_id
   WHERE bridge.project_session_id=NEW.project_session_id
     AND bridge.coordination_run_id=NEW.coordination_run_id
     AND bridge.state IN ('active','abandoned')
     AND run.lifecycle_state IN ('closed','cancelled','launch_failed')
     AND session.state IN ('closed','cancelled')
     AND session.terminal_path_json=NEW.terminal_ref
     AND json_valid(session.terminal_path_json)=1
     AND json_extract(session.terminal_path_json,'$.kind')=NEW.terminal_kind
     AND run.chair_agent_id=bridge.chair_agent_id
     AND lease.holder_agent_id=bridge.chair_agent_id
     AND lease.status='revoked'
     AND capability.revoked_at IS NOT NULL
     AND agent.lifecycle='archived'
     AND (
       (NEW.source_kind='project-session-close' AND EXISTS (
         SELECT 1 FROM operator_commands command
         WHERE command.project_session_id=NEW.project_session_id
            AND command.operator_id=NEW.owner_operator_id
            AND command.command_id=NEW.owner_ref
            AND command.operation='decide' AND command.status='committed'
            AND json_valid(command.result_json)=1
            AND json_extract(command.result_json,'$.projectSessionId')=NEW.project_session_id
            AND json_extract(command.result_json,'$.terminalPath.kind')=NEW.terminal_kind
       )) OR
       (NEW.source_kind='project-session-stop' AND EXISTS (
         SELECT 1 FROM operator_effect_custody custody
         WHERE custody.project_session_id=NEW.project_session_id
            AND custody.operator_id=NEW.owner_operator_id
            AND custody.command_id=NEW.owner_ref
            AND custody.operation='project-session-stop'
            AND custody.state IN ('dispatching','terminal')
            AND json_valid(custody.intent_json)=1
            AND json_extract(custody.intent_json,'$.kind')='project-session-stop'
            AND json_extract(custody.intent_json,'$.projectSessionId')=NEW.project_session_id
       )) OR
       (NEW.source_kind='chair-recovery-abandon' AND EXISTS (
         SELECT 1 FROM chair_bridge_recovery_custody recovery
          JOIN chair_bridge_losses loss ON loss.loss_id=recovery.loss_id
          WHERE recovery.recovery_id=NEW.owner_ref AND recovery.path='abandon'
            AND recovery.operator_id=NEW.owner_operator_id
            AND recovery.state IN ('committing','terminal')
            AND loss.project_session_id=NEW.project_session_id
            AND loss.coordination_run_id=NEW.coordination_run_id
       ))
     )
)
BEGIN SELECT RAISE(ABORT,'INVARIANT_launched_chair_bridge_retirement_proof'); END;

CREATE TRIGGER launched_chair_bridge_state_cas
BEFORE UPDATE OF state, revision ON launched_chair_bridge_state
WHEN NOT (
  (OLD.state='active' AND NEW.state='lost' AND NEW.revision=OLD.revision+1) OR
  (OLD.state='lost' AND NEW.state IN ('active','abandoned') AND NEW.revision=OLD.revision+1
    AND EXISTS (
      SELECT 1 FROM chair_bridge_recovery_custody recovery
      JOIN chair_bridge_losses loss ON loss.loss_id=recovery.loss_id
      WHERE recovery.state='committing'
        AND loss.project_session_id=OLD.project_session_id
        AND loss.coordination_run_id=OLD.coordination_run_id
        AND ((recovery.path IN ('rebind','takeover') AND NEW.state='active')
          OR (recovery.path='abandon' AND NEW.state='abandoned'))
    )) OR
  (OLD.state='active' AND NEW.state='active' AND NEW.revision=OLD.revision+1
    AND EXISTS (
      SELECT 1 FROM chair_live_handoff_custody custody
       WHERE custody.state='committing'
         AND custody.project_session_id=OLD.project_session_id
         AND custody.coordination_run_id=OLD.coordination_run_id
    )) OR
  (OLD.state='active' AND NEW.state='active' AND NEW.revision=OLD.revision+1
    AND EXISTS (
      SELECT 1
        FROM lifecycle_rotation_custodies custody
        JOIN lifecycle_rotation_custody_heads head
          ON head.run_id=custody.run_id AND head.agent_id=custody.agent_id
         AND head.custody_id=custody.custody_id
       WHERE custody.bridge_owner_kind='chair' AND head.state='committing' AND head.terminal=0
         AND custody.project_session_id=OLD.project_session_id
         AND custody.run_id=OLD.coordination_run_id AND custody.agent_id=OLD.chair_agent_id
         AND custody.source_adapter_id=OLD.provider_adapter_id
         AND custody.source_custody_action_id=OLD.provider_action_id
         AND custody.source_adapter_contract_digest=OLD.provider_contract_digest
         AND custody.source_provider_session_ref=OLD.provider_session_ref
         AND custody.source_provider_generation=OLD.provider_session_generation
         AND custody.source_principal_generation=OLD.principal_generation
         AND custody.source_bridge_generation=OLD.bridge_generation
         AND custody.source_bridge_revision=OLD.revision
         AND NEW.provider_adapter_id=custody.replacement_adapter_id
         AND NEW.provider_action_id=custody.provider_action_id
         AND NEW.provider_contract_digest=custody.replacement_contract_digest
         AND NEW.provider_session_generation=custody.target_provider_generation
         AND NEW.principal_generation=custody.target_principal_generation
         AND NEW.bridge_generation=custody.target_bridge_generation
         AND NEW.capability_hash=custody.staged_capability_hash
    ))
)
BEGIN SELECT RAISE(ABORT, 'INVARIANT_launched_chair_bridge_state_cas'); END;

CREATE TRIGGER leases_values_insert BEFORE INSERT ON leases BEGIN
  SELECT CASE WHEN NEW.kind <> 'write' OR NEW.status NOT IN ('active','quarantined','released') OR NEW.generation < 1
    THEN RAISE(ABORT, 'INVARIANT_leases_values') END;
  SELECT CASE WHEN NOT EXISTS (SELECT 1 FROM agents a WHERE a.agent_id=NEW.holder_agent_id AND a.run_id=NEW.run_id)
    THEN RAISE(ABORT, 'INVARIANT_leases_holder_same_run') END;
END;

CREATE TRIGGER leases_values_update BEFORE UPDATE OF kind,status,generation,holder_agent_id,run_id ON leases BEGIN
  SELECT CASE WHEN NEW.kind <> 'write' OR NEW.status NOT IN ('active','quarantined','released') OR NEW.generation < 1
    THEN RAISE(ABORT, 'INVARIANT_leases_values') END;
  SELECT CASE WHEN NOT EXISTS (SELECT 1 FROM agents a WHERE a.agent_id=NEW.holder_agent_id AND a.run_id=NEW.run_id)
    THEN RAISE(ABORT, 'INVARIANT_leases_holder_same_run') END;
END;

CREATE TRIGGER write_lease_lifecycle_custody_owner_update_guard
BEFORE UPDATE ON leases
WHEN EXISTS (
  SELECT 1
    FROM lifecycle_custody_write_leases ownership
    JOIN lifecycle_rotation_custody_heads head
      ON head.run_id=ownership.run_id AND head.agent_id=ownership.agent_id
     AND head.custody_id=ownership.custody_id
   WHERE ownership.lease_id=OLD.lease_id
     AND ownership.lease_generation=OLD.generation
     AND ownership.active_owner=1
)
AND NOT (
  OLD.status='quarantined' AND NEW.status='active'
  AND NEW.lease_id=OLD.lease_id AND NEW.run_id=OLD.run_id
  AND NEW.kind=OLD.kind AND NEW.holder_agent_id=OLD.holder_agent_id
  AND NEW.generation=OLD.generation AND NEW.expires_at=OLD.expires_at
  AND EXISTS (
    SELECT 1
      FROM lifecycle_custody_write_leases ownership
      JOIN lifecycle_rotation_custody_heads head
        ON head.run_id=ownership.run_id AND head.agent_id=ownership.agent_id
       AND head.custody_id=ownership.custody_id
     WHERE ownership.lease_id=OLD.lease_id
       AND ownership.lease_generation=OLD.generation
       AND ownership.active_owner=1 AND head.terminal=1
       AND head.disposition_code IN ('no-effect','superseded')
  )
)
BEGIN SELECT RAISE(ABORT,'INVARIANT_write_lease_lifecycle_custody_owner'); END;
CREATE TRIGGER write_lease_lifecycle_custody_owner_delete_guard
BEFORE DELETE ON leases
WHEN EXISTS (
  SELECT 1
    FROM lifecycle_custody_write_leases ownership
    JOIN lifecycle_rotation_custody_heads head
      ON head.run_id=ownership.run_id AND head.agent_id=ownership.agent_id
     AND head.custody_id=ownership.custody_id
   WHERE ownership.lease_id=OLD.lease_id
     AND ownership.lease_generation=OLD.generation
     AND ownership.active_owner=1
)
BEGIN SELECT RAISE(ABORT,'INVARIANT_write_lease_lifecycle_custody_owner'); END;

CREATE TRIGGER membership_same_run_insert BEFORE INSERT ON project_session_memberships
WHEN NOT EXISTS (SELECT 1 FROM runs r WHERE r.project_session_id=NEW.project_session_id AND r.run_id=NEW.coordination_run_id)
BEGIN SELECT RAISE(ABORT, 'AFAB_0004_MEMBERSHIP_RUN'); END;

CREATE TRIGGER messages_values_insert BEFORE INSERT ON messages BEGIN
  SELECT CASE WHEN NEW.requires_ack NOT IN (0,1) THEN RAISE(ABORT, 'INVARIANT_messages_requires_ack') END;
  SELECT CASE WHEN NOT EXISTS (SELECT 1 FROM agents a WHERE a.agent_id=NEW.sender_id AND a.run_id=NEW.run_id)
    THEN RAISE(ABORT, 'INVARIANT_messages_sender_same_run') END;
  SELECT CASE WHEN NEW.reply_to_message_id IS NOT NULL AND NOT EXISTS (SELECT 1 FROM messages m WHERE m.message_id=NEW.reply_to_message_id AND m.run_id=NEW.run_id)
    THEN RAISE(ABORT, 'INVARIANT_messages_reply_same_run') END;
END;

CREATE TRIGGER messages_values_update BEFORE UPDATE OF requires_ack,sender_id,reply_to_message_id,run_id ON messages BEGIN
  SELECT CASE WHEN NEW.requires_ack NOT IN (0,1) THEN RAISE(ABORT, 'INVARIANT_messages_requires_ack') END;
  SELECT CASE WHEN NOT EXISTS (SELECT 1 FROM agents a WHERE a.agent_id=NEW.sender_id AND a.run_id=NEW.run_id)
    THEN RAISE(ABORT, 'INVARIANT_messages_sender_same_run') END;
  SELECT CASE WHEN NEW.reply_to_message_id IS NOT NULL AND NOT EXISTS (SELECT 1 FROM messages m WHERE m.message_id=NEW.reply_to_message_id AND m.run_id=NEW.run_id)
    THEN RAISE(ABORT, 'INVARIANT_messages_reply_same_run') END;
END;

CREATE TRIGGER notification_cannot_mutate_attention BEFORE UPDATE OF state ON attention_items
WHEN NEW.state<>OLD.state AND EXISTS (
  SELECT 1 FROM notification_deliveries d WHERE d.item_id=OLD.item_id AND d.state='pending'
)
BEGIN SELECT RAISE(ABORT, 'AFAB_0004_NOTIFICATION_NOT_AUTHORITY'); END;

CREATE TRIGGER scoped_gate_terminal_settles_attention
AFTER UPDATE OF status ON scoped_gates
WHEN OLD.status IN ('pending','deferred')
 AND NEW.status IN ('approved','rejected','cancelled','superseded')
BEGIN
  UPDATE notification_deliveries
     SET state='deduplicated', updated_at=NEW.updated_at
   WHERE item_id=(
     SELECT item.item_id FROM attention_items item
      WHERE item.project_session_id=NEW.project_session_id
        AND item.coordination_run_id=NEW.coordination_run_id
        AND item.dedupe_key='scoped-gate:' || NEW.gate_id
        AND json_extract(item.payload_json,'$.gateId')=NEW.gate_id
   ) AND state='pending';
  UPDATE attention_items
     SET state=CASE WHEN NEW.status IN ('cancelled','superseded') THEN 'cancelled' ELSE 'resolved' END,
         revision=revision+1,
         updated_at=NEW.updated_at
   WHERE project_session_id=NEW.project_session_id
     AND coordination_run_id=NEW.coordination_run_id
     AND dedupe_key='scoped-gate:' || NEW.gate_id
     AND json_extract(payload_json,'$.gateId')=NEW.gate_id
     AND state IN ('open','acknowledged');
  SELECT CASE WHEN changes()<>1
    THEN RAISE(ABORT,'AFAB_0005_GATE_ATTENTION_MISSING') END;
END;

CREATE TRIGGER objective_check_status_insert BEFORE INSERT ON task_objective_checks
WHEN NEW.status NOT IN ('pending','pass','fail') BEGIN SELECT RAISE(ABORT, 'INVARIANT_objective_check_status'); END;

CREATE TRIGGER objective_check_status_update BEFORE UPDATE OF status ON task_objective_checks
WHEN NEW.status NOT IN ('pending','pass','fail') BEGIN SELECT RAISE(ABORT, 'INVARIANT_objective_check_status'); END;

CREATE TRIGGER operation_gate_block BEFORE UPDATE OF state ON operation_admissions
WHEN NEW.state IN ('authorised','executing') AND EXISTS (
  SELECT 1 FROM scoped_gates g JOIN scoped_gate_operations go ON go.gate_id=g.gate_id
   WHERE go.operation_id=NEW.operation_id AND g.project_session_id=NEW.project_session_id
     AND g.coordination_run_id=NEW.coordination_run_id AND g.status IN ('pending','deferred')
)
BEGIN SELECT RAISE(ABORT,'AFAB_0012_GATE_BLOCKED'); END;

CREATE TRIGGER operator_capability_bounds_insert BEFORE INSERT ON operator_capabilities BEGIN
  SELECT CASE WHEN NOT EXISTS (
    SELECT 1 FROM operator_principals p JOIN projects j ON j.project_id=p.project_id
     WHERE p.operator_id=NEW.operator_id AND p.project_id=NEW.project_id AND p.state='active'
       AND p.principal_generation=NEW.principal_generation
       AND p.project_authority_generation=NEW.project_authority_generation
       AND j.authority_generation=NEW.project_authority_generation
  ) THEN RAISE(ABORT, 'AFAB_0004_CAPABILITY_PRINCIPAL') END;
  SELECT CASE WHEN (NEW.kind='project-launch' AND (NEW.project_session_id IS NOT NULL OR NEW.session_generation IS NOT NULL)) OR
                        (NEW.kind<>'project-launch' AND (NEW.project_session_id IS NULL OR NEW.session_generation IS NULL))
    THEN RAISE(ABORT, 'AFAB_0004_CAPABILITY_SCOPE') END;
  SELECT CASE WHEN NEW.project_session_id IS NOT NULL AND NOT EXISTS (
    SELECT 1 FROM project_sessions s WHERE s.project_session_id=NEW.project_session_id
      AND s.project_id=NEW.project_id AND s.generation=NEW.session_generation
  ) THEN RAISE(ABORT, 'AFAB_0004_CAPABILITY_GENERATION') END;
END;

CREATE TRIGGER operator_effect_git_nonterminal_requires_four_owner_map
BEFORE UPDATE OF state ON operator_effect_custody
WHEN NEW.state IN ('conflict','quarantined')
BEGIN
  SELECT CASE WHEN NOT EXISTS (
    SELECT 1 FROM operator_git_effect_bindings binding
    JOIN operation_admissions admission ON admission.operation_id=binding.operation_id
    JOIN git_mutation_reservations reservation
      ON reservation.custody_id=binding.custody_id
     AND reservation.generation=binding.mutation_reservation_generation
    WHERE binding.custody_id=NEW.custody_id
      AND binding.state=NEW.state
      AND admission.state=NEW.state
      AND reservation.state=NEW.state
  ) THEN RAISE(ABORT,'INVARIANT_git_four_owner_map') END;
END;

CREATE TRIGGER operator_external_effect_binding_delete_forbidden
BEFORE DELETE ON operator_external_effect_bindings
BEGIN SELECT RAISE(ABORT,'INVARIANT_external_effect_binding_immutable'); END;

CREATE TRIGGER operator_external_effect_binding_identity_immutable
BEFORE UPDATE OF custody_id,effect_kind,integration_id,integration_generation,operation_id,
  contract_digest,target_id,target_revision,request_artifact_path,request_artifact_digest,
  idempotency_key,release_gate_id,release_gate_revision,release_binding_digest,created_at
ON operator_external_effect_bindings
BEGIN SELECT RAISE(ABORT,'INVARIANT_external_effect_binding_immutable'); END;

CREATE TRIGGER operator_external_effect_binding_insert_guard
BEFORE INSERT ON operator_external_effect_bindings
BEGIN
  SELECT CASE WHEN NOT EXISTS (
    SELECT 1 FROM operator_effect_custody custody
     WHERE custody.custody_id=NEW.custody_id
       AND custody.state='prepared'
       AND custody.operation='external-effect'
       AND json_valid(custody.intent_json)=1
       AND json_extract(custody.intent_json,'$.kind')=NEW.effect_kind
  ) THEN RAISE(ABORT,'INVARIANT_external_effect_parent') END;

  SELECT CASE WHEN NEW.effect_kind='registered-external-effect' AND NOT EXISTS (
    SELECT 1 FROM operator_effect_custody custody
     WHERE custody.custody_id=NEW.custody_id
       AND json_extract(custody.intent_json,'$.integrationId')=NEW.integration_id
       AND json_extract(custody.intent_json,'$.expectedIntegrationGeneration')=NEW.integration_generation
       AND json_extract(custody.intent_json,'$.operationId')=NEW.operation_id
       AND json_extract(custody.intent_json,'$.contractDigest')=NEW.contract_digest
       AND json_extract(custody.intent_json,'$.targetId')=NEW.target_id
       AND json_extract(custody.intent_json,'$.expectedTargetRevision')=NEW.target_revision
       AND json_extract(custody.intent_json,'$.requestArtifactRef.path')=NEW.request_artifact_path
       AND json_extract(custody.intent_json,'$.requestArtifactRef.digest')=NEW.request_artifact_digest
       AND json_extract(custody.intent_json,'$.idempotencyKey')=NEW.idempotency_key
  ) THEN RAISE(ABORT,'INVARIANT_external_effect_intent_binding') END;

  SELECT CASE WHEN NEW.effect_kind='promotion' AND NOT EXISTS (
    SELECT 1 FROM operator_effect_custody custody
    JOIN scoped_gates gate ON gate.gate_id=NEW.release_gate_id
     WHERE custody.custody_id=NEW.custody_id
       AND gate.project_session_id=custody.project_session_id
       AND gate.scope_kind='release'
       AND gate.status='approved'
       AND gate.revision=NEW.release_gate_revision
       AND json_extract(custody.intent_json,'$.gateId')=NEW.release_gate_id
       AND json_extract(custody.intent_json,'$.expectedGateRevision')=NEW.release_gate_revision
       AND json_extract(custody.intent_json,'$.releaseBinding.acceptedDeliveryReceiptRef.path')=NEW.request_artifact_path
       AND json_extract(custody.intent_json,'$.releaseBinding.acceptedDeliveryReceiptRef.digest')=NEW.request_artifact_digest
       AND json_extract(custody.intent_json,'$.releaseBinding.promotionAction')=NEW.operation_id
       AND json_extract(custody.intent_json,'$.releaseBinding.target')=NEW.target_id
       AND json_extract(gate.release_binding_json,'$.acceptedDeliveryReceiptRef.path')=NEW.request_artifact_path
       AND json_extract(gate.release_binding_json,'$.acceptedDeliveryReceiptRef.digest')=NEW.request_artifact_digest
       AND json_extract(gate.release_binding_json,'$.promotionAction')=NEW.operation_id
       AND json_extract(gate.release_binding_json,'$.target')=NEW.target_id
  ) THEN RAISE(ABORT,'INVARIANT_promotion_release_binding') END;
END;

CREATE TRIGGER operator_external_effect_binding_lookup_cas
BEFORE UPDATE OF lookup_generation,lookup_evidence_digest ON operator_external_effect_bindings
WHEN NEW.lookup_generation<>OLD.lookup_generation+1 OR NEW.lookup_evidence_digest IS NULL
BEGIN SELECT RAISE(ABORT,'INVARIANT_external_effect_lookup_cas'); END;

CREATE TRIGGER operator_external_effect_requires_typed_binding
BEFORE UPDATE OF state ON operator_effect_custody
WHEN json_valid(OLD.intent_json)=1
 AND json_extract(OLD.intent_json,'$.kind') IN ('registered-external-effect','promotion')
 AND NOT EXISTS(SELECT 1 FROM operator_external_effect_bindings b WHERE b.custody_id=OLD.custody_id)
BEGIN SELECT RAISE(ABORT,'INVARIANT_external_effect_binding_required'); END;

CREATE TRIGGER provider_actions_values_insert BEFORE INSERT ON provider_actions BEGIN
  SELECT CASE WHEN NEW.status NOT IN ('prepared','dispatched','accepted','terminal','ambiguous','quarantined') OR NEW.execution_count < 0 OR NEW.effect_count < 0 OR NEW.idempotency_proven NOT IN (0,1) OR (NEW.provider_session_generation IS NOT NULL AND NEW.provider_session_generation < 1) OR (NEW.turn_lease_generation IS NOT NULL AND NEW.turn_lease_generation < 1)
    THEN RAISE(ABORT, 'INVARIANT_provider_actions_values') END;
  SELECT CASE WHEN NEW.operation NOT IN ('spawn','attach') AND NEW.target_agent_id IS NOT NULL AND NOT EXISTS (SELECT 1 FROM agents a WHERE a.agent_id=NEW.target_agent_id AND a.run_id=NEW.run_id)
    THEN RAISE(ABORT, 'INVARIANT_provider_actions_target_same_run') END;
  SELECT CASE WHEN NEW.budget_authority_id IS NOT NULL AND NOT EXISTS (
    SELECT 1 FROM authorities authority
     WHERE authority.authority_id=NEW.budget_authority_id AND authority.run_id=NEW.run_id
  ) THEN RAISE(ABORT, 'INVARIANT_provider_actions_budget_authority_same_run') END;
  SELECT CASE WHEN NEW.budget_authority_id IS NOT NULL AND (
    NEW.budget_state<>'reserved' OR json_type(NEW.budget_reservation_json)<>'object'
    OR NOT EXISTS (SELECT 1 FROM json_each(NEW.budget_reservation_json))
    OR COALESCE(json_type(NEW.payload_json,'$.maxTurns'),'')<>'integer'
    OR json_extract(NEW.payload_json,'$.maxTurns')<1
    OR json_extract(NEW.payload_json,'$.maxTurns')>9007199254740991
    OR COALESCE(json_type(NEW.budget_reservation_json,'$.turns'),'')<>'integer'
    OR json_extract(NEW.budget_reservation_json,'$.turns')>9007199254740991
    OR json_extract(NEW.budget_reservation_json,'$.turns')<>json_extract(NEW.payload_json,'$.maxTurns')
    OR EXISTS (
      SELECT 1 FROM json_each(NEW.budget_reservation_json) reservation
       LEFT JOIN authority_budget budget
         ON budget.authority_id=NEW.budget_authority_id AND budget.unit_key=reservation.key
      WHERE reservation.type<>'integer' OR reservation.value<1
         OR budget.authority_id IS NULL OR budget.usage_unknown<>0
         OR budget.granted-budget.reserved-budget.consumed<reservation.value
         OR NOT (
           reservation.key IN ('turns','provider_calls','concurrent_turns','wall_clock_milliseconds')
           OR reservation.key GLOB 'cost:[A-Z][A-Z][A-Z]'
           OR reservation.key GLOB 'input_tokens:[a-z0-9]*'
           OR reservation.key GLOB 'output_tokens:[a-z0-9]*'
         )
    )
  ) THEN RAISE(ABORT, 'INVARIANT_provider_actions_budget_reservation') END;
  SELECT CASE WHEN NEW.budget_authority_id IS NOT NULL AND NOT EXISTS (
    SELECT 1 FROM tasks task
     WHERE task.run_id=NEW.run_id AND task.task_id=NEW.task_id
       AND task.state NOT IN ('complete','cancelled','degraded')
  ) THEN RAISE(ABORT, 'INVARIANT_provider_actions_task_active') END;
END;

CREATE TRIGGER provider_actions_budget_reserve
AFTER INSERT ON provider_actions
WHEN NEW.budget_state='reserved'
BEGIN
  UPDATE authority_budget
     SET reserved=reserved+(
           SELECT reservation.value FROM json_each(NEW.budget_reservation_json) reservation
            WHERE reservation.key=authority_budget.unit_key
         ),
         provider_reserved=provider_reserved+(
           SELECT reservation.value FROM json_each(NEW.budget_reservation_json) reservation
            WHERE reservation.key=authority_budget.unit_key
         )
   WHERE authority_id=NEW.budget_authority_id
     AND unit_key IN (SELECT reservation.key FROM json_each(NEW.budget_reservation_json) reservation);
END;

CREATE TRIGGER provider_actions_values_update BEFORE UPDATE OF status,execution_count,effect_count,idempotency_proven,provider_session_generation,turn_lease_generation,target_agent_id,run_id,budget_authority_id ON provider_actions BEGIN
  SELECT CASE WHEN NEW.status NOT IN ('prepared','dispatched','accepted','terminal','ambiguous','quarantined') OR NEW.execution_count < 0 OR NEW.effect_count < 0 OR NEW.idempotency_proven NOT IN (0,1) OR (NEW.provider_session_generation IS NOT NULL AND NEW.provider_session_generation < 1) OR (NEW.turn_lease_generation IS NOT NULL AND NEW.turn_lease_generation < 1)
    THEN RAISE(ABORT, 'INVARIANT_provider_actions_values') END;
  SELECT CASE WHEN NEW.operation NOT IN ('spawn','attach') AND NEW.target_agent_id IS NOT NULL AND NOT EXISTS (SELECT 1 FROM agents a WHERE a.agent_id=NEW.target_agent_id AND a.run_id=NEW.run_id)
    THEN RAISE(ABORT, 'INVARIANT_provider_actions_target_same_run') END;
  SELECT CASE WHEN NEW.budget_authority_id IS NOT NULL AND NOT EXISTS (
    SELECT 1 FROM authorities authority
     WHERE authority.authority_id=NEW.budget_authority_id AND authority.run_id=NEW.run_id
  ) THEN RAISE(ABORT, 'INVARIANT_provider_actions_budget_authority_same_run') END;
END;

CREATE TRIGGER provider_actions_budget_binding_immutable
BEFORE UPDATE OF task_id,budget_authority_id,budget_reservation_json,budget_started_at ON provider_actions
WHEN NEW.budget_authority_id IS NOT OLD.budget_authority_id
  OR NEW.task_id IS NOT OLD.task_id
  OR NEW.budget_reservation_json IS NOT OLD.budget_reservation_json
  OR NEW.budget_started_at IS NOT OLD.budget_started_at
BEGIN SELECT RAISE(ABORT, 'INVARIANT_provider_actions_budget_binding_immutable'); END;

CREATE TRIGGER provider_actions_budget_state_cas
BEFORE UPDATE OF budget_state,budget_settlement_json,status ON provider_actions
WHEN NEW.budget_state IS NOT OLD.budget_state
  OR NEW.budget_settlement_json IS NOT OLD.budget_settlement_json
BEGIN
  SELECT CASE WHEN NOT (
    (OLD.budget_state='reserved' AND NEW.budget_state IN ('settled','usage-unknown'))
    OR (OLD.budget_state='usage-unknown' AND NEW.budget_state='usage-unknown')
    OR (OLD.budget_state='usage-unknown' AND NEW.budget_state='settled')
  ) THEN RAISE(ABORT, 'INVARIANT_provider_actions_budget_state_cas') END;
  SELECT CASE WHEN json_type(NEW.budget_settlement_json)<>'object'
    OR (SELECT COUNT(*) FROM json_each(NEW.budget_settlement_json))<>
       (SELECT COUNT(*) FROM json_each(OLD.budget_reservation_json))
    OR EXISTS (
      SELECT 1 FROM json_each(OLD.budget_reservation_json) reservation
       LEFT JOIN json_each(NEW.budget_settlement_json) settlement ON settlement.key=reservation.key
      WHERE settlement.key IS NULL
         OR NOT (
           (settlement.type='integer' AND settlement.value BETWEEN 0 AND reservation.value)
           OR (settlement.type='text' AND settlement.value='unknown')
         )
    )
    OR EXISTS (
      SELECT 1 FROM json_each(NEW.budget_settlement_json) settlement
       LEFT JOIN json_each(OLD.budget_reservation_json) reservation ON reservation.key=settlement.key
      WHERE reservation.key IS NULL
    )
    OR (NEW.budget_state='settled' AND EXISTS (
      SELECT 1 FROM json_each(NEW.budget_settlement_json) WHERE type<>'integer'
    ))
    OR (NEW.budget_state='usage-unknown' AND NOT EXISTS (
      SELECT 1 FROM json_each(NEW.budget_settlement_json) WHERE type='text' AND value='unknown'
    ))
    OR (OLD.budget_state='usage-unknown' AND EXISTS (
      SELECT 1 FROM json_each(OLD.budget_settlement_json) prior
       JOIN json_each(NEW.budget_settlement_json) current ON current.key=prior.key
      WHERE prior.type='integer' AND (current.type<>'integer' OR current.value<>prior.value)
    ))
  THEN RAISE(ABORT, 'INVARIANT_provider_actions_budget_settlement') END;
END;

CREATE TRIGGER provider_actions_budget_settle
AFTER UPDATE OF budget_state,budget_settlement_json ON provider_actions
WHEN NEW.budget_state IS NOT OLD.budget_state
  OR NEW.budget_settlement_json IS NOT OLD.budget_settlement_json
BEGIN
  UPDATE authority_budget
     SET reserved=reserved-CASE WHEN (
           SELECT settlement.type='integer' AND (
             OLD.budget_state='reserved' OR prior.type='text'
           )
             FROM json_each(NEW.budget_settlement_json) settlement
             LEFT JOIN json_each(OLD.budget_settlement_json) prior ON prior.key=settlement.key
            WHERE settlement.key=authority_budget.unit_key
         ) THEN (
           SELECT reservation.value FROM json_each(OLD.budget_reservation_json) reservation
            WHERE reservation.key=authority_budget.unit_key
         ) ELSE 0 END,
         provider_reserved=provider_reserved-CASE WHEN (
           SELECT settlement.type='integer' AND (
             OLD.budget_state='reserved' OR prior.type='text'
           )
             FROM json_each(NEW.budget_settlement_json) settlement
             LEFT JOIN json_each(OLD.budget_settlement_json) prior ON prior.key=settlement.key
            WHERE settlement.key=authority_budget.unit_key
         ) THEN (
           SELECT reservation.value FROM json_each(OLD.budget_reservation_json) reservation
            WHERE reservation.key=authority_budget.unit_key
         ) ELSE 0 END,
         consumed=consumed+CASE WHEN (
           SELECT settlement.type='integer' AND (
             OLD.budget_state='reserved' OR prior.type='text'
           )
             FROM json_each(NEW.budget_settlement_json) settlement
             LEFT JOIN json_each(OLD.budget_settlement_json) prior ON prior.key=settlement.key
            WHERE settlement.key=authority_budget.unit_key
         ) THEN (
           SELECT settlement.value FROM json_each(NEW.budget_settlement_json) settlement
           WHERE settlement.key=authority_budget.unit_key
         ) ELSE 0 END,
         provider_consumed=provider_consumed+CASE WHEN (
           SELECT settlement.type='integer' AND (
             OLD.budget_state='reserved' OR prior.type='text'
           )
             FROM json_each(NEW.budget_settlement_json) settlement
             LEFT JOIN json_each(OLD.budget_settlement_json) prior ON prior.key=settlement.key
            WHERE settlement.key=authority_budget.unit_key
         ) THEN (
           SELECT settlement.value FROM json_each(NEW.budget_settlement_json) settlement
            WHERE settlement.key=authority_budget.unit_key
         ) ELSE 0 END,
         usage_unknown=CASE
           WHEN (SELECT settlement.type='text' FROM json_each(NEW.budget_settlement_json) settlement
                  WHERE settlement.key=authority_budget.unit_key) THEN 1
           ELSE CASE WHEN EXISTS (
             SELECT 1 FROM provider_actions action
              JOIN json_each(action.budget_settlement_json) settlement
                ON settlement.key=authority_budget.unit_key
             WHERE action.budget_authority_id=NEW.budget_authority_id
               AND action.budget_state='usage-unknown'
               AND settlement.type='text' AND settlement.value='unknown'
           ) THEN 1 ELSE 0 END
         END
   WHERE authority_id=NEW.budget_authority_id
     AND unit_key IN (SELECT reservation.key FROM json_each(OLD.budget_reservation_json) reservation);
END;

CREATE TRIGGER provider_actions_budget_delete_forbidden
BEFORE DELETE ON provider_actions
WHEN OLD.budget_authority_id IS NOT NULL
BEGIN SELECT RAISE(ABORT, 'INVARIANT_provider_actions_budget_binding_immutable'); END;

CREATE TRIGGER authority_budget_provider_ledger_update
AFTER UPDATE OF reserved,provider_reserved,consumed,provider_consumed,usage_unknown ON authority_budget
WHEN NEW.provider_reserved<>(
  SELECT COALESCE(SUM(reservation.value),0)
    FROM provider_actions action
    JOIN json_each(action.budget_reservation_json) reservation ON reservation.key=NEW.unit_key
    LEFT JOIN json_each(action.budget_settlement_json) settlement ON settlement.key=reservation.key
   WHERE action.budget_authority_id=NEW.authority_id
     AND (
       action.budget_state='reserved'
       OR (action.budget_state='usage-unknown' AND settlement.type='text' AND settlement.value='unknown')
     )
) OR NEW.provider_consumed<>(
  SELECT COALESCE(SUM(settlement.value),0)
    FROM provider_actions action
    JOIN json_each(action.budget_settlement_json) settlement ON settlement.key=NEW.unit_key
   WHERE action.budget_authority_id=NEW.authority_id
     AND settlement.type='integer'
     AND action.budget_state IN ('settled','usage-unknown')
) OR (NEW.usage_unknown=0 AND EXISTS (
  SELECT 1 FROM provider_actions action
  JOIN json_each(action.budget_settlement_json) settlement ON settlement.key=NEW.unit_key
   WHERE action.budget_authority_id=NEW.authority_id
     AND action.budget_state='usage-unknown'
     AND settlement.type='text' AND settlement.value='unknown'
))
BEGIN SELECT RAISE(ABORT, 'INVARIANT_authority_budget_provider_ledger'); END;

CREATE TRIGGER task_provider_action_unresolved
BEFORE UPDATE OF state ON tasks
WHEN NEW.state IN ('complete','cancelled','degraded') AND EXISTS (
  SELECT 1 FROM provider_actions action
   WHERE action.run_id=NEW.run_id AND action.task_id=NEW.task_id
     AND action.status IN ('prepared','dispatched','accepted','ambiguous')
)
BEGIN SELECT RAISE(ABORT, 'INVARIANT_task_provider_action_unresolved'); END;

CREATE TRIGGER provider_agent_custody_immutable_delete
BEFORE DELETE ON provider_agent_custody
BEGIN
  SELECT RAISE(ABORT, 'INVARIANT_provider_agent_custody_immutable');
END;

CREATE TRIGGER provider_agent_custody_immutable_update
BEFORE UPDATE ON provider_agent_custody
BEGIN
  SELECT RAISE(ABORT, 'INVARIANT_provider_agent_custody_immutable');
END;

CREATE TRIGGER provider_state_generation_insert BEFORE INSERT ON provider_state
WHEN NEW.provider_session_generation < 1 BEGIN SELECT RAISE(ABORT, 'INVARIANT_provider_state_generation'); END;

CREATE TRIGGER provider_state_generation_update BEFORE UPDATE OF provider_session_generation ON provider_state
WHEN NEW.provider_session_generation < 1 BEGIN SELECT RAISE(ABORT, 'INVARIANT_provider_state_generation'); END;

CREATE TRIGGER ps_awaiting_acceptance BEFORE UPDATE OF state ON project_sessions
WHEN NEW.state='awaiting_acceptance' AND (
  EXISTS (SELECT 1 FROM project_session_memberships m WHERE m.project_session_id=NEW.project_session_id AND m.required=1 AND m.state='active') OR
  EXISTS (SELECT 1 FROM runs r WHERE r.project_session_id=NEW.project_session_id AND r.lifecycle_state NOT IN ('awaiting_acceptance','closed','cancelled','launch_failed')) OR
  EXISTS (SELECT 1 FROM result_deliveries d WHERE d.project_session_id=NEW.project_session_id AND d.required=1 AND d.state NOT IN ('consumed','abandoned'))
)
BEGIN SELECT RAISE(ABORT, 'AFAB_0004_SESSION_CLOSURE_BLOCKED'); END;

CREATE TRIGGER ps_generation_step BEFORE UPDATE OF generation ON project_sessions
WHEN NEW.generation NOT IN (OLD.generation, OLD.generation + 1)
BEGIN SELECT RAISE(ABORT, 'AFAB_0004_SESSION_GENERATION'); END;

CREATE TRIGGER ps_membership_frozen BEFORE INSERT ON project_session_memberships
WHEN (SELECT state FROM project_sessions WHERE project_session_id=NEW.project_session_id)
     IN ('quiescing','awaiting_acceptance','closed','cancelled')
BEGIN SELECT RAISE(ABORT,'AFAB_0004_MEMBERSHIP_FROZEN'); END;

CREATE TRIGGER ps_origin_update BEFORE UPDATE OF origin_kind,origin_operator_id ON project_sessions
BEGIN SELECT RAISE(ABORT, 'AFAB_0004_SESSION_ORIGIN_IMMUTABLE'); END;

CREATE TRIGGER ps_revision_step BEFORE UPDATE OF state,revision,authority_ref,budget_ref,launch_packet_path,launch_packet_digest,membership_revision ON project_sessions
WHEN NEW.revision <> OLD.revision + 1
BEGIN SELECT RAISE(ABORT, 'AFAB_0004_SESSION_REVISION'); END;

CREATE TRIGGER resource_dimension_update BEFORE UPDATE OF used,reserved,usage_unknown ON resource_dimensions
WHEN NEW.used<0 OR NEW.reserved<0 OR NEW.used+NEW.reserved>NEW.limit_value OR NEW.usage_unknown NOT IN (0,1)
BEGIN SELECT RAISE(ABORT, 'AFAB_0004_RESOURCE_BOUNDS'); END;

CREATE TRIGGER result_claim_generation BEFORE UPDATE OF claim_generation ON result_deliveries
WHEN NEW.claim_generation NOT IN (OLD.claim_generation, OLD.claim_generation+1)
BEGIN SELECT RAISE(ABORT, 'AFAB_0004_RESULT_CLAIM_GENERATION'); END;

CREATE TRIGGER result_delivery_transition BEFORE UPDATE OF state ON result_deliveries
WHEN NOT (
  (OLD.state='pending' AND NEW.state IN ('claimed','overdue','abandoned')) OR
  (OLD.state='claimed' AND NEW.state IN ('provider-accepted','pending','overdue','abandoned')) OR
  (OLD.state='provider-accepted' AND NEW.state IN ('consumed','overdue','abandoned')) OR
  (OLD.state='overdue' AND NEW.state IN ('pending','abandoned')) OR
  (OLD.state=NEW.state)
)
BEGIN SELECT RAISE(ABORT, 'AFAB_0004_RESULT_TRANSITION'); END;

CREATE TRIGGER run_authority_revision_delete_forbidden
BEFORE DELETE ON run_authority_revisions
BEGIN SELECT RAISE(ABORT,'INVARIANT_git_authority_history_immutable'); END;

CREATE TRIGGER run_authority_revision_immutable
BEFORE UPDATE ON run_authority_revisions
BEGIN SELECT RAISE(ABORT,'INVARIANT_git_authority_history_immutable'); END;

CREATE TRIGGER run_chair_generation_step BEFORE UPDATE OF chair_generation ON runs
WHEN NEW.chair_generation NOT IN (OLD.chair_generation, OLD.chair_generation + 1)
BEGIN SELECT RAISE(ABORT, 'AFAB_0004_RUN_CHAIR_GENERATION'); END;

CREATE TRIGGER run_chair_lease_cross_owner_insert
BEFORE INSERT ON run_chair_leases
WHEN EXISTS (SELECT 1 FROM leases WHERE run_id=NEW.run_id AND lease_id=NEW.lease_id)
  OR EXISTS (SELECT 1 FROM task_owner_leases WHERE run_id=NEW.run_id AND lease_id=NEW.lease_id)
BEGIN SELECT RAISE(ABORT,'INVARIANT_lease_identity_single_owner'); END;

CREATE TRIGGER run_chair_lease_cross_owner_update
BEFORE UPDATE OF project_session_id,run_id,lease_id ON run_chair_leases
WHEN EXISTS (SELECT 1 FROM leases WHERE run_id=NEW.run_id AND lease_id=NEW.lease_id)
  OR EXISTS (SELECT 1 FROM task_owner_leases WHERE run_id=NEW.run_id AND lease_id=NEW.lease_id)
BEGIN SELECT RAISE(ABORT,'INVARIANT_lease_identity_single_owner'); END;

CREATE TRIGGER run_chair_lease_identity_immutable
BEFORE UPDATE OF project_session_id,run_id,lease_id,holder_agent_id,generation ON run_chair_leases
WHEN NEW.project_session_id<>OLD.project_session_id OR NEW.run_id<>OLD.run_id
  OR NEW.lease_id<>OLD.lease_id OR NEW.holder_agent_id<>OLD.holder_agent_id
  OR NEW.generation<>OLD.generation
BEGIN SELECT RAISE(ABORT,'INVARIANT_run_chair_lease_identity_immutable'); END;

CREATE TRIGGER run_git_allowlist_delete_forbidden
BEFORE DELETE ON run_git_allowlists
BEGIN SELECT RAISE(ABORT,'INVARIANT_run_git_allowlist_immutable'); END;

CREATE TRIGGER run_git_allowlist_identity_immutable
BEFORE UPDATE ON run_git_allowlists
BEGIN SELECT RAISE(ABORT,'INVARIANT_run_git_allowlist_immutable'); END;

CREATE TRIGGER run_git_allowlist_path_delete_forbidden
BEFORE DELETE ON run_git_allowlist_paths
BEGIN SELECT RAISE(ABORT,'INVARIANT_run_git_allowlist_child_immutable'); END;

CREATE TRIGGER run_git_allowlist_path_immutable
BEFORE UPDATE ON run_git_allowlist_paths
BEGIN SELECT RAISE(ABORT,'INVARIANT_run_git_allowlist_child_immutable'); END;

CREATE TRIGGER run_git_allowlist_profile_delete_forbidden
BEFORE DELETE ON run_git_allowlist_profiles
BEGIN SELECT RAISE(ABORT,'INVARIANT_run_git_allowlist_child_immutable'); END;

CREATE TRIGGER run_git_allowlist_profile_immutable
BEFORE UPDATE ON run_git_allowlist_profiles
BEGIN SELECT RAISE(ABORT,'INVARIANT_run_git_allowlist_child_immutable'); END;

CREATE TRIGGER run_git_allowlist_ref_delete_forbidden
BEFORE DELETE ON run_git_allowlist_refs
BEGIN SELECT RAISE(ABORT,'INVARIANT_run_git_allowlist_child_immutable'); END;

CREATE TRIGGER run_git_allowlist_ref_immutable
BEFORE UPDATE ON run_git_allowlist_refs
BEGIN SELECT RAISE(ABORT,'INVARIANT_run_git_allowlist_child_immutable'); END;

CREATE TRIGGER run_git_allowlist_remote_delete_forbidden
BEFORE DELETE ON run_git_allowlist_remotes
BEGIN SELECT RAISE(ABORT,'INVARIANT_run_git_allowlist_child_immutable'); END;

CREATE TRIGGER run_git_allowlist_remote_immutable
BEFORE UPDATE ON run_git_allowlist_remotes
BEGIN SELECT RAISE(ABORT,'INVARIANT_run_git_allowlist_child_immutable'); END;

CREATE TRIGGER run_git_allowlist_variant_delete_forbidden
BEFORE DELETE ON run_git_allowlist_variants
BEGIN SELECT RAISE(ABORT,'INVARIANT_run_git_allowlist_child_immutable'); END;

CREATE TRIGGER run_git_allowlist_variant_immutable
BEFORE UPDATE ON run_git_allowlist_variants
BEGIN SELECT RAISE(ABORT,'INVARIANT_run_git_allowlist_child_immutable'); END;

CREATE TRIGGER run_required_session_identity_update
BEFORE UPDATE OF project_session_id,lifecycle_state,revision,chair_generation,
  chair_lease_id,authority_ref,budget_ref,dependency_revision ON runs
WHEN NEW.project_session_id IS NULL OR NEW.lifecycle_state IS NULL OR NEW.revision IS NULL
  OR NEW.chair_generation IS NULL OR NEW.chair_lease_id IS NULL OR NEW.authority_ref IS NULL
  OR NEW.budget_ref IS NULL OR NEW.dependency_revision IS NULL
  OR NEW.project_session_id<>OLD.project_session_id
BEGIN SELECT RAISE(ABORT,'INVARIANT_run_required_session_identity'); END;

CREATE TRIGGER run_revision_step BEFORE UPDATE OF lifecycle_state,revision,chair_agent_id,chair_generation,chair_lease_id,authority_ref,budget_ref ON runs
WHEN NEW.revision <> OLD.revision + 1
BEGIN SELECT RAISE(ABORT, 'AFAB_0004_RUN_REVISION'); END;

CREATE TRIGGER run_session_insert BEFORE INSERT ON runs
WHEN NEW.project_session_id IS NULL OR NEW.lifecycle_state IS NULL OR NEW.revision IS NULL OR
     NEW.chair_generation IS NULL OR NEW.chair_lease_id IS NULL OR NEW.authority_ref IS NULL OR
     NEW.budget_ref IS NULL OR NEW.dependency_revision IS NULL
BEGIN SELECT RAISE(ABORT, 'AFAB_0004_RUN_SESSION_REQUIRED'); END;

CREATE TRIGGER run_topology_insert BEFORE INSERT ON runs BEGIN
  SELECT CASE WHEN
    ((SELECT mode FROM project_sessions WHERE project_session_id=NEW.project_session_id)='coordinated' AND NEW.topology_slot<>1) OR
    ((SELECT mode FROM project_sessions WHERE project_session_id=NEW.project_session_id)='independent' AND NEW.topology_slot IS NOT NULL)
    THEN RAISE(ABORT, 'AFAB_0004_RUN_TOPOLOGY') END;
END;

CREATE TRIGGER task_gate_readiness BEFORE UPDATE OF state ON tasks
WHEN NEW.state='active' AND EXISTS (
  SELECT 1 FROM scoped_gate_tasks gt JOIN scoped_gates g ON g.gate_id=gt.gate_id
   WHERE gt.run_id=NEW.run_id AND gt.task_id=NEW.task_id AND g.status IN ('pending','deferred')
     AND EXISTS (SELECT 1 FROM json_each(g.enforcement_points_json) WHERE value='task-readiness')
)
BEGIN SELECT RAISE(ABORT, 'AFAB_0004_GATE_BLOCKED'); END;

CREATE TRIGGER task_owner_lease_cross_owner_insert
BEFORE INSERT ON task_owner_leases
WHEN EXISTS (SELECT 1 FROM run_chair_leases WHERE run_id=NEW.run_id AND lease_id=NEW.lease_id)
  OR EXISTS (SELECT 1 FROM leases WHERE run_id=NEW.run_id AND lease_id=NEW.lease_id)
BEGIN SELECT RAISE(ABORT,'INVARIANT_lease_identity_single_owner'); END;

CREATE TRIGGER task_owner_lease_cross_owner_update
BEFORE UPDATE OF project_session_id,run_id,lease_id ON task_owner_leases
WHEN EXISTS (SELECT 1 FROM run_chair_leases WHERE run_id=NEW.run_id AND lease_id=NEW.lease_id)
  OR EXISTS (SELECT 1 FROM leases WHERE run_id=NEW.run_id AND lease_id=NEW.lease_id)
BEGIN SELECT RAISE(ABORT,'INVARIANT_lease_identity_single_owner'); END;

CREATE TRIGGER task_owner_lease_identity_immutable
BEFORE UPDATE OF project_session_id,run_id,task_id,lease_id,holder_agent_id,generation ON task_owner_leases
WHEN NEW.project_session_id<>OLD.project_session_id OR NEW.run_id<>OLD.run_id
  OR NEW.task_id<>OLD.task_id OR NEW.lease_id<>OLD.lease_id
  OR NEW.holder_agent_id<>OLD.holder_agent_id OR NEW.generation<>OLD.generation
BEGIN SELECT RAISE(ABORT,'INVARIANT_task_owner_lease_identity_immutable'); END;

CREATE TRIGGER tasks_values_insert BEFORE INSERT ON tasks BEGIN
  SELECT CASE WHEN NEW.state NOT IN ('blocked','ready','active','complete','cancelled','degraded') OR NEW.revision < 0 OR NEW.owner_lease_generation < 0
    THEN RAISE(ABORT, 'INVARIANT_tasks_values') END;
  SELECT CASE WHEN NOT EXISTS (SELECT 1 FROM authorities a WHERE a.authority_id=NEW.authority_id AND a.run_id=NEW.run_id)
    THEN RAISE(ABORT, 'INVARIANT_tasks_authority_same_run') END;
  SELECT CASE WHEN NEW.owner_agent_id IS NOT NULL AND NOT EXISTS (SELECT 1 FROM agents a WHERE a.agent_id=NEW.owner_agent_id AND a.run_id=NEW.run_id)
    THEN RAISE(ABORT, 'INVARIANT_tasks_owner_same_run') END;
  SELECT CASE WHEN NOT EXISTS (SELECT 1 FROM agents a WHERE a.agent_id=NEW.created_by AND a.run_id=NEW.run_id)
    THEN RAISE(ABORT, 'INVARIANT_tasks_creator_same_run') END;
END;

CREATE TRIGGER tasks_values_update BEFORE UPDATE OF state,revision,owner_lease_generation,authority_id,owner_agent_id,created_by,run_id ON tasks BEGIN
  SELECT CASE WHEN NEW.state NOT IN ('blocked','ready','active','complete','cancelled','degraded') OR NEW.revision < 0 OR NEW.owner_lease_generation < 0
    THEN RAISE(ABORT, 'INVARIANT_tasks_values') END;
  SELECT CASE WHEN NOT EXISTS (SELECT 1 FROM authorities a WHERE a.authority_id=NEW.authority_id AND a.run_id=NEW.run_id)
    THEN RAISE(ABORT, 'INVARIANT_tasks_authority_same_run') END;
  SELECT CASE WHEN NEW.owner_agent_id IS NOT NULL AND NOT EXISTS (SELECT 1 FROM agents a WHERE a.agent_id=NEW.owner_agent_id AND a.run_id=NEW.run_id)
    THEN RAISE(ABORT, 'INVARIANT_tasks_owner_same_run') END;
  SELECT CASE WHEN NOT EXISTS (SELECT 1 FROM agents a WHERE a.agent_id=NEW.created_by AND a.run_id=NEW.run_id)
    THEN RAISE(ABORT, 'INVARIANT_tasks_creator_same_run') END;
END;

CREATE TRIGGER teams_values_insert BEFORE INSERT ON teams
WHEN NEW.state NOT IN ('active','frozen','barrier-closed') OR NEW.generation < 1 OR NEW.depth < 1
BEGIN SELECT RAISE(ABORT, 'INVARIANT_teams_values'); END;

CREATE TRIGGER teams_values_update BEFORE UPDATE OF state,generation,depth ON teams
WHEN NEW.state NOT IN ('active','frozen','barrier-closed') OR NEW.generation < 1 OR NEW.depth < 1
BEGIN SELECT RAISE(ABORT, 'INVARIANT_teams_values'); END;

CREATE TRIGGER teams_run_leader_cap_insert BEFORE INSERT ON teams
WHEN (SELECT COUNT(*) FROM teams WHERE run_id=NEW.run_id) >= 4
BEGIN SELECT RAISE(ABORT, 'INVARIANT_teams_run_leader_cap'); END;

CREATE TRIGGER teams_run_leader_cap_update BEFORE UPDATE OF run_id ON teams
WHEN NEW.run_id<>OLD.run_id AND (SELECT COUNT(*) FROM teams WHERE run_id=NEW.run_id) >= 4
BEGIN SELECT RAISE(ABORT, 'INVARIANT_teams_run_leader_cap'); END;

CREATE TRIGGER workstream_custody_immutable_delete
BEFORE DELETE ON workstream_custody
BEGIN SELECT RAISE(ABORT, 'INVARIANT_workstream_custody_immutable'); END;

CREATE TRIGGER workstream_custody_immutable_update
BEFORE UPDATE ON workstream_custody
BEGIN SELECT RAISE(ABORT, 'INVARIANT_workstream_custody_immutable'); END;

CREATE TRIGGER run_plan_declarations_contiguous_insert
BEFORE INSERT ON run_plan_declarations
WHEN NEW.plan_revision<>(
  SELECT COALESCE(MAX(plan_revision),0)+1
    FROM run_plan_declarations WHERE run_id=NEW.run_id
)
BEGIN SELECT RAISE(ABORT, 'INVARIANT_run_plan_revision_contiguous'); END;

CREATE TRIGGER run_plan_declarations_immutable_delete
BEFORE DELETE ON run_plan_declarations
BEGIN SELECT RAISE(ABORT, 'INVARIANT_run_plan_declaration_immutable'); END;

CREATE TRIGGER run_plan_declarations_immutable_update
BEFORE UPDATE ON run_plan_declarations
BEGIN SELECT RAISE(ABORT, 'INVARIANT_run_plan_declaration_immutable'); END;

CREATE TRIGGER write_lease_cross_owner_insert
BEFORE INSERT ON leases
WHEN EXISTS (SELECT 1 FROM run_chair_leases WHERE run_id=NEW.run_id AND lease_id=NEW.lease_id)
  OR EXISTS (SELECT 1 FROM task_owner_leases WHERE run_id=NEW.run_id AND lease_id=NEW.lease_id)
BEGIN SELECT RAISE(ABORT,'INVARIANT_lease_identity_single_owner'); END;

CREATE TRIGGER write_lease_cross_owner_update
BEFORE UPDATE OF run_id,lease_id ON leases
WHEN EXISTS (SELECT 1 FROM run_chair_leases WHERE run_id=NEW.run_id AND lease_id=NEW.lease_id)
  OR EXISTS (SELECT 1 FROM task_owner_leases WHERE run_id=NEW.run_id AND lease_id=NEW.lease_id)
BEGIN SELECT RAISE(ABORT,'INVARIANT_lease_identity_single_owner'); END;

CREATE TRIGGER write_lease_identity_immutable
BEFORE UPDATE OF lease_id,run_id,kind ON leases
WHEN NEW.lease_id<>OLD.lease_id OR NEW.run_id<>OLD.run_id OR NEW.kind<>OLD.kind
BEGIN SELECT RAISE(ABORT,'INVARIANT_write_lease_identity_immutable'); END;

CREATE TRIGGER writer_prefix_exact_overlap BEFORE INSERT ON writer_prefixes
WHEN EXISTS (
  SELECT 1 FROM writer_prefixes p JOIN writer_admissions w ON w.writer_admission_id=p.writer_admission_id
   WHERE w.state='active' AND p.canonical_prefix=NEW.canonical_prefix
)
BEGIN SELECT RAISE(ABORT, 'AFAB_0004_WRITER_OVERLAP'); END;

-- Spec 04 v1.30 current review, lifecycle-routing and delivery catalogue.
-- These relations replace the retired caller-authored routing/review evidence stores.

CREATE UNIQUE INDEX artifacts_exact_revision
  ON artifacts(artifact_id, revision);

CREATE UNIQUE INDEX run_authority_exact_ref
  ON run_authority_revisions(
    project_session_id, coordination_run_id, authority_revision, authority_ref
  );

CREATE TABLE artifact_publication_lineage (
  artifact_id TEXT NOT NULL,
  artifact_revision INTEGER NOT NULL CHECK (artifact_revision >= 1),
  run_id TEXT NOT NULL,
  publisher_agent_id TEXT NOT NULL,
  publisher_principal_generation INTEGER NOT NULL CHECK (publisher_principal_generation >= 1),
  publisher_bridge_generation INTEGER NOT NULL CHECK (publisher_bridge_generation >= 1),
  provider_custody_adapter_id TEXT NOT NULL,
  provider_custody_action_id TEXT NOT NULL,
  provider_session_generation INTEGER NOT NULL CHECK (provider_session_generation >= 1),
  adapter_contract_digest TEXT NOT NULL,
  provider_family TEXT NOT NULL,
  model TEXT NOT NULL,
  route_receipt_digest TEXT,
  state TEXT NOT NULL CHECK (state IN ('proved','unproved')),
  reason TEXT,
  lineage_digest TEXT NOT NULL UNIQUE,
  created_at INTEGER NOT NULL,
  PRIMARY KEY (artifact_id, artifact_revision),
  UNIQUE (artifact_id, artifact_revision, lineage_digest),
  FOREIGN KEY (artifact_id, artifact_revision)
    REFERENCES artifacts(artifact_id, revision),
  FOREIGN KEY (run_id, publisher_agent_id) REFERENCES agents(run_id, agent_id),
  FOREIGN KEY (provider_custody_adapter_id, provider_custody_action_id)
    REFERENCES provider_actions(adapter_id, action_id),
  CHECK ((state = 'proved') = (reason IS NULL))
);

CREATE TABLE provider_session_lineage (
  run_id TEXT NOT NULL,
  agent_id TEXT NOT NULL,
  principal_generation INTEGER NOT NULL CHECK (principal_generation >= 1),
  bridge_owner_kind TEXT NOT NULL CHECK (bridge_owner_kind IN ('child','chair')),
  bridge_row_id TEXT NOT NULL,
  bridge_generation INTEGER NOT NULL CHECK (bridge_generation >= 1),
  provider_session_ref TEXT NOT NULL,
  provider_session_generation INTEGER NOT NULL CHECK (provider_session_generation >= 1),
  adapter_id TEXT NOT NULL,
  adapter_contract_digest TEXT NOT NULL,
  provider_family TEXT NOT NULL,
  model TEXT NOT NULL,
  route_receipt_digest TEXT,
  owner_kind TEXT NOT NULL CHECK (owner_kind IN ('chair-launch','retained-child')),
  owner_action_id TEXT NOT NULL,
  state TEXT NOT NULL CHECK (state IN ('active','retired')),
  lineage_digest TEXT NOT NULL UNIQUE,
  created_at INTEGER NOT NULL,
  PRIMARY KEY (run_id, agent_id, provider_session_generation),
  FOREIGN KEY (run_id, agent_id) REFERENCES agents(run_id, agent_id),
  FOREIGN KEY (adapter_id, owner_action_id) REFERENCES provider_actions(adapter_id, action_id)
);

CREATE TABLE delivery_run_starts (
  project_session_id TEXT NOT NULL,
  run_id TEXT NOT NULL,
  delivery_run_id TEXT NOT NULL,
  repository_object_format TEXT NOT NULL CHECK (repository_object_format IN ('sha1','sha256')),
  approved_base_object_id TEXT NOT NULL,
  authority_digest TEXT NOT NULL,
  created_revision INTEGER NOT NULL CHECK (created_revision >= 1),
  PRIMARY KEY (project_session_id, run_id, delivery_run_id),
  FOREIGN KEY (project_session_id, run_id) REFERENCES runs(project_session_id, run_id)
);

CREATE TABLE delivery_requirement_maps (
  run_id TEXT NOT NULL,
  delivery_run_id TEXT NOT NULL,
  map_generation INTEGER NOT NULL CHECK (map_generation >= 1),
  closure_digest TEXT NOT NULL,
  catalogue_digest TEXT NOT NULL,
  accepted_scope_artifact_id TEXT NOT NULL,
  accepted_scope_revision INTEGER NOT NULL CHECK (accepted_scope_revision >= 1),
  accepted_scope_digest TEXT NOT NULL,
  source_set_digest TEXT NOT NULL,
  requirement_set_digest TEXT NOT NULL,
  artifact_id TEXT NOT NULL,
  artifact_revision INTEGER NOT NULL CHECK (artifact_revision >= 1),
  content_digest TEXT NOT NULL UNIQUE,
  current INTEGER NOT NULL CHECK (current IN (0,1)),
  private_cas_path TEXT NOT NULL,
  PRIMARY KEY (run_id, delivery_run_id, map_generation),
  FOREIGN KEY (accepted_scope_artifact_id, accepted_scope_revision)
    REFERENCES artifacts(artifact_id, revision),
  FOREIGN KEY (artifact_id, artifact_revision) REFERENCES artifacts(artifact_id, revision)
);

CREATE UNIQUE INDEX one_current_delivery_requirement_map
  ON delivery_requirement_maps(run_id, delivery_run_id)
  WHERE current = 1;

CREATE TABLE coordination_gate_snapshots (
  run_id TEXT NOT NULL,
  delivery_run_id TEXT NOT NULL,
  snapshot_generation INTEGER NOT NULL CHECK (snapshot_generation >= 1),
  event_watermark INTEGER NOT NULL CHECK (event_watermark >= 0),
  chair_snapshot_digest TEXT NOT NULL,
  authority_digest TEXT NOT NULL,
  accepted_scope_digest TEXT NOT NULL,
  requirement_map_digest TEXT NOT NULL,
  gate_closure_digest TEXT NOT NULL,
  objective_evidence_digest TEXT NOT NULL,
  artifact_id TEXT NOT NULL,
  artifact_revision INTEGER NOT NULL CHECK (artifact_revision >= 1),
  content_digest TEXT NOT NULL UNIQUE,
  private_cas_path TEXT NOT NULL,
  PRIMARY KEY (run_id, delivery_run_id, snapshot_generation),
  FOREIGN KEY (artifact_id, artifact_revision) REFERENCES artifacts(artifact_id, revision)
);

CREATE TABLE implementation_delivery_manifests (
  run_id TEXT NOT NULL,
  delivery_run_id TEXT NOT NULL,
  seal_generation INTEGER NOT NULL CHECK (seal_generation >= 1),
  command_id TEXT NOT NULL,
  snapshot_generation INTEGER NOT NULL CHECK (snapshot_generation >= 1),
  profile_digest TEXT NOT NULL,
  accepted_scope_digest TEXT NOT NULL,
  requirement_map_digest TEXT NOT NULL,
  evidence_closure_digest TEXT NOT NULL,
  base_object_id TEXT NOT NULL,
  head_object_id TEXT NOT NULL,
  head_tree_id TEXT NOT NULL,
  index_tree_id TEXT NOT NULL,
  repository_source_state_digest TEXT NOT NULL,
  artifact_id TEXT NOT NULL,
  artifact_revision INTEGER NOT NULL CHECK (artifact_revision >= 1),
  content_digest TEXT NOT NULL UNIQUE,
  publication_lineage_digest TEXT NOT NULL,
  private_cas_path TEXT NOT NULL,
  PRIMARY KEY (run_id, delivery_run_id, seal_generation),
  UNIQUE (run_id, command_id),
  FOREIGN KEY (artifact_id, artifact_revision) REFERENCES artifacts(artifact_id, revision),
  FOREIGN KEY (artifact_id, artifact_revision, publication_lineage_digest)
    REFERENCES artifact_publication_lineage(artifact_id, artifact_revision, lineage_digest)
);

CREATE TABLE delivery_review_bases (
  run_id TEXT NOT NULL,
  delivery_run_id TEXT NOT NULL,
  review_basis_revision INTEGER NOT NULL CHECK (review_basis_revision >= 1),
  manifest_artifact_id TEXT NOT NULL,
  manifest_artifact_revision INTEGER NOT NULL CHECK (manifest_artifact_revision >= 1),
  manifest_digest TEXT NOT NULL,
  snapshot_digest TEXT NOT NULL,
  profile_digest TEXT NOT NULL,
  repository_source_state_digest TEXT NOT NULL,
  requirement_map_digest TEXT NOT NULL,
  evidence_closure_digest TEXT NOT NULL,
  current INTEGER NOT NULL CHECK (current IN (0,1)),
  basis_digest TEXT NOT NULL UNIQUE,
  PRIMARY KEY (run_id, delivery_run_id, review_basis_revision),
  FOREIGN KEY (manifest_artifact_id, manifest_artifact_revision)
    REFERENCES artifacts(artifact_id, revision)
);

CREATE UNIQUE INDEX one_current_delivery_review_basis
  ON delivery_review_bases(run_id, delivery_run_id)
  WHERE current = 1;

CREATE TABLE review_target_preparation_high_water (
  run_id TEXT PRIMARY KEY,
  preparation_generation INTEGER NOT NULL CHECK (preparation_generation >= 0),
  target_generation INTEGER NOT NULL CHECK (target_generation >= 0),
  bundle_generation INTEGER NOT NULL CHECK (bundle_generation >= 0),
  revision INTEGER NOT NULL CHECK (revision >= 1),
  FOREIGN KEY (run_id) REFERENCES runs(run_id)
);

CREATE TABLE review_target_preparations (
  run_id TEXT NOT NULL,
  preparation_id TEXT NOT NULL,
  preparation_generation INTEGER NOT NULL CHECK (preparation_generation >= 1),
  owner_command_id TEXT NOT NULL,
  semantic_input_digest TEXT NOT NULL,
  full_input_digest TEXT NOT NULL,
  actor_principal_digest TEXT NOT NULL,
  task_id TEXT NOT NULL,
  expected_target_generation INTEGER NOT NULL CHECK (expected_target_generation >= 0),
  delivery_manifest_artifact_id TEXT NOT NULL,
  delivery_manifest_artifact_revision INTEGER NOT NULL CHECK (delivery_manifest_artifact_revision >= 1),
  reserved_target_generation INTEGER NOT NULL CHECK (reserved_target_generation >= 1),
  reserved_bundle_generation INTEGER NOT NULL CHECK (reserved_bundle_generation >= 1),
  state TEXT NOT NULL CHECK (state IN (
    'prepared','building','built','succeeded','conflicted','failed'
  )),
  revision INTEGER NOT NULL CHECK (revision >= 1),
  worker_claim_generation INTEGER NOT NULL DEFAULT 0 CHECK (worker_claim_generation >= 0),
  worker_instance_id TEXT,
  worker_lease_expires_at INTEGER,
  captured_precondition_digest TEXT,
  progress_kind TEXT CHECK (progress_kind IS NULL OR progress_kind IN ('phase-only','verified-build-items')),
  progress_plan_digest TEXT,
  progress_total INTEGER CHECK (progress_total IS NULL OR progress_total >= 0),
  progress_completed INTEGER CHECK (progress_completed IS NULL OR progress_completed >= 0),
  built_bundle_digest TEXT,
  built_manifest_root_digest TEXT,
  terminal_kind TEXT,
  terminal_code TEXT,
  terminal_evidence_digest TEXT,
  target_generation INTEGER CHECK (target_generation IS NULL OR target_generation >= 1),
  accepted_receipt_digest TEXT NOT NULL,
  created_at INTEGER NOT NULL,
  updated_at INTEGER NOT NULL,
  PRIMARY KEY (run_id, preparation_id),
  UNIQUE (run_id, preparation_generation),
  UNIQUE (run_id, reserved_target_generation),
  UNIQUE (run_id, reserved_bundle_generation),
  UNIQUE (owner_command_id),
  FOREIGN KEY (run_id, task_id) REFERENCES tasks(run_id, task_id),
  FOREIGN KEY (delivery_manifest_artifact_id, delivery_manifest_artifact_revision)
    REFERENCES artifacts(artifact_id, revision),
  CHECK ((progress_total IS NULL) = (progress_completed IS NULL)),
  CHECK (progress_completed IS NULL OR progress_completed <= progress_total),
  CHECK ((worker_instance_id IS NULL) = (worker_lease_expires_at IS NULL)),
  CHECK ((state = 'succeeded') = (target_generation IS NOT NULL))
);

CREATE UNIQUE INDEX one_active_review_target_preparation_per_run
  ON review_target_preparations(run_id)
  WHERE state IN ('prepared','building','built');

CREATE TABLE review_bundles (
  run_id TEXT NOT NULL,
  bundle_generation INTEGER NOT NULL CHECK (bundle_generation >= 1),
  delivery_run_id TEXT NOT NULL,
  review_basis_revision INTEGER NOT NULL CHECK (review_basis_revision >= 1),
  review_basis_digest TEXT NOT NULL,
  delivery_artifact_id TEXT NOT NULL,
  delivery_artifact_revision INTEGER NOT NULL CHECK (delivery_artifact_revision >= 1),
  base_object_id TEXT NOT NULL,
  head_object_id TEXT NOT NULL,
  head_tree_id TEXT NOT NULL,
  index_tree_id TEXT NOT NULL,
  review_diff_codec_digest TEXT NOT NULL,
  review_diff_rules_digest TEXT NOT NULL,
  review_diff_set_digest TEXT NOT NULL,
  repository_source_state_digest TEXT NOT NULL,
  publication_lineage_digest TEXT NOT NULL,
  coverage_digest TEXT NOT NULL,
  manifest_body_digest TEXT NOT NULL,
  manifest_root_digest TEXT NOT NULL,
  bundle_digest TEXT NOT NULL UNIQUE,
  bundle_search_index_digest TEXT NOT NULL,
  risk_read_map_digest TEXT NOT NULL,
  mandatory_read_set_digest TEXT NOT NULL,
  mandatory_read_count INTEGER NOT NULL CHECK (mandatory_read_count BETWEEN 0 AND 80),
  mandatory_read_bytes INTEGER NOT NULL CHECK (mandatory_read_bytes BETWEEN 0 AND 6291456),
  changed_path_count INTEGER NOT NULL CHECK (changed_path_count BETWEEN 0 AND 4096),
  required_evidence_count INTEGER NOT NULL CHECK (required_evidence_count BETWEEN 0 AND 1024),
  carried_finding_count INTEGER NOT NULL CHECK (carried_finding_count >= 0),
  object_count INTEGER NOT NULL CHECK (object_count BETWEEN 0 AND 16384),
  chunk_count INTEGER NOT NULL CHECK (chunk_count BETWEEN 0 AND 32768),
  total_object_bytes INTEGER NOT NULL CHECK (total_object_bytes BETWEEN 0 AND 67108864),
  manifest_page_bytes INTEGER NOT NULL CHECK (manifest_page_bytes BETWEEN 0 AND 1048576),
  search_index_bytes INTEGER NOT NULL CHECK (search_index_bytes BETWEEN 0 AND 4194304),
  risk_map_bytes INTEGER NOT NULL CHECK (risk_map_bytes BETWEEN 0 AND 262144),
  private_manifest_body_path TEXT NOT NULL,
  private_manifest_root_path TEXT NOT NULL,
  private_bundle_ref_path TEXT NOT NULL,
  created_at INTEGER NOT NULL,
  PRIMARY KEY (run_id, bundle_generation),
  FOREIGN KEY (run_id, delivery_run_id, review_basis_revision)
    REFERENCES delivery_review_bases(run_id, delivery_run_id, review_basis_revision),
  FOREIGN KEY (delivery_artifact_id, delivery_artifact_revision)
    REFERENCES artifacts(artifact_id, revision),
  FOREIGN KEY (delivery_artifact_id, delivery_artifact_revision, publication_lineage_digest)
    REFERENCES artifact_publication_lineage(artifact_id, artifact_revision, lineage_digest)
);

CREATE TABLE review_bundle_objects (
  bundle_digest TEXT NOT NULL,
  object_digest TEXT NOT NULL,
  media_type TEXT NOT NULL,
  byte_length INTEGER NOT NULL CHECK (byte_length BETWEEN 0 AND 16777216),
  ordinal INTEGER NOT NULL CHECK (ordinal >= 0),
  PRIMARY KEY (bundle_digest, object_digest),
  UNIQUE (bundle_digest, ordinal),
  FOREIGN KEY (bundle_digest) REFERENCES review_bundles(bundle_digest)
);

CREATE TABLE review_bundle_chunks (
  bundle_digest TEXT NOT NULL,
  object_digest TEXT NOT NULL,
  ordinal INTEGER NOT NULL CHECK (ordinal >= 0),
  chunk_digest TEXT NOT NULL,
  byte_length INTEGER NOT NULL CHECK (byte_length BETWEEN 1 AND 65536),
  private_chunk_path TEXT NOT NULL,
  PRIMARY KEY (bundle_digest, object_digest, ordinal),
  FOREIGN KEY (bundle_digest, object_digest)
    REFERENCES review_bundle_objects(bundle_digest, object_digest)
);

CREATE TABLE review_bundle_manifest_pages (
  bundle_digest TEXT NOT NULL,
  ordinal INTEGER NOT NULL CHECK (ordinal BETWEEN 0 AND 15),
  page_digest TEXT NOT NULL,
  byte_length INTEGER NOT NULL CHECK (byte_length BETWEEN 1 AND 65536),
  private_page_path TEXT NOT NULL,
  PRIMARY KEY (bundle_digest, ordinal),
  UNIQUE (bundle_digest, page_digest),
  FOREIGN KEY (bundle_digest) REFERENCES review_bundles(bundle_digest)
);

CREATE TABLE review_finding_sets (
  finding_set_digest TEXT PRIMARY KEY CHECK (
    length(finding_set_digest) = 71 AND substr(finding_set_digest, 1, 7) = 'sha256:' AND
    substr(finding_set_digest, 8) NOT GLOB '*[^0-9a-f]*'
  ),
  finding_count INTEGER NOT NULL CHECK (finding_count >= 0),
  page_count INTEGER NOT NULL CHECK (page_count BETWEEN 0 AND 16),
  canonical_byte_length INTEGER NOT NULL CHECK (canonical_byte_length BETWEEN 0 AND 1048576),
  created_at INTEGER NOT NULL,
  CHECK ((finding_count = 0) = (page_count = 0)),
  CHECK (
    (finding_count = 0 AND page_count = 0 AND
      finding_set_digest = 'sha256:58afae1b74b0f7295f280a34196c2e092e4040016e64927e132f99356b48b7a2' AND
      canonical_byte_length = 47) OR
    (finding_count > 0 AND page_count > 0 AND canonical_byte_length > 0)
  )
);

CREATE TABLE review_finding_pages (
  page_digest TEXT PRIMARY KEY CHECK (
    length(page_digest) = 71 AND substr(page_digest, 1, 7) = 'sha256:' AND
    substr(page_digest, 8) NOT GLOB '*[^0-9a-f]*'
  ),
  member_count INTEGER NOT NULL CHECK (member_count >= 1),
  canonical_byte_length INTEGER NOT NULL CHECK (canonical_byte_length BETWEEN 1 AND 65536),
  private_page_path TEXT NOT NULL,
  created_at INTEGER NOT NULL
);

CREATE TABLE review_finding_set_pages (
  finding_set_digest TEXT NOT NULL,
  ordinal INTEGER NOT NULL CHECK (ordinal BETWEEN 0 AND 15),
  page_digest TEXT NOT NULL,
  member_count INTEGER NOT NULL CHECK (member_count >= 1),
  first_finding_digest TEXT,
  last_finding_digest TEXT,
  PRIMARY KEY (finding_set_digest, ordinal),
  UNIQUE (finding_set_digest, page_digest),
  FOREIGN KEY (finding_set_digest) REFERENCES review_finding_sets(finding_set_digest),
  FOREIGN KEY (page_digest) REFERENCES review_finding_pages(page_digest),
  CHECK (first_finding_digest IS NOT NULL AND last_finding_digest IS NOT NULL),
  CHECK (
    length(first_finding_digest) = 71 AND substr(first_finding_digest, 1, 7) = 'sha256:' AND
    substr(first_finding_digest, 8) NOT GLOB '*[^0-9a-f]*'
  ),
  CHECK (
    length(last_finding_digest) = 71 AND substr(last_finding_digest, 1, 7) = 'sha256:' AND
    substr(last_finding_digest, 8) NOT GLOB '*[^0-9a-f]*'
  )
);

CREATE TABLE review_finding_members (
  page_digest TEXT NOT NULL,
  member_ordinal INTEGER NOT NULL CHECK (member_ordinal >= 0),
  finding_digest TEXT NOT NULL CHECK (
    length(finding_digest) = 71 AND substr(finding_digest, 1, 7) = 'sha256:' AND
    substr(finding_digest, 8) NOT GLOB '*[^0-9a-f]*'
  ),
  finding_id TEXT NOT NULL,
  severity TEXT NOT NULL CHECK (severity IN ('P0','P1','P2')),
  safe_record_json TEXT NOT NULL CHECK (json_valid(safe_record_json)),
  PRIMARY KEY (page_digest, member_ordinal),
  UNIQUE (page_digest, finding_digest, finding_id),
  FOREIGN KEY (page_digest) REFERENCES review_finding_pages(page_digest)
);

CREATE TRIGGER review_finding_members_closed_insert
BEFORE INSERT ON review_finding_members
WHEN json_type(NEW.safe_record_json) <> 'object'
  OR NEW.safe_record_json <> json(NEW.safe_record_json)
  OR EXISTS (
    SELECT 1
      FROM json_tree(NEW.safe_record_json) object
      JOIN json_tree(NEW.safe_record_json) earlier ON earlier.parent = object.id
      JOIN json_tree(NEW.safe_record_json) later
        ON later.parent = object.id AND later.id > earlier.id
     WHERE object.type = 'object'
       AND CAST(earlier.key AS TEXT) >= CAST(later.key AS TEXT)
  )
  OR (SELECT COUNT(*) FROM json_each(NEW.safe_record_json)) <> 12
  OR EXISTS (
    SELECT 1 FROM json_each(NEW.safe_record_json)
     WHERE key NOT IN (
       'evidence','findingDigest','findingId','originActionRef','originBundleDigest',
       'originDeliveryManifestRef','originDeliveryReviewBasisDigest',
       'originResultDigest','originTargetGeneration','repairCurrency','severity','summary'
     )
  )
  OR json_extract(NEW.safe_record_json, '$.findingDigest') IS NOT NEW.finding_digest
  OR json_extract(NEW.safe_record_json, '$.findingId') IS NOT NEW.finding_id
  OR json_extract(NEW.safe_record_json, '$.severity') IS NOT NEW.severity
  OR length(CAST(NEW.finding_id AS BLOB)) NOT BETWEEN 1 AND 64
  OR COALESCE(json_type(NEW.safe_record_json, '$.summary'), '') <> 'text'
  OR length(CAST(json_extract(NEW.safe_record_json, '$.summary') AS BLOB)) NOT BETWEEN 1 AND 256
  OR COALESCE(json_type(NEW.safe_record_json, '$.evidence'), '') <> 'text'
  OR length(CAST(json_extract(NEW.safe_record_json, '$.evidence') AS BLOB)) NOT BETWEEN 1 AND 768
  OR COALESCE(json_type(NEW.safe_record_json, '$.originTargetGeneration'), '') <> 'integer'
  OR json_extract(NEW.safe_record_json, '$.originTargetGeneration') < 1
  OR COALESCE(json_type(NEW.safe_record_json, '$.originDeliveryManifestRef'), '') <> 'text'
  OR length(CAST(json_extract(
    NEW.safe_record_json, '$.originDeliveryManifestRef'
  ) AS BLOB)) NOT BETWEEN 1 AND 256
  OR COALESCE(json_type(NEW.safe_record_json, '$.originResultDigest'), '') <> 'text'
  OR COALESCE(json_type(NEW.safe_record_json, '$.originDeliveryReviewBasisDigest'), '') <> 'text'
  OR COALESCE(json_type(NEW.safe_record_json, '$.originBundleDigest'), '') <> 'text'
  OR EXISTS (
    SELECT 1 FROM json_each(NEW.safe_record_json) scalar
     WHERE scalar.key IN (
       'originResultDigest','originDeliveryReviewBasisDigest','originBundleDigest'
     ) AND (
       length(CAST(scalar.value AS TEXT)) <> 71 OR
       substr(CAST(scalar.value AS TEXT), 1, 7) <> 'sha256:' OR
       substr(CAST(scalar.value AS TEXT), 8) GLOB '*[^0-9a-f]*'
     )
  )
  OR (SELECT COUNT(*) FROM json_each(NEW.safe_record_json, '$.originActionRef')) <> 2
  OR EXISTS (
    SELECT 1 FROM json_each(NEW.safe_record_json, '$.originActionRef')
     WHERE key NOT IN ('actionId','adapterId')
  )
  OR COALESCE(json_type(NEW.safe_record_json, '$.originActionRef.adapterId'), '') <> 'text'
  OR COALESCE(json_type(NEW.safe_record_json, '$.originActionRef.actionId'), '') <> 'text'
  OR length(CAST(json_extract(
    NEW.safe_record_json, '$.originActionRef.adapterId'
  ) AS BLOB)) NOT BETWEEN 1 AND 256
  OR length(CAST(json_extract(
    NEW.safe_record_json, '$.originActionRef.actionId'
  ) AS BLOB)) NOT BETWEEN 1 AND 256
  OR (SELECT COUNT(*) FROM json_each(NEW.safe_record_json, '$.repairCurrency')) <> 3
  OR EXISTS (
    SELECT 1 FROM json_each(NEW.safe_record_json, '$.repairCurrency')
     WHERE key NOT IN ('evidenceRefs','kind','originRepositorySourceStateDigest')
  )
  OR json_extract(NEW.safe_record_json, '$.repairCurrency.kind') NOT IN (
    'repository-source','registered-evidence','mixed'
  )
  OR COALESCE(json_type(
    NEW.safe_record_json, '$.repairCurrency.originRepositorySourceStateDigest'
  ), '') NOT IN ('null','text')
  OR (json_type(NEW.safe_record_json,
      '$.repairCurrency.originRepositorySourceStateDigest') = 'text' AND (
    length(json_extract(NEW.safe_record_json,
      '$.repairCurrency.originRepositorySourceStateDigest')) <> 71 OR
    substr(json_extract(NEW.safe_record_json,
      '$.repairCurrency.originRepositorySourceStateDigest'), 1, 7) <> 'sha256:' OR
    substr(json_extract(NEW.safe_record_json,
      '$.repairCurrency.originRepositorySourceStateDigest'), 8) GLOB '*[^0-9a-f]*'
  ))
  OR COALESCE(json_type(NEW.safe_record_json, '$.repairCurrency.evidenceRefs'), '') <> 'array'
  OR EXISTS (
    SELECT 1 FROM json_each(NEW.safe_record_json, '$.repairCurrency.evidenceRefs') evidence
     WHERE evidence.type <> 'object'
        OR (SELECT COUNT(*) FROM json_each(evidence.value)) <> 3
        OR EXISTS (
          SELECT 1 FROM json_each(evidence.value)
           WHERE key NOT IN ('contentDigest','evidenceRef','evidenceRevision')
        )
        OR COALESCE(json_type(evidence.value, '$.evidenceRef'), '') <> 'text'
        OR length(CAST(json_extract(
          evidence.value, '$.evidenceRef'
        ) AS BLOB)) NOT BETWEEN 1 AND 256
        OR COALESCE(json_type(evidence.value, '$.evidenceRevision'), '') <> 'integer'
        OR json_extract(evidence.value, '$.evidenceRevision') < 1
        OR COALESCE(json_type(evidence.value, '$.contentDigest'), '') <> 'text'
        OR length(json_extract(evidence.value, '$.contentDigest')) <> 71
        OR substr(json_extract(evidence.value, '$.contentDigest'), 1, 7) <> 'sha256:'
        OR substr(json_extract(evidence.value, '$.contentDigest'), 8) GLOB '*[^0-9a-f]*'
  )
  OR EXISTS (
    SELECT 1
      FROM json_each(NEW.safe_record_json, '$.repairCurrency.evidenceRefs') earlier
      JOIN json_each(NEW.safe_record_json, '$.repairCurrency.evidenceRefs') later
        ON CAST(later.key AS INTEGER) = CAST(earlier.key AS INTEGER) + 1
     WHERE json_extract(earlier.value, '$.evidenceRef') >
           json_extract(later.value, '$.evidenceRef')
        OR (json_extract(earlier.value, '$.evidenceRef') =
            json_extract(later.value, '$.evidenceRef') AND
            json_extract(earlier.value, '$.evidenceRevision') >=
            json_extract(later.value, '$.evidenceRevision'))
  )
  OR NOT (
    (json_extract(NEW.safe_record_json, '$.repairCurrency.kind') = 'repository-source'
      AND json_type(NEW.safe_record_json,
        '$.repairCurrency.originRepositorySourceStateDigest') = 'text'
      AND json_array_length(NEW.safe_record_json, '$.repairCurrency.evidenceRefs') = 0) OR
    (json_extract(NEW.safe_record_json, '$.repairCurrency.kind') = 'registered-evidence'
      AND json_type(NEW.safe_record_json,
        '$.repairCurrency.originRepositorySourceStateDigest') = 'null'
      AND json_array_length(NEW.safe_record_json, '$.repairCurrency.evidenceRefs') > 0) OR
    (json_extract(NEW.safe_record_json, '$.repairCurrency.kind') = 'mixed'
      AND json_type(NEW.safe_record_json,
        '$.repairCurrency.originRepositorySourceStateDigest') = 'text'
      AND json_array_length(NEW.safe_record_json, '$.repairCurrency.evidenceRefs') > 0)
  )
  OR NEW.member_ordinal <> COALESCE((
    SELECT MAX(member_ordinal) + 1 FROM review_finding_members
     WHERE page_digest = NEW.page_digest
  ), 0)
  OR NEW.member_ordinal >= (
    SELECT member_count FROM review_finding_pages WHERE page_digest = NEW.page_digest
  )
  OR EXISTS (
    SELECT 1 FROM review_finding_members prior
     WHERE prior.page_digest = NEW.page_digest
       AND prior.member_ordinal = NEW.member_ordinal - 1
       AND (prior.finding_digest > NEW.finding_digest OR
         (prior.finding_digest = NEW.finding_digest AND prior.finding_id >= NEW.finding_id))
  )
BEGIN
  SELECT RAISE(ABORT, 'INVARIANT_review_finding_member_closed');
END;

CREATE TRIGGER review_finding_set_pages_closed_insert
BEFORE INSERT ON review_finding_set_pages
WHEN NEW.ordinal <> COALESCE((
    SELECT MAX(ordinal) + 1 FROM review_finding_set_pages
     WHERE finding_set_digest = NEW.finding_set_digest
  ), 0)
  OR NEW.ordinal >= (
    SELECT page_count FROM review_finding_sets
     WHERE finding_set_digest = NEW.finding_set_digest
  )
  OR NOT EXISTS (
    SELECT 1
      FROM review_finding_pages page
     WHERE page.page_digest = NEW.page_digest
       AND page.member_count = NEW.member_count
       AND page.member_count = (
         SELECT COUNT(*) FROM review_finding_members member
          WHERE member.page_digest = page.page_digest
       )
       AND page.canonical_byte_length = (
         SELECT length(CAST(
           '{"members":[' || group_concat(member.safe_record_json, ',') ||
           '],"schemaVersion":1}' AS BLOB
         ))
           FROM (
             SELECT safe_record_json
               FROM review_finding_members
              WHERE page_digest = page.page_digest
              ORDER BY member_ordinal
           ) member
       )
       AND NEW.first_finding_digest = (
         SELECT finding_digest FROM review_finding_members
          WHERE page_digest = page.page_digest ORDER BY member_ordinal LIMIT 1
       )
       AND NEW.last_finding_digest = (
         SELECT finding_digest FROM review_finding_members
          WHERE page_digest = page.page_digest ORDER BY member_ordinal DESC LIMIT 1
       )
  )
  OR EXISTS (
    SELECT 1 FROM review_finding_set_pages prior
     WHERE prior.finding_set_digest = NEW.finding_set_digest
       AND prior.ordinal = NEW.ordinal - 1
       AND prior.last_finding_digest >= NEW.first_finding_digest
  )
  OR NEW.member_count + COALESCE((
    SELECT SUM(member_count) FROM review_finding_set_pages
     WHERE finding_set_digest = NEW.finding_set_digest
  ), 0) > (
    SELECT finding_count FROM review_finding_sets
     WHERE finding_set_digest = NEW.finding_set_digest
  )
  OR (
    NEW.ordinal + 1 = (
      SELECT page_count FROM review_finding_sets
       WHERE finding_set_digest = NEW.finding_set_digest
    ) AND (
      SELECT canonical_byte_length FROM review_finding_sets
       WHERE finding_set_digest = NEW.finding_set_digest
    ) <> (
      SELECT length(CAST(
        '{"findingCount":' || (
          SELECT finding_count FROM review_finding_sets
           WHERE finding_set_digest = NEW.finding_set_digest
        ) || ',"pages":[' || group_concat(entry, ',') || '],"schemaVersion":1}'
        AS BLOB
      ))
        FROM (
          SELECT ordinal,
            '{"firstFindingDigest":' || json_quote(first_finding_digest) ||
            ',"lastFindingDigest":' || json_quote(last_finding_digest) ||
            ',"memberCount":' || member_count ||
            ',"ordinal":' || ordinal ||
            ',"pageDigest":' || json_quote(page_digest) || '}' AS entry
            FROM review_finding_set_pages
           WHERE finding_set_digest = NEW.finding_set_digest
          UNION ALL
          SELECT NEW.ordinal,
            '{"firstFindingDigest":' || json_quote(NEW.first_finding_digest) ||
            ',"lastFindingDigest":' || json_quote(NEW.last_finding_digest) ||
            ',"memberCount":' || NEW.member_count ||
            ',"ordinal":' || NEW.ordinal ||
            ',"pageDigest":' || json_quote(NEW.page_digest) || '}'
          ORDER BY ordinal
        ) ordered_pages
    )
  )
BEGIN
  SELECT RAISE(ABORT, 'INVARIANT_review_finding_set_page_closed');
END;

CREATE TRIGGER review_finding_sets_immutable_update
BEFORE UPDATE ON review_finding_sets
BEGIN SELECT RAISE(ABORT, 'INVARIANT_review_finding_graph_immutable'); END;

CREATE TRIGGER review_finding_sets_immutable_delete
BEFORE DELETE ON review_finding_sets
BEGIN SELECT RAISE(ABORT, 'INVARIANT_review_finding_graph_immutable'); END;

CREATE TRIGGER review_finding_pages_immutable_update
BEFORE UPDATE ON review_finding_pages
BEGIN SELECT RAISE(ABORT, 'INVARIANT_review_finding_graph_immutable'); END;

CREATE TRIGGER review_finding_pages_immutable_delete
BEFORE DELETE ON review_finding_pages
BEGIN SELECT RAISE(ABORT, 'INVARIANT_review_finding_graph_immutable'); END;

CREATE TRIGGER review_finding_set_pages_immutable_update
BEFORE UPDATE ON review_finding_set_pages
BEGIN SELECT RAISE(ABORT, 'INVARIANT_review_finding_graph_immutable'); END;

CREATE TRIGGER review_finding_set_pages_immutable_delete
BEFORE DELETE ON review_finding_set_pages
BEGIN SELECT RAISE(ABORT, 'INVARIANT_review_finding_graph_immutable'); END;

CREATE TRIGGER review_finding_members_immutable_update
BEFORE UPDATE ON review_finding_members
BEGIN SELECT RAISE(ABORT, 'INVARIANT_review_finding_graph_immutable'); END;

CREATE TRIGGER review_finding_members_immutable_delete
BEFORE DELETE ON review_finding_members
BEGIN SELECT RAISE(ABORT, 'INVARIANT_review_finding_graph_immutable'); END;

CREATE VIEW review_finding_sets_complete AS
SELECT finding_set.*
  FROM review_finding_sets finding_set
 WHERE finding_set.page_count = (
   SELECT COUNT(*) FROM review_finding_set_pages set_page
    WHERE set_page.finding_set_digest = finding_set.finding_set_digest
 )
   AND finding_set.finding_count = COALESCE((
     SELECT SUM(set_page.member_count) FROM review_finding_set_pages set_page
      WHERE set_page.finding_set_digest = finding_set.finding_set_digest
   ), 0)
   AND NOT EXISTS (
     SELECT 1
       FROM review_finding_set_pages set_page
       JOIN review_finding_pages page ON page.page_digest = set_page.page_digest
      WHERE set_page.finding_set_digest = finding_set.finding_set_digest
        AND (
          set_page.member_count <> page.member_count OR
          page.member_count <> (
            SELECT COUNT(*) FROM review_finding_members member
             WHERE member.page_digest = page.page_digest
          ) OR
          set_page.first_finding_digest <> (
            SELECT finding_digest FROM review_finding_members member
             WHERE member.page_digest = page.page_digest
             ORDER BY member.member_ordinal LIMIT 1
          ) OR
          set_page.last_finding_digest <> (
            SELECT finding_digest FROM review_finding_members member
             WHERE member.page_digest = page.page_digest
             ORDER BY member.member_ordinal DESC LIMIT 1
          )
        )
   );

CREATE TABLE provider_action_pair_preflights (
  adapter_id TEXT NOT NULL,
  action_id TEXT NOT NULL,
  scope_kind TEXT NOT NULL CHECK(scope_kind IN ('provider-smoke','run-action')),
  run_id TEXT,
  owner_digest TEXT NOT NULL,
  actor_principal_digest TEXT NOT NULL,
  input_digest TEXT NOT NULL,
  state TEXT NOT NULL CHECK (state IN ('resolving','admitted','released')),
  failure_json TEXT CHECK (failure_json IS NULL OR json_valid(failure_json)),
  created_at INTEGER NOT NULL,
  updated_at INTEGER NOT NULL,
  PRIMARY KEY (adapter_id, action_id),
  UNIQUE (adapter_id, action_id, owner_digest),
  UNIQUE (run_id, adapter_id, action_id),
  UNIQUE (run_id, adapter_id, action_id, owner_digest),
  FOREIGN KEY (run_id) REFERENCES runs(run_id),
  CHECK (
    (scope_kind='provider-smoke' AND run_id IS NULL) OR
    (scope_kind='run-action' AND run_id IS NOT NULL)
  ),
  CHECK ((state = 'released') = (failure_json IS NOT NULL))
);

CREATE TRIGGER provider_action_pair_preflights_closed_update
BEFORE UPDATE ON provider_action_pair_preflights
WHEN NEW.adapter_id IS NOT OLD.adapter_id
  OR NEW.action_id IS NOT OLD.action_id
  OR NEW.scope_kind IS NOT OLD.scope_kind
  OR NEW.run_id IS NOT OLD.run_id
  OR NEW.owner_digest IS NOT OLD.owner_digest
  OR NEW.actor_principal_digest IS NOT OLD.actor_principal_digest
  OR NEW.input_digest IS NOT OLD.input_digest
  OR NEW.created_at IS NOT OLD.created_at
  OR NEW.updated_at < OLD.updated_at
  OR OLD.state <> 'resolving'
  OR NEW.state NOT IN ('admitted','released')
BEGIN SELECT RAISE(ABORT, 'INVARIANT_provider_action_preflight_transition'); END;

CREATE TRIGGER provider_action_pair_preflights_no_delete
BEFORE DELETE ON provider_action_pair_preflights
BEGIN SELECT RAISE(ABORT, 'INVARIANT_provider_action_preflight_immutable'); END;

CREATE TABLE review_finding_capacity_reservations (
  adapter_id TEXT NOT NULL,
  action_id TEXT NOT NULL,
  run_id TEXT NOT NULL,
  target_generation INTEGER NOT NULL CHECK (target_generation >= 1),
  slot TEXT NOT NULL CHECK (slot IN ('native','other-primary','cursor-grok','agy-gemini')),
  owner_digest TEXT NOT NULL,
  finding_window_mode TEXT NOT NULL CHECK (finding_window_mode IN ('normal','resolution-only')),
  prior_open_finding_set_digest TEXT NOT NULL,
  maximum_new_findings INTEGER NOT NULL CHECK (maximum_new_findings BETWEEN 0 AND 32),
  maximum_new_finding_bytes INTEGER NOT NULL CHECK (maximum_new_finding_bytes >= 0),
  reservation_digest TEXT NOT NULL CHECK (
    length(reservation_digest) = 71 AND
    substr(reservation_digest, 1, 7) = 'sha256:' AND
    substr(reservation_digest, 8) NOT GLOB '*[^0-9a-f]*'
  ),
  state TEXT NOT NULL CHECK (state IN ('preflight','attached','released','settled')),
  created_at INTEGER NOT NULL,
  updated_at INTEGER NOT NULL,
  PRIMARY KEY (adapter_id, action_id),
  UNIQUE (adapter_id, action_id, reservation_digest),
  UNIQUE (run_id, adapter_id, action_id, reservation_digest),
  FOREIGN KEY (run_id, adapter_id, action_id, owner_digest)
    REFERENCES provider_action_pair_preflights(
      run_id, adapter_id, action_id, owner_digest
    ),
  FOREIGN KEY (prior_open_finding_set_digest)
    REFERENCES review_finding_sets(finding_set_digest),
  CHECK ((finding_window_mode = 'resolution-only') =
    (maximum_new_findings = 0 AND maximum_new_finding_bytes = 0))
);

CREATE TRIGGER review_finding_capacity_reservations_complete_insert
BEFORE INSERT ON review_finding_capacity_reservations
WHEN NOT EXISTS (
  SELECT 1 FROM review_finding_sets_complete complete
   WHERE complete.finding_set_digest = NEW.prior_open_finding_set_digest
)
BEGIN SELECT RAISE(ABORT, 'INVARIANT_review_finding_set_incomplete'); END;

CREATE TRIGGER review_finding_capacity_reservations_complete_update
BEFORE UPDATE OF prior_open_finding_set_digest ON review_finding_capacity_reservations
WHEN NOT EXISTS (
  SELECT 1 FROM review_finding_sets_complete complete
   WHERE complete.finding_set_digest = NEW.prior_open_finding_set_digest
)
BEGIN SELECT RAISE(ABORT, 'INVARIANT_review_finding_set_incomplete'); END;

CREATE TRIGGER review_finding_capacity_reservations_state_update
BEFORE UPDATE ON review_finding_capacity_reservations
WHEN NOT (
    (OLD.state = 'preflight' AND NEW.state IN ('attached','released')) OR
    (OLD.state = 'attached' AND NEW.state IN ('settled','released'))
  )
  OR NEW.adapter_id IS NOT OLD.adapter_id
  OR NEW.action_id IS NOT OLD.action_id
  OR NEW.run_id IS NOT OLD.run_id
  OR NEW.target_generation IS NOT OLD.target_generation
  OR NEW.slot IS NOT OLD.slot
  OR NEW.owner_digest IS NOT OLD.owner_digest
  OR NEW.finding_window_mode IS NOT OLD.finding_window_mode
  OR NEW.prior_open_finding_set_digest IS NOT OLD.prior_open_finding_set_digest
  OR NEW.maximum_new_findings IS NOT OLD.maximum_new_findings
  OR NEW.maximum_new_finding_bytes IS NOT OLD.maximum_new_finding_bytes
  OR NEW.reservation_digest IS NOT OLD.reservation_digest
  OR NEW.created_at IS NOT OLD.created_at
  OR NEW.updated_at <= OLD.updated_at
  OR (NEW.state = 'attached' AND NOT EXISTS (
    SELECT 1 FROM provider_actions action
     WHERE action.run_id = NEW.run_id
       AND action.adapter_id = NEW.adapter_id
       AND action.action_id = NEW.action_id
       AND action.finding_capacity_reservation_digest = NEW.reservation_digest
  ))
BEGIN
  SELECT RAISE(ABORT, 'INVARIANT_review_finding_capacity_reservation_state');
END;

CREATE TRIGGER review_finding_capacity_reservations_state_delete
BEFORE DELETE ON review_finding_capacity_reservations
BEGIN
  SELECT RAISE(ABORT, 'INVARIANT_review_finding_capacity_reservation_state');
END;

CREATE TABLE review_terminal_sequence_high_water (
  run_id TEXT PRIMARY KEY,
  terminal_sequence INTEGER NOT NULL CHECK (terminal_sequence >= 0),
  revision INTEGER NOT NULL CHECK (revision >= 1),
  FOREIGN KEY (run_id) REFERENCES runs(run_id)
);

CREATE TABLE review_certification_cuts (
  run_id TEXT NOT NULL,
  target_generation INTEGER NOT NULL CHECK (target_generation >= 1),
  predecessor_binding_generation INTEGER NOT NULL CHECK (predecessor_binding_generation >= 1),
  predecessor_binding_digest TEXT NOT NULL,
  terminal_sequence_high_water INTEGER NOT NULL CHECK (terminal_sequence_high_water >= 0),
  lifecycle_custody_agent_id TEXT NOT NULL,
  lifecycle_custody_id TEXT NOT NULL,
  lifecycle_custody_revision INTEGER NOT NULL CHECK (lifecycle_custody_revision >= 1),
  lifecycle_adoption_evidence_digest TEXT NOT NULL,
  cut_digest TEXT NOT NULL UNIQUE,
  created_at INTEGER NOT NULL,
  PRIMARY KEY (run_id, target_generation, lifecycle_custody_agent_id,
    lifecycle_custody_id, lifecycle_custody_revision),
  UNIQUE (run_id, target_generation, lifecycle_custody_agent_id,
    lifecycle_custody_id, lifecycle_custody_revision,
    predecessor_binding_generation, cut_digest),
  FOREIGN KEY (run_id, lifecycle_custody_agent_id, lifecycle_custody_id,
      lifecycle_custody_revision)
    REFERENCES lifecycle_rotation_custody_revisions(run_id, agent_id, custody_id, revision)
);

CREATE TABLE review_completion_targets (
  run_id TEXT NOT NULL,
  target_generation INTEGER NOT NULL CHECK (target_generation >= 1),
  preparation_id TEXT NOT NULL UNIQUE,
  review_subject_digest TEXT NOT NULL,
  task_id TEXT NOT NULL,
  reviewed_artifact_id TEXT NOT NULL,
  reviewed_artifact_revision INTEGER NOT NULL CHECK (reviewed_artifact_revision >= 1),
  publication_lineage_digest TEXT NOT NULL,
  delivery_review_basis_revision INTEGER NOT NULL CHECK (delivery_review_basis_revision >= 1),
  delivery_review_basis_digest TEXT NOT NULL,
  repository_source_state_digest TEXT NOT NULL,
  bundle_generation INTEGER NOT NULL CHECK (bundle_generation >= 1),
  bundle_digest TEXT NOT NULL,
  manifest_body_digest TEXT NOT NULL,
  manifest_root_digest TEXT NOT NULL,
  coverage_digest TEXT NOT NULL,
  bundle_search_index_digest TEXT NOT NULL,
  risk_read_map_digest TEXT NOT NULL,
  mandatory_read_set_digest TEXT NOT NULL,
  mandatory_read_count INTEGER NOT NULL CHECK (mandatory_read_count BETWEEN 0 AND 80),
  mandatory_read_bytes INTEGER NOT NULL CHECK (mandatory_read_bytes BETWEEN 0 AND 6291456),
  object_count INTEGER NOT NULL CHECK (object_count BETWEEN 0 AND 16384),
  chunk_count INTEGER NOT NULL CHECK (chunk_count BETWEEN 0 AND 32768),
  total_object_bytes INTEGER NOT NULL CHECK (total_object_bytes BETWEEN 0 AND 67108864),
  profile_id TEXT NOT NULL,
  profile_schema_digest TEXT NOT NULL,
  resolved_profile_digest TEXT NOT NULL,
  initial_chair_binding_digest TEXT NOT NULL,
  state TEXT NOT NULL CHECK (state IN ('current','superseded')),
  created_at INTEGER NOT NULL,
  PRIMARY KEY (run_id, target_generation),
  UNIQUE (run_id, target_generation, review_subject_digest),
  UNIQUE (run_id, review_subject_digest),
  FOREIGN KEY (run_id, task_id) REFERENCES tasks(run_id, task_id),
  FOREIGN KEY (reviewed_artifact_id, reviewed_artifact_revision)
    REFERENCES artifacts(artifact_id, revision),
  FOREIGN KEY (reviewed_artifact_id, reviewed_artifact_revision, publication_lineage_digest)
    REFERENCES artifact_publication_lineage(artifact_id, artifact_revision, lineage_digest),
  FOREIGN KEY (run_id, bundle_generation) REFERENCES review_bundles(run_id, bundle_generation)
);

CREATE UNIQUE INDEX one_current_review_completion_target_per_run
  ON review_completion_targets(run_id)
  WHERE state = 'current';

CREATE TABLE review_target_chair_bindings (
  run_id TEXT NOT NULL,
  target_generation INTEGER NOT NULL CHECK (target_generation >= 1),
  binding_generation INTEGER NOT NULL CHECK (binding_generation >= 1),
  predecessor_binding_generation INTEGER,
  predecessor_binding_digest TEXT,
  predecessor_certification_cut_sequence INTEGER,
  predecessor_certification_cut_digest TEXT,
  predecessor_certification_cut_custody_agent_id TEXT,
  predecessor_certification_cut_custody_id TEXT,
  predecessor_certification_cut_custody_revision INTEGER,
  agent_id TEXT NOT NULL,
  principal_generation INTEGER NOT NULL CHECK (principal_generation >= 1),
  chair_lease_generation INTEGER NOT NULL CHECK (chair_lease_generation >= 1),
  provider_session_generation INTEGER NOT NULL CHECK (provider_session_generation >= 1),
  bridge_generation INTEGER NOT NULL CHECK (bridge_generation >= 1),
  adapter_id TEXT NOT NULL,
  adapter_contract_digest TEXT NOT NULL,
  model_family TEXT NOT NULL,
  model TEXT NOT NULL,
  review_subject_digest TEXT NOT NULL,
  route_receipt_digest TEXT NOT NULL,
  profile_digest TEXT NOT NULL,
  task_id TEXT NOT NULL,
  reviewed_artifact_id TEXT NOT NULL,
  delivery_review_basis_digest TEXT NOT NULL,
  repository_source_state_digest TEXT NOT NULL,
  bundle_digest TEXT NOT NULL,
  lifecycle_custody_id TEXT,
  lifecycle_custody_revision INTEGER,
  checkpoint_digest TEXT,
  lifecycle_adoption_evidence_digest TEXT,
  binding_digest TEXT NOT NULL,
  created_at INTEGER NOT NULL,
  PRIMARY KEY (run_id, target_generation, binding_generation),
  UNIQUE (run_id, target_generation, binding_generation, binding_digest),
  FOREIGN KEY (run_id, target_generation, review_subject_digest)
    REFERENCES review_completion_targets(run_id, target_generation, review_subject_digest),
  FOREIGN KEY (run_id, target_generation,
      predecessor_certification_cut_custody_agent_id,
      predecessor_certification_cut_custody_id,
      predecessor_certification_cut_custody_revision,
      predecessor_binding_generation, predecessor_certification_cut_digest)
    REFERENCES review_certification_cuts(run_id, target_generation,
      lifecycle_custody_agent_id, lifecycle_custody_id,
      lifecycle_custody_revision, predecessor_binding_generation, cut_digest),
  FOREIGN KEY (run_id, agent_id, lifecycle_custody_id, lifecycle_custody_revision)
    REFERENCES lifecycle_rotation_custody_revisions(run_id, agent_id, custody_id, revision),
  CHECK ((binding_generation = 1 AND predecessor_binding_generation IS NULL AND
      predecessor_binding_digest IS NULL AND
      predecessor_certification_cut_sequence IS NULL AND
      predecessor_certification_cut_digest IS NULL AND
      predecessor_certification_cut_custody_agent_id IS NULL AND
      predecessor_certification_cut_custody_id IS NULL AND
      predecessor_certification_cut_custody_revision IS NULL AND
      lifecycle_custody_id IS NULL AND lifecycle_custody_revision IS NULL AND
      checkpoint_digest IS NULL AND lifecycle_adoption_evidence_digest IS NULL) OR
    (binding_generation > 1 AND predecessor_binding_generation = binding_generation - 1 AND
      predecessor_binding_digest IS NOT NULL AND
      predecessor_certification_cut_sequence IS NOT NULL AND
      predecessor_certification_cut_digest IS NOT NULL AND
      predecessor_certification_cut_custody_agent_id = agent_id AND
      predecessor_certification_cut_custody_id IS NOT NULL AND
      predecessor_certification_cut_custody_revision IS NOT NULL AND
      lifecycle_custody_id = predecessor_certification_cut_custody_id AND
      lifecycle_custody_revision = predecessor_certification_cut_custody_revision AND
      checkpoint_digest IS NOT NULL AND lifecycle_adoption_evidence_digest IS NOT NULL))
);

CREATE TABLE review_target_chair_binding_heads (
  run_id TEXT NOT NULL,
  target_generation INTEGER NOT NULL,
  active_binding_generation INTEGER NOT NULL CHECK (active_binding_generation >= 1),
  revision INTEGER NOT NULL CHECK (revision >= 1),
  PRIMARY KEY (run_id, target_generation),
  FOREIGN KEY (run_id, target_generation, active_binding_generation)
    REFERENCES review_target_chair_bindings(run_id, target_generation, binding_generation)
);

CREATE TABLE review_target_rebind_receipts (
  run_id TEXT NOT NULL,
  target_generation INTEGER NOT NULL,
  lifecycle_custody_agent_id TEXT NOT NULL,
  lifecycle_custody_id TEXT NOT NULL,
  lifecycle_custody_revision INTEGER NOT NULL,
  command_id TEXT NOT NULL,
  review_subject_digest TEXT NOT NULL,
  prior_binding_generation INTEGER NOT NULL CHECK (prior_binding_generation >= 1),
  new_binding_generation INTEGER NOT NULL CHECK (new_binding_generation >= 2),
  prior_binding_digest TEXT NOT NULL,
  new_binding_digest TEXT NOT NULL,
  lifecycle_adoption_digest TEXT NOT NULL,
  bundle_digest TEXT NOT NULL,
  profile_digest TEXT NOT NULL,
  slot_head_set_digest TEXT NOT NULL,
  open_and_repair_finding_set_digest TEXT NOT NULL,
  rebind_receipt_digest TEXT NOT NULL UNIQUE,
  created_at INTEGER NOT NULL,
  PRIMARY KEY (run_id, target_generation, lifecycle_custody_agent_id,
    lifecycle_custody_id, lifecycle_custody_revision),
  UNIQUE (command_id),
  FOREIGN KEY (run_id, target_generation, review_subject_digest)
    REFERENCES review_completion_targets(run_id, target_generation, review_subject_digest),
  FOREIGN KEY (run_id, target_generation, prior_binding_generation)
    REFERENCES review_target_chair_bindings(run_id, target_generation, binding_generation),
  FOREIGN KEY (run_id, target_generation, new_binding_generation)
    REFERENCES review_target_chair_bindings(run_id, target_generation, binding_generation),
  FOREIGN KEY (run_id, lifecycle_custody_agent_id, lifecycle_custody_id,
      lifecycle_custody_revision)
    REFERENCES lifecycle_rotation_custody_revisions(run_id, agent_id, custody_id, revision),
  CHECK (new_binding_generation = prior_binding_generation + 1)
);

CREATE TABLE review_certifying_slot_availability_revisions (
  project_session_id TEXT NOT NULL,
  profile_id TEXT NOT NULL,
  profile_schema_digest TEXT NOT NULL,
  target_chair_family TEXT NOT NULL,
  slot TEXT NOT NULL CHECK (slot IN ('native','other-primary','cursor-grok','agy-gemini')),
  adapter_id TEXT NOT NULL,
  adapter_contract_digest TEXT NOT NULL,
  provider_family TEXT NOT NULL,
  model TEXT NOT NULL,
  source_mode TEXT NOT NULL,
  runtime_identity_digest TEXT NOT NULL,
  platform_identity_digest TEXT NOT NULL,
  availability_revision INTEGER NOT NULL CHECK (availability_revision >= 1),
  state TEXT NOT NULL CHECK (state IN ('available','unavailable')),
  reason TEXT CHECK (reason IS NULL OR reason IN (
    'adapter-inactive','contract-mismatch','confinement-unproved',
    'portal-unavailable','provider-runtime-unavailable'
  )),
  created_at INTEGER NOT NULL,
  PRIMARY KEY (project_session_id, profile_id, profile_schema_digest,
    target_chair_family, slot, adapter_id, adapter_contract_digest,
    provider_family, model, source_mode, runtime_identity_digest,
    platform_identity_digest, availability_revision),
  CHECK ((state = 'available') = (reason IS NULL))
);

CREATE TABLE review_certifying_slot_availability_heads (
  project_session_id TEXT NOT NULL,
  profile_id TEXT NOT NULL,
  profile_schema_digest TEXT NOT NULL,
  target_chair_family TEXT NOT NULL,
  slot TEXT NOT NULL,
  adapter_id TEXT NOT NULL,
  adapter_contract_digest TEXT NOT NULL,
  provider_family TEXT NOT NULL,
  model TEXT NOT NULL,
  source_mode TEXT NOT NULL,
  runtime_identity_digest TEXT NOT NULL,
  platform_identity_digest TEXT NOT NULL,
  current_availability_revision INTEGER NOT NULL CHECK (current_availability_revision >= 1),
  revision INTEGER NOT NULL CHECK (revision >= 1),
  PRIMARY KEY (project_session_id, profile_id, profile_schema_digest,
    target_chair_family, slot, adapter_id, adapter_contract_digest,
    provider_family, model, source_mode, runtime_identity_digest,
    platform_identity_digest),
  FOREIGN KEY (project_session_id, profile_id, profile_schema_digest,
      target_chair_family, slot, adapter_id, adapter_contract_digest,
      provider_family, model, source_mode, runtime_identity_digest,
      platform_identity_digest, current_availability_revision)
    REFERENCES review_certifying_slot_availability_revisions(
      project_session_id, profile_id, profile_schema_digest,
      target_chair_family, slot, adapter_id, adapter_contract_digest,
      provider_family, model, source_mode, runtime_identity_digest,
      platform_identity_digest, availability_revision)
);

CREATE TABLE review_profile_snapshots (
  run_id TEXT NOT NULL,
  target_generation INTEGER NOT NULL,
  profile_id TEXT NOT NULL,
  profile_schema_digest TEXT NOT NULL,
  profile_catalogue_digest TEXT NOT NULL,
  target_chair_family TEXT NOT NULL,
  resolved_profile_json TEXT NOT NULL CHECK (json_valid(resolved_profile_json)),
  resolved_profile_digest TEXT NOT NULL UNIQUE,
  created_at INTEGER NOT NULL,
  PRIMARY KEY (run_id, target_generation),
  FOREIGN KEY (run_id, target_generation) REFERENCES review_completion_targets(run_id, target_generation)
);

CREATE TABLE review_profile_slots (
  run_id TEXT NOT NULL,
  target_generation INTEGER NOT NULL,
  slot TEXT NOT NULL CHECK (slot IN ('native','other-primary','cursor-grok','agy-gemini')),
  ordinal INTEGER NOT NULL CHECK (ordinal BETWEEN 0 AND 3),
  adapter_class TEXT NOT NULL,
  adapter_id TEXT NOT NULL,
  adapter_contract_digest TEXT NOT NULL,
  provider_family TEXT NOT NULL,
  model TEXT NOT NULL,
  requested_effort TEXT,
  resolved_effort_kind TEXT NOT NULL CHECK (resolved_effort_kind IN ('applied','inapplicable')),
  resolved_effort_value TEXT,
  source_mode TEXT NOT NULL,
  runtime_identity_digest TEXT NOT NULL,
  platform_identity_digest TEXT NOT NULL,
  maximum_provider_turns INTEGER NOT NULL CHECK (maximum_provider_turns >= 1),
  maximum_internal_steps INTEGER NOT NULL CHECK (maximum_internal_steps >= 1),
  maximum_portal_reads INTEGER NOT NULL CHECK (maximum_portal_reads >= 1),
  reviewer_family_relation TEXT NOT NULL CHECK (reviewer_family_relation IN (
    'same-family-exempt','distinct-family-proved','same-family-forbidden','family-unproved'
  )),
  slot_json TEXT NOT NULL CHECK (json_valid(slot_json)),
  slot_digest TEXT NOT NULL,
  PRIMARY KEY (run_id, target_generation, slot),
  UNIQUE (run_id, target_generation, ordinal),
  FOREIGN KEY (run_id, target_generation) REFERENCES review_profile_snapshots(run_id, target_generation),
  CHECK ((resolved_effort_kind = 'applied') = (resolved_effort_value IS NOT NULL)),
  CHECK ((slot = 'native' AND reviewer_family_relation = 'same-family-exempt') OR
    (slot != 'native' AND reviewer_family_relation = 'distinct-family-proved'))
);

CREATE TABLE review_portal_process_custody (
  adapter_id TEXT NOT NULL,
  action_id TEXT NOT NULL,
  contract_digest TEXT NOT NULL,
  daemon_instance_id TEXT NOT NULL,
  supervisor_pid INTEGER NOT NULL CHECK (supervisor_pid > 0),
  supervisor_start_time INTEGER NOT NULL,
  provider_root_pid INTEGER NOT NULL CHECK (provider_root_pid > 0),
  provider_root_start_time INTEGER NOT NULL,
  process_group_id INTEGER NOT NULL CHECK (process_group_id > 0),
  session_id INTEGER NOT NULL CHECK (session_id > 0),
  executable_identity_digest TEXT NOT NULL,
  ancestry_manifest_digest TEXT NOT NULL,
  custody_directory_path TEXT NOT NULL,
  custody_directory_device TEXT NOT NULL,
  custody_directory_inode TEXT NOT NULL,
  socket_basename TEXT NOT NULL,
  socket_file_digest TEXT NOT NULL,
  capsule_basename TEXT NOT NULL,
  capsule_file_digest TEXT NOT NULL,
  control_fd_number INTEGER NOT NULL CHECK (control_fd_number = 3),
  connection_state TEXT NOT NULL CHECK (connection_state IN ('waiting','consumed','closed')),
  process_state TEXT NOT NULL CHECK (process_state IN (
    'preparing','running','terminating','cleaned','integrity-failure'
  )),
  cleanup_generation INTEGER NOT NULL CHECK (cleanup_generation >= 0),
  cleanup_evidence_digest TEXT,
  revision INTEGER NOT NULL CHECK (revision >= 1),
  created_at INTEGER NOT NULL,
  updated_at INTEGER NOT NULL,
  PRIMARY KEY (adapter_id, action_id),
  FOREIGN KEY (adapter_id, action_id) REFERENCES provider_action_pair_preflights(adapter_id, action_id),
  CHECK (instr(socket_basename, '/') = 0 AND socket_basename NOT IN ('.','..')),
  CHECK (instr(capsule_basename, '/') = 0 AND capsule_basename NOT IN ('.','..')),
  CHECK ((process_state IN ('cleaned','integrity-failure')) =
    (cleanup_evidence_digest IS NOT NULL))
);

CREATE TABLE provider_failure_substitution_events (
  adapter_id TEXT NOT NULL,
  action_id TEXT NOT NULL,
  event_generation INTEGER NOT NULL CHECK (event_generation >= 1),
  run_id TEXT NOT NULL,
  requested_family TEXT NOT NULL,
  requested_model TEXT NOT NULL,
  resolved_adapter_id TEXT,
  resolved_family TEXT,
  resolved_model TEXT,
  code TEXT NOT NULL CHECK (code IN (
    'adapter-unavailable','adapter-contract-mismatch','provider-unavailable',
    'provider-timeout','provider-rejected','provider-response-invalid',
    'route-rejected','model-unavailable','capability-unavailable',
    'quota-exhausted','substitution-applied','optional-leg-degraded'
  )),
  evidence_digest TEXT NOT NULL,
  created_at INTEGER NOT NULL,
  PRIMARY KEY (adapter_id, action_id, event_generation),
  FOREIGN KEY (adapter_id, action_id)
    REFERENCES provider_action_pair_preflights(adapter_id, action_id),
  CHECK ((resolved_adapter_id IS NULL) = (resolved_family IS NULL)),
  CHECK ((resolved_family IS NULL) = (resolved_model IS NULL))
);

CREATE TABLE provider_action_routes (
  adapter_id TEXT NOT NULL,
  action_id TEXT NOT NULL,
  run_id TEXT NOT NULL,
  task_id TEXT NOT NULL,
  certifying_review INTEGER NOT NULL CHECK (certifying_review IN (0,1)),
  target_generation INTEGER,
  slot TEXT,
  slot_head_generation INTEGER,
  attempt_generation INTEGER,
  reviewed_artifact_id TEXT,
  reviewed_artifact_revision INTEGER,
  publication_lineage_digest TEXT,
  bundle_digest TEXT,
  manifest_root_digest TEXT,
  coverage_digest TEXT,
  bundle_search_index_digest TEXT,
  risk_read_map_digest TEXT,
  mandatory_read_set_digest TEXT,
  profile_digest TEXT,
  profile_schema_digest TEXT,
  final_prompt_digest TEXT,
  chair_binding_generation INTEGER,
  route_request_json TEXT NOT NULL CHECK (json_valid(route_request_json)),
  route_request_digest TEXT NOT NULL,
  route_receipt_json TEXT NOT NULL CHECK (json_valid(route_receipt_json)),
  route_receipt_digest TEXT NOT NULL,
  requested_adapter_id TEXT NOT NULL,
  resolved_adapter_id TEXT NOT NULL,
  adapter_contract_digest TEXT NOT NULL,
  provider_family TEXT NOT NULL,
  model TEXT NOT NULL,
  requested_effort TEXT,
  resolved_effort_kind TEXT NOT NULL CHECK (resolved_effort_kind IN ('applied','inapplicable')),
  resolved_effort_value TEXT,
  capability_snapshot_generation INTEGER NOT NULL CHECK (capability_snapshot_generation >= 1),
  capability_snapshot_digest TEXT NOT NULL,
  capability_body_digest TEXT NOT NULL,
  effective_configuration_id TEXT NOT NULL,
  effective_configuration_revision INTEGER NOT NULL CHECK (effective_configuration_revision >= 1),
  effective_configuration_ref_digest TEXT NOT NULL,
  requested_configuration_digest TEXT NOT NULL,
  effective_route_configuration_digest TEXT NOT NULL,
  deployed_route_admission_json TEXT NOT NULL CHECK (json_valid(deployed_route_admission_json)),
  deployed_route_admission_digest TEXT NOT NULL,
  route_policy_revision INTEGER NOT NULL CHECK (route_policy_revision >= 1),
  harness_revision INTEGER NOT NULL CHECK (harness_revision >= 1),
  harness_digest TEXT NOT NULL,
  context_policy_revision INTEGER NOT NULL CHECK (context_policy_revision >= 1),
  context_policy_digest TEXT NOT NULL,
  permission_profile_digest TEXT NOT NULL,
  discovery_surface_evidence_id TEXT NOT NULL,
  discovery_surface_evidence_revision INTEGER NOT NULL CHECK (discovery_surface_evidence_revision >= 1),
  discovery_surface_digest TEXT NOT NULL,
  admission_digest TEXT NOT NULL UNIQUE,
  created_at INTEGER NOT NULL,
  PRIMARY KEY (adapter_id, action_id),
  FOREIGN KEY (run_id, adapter_id, action_id)
    REFERENCES provider_action_pair_preflights(run_id, adapter_id, action_id),
  FOREIGN KEY (run_id, adapter_id, action_id)
    REFERENCES provider_actions(run_id, adapter_id, action_id),
  FOREIGN KEY (adapter_id, capability_snapshot_generation,
      capability_snapshot_digest, capability_body_digest)
    REFERENCES adapter_capability_snapshots(adapter_id, snapshot_generation,
      snapshot_digest, capability_body_digest),
  FOREIGN KEY (effective_configuration_id, effective_configuration_revision,
      effective_configuration_ref_digest)
    REFERENCES adapter_effective_configurations(configuration_id,
      configuration_revision, configuration_digest),
  FOREIGN KEY (discovery_surface_evidence_id,
      discovery_surface_evidence_revision, discovery_surface_digest)
    REFERENCES discovery_surface_manifests(evidence_id, evidence_revision, manifest_digest),
  CHECK (requested_adapter_id = adapter_id AND resolved_adapter_id = adapter_id),
  CHECK ((resolved_effort_kind = 'applied') = (resolved_effort_value IS NOT NULL)),
  CHECK (resolved_effort_kind != 'inapplicable' OR requested_effort IS NULL),
  CHECK ((certifying_review = 0 AND target_generation IS NULL AND slot IS NULL AND
      slot_head_generation IS NULL AND attempt_generation IS NULL AND
      reviewed_artifact_id IS NULL AND reviewed_artifact_revision IS NULL AND
      publication_lineage_digest IS NULL AND bundle_digest IS NULL AND
      manifest_root_digest IS NULL AND coverage_digest IS NULL AND
      profile_digest IS NULL AND profile_schema_digest IS NULL AND
      final_prompt_digest IS NULL AND chair_binding_generation IS NULL) OR
    (certifying_review = 1 AND target_generation IS NOT NULL AND
      slot IN ('native','other-primary','cursor-grok','agy-gemini') AND
      slot_head_generation IS NOT NULL AND attempt_generation IS NOT NULL AND
      reviewed_artifact_id IS NOT NULL AND reviewed_artifact_revision IS NOT NULL AND
      publication_lineage_digest IS NOT NULL AND bundle_digest IS NOT NULL AND
      manifest_root_digest IS NOT NULL AND coverage_digest IS NOT NULL AND
      profile_digest IS NOT NULL AND profile_schema_digest IS NOT NULL AND
      final_prompt_digest IS NOT NULL AND chair_binding_generation IS NOT NULL))
);

CREATE UNIQUE INDEX one_nonterminal_certifying_action_per_slot_head
  ON provider_action_routes(run_id, target_generation, slot, slot_head_generation)
  WHERE certifying_review = 1;

CREATE TABLE provider_review_terminal_journal (
  adapter_id TEXT NOT NULL,
  action_id TEXT NOT NULL,
  run_id TEXT NOT NULL,
  target_generation INTEGER NOT NULL,
  slot TEXT NOT NULL,
  attempt_generation INTEGER NOT NULL CHECK (attempt_generation >= 1),
  terminal_kind TEXT NOT NULL CHECK (terminal_kind IN (
    'safe-answer','unusable-answer','provider-terminal-failure',
    'terminal-no-effect','integrity-terminal','retired-unknown'
  )),
  terminal_sequence INTEGER NOT NULL CHECK (terminal_sequence >= 1),
  terminal_input_digest TEXT NOT NULL,
  private_answer_digest TEXT,
  private_result_digest TEXT,
  private_adapter_result_digest TEXT,
  authenticated_usage_digest TEXT,
  read_journal_digest TEXT,
  public_terminal_projection_digest TEXT NOT NULL,
  evidence_mutation_receipt_digest TEXT,
  created_at INTEGER NOT NULL,
  PRIMARY KEY (adapter_id, action_id),
  UNIQUE (adapter_id, action_id, target_generation, slot, attempt_generation),
  UNIQUE (run_id, terminal_sequence),
  FOREIGN KEY (adapter_id, action_id) REFERENCES provider_action_routes(adapter_id, action_id)
);

CREATE TABLE provider_review_results (
  adapter_id TEXT NOT NULL,
  action_id TEXT NOT NULL,
  result_kind TEXT NOT NULL CHECK (result_kind IN (
    'safe-answer','unusable-answer','provider-terminal-failure'
  )),
  provider_answer_digest TEXT,
  provider_answer_length INTEGER CHECK (provider_answer_length IS NULL OR provider_answer_length >= 0),
  safe_result_json TEXT CHECK (safe_result_json IS NULL OR json_valid(safe_result_json)),
  result_digest TEXT NOT NULL UNIQUE,
  finding_set_digest TEXT,
  resolved_finding_set_digest TEXT,
  classifier_digest TEXT,
  secret_selector_digest TEXT,
  failure_code TEXT CHECK (failure_code IS NULL OR failure_code IN (
    'max-turns-exhausted','provider-rejected','terminal-no-answer','adapter-terminal-failure'
  )),
  private_diagnostic_digest TEXT,
  created_at INTEGER NOT NULL,
  PRIMARY KEY (adapter_id, action_id),
  FOREIGN KEY (adapter_id, action_id)
    REFERENCES provider_review_terminal_journal(adapter_id, action_id),
  CHECK ((result_kind = 'safe-answer' AND provider_answer_digest IS NOT NULL AND
      provider_answer_length IS NOT NULL AND safe_result_json IS NOT NULL AND
      finding_set_digest IS NOT NULL AND resolved_finding_set_digest IS NOT NULL AND
      classifier_digest IS NOT NULL AND secret_selector_digest IS NOT NULL AND
      failure_code IS NULL AND private_diagnostic_digest IS NULL) OR
    (result_kind = 'unusable-answer' AND provider_answer_digest IS NOT NULL AND
      provider_answer_length IS NOT NULL AND safe_result_json IS NULL AND
      finding_set_digest IS NULL AND resolved_finding_set_digest IS NULL AND
      classifier_digest IS NOT NULL AND secret_selector_digest IS NOT NULL AND
      failure_code IS NULL AND private_diagnostic_digest IS NULL) OR
    (result_kind = 'provider-terminal-failure' AND provider_answer_digest IS NULL AND
      provider_answer_length IS NULL AND safe_result_json IS NULL AND
      finding_set_digest IS NULL AND resolved_finding_set_digest IS NULL AND
      classifier_digest IS NULL AND secret_selector_digest IS NULL AND
      failure_code IS NOT NULL AND private_diagnostic_digest IS NOT NULL))
);

CREATE TRIGGER provider_review_results_complete_finding_sets
BEFORE INSERT ON provider_review_results
WHEN NEW.result_kind = 'safe-answer' AND (
  NOT EXISTS (
    SELECT 1 FROM review_finding_sets_complete complete
     WHERE complete.finding_set_digest = NEW.finding_set_digest
  ) OR NOT EXISTS (
    SELECT 1 FROM review_finding_sets_complete complete
     WHERE complete.finding_set_digest = NEW.resolved_finding_set_digest
  )
)
BEGIN SELECT RAISE(ABORT, 'INVARIANT_review_finding_set_incomplete'); END;

CREATE TABLE review_slot_heads (
  run_id TEXT NOT NULL,
  target_generation INTEGER NOT NULL,
  slot TEXT NOT NULL CHECK (slot IN ('native','other-primary','cursor-grok','agy-gemini')),
  head_generation INTEGER NOT NULL CHECK (head_generation >= 0),
  head_evidence_id TEXT,
  latest_attempt_generation INTEGER NOT NULL CHECK (latest_attempt_generation >= 0),
  latest_action_adapter_id TEXT,
  latest_action_id TEXT,
  latest_action_state TEXT,
  open_finding_set_digest TEXT NOT NULL,
  repair_required_finding_set_digest TEXT NOT NULL,
  prior_target_generation INTEGER,
  prior_target_head_evidence_id TEXT,
  revision INTEGER NOT NULL CHECK (revision >= 1),
  updated_at INTEGER NOT NULL,
  PRIMARY KEY (run_id, target_generation, slot),
  FOREIGN KEY (run_id, target_generation) REFERENCES review_completion_targets(run_id, target_generation),
  FOREIGN KEY (open_finding_set_digest) REFERENCES review_finding_sets(finding_set_digest),
  FOREIGN KEY (repair_required_finding_set_digest) REFERENCES review_finding_sets(finding_set_digest),
  CHECK ((latest_action_adapter_id IS NULL) = (latest_action_id IS NULL)),
  CHECK ((latest_action_id IS NULL) = (latest_action_state IS NULL)),
  CHECK ((head_generation = 0) = (head_evidence_id IS NULL))
);

CREATE TRIGGER review_slot_heads_complete_finding_sets_insert
BEFORE INSERT ON review_slot_heads
WHEN NOT EXISTS (
    SELECT 1 FROM review_finding_sets_complete complete
     WHERE complete.finding_set_digest = NEW.open_finding_set_digest
  ) OR NOT EXISTS (
    SELECT 1 FROM review_finding_sets_complete complete
     WHERE complete.finding_set_digest = NEW.repair_required_finding_set_digest
  )
BEGIN SELECT RAISE(ABORT, 'INVARIANT_review_finding_set_incomplete'); END;

CREATE TRIGGER review_slot_heads_complete_finding_sets_update
BEFORE UPDATE OF open_finding_set_digest,repair_required_finding_set_digest ON review_slot_heads
WHEN NOT EXISTS (
    SELECT 1 FROM review_finding_sets_complete complete
     WHERE complete.finding_set_digest = NEW.open_finding_set_digest
  ) OR NOT EXISTS (
    SELECT 1 FROM review_finding_sets_complete complete
     WHERE complete.finding_set_digest = NEW.repair_required_finding_set_digest
  )
BEGIN SELECT RAISE(ABORT, 'INVARIANT_review_finding_set_incomplete'); END;

CREATE TABLE provider_review_evidence (
  run_id TEXT NOT NULL,
  evidence_id TEXT NOT NULL,
  target_generation INTEGER NOT NULL,
  slot TEXT NOT NULL,
  prior_head_generation INTEGER NOT NULL CHECK (prior_head_generation >= 0),
  new_head_generation INTEGER NOT NULL CHECK (new_head_generation >= 1),
  prior_evidence_id TEXT,
  prior_open_finding_set_digest TEXT NOT NULL,
  provider_resolved_finding_set_digest TEXT NOT NULL,
  accepted_resolved_finding_set_digest TEXT NOT NULL,
  current_finding_set_digest TEXT NOT NULL,
  new_open_finding_set_digest TEXT NOT NULL,
  repair_required_finding_set_digest TEXT NOT NULL,
  reservation_digest TEXT NOT NULL,
  terminal_sequence INTEGER NOT NULL CHECK (terminal_sequence >= 1),
  certification_basis_at_terminal_digest TEXT NOT NULL,
  adapter_id TEXT NOT NULL,
  action_id TEXT NOT NULL,
  result_digest TEXT NOT NULL,
  route_receipt_digest TEXT NOT NULL,
  bundle_digest TEXT NOT NULL,
  coverage_digest TEXT NOT NULL,
  profile_digest TEXT NOT NULL,
  chair_binding_generation INTEGER NOT NULL CHECK (chair_binding_generation >= 1),
  route_observation_digest TEXT,
  actual_route_identity_digest TEXT,
  task_id TEXT NOT NULL,
  answer_digest TEXT NOT NULL,
  read_coverage_digest TEXT NOT NULL,
  reviewer_family_relation TEXT NOT NULL CHECK (reviewer_family_relation IN (
    'same-family-exempt','distinct-family-proved','same-family-forbidden','family-unproved'
  )),
  certifying INTEGER NOT NULL CHECK (certifying IN (0,1)),
  evidence_digest TEXT NOT NULL UNIQUE,
  created_at INTEGER NOT NULL,
  PRIMARY KEY (run_id, evidence_id),
  UNIQUE (run_id, target_generation, slot, new_head_generation),
  FOREIGN KEY (adapter_id, action_id) REFERENCES provider_review_results(adapter_id, action_id),
  FOREIGN KEY (run_id, target_generation, slot)
    REFERENCES review_slot_heads(run_id, target_generation, slot),
  FOREIGN KEY (prior_open_finding_set_digest) REFERENCES review_finding_sets(finding_set_digest),
  FOREIGN KEY (provider_resolved_finding_set_digest) REFERENCES review_finding_sets(finding_set_digest),
  FOREIGN KEY (accepted_resolved_finding_set_digest) REFERENCES review_finding_sets(finding_set_digest),
  FOREIGN KEY (current_finding_set_digest) REFERENCES review_finding_sets(finding_set_digest),
  FOREIGN KEY (new_open_finding_set_digest) REFERENCES review_finding_sets(finding_set_digest),
  FOREIGN KEY (repair_required_finding_set_digest) REFERENCES review_finding_sets(finding_set_digest),
  FOREIGN KEY (run_id, adapter_id, action_id, reservation_digest)
    REFERENCES review_finding_capacity_reservations(
      run_id, adapter_id, action_id, reservation_digest
    ),
  CHECK (new_head_generation = prior_head_generation + 1),
  CHECK (actual_route_identity_digest IS NULL OR route_observation_digest IS NOT NULL)
);

CREATE TRIGGER provider_review_evidence_complete_finding_sets
BEFORE INSERT ON provider_review_evidence
WHEN NOT EXISTS (
    SELECT 1 FROM review_finding_sets_complete complete
     WHERE complete.finding_set_digest = NEW.prior_open_finding_set_digest
  ) OR NOT EXISTS (
    SELECT 1 FROM review_finding_sets_complete complete
     WHERE complete.finding_set_digest = NEW.provider_resolved_finding_set_digest
  ) OR NOT EXISTS (
    SELECT 1 FROM review_finding_sets_complete complete
     WHERE complete.finding_set_digest = NEW.accepted_resolved_finding_set_digest
  ) OR NOT EXISTS (
    SELECT 1 FROM review_finding_sets_complete complete
     WHERE complete.finding_set_digest = NEW.current_finding_set_digest
  ) OR NOT EXISTS (
    SELECT 1 FROM review_finding_sets_complete complete
     WHERE complete.finding_set_digest = NEW.new_open_finding_set_digest
  ) OR NOT EXISTS (
    SELECT 1 FROM review_finding_sets_complete complete
     WHERE complete.finding_set_digest = NEW.repair_required_finding_set_digest
  )
BEGIN SELECT RAISE(ABORT, 'INVARIANT_review_finding_set_incomplete'); END;

CREATE TABLE review_evidence_annotations (
  run_id TEXT NOT NULL,
  evidence_id TEXT NOT NULL,
  annotation_revision INTEGER NOT NULL CHECK (annotation_revision >= 1),
  prior_annotation_revision INTEGER,
  command_id TEXT NOT NULL UNIQUE,
  chair_binding_generation INTEGER NOT NULL CHECK (chair_binding_generation >= 1),
  disposition TEXT NOT NULL CHECK (disposition IN (
    'substantiated','unsubstantiated','duplicate','needs-more-evidence'
  )),
  note TEXT NOT NULL CHECK (length(CAST(note AS BLOB)) <= 512),
  note_digest TEXT NOT NULL,
  annotation_digest TEXT NOT NULL UNIQUE,
  created_at INTEGER NOT NULL,
  PRIMARY KEY (run_id, evidence_id, annotation_revision),
  FOREIGN KEY (run_id, evidence_id) REFERENCES provider_review_evidence(run_id, evidence_id),
  CHECK ((annotation_revision = 1 AND prior_annotation_revision IS NULL) OR
    (annotation_revision > 1 AND prior_annotation_revision = annotation_revision - 1))
);

CREATE TABLE review_evidence_annotation_heads (
  run_id TEXT NOT NULL,
  evidence_id TEXT NOT NULL,
  current_annotation_revision INTEGER NOT NULL CHECK (current_annotation_revision >= 1),
  revision INTEGER NOT NULL CHECK (revision >= 1),
  PRIMARY KEY (run_id, evidence_id),
  FOREIGN KEY (run_id, evidence_id, current_annotation_revision)
    REFERENCES review_evidence_annotations(run_id, evidence_id, annotation_revision)
);

CREATE TABLE route_integrity_recoveries (
  run_id TEXT NOT NULL,
  adapter_id TEXT NOT NULL,
  action_id TEXT NOT NULL,
  recovery_generation INTEGER NOT NULL CHECK (recovery_generation >= 1),
  owner_daemon_generation INTEGER NOT NULL CHECK (owner_daemon_generation >= 1),
  state TEXT NOT NULL CHECK (state IN (
    'detected','inspecting','terminal-proved-no-effect','terminal-proved-usage',
    'awaiting-human-retire','terminal-retired-unknown'
  )),
  reason TEXT NOT NULL CHECK (reason IN (
    'intact-effect-ambiguity','route-row-missing','route-row-conflict',
    'route-receipt-mismatch','target-binding-invalid','bundle-binding-invalid',
    'prompt-binding-invalid','profile-binding-invalid','lineage-binding-invalid'
  )),
  terminal_disposition TEXT CHECK (terminal_disposition IS NULL OR
    terminal_disposition IN (
      'proved-no-effect-release','exact-usage-settled',
      'conservative-full-ceiling-settled','full-ceiling-retired'
    )),
  reservation_id TEXT NOT NULL,
  reservation_digest TEXT NOT NULL,
  route_state TEXT NOT NULL CHECK (route_state IN ('present','missing','integrity-failed')),
  route_receipt_digest TEXT,
  recovery_evidence_digest TEXT,
  lookup_state TEXT NOT NULL CHECK (lookup_state IN ('not-attempted','in-flight','completed')),
  lookup_evidence_digest TEXT,
  settlement_digest TEXT,
  created_at INTEGER NOT NULL,
  updated_at INTEGER NOT NULL,
  PRIMARY KEY (adapter_id, action_id),
  FOREIGN KEY (adapter_id, action_id) REFERENCES provider_action_pair_preflights(adapter_id, action_id),
  CHECK ((route_state = 'present' AND route_receipt_digest IS NOT NULL AND
      recovery_evidence_digest IS NULL) OR
    (route_state IN ('missing','integrity-failed') AND route_receipt_digest IS NULL AND
      recovery_evidence_digest IS NOT NULL)),
  CHECK ((lookup_state = 'completed') = (lookup_evidence_digest IS NOT NULL)),
  CHECK ((state IN ('detected','inspecting','awaiting-human-retire') AND
      terminal_disposition IS NULL AND settlement_digest IS NULL) OR
    (state = 'terminal-proved-no-effect' AND
      terminal_disposition = 'proved-no-effect-release' AND settlement_digest IS NOT NULL) OR
    (state = 'terminal-proved-usage' AND terminal_disposition IN (
      'exact-usage-settled','conservative-full-ceiling-settled'
    ) AND settlement_digest IS NOT NULL) OR
    (state = 'terminal-retired-unknown' AND
      terminal_disposition = 'full-ceiling-retired' AND settlement_digest IS NOT NULL))
);

CREATE TABLE adapter_capability_snapshots (
  adapter_id TEXT NOT NULL,
  snapshot_generation INTEGER NOT NULL CHECK (snapshot_generation >= 1),
  snapshot_id TEXT NOT NULL UNIQUE,
  adapter_contract_digest TEXT NOT NULL,
  host_id TEXT NOT NULL,
  host_version TEXT NOT NULL,
  source TEXT NOT NULL CHECK (source IN (
    'runtime-discovery','version-pinned-conformance','unavailable'
  )),
  observed_at INTEGER NOT NULL,
  expires_at INTEGER NOT NULL CHECK (expires_at > observed_at),
  capability_body_digest TEXT NOT NULL,
  snapshot_json TEXT NOT NULL CHECK (json_valid(snapshot_json)),
  snapshot_digest TEXT NOT NULL UNIQUE,
  created_at INTEGER NOT NULL,
  PRIMARY KEY (adapter_id, snapshot_generation),
  UNIQUE (adapter_id, snapshot_generation, snapshot_digest, capability_body_digest)
);

CREATE TABLE adapter_capability_current (
  adapter_id TEXT PRIMARY KEY,
  snapshot_generation INTEGER NOT NULL CHECK (snapshot_generation >= 1),
  snapshot_digest TEXT NOT NULL,
  capability_body_digest TEXT NOT NULL,
  revision INTEGER NOT NULL CHECK (revision >= 1),
  FOREIGN KEY (adapter_id, snapshot_generation, snapshot_digest,
      capability_body_digest)
    REFERENCES adapter_capability_snapshots(adapter_id, snapshot_generation,
      snapshot_digest, capability_body_digest)
);

CREATE TABLE discovery_surface_manifests (
  evidence_id TEXT NOT NULL,
  evidence_revision INTEGER NOT NULL CHECK (evidence_revision >= 1),
  artifact_path TEXT NOT NULL,
  artifact_digest TEXT NOT NULL,
  host_id TEXT NOT NULL,
  host_version TEXT NOT NULL,
  provider_profile TEXT NOT NULL,
  raw_native_mode TEXT NOT NULL,
  permission_profile_digest TEXT NOT NULL,
  manifest_json TEXT NOT NULL CHECK (json_valid(manifest_json)),
  manifest_digest TEXT NOT NULL,
  created_at INTEGER NOT NULL,
  PRIMARY KEY (evidence_id, evidence_revision),
  UNIQUE (evidence_id, evidence_revision, manifest_digest),
  FOREIGN KEY (evidence_id, evidence_revision) REFERENCES artifacts(artifact_id, revision),
  CHECK (artifact_digest = manifest_digest)
);

CREATE TABLE adapter_activation_subjects (
  adapter_id TEXT NOT NULL,
  activation_id TEXT NOT NULL,
  activation_revision INTEGER NOT NULL CHECK (activation_revision >= 1),
  evidence_id TEXT NOT NULL,
  evidence_revision INTEGER NOT NULL CHECK (evidence_revision >= 1),
  created_at INTEGER NOT NULL,
  PRIMARY KEY (adapter_id, activation_id, activation_revision),
  UNIQUE (evidence_id, evidence_revision),
  FOREIGN KEY (evidence_id, evidence_revision) REFERENCES artifacts(artifact_id, revision)
);

CREATE TABLE adapter_provider_smoke_subjects (
  adapter_id TEXT NOT NULL,
  smoke_id TEXT NOT NULL,
  action_adapter_id TEXT NOT NULL,
  action_id TEXT NOT NULL,
  evidence_id TEXT NOT NULL,
  evidence_revision INTEGER NOT NULL CHECK (evidence_revision >= 1),
  created_at INTEGER NOT NULL,
  PRIMARY KEY (adapter_id, smoke_id),
  UNIQUE (action_adapter_id, action_id),
  UNIQUE (evidence_id, evidence_revision),
  FOREIGN KEY (evidence_id, evidence_revision) REFERENCES artifacts(artifact_id, revision),
  FOREIGN KEY (action_adapter_id, action_id)
    REFERENCES provider_action_pair_preflights(adapter_id, action_id)
);

CREATE TABLE adapter_effective_configurations (
  configuration_id TEXT NOT NULL,
  configuration_revision INTEGER NOT NULL CHECK (configuration_revision >= 1),
  adapter_id TEXT NOT NULL,
  adapter_contract_digest TEXT NOT NULL,
  executable_identity_digest TEXT NOT NULL,
  capability_snapshot_generation INTEGER NOT NULL CHECK (capability_snapshot_generation >= 1),
  capability_snapshot_digest TEXT NOT NULL,
  capability_body_digest TEXT NOT NULL,
  subject_kind TEXT NOT NULL CHECK (subject_kind IN ('activation','provider-smoke','provider-action')),
  subject_ref_digest TEXT NOT NULL,
  subject_activation_id TEXT,
  subject_activation_revision INTEGER,
  subject_smoke_id TEXT,
  subject_action_adapter_id TEXT,
  subject_action_id TEXT,
  activation_configuration_id TEXT,
  activation_configuration_revision INTEGER,
  activation_configuration_digest TEXT,
  requested_configuration_digest TEXT NOT NULL,
  effective_configuration_digest TEXT NOT NULL,
  permission_profile_digest TEXT NOT NULL,
  discovery_surface_evidence_id TEXT NOT NULL,
  discovery_surface_evidence_revision INTEGER NOT NULL,
  evidence_id TEXT NOT NULL,
  evidence_revision INTEGER NOT NULL CHECK (evidence_revision >= 1),
  configuration_json TEXT NOT NULL CHECK (json_valid(configuration_json)),
  configuration_digest TEXT NOT NULL UNIQUE,
  created_at INTEGER NOT NULL,
  PRIMARY KEY (configuration_id, configuration_revision),
  UNIQUE (configuration_id, configuration_revision, configuration_digest),
  UNIQUE (evidence_id, evidence_revision),
  FOREIGN KEY (evidence_id, evidence_revision) REFERENCES artifacts(artifact_id, revision),
  FOREIGN KEY (adapter_id, capability_snapshot_generation,
      capability_snapshot_digest, capability_body_digest)
    REFERENCES adapter_capability_snapshots(adapter_id, snapshot_generation,
      snapshot_digest, capability_body_digest),
  FOREIGN KEY (discovery_surface_evidence_id, discovery_surface_evidence_revision)
    REFERENCES discovery_surface_manifests(evidence_id, evidence_revision),
  FOREIGN KEY (adapter_id, subject_activation_id, subject_activation_revision)
    REFERENCES adapter_activation_subjects(adapter_id, activation_id, activation_revision),
  FOREIGN KEY (adapter_id, subject_smoke_id)
    REFERENCES adapter_provider_smoke_subjects(adapter_id, smoke_id),
  FOREIGN KEY (subject_action_adapter_id, subject_action_id)
    REFERENCES provider_action_pair_preflights(adapter_id, action_id),
  FOREIGN KEY (activation_configuration_id, activation_configuration_revision,
      activation_configuration_digest)
    REFERENCES adapter_effective_configurations(configuration_id,
      configuration_revision, configuration_digest),
  CHECK ((subject_kind = 'activation' AND subject_activation_id IS NOT NULL AND
      subject_activation_revision IS NOT NULL AND subject_smoke_id IS NULL AND
      subject_action_adapter_id IS NULL AND subject_action_id IS NULL) OR
    (subject_kind = 'provider-smoke' AND subject_activation_id IS NULL AND
      subject_activation_revision IS NULL AND subject_smoke_id IS NOT NULL AND
      subject_action_adapter_id IS NULL AND subject_action_id IS NULL) OR
    (subject_kind = 'provider-action' AND subject_activation_id IS NULL AND
      subject_activation_revision IS NULL AND subject_smoke_id IS NULL AND
      subject_action_adapter_id IS NOT NULL AND
      subject_action_adapter_id = adapter_id AND subject_action_id IS NOT NULL)),
  CHECK ((subject_kind = 'activation' AND activation_configuration_id IS NULL AND
      activation_configuration_revision IS NULL AND activation_configuration_digest IS NULL) OR
    (subject_kind IN ('provider-smoke','provider-action') AND
      activation_configuration_id IS NOT NULL AND
      activation_configuration_revision IS NOT NULL AND
      activation_configuration_digest IS NOT NULL))
);

CREATE UNIQUE INDEX one_effective_configuration_per_activation_subject
  ON adapter_effective_configurations(adapter_id, subject_activation_id, subject_activation_revision)
  WHERE subject_kind = 'activation';

CREATE UNIQUE INDEX one_effective_configuration_per_smoke_subject
  ON adapter_effective_configurations(adapter_id, subject_smoke_id)
  WHERE subject_kind = 'provider-smoke';

CREATE UNIQUE INDEX one_effective_configuration_per_provider_action
  ON adapter_effective_configurations(subject_action_adapter_id, subject_action_id)
  WHERE subject_kind = 'provider-action';

CREATE TABLE provider_action_route_dispatches (
  adapter_id TEXT NOT NULL,
  action_id TEXT NOT NULL,
  dispatch_ordinal INTEGER NOT NULL CHECK (dispatch_ordinal >= 1),
  admission_digest TEXT NOT NULL,
  capability_snapshot_generation INTEGER NOT NULL CHECK (capability_snapshot_generation >= 1),
  capability_snapshot_digest TEXT NOT NULL,
  capability_body_digest TEXT NOT NULL,
  effective_configuration_id TEXT NOT NULL,
  effective_configuration_revision INTEGER NOT NULL CHECK (effective_configuration_revision >= 1),
  effective_configuration_ref_digest TEXT NOT NULL,
  permission_profile_digest TEXT NOT NULL,
  discovery_surface_evidence_id TEXT NOT NULL,
  discovery_surface_evidence_revision INTEGER NOT NULL CHECK (discovery_surface_evidence_revision >= 1),
  dispatched_at INTEGER NOT NULL,
  dispatch_json TEXT NOT NULL CHECK (json_valid(dispatch_json)),
  dispatch_digest TEXT NOT NULL UNIQUE,
  PRIMARY KEY (adapter_id, action_id, dispatch_ordinal),
  FOREIGN KEY (adapter_id, action_id) REFERENCES provider_action_routes(adapter_id, action_id),
  FOREIGN KEY (adapter_id, capability_snapshot_generation,
      capability_snapshot_digest, capability_body_digest)
    REFERENCES adapter_capability_snapshots(adapter_id, snapshot_generation,
      snapshot_digest, capability_body_digest),
  FOREIGN KEY (effective_configuration_id, effective_configuration_revision,
      effective_configuration_ref_digest)
    REFERENCES adapter_effective_configurations(configuration_id,
      configuration_revision, configuration_digest),
  FOREIGN KEY (discovery_surface_evidence_id, discovery_surface_evidence_revision)
    REFERENCES discovery_surface_manifests(evidence_id, evidence_revision)
);

CREATE TABLE provider_action_route_observations (
  adapter_id TEXT NOT NULL,
  action_id TEXT NOT NULL,
  admission_digest TEXT NOT NULL,
  observation_json TEXT NOT NULL CHECK (json_valid(observation_json)),
  observation_digest TEXT NOT NULL UNIQUE,
  observed_at INTEGER NOT NULL,
  PRIMARY KEY (adapter_id, action_id),
  FOREIGN KEY (adapter_id, action_id) REFERENCES provider_action_routes(adapter_id, action_id)
);

CREATE TABLE coordination_policy_revisions (
  project_session_id TEXT NOT NULL,
  coordination_run_id TEXT NOT NULL,
  policy_revision INTEGER NOT NULL CHECK (policy_revision >= 1),
  policy_ref TEXT NOT NULL CHECK (
    length(policy_ref) = 71 AND substr(policy_ref, 1, 7) = 'sha256:' AND
    substr(policy_ref, 8) NOT GLOB '*[^0-9a-f]*'
  ),
  policy_digest TEXT NOT NULL CHECK (
    length(policy_digest) = 71 AND substr(policy_digest, 1, 7) = 'sha256:' AND
    substr(policy_digest, 8) NOT GLOB '*[^0-9a-f]*'
  ),
  created_at INTEGER NOT NULL,
  PRIMARY KEY (project_session_id, coordination_run_id, policy_revision),
  UNIQUE (project_session_id, coordination_run_id, policy_revision,
    policy_ref, policy_digest),
  FOREIGN KEY (project_session_id, coordination_run_id)
    REFERENCES runs(project_session_id, run_id)
);

CREATE TABLE coordination_policy_current (
  project_session_id TEXT NOT NULL,
  coordination_run_id TEXT NOT NULL,
  policy_revision INTEGER NOT NULL CHECK (policy_revision >= 1),
  policy_ref TEXT NOT NULL,
  policy_digest TEXT NOT NULL,
  revision INTEGER NOT NULL CHECK (revision >= 1),
  PRIMARY KEY (project_session_id, coordination_run_id),
  FOREIGN KEY (project_session_id, coordination_run_id, policy_revision,
      policy_ref, policy_digest)
    REFERENCES coordination_policy_revisions(project_session_id,
      coordination_run_id, policy_revision, policy_ref, policy_digest)
);

CREATE TRIGGER coordination_policy_revisions_contiguous_insert
BEFORE INSERT ON coordination_policy_revisions
WHEN NEW.policy_revision <> COALESCE((
  SELECT MAX(policy_revision) + 1
    FROM coordination_policy_revisions
   WHERE project_session_id = NEW.project_session_id
     AND coordination_run_id = NEW.coordination_run_id
), 1)
  OR NEW.policy_revision > COALESCE((
    SELECT policy_revision + 1
      FROM coordination_policy_current
     WHERE project_session_id = NEW.project_session_id
       AND coordination_run_id = NEW.coordination_run_id
  ), 1)
BEGIN
  SELECT RAISE(ABORT, 'INVARIANT_coordination_policy_revision_contiguous');
END;

CREATE TRIGGER coordination_policy_revisions_immutable_update
BEFORE UPDATE ON coordination_policy_revisions
BEGIN
  SELECT RAISE(ABORT, 'INVARIANT_coordination_policy_history_immutable');
END;

CREATE TRIGGER coordination_policy_revisions_immutable_delete
BEFORE DELETE ON coordination_policy_revisions
BEGIN
  SELECT RAISE(ABORT, 'INVARIANT_coordination_policy_history_immutable');
END;

CREATE TRIGGER coordination_policy_current_insert
BEFORE INSERT ON coordination_policy_current
WHEN NEW.revision <> 1 OR NEW.policy_revision <> (
  SELECT MAX(policy_revision)
    FROM coordination_policy_revisions
   WHERE project_session_id = NEW.project_session_id
     AND coordination_run_id = NEW.coordination_run_id
)
BEGIN
  SELECT RAISE(ABORT, 'INVARIANT_coordination_policy_current_cas');
END;

CREATE TRIGGER coordination_policy_current_update
BEFORE UPDATE ON coordination_policy_current
WHEN NEW.project_session_id IS NOT OLD.project_session_id
  OR NEW.coordination_run_id IS NOT OLD.coordination_run_id
  OR NEW.revision <> OLD.revision + 1
  OR NEW.policy_revision <> OLD.policy_revision + 1
  OR NEW.policy_revision <> (
    SELECT MAX(policy_revision)
      FROM coordination_policy_revisions
     WHERE project_session_id = NEW.project_session_id
       AND coordination_run_id = NEW.coordination_run_id
  )
BEGIN
  SELECT RAISE(ABORT, 'INVARIANT_coordination_policy_current_cas');
END;

CREATE TRIGGER coordination_policy_current_delete
BEFORE DELETE ON coordination_policy_current
BEGIN
  SELECT RAISE(ABORT, 'INVARIANT_coordination_policy_current_cas');
END;

CREATE TABLE topology_wave_plans (
  project_session_id TEXT NOT NULL,
  coordination_run_id TEXT NOT NULL,
  task_id TEXT NOT NULL,
  wave_id TEXT NOT NULL,
  wave_revision INTEGER NOT NULL CHECK (wave_revision >= 1),
  predecessor_wave_id TEXT,
  predecessor_wave_revision INTEGER,
  predecessor_plan_digest TEXT,
  chair_agent_id TEXT NOT NULL,
  principal_generation INTEGER NOT NULL CHECK (principal_generation >= 1),
  chair_lease_generation INTEGER NOT NULL CHECK (chair_lease_generation >= 1),
  authority_revision INTEGER NOT NULL CHECK (authority_revision >= 1),
  authority_ref TEXT NOT NULL,
  authority_digest TEXT NOT NULL,
  policy_revision INTEGER NOT NULL CHECK (policy_revision >= 1),
  policy_ref TEXT NOT NULL,
  policy_digest TEXT NOT NULL,
  rationale_evidence_id TEXT NOT NULL,
  rationale_evidence_revision INTEGER NOT NULL CHECK (rationale_evidence_revision >= 1),
  state TEXT NOT NULL CHECK (state IN (
    'proposed','approved','started','completed','superseded','cancelled'
  )),
  plan_json TEXT NOT NULL CHECK (json_valid(plan_json)),
  plan_digest TEXT NOT NULL UNIQUE,
  created_at INTEGER NOT NULL,
  PRIMARY KEY (project_session_id, coordination_run_id, task_id, wave_id, wave_revision),
  UNIQUE (project_session_id, coordination_run_id, task_id, wave_revision),
  UNIQUE (project_session_id, coordination_run_id, task_id,
    wave_id, wave_revision, plan_digest),
  FOREIGN KEY (rationale_evidence_id, rationale_evidence_revision)
    REFERENCES artifacts(artifact_id, revision),
  FOREIGN KEY (coordination_run_id, task_id) REFERENCES tasks(run_id, task_id),
  FOREIGN KEY (coordination_run_id, chair_agent_id) REFERENCES agents(run_id, agent_id),
  FOREIGN KEY (project_session_id, coordination_run_id, authority_revision, authority_ref)
    REFERENCES run_authority_revisions(project_session_id, coordination_run_id,
      authority_revision, authority_ref),
  FOREIGN KEY (project_session_id, coordination_run_id, policy_revision,
      policy_ref, policy_digest)
    REFERENCES coordination_policy_revisions(project_session_id,
      coordination_run_id, policy_revision, policy_ref, policy_digest),
  FOREIGN KEY (project_session_id, coordination_run_id, task_id,
      predecessor_wave_id, predecessor_wave_revision, predecessor_plan_digest)
    REFERENCES topology_wave_plans(project_session_id, coordination_run_id, task_id,
      wave_id, wave_revision, plan_digest),
  CHECK ((wave_revision = 1 AND predecessor_wave_id IS NULL AND
      predecessor_wave_revision IS NULL AND predecessor_plan_digest IS NULL) OR
    (wave_revision > 1 AND predecessor_wave_id IS NOT NULL AND
      predecessor_wave_revision IS NOT NULL AND predecessor_plan_digest IS NOT NULL))
);

CREATE TRIGGER topology_wave_plans_codec_insert
BEFORE INSERT ON topology_wave_plans
WHEN
  json_type(NEW.plan_json) <> 'object'
  OR (SELECT COUNT(*) FROM json_each(NEW.plan_json)) <> 22
  OR EXISTS (
    SELECT 1 FROM json_each(NEW.plan_json)
     WHERE key NOT IN (
       'schemaVersion','projectSessionId','coordinationRunId','taskId','waveId',
       'waveRevision','predecessor','dependencies','decomposability','topology',
       'chair','stageOwners','writePartitions','contention','budget','stopConditions',
       'authority','policy','state','rationaleRef','createdAt','planDigest'
     )
  )
  OR COALESCE(json_type(NEW.plan_json, '$.schemaVersion'), '') <> 'integer'
  OR json_extract(NEW.plan_json, '$.schemaVersion') IS NOT 1
  OR COALESCE(json_type(NEW.plan_json, '$.projectSessionId'), '') <> 'text'
  OR json_extract(NEW.plan_json, '$.projectSessionId') IS NOT NEW.project_session_id
  OR COALESCE(json_type(NEW.plan_json, '$.coordinationRunId'), '') <> 'text'
  OR json_extract(NEW.plan_json, '$.coordinationRunId') IS NOT NEW.coordination_run_id
  OR COALESCE(json_type(NEW.plan_json, '$.taskId'), '') <> 'text'
  OR json_extract(NEW.plan_json, '$.taskId') IS NOT NEW.task_id
  OR COALESCE(json_type(NEW.plan_json, '$.waveId'), '') <> 'text'
  OR json_extract(NEW.plan_json, '$.waveId') IS NOT NEW.wave_id
  OR COALESCE(json_type(NEW.plan_json, '$.waveRevision'), '') <> 'integer'
  OR json_extract(NEW.plan_json, '$.waveRevision') IS NOT NEW.wave_revision
  OR COALESCE(json_type(NEW.plan_json, '$.predecessor'), '') NOT IN ('null','object')
  OR COALESCE(json_type(NEW.plan_json, '$.dependencies'), '') <> 'array'
  OR COALESCE(json_type(NEW.plan_json, '$.decomposability'), '') <> 'object'
  OR COALESCE(json_type(NEW.plan_json, '$.topology'), '') <> 'object'
  OR COALESCE(json_type(NEW.plan_json, '$.chair'), '') <> 'object'
  OR COALESCE(json_type(NEW.plan_json, '$.stageOwners'), '') <> 'array'
  OR COALESCE(json_type(NEW.plan_json, '$.writePartitions'), '') <> 'array'
  OR COALESCE(json_type(NEW.plan_json, '$.contention'), '') <> 'object'
  OR COALESCE(json_type(NEW.plan_json, '$.budget'), '') <> 'object'
  OR COALESCE(json_type(NEW.plan_json, '$.stopConditions'), '') <> 'array'
  OR COALESCE(json_type(NEW.plan_json, '$.authority'), '') <> 'object'
  OR COALESCE(json_type(NEW.plan_json, '$.policy'), '') <> 'object'
  OR COALESCE(json_type(NEW.plan_json, '$.state'), '') <> 'text'
  OR json_extract(NEW.plan_json, '$.state') IS NOT NEW.state
  OR COALESCE(json_type(NEW.plan_json, '$.rationaleRef'), '') <> 'object'
  OR COALESCE(json_type(NEW.plan_json, '$.createdAt'), '') <> 'integer'
  OR json_extract(NEW.plan_json, '$.createdAt') IS NOT NEW.created_at
  OR COALESCE(json_type(NEW.plan_json, '$.planDigest'), '') <> 'text'
  OR json_extract(NEW.plan_json, '$.planDigest') IS NOT NEW.plan_digest
  OR length(NEW.plan_digest) <> 71
  OR substr(NEW.plan_digest, 1, 7) <> 'sha256:'
  OR substr(NEW.plan_digest, 8) GLOB '*[^0-9a-f]*'
  OR fabric_topology_plan_digest(NEW.plan_json) IS NOT NEW.plan_digest
  OR NEW.plan_json <> json(NEW.plan_json)
  OR EXISTS (
    SELECT 1
      FROM json_tree(NEW.plan_json) object
      JOIN json_tree(NEW.plan_json) earlier ON earlier.parent = object.id
      JOIN json_tree(NEW.plan_json) later
        ON later.parent = object.id AND later.id > earlier.id
     WHERE object.type = 'object'
       AND CAST(earlier.key AS TEXT) >= CAST(later.key AS TEXT)
  )
  OR (SELECT COUNT(*) FROM json_each(NEW.plan_json, '$.chair')) <> 3
  OR EXISTS (
    SELECT 1 FROM json_each(NEW.plan_json, '$.chair')
     WHERE key NOT IN ('agentId','chairLeaseGeneration','principalGeneration')
  )
  OR COALESCE(json_type(NEW.plan_json, '$.chair.agentId'), '') <> 'text'
  OR COALESCE(json_type(NEW.plan_json, '$.chair.principalGeneration'), '') <> 'integer'
  OR json_extract(NEW.plan_json, '$.chair.principalGeneration') < 1
  OR json_extract(NEW.plan_json, '$.chair.agentId') IS NOT NEW.chair_agent_id
  OR json_extract(NEW.plan_json, '$.chair.principalGeneration') IS NOT NEW.principal_generation
  OR COALESCE(json_type(NEW.plan_json, '$.chair.chairLeaseGeneration'), '') <> 'integer'
  OR json_extract(NEW.plan_json, '$.chair.chairLeaseGeneration') < 1
  OR json_extract(NEW.plan_json, '$.chair.chairLeaseGeneration') IS NOT NEW.chair_lease_generation
  OR (SELECT COUNT(*) FROM json_each(NEW.plan_json, '$.authority')) <> 3
  OR EXISTS (
    SELECT 1 FROM json_each(NEW.plan_json, '$.authority')
     WHERE key NOT IN ('authorityDigest','authorityRef','authorityRevision')
  )
  OR COALESCE(json_type(NEW.plan_json, '$.authority.authorityRevision'), '') <> 'integer'
  OR COALESCE(json_type(NEW.plan_json, '$.authority.authorityRef'), '') <> 'text'
  OR COALESCE(json_type(NEW.plan_json, '$.authority.authorityDigest'), '') <> 'text'
  OR json_extract(NEW.plan_json, '$.authority.authorityRevision') IS NOT NEW.authority_revision
  OR json_extract(NEW.plan_json, '$.authority.authorityRef') IS NOT NEW.authority_ref
  OR json_extract(NEW.plan_json, '$.authority.authorityDigest') IS NOT NEW.authority_digest
  OR (SELECT COUNT(*) FROM json_each(NEW.plan_json, '$.policy')) <> 3
  OR EXISTS (
    SELECT 1 FROM json_each(NEW.plan_json, '$.policy')
     WHERE key NOT IN ('policyDigest','policyRef','policyRevision')
  )
  OR COALESCE(json_type(NEW.plan_json, '$.policy.policyRevision'), '') <> 'integer'
  OR COALESCE(json_type(NEW.plan_json, '$.policy.policyRef'), '') <> 'text'
  OR COALESCE(json_type(NEW.plan_json, '$.policy.policyDigest'), '') <> 'text'
  OR json_extract(NEW.plan_json, '$.policy.policyRevision') IS NOT NEW.policy_revision
  OR json_extract(NEW.plan_json, '$.policy.policyRef') IS NOT NEW.policy_ref
  OR json_extract(NEW.plan_json, '$.policy.policyDigest') IS NOT NEW.policy_digest
  OR (SELECT COUNT(*) FROM json_each(NEW.plan_json, '$.rationaleRef')) <> 2
  OR EXISTS (
    SELECT 1 FROM json_each(NEW.plan_json, '$.rationaleRef')
     WHERE key NOT IN ('evidenceId','evidenceRevision')
  )
  OR COALESCE(json_type(NEW.plan_json, '$.rationaleRef.evidenceId'), '') <> 'text'
  OR COALESCE(json_type(NEW.plan_json, '$.rationaleRef.evidenceRevision'), '') <> 'integer'
  OR json_extract(NEW.plan_json, '$.rationaleRef.evidenceRevision') < 1
  OR json_extract(NEW.plan_json, '$.rationaleRef.evidenceId') IS NOT NEW.rationale_evidence_id
  OR json_extract(NEW.plan_json, '$.rationaleRef.evidenceRevision') IS NOT NEW.rationale_evidence_revision
  OR (SELECT COUNT(*) FROM json_each(NEW.plan_json, '$.decomposability')) <> 2
  OR EXISTS (
    SELECT 1 FROM json_each(NEW.plan_json, '$.decomposability')
     WHERE key NOT IN ('evidenceRef','kind')
  )
  OR json_extract(NEW.plan_json, '$.decomposability.kind') NOT IN (
    'atomic','decomposable','conditionally-decomposable'
  )
  OR COALESCE(json_type(NEW.plan_json, '$.decomposability.evidenceRef'), '') <> 'text'
  OR (SELECT COUNT(*) FROM json_each(NEW.plan_json, '$.topology')) <> 3
  OR EXISTS (
    SELECT 1 FROM json_each(NEW.plan_json, '$.topology')
     WHERE key NOT IN ('executionShape','maximumConcurrentAgents','mode')
  )
  OR json_extract(NEW.plan_json, '$.topology.executionShape') NOT IN (
    'single-owner','fabric-explicit','host-native'
  )
  OR json_extract(NEW.plan_json, '$.topology.mode') NOT IN (
    'serial','parallel','fan-out-fan-in','dynamic'
  )
  OR COALESCE(json_type(NEW.plan_json, '$.topology.maximumConcurrentAgents'), '') <> 'integer'
  OR json_extract(NEW.plan_json, '$.topology.maximumConcurrentAgents') < 1
  OR (SELECT COUNT(*) FROM json_each(NEW.plan_json, '$.contention')) <> 3
  OR EXISTS (
    SELECT 1 FROM json_each(NEW.plan_json, '$.contention')
     WHERE key NOT IN ('evidenceRef','mode','serializationOwnerAgentId')
  )
  OR json_extract(NEW.plan_json, '$.contention.mode') NOT IN (
    'none','serialized','disjoint-partitions'
  )
  OR COALESCE(json_type(NEW.plan_json, '$.contention.evidenceRef'), '') <> 'text'
  OR COALESCE(json_type(NEW.plan_json, '$.contention.serializationOwnerAgentId'), '')
       NOT IN ('null','text')
  OR (json_extract(NEW.plan_json, '$.contention.mode') = 'serialized') IS NOT
     (json_type(NEW.plan_json, '$.contention.serializationOwnerAgentId') = 'text')
  OR (SELECT COUNT(*) FROM json_each(NEW.plan_json, '$.budget')) <> 4
  OR EXISTS (
    SELECT 1 FROM json_each(NEW.plan_json, '$.budget')
     WHERE key NOT IN (
       'maximumParallelAgents','providerTurns','toolCalls','wallClockSeconds'
     )
  )
  OR EXISTS (
    SELECT 1 FROM json_each(NEW.plan_json, '$.budget')
     WHERE type <> 'integer' OR value < 0
  )
  OR json_extract(NEW.plan_json, '$.budget.maximumParallelAgents') < 1
  OR json_extract(NEW.plan_json, '$.budget.maximumParallelAgents') >
     json_extract(NEW.plan_json, '$.topology.maximumConcurrentAgents')
  OR EXISTS (
    SELECT 1 FROM json_each(NEW.plan_json, '$.dependencies') dependency
     WHERE dependency.type <> 'object'
        OR (SELECT COUNT(*) FROM json_each(dependency.value)) <> 3
        OR EXISTS (
          SELECT 1 FROM json_each(dependency.value)
           WHERE key NOT IN ('dependencyTaskId','evidenceRef','requiredState')
        )
        OR COALESCE(json_type(dependency.value, '$.dependencyTaskId'), '') <> 'text'
        OR COALESCE(json_type(dependency.value, '$.evidenceRef'), '') <> 'text'
        OR json_extract(dependency.value, '$.requiredState') NOT IN ('ready','completed')
  )
  OR EXISTS (
    SELECT 1
      FROM json_each(NEW.plan_json, '$.dependencies') earlier
      JOIN json_each(NEW.plan_json, '$.dependencies') later
        ON CAST(later.key AS INTEGER) = CAST(earlier.key AS INTEGER) + 1
     WHERE json_extract(earlier.value, '$.dependencyTaskId') >=
           json_extract(later.value, '$.dependencyTaskId')
  )
  OR json_array_length(NEW.plan_json, '$.stageOwners') < 1
  OR EXISTS (
    SELECT 1 FROM json_each(NEW.plan_json, '$.stageOwners') owner
     WHERE owner.type <> 'object'
        OR (SELECT COUNT(*) FROM json_each(owner.value)) <> 4
        OR EXISTS (
          SELECT 1 FROM json_each(owner.value)
           WHERE key NOT IN ('ownerAgentId','stageId','taskId','writePartitionId')
        )
        OR COALESCE(json_type(owner.value, '$.ownerAgentId'), '') <> 'text'
        OR COALESCE(json_type(owner.value, '$.stageId'), '') <> 'text'
        OR COALESCE(json_type(owner.value, '$.taskId'), '') <> 'text'
        OR COALESCE(json_type(owner.value, '$.writePartitionId'), '') NOT IN ('null','text')
  )
  OR EXISTS (
    SELECT 1
      FROM json_each(NEW.plan_json, '$.stageOwners') earlier
      JOIN json_each(NEW.plan_json, '$.stageOwners') later
        ON CAST(later.key AS INTEGER) = CAST(earlier.key AS INTEGER) + 1
     WHERE json_extract(earlier.value, '$.stageId') >=
           json_extract(later.value, '$.stageId')
  )
  OR EXISTS (
    SELECT 1 FROM json_each(NEW.plan_json, '$.writePartitions') partition
     WHERE partition.type <> 'object'
        OR (SELECT COUNT(*) FROM json_each(partition.value)) <> 5
        OR EXISTS (
          SELECT 1 FROM json_each(partition.value)
           WHERE key NOT IN ('authorityRef','mode','ownerAgentId','partitionId','pathSetDigest')
        )
        OR COALESCE(json_type(partition.value, '$.authorityRef'), '') <> 'text'
        OR json_extract(partition.value, '$.authorityRef') IS NOT NEW.authority_ref
        OR json_extract(partition.value, '$.mode') NOT IN ('exclusive-write','shared-read')
        OR COALESCE(json_type(partition.value, '$.ownerAgentId'), '') <> 'text'
        OR COALESCE(json_type(partition.value, '$.partitionId'), '') <> 'text'
        OR COALESCE(json_type(partition.value, '$.pathSetDigest'), '') <> 'text'
        OR length(json_extract(partition.value, '$.pathSetDigest')) <> 71
        OR substr(json_extract(partition.value, '$.pathSetDigest'), 1, 7) <> 'sha256:'
        OR substr(json_extract(partition.value, '$.pathSetDigest'), 8) GLOB '*[^0-9a-f]*'
  )
  OR EXISTS (
    SELECT 1
      FROM json_each(NEW.plan_json, '$.writePartitions') earlier
      JOIN json_each(NEW.plan_json, '$.writePartitions') later
        ON CAST(later.key AS INTEGER) = CAST(earlier.key AS INTEGER) + 1
     WHERE json_extract(earlier.value, '$.partitionId') >=
           json_extract(later.value, '$.partitionId')
  )
  OR EXISTS (
    SELECT 1 FROM json_each(NEW.plan_json, '$.stageOwners') owner
     WHERE json_type(owner.value, '$.writePartitionId') = 'text'
       AND NOT EXISTS (
         SELECT 1 FROM json_each(NEW.plan_json, '$.writePartitions') partition
          WHERE json_extract(partition.value, '$.partitionId') =
                json_extract(owner.value, '$.writePartitionId')
            AND json_extract(partition.value, '$.ownerAgentId') =
                json_extract(owner.value, '$.ownerAgentId')
       )
  )
  OR EXISTS (
    SELECT 1 FROM json_each(NEW.plan_json, '$.writePartitions') partition
     WHERE NOT EXISTS (
       SELECT 1 FROM json_each(NEW.plan_json, '$.stageOwners') owner
        WHERE json_extract(owner.value, '$.writePartitionId') =
              json_extract(partition.value, '$.partitionId')
          AND json_extract(owner.value, '$.ownerAgentId') =
              json_extract(partition.value, '$.ownerAgentId')
     )
  )
  OR json_array_length(NEW.plan_json, '$.stopConditions') < 1
  OR EXISTS (
    SELECT 1 FROM json_each(NEW.plan_json, '$.stopConditions') condition
     WHERE condition.type <> 'object'
        OR (SELECT COUNT(*) FROM json_each(condition.value)) <> 3
        OR EXISTS (
          SELECT 1 FROM json_each(condition.value)
           WHERE key NOT IN ('conditionId','kind','predicateRef')
        )
        OR COALESCE(json_type(condition.value, '$.conditionId'), '') <> 'text'
        OR json_extract(condition.value, '$.kind') NOT IN (
          'objective-complete','gate-failed','budget-exhausted','human-gate'
        )
        OR COALESCE(json_type(condition.value, '$.predicateRef'), '') <> 'text'
  )
  OR EXISTS (
    SELECT 1
      FROM json_each(NEW.plan_json, '$.stopConditions') earlier
      JOIN json_each(NEW.plan_json, '$.stopConditions') later
        ON CAST(later.key AS INTEGER) = CAST(earlier.key AS INTEGER) + 1
     WHERE json_extract(earlier.value, '$.conditionId') >=
           json_extract(later.value, '$.conditionId')
  )
  OR (NEW.wave_revision = 1 AND json_type(NEW.plan_json, '$.predecessor') <> 'null')
  OR (NEW.wave_revision > 1 AND (
    json_type(NEW.plan_json, '$.predecessor') <> 'object'
    OR (SELECT COUNT(*) FROM json_each(NEW.plan_json, '$.predecessor')) <> 7
    OR EXISTS (
      SELECT 1 FROM json_each(NEW.plan_json, '$.predecessor')
       WHERE key NOT IN (
         'coordinationRunId','planDigest','projectSessionId','schemaVersion',
         'taskId','waveId','waveRevision'
       )
    )
    OR json_extract(NEW.plan_json, '$.predecessor.schemaVersion') IS NOT 1
    OR json_extract(NEW.plan_json, '$.predecessor.projectSessionId') IS NOT NEW.project_session_id
    OR json_extract(NEW.plan_json, '$.predecessor.coordinationRunId') IS NOT NEW.coordination_run_id
    OR json_extract(NEW.plan_json, '$.predecessor.taskId') IS NOT NEW.task_id
    OR json_extract(NEW.plan_json, '$.predecessor.waveId') IS NOT NEW.predecessor_wave_id
    OR json_extract(NEW.plan_json, '$.predecessor.waveRevision') IS NOT NEW.predecessor_wave_revision
    OR json_extract(NEW.plan_json, '$.predecessor.planDigest') IS NOT NEW.predecessor_plan_digest
  ))
BEGIN
  SELECT RAISE(ABORT, 'INVARIANT_topology_wave_plan_codec');
END;

CREATE TRIGGER topology_wave_plans_currency_insert
BEFORE INSERT ON topology_wave_plans
WHEN json_type(NEW.plan_json) = 'object'
  AND (SELECT COUNT(*) FROM json_each(NEW.plan_json)) = 22
  AND json_type(NEW.plan_json, '$.stageOwners') = 'array'
  AND json_type(NEW.plan_json, '$.writePartitions') = 'array'
  AND json_type(NEW.plan_json, '$.dependencies') = 'array'
  AND json_type(NEW.plan_json, '$.decomposability.evidenceRef') = 'text'
  AND json_type(NEW.plan_json, '$.contention.evidenceRef') = 'text'
  AND (
NOT EXISTS (
  SELECT 1
    FROM runs run
    JOIN agents chair
      ON chair.run_id = run.run_id
     AND chair.agent_id = run.chair_agent_id
    JOIN authorities authority
      ON authority.authority_id = chair.authority_id
     AND authority.run_id = run.run_id
    JOIN run_chair_leases chair_lease
      ON chair_lease.project_session_id = run.project_session_id
     AND chair_lease.run_id = run.run_id
     AND chair_lease.lease_id = run.chair_lease_id
     AND chair_lease.holder_agent_id = run.chair_agent_id
     AND chair_lease.generation = run.chair_generation
     AND chair_lease.status = 'active'
    JOIN agent_lifecycle_identity_high_water identity_high_water
      ON identity_high_water.run_id = run.run_id
     AND identity_high_water.agent_id = run.chair_agent_id
    JOIN coordination_policy_current policy_current
      ON policy_current.project_session_id = run.project_session_id
     AND policy_current.coordination_run_id = run.run_id
   WHERE run.project_session_id = NEW.project_session_id
     AND run.run_id = NEW.coordination_run_id
     AND run.chair_agent_id = NEW.chair_agent_id
     AND run.chair_generation = NEW.chair_lease_generation
     AND run.authority_revision = NEW.authority_revision
     AND run.authority_ref = NEW.authority_ref
     AND NEW.authority_digest = NEW.authority_ref
     AND NEW.authority_digest = 'sha256:' || authority.authority_hash
     AND identity_high_water.principal_generation = NEW.principal_generation
     AND policy_current.policy_revision = NEW.policy_revision
     AND policy_current.policy_ref = NEW.policy_ref
     AND policy_current.policy_digest = NEW.policy_digest
)
OR EXISTS (
  SELECT 1 FROM json_each(NEW.plan_json, '$.stageOwners') owner
   WHERE NOT EXISTS (
     SELECT 1 FROM agents current_agent
      WHERE current_agent.run_id = NEW.coordination_run_id
        AND current_agent.agent_id = json_extract(owner.value, '$.ownerAgentId')
        AND current_agent.lifecycle <> 'archived'
   )
)
OR EXISTS (
  SELECT 1 FROM json_each(NEW.plan_json, '$.stageOwners') owner
   WHERE NOT EXISTS (
     SELECT 1 FROM tasks current_task
      WHERE current_task.run_id = NEW.coordination_run_id
        AND current_task.task_id = json_extract(owner.value, '$.taskId')
   )
)
OR EXISTS (
  SELECT 1 FROM json_each(NEW.plan_json, '$.writePartitions') partition
   WHERE NOT EXISTS (
     SELECT 1
       FROM agents owner
       JOIN authorities owner_authority
         ON owner_authority.authority_id = owner.authority_id
        AND owner_authority.run_id = owner.run_id
      WHERE owner.run_id = NEW.coordination_run_id
        AND owner.agent_id = json_extract(partition.value, '$.ownerAgentId')
        AND 'sha256:' || owner_authority.authority_hash =
          json_extract(partition.value, '$.authorityRef')
   )
)
OR EXISTS (
  SELECT 1 FROM json_each(NEW.plan_json, '$.dependencies') dependency
   WHERE NOT EXISTS (
     SELECT 1 FROM tasks dependency_task
      WHERE dependency_task.run_id = NEW.coordination_run_id
        AND dependency_task.task_id = json_extract(
          dependency.value, '$.dependencyTaskId'
        )
        AND (
          (json_extract(dependency.value, '$.requiredState') = 'ready'
            AND dependency_task.state = 'ready') OR
          (json_extract(dependency.value, '$.requiredState') = 'completed'
            AND dependency_task.state = 'complete')
        )
   )
   OR NOT (
     json_extract(dependency.value, '$.evidenceRef') =
       json_extract(dependency.value, '$.dependencyTaskId')
     OR EXISTS (
       SELECT 1
         FROM artifacts evidence
         JOIN project_sessions evidence_session
           ON evidence_session.project_id = evidence.project_id
        WHERE evidence_session.project_session_id = NEW.project_session_id
          AND evidence.registry_state = 'active'
          AND (evidence.artifact_id = json_extract(dependency.value, '$.evidenceRef')
            OR evidence.sha256 = json_extract(dependency.value, '$.evidenceRef'))
          AND (evidence.project_session_id IS NULL OR
            evidence.project_session_id = NEW.project_session_id)
          AND (evidence.run_id IS NULL OR evidence.run_id = NEW.coordination_run_id)
     )
   )
)
OR NOT EXISTS (
  SELECT 1
    FROM artifacts evidence
    JOIN project_sessions evidence_session
      ON evidence_session.project_id = evidence.project_id
   WHERE evidence_session.project_session_id = NEW.project_session_id
     AND evidence.registry_state = 'active'
     AND (evidence.artifact_id = json_extract(
       NEW.plan_json, '$.decomposability.evidenceRef'
     ) OR evidence.sha256 = json_extract(
       NEW.plan_json, '$.decomposability.evidenceRef'
     ))
     AND (evidence.project_session_id IS NULL OR
       evidence.project_session_id = NEW.project_session_id)
     AND (evidence.run_id IS NULL OR evidence.run_id = NEW.coordination_run_id)
)
OR NOT EXISTS (
  SELECT 1
    FROM artifacts evidence
    JOIN project_sessions evidence_session
      ON evidence_session.project_id = evidence.project_id
   WHERE evidence_session.project_session_id = NEW.project_session_id
     AND evidence.registry_state = 'active'
     AND (evidence.artifact_id = json_extract(
       NEW.plan_json, '$.contention.evidenceRef'
     ) OR evidence.sha256 = json_extract(
       NEW.plan_json, '$.contention.evidenceRef'
     ))
     AND (evidence.project_session_id IS NULL OR
       evidence.project_session_id = NEW.project_session_id)
     AND (evidence.run_id IS NULL OR evidence.run_id = NEW.coordination_run_id)
)
OR NOT EXISTS (
  SELECT 1
    FROM artifacts rationale
    JOIN project_sessions rationale_session
      ON rationale_session.project_id = rationale.project_id
   WHERE rationale_session.project_session_id = NEW.project_session_id
     AND rationale.artifact_id = NEW.rationale_evidence_id
     AND rationale.revision = NEW.rationale_evidence_revision
     AND rationale.registry_state = 'active'
     AND (rationale.project_session_id IS NULL OR
       rationale.project_session_id = NEW.project_session_id)
     AND (rationale.run_id IS NULL OR rationale.run_id = NEW.coordination_run_id)
)
)
BEGIN
  SELECT RAISE(ABORT, 'INVARIANT_topology_wave_plan_currency');
END;

CREATE TABLE topology_wave_current (
  project_session_id TEXT NOT NULL,
  coordination_run_id TEXT NOT NULL,
  task_id TEXT NOT NULL,
  wave_id TEXT NOT NULL,
  wave_revision INTEGER NOT NULL CHECK (wave_revision >= 1),
  plan_digest TEXT NOT NULL,
  revision INTEGER NOT NULL CHECK (revision >= 1),
  PRIMARY KEY (project_session_id, coordination_run_id, task_id),
  FOREIGN KEY (project_session_id, coordination_run_id, task_id,
      wave_id, wave_revision, plan_digest)
    REFERENCES topology_wave_plans(project_session_id, coordination_run_id,
      task_id, wave_id, wave_revision, plan_digest)
);

CREATE TRIGGER topology_wave_plans_contiguous_insert
BEFORE INSERT ON topology_wave_plans
WHEN
  (NEW.wave_revision = 1 AND EXISTS (
    SELECT 1 FROM topology_wave_plans prior
     WHERE prior.project_session_id = NEW.project_session_id
       AND prior.coordination_run_id = NEW.coordination_run_id
       AND prior.task_id = NEW.task_id
  ))
  OR (NEW.wave_revision > 1 AND NOT EXISTS (
    SELECT 1
      FROM topology_wave_current current
     WHERE current.project_session_id = NEW.project_session_id
       AND current.coordination_run_id = NEW.coordination_run_id
       AND current.task_id = NEW.task_id
       AND current.wave_revision = NEW.wave_revision - 1
       AND current.wave_id = NEW.predecessor_wave_id
       AND current.wave_revision = NEW.predecessor_wave_revision
       AND current.plan_digest = NEW.predecessor_plan_digest
  ))
BEGIN
  SELECT RAISE(ABORT, 'INVARIANT_topology_wave_plan_contiguous');
END;

CREATE TRIGGER topology_wave_current_insert
BEFORE INSERT ON topology_wave_current
WHEN NEW.revision <> 1 OR NEW.wave_revision <> 1
BEGIN
  SELECT RAISE(ABORT, 'INVARIANT_topology_wave_current_cas');
END;

CREATE TRIGGER topology_wave_current_update
BEFORE UPDATE ON topology_wave_current
WHEN NEW.project_session_id IS NOT OLD.project_session_id
  OR NEW.coordination_run_id IS NOT OLD.coordination_run_id
  OR NEW.task_id IS NOT OLD.task_id
  OR NEW.revision <> OLD.revision + 1
  OR NEW.wave_revision <> OLD.wave_revision + 1
  OR NOT EXISTS (
    SELECT 1 FROM topology_wave_plans next
     WHERE next.project_session_id = NEW.project_session_id
       AND next.coordination_run_id = NEW.coordination_run_id
       AND next.task_id = NEW.task_id
       AND next.wave_id = NEW.wave_id
       AND next.wave_revision = NEW.wave_revision
       AND next.plan_digest = NEW.plan_digest
       AND next.predecessor_wave_id = OLD.wave_id
       AND next.predecessor_wave_revision = OLD.wave_revision
       AND next.predecessor_plan_digest = OLD.plan_digest
  )
BEGIN
  SELECT RAISE(ABORT, 'INVARIANT_topology_wave_current_cas');
END;

CREATE TRIGGER topology_wave_current_delete
BEFORE DELETE ON topology_wave_current
BEGIN
  SELECT RAISE(ABORT, 'INVARIANT_topology_wave_current_cas');
END;

CREATE TABLE topology_wave_append_receipts (
  command_id TEXT PRIMARY KEY,
  request_digest TEXT NOT NULL,
  actor_principal_digest TEXT NOT NULL,
  project_session_id TEXT NOT NULL,
  coordination_run_id TEXT NOT NULL,
  task_id TEXT NOT NULL,
  prior_wave_id TEXT,
  prior_wave_revision INTEGER,
  prior_plan_digest TEXT,
  wave_id TEXT NOT NULL,
  wave_revision INTEGER NOT NULL CHECK (wave_revision >= 1),
  plan_digest TEXT NOT NULL,
  pointer_revision INTEGER NOT NULL CHECK (pointer_revision >= 1),
  receipt_json TEXT NOT NULL CHECK (json_valid(receipt_json)),
  receipt_digest TEXT NOT NULL UNIQUE,
  created_at INTEGER NOT NULL,
  FOREIGN KEY (project_session_id, coordination_run_id, task_id,
      wave_id, wave_revision, plan_digest)
    REFERENCES topology_wave_plans(project_session_id, coordination_run_id,
      task_id, wave_id, wave_revision, plan_digest),
  CHECK ((prior_wave_id IS NULL) = (prior_wave_revision IS NULL)),
  CHECK ((prior_wave_revision IS NULL) = (prior_plan_digest IS NULL))
);

CREATE TABLE provider_context_pressure_current (
  run_id TEXT NOT NULL,
  agent_id TEXT NOT NULL,
  adapter_id TEXT NOT NULL,
  provider_generation INTEGER NOT NULL CHECK (provider_generation >= 1),
  context_revision INTEGER NOT NULL CHECK (context_revision >= 0),
  observation_source_event_id TEXT NOT NULL,
  pressure TEXT NOT NULL CHECK (pressure IN ('low','medium','high','unknown')),
  source TEXT NOT NULL CHECK (source IN ('native-exact','native-estimated','hook-boundary','unavailable')),
  confidence TEXT NOT NULL CHECK (confidence IN ('exact','estimated','unknown')),
  window_tokens INTEGER CHECK (window_tokens IS NULL OR window_tokens >= 0),
  used_tokens INTEGER CHECK (used_tokens IS NULL OR used_tokens >= 0),
  remaining_tokens INTEGER CHECK (remaining_tokens IS NULL OR remaining_tokens >= 0),
  observed_at INTEGER NOT NULL,
  expires_at INTEGER NOT NULL CHECK (expires_at > observed_at),
  evidence_digest TEXT NOT NULL,
  revision INTEGER NOT NULL CHECK (revision >= 1),
  PRIMARY KEY (run_id, agent_id),
  FOREIGN KEY (run_id, agent_id, adapter_id)
    REFERENCES agent_adapter_bindings(run_id, agent_id, adapter_id),
  FOREIGN KEY (run_id, agent_id, observation_source_event_id,
      provider_generation, context_revision, evidence_digest)
    REFERENCES provider_context_observation_audit(run_id, agent_id, source_event_id,
      provider_generation, context_revision, evidence_digest),
  CHECK (source != 'unavailable' OR
    (pressure = 'unknown' AND confidence = 'unknown' AND window_tokens IS NULL AND
      used_tokens IS NULL AND remaining_tokens IS NULL)),
  CHECK (source != 'native-exact' OR
    (confidence = 'exact' AND window_tokens IS NOT NULL AND
      used_tokens IS NOT NULL AND remaining_tokens IS NOT NULL AND
      used_tokens + remaining_tokens = window_tokens)),
  CHECK (source != 'native-estimated' OR
    (confidence = 'estimated' AND window_tokens IS NOT NULL AND
      used_tokens IS NOT NULL AND remaining_tokens IS NOT NULL AND
      used_tokens + remaining_tokens = window_tokens)),
  CHECK (source != 'hook-boundary' OR
    (confidence IN ('exact','estimated') AND
      ((window_tokens IS NULL AND used_tokens IS NULL AND remaining_tokens IS NULL) OR
       (window_tokens IS NOT NULL AND used_tokens IS NOT NULL AND
        remaining_tokens IS NOT NULL AND used_tokens + remaining_tokens = window_tokens)))),
  CHECK (confidence != 'unknown' OR pressure = 'unknown')
);

CREATE TRIGGER provider_action_route_dispatches_point_of_use
BEFORE INSERT ON provider_action_route_dispatches
WHEN NOT EXISTS (
  SELECT 1
    FROM provider_action_routes route
    JOIN adapter_capability_snapshots admitted_snapshot
      ON admitted_snapshot.adapter_id = route.adapter_id
     AND admitted_snapshot.snapshot_generation = route.capability_snapshot_generation
     AND admitted_snapshot.snapshot_digest = route.capability_snapshot_digest
     AND admitted_snapshot.capability_body_digest = route.capability_body_digest
    JOIN adapter_capability_snapshots dispatch_snapshot
      ON dispatch_snapshot.adapter_id = NEW.adapter_id
     AND dispatch_snapshot.snapshot_generation = NEW.capability_snapshot_generation
     AND dispatch_snapshot.snapshot_digest = NEW.capability_snapshot_digest
     AND dispatch_snapshot.capability_body_digest = NEW.capability_body_digest
    JOIN adapter_capability_current current_snapshot
      ON current_snapshot.adapter_id = NEW.adapter_id
     AND current_snapshot.snapshot_generation = NEW.capability_snapshot_generation
     AND current_snapshot.snapshot_digest = NEW.capability_snapshot_digest
     AND current_snapshot.capability_body_digest = NEW.capability_body_digest
    JOIN adapter_effective_configurations configuration
      ON configuration.configuration_id = NEW.effective_configuration_id
     AND configuration.configuration_revision = NEW.effective_configuration_revision
     AND configuration.configuration_digest = NEW.effective_configuration_ref_digest
   WHERE route.adapter_id = NEW.adapter_id
     AND route.action_id = NEW.action_id
     AND route.admission_digest = NEW.admission_digest
     AND route.capability_body_digest = NEW.capability_body_digest
     AND route.effective_configuration_id = NEW.effective_configuration_id
     AND route.effective_configuration_revision = NEW.effective_configuration_revision
     AND route.effective_configuration_ref_digest = NEW.effective_configuration_ref_digest
     AND route.permission_profile_digest = NEW.permission_profile_digest
     AND route.discovery_surface_evidence_id = NEW.discovery_surface_evidence_id
     AND route.discovery_surface_evidence_revision = NEW.discovery_surface_evidence_revision
     AND admitted_snapshot.adapter_contract_digest = dispatch_snapshot.adapter_contract_digest
     AND admitted_snapshot.host_id = dispatch_snapshot.host_id
     AND dispatch_snapshot.expires_at > NEW.dispatched_at
     AND configuration.subject_kind = 'provider-action'
     AND configuration.subject_action_adapter_id = NEW.adapter_id
     AND configuration.subject_action_id = NEW.action_id
     AND configuration.permission_profile_digest = NEW.permission_profile_digest
     AND configuration.discovery_surface_evidence_id = NEW.discovery_surface_evidence_id
     AND configuration.discovery_surface_evidence_revision = NEW.discovery_surface_evidence_revision
     AND NEW.dispatch_ordinal = COALESCE((
       SELECT MAX(prior.dispatch_ordinal) + 1
         FROM provider_action_route_dispatches prior
        WHERE prior.adapter_id = NEW.adapter_id AND prior.action_id = NEW.action_id
     ), 1)
)
BEGIN
  SELECT RAISE(ABORT, 'INVARIANT_provider_action_route_dispatch_point_of_use');
END;

CREATE TRIGGER provider_action_route_observations_parent_equality
BEFORE INSERT ON provider_action_route_observations
WHEN NOT EXISTS (
  SELECT 1 FROM provider_action_routes route
   WHERE route.adapter_id = NEW.adapter_id
     AND route.action_id = NEW.action_id
     AND route.admission_digest = NEW.admission_digest
)
BEGIN
  SELECT RAISE(ABORT, 'INVARIANT_provider_action_route_observation_parent');
END;

CREATE TRIGGER adapter_capability_snapshots_immutable_update
BEFORE UPDATE ON adapter_capability_snapshots
BEGIN SELECT RAISE(ABORT, 'INVARIANT_current_history_immutable'); END;

CREATE TRIGGER adapter_capability_snapshots_immutable_delete
BEFORE DELETE ON adapter_capability_snapshots
BEGIN SELECT RAISE(ABORT, 'INVARIANT_current_history_immutable'); END;

CREATE TRIGGER adapter_effective_configurations_immutable_update
BEFORE UPDATE ON adapter_effective_configurations
BEGIN SELECT RAISE(ABORT, 'INVARIANT_current_history_immutable'); END;

CREATE TRIGGER adapter_effective_configurations_immutable_delete
BEFORE DELETE ON adapter_effective_configurations
BEGIN SELECT RAISE(ABORT, 'INVARIANT_current_history_immutable'); END;

CREATE TRIGGER artifact_publication_lineage_immutable_update
BEFORE UPDATE ON artifact_publication_lineage
BEGIN SELECT RAISE(ABORT, 'INVARIANT_current_history_immutable'); END;

CREATE TRIGGER artifact_publication_lineage_immutable_delete
BEFORE DELETE ON artifact_publication_lineage
BEGIN SELECT RAISE(ABORT, 'INVARIANT_current_history_immutable'); END;

CREATE TRIGGER coordination_gate_snapshots_immutable_update
BEFORE UPDATE ON coordination_gate_snapshots
BEGIN SELECT RAISE(ABORT, 'INVARIANT_current_history_immutable'); END;

CREATE TRIGGER coordination_gate_snapshots_immutable_delete
BEFORE DELETE ON coordination_gate_snapshots
BEGIN SELECT RAISE(ABORT, 'INVARIANT_current_history_immutable'); END;

CREATE TRIGGER discovery_surface_manifests_immutable_update
BEFORE UPDATE ON discovery_surface_manifests
BEGIN SELECT RAISE(ABORT, 'INVARIANT_current_history_immutable'); END;

CREATE TRIGGER discovery_surface_manifests_immutable_delete
BEFORE DELETE ON discovery_surface_manifests
BEGIN SELECT RAISE(ABORT, 'INVARIANT_current_history_immutable'); END;

CREATE TRIGGER implementation_delivery_manifests_immutable_update
BEFORE UPDATE ON implementation_delivery_manifests
BEGIN SELECT RAISE(ABORT, 'INVARIANT_current_history_immutable'); END;

CREATE TRIGGER implementation_delivery_manifests_immutable_delete
BEFORE DELETE ON implementation_delivery_manifests
BEGIN SELECT RAISE(ABORT, 'INVARIANT_current_history_immutable'); END;

CREATE TRIGGER provider_action_route_dispatches_immutable_update
BEFORE UPDATE ON provider_action_route_dispatches
BEGIN SELECT RAISE(ABORT, 'INVARIANT_current_history_immutable'); END;

CREATE TRIGGER provider_action_route_dispatches_immutable_delete
BEFORE DELETE ON provider_action_route_dispatches
BEGIN SELECT RAISE(ABORT, 'INVARIANT_current_history_immutable'); END;

CREATE TRIGGER provider_action_route_observations_immutable_update
BEFORE UPDATE ON provider_action_route_observations
BEGIN SELECT RAISE(ABORT, 'INVARIANT_current_history_immutable'); END;

CREATE TRIGGER provider_action_route_observations_immutable_delete
BEFORE DELETE ON provider_action_route_observations
BEGIN SELECT RAISE(ABORT, 'INVARIANT_current_history_immutable'); END;

CREATE TRIGGER provider_action_routes_immutable_update
BEFORE UPDATE ON provider_action_routes
BEGIN SELECT RAISE(ABORT, 'INVARIANT_current_history_immutable'); END;

CREATE TRIGGER provider_action_routes_immutable_delete
BEFORE DELETE ON provider_action_routes
BEGIN SELECT RAISE(ABORT, 'INVARIANT_current_history_immutable'); END;

CREATE TRIGGER provider_context_observation_audit_immutable_update
BEFORE UPDATE ON provider_context_observation_audit
BEGIN SELECT RAISE(ABORT, 'INVARIANT_current_history_immutable'); END;

CREATE TRIGGER provider_context_observation_audit_immutable_delete
BEFORE DELETE ON provider_context_observation_audit
BEGIN SELECT RAISE(ABORT, 'INVARIANT_current_history_immutable'); END;

CREATE TRIGGER provider_review_evidence_immutable_update
BEFORE UPDATE ON provider_review_evidence
BEGIN SELECT RAISE(ABORT, 'INVARIANT_current_history_immutable'); END;

CREATE TRIGGER provider_review_evidence_immutable_delete
BEFORE DELETE ON provider_review_evidence
BEGIN SELECT RAISE(ABORT, 'INVARIANT_current_history_immutable'); END;

CREATE TRIGGER provider_review_results_immutable_update
BEFORE UPDATE ON provider_review_results
BEGIN SELECT RAISE(ABORT, 'INVARIANT_current_history_immutable'); END;

CREATE TRIGGER provider_review_results_immutable_delete
BEFORE DELETE ON provider_review_results
BEGIN SELECT RAISE(ABORT, 'INVARIANT_current_history_immutable'); END;

CREATE TRIGGER provider_review_terminal_journal_immutable_update
BEFORE UPDATE ON provider_review_terminal_journal
BEGIN SELECT RAISE(ABORT, 'INVARIANT_current_history_immutable'); END;

CREATE TRIGGER provider_review_terminal_journal_immutable_delete
BEFORE DELETE ON provider_review_terminal_journal
BEGIN SELECT RAISE(ABORT, 'INVARIANT_current_history_immutable'); END;

CREATE TRIGGER review_bundles_immutable_update
BEFORE UPDATE ON review_bundles
BEGIN SELECT RAISE(ABORT, 'INVARIANT_current_history_immutable'); END;

CREATE TRIGGER review_bundles_immutable_delete
BEFORE DELETE ON review_bundles
BEGIN SELECT RAISE(ABORT, 'INVARIANT_current_history_immutable'); END;

CREATE TRIGGER review_certification_cuts_immutable_update
BEFORE UPDATE ON review_certification_cuts
BEGIN SELECT RAISE(ABORT, 'INVARIANT_current_history_immutable'); END;

CREATE TRIGGER review_certification_cuts_immutable_delete
BEFORE DELETE ON review_certification_cuts
BEGIN SELECT RAISE(ABORT, 'INVARIANT_current_history_immutable'); END;

CREATE TRIGGER review_evidence_annotations_immutable_update
BEFORE UPDATE ON review_evidence_annotations
BEGIN SELECT RAISE(ABORT, 'INVARIANT_current_history_immutable'); END;

CREATE TRIGGER review_evidence_annotations_immutable_delete
BEFORE DELETE ON review_evidence_annotations
BEGIN SELECT RAISE(ABORT, 'INVARIANT_current_history_immutable'); END;

CREATE TRIGGER review_target_chair_bindings_immutable_update
BEFORE UPDATE ON review_target_chair_bindings
BEGIN SELECT RAISE(ABORT, 'INVARIANT_current_history_immutable'); END;

CREATE TRIGGER review_target_chair_bindings_immutable_delete
BEFORE DELETE ON review_target_chair_bindings
BEGIN SELECT RAISE(ABORT, 'INVARIANT_current_history_immutable'); END;

CREATE TRIGGER topology_wave_append_receipts_immutable_update
BEFORE UPDATE ON topology_wave_append_receipts
BEGIN SELECT RAISE(ABORT, 'INVARIANT_current_history_immutable'); END;

CREATE TRIGGER topology_wave_append_receipts_immutable_delete
BEFORE DELETE ON topology_wave_append_receipts
BEGIN SELECT RAISE(ABORT, 'INVARIANT_current_history_immutable'); END;

CREATE TRIGGER topology_wave_plans_immutable_update
BEFORE UPDATE ON topology_wave_plans
BEGIN SELECT RAISE(ABORT, 'INVARIANT_current_history_immutable'); END;

CREATE TRIGGER topology_wave_plans_immutable_delete
BEFORE DELETE ON topology_wave_plans
BEGIN SELECT RAISE(ABORT, 'INVARIANT_current_history_immutable'); END;

INSERT INTO daemon_global_state(singleton, revision) VALUES (1, 1);
