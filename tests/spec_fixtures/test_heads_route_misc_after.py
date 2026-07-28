#!/usr/bin/env python3
"""Executable SQLite oracle for Lane A heads, routes, and MF repairs.

The main schema is an isolated transliteration of the keys, checks, update
guards, and owner predicates in the review, lifecycle-custody and
observability modules. The adapter-integrity test additionally executes the two exact
normative table definitions it covers. This remains deliberately narrower than
the complete generated baseline schema.

Run:
    python3 tests/spec_fixtures/test_heads_route_misc_after.py
"""

from __future__ import annotations

import sqlite3
import unittest
from collections.abc import Sequence
from pathlib import Path
from typing import Any

from spec_sources import AGENT_FABRIC_HARDENING, read_specs


CASES_RUN = 0
ROOT = Path(__file__).resolve().parents[2]
HARDENING_SPECS = read_specs(AGENT_FABRIC_HARDENING)


def normative_table_sql(table: str) -> str:
    start = HARDENING_SPECS.index(f"\n{table}(") + 1
    end = HARDENING_SPECS.index("\n)\n", start) + 2
    return f"CREATE TABLE {HARDENING_SPECS[start:end]};"


ACTIVATION_PARENT_CHECK_ERROR = """CHECK constraint failed: (subject_kind='activation' AND
      activation_configuration_id IS NULL AND
      activation_configuration_revision IS NULL AND
      activation_configuration_digest IS NULL AND
      activation_configuration_subject_kind IS NULL) OR
    (subject_kind IN ('provider-smoke','provider-action') AND
      activation_configuration_id IS NOT NULL AND
      activation_configuration_revision IS NOT NULL AND
      activation_configuration_digest IS NOT NULL AND
      activation_configuration_subject_kind='activation')"""


