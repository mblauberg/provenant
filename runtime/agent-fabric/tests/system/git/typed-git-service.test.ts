import Database from "better-sqlite3";
import { afterEach, describe, expect, it } from "vitest";

import { parseIdentifier, parseOperatorCapabilityGrant } from "@local/agent-fabric-protocol";
import type {
  GitActionGrant,
  GitRepositoryBinding,
  OperatorGitIntent,
  Sha256Digest,
} from "@local/agent-fabric-protocol";

import { applyMigrations } from "../../../src/core/migrations.ts";
import { ProviderActionAdmissionCoordinator } from "../../../src/application/provider-action-admission.ts";
import { readGlobalLiveness } from "../../../src/lifecycle/global-liveness.ts";
import { canonicalJson, sha256 } from "../../../src/persistence/row-codec.ts";
import { OperatorActionStore } from "../../../src/operator/action-store.ts";
import type { GitMutationDispatchContext, GitMutationInspection, GitMutationPort } from "../../../src/operator/fixed-git-mutation-port.ts";
import { createProductionOperatorActionPorts } from "../../../src/operator/production-action-ports.ts";
import { OperatorStore } from "../../../src/operator/store.ts";
import { NotificationOutbox } from "../../../src/attention/outbox.ts";
import {
  TypedGitService,
  deriveGitEffectBindingDigest,
  deriveGitGrantDigest,
  derivePreauthorisedGitOperationId,
  deriveGitResultRecipeDigest,
} from "../../../src/operator/typed-git-service.ts";

const databases: Database.Database[] = [];
const now = Date.parse("2026-07-12T10:00:00.000Z");
const sha = (value: string): Sha256Digest => `sha256:${value.repeat(64).slice(0, 64)}` as Sha256Digest;

function seedGateAttention(
  database: Database.Database,
  gateId: string,
  title: string,
  summary: string,
): void {
  const outbox = new NotificationOutbox({ database, clock: () => now });
  const producer = {
    producerId: "operator:operator_01",
    projectId: "project_01",
    projectSessionId: "session_01",
    coordinationRunId: "run_01",
    principalGeneration: 1,
  } as const;
  const attention = outbox.upsertAttention(producer, {
    dedupeKey: `scoped-gate:${gateId}`,
    kind: "consequential-gate",
    severity: "critical",
    payload: { gateId, title, summary, priority: "critical-path", duplicateCount: 1 },
  });
  outbox.enqueue(producer, {
    itemId: attention.itemId,
    expectedItemRevision: attention.revision,
    targetIntegration: "native-desktop",
  });
}

afterEach(() => {
  for (const database of databases.splice(0)) database.close();
});

class FakeGitPort implements GitMutationPort {
  dispatchAttemptCount = 0;
  dispatchCount = 0;
  inspectCount = 0;
  onObserve: (() => void) | null = null;
  onBeforePointOfUse: (() => void) | null = null;
  observation: GitRepositoryBinding;
  outcome: GitMutationInspection;

  constructor(observation: GitRepositoryBinding) {
    this.observation = observation;
    this.outcome = {
      outcome: "exact-applied",
      repository: { ...observation, repositoryStateDigest: sha("f") },
      evidenceDigest: sha("e"),
      failureSignatureDigest: null,
      conflict: null,
    };
  }

  assertAvailable(): void {}

  observe(): Promise<GitRepositoryBinding> {
    this.onObserve?.();
    this.onObserve = null;
    return Promise.resolve(this.observation);
  }

  dispatch(
    _intent: OperatorGitIntent,
    _context: GitMutationDispatchContext,
    pointOfUse: () => void,
  ): Promise<GitMutationInspection> {
    this.dispatchAttemptCount += 1;
    this.onBeforePointOfUse?.();
    this.onBeforePointOfUse = null;
    pointOfUse();
    this.dispatchCount += 1;
    return Promise.resolve(this.outcome);
  }

  inspect(_intent: OperatorGitIntent, _context: GitMutationDispatchContext): Promise<GitMutationInspection> {
    this.inspectCount += 1;
    return Promise.resolve(this.outcome);
  }
}

function fixture(): {
  database: Database.Database;
  service: TypedGitService;
  port: FakeGitPort;
  intent: OperatorGitIntent;
  request: Parameters<TypedGitService["prepare"]>[0];
} {
  const database = new Database(":memory:");
  databases.push(database);
  applyMigrations(database);
  const projectId = parseIdentifier<"ProjectId">("project_01", "test.projectId");
  const projectSessionId = parseIdentifier<"ProjectSessionId">("session_01", "test.projectSessionId");
  const coordinationRunId = parseIdentifier<"CoordinationRunId">("run_01", "test.coordinationRunId");
  const repository: GitRepositoryBinding = {
    repositoryRoot: "/repo",
    worktreePath: "/repo/.worktrees/writer",
    gitCommonDir: "/repo/.git",
    commonDirectoryIdentityDigest: sha("1"),
    repositoryStateDigest: sha("2"),
    headDigest: sha("3"),
    indexDigest: sha("4"),
    worktreeDigest: sha("5"),
    remoteStateDigest: sha("6"),
    configDigest: sha("7"),
    worktreeRegistryDigest: sha("8"),
  };
  const profile = {
    profileId: "sealed-git-v1",
    revision: 1,
    digest: sha("9"),
    gitBinaryDigest: sha("a"),
    objectFormat: "sha1" as const,
  };
  const recipeWithoutDigest = {
    schemaVersion: 1 as const,
    executionProfileDigest: profile.digest,
    beforeRepositoryStateDigest: repository.repositoryStateDigest,
    expectedSuccessRepositoryStateDigest: sha("f"),
    expectedConflict: null,
    refUpdates: [],
    configUpdates: [],
    commitMappings: [],
    affectedPaths: [{ path: "src/index.ts", beforeDigest: null, afterDigest: sha("b") }],
    bounds: { maximumRefOrConfigUpdates: 64 as const, maximumCommitMappings: 128 as const, maximumConflictPaths: 4096 as const },
  };
  const resultRecipeDigest = deriveGitResultRecipeDigest(recipeWithoutDigest);
  const grantWithoutDigest = {
    grantId: "grant_01",
    revision: 1,
    projectId,
    projectSessionId,
    sessionGeneration: 1,
    issuingSessionRevision: 2,
    coordinationRunId,
    issuingRunRevision: 4,
    issuingDependencyRevision: 1,
    authorityRef: sha("c"),
    authorityRevision: 1,
    gitAllowlistEpoch: 1,
    gitAllowlistDigest: sha("d"),
    repositoryRoot: repository.repositoryRoot,
    worktreePath: repository.worktreePath,
    executionProfileId: profile.profileId,
    executionProfileRevision: 1,
    executionProfileDigest: profile.digest,
    constraints: {
      operationVariants: ["stage"] as const,
      remoteBindings: [],
      refs: [],
      pathPrefixes: ["src"],
      allowWorktreeCreation: false,
    },
    sourceAuthority: { kind: "operator-command" as const, digest: sha("e") },
    expiresAt: "2026-07-12T11:00:00.000Z" as GitActionGrant["expiresAt"],
  };
  const grant: GitActionGrant = { ...grantWithoutDigest, grantDigest: deriveGitGrantDigest(grantWithoutDigest) };
  const operation = { variant: "stage" as const, paths: ["src/index.ts"] };
  const effectBindingDigest = deriveGitEffectBindingDigest({
    projectId: "project_01",
    projectSessionId: "session_01",
    coordinationRunId: "run_01",
    authorityRef: grant.authorityRef,
    authorityRevision: 1,
    gitAllowlistEpoch: 1,
    gitAllowlistDigest: grant.gitAllowlistDigest,
    repository,
    executionProfile: profile,
    remoteBinding: null,
    operation,
    resultRecipeDigest,
  });
  const operationId = derivePreauthorisedGitOperationId({
    operatorId: "operator_01",
    projectId,
    projectSessionId,
    previewId: "preview_git_01",
    effectBindingDigest,
  });
  const intent = {
    kind: "git",
    authorisation: {
      projectId,
      projectSessionId,
      expectedSessionRevision: 2,
      expectedSessionGeneration: 1,
      coordinationRunId,
      expectedRunRevision: 4,
      expectedDependencyRevision: 1,
      authorityRef: grant.authorityRef,
      expectedAuthorityRevision: 1,
      expectedGitAllowlistEpoch: 1,
      gitAllowlistDigest: grant.gitAllowlistDigest,
      repositoryRoot: repository.repositoryRoot,
      worktreePath: repository.worktreePath,
      repositoryStateDigest: repository.repositoryStateDigest,
      executionProfileId: profile.profileId,
      executionProfileRevision: 1,
      executionProfileDigest: profile.digest,
      operationVariant: "stage",
      remoteBinding: null,
      resultRecipeDigest,
      operationId,
      effectBindingDigest,
      decision: { kind: "preauthorised", grantId: grant.grantId, expectedGrantRevision: 1, grantDigest: grant.grantDigest },
    },
    repository,
    executionProfile: profile,
    operation,
    resultRecipe: { ...recipeWithoutDigest, resultRecipeDigest },
  } satisfies OperatorGitIntent;
  database.exec(`
    INSERT INTO projects(project_id,canonical_root,trust_record_digest,revision,authority_generation,created_at,updated_at)
    VALUES('project_01','/repo','${sha("1")}',1,1,1,1);
    INSERT INTO project_sessions(
      project_session_id,project_id,mode,state,revision,generation,authority_ref,budget_ref,
      launch_packet_path,launch_packet_digest,membership_revision,origin_kind,origin_operator_id,created_at,updated_at
    ) VALUES('session_01','project_01','coordinated','active',2,1,'${grant.authorityRef}','budget_01',
      'launch.json','${sha("1")}',1,'operator-launch','operator_01',1,1);
    INSERT INTO runs(
      run_id,chair_agent_id,workspace_root,project_run_directory,created_at,project_session_id,lifecycle_state,
      revision,chair_generation,chair_lease_id,authority_ref,budget_ref,dependency_revision,topology_slot,
      project_run_directory_basis,authority_revision,git_allowlist_epoch,git_allowlist_digest
    ) VALUES('run_01','chair_01','/repo','.agent-run/AFAB-004',1,'session_01','active',4,1,'chair:run_01:1',
      '${grant.authorityRef}','budget_01',1,1,'project-relative',1,1,'${grant.gitAllowlistDigest}');
    INSERT INTO run_authority_revisions(
      project_session_id,coordination_run_id,authority_revision,authority_ref,git_allowlist_epoch,
      git_allowlist_digest,activated_at_run_revision,created_at
    ) VALUES('session_01','run_01',1,'${grant.authorityRef}',1,'${grant.gitAllowlistDigest}',4,1);
    INSERT INTO resource_scopes(scope_id,project_id,project_session_id,coordination_run_id,scope_kind,owner_ref,state,revision)
    VALUES('scope_run','project_01','session_01','run_01','coordination-run','run_01','active',1);
    INSERT INTO resource_reservations(
      reservation_id,project_session_id,coordination_run_id,leaf_scope_id,operation_id,actor_agent_id,
      state,revision,generation,identity_hash,path_json,amounts_json,created_at,updated_at
    ) VALUES('writer_resource','session_01','run_01','scope_run',NULL,'chair_01','reserved',1,1,'writer_hash','[]','{}',1,1);
    INSERT INTO writer_admissions(writer_admission_id,reservation_id,repository_root,worktree_path,writer_generation,state)
    VALUES('writer_01','writer_resource','/repo','/repo/.worktrees/writer',1,'active');
    INSERT INTO writer_prefixes(writer_admission_id,canonical_prefix) VALUES('writer_01','src');
    INSERT INTO git_execution_profiles(
      profile_id,revision,profile_digest,git_binary_path,git_binary_version,git_binary_digest,object_format,
      merge_backend_id,rebase_backend_id,environment_digest,helper_registry_digest,inspector_digest,state,created_at
    ) VALUES('sealed-git-v1',1,'${profile.digest}','/usr/bin/git','2.39','${profile.gitBinaryDigest}','sha1',
      'merge-ort-v1','rebase-apply-v1','${sha("1")}','${sha("2")}','${sha("3")}','active',1);
    INSERT INTO run_git_allowlists(
      project_session_id,coordination_run_id,authority_revision,git_allowlist_epoch,git_allowlist_digest,
      allow_worktree_creation,maximum_expiry,constraints_json,created_at
    ) VALUES('session_01','run_01',1,1,'${grant.gitAllowlistDigest}',1,${now + 7_200_000},'{}',1);
    INSERT INTO run_git_allowlist_variants VALUES('session_01','run_01',1,1,'stage');
    INSERT INTO run_git_allowlist_variants VALUES('session_01','run_01',1,1,'worktree-create-detached');
    INSERT INTO run_git_allowlist_profiles
    VALUES('session_01','run_01',1,1,'sealed-git-v1',1,'${profile.digest}');
    INSERT INTO run_git_allowlist_paths
    VALUES('session_01','run_01',1,1,'/repo','/repo/.worktrees/writer','src');
  `);
  database.prepare(`
    INSERT INTO operator_git_grants(
      grant_id,revision,project_id,project_session_id,session_generation,issuing_session_revision,
      coordination_run_id,issuing_run_revision,issuing_dependency_revision,authority_ref,authority_revision,
      git_allowlist_epoch,git_allowlist_digest,repository_root,worktree_path,execution_profile_id,
      execution_profile_revision,execution_profile_digest,allow_worktree_creation,source_kind,source_digest,
      constraints_json,grant_digest,state,expires_at,created_at,revoked_at
    ) VALUES(?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,NULL)
  `).run(
    grant.grantId, grant.revision, grant.projectId, grant.projectSessionId, grant.sessionGeneration,
    grant.issuingSessionRevision, grant.coordinationRunId, grant.issuingRunRevision,
    grant.issuingDependencyRevision, grant.authorityRef, grant.authorityRevision, grant.gitAllowlistEpoch,
    grant.gitAllowlistDigest, grant.repositoryRoot, grant.worktreePath, grant.executionProfileId,
    grant.executionProfileRevision, grant.executionProfileDigest, 0, "operator-command", grant.sourceAuthority.digest,
    JSON.stringify(grant.constraints), grant.grantDigest, "active", Date.parse(grant.expiresAt), 1,
  );
  database.prepare("INSERT INTO operator_git_grant_variants VALUES('grant_01',1,'stage')").run();
  database.prepare("INSERT INTO operator_git_grant_paths VALUES('grant_01',1,'src')").run();
  const port = new FakeGitPort(repository);
  const service = new TypedGitService({
    database,
    gitPort: port,
    conflictInspector: { inspect: async (candidate) => await port.inspect(candidate, { remoteTarget: null }) },
    clock: () => now,
    daemonInstanceId: "daemon_01",
  });
  const request = {
    commandId: "commit_git_01",
    previewId: "preview_git_01",
    operatorId: "operator_01",
    projectId: "project_01",
    projectSessionId: "session_01",
    principalGeneration: 1,
    operation: "git",
    intent,
    intentDigest: sha("a"),
    beforeStateDigest: sha("b"),
    attemptGeneration: 1,
  };
  database.prepare(`
    INSERT INTO operator_effect_custody(
      custody_id,operator_id,project_id,project_session_id,principal_generation,command_id,operation,
      intent_digest,before_state_digest,intent_json,state,created_at,updated_at
    ) VALUES(?,?,?,?,?,?,?,?,?,?,'prepared',?,?)
  `).run(
    service.custodyId(request), request.operatorId, request.projectId, request.projectSessionId,
    request.principalGeneration, request.commandId, request.operation, request.intentDigest,
    request.beforeStateDigest, canonicalJson(request.intent), now, now,
  );
  return { database, service, port, intent, request };
}