SCHEMA = r"""
PRAGMA foreign_keys=ON;

CREATE TABLE lifecycle_receipt_scope_checkpoints(
  project_session_id TEXT NOT NULL,
  run_id TEXT NOT NULL,
  authority_id TEXT NOT NULL,
  receipt_count INTEGER NOT NULL CHECK(receipt_count>=0),
  head_authority_sequence INTEGER NOT NULL CHECK(head_authority_sequence>=0),
  head_receipt_digest TEXT,
  ordered_record_set_digest TEXT NOT NULL,
  checkpoint_digest TEXT NOT NULL,
  PRIMARY KEY(project_session_id,run_id,receipt_count),
  UNIQUE(checkpoint_digest),
  UNIQUE(project_session_id,run_id,checkpoint_digest),
  CHECK(receipt_count=head_authority_sequence),
  CHECK((receipt_count=0)=(head_receipt_digest IS NULL))
) STRICT;

CREATE TABLE lifecycle_receipt_scope_heads(
  project_session_id TEXT NOT NULL,
  run_id TEXT NOT NULL,
  checkpoint_digest TEXT NOT NULL,
  revision INTEGER NOT NULL CHECK(revision>=1),
  PRIMARY KEY(project_session_id,run_id),
  UNIQUE(project_session_id,run_id,checkpoint_digest,revision),
  FOREIGN KEY(project_session_id,run_id,checkpoint_digest)
    REFERENCES lifecycle_receipt_scope_checkpoints(
      project_session_id,run_id,checkpoint_digest)
) STRICT;

CREATE TRIGGER lifecycle_scope_head_advance_guard
BEFORE UPDATE ON lifecycle_receipt_scope_heads
WHEN NEW.project_session_id<>OLD.project_session_id
  OR NEW.run_id<>OLD.run_id
  OR NEW.revision<>OLD.revision+1
BEGIN
  SELECT RAISE(ABORT,'lifecycle-scope-head-cas-failed');
END;

CREATE TABLE lifecycle_rotation_custody_revisions(
  project_session_id TEXT NOT NULL,
  run_id TEXT NOT NULL,
  agent_id TEXT NOT NULL,
  custody_id TEXT NOT NULL,
  revision INTEGER NOT NULL CHECK(revision>=1),
  state TEXT NOT NULL CHECK(state IN
    ('awaiting-boundary','prepared','dispatched','accepted','ambiguous',
     'provider-terminal','committing','finalized')),
  disposition_code TEXT NOT NULL CHECK(disposition_code IN
    ('none','adopted','no-effect','quarantined','superseded','abandoned')),
  semantic_digest TEXT NOT NULL,
  source_ref_digest TEXT NOT NULL,
  journal_digest TEXT NOT NULL,
  PRIMARY KEY(run_id,agent_id,custody_id,revision),
  UNIQUE(project_session_id,run_id,agent_id,custody_id,revision,state,
    disposition_code,semantic_digest,source_ref_digest,journal_digest),
  CHECK((state='finalized')=(disposition_code<>'none'))
) STRICT;

CREATE TABLE lifecycle_rotation_custody_heads(
  project_session_id TEXT NOT NULL,
  run_id TEXT NOT NULL,
  agent_id TEXT NOT NULL,
  custody_id TEXT NOT NULL,
  current_revision INTEGER NOT NULL CHECK(current_revision>=1),
  state TEXT NOT NULL,
  disposition_code TEXT NOT NULL,
  semantic_digest TEXT NOT NULL,
  source_ref_digest TEXT NOT NULL,
  journal_digest TEXT NOT NULL,
  terminal INTEGER NOT NULL CHECK(terminal IN (0,1)),
  head_revision INTEGER NOT NULL CHECK(head_revision>=1),
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

CREATE TABLE lifecycle_generation_loss_revisions(
  project_session_id TEXT NOT NULL,
  run_id TEXT NOT NULL,
  agent_id TEXT NOT NULL,
  generation_loss_id TEXT NOT NULL,
  revision INTEGER NOT NULL CHECK(revision>=1),
  state TEXT NOT NULL CHECK(state IN
    ('open','recovery-in-progress','recovered-adopted','abandoned')),
  abandon_kind_code TEXT NOT NULL CHECK(abandon_kind_code IN
    ('none','direct-open','recovery-attempt')),
  semantic_digest TEXT NOT NULL,
  source_ref_digest TEXT NOT NULL,
  journal_digest TEXT NOT NULL,
  PRIMARY KEY(run_id,agent_id,generation_loss_id,revision),
  UNIQUE(project_session_id,run_id,agent_id,generation_loss_id,revision,state,
    abandon_kind_code,semantic_digest,source_ref_digest,journal_digest),
  CHECK((state='abandoned')=(abandon_kind_code<>'none'))
) STRICT;

CREATE TABLE lifecycle_generation_loss_heads(
  project_session_id TEXT NOT NULL,
  run_id TEXT NOT NULL,
  agent_id TEXT NOT NULL,
  generation_loss_id TEXT NOT NULL,
  current_revision INTEGER NOT NULL CHECK(current_revision>=1),
  state TEXT NOT NULL,
  abandon_kind_code TEXT NOT NULL,
  semantic_digest TEXT NOT NULL,
  source_ref_digest TEXT NOT NULL,
  journal_digest TEXT NOT NULL,
  terminal INTEGER NOT NULL CHECK(terminal IN (0,1)),
  head_revision INTEGER NOT NULL CHECK(head_revision>=1),
  PRIMARY KEY(run_id,agent_id,generation_loss_id),
  UNIQUE(project_session_id,run_id,agent_id,generation_loss_id),
  FOREIGN KEY(project_session_id,run_id,agent_id,generation_loss_id,
      current_revision,state,abandon_kind_code,semantic_digest,
      source_ref_digest,journal_digest)
    REFERENCES lifecycle_generation_loss_revisions(
      project_session_id,run_id,agent_id,generation_loss_id,revision,state,
      abandon_kind_code,semantic_digest,source_ref_digest,journal_digest),
  CHECK((terminal=1)=(state IN ('recovered-adopted','abandoned'))),
  CHECK((state='abandoned')=(abandon_kind_code<>'none'))
) STRICT;

CREATE UNIQUE INDEX one_nonterminal_generation_loss_per_agent
  ON lifecycle_generation_loss_heads(run_id,agent_id)
  WHERE terminal=0;

CREATE TABLE provider_action_pair_preflights(
  adapter_id TEXT NOT NULL,
  action_id TEXT NOT NULL,
  run_id TEXT NOT NULL,
  action_kind TEXT NOT NULL CHECK(action_kind IN ('generic','certifying')),
  PRIMARY KEY(adapter_id,action_id)
) STRICT;

CREATE TABLE provider_actions(
  adapter_id TEXT NOT NULL,
  action_id TEXT NOT NULL,
  run_id TEXT NOT NULL,
  route_ordinal INTEGER NOT NULL CHECK(route_ordinal>=1),
  dispatch_count INTEGER NOT NULL DEFAULT 0 CHECK(dispatch_count>=0),
  PRIMARY KEY(adapter_id,action_id),
  UNIQUE(adapter_id,action_id,run_id),
  FOREIGN KEY(adapter_id,action_id)
    REFERENCES provider_action_pair_preflights(adapter_id,action_id)
) STRICT;

CREATE TABLE review_finding_capacity_reservations(
  adapter_id TEXT NOT NULL,
  action_id TEXT NOT NULL,
  run_id TEXT NOT NULL,
  target_generation INTEGER NOT NULL CHECK(target_generation>=1),
  slot TEXT NOT NULL CHECK(slot IN
    ('native','other-primary','cursor-grok','agy-gemini')),
  attempt_generation INTEGER CHECK(
    attempt_generation IS NULL OR attempt_generation>=1),
  reservation_digest TEXT NOT NULL,
  state TEXT NOT NULL CHECK(
    state IN ('preflight','attached','released','settled')),
  PRIMARY KEY(adapter_id,action_id),
  UNIQUE(adapter_id,action_id,run_id,target_generation,slot,
    attempt_generation,reservation_digest),
  FOREIGN KEY(adapter_id,action_id)
    REFERENCES provider_action_pair_preflights(adapter_id,action_id),
  CHECK(
    (state IN ('preflight','released') AND attempt_generation IS NULL) OR
    (state IN ('attached','settled') AND attempt_generation IS NOT NULL))
) STRICT;

CREATE TABLE adapter_capability_snapshots(
  adapter_id TEXT NOT NULL,
  snapshot_generation INTEGER NOT NULL CHECK(snapshot_generation>=1),
  snapshot_id TEXT NOT NULL,
  source TEXT NOT NULL CHECK(source IN
    ('runtime-discovery','version-pinned-conformance','unavailable')),
  capability_body_digest TEXT NOT NULL,
  snapshot_json TEXT NOT NULL,
  capability_kind TEXT GENERATED ALWAYS AS
    (json_extract(snapshot_json,'$.capabilities.kind')) STORED NOT NULL,
  snapshot_digest TEXT NOT NULL,
  PRIMARY KEY(adapter_id,snapshot_generation),
  UNIQUE(snapshot_id),
  UNIQUE(snapshot_digest),
  UNIQUE(adapter_id,snapshot_generation,snapshot_digest,
    capability_body_digest),
  CHECK(capability_kind IS NOT NULL AND
    capability_kind IN ('available','unavailable')),
  CHECK((source='unavailable' AND capability_kind='unavailable') OR
    (source IN ('runtime-discovery','version-pinned-conformance') AND
      capability_kind='available'))
) STRICT;

CREATE TABLE discovery_surface_manifests(
  evidence_id TEXT NOT NULL,
  evidence_revision INTEGER NOT NULL CHECK(evidence_revision>=1),
  manifest_digest TEXT NOT NULL,
  permission_profile_digest TEXT NOT NULL,
  PRIMARY KEY(evidence_id,evidence_revision),
  UNIQUE(evidence_id,evidence_revision,manifest_digest)
) STRICT;

CREATE TABLE adapter_activation_subjects(
  adapter_id TEXT NOT NULL,
  activation_id TEXT NOT NULL,
  activation_revision INTEGER NOT NULL,
  PRIMARY KEY(adapter_id,activation_id,activation_revision)
) STRICT;

CREATE TABLE adapter_provider_smoke_subjects(
  adapter_id TEXT NOT NULL,
  smoke_id TEXT NOT NULL,
  PRIMARY KEY(adapter_id,smoke_id)
) STRICT;

CREATE TABLE adapter_effective_configurations(
  configuration_id TEXT NOT NULL,
  configuration_revision INTEGER NOT NULL CHECK(configuration_revision>=1),
  adapter_id TEXT NOT NULL,
  adapter_contract_digest TEXT NOT NULL,
  executable_identity_digest TEXT NOT NULL,
  subject_kind TEXT NOT NULL CHECK(subject_kind IN
    ('activation','provider-smoke','provider-action')),
  subject_activation_id TEXT,
  subject_activation_revision INTEGER,
  subject_smoke_id TEXT,
  subject_action_adapter_id TEXT,
  subject_action_id TEXT,
  activation_configuration_id TEXT,
  activation_configuration_revision INTEGER,
  activation_configuration_digest TEXT,
  activation_configuration_subject_kind TEXT,
  capability_body_digest TEXT NOT NULL,
  permission_profile_digest TEXT NOT NULL,
  discovery_surface_evidence_id TEXT NOT NULL,
  discovery_surface_evidence_revision INTEGER NOT NULL,
  discovery_surface_digest TEXT NOT NULL,
  configuration_digest TEXT NOT NULL,
  PRIMARY KEY(configuration_id,configuration_revision),
  UNIQUE(configuration_id,configuration_revision,configuration_digest),
  UNIQUE(adapter_id,subject_kind,configuration_id,configuration_revision,
    configuration_digest,adapter_contract_digest,executable_identity_digest),
  UNIQUE(configuration_digest),
  UNIQUE(subject_action_adapter_id,subject_action_id,configuration_id,
    configuration_revision,configuration_digest,capability_body_digest,
    permission_profile_digest,discovery_surface_evidence_id,
    discovery_surface_evidence_revision,discovery_surface_digest),
  FOREIGN KEY(adapter_id,activation_configuration_subject_kind,
      activation_configuration_id,activation_configuration_revision,
      activation_configuration_digest,adapter_contract_digest,
      executable_identity_digest)
    REFERENCES adapter_effective_configurations(
      adapter_id,subject_kind,configuration_id,configuration_revision,
      configuration_digest,adapter_contract_digest,
      executable_identity_digest),
  FOREIGN KEY(adapter_id,subject_activation_id,subject_activation_revision)
    REFERENCES adapter_activation_subjects(
      adapter_id,activation_id,activation_revision),
  FOREIGN KEY(adapter_id,subject_smoke_id)
    REFERENCES adapter_provider_smoke_subjects(adapter_id,smoke_id),
  FOREIGN KEY(subject_action_adapter_id,subject_action_id)
    REFERENCES provider_action_pair_preflights(adapter_id,action_id),
  FOREIGN KEY(discovery_surface_evidence_id,
      discovery_surface_evidence_revision,discovery_surface_digest)
    REFERENCES discovery_surface_manifests(
      evidence_id,evidence_revision,manifest_digest),
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
) STRICT;

CREATE UNIQUE INDEX one_effective_configuration_per_activation_subject
  ON adapter_effective_configurations(
    adapter_id,subject_activation_id,subject_activation_revision)
  WHERE subject_kind='activation';
CREATE UNIQUE INDEX one_effective_configuration_per_smoke_subject
  ON adapter_effective_configurations(adapter_id,subject_smoke_id)
  WHERE subject_kind='provider-smoke';
CREATE UNIQUE INDEX one_effective_configuration_per_provider_action
  ON adapter_effective_configurations(
    subject_action_adapter_id,subject_action_id)
  WHERE subject_kind='provider-action';

CREATE TABLE provider_action_routes(
  adapter_id TEXT NOT NULL,
  action_id TEXT NOT NULL,
  run_id TEXT NOT NULL,
  route_kind TEXT NOT NULL CHECK(route_kind IN ('generic','certifying')),
  target_generation INTEGER,
  slot TEXT,
  attempt_generation INTEGER,
  reservation_digest TEXT,
  route_receipt_digest TEXT NOT NULL,
  deployed_route_admission_digest TEXT NOT NULL,
  capability_snapshot_generation INTEGER NOT NULL,
  capability_snapshot_digest TEXT NOT NULL,
  capability_body_digest TEXT NOT NULL,
  effective_configuration_id TEXT NOT NULL,
  effective_configuration_revision INTEGER NOT NULL,
  effective_configuration_ref_digest TEXT NOT NULL,
  permission_profile_digest TEXT NOT NULL,
  discovery_surface_evidence_id TEXT NOT NULL,
  discovery_surface_evidence_revision INTEGER NOT NULL,
  discovery_surface_digest TEXT NOT NULL,
  PRIMARY KEY(adapter_id,action_id),
  UNIQUE(adapter_id,action_id,deployed_route_admission_digest),
  UNIQUE(adapter_id,action_id,deployed_route_admission_digest,
    capability_body_digest,effective_configuration_id,
    effective_configuration_revision,effective_configuration_ref_digest,
    permission_profile_digest,discovery_surface_evidence_id,
    discovery_surface_evidence_revision,discovery_surface_digest),
  CHECK(
    (route_kind='generic' AND target_generation IS NULL AND slot IS NULL AND
      attempt_generation IS NULL AND reservation_digest IS NULL) OR
    (route_kind='certifying' AND target_generation IS NOT NULL AND
      slot IS NOT NULL AND attempt_generation IS NOT NULL AND
      reservation_digest IS NOT NULL)),
  FOREIGN KEY(adapter_id,action_id,run_id)
    REFERENCES provider_actions(adapter_id,action_id,run_id),
  FOREIGN KEY(adapter_id,action_id,run_id,target_generation,slot,
      attempt_generation,reservation_digest)
    REFERENCES review_finding_capacity_reservations(
      adapter_id,action_id,run_id,target_generation,slot,
      attempt_generation,reservation_digest),
  FOREIGN KEY(adapter_id,capability_snapshot_generation,
      capability_snapshot_digest,capability_body_digest)
    REFERENCES adapter_capability_snapshots(
      adapter_id,snapshot_generation,snapshot_digest,capability_body_digest),
  FOREIGN KEY(adapter_id,action_id,effective_configuration_id,
      effective_configuration_revision,effective_configuration_ref_digest,
      capability_body_digest,permission_profile_digest,
      discovery_surface_evidence_id,discovery_surface_evidence_revision,
      discovery_surface_digest)
    REFERENCES adapter_effective_configurations(
      subject_action_adapter_id,subject_action_id,configuration_id,
      configuration_revision,configuration_digest,capability_body_digest,
      permission_profile_digest,discovery_surface_evidence_id,
      discovery_surface_evidence_revision,discovery_surface_digest),
  FOREIGN KEY(discovery_surface_evidence_id,
      discovery_surface_evidence_revision,discovery_surface_digest)
    REFERENCES discovery_surface_manifests(
      evidence_id,evidence_revision,manifest_digest)
) STRICT;

CREATE TABLE provider_action_route_dispatches(
  adapter_id TEXT NOT NULL,
  action_id TEXT NOT NULL,
  dispatch_ordinal INTEGER NOT NULL CHECK(dispatch_ordinal>=1),
  admission_digest TEXT NOT NULL,
  capability_snapshot_generation INTEGER NOT NULL,
  capability_snapshot_digest TEXT NOT NULL,
  capability_body_digest TEXT NOT NULL,
  effective_configuration_id TEXT NOT NULL,
  effective_configuration_revision INTEGER NOT NULL,
  effective_configuration_ref_digest TEXT NOT NULL,
  permission_profile_digest TEXT NOT NULL,
  discovery_surface_evidence_id TEXT NOT NULL,
  discovery_surface_evidence_revision INTEGER NOT NULL,
  discovery_surface_digest TEXT NOT NULL,
  dispatch_digest TEXT NOT NULL,
  PRIMARY KEY(adapter_id,action_id,dispatch_ordinal),
  UNIQUE(dispatch_digest),
  FOREIGN KEY(adapter_id,action_id,admission_digest,capability_body_digest,
      effective_configuration_id,effective_configuration_revision,
      effective_configuration_ref_digest,permission_profile_digest,
      discovery_surface_evidence_id,discovery_surface_evidence_revision,
      discovery_surface_digest)
    REFERENCES provider_action_routes(
      adapter_id,action_id,deployed_route_admission_digest,
      capability_body_digest,effective_configuration_id,
      effective_configuration_revision,effective_configuration_ref_digest,
      permission_profile_digest,discovery_surface_evidence_id,
      discovery_surface_evidence_revision,discovery_surface_digest),
  FOREIGN KEY(adapter_id,capability_snapshot_generation,
      capability_snapshot_digest,capability_body_digest)
    REFERENCES adapter_capability_snapshots(
      adapter_id,snapshot_generation,snapshot_digest,capability_body_digest),
  FOREIGN KEY(effective_configuration_id,effective_configuration_revision,
      effective_configuration_ref_digest)
    REFERENCES adapter_effective_configurations(
      configuration_id,configuration_revision,configuration_digest),
  FOREIGN KEY(discovery_surface_evidence_id,
      discovery_surface_evidence_revision,discovery_surface_digest)
    REFERENCES discovery_surface_manifests(
      evidence_id,evidence_revision,manifest_digest)
) STRICT;

CREATE TABLE provider_action_route_observations(
  adapter_id TEXT NOT NULL,
  action_id TEXT NOT NULL,
  admission_digest TEXT NOT NULL,
  observation_json TEXT NOT NULL,
  observation_digest TEXT NOT NULL,
  PRIMARY KEY(adapter_id,action_id),
  UNIQUE(observation_digest),
  UNIQUE(adapter_id,action_id,admission_digest,observation_digest),
  FOREIGN KEY(adapter_id,action_id,admission_digest)
    REFERENCES provider_action_routes(
      adapter_id,action_id,deployed_route_admission_digest)
) STRICT;

CREATE TABLE provider_review_results(
  adapter_id TEXT NOT NULL,
  action_id TEXT NOT NULL,
  terminal_sequence INTEGER NOT NULL CHECK(terminal_sequence>=1),
  result_digest TEXT NOT NULL,
  PRIMARY KEY(adapter_id,action_id),
  UNIQUE(adapter_id,action_id,terminal_sequence,result_digest),
  FOREIGN KEY(adapter_id,action_id)
    REFERENCES provider_actions(adapter_id,action_id)
) STRICT;

CREATE TABLE review_finding_sets(
  finding_set_digest TEXT PRIMARY KEY
) STRICT;

CREATE TABLE provider_review_evidence(
  run_id TEXT NOT NULL,
  evidence_id TEXT NOT NULL,
  target_generation INTEGER NOT NULL CHECK(target_generation>=1),
  slot TEXT NOT NULL CHECK(slot IN
    ('native','other-primary','cursor-grok','agy-gemini')),
  action_adapter_id TEXT NOT NULL,
  action_id TEXT NOT NULL,
  terminal_sequence INTEGER NOT NULL CHECK(terminal_sequence>=1),
  terminal_result_digest TEXT NOT NULL,
  route_receipt_digest TEXT NOT NULL,
  route_admission_digest TEXT NOT NULL,
  route_observation_digest TEXT,
  actual_route_identity_digest TEXT,
  attempt_generation INTEGER NOT NULL CHECK(attempt_generation>=1),
  finding_capacity_reservation_digest TEXT NOT NULL,
  prior_head_generation INTEGER NOT NULL CHECK(prior_head_generation>=0),
  new_head_generation INTEGER NOT NULL CHECK(new_head_generation>=1),
  prior_evidence_id TEXT,
  evidence_digest TEXT NOT NULL,
  PRIMARY KEY(run_id,evidence_id),
  UNIQUE(action_adapter_id,action_id),
  UNIQUE(evidence_digest),
  UNIQUE(run_id,target_generation,slot,new_head_generation),
  UNIQUE(run_id,target_generation,slot,new_head_generation,evidence_id),
  CHECK(new_head_generation=prior_head_generation+1),
  CHECK((prior_head_generation=0)=(prior_evidence_id IS NULL)),
  CHECK(actual_route_identity_digest IS NULL OR
    route_observation_digest IS NOT NULL),
  FOREIGN KEY(action_adapter_id,action_id,terminal_sequence,
      terminal_result_digest)
    REFERENCES provider_review_results(
      adapter_id,action_id,terminal_sequence,result_digest),
  FOREIGN KEY(action_adapter_id,action_id,route_admission_digest,
      route_observation_digest)
    REFERENCES provider_action_route_observations(
      adapter_id,action_id,admission_digest,observation_digest),
  FOREIGN KEY(action_adapter_id,action_id,run_id,target_generation,slot,
      attempt_generation,finding_capacity_reservation_digest)
    REFERENCES review_finding_capacity_reservations(
      adapter_id,action_id,run_id,target_generation,slot,
      attempt_generation,reservation_digest),
  FOREIGN KEY(run_id,target_generation,slot,prior_head_generation,
      prior_evidence_id)
    REFERENCES provider_review_evidence(
      run_id,target_generation,slot,new_head_generation,evidence_id)
) STRICT;

CREATE TABLE review_slot_heads(
  run_id TEXT NOT NULL,
  target_generation INTEGER NOT NULL CHECK(target_generation>=1),
  slot TEXT NOT NULL CHECK(slot IN
    ('native','other-primary','cursor-grok','agy-gemini')),
  head_generation INTEGER NOT NULL CHECK(head_generation>=0),
  head_evidence_id TEXT,
  latest_attempt_generation INTEGER NOT NULL CHECK(latest_attempt_generation>=0),
  latest_action_adapter_id TEXT,
  latest_action_id TEXT,
  latest_action_state TEXT,
  open_finding_set_digest TEXT NOT NULL,
  repair_required_finding_set_digest TEXT NOT NULL,
  revision INTEGER NOT NULL CHECK(revision>=1),
  PRIMARY KEY(run_id,target_generation,slot),
  CHECK((head_generation=0 AND head_evidence_id IS NULL) OR
    (head_generation>=1 AND head_evidence_id IS NOT NULL)),
  CHECK((latest_attempt_generation=0 AND latest_action_adapter_id IS NULL AND
      latest_action_id IS NULL AND latest_action_state IS NULL) OR
    (latest_attempt_generation>=1 AND latest_action_adapter_id IS NOT NULL AND
      latest_action_id IS NOT NULL AND latest_action_state IS NOT NULL)),
  FOREIGN KEY(run_id,target_generation,slot,head_generation,head_evidence_id)
    REFERENCES provider_review_evidence(
      run_id,target_generation,slot,new_head_generation,evidence_id),
  FOREIGN KEY(latest_action_adapter_id,latest_action_id)
    REFERENCES provider_actions(adapter_id,action_id),
  FOREIGN KEY(open_finding_set_digest)
    REFERENCES review_finding_sets(finding_set_digest),
  FOREIGN KEY(repair_required_finding_set_digest)
    REFERENCES review_finding_sets(finding_set_digest)
) STRICT;

CREATE TABLE agent_adapter_bindings(
  run_id TEXT NOT NULL,
  agent_id TEXT NOT NULL,
  adapter_id TEXT NOT NULL,
  provider_generation INTEGER NOT NULL CHECK(provider_generation>=1),
  context_revision INTEGER NOT NULL CHECK(context_revision>=1),
  revision INTEGER NOT NULL CHECK(revision>=1),
  PRIMARY KEY(run_id,agent_id),
  UNIQUE(run_id,agent_id,adapter_id)
) STRICT;

CREATE TABLE provider_context_observation_audit(
  observation_id TEXT PRIMARY KEY,
  source_event_id TEXT NOT NULL,
  run_id TEXT NOT NULL,
  agent_id TEXT NOT NULL,
  provider_generation INTEGER NOT NULL,
  context_revision INTEGER NOT NULL,
  evidence_digest TEXT NOT NULL,
  UNIQUE(run_id,agent_id,source_event_id,provider_generation,
    context_revision,evidence_digest)
) STRICT;

CREATE TABLE provider_context_pressure_current(
  run_id TEXT NOT NULL,
  agent_id TEXT NOT NULL,
  adapter_id TEXT NOT NULL,
  provider_generation INTEGER NOT NULL,
  context_revision INTEGER NOT NULL,
  observation_source_event_id TEXT NOT NULL,
  pressure TEXT NOT NULL CHECK(pressure IN ('low','medium','high','unknown')),
  source TEXT NOT NULL CHECK(source IN
    ('native-exact','native-estimated','hook-boundary','unavailable')),
  confidence TEXT NOT NULL CHECK(confidence IN ('exact','estimated','unknown')),
  window_tokens INTEGER,
  used_tokens INTEGER,
  remaining_tokens INTEGER,
  observed_at TEXT NOT NULL,
  expires_at TEXT NOT NULL,
  evidence_digest TEXT NOT NULL,
  revision INTEGER NOT NULL CHECK(revision>=1),
  PRIMARY KEY(run_id,agent_id),
  FOREIGN KEY(run_id,agent_id,adapter_id)
    REFERENCES agent_adapter_bindings(run_id,agent_id,adapter_id),
  FOREIGN KEY(run_id,agent_id,observation_source_event_id,
      provider_generation,context_revision,evidence_digest)
    REFERENCES provider_context_observation_audit(
      run_id,agent_id,source_event_id,provider_generation,
      context_revision,evidence_digest),
  CHECK(expires_at>observed_at),
  CHECK(source<>'unavailable' OR
    (pressure='unknown' AND confidence='unknown' AND window_tokens IS NULL AND
      used_tokens IS NULL AND remaining_tokens IS NULL)),
  CHECK(source<>'native-exact' OR
    (confidence='exact' AND window_tokens IS NOT NULL AND
      used_tokens IS NOT NULL AND remaining_tokens IS NOT NULL AND
      used_tokens+remaining_tokens=window_tokens)),
  CHECK(confidence<>'unknown' OR pressure='unknown')
) STRICT;

CREATE TRIGGER observation_audit_immutable_update
BEFORE UPDATE ON provider_context_observation_audit
BEGIN
  SELECT RAISE(ABORT,'provider-context-observation-immutable');
END;

CREATE TRIGGER observation_audit_immutable_delete
BEFORE DELETE ON provider_context_observation_audit
BEGIN
  SELECT RAISE(ABORT,'provider-context-observation-immutable');
END;

CREATE TABLE provider_recovery_inventory(
  adapter_id TEXT NOT NULL,
  action_id TEXT NOT NULL,
  task_bound INTEGER NOT NULL CHECK(task_bound IN (0,1)),
  answer_bearing INTEGER NOT NULL CHECK(answer_bearing IN (0,1)),
  custody_kind TEXT NOT NULL CHECK(custody_kind IN
    ('ordinary','lifecycle','launch')),
  certifying INTEGER NOT NULL CHECK(certifying IN (0,1)),
  route_state TEXT NOT NULL CHECK(route_state IN
    ('present','missing','integrity-failed')),
  effect_state TEXT NOT NULL CHECK(effect_state IN ('resolved','unresolved')),
  dispatch_count INTEGER NOT NULL CHECK(dispatch_count>=0),
  PRIMARY KEY(adapter_id,action_id),
  UNIQUE(adapter_id,action_id,route_state),
  CHECK(certifying=0 OR custody_kind='ordinary')
) STRICT;

CREATE VIEW provider_recovery_owner_matches AS
SELECT adapter_id,action_id,
  (custody_kind='lifecycle') AS lifecycle_match,
  (custody_kind='launch') AS launch_match,
  (custody_kind='ordinary' AND certifying=1) AS certifying_match,
  (custody_kind='ordinary' AND certifying=0 AND task_bound=1 AND
    answer_bearing=1 AND
    (effect_state='unresolved' OR route_state IN
      ('missing','integrity-failed'))) AS generic_match
FROM provider_recovery_inventory;

CREATE VIEW provider_recovery_owners AS
SELECT i.adapter_id,i.action_id,
  CASE
    WHEN m.lifecycle_match THEN 'LifecycleRotationRecoveryService'
    WHEN m.launch_match THEN 'LaunchCustodyRecoveryService'
    WHEN m.certifying_match THEN 'ProviderRouteIntegrityRecoveryService'
    WHEN m.generic_match THEN 'GenericProviderRouteRecoveryService'
    ELSE 'none'
  END AS owner
FROM provider_recovery_inventory i
JOIN provider_recovery_owner_matches m USING(adapter_id,action_id);

CREATE TABLE generic_provider_route_recovery_evidence(
  adapter_id TEXT NOT NULL,
  action_id TEXT NOT NULL,
  route_state TEXT NOT NULL CHECK(route_state IN
    ('missing','integrity-failed')),
  recovery_evidence_digest TEXT NOT NULL,
  dispatch_count_at_recovery INTEGER NOT NULL,
  PRIMARY KEY(adapter_id,action_id),
  UNIQUE(recovery_evidence_digest),
  FOREIGN KEY(adapter_id,action_id,route_state)
    REFERENCES provider_recovery_inventory(adapter_id,action_id,route_state)
) STRICT;

CREATE TRIGGER generic_recovery_owner_guard
BEFORE INSERT ON generic_provider_route_recovery_evidence
WHEN NOT EXISTS (
  SELECT 1 FROM provider_recovery_owners o
  WHERE o.adapter_id=NEW.adapter_id AND o.action_id=NEW.action_id
    AND o.owner='GenericProviderRouteRecoveryService')
BEGIN
  SELECT RAISE(ABORT,'generic-provider-route-owner-crossed');
END;
"""