describe("typed Git effect custody", () => {
  it("atomically prepares four owners, dispatches once and replays terminal proof without Git I/O", async () => {
    const value = fixture();

    value.service.prepare(value.request);
    expect(value.database.prepare(`
      SELECT b.state AS binding_state,c.state AS custody_state,a.state AS admission_state,r.state AS reservation_state
        FROM operator_git_effect_bindings b
        JOIN operator_effect_custody c ON c.custody_id=b.custody_id
        JOIN operation_admissions a ON a.operation_id=b.operation_id
        JOIN git_mutation_reservations r ON r.custody_id=b.custody_id
    `).get()).toEqual({
      binding_state: "prepared",
      custody_state: "prepared",
      admission_state: "authorised",
      reservation_state: "reserved",
    });
    expect(() => value.database.prepare(
      "UPDATE operator_effect_custody SET state='conflict' WHERE custody_id=?",
    ).run(value.service.custodyId(value.request))).toThrow(/git_four_owner_map/iu);

    await expect(value.service.dispatch(value.request)).resolves.toMatchObject({ status: "committed" });
    await expect(value.service.dispatch(value.request)).resolves.toMatchObject({ status: "committed" });
    expect(value.port.dispatchAttemptCount).toBe(1);
    expect(value.port.dispatchCount).toBe(1);
    expect(value.database.prepare(`
      SELECT b.state AS binding_state,c.state AS custody_state,a.state AS admission_state,r.state AS reservation_state
        FROM operator_git_effect_bindings b
        JOIN operator_effect_custody c ON c.custody_id=b.custody_id
        JOIN operation_admissions a ON a.operation_id=b.operation_id
        JOIN git_mutation_reservations r ON r.custody_id=b.custody_id
    `).get()).toEqual({
      binding_state: "applied",
      custody_state: "terminal",
      admission_state: "terminal",
      reservation_state: "released",
    });
  });

  it("rejects a sibling grant before preparing custody or calling Git", () => {
    const value = fixture();
    value.database.prepare("UPDATE operator_git_grant_variants SET operation_variant='unstage'").run();

    expect(() => value.service.prepare(value.request)).toThrow(/grant|variant|authority/iu);
    expect(value.port.dispatchCount).toBe(0);
    expect(value.database.prepare("SELECT COUNT(*) AS count FROM operator_git_effect_bindings").get()).toEqual({ count: 0 });
    expect(value.database.prepare("SELECT COUNT(*) AS count FROM git_mutation_reservations").get()).toEqual({ count: 0 });
  });

  it("rechecks the live grant after async observation and before claiming dispatch", async () => {
    const value = fixture();
    value.service.prepare(value.request);
    value.port.onObserve = () => {
      value.database.prepare(`
        UPDATE operator_git_grants SET state='revoked',revoked_at=?
         WHERE grant_id='grant_01' AND revision=1
      `).run(now);
    };

    await expect(value.service.dispatch(value.request)).rejects.toMatchObject({ code: "CAPABILITY_FORBIDDEN" });
    expect(value.port.dispatchCount).toBe(0);
    expect(value.database.prepare(`
      SELECT b.state AS binding_state,c.state AS custody_state,a.state AS admission_state,r.state AS reservation_state
        FROM operator_git_effect_bindings b
        JOIN operator_effect_custody c ON c.custody_id=b.custody_id
        JOIN operation_admissions a ON a.operation_id=b.operation_id
        JOIN git_mutation_reservations r ON r.custody_id=b.custody_id
    `).get()).toEqual({
      binding_state: "prepared",
      custody_state: "prepared",
      admission_state: "authorised",
      reservation_state: "reserved",
    });
  });

  it("claims authority only from the mutation port point-of-use boundary", async () => {
    const value = fixture();
    value.service.prepare(value.request);
    value.port.onBeforePointOfUse = () => {
      value.database.prepare(`
        UPDATE operator_git_grants SET state='revoked',revoked_at=?
         WHERE grant_id='grant_01' AND revision=1
      `).run(now);
    };

    await expect(value.service.dispatch(value.request)).rejects.toMatchObject({ code: "CAPABILITY_FORBIDDEN" });
    expect(value.port.dispatchAttemptCount).toBe(1);
    expect(value.port.dispatchCount).toBe(0);
    expect(value.database.prepare(`
      SELECT b.state AS binding_state,c.state AS custody_state,a.state AS admission_state,r.state AS reservation_state
        FROM operator_git_effect_bindings b
        JOIN operator_effect_custody c ON c.custody_id=b.custody_id
        JOIN operation_admissions a ON a.operation_id=b.operation_id
        JOIN git_mutation_reservations r ON r.custody_id=b.custody_id
    `).get()).toEqual({
      binding_state: "prepared",
      custody_state: "prepared",
      admission_state: "authorised",
      reservation_state: "reserved",
    });
  });

  it.each([
    ["run authority", "UPDATE runs SET revision=revision+1 WHERE run_id='run_01'"],
    ["execution profile", "UPDATE git_execution_profiles SET state='revoked' WHERE profile_id='sealed-git-v1' AND revision=1"],
    ["writer admission", "UPDATE writer_admissions SET state='revoked' WHERE writer_admission_id='writer_01'"],
  ])("rechecks %s after async observation before Git I/O", async (_name, mutation) => {
    const value = fixture();
    value.service.prepare(value.request);
    value.port.onObserve = () => value.database.exec(mutation);

    await expect(value.service.dispatch(value.request)).rejects.toBeInstanceOf(Error);
    expect(value.port.dispatchCount).toBe(0);
  });

  it("retains intact conflict custody and quarantines destroyed conflict only through explicit read-only reconciliation", async () => {
    const value = fixture();
    value.service.prepare(value.request);
    value.port.outcome = {
      outcome: "exact-conflict",
      repository: value.intent.repository,
      evidenceDigest: sha("1"),
      failureSignatureDigest: null,
      conflict: {
        kind: "merge",
        operationStateDigest: sha("2"),
        indexDigest: sha("3"),
        worktreeDigest: sha("4"),
        conflictPaths: [],
      },
    };
    await value.service.dispatch(value.request);
    const conflict = value.service.status(value.request.commandId, value.request.intentDigest);
    expect(conflict).toMatchObject({ status: "conflict", gitCustody: { ownedConflictGeneration: 1, lookupGeneration: 1 } });
    if (conflict?.status !== "conflict") throw new Error("expected typed Git conflict status");
    expect(readGlobalLiveness(value.database, { now, daemonInstanceGeneration: 1 })).toMatchObject({
      failClosed: false,
      contributors: { operatorEffects: 1 },
    });

    const recovered = await value.service.recover();
    expect(recovered).toEqual({ reconciled: 0, quarantined: 0 });
    expect(value.port.inspectCount).toBe(0);

    const current = conflict.gitCustody;
    const retained = await value.service.reconcileConflict({
      reconciliationCommandId: "reconcile_git_01",
      targetCommandId: value.request.commandId,
      intentDigest: value.request.intentDigest,
      nextAttemptGeneration: 2,
      binding: {
        kind: "owned-conflict",
        custodyId: current.custodyId,
        expectedBindingState: "conflict",
        expectedBindingStateRevision: current.bindingStateRevision,
        expectedOwnedConflictGeneration: current.ownedConflictGeneration ?? 0,
        expectedPredecessorCustodyId: null,
        expectedPredecessorConflictGeneration: null,
        expectedReservationGeneration: current.reservationGeneration,
        expectedCommonDirectoryIdentityDigest: current.commonDirectoryIdentityDigest,
        expectedLookupGeneration: current.lookupGeneration,
        expectedLookupEvidenceDigest: current.lookupEvidenceDigest,
        expectedResolutionEligibility: "none",
      },
    });
    expect(retained).toMatchObject({
      status: "conflict",
      attemptGeneration: 2,
      gitCustody: { ownedConflictGeneration: 1, lookupGeneration: 2, resolutionEligibility: { kind: "none" } },
    });
    expect(value.port.inspectCount).toBe(1);

    value.port.outcome = {
      outcome: "exact-no-effect",
      repository: value.intent.repository,
      evidenceDigest: sha("5"),
      failureSignatureDigest: null,
      conflict: null,
    };
    if (retained.status !== "conflict") throw new Error("expected retained conflict status");
    const latest = retained.gitCustody;
    const quarantined = await value.service.reconcileConflict({
      reconciliationCommandId: "reconcile_git_02",
      targetCommandId: value.request.commandId,
      intentDigest: value.request.intentDigest,
      nextAttemptGeneration: 3,
      binding: {
        kind: "owned-conflict",
        custodyId: latest.custodyId,
        expectedBindingState: "conflict",
        expectedBindingStateRevision: latest.bindingStateRevision,
        expectedOwnedConflictGeneration: latest.ownedConflictGeneration ?? 0,
        expectedPredecessorCustodyId: null,
        expectedPredecessorConflictGeneration: null,
        expectedReservationGeneration: latest.reservationGeneration,
        expectedCommonDirectoryIdentityDigest: latest.commonDirectoryIdentityDigest,
        expectedLookupGeneration: latest.lookupGeneration,
        expectedLookupEvidenceDigest: latest.lookupEvidenceDigest,
        expectedResolutionEligibility: "none",
      },
    });
    expect(quarantined).toMatchObject({
      status: "quarantined",
      attemptGeneration: 3,
      gitCustody: {
        lookupGeneration: 3,
        lookupOutcome: "conflict-state-unverifiable",
        resolutionEligibility: { kind: "eligible", reason: "conflict-state-unverifiable" },
      },
    });
    expect(value.port.dispatchCount).toBe(1);
    expect(value.port.inspectCount).toBe(2);
    expect(value.database.prepare(`
      SELECT b.state AS binding_state,c.state AS custody_state,a.state AS admission_state,r.state AS reservation_state
        FROM operator_git_effect_bindings b
        JOIN operator_effect_custody c ON c.custody_id=b.custody_id
        JOIN operation_admissions a ON a.operation_id=b.operation_id
        JOIN git_mutation_reservations r ON r.custody_id=b.custody_id
    `).get()).toEqual({
      binding_state: "quarantined",
      custody_state: "quarantined",
      admission_state: "quarantined",
      reservation_state: "quarantined",
    });
  });

  it("recovers an undispatched ordinary action as proved no-effect without inspection", async () => {
    const value = fixture();
    value.service.prepare(value.request);

    await expect(value.service.observe(value.request)).resolves.toMatchObject({ status: "rejected", code: "state-changed" });
    expect(value.port.dispatchCount).toBe(0);
    expect(value.port.inspectCount).toBe(0);
    expect(value.database.prepare(`
      SELECT b.state AS binding_state,c.state AS custody_state,a.state AS admission_state,r.state AS reservation_state
        FROM operator_git_effect_bindings b
        JOIN operator_effect_custody c ON c.custody_id=b.custody_id
        JOIN operation_admissions a ON a.operation_id=b.operation_id
        JOIN git_mutation_reservations r ON r.custody_id=b.custody_id
    `).get()).toEqual({
      binding_state: "rejected",
      custody_state: "no-effect",
      admission_state: "cancelled",
      reservation_state: "released",
    });
  });

  it("is reachable through the production operator state/effect composition", async () => {
    const value = fixture();
    value.database.prepare("DELETE FROM operator_effect_custody").run();
    const ports = createProductionOperatorActionPorts({
      database: value.database,
      clock: () => now,
      providerActionAdmission: new ProviderActionAdmissionCoordinator({ database: value.database, clock: () => now }),
      adapter: {
        capabilities: () => Promise.resolve({}),
        dispatch: () => Promise.reject(new Error("provider dispatch is not expected")),
        lookup: () => Promise.reject(new Error("provider lookup is not expected")),
      },
      typedGit: value.service,
    });
    await expect(ports.statePort.read(value.intent)).resolves.toMatchObject({
      kind: "git",
      state: { repository: value.intent.repository, grant: { grantId: "grant_01" } },
    });
    ports.effectPort.prepare?.(value.request);
    await expect(ports.effectPort.dispatch(value.request)).resolves.toMatchObject({ status: "committed" });
    expect(value.port.dispatchCount).toBe(1);
    expect(value.database.prepare("SELECT state FROM operator_git_effect_bindings").get()).toEqual({ state: "applied" });
  });

  it("runs conflict status and read-only reconciliation through the public operator action owner with stable replay", async () => {
    const value = fixture();
    value.database.prepare("DELETE FROM operator_effect_custody").run();
    const operatorStore = new OperatorStore({ database: value.database, clock: () => now });
    operatorStore.registerPrincipal({
      operatorId: "operator_01",
      projectId: "project_01",
      authenticatedSubjectHash: "subject_hash_git",
      projectAuthorityGeneration: 1,
    });
    const capability = parseOperatorCapabilityGrant({
      capabilityId: "cap_git_01",
      operatorId: "operator_01",
      projectId: "project_01",
      projectAuthorityGeneration: 1,
      principalGeneration: 1,
      issuedAt: "2026-07-12T09:00:00.000Z",
      expiresAt: "2026-07-12T12:00:00.000Z",
      status: "active",
      kind: "session",
      projectSessionId: "session_01",
      sessionGeneration: 1,
      actions: ["read", "git", "git-custody-resolve"],
    });
    operatorStore.issueCapability(capability, "git-capability-secret");
    const previewCommandId = "preview_command_git_01";
    const previewId = `preview_${sha256(`operator_01:${previewCommandId}`).slice(0, 48)}`;
    const intent = {
      ...value.intent,
      authorisation: {
        ...value.intent.authorisation,
        operationId: derivePreauthorisedGitOperationId({
          operatorId: "operator_01",
          projectId: value.intent.authorisation.projectId,
          projectSessionId: value.intent.authorisation.projectSessionId,
          previewId,
          effectBindingDigest: value.intent.authorisation.effectBindingDigest,
        }),
      },
    } satisfies OperatorGitIntent;
    value.port.outcome = {
      outcome: "exact-conflict",
      repository: value.intent.repository,
      evidenceDigest: sha("1"),
      failureSignatureDigest: null,
      conflict: {
        kind: "merge",
        operationStateDigest: sha("2"),
        indexDigest: sha("3"),
        worktreeDigest: sha("4"),
        conflictPaths: [],
      },
    };
    const ports = createProductionOperatorActionPorts({
      database: value.database,
      clock: () => now,
      providerActionAdmission: new ProviderActionAdmissionCoordinator({ database: value.database, clock: () => now }),
      adapter: {
        capabilities: () => Promise.resolve({}),
        dispatch: () => Promise.reject(new Error("provider dispatch is not expected")),
        lookup: () => Promise.reject(new Error("provider lookup is not expected")),
      },
      typedGit: value.service,
    });
    const actions = new OperatorActionStore({
      database: value.database,
      operatorStore,
      statePort: ports.statePort,
      effectPort: ports.effectPort,
      clock: () => now,
    });
    const context = {
      operatorId: parseIdentifier<"OperatorId">("operator_01", "test.operatorId"),
      projectId: parseIdentifier<"ProjectId">("project_01", "test.projectId"),
      projectAuthorityGeneration: 1,
      principalGeneration: 1,
    };
    const credential = {
      capabilityId: parseIdentifier<"CapabilityId">("cap_git_01", "test.capabilityId"),
      token: "git-capability-secret",
    };
    const preview = await actions.preview(context, {
      command: {
        credential,
        commandId: parseIdentifier<"CommandId">(previewCommandId, "test.previewCommandId"),
        expectedRevision: 2,
        actor: context.operatorId,
        provenance: {
          kind: "console-direct-input",
          clientId: parseIdentifier<"OperatorClientId">("console_git_01", "test.clientId"),
          inputEventId: "input_preview_git_01",
        },
        evidenceRefs: [],
      },
      projectId: context.projectId,
      intent,
    });
    expect(preview.previewId).toBe(previewId);
    const commitCommandId = parseIdentifier<"CommandId">("commit_command_git_01", "test.commitCommandId");
    await actions.commit(context, {
      command: {
        credential,
        commandId: commitCommandId,
        expectedRevision: 2,
        actor: context.operatorId,
        provenance: {
          kind: "console-direct-input",
          clientId: parseIdentifier<"OperatorClientId">("console_git_01", "test.clientId"),
          inputEventId: "input_commit_git_01",
        },
        evidenceRefs: [],
      },
      projectId: context.projectId,
      previewId: preview.previewId,
      expectedPreviewRevision: preview.previewRevision,
      previewDigest: preview.previewDigest,
      expectedIntentDigest: preview.intentDigest,
      confirmation: { kind: "explicit", confirmationId: "confirm_git_01" },
    });
    const conflict = actions.status({ credential, projectId: context.projectId, commandId: commitCommandId });
    if (conflict.status !== "conflict") throw new Error("expected public typed Git conflict status");
    const git = conflict.gitCustody;
    const reconcileRequest = {
      command: {
        credential,
        commandId: parseIdentifier<"CommandId">("reconcile_command_git_01", "test.reconcileCommandId"),
        expectedRevision: 2,
        actor: context.operatorId,
        provenance: {
          kind: "console-direct-input" as const,
          clientId: parseIdentifier<"OperatorClientId">("console_git_01", "test.clientId"),
          inputEventId: "input_reconcile_git_01",
        },
        evidenceRefs: [],
      },
      projectId: context.projectId,
      targetCommandId: commitCommandId,
      expectedStatus: "conflict" as const,
      expectedAttemptGeneration: conflict.attemptGeneration,
      mode: "observe-only" as const,
      gitConflict: {
        kind: "owned-conflict" as const,
        custodyId: git.custodyId,
        expectedBindingState: "conflict" as const,
        expectedBindingStateRevision: git.bindingStateRevision,
        expectedOwnedConflictGeneration: git.ownedConflictGeneration ?? 0,
        expectedPredecessorCustodyId: git.predecessorCustodyId,
        expectedPredecessorConflictGeneration: git.predecessorConflictGeneration,
        expectedReservationGeneration: git.reservationGeneration,
        expectedCommonDirectoryIdentityDigest: git.commonDirectoryIdentityDigest,
        expectedLookupGeneration: git.lookupGeneration,
        expectedLookupEvidenceDigest: git.lookupEvidenceDigest,
        expectedResolutionEligibility: "none" as const,
      },
    };
    const retained = await actions.reconcile(context, reconcileRequest);
    expect(retained).toMatchObject({ status: "conflict", attemptGeneration: 2, gitCustody: { lookupGeneration: 2 } });
    const inspectionCount = value.port.inspectCount;
    await expect(actions.reconcile(context, reconcileRequest)).resolves.toEqual(retained);
    expect(value.port.inspectCount).toBe(inspectionCount);
    await expect(actions.reconcile(context, {
      ...reconcileRequest,
      gitConflict: { ...reconcileRequest.gitConflict, expectedLookupGeneration: git.lookupGeneration + 1 },
    })).rejects.toMatchObject({ code: "DEDUPE_CONFLICT" });
    expect(value.port.dispatchCount).toBe(1);
  });

  it("issues only a positive allow-list subset through git-authorise", () => {
    const value = fixture();
    const inputDigest = sha("f");
    const proposedWithoutDigest = {
      grantId: "grant_02",
            revision: 1,
            projectId: value.intent.authorisation.projectId,
            projectSessionId: value.intent.authorisation.projectSessionId,
            sessionGeneration: 1,
            issuingSessionRevision: 2,
            coordinationRunId: value.intent.authorisation.coordinationRunId,
            issuingRunRevision: 4,
            issuingDependencyRevision: 1,
            authorityRef: value.intent.authorisation.authorityRef,
            authorityRevision: 1,
            gitAllowlistEpoch: 1,
            gitAllowlistDigest: value.intent.authorisation.gitAllowlistDigest as Sha256Digest,
            repositoryRoot: value.intent.repository.repositoryRoot,
            worktreePath: value.intent.repository.worktreePath,
            executionProfileId: value.intent.executionProfile.profileId,
            executionProfileRevision: 1,
            executionProfileDigest: value.intent.executionProfile.digest,
            constraints: {
              operationVariants: ["stage"] as const,
              remoteBindings: [],
              refs: [],
              pathPrefixes: ["src"],
              allowWorktreeCreation: false,
            },
            sourceAuthority: { kind: "operator-command" as const, digest: inputDigest },
      expiresAt: "2026-07-12T11:00:00.000Z" as GitActionGrant["expiresAt"],
    };
    const proposed: GitActionGrant = {
      ...proposedWithoutDigest,
      grantDigest: deriveGitGrantDigest(proposedWithoutDigest),
    };
    const intent = {
      kind: "git-authorise" as const,
      projectId: value.intent.authorisation.projectId,
      projectSessionId: value.intent.authorisation.projectSessionId,
      expectedSessionRevision: 2,
      expectedSessionGeneration: 1,
      coordinationRunId: value.intent.authorisation.coordinationRunId,
      expectedRunRevision: 4,
      expectedDependencyRevision: 1,
      authorityRef: value.intent.authorisation.authorityRef,
      expectedAuthorityRevision: 1,
      expectedGitAllowlistEpoch: 1,
      gitAllowlistDigest: value.intent.authorisation.gitAllowlistDigest as Sha256Digest,
      action: "issue" as const,
      proposedGrant: proposed,
    };
    const request = { ...value.request, operation: "git-authorise", intent, operatorInputRecordDigest: inputDigest };

    value.service.prepareAdministrative(request);
    expect(value.service.administrativeOutcome(intent)).toMatchObject({
      status: "committed",
      afterState: { grantId: "grant_02", state: "active" },
    });
    expect(value.database.prepare("SELECT operation_variant FROM operator_git_grant_variants WHERE grant_id='grant_02'").get())
      .toEqual({ operation_variant: "stage" });

    const widenedWithoutDigest = {
      ...proposedWithoutDigest,
      grantId: "grant_03",
      constraints: { ...proposedWithoutDigest.constraints, pathPrefixes: ["docs"] },
    };
    const widened = { ...widenedWithoutDigest, grantDigest: deriveGitGrantDigest(widenedWithoutDigest) };
    expect(() => value.service.prepareAdministrative({
      ...request,
      commandId: "commit_git_authorise_02",
      intent: { ...intent, proposedGrant: widened },
    })).toThrow(/allow-list/iu);
    expect(value.database.prepare("SELECT COUNT(*) AS count FROM operator_git_grants WHERE grant_id='grant_03'").get())
      .toEqual({ count: 0 });

    const worktreeWithoutDigest = {
      ...proposedWithoutDigest,
      grantId: "grant_04",
      constraints: {
        ...proposedWithoutDigest.constraints,
        operationVariants: ["worktree-create-detached"] as const,
        allowWorktreeCreation: false,
      },
    };
    const worktreeGrant = {
      ...worktreeWithoutDigest,
      grantDigest: deriveGitGrantDigest(worktreeWithoutDigest),
    };
    expect(() => value.service.prepareAdministrative({
      ...request,
      commandId: "commit_git_authorise_03",
      intent: { ...intent, proposedGrant: worktreeGrant },
    })).toThrow(/worktree/iu);
    expect(value.database.prepare("SELECT COUNT(*) AS count FROM operator_git_grants WHERE grant_id='grant_04'").get())
      .toEqual({ count: 0 });
  });

  it("creates one no-authority gate draft and binds only its exact operation ID", async () => {
    const value = fixture();
    const operation = { variant: "branch-delete-force" as const, sourceRef: "refs/heads/feature", sourceObjectDigest: sha("1") };
    const effectBindingDigest = deriveGitEffectBindingDigest({
      projectId: value.intent.authorisation.projectId,
      projectSessionId: value.intent.authorisation.projectSessionId,
      coordinationRunId: value.intent.authorisation.coordinationRunId,
      authorityRef: value.intent.authorisation.authorityRef,
      authorityRevision: 1,
      gitAllowlistEpoch: 1,
      gitAllowlistDigest: value.intent.authorisation.gitAllowlistDigest,
      repository: value.intent.repository,
      executionProfile: value.intent.executionProfile,
      remoteBinding: null,
      operation,
      resultRecipeDigest: value.intent.resultRecipe.resultRecipeDigest,
    });
    const intent = {
      kind: "git-operation-draft" as const,
      action: "create" as const,
      draftRequestId: "draft_request_01",
      expiresAt: "2026-07-12T11:00:00.000Z" as GitActionGrant["expiresAt"],
      binding: {
        kind: "mutation" as const,
        authorisation: {
          ...value.intent.authorisation,
          operationVariant: operation.variant,
          effectBindingDigest,
          operationId: undefined,
          decision: undefined,
        },
        repository: value.intent.repository,
        executionProfile: value.intent.executionProfile,
        operation,
        resultRecipe: value.intent.resultRecipe,
      },
    };
    delete (intent.binding.authorisation as Record<string, unknown>).operationId;
    delete (intent.binding.authorisation as Record<string, unknown>).decision;
    const request = { ...value.request, operation: "git", intent, operatorInputRecordDigest: sha("2") };

    value.service.prepareAdministrative(request);
    value.service.prepareAdministrative(request);
    const draft = value.database.prepare("SELECT draft_id,revision,operation_id,draft_digest,state FROM git_operation_drafts").get() as {
      draft_id: string; revision: number; operation_id: string; draft_digest: Sha256Digest; state: string;
    };
    expect(draft).toMatchObject({ revision: 1, state: "open" });
    expect(value.database.prepare("SELECT operation_kind,state FROM operation_admissions WHERE operation_id=?").get(draft.operation_id))
      .toEqual({ operation_kind: "branch-delete-force", state: "prepared" });
    expect(value.database.prepare("SELECT COUNT(*) AS count FROM operator_git_effect_bindings").get()).toEqual({ count: 0 });
    value.database.exec(`
      INSERT INTO scoped_gates(
        gate_id,project_session_id,coordination_run_id,dedupe_key,scope_kind,scope_task_id,dependency_revision,
        blocked_operation_ids_json,enforcement_points_json,question,reason,options_json,recommendation,
        consequences_json,evidence_refs_json,created_by_ref,expected_approver_ref,status,human_required,
        revision,created_at,updated_at
      ) VALUES('gate_git_01','session_01','run_01','git:draft','run',NULL,1,
        '["${draft.operation_id}"]','["operation"]','Approve?','Exact Git effect','["approve"]','approve',
        '[]','[]','operator_01','operator_01','pending',1,1,1,1);
      INSERT INTO scoped_gate_operations VALUES('gate_git_01','${draft.operation_id}');
    `);
    seedGateAttention(value.database, "gate_git_01", "Approve?", "Exact Git effect");
    expect(value.database.prepare("SELECT revision,state FROM git_operation_drafts WHERE draft_id=?").get(draft.draft_id))
      .toEqual({ revision: 2, state: "gate-bound" });
    value.database.prepare(`
      UPDATE scoped_gates
         SET status='approved',resolved_by_operator_id='operator_01',resolution_json='{"decision":"approve"}',
             revision=2,updated_at=2
       WHERE gate_id='gate_git_01'
    `).run();
    const finalIntent = {
      kind: "git" as const,
      authorisation: {
        ...intent.binding.authorisation,
        operationId: draft.operation_id,
        decision: {
          kind: "gate" as const,
          draftId: draft.draft_id,
          expectedDraftRevision: 2,
          draftDigest: draft.draft_digest,
          gateId: "gate_git_01",
          expectedGateRevision: 2,
          expectedGateStatus: "approved" as const,
          blockedOperationId: draft.operation_id,
        },
      },
      repository: value.intent.repository,
      executionProfile: value.intent.executionProfile,
      operation,
      resultRecipe: value.intent.resultRecipe,
    };
    const finalRequest = {
      ...value.request,
      commandId: "commit_gate_git_01",
      previewId: "preview_gate_git_01",
      intent: finalIntent,
    };
    value.database.prepare(`
      INSERT INTO operator_effect_custody(
        custody_id,operator_id,project_id,project_session_id,principal_generation,command_id,operation,
        intent_digest,before_state_digest,intent_json,state,created_at,updated_at
      ) VALUES(?,?,?,?,?,?,?,?,?,?,'prepared',?,?)
    `).run(
      value.service.custodyId(finalRequest), finalRequest.operatorId, finalRequest.projectId,
      finalRequest.projectSessionId, finalRequest.principalGeneration, finalRequest.commandId,
      finalRequest.operation, finalRequest.intentDigest, finalRequest.beforeStateDigest,
      canonicalJson(finalRequest.intent), now, now,
    );
    value.service.prepare(finalRequest);
    expect(value.database.prepare("SELECT state FROM git_operation_drafts WHERE draft_id=?").get(draft.draft_id))
      .toEqual({ state: "consumed" });
    expect(value.database.prepare("SELECT state FROM operation_admissions WHERE operation_id=?").get(draft.operation_id))
      .toEqual({ state: "authorised" });
    value.port.onObserve = () => {
      value.database.prepare(`
        UPDATE scoped_gates SET status='superseded',revision=revision+1,updated_at=3
         WHERE gate_id='gate_git_01'
      `).run();
    };
    await expect(value.service.dispatch(finalRequest)).rejects.toMatchObject({ code: "GATE_BLOCKED" });
    expect(value.port.dispatchCount).toBe(0);
  });

  it("human-adjudicates only eligible gate-bound custody with zero further Git calls", async () => {
    const value = fixture();
    value.service.prepare(value.request);
    value.port.outcome = {
      outcome: "exact-conflict",
      repository: value.intent.repository,
      evidenceDigest: sha("1"),
      failureSignatureDigest: null,
      conflict: {
        kind: "merge",
        operationStateDigest: sha("2"),
        indexDigest: sha("3"),
        worktreeDigest: sha("4"),
        conflictPaths: [],
      },
    };
    await value.service.dispatch(value.request);
    const conflict = value.service.status(value.request.commandId, value.request.intentDigest);
    if (conflict?.status !== "conflict") throw new Error("expected conflict before custody resolution");
    value.port.outcome = {
      outcome: "exact-no-effect",
      repository: value.intent.repository,
      evidenceDigest: sha("5"),
      failureSignatureDigest: null,
      conflict: null,
    };
    const quarantined = await value.service.reconcileConflict({
      reconciliationCommandId: "reconcile_for_resolution_01",
      targetCommandId: value.request.commandId,
      intentDigest: value.request.intentDigest,
      nextAttemptGeneration: 2,
      binding: {
        kind: "owned-conflict",
        custodyId: conflict.gitCustody.custodyId,
        expectedBindingState: "conflict",
        expectedBindingStateRevision: conflict.gitCustody.bindingStateRevision,
        expectedOwnedConflictGeneration: conflict.gitCustody.ownedConflictGeneration ?? 0,
        expectedPredecessorCustodyId: null,
        expectedPredecessorConflictGeneration: null,
        expectedReservationGeneration: conflict.gitCustody.reservationGeneration,
        expectedCommonDirectoryIdentityDigest: conflict.gitCustody.commonDirectoryIdentityDigest,
        expectedLookupGeneration: conflict.gitCustody.lookupGeneration,
        expectedLookupEvidenceDigest: conflict.gitCustody.lookupEvidenceDigest,
        expectedResolutionEligibility: "none",
      },
    });
    if (quarantined.status !== "quarantined" || quarantined.gitCustody.lookupEvidenceDigest === null) {
      throw new Error("expected eligible quarantined custody");
    }
    const binding = {
      kind: "custody-resolution" as const,
      projectId: value.intent.authorisation.projectId,
      projectSessionId: value.intent.authorisation.projectSessionId,
      expectedSessionRevision: 2,
      expectedSessionGeneration: 1,
      coordinationRunId: value.intent.authorisation.coordinationRunId,
      expectedRunRevision: 4,
      expectedDependencyRevision: 1,
      authorityRef: value.intent.authorisation.authorityRef,
      expectedAuthorityRevision: 1,
      custodyId: quarantined.gitCustody.custodyId,
      expectedCustodyState: "quarantined" as const,
      expectedLookupGeneration: quarantined.gitCustody.lookupGeneration,
      lookupEvidenceDigest: quarantined.gitCustody.lookupEvidenceDigest,
      resolutionEligibilityReason: "conflict-state-unverifiable" as const,
      adjudication: "no-effect" as const,
      reason: "The bounded native conflict state was destroyed outside the Fabric.",
    };
    const draftIntent = {
      kind: "git-operation-draft" as const,
      action: "create" as const,
      draftRequestId: "resolution_draft_request_01",
      expiresAt: "2026-07-12T11:00:00.000Z" as GitActionGrant["expiresAt"],
      binding,
    };
    const draftRequest = {
      ...value.request,
      commandId: "commit_resolution_draft_01",
      operation: "git-custody-resolve",
      intent: draftIntent,
      operatorInputRecordDigest: sha("6"),
    };
    value.service.prepareAdministrative(draftRequest);
    const draft = value.database.prepare(`
      SELECT draft_id,revision,operation_id,draft_digest FROM git_operation_drafts
       WHERE draft_kind='custody-resolution'
    `).get() as { draft_id: string; revision: number; operation_id: string; draft_digest: Sha256Digest };
    value.database.exec(`
      INSERT INTO scoped_gates(
        gate_id,project_session_id,coordination_run_id,dedupe_key,scope_kind,scope_task_id,dependency_revision,
        blocked_operation_ids_json,enforcement_points_json,question,reason,options_json,recommendation,
        consequences_json,evidence_refs_json,created_by_ref,expected_approver_ref,status,human_required,
        revision,created_at,updated_at
      ) VALUES('gate_resolution_01','session_01','run_01','git:resolution','run',NULL,1,
        '["${draft.operation_id}"]','["operation"]','Adjudicate?','Permanent proof loss','["approve"]','approve',
        '[]','[]','operator_01','operator_01','pending',1,1,1,1);
      INSERT INTO scoped_gate_operations VALUES('gate_resolution_01','${draft.operation_id}');
    `);
    seedGateAttention(value.database, "gate_resolution_01", "Adjudicate?", "Permanent proof loss");
    value.database.exec(`
      UPDATE scoped_gates
         SET status='approved',resolved_by_operator_id='operator_01',resolution_json='{"decision":"approve"}',
             revision=2,updated_at=2
       WHERE gate_id='gate_resolution_01';
    `);
    const { kind: _bindingKind, ...resolutionFields } = binding;
    const finalIntent = {
      kind: "git-custody-resolve" as const,
      ...resolutionFields,
      draftId: draft.draft_id,
      expectedDraftRevision: 2,
      draftDigest: draft.draft_digest,
      operationId: draft.operation_id,
      gateId: "gate_resolution_01",
      expectedGateRevision: 2,
      expectedGateStatus: "approved" as const,
    };
    const finalRequest = {
      ...value.request,
      commandId: "commit_resolution_01",
      operation: "git-custody-resolve",
      intent: finalIntent,
      operatorInputRecordDigest: sha("7"),
    };
    value.service.prepareAdministrative(finalRequest);

    expect(value.service.administrativeOutcome(finalIntent)).toMatchObject({
      status: "committed",
      afterState: { kind: "human-adjudicated-no-effect" },
    });
    expect(value.database.prepare(`
      SELECT b.state AS binding_state,c.state AS custody_state,a.state AS admission_state,r.state AS reservation_state
        FROM operator_git_effect_bindings b
        JOIN operator_effect_custody c ON c.custody_id=b.custody_id
        JOIN operation_admissions a ON a.operation_id=b.operation_id
        JOIN git_mutation_reservations r ON r.custody_id=b.custody_id
    `).get()).toEqual({
      binding_state: "human-resolved",
      custody_state: "terminal",
      admission_state: "terminal",
      reservation_state: "released",
    });
    expect(value.database.prepare("SELECT state FROM operation_admissions WHERE operation_id=?").get(draft.operation_id))
      .toEqual({ state: "terminal" });
    expect(value.port.dispatchCount).toBe(1);
    expect(value.port.inspectCount).toBe(1);
  });
});