PRESSURE_COLUMNS = (
    "run_id",
    "agent_id",
    "adapter_id",
    "provider_generation",
    "context_revision",
    "observation_source_event_id",
    "pressure",
    "source",
    "confidence",
    "window_tokens",
    "used_tokens",
    "remaining_tokens",
    "observed_at",
    "expires_at",
    "evidence_digest",
    "revision",
)


def mark_case() -> None:
    global CASES_RUN
    CASES_RUN += 1


class LaneAHeadsRouteMiscOracle(unittest.TestCase):
    maxDiff = None

    def setUp(self) -> None:
        self.db = sqlite3.connect(":memory:", isolation_level=None)
        self.db.execute("PRAGMA foreign_keys=ON")
        self.db.executescript(SCHEMA)

    def tearDown(self) -> None:
        self.db.close()

    def accept(self, sql: str, parameters: Sequence[Any] = ()) -> None:
        self.db.execute(sql, parameters)
        mark_case()

    def reject(
        self,
        sql: str,
        parameters: Sequence[Any] = (),
        error: type[sqlite3.Error] = sqlite3.IntegrityError,
        *,
        message: str,
    ) -> None:
        with self.assertRaises(error) as caught:
            self.db.execute(sql, parameters)
        self.assertEqual(str(caught.exception), message)
        mark_case()

    def assert_foreign_keys_clean(self) -> None:
        self.assertEqual(self.db.execute("PRAGMA foreign_key_check").fetchall(), [])

    def seed_scope_checkpoint(
        self,
        session: str,
        run: str,
        count: int,
        checkpoint: str,
    ) -> None:
        head = None if count == 0 else f"receipt-{session}-{count}"
        self.db.execute(
            """INSERT INTO lifecycle_receipt_scope_checkpoints
               VALUES(?,?,?,?,?,?,?,?)""",
            (
                session,
                run,
                f"authority-{session}",
                count,
                count,
                head,
                f"ordered-{session}-{count}",
                checkpoint,
            ),
        )

    def seed_custody_revision(
        self,
        agent: str,
        custody: str,
        state: str,
        disposition: str,
    ) -> tuple[Any, ...]:
        row = (
            "ps-1",
            "run-1",
            agent,
            custody,
            1,
            state,
            disposition,
            f"semantic-{custody}",
            f"source-{custody}",
            f"journal-{custody}",
        )
        self.db.execute(
            "INSERT INTO lifecycle_rotation_custody_revisions VALUES(?,?,?,?,?,?,?,?,?,?)",
            row,
        )
        return row

    def seed_loss_revision(
        self,
        agent: str,
        loss: str,
        state: str,
        abandon: str,
    ) -> tuple[Any, ...]:
        row = (
            "ps-1",
            "run-1",
            agent,
            loss,
            1,
            state,
            abandon,
            f"semantic-{loss}",
            f"source-{loss}",
            f"journal-{loss}",
        )
        self.db.execute(
            "INSERT INTO lifecycle_generation_loss_revisions VALUES(?,?,?,?,?,?,?,?,?,?)",
            row,
        )
        return row

    def seed_adapter_primitives(self, adapter: str) -> None:
        self.db.execute(
            """INSERT OR IGNORE INTO adapter_capability_snapshots(
                 adapter_id,snapshot_generation,snapshot_id,source,
                 capability_body_digest,snapshot_json,snapshot_digest)
               VALUES(?,1,?,'runtime-discovery',?,
                 '{"capabilities":{"kind":"available"}}',?)""",
            (
                adapter,
                f"snapshot-id-{adapter}-1",
                f"body-{adapter}",
                f"snapshot-{adapter}-1",
            ),
        )
        self.db.execute(
            """INSERT OR IGNORE INTO discovery_surface_manifests
               VALUES(?,1,?,?)""",
            (
                f"surface-id-{adapter}",
                f"surface-{adapter}",
                f"permission-{adapter}",
            ),
        )
        self.db.execute(
            "INSERT OR IGNORE INTO adapter_activation_subjects VALUES(?,?,1)",
            (adapter, f"activation-{adapter}"),
        )
        self.db.execute(
            """INSERT OR IGNORE INTO adapter_effective_configurations(
                 configuration_id,configuration_revision,adapter_id,
                 adapter_contract_digest,executable_identity_digest,
                 subject_kind,subject_activation_id,subject_activation_revision,
                 capability_body_digest,permission_profile_digest,
                 discovery_surface_evidence_id,
                 discovery_surface_evidence_revision,
                 discovery_surface_digest,configuration_digest)
               VALUES(?,1,?,?,?,'activation',?,1,?,?,?,?,?,?)""",
            (
                f"activation-config-{adapter}",
                adapter,
                f"contract-{adapter}",
                f"executable-{adapter}",
                f"activation-{adapter}",
                f"body-{adapter}",
                f"permission-{adapter}",
                f"surface-id-{adapter}",
                1,
                f"surface-{adapter}",
                f"activation-config-digest-{adapter}",
            ),
        )

    def seed_preflight_and_configuration(
        self,
        adapter: str,
        action: str,
        *,
        run: str = "run-1",
        kind: str = "generic",
    ) -> None:
        self.seed_adapter_primitives(adapter)
        self.db.execute(
            "INSERT INTO provider_action_pair_preflights VALUES(?,?,?,?)",
            (adapter, action, run, kind),
        )
        self.db.execute(
            """INSERT INTO adapter_effective_configurations(
                 configuration_id,configuration_revision,adapter_id,
                 adapter_contract_digest,executable_identity_digest,
                 subject_kind,subject_action_adapter_id,subject_action_id,
                 activation_configuration_id,
                 activation_configuration_revision,
                 activation_configuration_digest,
                 activation_configuration_subject_kind,
                 capability_body_digest,permission_profile_digest,
                 discovery_surface_evidence_id,
                 discovery_surface_evidence_revision,
                 discovery_surface_digest,configuration_digest)
               VALUES(?,1,?,?,?,'provider-action',?,?,?,1,?,'activation',?,?,?,?,?,?)""",
            (
                f"config-{action}",
                adapter,
                f"contract-{adapter}",
                f"executable-{adapter}",
                adapter,
                action,
                f"activation-config-{adapter}",
                f"activation-config-digest-{adapter}",
                f"body-{adapter}",
                f"permission-{adapter}",
                f"surface-id-{adapter}",
                1,
                f"surface-{adapter}",
                f"config-digest-{action}",
            ),
        )

    def insert_reservation(
        self,
        adapter: str,
        action: str,
        *,
        run: str = "run-1",
        target: int = 1,
        slot: str = "native",
        attempt: int | None = 1,
        state: str = "attached",
    ) -> None:
        self.db.execute(
            """INSERT INTO review_finding_capacity_reservations
               VALUES(?,?,?,?,?,?,?,?)""",
            (
                adapter,
                action,
                run,
                target,
                slot,
                attempt,
                f"reservation-{action}",
                state,
            ),
        )

    def insert_action(
        self,
        adapter: str,
        action: str,
        *,
        run: str = "run-1",
        ordinal: int = 1,
    ) -> None:
        self.db.execute(
            "INSERT INTO provider_actions VALUES(?,?,?,?,0)",
            (adapter, action, run, ordinal),
        )

    def route_values(
        self,
        adapter: str,
        action: str,
        *,
        run: str = "run-1",
        kind: str = "generic",
        target: int = 1,
        slot: str = "native",
        attempt: int = 1,
    ) -> tuple[Any, ...]:
        cert = kind == "certifying"
        return (
            adapter,
            action,
            run,
            kind,
            target if cert else None,
            slot if cert else None,
            attempt if cert else None,
            f"reservation-{action}" if cert else None,
            f"route-receipt-{action}",
            f"admission-{action}",
            1,
            f"snapshot-{adapter}-1",
            f"body-{adapter}",
            f"config-{action}",
            1,
            f"config-digest-{action}",
            f"permission-{adapter}",
            f"surface-id-{adapter}",
            1,
            f"surface-{adapter}",
        )

    def insert_route(self, values: Sequence[Any]) -> None:
        self.db.execute(
            "INSERT INTO provider_action_routes VALUES(" + ",".join("?" * 20) + ")",
            values,
        )

    def seed_route(
        self,
        adapter: str,
        action: str,
        *,
        run: str = "run-1",
        kind: str = "generic",
        target: int = 1,
        slot: str = "native",
        attempt: int = 1,
        ordinal: int = 1,
    ) -> None:
        self.seed_preflight_and_configuration(adapter, action, run=run, kind=kind)
        if kind == "certifying":
            self.insert_reservation(
                adapter,
                action,
                run=run,
                target=target,
                slot=slot,
                attempt=attempt,
            )
        self.insert_action(adapter, action, run=run, ordinal=ordinal)
        self.insert_route(
            self.route_values(
                adapter,
                action,
                run=run,
                kind=kind,
                target=target,
                slot=slot,
                attempt=attempt,
            )
        )

    def dispatch_values(
        self,
        adapter: str,
        action: str,
        ordinal: int = 1,
    ) -> list[Any]:
        return [
            adapter,
            action,
            ordinal,
            f"admission-{action}",
            1,
            f"snapshot-{adapter}-1",
            f"body-{adapter}",
            f"config-{action}",
            1,
            f"config-digest-{action}",
            f"permission-{adapter}",
            f"surface-id-{adapter}",
            1,
            f"surface-{adapter}",
            f"dispatch-{action}-{ordinal}",
        ]

    def insert_dispatch(self, values: Sequence[Any]) -> None:
        self.db.execute(
            "INSERT INTO provider_action_route_dispatches VALUES("
            + ",".join("?" * 15)
            + ")",
            values,
        )

    def seed_review_terminal(
        self,
        adapter: str,
        action: str,
        *,
        observation: bool = True,
    ) -> None:
        self.db.execute(
            "INSERT INTO provider_review_results VALUES(?,?,1,?)",
            (adapter, action, f"result-{action}"),
        )
        if observation:
            self.db.execute(
                "INSERT INTO provider_action_route_observations VALUES(?,?,?,?,?)",
                (
                    adapter,
                    action,
                    f"admission-{action}",
                    '{"proved":true}',
                    f"observation-{action}",
                ),
            )
    def evidence_values(
        self,
        adapter: str,
        action: str,
        *,
        evidence: str = "evidence-1",
        target: int = 1,
        slot: str = "native",
        observation: str | None = None,
        actual: str | None = None,
        prior_generation: int = 0,
        new_generation: int = 1,
        prior_evidence: str | None = None,
    ) -> list[Any]:
        return [
            "run-1",
            evidence,
            target,
            slot,
            adapter,
            action,
            1,
            f"result-{action}",
            f"route-receipt-{action}",
            f"admission-{action}",
            observation,
            actual,
            1,
            f"reservation-{action}",
            prior_generation,
            new_generation,
            prior_evidence,
            f"digest-{evidence}",
        ]

    def insert_evidence(self, values: Sequence[Any]) -> None:
        self.db.execute(
            "INSERT INTO provider_review_evidence VALUES("
            + ",".join("?" * 18)
            + ")",
            values,
        )

    def seed_pressure(self) -> None:
        self.db.execute(
            "INSERT INTO agent_adapter_bindings VALUES('run-1','agent-1','adapter-old',1,1,1)"
        )
        self.db.execute(
            """INSERT INTO provider_context_observation_audit
               VALUES('obs-old','event-old','run-1','agent-1',1,1,'pressure-old')"""
        )
        self.db.execute(
            """INSERT INTO provider_context_observation_audit
               VALUES('obs-new','event-new','run-1','agent-1',2,1,'pressure-new')"""
        )
        self.db.execute(
            """INSERT INTO provider_context_pressure_current
               VALUES('run-1','agent-1','adapter-old',1,1,'event-old','high',
                 'native-exact','exact',100,80,20,
                 '2026-07-14T00:00:00Z','2026-07-14T01:00:00Z',
                 'pressure-old',1)"""
        )

    def capture_pressure(self) -> tuple[Any, ...] | None:
        columns = ",".join(PRESSURE_COLUMNS)
        row = self.db.execute(
            f"SELECT {columns} FROM provider_context_pressure_current "
            "WHERE run_id='run-1' AND agent_id='agent-1'"
        ).fetchone()
        return None if row is None else tuple(row)

    def adopt_binding(
        self,
        *,
        fault: str | None = None,
        cross_before_clear: bool = False,
    ) -> None:
        self.db.execute("BEGIN IMMEDIATE")
        try:
            expected = self.capture_pressure()
            if fault == "before-clear":
                raise RuntimeError(fault)
            if expected is None:
                present = self.db.execute(
                    """SELECT 1 FROM provider_context_pressure_current
                       WHERE run_id='run-1' AND agent_id='agent-1'"""
                ).fetchone()
                if present is not None:
                    raise sqlite3.IntegrityError("pressure-present-after-absent-capture")
            else:
                if cross_before_clear:
                    self.db.execute(
                        """UPDATE provider_context_pressure_current
                           SET revision=revision+1
                           WHERE run_id='run-1' AND agent_id='agent-1'"""
                    )
                predicate = " AND ".join(f"{column} IS ?" for column in PRESSURE_COLUMNS)
                cursor = self.db.execute(
                    f"DELETE FROM provider_context_pressure_current WHERE {predicate}",
                    expected,
                )
                if cursor.rowcount != 1:
                    raise sqlite3.IntegrityError("pressure-compare-delete-crossed")
            if fault == "after-clear":
                raise RuntimeError(fault)
            self.db.execute(
                """UPDATE agent_adapter_bindings
                   SET adapter_id='adapter-new',provider_generation=2,
                       context_revision=1,revision=revision+1
                   WHERE run_id='run-1' AND agent_id='agent-1'"""
            )
            if fault == "after-binding-update":
                raise RuntimeError(fault)
            self.db.execute("COMMIT")
        except BaseException:
            self.db.execute("ROLLBACK")
            raise

    def test_scope_heads_zero_positive_advance_and_crossings(self) -> None:
        self.seed_scope_checkpoint("ps-1", "run-1", 0, "scope-zero")
        self.seed_scope_checkpoint("ps-1", "run-1", 1, "scope-positive")
        self.seed_scope_checkpoint("ps-2", "run-2", 0, "scope-other")

        self.accept(
            "INSERT INTO lifecycle_receipt_scope_heads VALUES('ps-1','run-1','scope-zero',1)"
        )
        joined = self.db.execute(
            """SELECT c.receipt_count,c.head_receipt_digest
               FROM lifecycle_receipt_scope_heads h
               JOIN lifecycle_receipt_scope_checkpoints c
                 USING(project_session_id,run_id,checkpoint_digest)"""
        ).fetchone()
        self.assertEqual(joined, (0, None))
        mark_case()

        self.accept(
            """UPDATE lifecycle_receipt_scope_heads
               SET checkpoint_digest='scope-positive',revision=2
               WHERE project_session_id='ps-1' AND run_id='run-1'"""
        )
        self.assertEqual(
            self.db.execute(
                """SELECT c.receipt_count,c.head_receipt_digest,h.revision
                   FROM lifecycle_receipt_scope_heads h
                   JOIN lifecycle_receipt_scope_checkpoints c
                     USING(project_session_id,run_id,checkpoint_digest)"""
            ).fetchone(),
            (1, "receipt-ps-1-1", 2),
        )
        mark_case()

        self.reject(
            "INSERT INTO lifecycle_receipt_scope_heads VALUES('ps-1','run-x','scope-other',1)",
            message="FOREIGN KEY constraint failed",
        )
        self.reject(
            """UPDATE lifecycle_receipt_scope_heads
               SET checkpoint_digest='scope-zero',revision=4
               WHERE project_session_id='ps-1' AND run_id='run-1'""",
            message="lifecycle-scope-head-cas-failed",
        )
        columns = {
            row[1]
            for row in self.db.execute(
                "PRAGMA table_info(lifecycle_receipt_scope_heads)"
            )
        }
        self.assertNotIn("receipt_count", columns)
        self.assertNotIn("head_receipt_digest", columns)
        mark_case()
        self.assert_foreign_keys_clean()

    def test_generation_loss_head_exact_state_matrix_and_crossings(self) -> None:
        arms = (
            ("agent-open", "loss-open", "open", "none", 0),
            (
                "agent-recovery",
                "loss-recovery",
                "recovery-in-progress",
                "none",
                0,
            ),
            (
                "agent-adopted",
                "loss-adopted",
                "recovered-adopted",
                "none",
                1,
            ),
            (
                "agent-abandoned-direct",
                "loss-abandoned-direct",
                "abandoned",
                "direct-open",
                1,
            ),
            (
                "agent-abandoned-recovery",
                "loss-abandoned-recovery",
                "abandoned",
                "recovery-attempt",
                1,
            ),
        )
        for agent, loss, state, abandon, terminal in arms:
            with self.subTest(state=state, abandon=abandon):
                row = self.seed_loss_revision(agent, loss, state, abandon)
                self.accept(
                    "INSERT INTO lifecycle_generation_loss_heads VALUES(?,?,?,?,?,?,?,?,?,?,?,?)",
                    (*row[:7], *row[7:], terminal, 1),
                )

        self.seed_loss_revision("agent-cross", "loss-cross-a", "open", "none")
        self.seed_loss_revision("agent-cross", "loss-cross-b", "abandoned", "direct-open")
        self.reject(
            """INSERT INTO lifecycle_generation_loss_heads
               VALUES('ps-1','run-1','agent-cross','loss-cross-a',1,
                 'open','none','semantic-loss-cross-b','source-loss-cross-a',
                 'journal-loss-cross-a',0,1)""",
            message="FOREIGN KEY constraint failed",
        )
        self.seed_loss_revision("agent-terminal-cross", "loss-x", "open", "none")
        self.reject(
            """INSERT INTO lifecycle_generation_loss_heads
               VALUES('ps-1','run-1','agent-terminal-cross','loss-x',1,
                 'open','none','semantic-loss-x','source-loss-x',
                 'journal-loss-x',1,1)""",
            message=(
                "CHECK constraint failed: "
                "(terminal=1)=(state IN ('recovered-adopted','abandoned'))"
            ),
        )
        self.reject(
            """INSERT INTO lifecycle_generation_loss_revisions
               VALUES('ps-1','run-1','agent-bad','loss-bad',1,'open',
                 'direct-open','s-bad','r-bad','j-bad')""",
            message=(
                "CHECK constraint failed: "
                "(state='abandoned')=(abandon_kind_code<>'none')"
            ),
        )
        self.assert_foreign_keys_clean()

    def test_custody_head_nonnull_parity_and_crossings(self) -> None:
        open_row = self.seed_custody_revision(
            "agent-open", "custody-open", "prepared", "none"
        )
        final_row = self.seed_custody_revision(
            "agent-final", "custody-final", "finalized", "adopted"
        )
        self.accept(
            "INSERT INTO lifecycle_rotation_custody_heads VALUES(?,?,?,?,?,?,?,?,?,?,?,?)",
            (*open_row[:7], *open_row[7:], 0, 1),
        )
        self.accept(
            "INSERT INTO lifecycle_rotation_custody_heads VALUES(?,?,?,?,?,?,?,?,?,?,?,?)",
            (*final_row[:7], *final_row[7:], 1, 1),
        )
        self.reject(
            """INSERT INTO lifecycle_rotation_custody_heads
               VALUES('ps-1','run-1','agent-null','custody-null',1,NULL,
                 'none','s','r','j',0,1)""",
            message=(
                "NOT NULL constraint failed: "
                "lifecycle_rotation_custody_heads.state"
            ),
        )
        self.reject(
            """INSERT INTO lifecycle_rotation_custody_heads
               VALUES('ps-1','run-1','agent-null','custody-null',1,
                 'prepared',NULL,'s','r','j',0,1)""",
            message=(
                "NOT NULL constraint failed: "
                "lifecycle_rotation_custody_heads.disposition_code"
            ),
        )
        self.seed_custody_revision("agent-cross", "custody-a", "prepared", "none")
        self.seed_custody_revision("agent-cross", "custody-b", "finalized", "adopted")
        self.reject(
            """INSERT INTO lifecycle_rotation_custody_heads
               VALUES('ps-1','run-1','agent-cross','custody-a',1,'prepared',
                 'none','semantic-custody-b','source-custody-a',
                 'journal-custody-a',0,1)""",
            message="FOREIGN KEY constraint failed",
        )
        self.seed_custody_revision(
            "agent-parity", "custody-parity", "prepared", "none"
        )
        self.reject(
            """INSERT INTO lifecycle_rotation_custody_heads
               VALUES('ps-1','run-1','agent-parity','custody-parity',1,
                 'prepared','none','semantic-custody-parity',
                 'source-custody-parity','journal-custody-parity',1,1)""",
            message="CHECK constraint failed: (terminal=1)=(state='finalized')",
        )
        self.assert_foreign_keys_clean()

    def test_capability_source_and_generated_kind_closed_matrix(self) -> None:
        legal = (
            ("runtime-discovery", "available"),
            ("version-pinned-conformance", "available"),
            ("unavailable", "unavailable"),
        )
        generation = 1
        for source, kind in legal:
            with self.subTest(source=source, kind=kind):
                self.accept(
                    """INSERT INTO adapter_capability_snapshots(
                         adapter_id,snapshot_generation,snapshot_id,source,
                         capability_body_digest,snapshot_json,snapshot_digest)
                       VALUES('cap-adapter',?,?,?,?,?,?)""",
                    (
                        generation,
                        f"cap-id-{generation}",
                        source,
                        f"cap-body-{generation}",
                        f'{{"capabilities":{{"kind":"{kind}"}}}}',
                        f"cap-digest-{generation}",
                    ),
                )
                generation += 1

        invalid = (
            (
                "runtime-discovery",
                "unavailable",
                """CHECK constraint failed: (source='unavailable' AND capability_kind='unavailable') OR
    (source IN ('runtime-discovery','version-pinned-conformance') AND
      capability_kind='available')""",
            ),
            (
                "version-pinned-conformance",
                "unavailable",
                """CHECK constraint failed: (source='unavailable' AND capability_kind='unavailable') OR
    (source IN ('runtime-discovery','version-pinned-conformance') AND
      capability_kind='available')""",
            ),
            (
                "unavailable",
                "available",
                """CHECK constraint failed: (source='unavailable' AND capability_kind='unavailable') OR
    (source IN ('runtime-discovery','version-pinned-conformance') AND
      capability_kind='available')""",
            ),
            (
                "future-source",
                "available",
                """CHECK constraint failed: source IN
    ('runtime-discovery','version-pinned-conformance','unavailable')""",
            ),
            (
                None,
                "available",
                "NOT NULL constraint failed: adapter_capability_snapshots.source",
            ),
            (
                "runtime-discovery",
                "future-kind",
                """CHECK constraint failed: capability_kind IS NOT NULL AND
    capability_kind IN ('available','unavailable')""",
            ),
        )
        for source, kind, message in invalid:
            with self.subTest(source=source, kind=kind):
                self.reject(
                    """INSERT INTO adapter_capability_snapshots(
                         adapter_id,snapshot_generation,snapshot_id,source,
                         capability_body_digest,snapshot_json,snapshot_digest)
                       VALUES('invalid-adapter',?,?,?,?,?,?)""",
                    (
                        generation,
                        f"invalid-id-{generation}",
                        source,
                        f"invalid-body-{generation}",
                        f'{{"capabilities":{{"kind":"{kind}"}}}}',
                        f"invalid-digest-{generation}",
                    ),
                    message=message,
                )
                generation += 1
        self.reject(
            """INSERT INTO adapter_capability_snapshots(
                 adapter_id,snapshot_generation,snapshot_id,source,
                 capability_body_digest,snapshot_json,snapshot_digest)
               VALUES('null-kind',1,'null-kind-id','runtime-discovery',
                 'null-kind-body','{"capabilities":{}}','null-kind-digest')""",
            message=(
                "NOT NULL constraint failed: "
                "adapter_capability_snapshots.capability_kind"
            ),
        )
        self.reject(
            """INSERT INTO adapter_capability_snapshots(
                 adapter_id,snapshot_generation,snapshot_id,source,
                 capability_body_digest,snapshot_json,capability_kind,
                 snapshot_digest)
               VALUES('forged-kind',1,'forged-kind-id','runtime-discovery',
                 'forged-kind-body','{"capabilities":{"kind":"available"}}',
                 'available','forged-kind-digest')""",
            error=sqlite3.OperationalError,
            message='cannot INSERT into generated column "capability_kind"',
        )
        self.assert_foreign_keys_clean()

    def test_normative_adapter_integrity_ddl_executes_exactly(self) -> None:
        db = sqlite3.connect(":memory:", isolation_level=None)
        self.addCleanup(db.close)
        db.execute("PRAGMA foreign_keys=ON")
        db.executescript(
            """
            CREATE TABLE artifacts(
              artifact_id TEXT NOT NULL,
              revision INTEGER NOT NULL,
              PRIMARY KEY(artifact_id,revision)
            );
            CREATE TABLE discovery_surface_manifests(
              evidence_id TEXT NOT NULL,
              evidence_revision INTEGER NOT NULL,
              manifest_digest TEXT,
              PRIMARY KEY(evidence_id,evidence_revision),
              UNIQUE(evidence_id,evidence_revision,manifest_digest)
            );
            CREATE TABLE adapter_activation_subjects(
              adapter_id TEXT NOT NULL,
              activation_id TEXT NOT NULL,
              activation_revision INTEGER NOT NULL,
              PRIMARY KEY(adapter_id,activation_id,activation_revision)
            );
            CREATE TABLE adapter_provider_smoke_subjects(
              adapter_id TEXT NOT NULL,
              smoke_id TEXT NOT NULL,
              PRIMARY KEY(adapter_id,smoke_id)
            );
            CREATE TABLE provider_action_pair_preflights(
              adapter_id TEXT NOT NULL,
              action_id TEXT NOT NULL,
              PRIMARY KEY(adapter_id,action_id)
            );
            """
        )
        db.execute(normative_table_sql("adapter_capability_snapshots"))
        db.execute(normative_table_sql("adapter_effective_configurations"))

        db.execute(
            """INSERT INTO adapter_capability_snapshots(
                 adapter_id,snapshot_generation,snapshot_id,
                 adapter_contract_digest,host_id,host_version,source,
                 snapshot_json,snapshot_digest)
               VALUES('adapter-n',1,'snapshot-n','contract-n','host-n','1',
                 'runtime-discovery',
                 '{"capabilities":{"kind":"available"}}','snapshot-d-n')"""
        )
        with self.assertRaises(sqlite3.IntegrityError) as caught:
            db.execute(
                """INSERT INTO adapter_capability_snapshots(
                     adapter_id,snapshot_generation,snapshot_id,
                     adapter_contract_digest,host_id,host_version,source,
                     snapshot_json,snapshot_digest)
                   VALUES('adapter-n',2,'snapshot-n-2','contract-n','host-n','1',
                     'unavailable',
                     '{"capabilities":{"kind":"available"}}','snapshot-d-n-2')"""
            )
        self.assertEqual(
            str(caught.exception),
            """CHECK constraint failed: (source='unavailable' AND capability_kind='unavailable') OR
    (source IN ('runtime-discovery','version-pinned-conformance') AND
      capability_kind='available')""",
        )
        mark_case()

        db.execute(
            "INSERT INTO adapter_activation_subjects VALUES('adapter-n','activation-n',1)"
        )
        db.execute(
            "INSERT INTO adapter_provider_smoke_subjects VALUES('adapter-n','smoke-n')"
        )
        db.execute(
            """INSERT INTO adapter_effective_configurations(
                 configuration_id,configuration_revision,adapter_id,
                 adapter_contract_digest,executable_identity_digest,
                 subject_kind,subject_ref_digest,subject_activation_id,
                 subject_activation_revision,configuration_digest)
               VALUES('activation-config-n',1,'adapter-n','contract-n',
                 'executable-n','activation','activation-ref-n',
                 'activation-n',1,'activation-config-digest-n')"""
        )
        db.execute(
            """INSERT INTO adapter_effective_configurations(
                 configuration_id,configuration_revision,adapter_id,
                 adapter_contract_digest,executable_identity_digest,
                 subject_kind,subject_ref_digest,subject_smoke_id,
                 activation_configuration_id,
                 activation_configuration_revision,
                 activation_configuration_digest,
                 activation_configuration_subject_kind,configuration_digest)
               VALUES('smoke-config-n',1,'adapter-n','contract-n',
                 'executable-n','provider-smoke','smoke-ref-n','smoke-n',
                 'activation-config-n',1,'activation-config-digest-n',
                 'activation','smoke-config-digest-n')"""
        )
        mark_case()

        for crossing, contract, executable in (
            ("contract", "contract-crossed", "executable-n"),
            ("executable", "contract-n", "executable-crossed"),
        ):
            with self.subTest(crossing=crossing):
                with self.assertRaises(sqlite3.IntegrityError) as caught:
                    db.execute(
                        """INSERT INTO adapter_effective_configurations(
                             configuration_id,configuration_revision,adapter_id,
                             adapter_contract_digest,executable_identity_digest,
                             subject_kind,subject_ref_digest,subject_smoke_id,
                             activation_configuration_id,
                             activation_configuration_revision,
                             activation_configuration_digest,
                             activation_configuration_subject_kind,
                             configuration_digest)
                           VALUES(?,1,'adapter-n',?,?,'provider-smoke',?,
                             'smoke-n','activation-config-n',1,
                             'activation-config-digest-n','activation',?)""",
                        (
                            f"smoke-cross-{crossing}",
                            contract,
                            executable,
                            f"smoke-ref-{crossing}",
                            f"smoke-digest-{crossing}",
                        ),
                    )
                self.assertEqual(
                    str(caught.exception),
                    "FOREIGN KEY constraint failed",
                )
                mark_case()
        self.assertEqual(db.execute("PRAGMA foreign_key_check").fetchall(), [])

    def test_effective_configuration_activation_parent_correlation(self) -> None:
        self.seed_adapter_primitives("adapter-a")
        self.seed_adapter_primitives("adapter-b")
        self.db.execute(
            "INSERT INTO adapter_provider_smoke_subjects VALUES('adapter-a','smoke-a')"
        )
        self.db.execute(
            "INSERT INTO adapter_provider_smoke_subjects VALUES('adapter-b','smoke-b')"
        )
        self.db.execute(
            """INSERT INTO provider_action_pair_preflights
               VALUES('adapter-a','action-a','run-1','generic')"""
        )
        self.assertEqual(
            self.db.execute(
                """SELECT subject_kind FROM adapter_effective_configurations
                   WHERE configuration_id='activation-config-adapter-a'"""
            ).fetchone(),
            ("activation",),
        )
        mark_case()

        smoke_sql = """INSERT INTO adapter_effective_configurations(
          configuration_id,configuration_revision,adapter_id,
          adapter_contract_digest,executable_identity_digest,subject_kind,
          subject_smoke_id,activation_configuration_id,
          activation_configuration_revision,activation_configuration_digest,
          activation_configuration_subject_kind,capability_body_digest,
          permission_profile_digest,discovery_surface_evidence_id,
          discovery_surface_evidence_revision,discovery_surface_digest,
          configuration_digest)
          VALUES(?,1,?,?,?,'provider-smoke',?,?,1,?,'activation',?,?,?,?,?,?)"""
        self.accept(
            smoke_sql,
            (
                "smoke-config-a",
                "adapter-a",
                "contract-adapter-a",
                "executable-adapter-a",
                "smoke-a",
                "activation-config-adapter-a",
                "activation-config-digest-adapter-a",
                "body-adapter-a",
                "permission-adapter-a",
                "surface-id-adapter-a",
                1,
                "surface-adapter-a",
                "smoke-config-digest-a",
            ),
        )

        self.reject(
            """INSERT INTO adapter_effective_configurations(
                 configuration_id,configuration_revision,adapter_id,
                 adapter_contract_digest,executable_identity_digest,
                 subject_kind,subject_smoke_id,capability_body_digest,
                 permission_profile_digest,discovery_surface_evidence_id,
                 discovery_surface_evidence_revision,
                 discovery_surface_digest,configuration_digest)
               VALUES('null-parent',1,'adapter-b','contract-adapter-b',
                 'executable-adapter-b','provider-smoke','smoke-b',
                 'body-adapter-b','permission-adapter-b',
                 'surface-id-adapter-b',1,'surface-adapter-b','null-parent-d')""",
            message=ACTIVATION_PARENT_CHECK_ERROR,
        )
        self.reject(
            """INSERT INTO adapter_effective_configurations(
                 configuration_id,configuration_revision,adapter_id,
                 adapter_contract_digest,executable_identity_digest,
                 subject_kind,subject_smoke_id,activation_configuration_id,
                 activation_configuration_subject_kind,capability_body_digest,
                 permission_profile_digest,discovery_surface_evidence_id,
                 discovery_surface_evidence_revision,
                 discovery_surface_digest,configuration_digest)
               VALUES('half-parent',1,'adapter-b','contract-adapter-b',
                 'executable-adapter-b','provider-smoke','smoke-b',
                 'activation-config-adapter-b','activation',
                 'body-adapter-b','permission-adapter-b',
                 'surface-id-adapter-b',1,'surface-adapter-b','half-parent-d')""",
            message=ACTIVATION_PARENT_CHECK_ERROR,
        )
        self.reject(
            """INSERT INTO adapter_effective_configurations(
                 configuration_id,configuration_revision,adapter_id,
                 adapter_contract_digest,executable_identity_digest,
                 subject_kind,subject_activation_id,subject_activation_revision,
                 activation_configuration_id,
                 activation_configuration_revision,
                 activation_configuration_digest,
                 activation_configuration_subject_kind,
                 capability_body_digest,permission_profile_digest,
                 discovery_surface_evidence_id,
                 discovery_surface_evidence_revision,
                 discovery_surface_digest,configuration_digest)
               VALUES('activation-with-parent',1,'adapter-a',
                 'contract-adapter-a','executable-adapter-a','activation',
                 'activation-adapter-a',1,'activation-config-adapter-a',1,
                 'activation-config-digest-adapter-a','activation',
                 'body-adapter-a','permission-adapter-a',
                 'surface-id-adapter-a',1,'surface-adapter-a',
                 'activation-with-parent-d')""",
            message=ACTIVATION_PARENT_CHECK_ERROR,
        )
        self.reject(
            smoke_sql,
            (
                "cross-adapter-parent",
                "adapter-b",
                "contract-adapter-b",
                "executable-adapter-b",
                "smoke-b",
                "activation-config-adapter-a",
                "activation-config-digest-adapter-a",
                "body-adapter-b",
                "permission-adapter-b",
                "surface-id-adapter-b",
                1,
                "surface-adapter-b",
                "cross-adapter-parent-d",
            ),
            message="FOREIGN KEY constraint failed",
        )
        self.reject(
            """INSERT INTO adapter_effective_configurations(
                 configuration_id,configuration_revision,adapter_id,
                 adapter_contract_digest,executable_identity_digest,
                 subject_kind,subject_action_adapter_id,subject_action_id,
                 activation_configuration_id,
                 activation_configuration_revision,
                 activation_configuration_digest,
                 activation_configuration_subject_kind,
                 capability_body_digest,permission_profile_digest,
                 discovery_surface_evidence_id,
                 discovery_surface_evidence_revision,
                 discovery_surface_digest,configuration_digest)
               VALUES('nonactivation-parent',1,'adapter-a',
                 'contract-adapter-a','executable-adapter-a','provider-action',
                 'adapter-a','action-a','smoke-config-a',1,
                 'smoke-config-digest-a','activation','body-adapter-a',
                 'permission-adapter-a','surface-id-adapter-a',1,
                 'surface-adapter-a','nonactivation-parent-d')""",
            message="FOREIGN KEY constraint failed",
        )
        self.reject(
            smoke_sql,
            (
                "cross-parent-digest",
                "adapter-b",
                "contract-adapter-b",
                "executable-adapter-b",
                "smoke-b",
                "activation-config-adapter-b",
                "activation-config-digest-adapter-a",
                "body-adapter-b",
                "permission-adapter-b",
                "surface-id-adapter-b",
                1,
                "surface-adapter-b",
                "cross-parent-digest-d",
            ),
            message="FOREIGN KEY constraint failed",
        )
        lineage_smoke_sql = """INSERT INTO adapter_effective_configurations(
          configuration_id,configuration_revision,adapter_id,
          adapter_contract_digest,executable_identity_digest,subject_kind,
          subject_smoke_id,activation_configuration_id,
          activation_configuration_revision,activation_configuration_digest,
          activation_configuration_subject_kind,capability_body_digest,
          permission_profile_digest,discovery_surface_evidence_id,
          discovery_surface_evidence_revision,discovery_surface_digest,
          configuration_digest)
          VALUES(?,1,'adapter-b',?,?,'provider-smoke','smoke-b',
            'activation-config-adapter-b',1,
            'activation-config-digest-adapter-b','activation',
            'body-adapter-b','permission-adapter-b','surface-id-adapter-b',1,
            'surface-adapter-b',?)"""
        self.reject(
            lineage_smoke_sql,
            (
                "cross-parent-contract",
                "contract-adapter-a",
                "executable-adapter-b",
                "cross-parent-contract-d",
            ),
            message="FOREIGN KEY constraint failed",
        )
        self.reject(
            lineage_smoke_sql,
            (
                "cross-parent-executable",
                "contract-adapter-b",
                "executable-adapter-a",
                "cross-parent-executable-d",
            ),
            message="FOREIGN KEY constraint failed",
        )
        self.seed_preflight_and_configuration(
            "adapter-a", "action-valid", kind="generic"
        )
        mark_case()
        self.assert_foreign_keys_clean()

    def test_parent_first_route_admission_and_attached_reservation(self) -> None:
        self.seed_preflight_and_configuration(
            "adapter-a", "generic-before-action", kind="generic"
        )
        self.reject(
            "INSERT INTO provider_action_routes VALUES(" + ",".join("?" * 20) + ")",
            self.route_values("adapter-a", "generic-before-action"),
            message="FOREIGN KEY constraint failed",
        )

        self.seed_preflight_and_configuration(
            "adapter-a", "canonical-cert", kind="certifying"
        )
        self.db.execute("BEGIN IMMEDIATE")
        self.insert_reservation(
            "adapter-a", "canonical-cert", attempt=None, state="preflight"
        )
        self.assertIsNone(
            self.db.execute(
                """SELECT attempt_generation
                   FROM review_finding_capacity_reservations
                   WHERE adapter_id='adapter-a' AND action_id='canonical-cert'"""
            ).fetchone()[0]
        )
        self.db.execute(
            """UPDATE review_finding_capacity_reservations
               SET attempt_generation=1, state='attached'
               WHERE adapter_id='adapter-a' AND action_id='canonical-cert'"""
        )
        self.insert_action("adapter-a", "canonical-cert", ordinal=3)
        self.insert_route(
            self.route_values("adapter-a", "canonical-cert", kind="certifying")
        )
        self.db.execute("COMMIT")
        mark_case()
        self.accept(
            """UPDATE review_finding_capacity_reservations
               SET state='settled'
               WHERE adapter_id='adapter-a' AND action_id='canonical-cert'"""
        )
        self.assert_foreign_keys_clean()

    def test_dispatch_and_observation_bind_full_route_closure(self) -> None:
        self.seed_route("adapter-a", "action-a", ordinal=1)
        self.seed_route("adapter-a", "action-b", ordinal=2)
        self.seed_adapter_primitives("adapter-b")
        self.db.execute(
            """INSERT INTO adapter_capability_snapshots(
                 adapter_id,snapshot_generation,snapshot_id,source,
                 capability_body_digest,snapshot_json,snapshot_digest)
               VALUES('adapter-a',2,'snapshot-id-adapter-a-2',
                 'runtime-discovery','body-refresh',
                 '{"capabilities":{"kind":"available"}}',
                 'snapshot-adapter-a-2')"""
        )
        self.db.execute(
            """INSERT INTO discovery_surface_manifests
               VALUES('surface-id-alt',1,'surface-alt','permission-alt')"""
        )

        self.accept_dispatch(self.dispatch_values("adapter-a", "action-a", 1))

        variants: list[tuple[str, int, Any]] = [
            ("admission", 3, "admission-action-b"),
            ("body", 6, "body-refresh"),
            ("configuration-id", 7, "config-action-b"),
            ("configuration-digest", 9, "config-digest-action-b"),
            ("permission", 10, "permission-alt"),
            ("surface-id", 11, "surface-id-alt"),
            ("surface-digest", 13, "surface-alt"),
        ]
        for ordinal, (name, index, replacement) in enumerate(variants, start=2):
            with self.subTest(field=name):
                values = self.dispatch_values("adapter-a", "action-a", ordinal)
                values[index] = replacement
                if name == "body":
                    values[4] = 2
                    values[5] = "snapshot-adapter-a-2"
                if name == "configuration-id":
                    values[9] = "config-digest-action-b"
                if name == "configuration-digest":
                    values[7] = "config-action-b"
                if name == "surface-id":
                    values[13] = "surface-alt"
                if name == "surface-digest":
                    values[11] = "surface-id-alt"
                values[14] = f"dispatch-cross-{name}"
                self.reject_dispatch(values)

        self.reject(
            """INSERT INTO provider_action_route_observations
               VALUES('adapter-a','action-a','admission-action-b',
                 '{"proved":true}','observation-cross-admission')""",
            message="FOREIGN KEY constraint failed",
        )

        self.db.execute(
            """INSERT INTO adapter_capability_snapshots(
                 adapter_id,snapshot_generation,snapshot_id,source,
                 capability_body_digest,snapshot_json,snapshot_digest)
               VALUES('adapter-a',3,'snapshot-id-adapter-a-3',
                 'version-pinned-conformance','body-adapter-a',
                 '{"capabilities":{"kind":"available"}}',
                 'snapshot-adapter-a-3')"""
        )
        refreshed = self.dispatch_values("adapter-a", "action-a", 20)
        refreshed[4] = 3
        refreshed[5] = "snapshot-adapter-a-3"
        refreshed[14] = "dispatch-instance-refresh"
        self.accept_dispatch(refreshed)
        self.accept(
            """INSERT INTO provider_action_route_observations
               VALUES('adapter-a','action-a','admission-action-a',
                 '{"proved":true}','observation-action-a')"""
        )
        self.assert_foreign_keys_clean()

    def accept_dispatch(self, values: Sequence[Any]) -> None:
        self.insert_dispatch(values)
        mark_case()

    def reject_dispatch(self, values: Sequence[Any]) -> None:
        with self.assertRaises(sqlite3.IntegrityError) as caught:
            self.insert_dispatch(values)
        self.assertEqual(str(caught.exception), "FOREIGN KEY constraint failed")
        mark_case()

    def test_actual_route_result_reservation_and_review_heads(self) -> None:
        self.db.execute("INSERT INTO review_finding_sets VALUES('finding-empty')")
        self.seed_route(
            "adapter-a", "review-a", kind="certifying", target=1, slot="native"
        )
        self.seed_review_terminal("adapter-a", "review-a")
        values = self.evidence_values(
            "adapter-a",
            "review-a",
            observation="observation-review-a",
            actual="actual-proved-mismatch",
        )
        self.insert_evidence(values)
        mark_case()

        self.accept(
            """INSERT INTO review_slot_heads VALUES(
                 'run-1',2,'native',0,NULL,0,NULL,NULL,NULL,
                 'finding-empty','finding-empty',1)"""
        )
        self.accept(
            """INSERT INTO review_slot_heads VALUES(
                 'run-1',1,'native',1,'evidence-1',1,'adapter-a','review-a',
                 'terminal','finding-empty','finding-empty',1)"""
        )
        self.reject(
            """INSERT INTO review_slot_heads VALUES(
                 'run-1',3,'native',1,NULL,0,NULL,NULL,NULL,
                 'finding-empty','finding-empty',1)""",
            message="""CHECK constraint failed: (head_generation=0 AND head_evidence_id IS NULL) OR
    (head_generation>=1 AND head_evidence_id IS NOT NULL)""",
        )
        self.assert_foreign_keys_clean()

    def test_review_evidence_rejects_real_constraints(self) -> None:
        self.seed_route(
            "adapter-a", "review-a", kind="certifying", target=1, slot="native"
        )
        self.seed_route(
            "adapter-b",
            "review-b",
            kind="certifying",
            target=2,
            slot="other-primary",
        )
        self.seed_review_terminal("adapter-a", "review-a")
        self.seed_review_terminal("adapter-b", "review-b")

        without_observation = self.evidence_values(
            "adapter-a",
            "review-a",
            evidence="without-observation",
            observation=None,
            actual="actual-review-b",
        )
        self.reject_evidence(
            without_observation,
            """CHECK constraint failed: actual_route_identity_digest IS NULL OR
    route_observation_digest IS NOT NULL""",
        )

        crossed_reservation = self.evidence_values(
            "adapter-a",
            "review-a",
            evidence="cross-reservation",
            target=2,
            slot="other-primary",
            observation="observation-review-a",
        )
        crossed_reservation[13] = "reservation-review-b"
        self.reject_evidence(
            crossed_reservation,
            "FOREIGN KEY constraint failed",
        )

        skipped_generation = self.evidence_values(
            "adapter-a",
            "review-a",
            evidence="skipped-generation",
            observation="observation-review-a",
            prior_generation=0,
            new_generation=2,
        )
        self.reject_evidence(
            skipped_generation,
            "CHECK constraint failed: new_head_generation=prior_head_generation+1",
        )
        self.assert_foreign_keys_clean()

    def test_review_evidence_prior_chain_accepts_exact_target_and_slot(self) -> None:
        self.seed_route(
            "adapter-a", "review-a", kind="certifying", target=1,
            slot="native", ordinal=1,
        )
        self.seed_review_terminal("adapter-a", "review-a")
        first = self.evidence_values(
            "adapter-a", "review-a", evidence="evidence-first",
            observation="observation-review-a",
        )
        self.insert_evidence(first)
        mark_case()

        self.seed_route(
            "adapter-b", "review-b", kind="certifying", target=1,
            slot="native", attempt=2, ordinal=2,
        )
        self.seed_review_terminal("adapter-b", "review-b")
        successor = self.evidence_values(
            "adapter-b", "review-b", evidence="evidence-successor",
            observation="observation-review-b", prior_generation=1,
            new_generation=2, prior_evidence="evidence-first",
        )
        successor[12] = 2
        self.insert_evidence(successor)
        mark_case()

        self.assert_foreign_keys_clean()

    def reject_evidence(
        self,
        values: Sequence[Any],
        message: str,
    ) -> None:
        with self.assertRaises(sqlite3.IntegrityError) as caught:
            self.insert_evidence(values)
        self.assertEqual(str(caught.exception), message)
        mark_case()

    def test_pressure_child_fk_and_compare_delete_crash_matrix(self) -> None:
        self.seed_pressure()
        before = self.capture_pressure()
        self.assertIsNotNone(before)

        self.accept(
            """UPDATE agent_adapter_bindings SET revision=revision+1
               WHERE run_id='run-1' AND agent_id='agent-1'"""
        )
        with self.assertRaises(sqlite3.IntegrityError) as caught:
            self.db.execute(
                """UPDATE agent_adapter_bindings SET adapter_id='adapter-crossed'
                   WHERE run_id='run-1' AND agent_id='agent-1'"""
            )
        self.assertEqual(str(caught.exception), "FOREIGN KEY constraint failed")
        mark_case()
        with self.assertRaises(sqlite3.IntegrityError) as caught:
            self.db.execute(
                """DELETE FROM agent_adapter_bindings
                   WHERE run_id='run-1' AND agent_id='agent-1'"""
            )
        self.assertEqual(str(caught.exception), "FOREIGN KEY constraint failed")
        mark_case()

        for fault in ("before-clear", "after-clear", "after-binding-update"):
            with self.subTest(fault=fault):
                with self.assertRaises(RuntimeError):
                    self.adopt_binding(fault=fault)
                self.assertEqual(self.capture_pressure(), before)
                self.assertEqual(
                    self.db.execute(
                        """SELECT adapter_id,provider_generation,revision
                           FROM agent_adapter_bindings
                           WHERE run_id='run-1' AND agent_id='agent-1'"""
                    ).fetchone(),
                    ("adapter-old", 1, 2),
                )
                mark_case()

        with self.assertRaises(sqlite3.IntegrityError) as caught:
            self.adopt_binding(cross_before_clear=True)
        self.assertEqual(
            str(caught.exception),
            "pressure-compare-delete-crossed",
        )
        self.assertEqual(self.capture_pressure(), before)
        mark_case()

        self.adopt_binding()
        self.assertIsNone(self.capture_pressure())
        self.assertEqual(
            self.db.execute(
                """SELECT adapter_id,provider_generation,context_revision,revision
                   FROM agent_adapter_bindings
                   WHERE run_id='run-1' AND agent_id='agent-1'"""
            ).fetchone(),
            ("adapter-new", 2, 1, 3),
        )
        mark_case()

        self.accept(
            """INSERT INTO provider_context_pressure_current
               VALUES('run-1','agent-1','adapter-new',2,1,'event-new','low',
                 'native-exact','exact',100,10,90,
                 '2026-07-14T02:00:00Z','2026-07-14T03:00:00Z',
                 'pressure-new',1)"""
        )
        self.reject(
            """UPDATE provider_context_observation_audit
               SET evidence_digest='mutated'
               WHERE observation_id='obs-new'""",
            message="provider-context-observation-immutable",
        )
        self.assert_foreign_keys_clean()

    def test_pressure_absent_arm_commits_without_rekey(self) -> None:
        self.seed_pressure()
        self.db.execute(
            "DELETE FROM provider_context_pressure_current WHERE run_id='run-1' AND agent_id='agent-1'"
        )
        self.adopt_binding()
        self.assertEqual(
            self.db.execute(
                """SELECT adapter_id,provider_generation,revision
                   FROM agent_adapter_bindings
                   WHERE run_id='run-1' AND agent_id='agent-1'"""
            ).fetchone(),
            ("adapter-new", 2, 2),
        )
        self.assertIsNone(self.capture_pressure())
        mark_case()
        self.assert_foreign_keys_clean()

    def test_generic_recovery_owner_partition_and_no_redispatch(self) -> None:
        rows = (
            (
                "adapter-g",
                "generic-missing",
                1,
                1,
                "ordinary",
                0,
                "missing",
                "resolved",
                1,
                "GenericProviderRouteRecoveryService",
            ),
            (
                "adapter-g",
                "generic-corrupt",
                1,
                1,
                "ordinary",
                0,
                "integrity-failed",
                "resolved",
                2,
                "GenericProviderRouteRecoveryService",
            ),
            (
                "adapter-g",
                "generic-unresolved",
                1,
                1,
                "ordinary",
                0,
                "present",
                "unresolved",
                3,
                "GenericProviderRouteRecoveryService",
            ),
            (
                "adapter-c",
                "certifying",
                1,
                1,
                "ordinary",
                1,
                "missing",
                "unresolved",
                4,
                "ProviderRouteIntegrityRecoveryService",
            ),
            (
                "adapter-l",
                "lifecycle",
                1,
                1,
                "lifecycle",
                0,
                "missing",
                "unresolved",
                5,
                "LifecycleRotationRecoveryService",
            ),
            (
                "adapter-l",
                "launch",
                1,
                1,
                "launch",
                0,
                "missing",
                "unresolved",
                6,
                "LaunchCustodyRecoveryService",
            ),
            (
                "adapter-g",
                "healthy",
                1,
                1,
                "ordinary",
                0,
                "present",
                "resolved",
                7,
                "none",
            ),
            (
                "adapter-g",
                "not-answer-bearing",
                1,
                0,
                "ordinary",
                0,
                "missing",
                "unresolved",
                8,
                "none",
            ),
        )
        for row in rows:
            self.db.execute(
                "INSERT INTO provider_recovery_inventory VALUES(?,?,?,?,?,?,?,?,?)",
                row[:9],
            )
        for row in rows:
            with self.subTest(action=row[1]):
                owner = self.db.execute(
                    """SELECT owner FROM provider_recovery_owners
                       WHERE adapter_id=? AND action_id=?""",
                    row[:2],
                ).fetchone()
                self.assertEqual(owner, (row[9],))
                mark_case()

        overlap = self.db.execute(
            """SELECT adapter_id,action_id,
                      lifecycle_match+launch_match+certifying_match+generic_match
               FROM provider_recovery_owner_matches
               WHERE lifecycle_match+launch_match+certifying_match+generic_match>1"""
        ).fetchall()
        self.assertEqual(overlap, [])
        mark_case()

        self.accept(
            """INSERT INTO generic_provider_route_recovery_evidence
               VALUES('adapter-g','generic-missing','missing',
                 'recovery-generic-missing',1)"""
        )
        self.accept(
            """INSERT INTO generic_provider_route_recovery_evidence
               VALUES('adapter-g','generic-corrupt','integrity-failed',
                 'recovery-generic-corrupt',2)"""
        )
        visible = self.db.execute(
            """SELECT e.action_id,e.route_state,e.recovery_evidence_digest
               FROM generic_provider_route_recovery_evidence e
               JOIN provider_recovery_owners o USING(adapter_id,action_id)
               WHERE o.owner='GenericProviderRouteRecoveryService'
               ORDER BY e.action_id"""
        ).fetchall()
        self.assertEqual(
            visible,
            [
                (
                    "generic-corrupt",
                    "integrity-failed",
                    "recovery-generic-corrupt",
                ),
                ("generic-missing", "missing", "recovery-generic-missing"),
            ],
        )
        mark_case()
        dispatches = self.db.execute(
            """SELECT action_id,dispatch_count FROM provider_recovery_inventory
               WHERE action_id IN ('generic-missing','generic-corrupt')
               ORDER BY action_id"""
        ).fetchall()
        self.assertEqual(
            dispatches, [("generic-corrupt", 2), ("generic-missing", 1)]
        )
        mark_case()
        self.assert_foreign_keys_clean()

    def test_representative_database_finishes_with_empty_fk_check(self) -> None:
        self.seed_scope_checkpoint("ps-1", "run-1", 0, "scope-zero")
        self.db.execute(
            "INSERT INTO lifecycle_receipt_scope_heads VALUES('ps-1','run-1','scope-zero',1)"
        )
        custody = self.seed_custody_revision(
            "agent-c", "custody-c", "prepared", "none"
        )
        self.db.execute(
            "INSERT INTO lifecycle_rotation_custody_heads VALUES(?,?,?,?,?,?,?,?,?,?,?,?)",
            (*custody[:7], *custody[7:], 0, 1),
        )
        loss = self.seed_loss_revision("agent-l", "loss-l", "open", "none")
        self.db.execute(
            "INSERT INTO lifecycle_generation_loss_heads VALUES(?,?,?,?,?,?,?,?,?,?,?,?)",
            (*loss[:7], *loss[7:], 0, 1),
        )
        self.seed_route(
            "adapter-a", "review-a", kind="certifying", target=1, slot="native"
        )
        self.seed_review_terminal("adapter-a", "review-a")
        self.insert_evidence(
            self.evidence_values(
                "adapter-a",
                "review-a",
                observation="observation-review-a",
                actual="actual-review-a",
            )
        )
        self.seed_pressure()
        self.db.execute(
            """INSERT INTO provider_recovery_inventory
               VALUES('adapter-g','generic',1,1,'ordinary',0,'missing',
                 'unresolved',0)"""
        )
        self.db.execute(
            """INSERT INTO generic_provider_route_recovery_evidence
               VALUES('adapter-g','generic','missing','recovery-generic',0)"""
        )
        self.assert_foreign_keys_clean()
        mark_case()


if __name__ == "__main__":
    suite = unittest.defaultTestLoader.loadTestsFromTestCase(
        LaneAHeadsRouteMiscOracle
    )
    result = unittest.TextTestRunner(verbosity=2).run(suite)
    print(f"ORACLE_CASES={CASES_RUN}")
    raise SystemExit(0 if result.wasSuccessful() else 1)
