import Database from "better-sqlite3";
import { createHash } from "node:crypto";
import { mkdtempSync, readFileSync, realpathSync, rmSync, symlinkSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { fileURLToPath } from "node:url";

import { afterEach, describe, expect, it } from "vitest";
import {
  parseArtifactRef,
  parseProjectSessionLaunchIntent,
  parseSha256Digest,
  type ChairBridgeRecoveryIntent,
  type ChairLiveHandoffIntent,
  type OperatorId,
  type OperatorActionCommitRequest,
  type OperatorActionPreviewRequest,
  type ProjectId,
  type ProjectSessionTransitionRequest,
  type Sha256Digest,
} from "@local/agent-fabric-protocol";

import { applyMigrations } from "../../../src/core/migrations.ts";
import { ProviderActionAdmissionCoordinator } from "../../../src/application/provider-action-admission.ts";
import { openFabric } from "../../../src/index.ts";
import { OperatorActionStore } from "../../../src/operator/action-store.ts";
import { OperatorStore } from "../../../src/operator/store.ts";
import {
  chairLaunchAttestationDigest,
  chairLaunchChallengeDigest,
} from "../../../src/adapters/providers/types.ts";
import {
  LaunchCustodyService,
  computeLaunchResourceStateDigest,
  normaliseLaunchChairAuthority,
  parseLaunchAdapterContract,
  type LaunchCustodyIntent,
} from "../../../src/project-session/launch-custody.ts";
import {
  ProviderAgentCustodyAdapter,
  type ProviderAgentCustodyAdapterOptions,
} from "../../../src/project-session/provider-agent-custody.ts";
import { ProviderAgentCustodyRecoveryAdapter } from "../../../src/project-session/provider-agent-custody-recovery.ts";
import {
  ChairLiveHandoffCustodyAdapter,
  type ChairLiveHandoffCustodyAdapterOptions,
} from "../../../src/project-session/chair-live-handoff-custody.ts";
import { ChairLiveHandoffCustodyRecoveryAdapter } from "../../../src/project-session/chair-live-handoff-custody-recovery.ts";
import {
  ChairRecoveryCustodyService,
  type ChairRecoveryCustodyServiceOptions,
} from "../../../src/project-session/chair-recovery-custody.ts";
import { LaunchService, type LaunchServiceOptions } from "../../../src/project-session/launch-service.ts";
import { LaunchSettlement, type LaunchSettlementOptions } from "../../../src/project-session/launch-settlement.ts";
import { reconcileUnknownLaunchUsage as reconcileUnknownLaunchUsageOwner } from "../../../src/project-session/launch-usage-reconciliation.ts";
import { recoverLaunchCustodyFamilies } from "../../../src/project-session/custody-startup.ts";
import { ProjectSessionStore } from "../../../src/project-session/store.ts";
import { ProjectFabricCoreError } from "../../../src/project-session/contracts.ts";
import { canonicalJson, sha256 } from "../../../src/persistence/row-codec.ts";
import { admitProviderActionFixture } from "../../support/provider-action-fixture.ts";
import { TEST_AUTHORITY_V2_FIELDS } from "../../support/authority-v2-testkit.ts";

const databases: Database.Database[] = [];
const directories: string[] = [];
const now = Date.parse("2027-01-01T00:00:00Z");
const digest = (value: string): Sha256Digest => parseSha256Digest(`sha256:${sha256(value)}`, "test.digest");
const attestationChallenge = "ab".repeat(32);
const attestationChallengeDigest = parseSha256Digest(
  `sha256:${createHash("sha256").update(Buffer.from(attestationChallenge, "hex")).digest("hex")}`,
  "test.attestationChallengeDigest",
);
const trustDigest = digest("trusted-project-record");
const contract = {
  schemaVersion: 1,
  method: "launch_chair",
  oneUse: true,
  secretTransport: "private-environment",
  environment: {
    capability: "AGENT_FABRIC_CAPABILITY",
    socketPath: "AGENT_FABRIC_SOCKET_PATH",
    attestationChallenge: "AGENT_FABRIC_ATTESTATION_CHALLENGE",
  },
  inputSchemaId: "chair-launch-input.v1",
  publicPayloadSchema: {
    type: "object",
    additionalProperties: false,
    required: ["model"],
    properties: { model: { type: "string", minLength: 1 } },
  },
  noEffectProofSchemas: {
    "provider-no-effect.v1": {
      type: "object",
      additionalProperties: false,
      required: ["effectCount"],
      properties: { effectCount: { const: 0 } },
    },
  },
  attestation: {
    method: "provider-session-random-challenge-v1",
    bridgeContract: "agent-fabric-session-bridge-v1",
    origin: "provider-session-tool-call",
    oneUse: true,
    bridgeLifetime: "provider-session",
    digestAlgorithm: "sha256",
    nativeAttribution: "claude-sdk-assistant-request-tool-use-v1",
  },
} as const;
const contractDigest = digest(canonicalJson(contract));
const recoveryAdapter = fileURLToPath(new URL("../../support/launch-custody-recovery-adapter.ts", import.meta.url));

function databaseContains(database: Database.Database, needle: string): boolean {
  const tables = database.prepare(`
    SELECT name FROM sqlite_master
     WHERE type='table' AND name NOT LIKE 'sqlite_%'
     ORDER BY name
  `).all() as Array<{ name: string }>;
  for (const { name } of tables) {
    const quotedTable = `"${name.replaceAll('"', '""')}"`;
    const columns = database.prepare(`PRAGMA table_info(${quotedTable})`).all() as Array<{ name: string }>;
    for (const column of columns) {
      const quotedColumn = `"${column.name.replaceAll('"', '""')}"`;
      if (database.prepare(`SELECT 1 FROM ${quotedTable} WHERE instr(CAST(${quotedColumn} AS TEXT), ?) > 0 LIMIT 1`).get(needle) !== undefined) {
        return true;
      }
    }
  }
  return false;
}

/**
 * Test-only mirror of `fabric.ts`'s S4e2 construction block (issue #354, plan §1 "What dissolve
 * means, minimally"): builds each of the four project-session custody families with only its own
 * narrow options, then hands the already-built instances to the `LaunchCustodyService` facade.
 * Accepts the same flat, legacy-shaped option bag the tests already assemble so call sites below
 * are unchanged; there is no production equivalent of this flat bag anymore (the mixed
 * `LaunchCustodyServiceOptions` type was deleted).
 */
function buildLaunchCustodyService(options: Readonly<{
  database: Database.Database;
  providerActionAdmission: ProviderAgentCustodyAdapterOptions["providerActionAdmission"];
  clock: () => number;
  fault?: (label: string) => void;
  randomCapability: () => string;
  randomAttestationChallenge?: () => string;
  fabricSocketPath: string;
  adapterContracts: LaunchServiceOptions["adapterContracts"];
  adapterEffects: ChairRecoveryCustodyServiceOptions["adapterEffects"] & LaunchServiceOptions["adapterEffects"] & LaunchSettlementOptions["adapterEffects"];
  agentEffects?: ProviderAgentCustodyAdapterOptions["agentEffects"];
  retireVolatileChairBridge?: ChairLiveHandoffCustodyAdapterOptions["retireVolatileChairBridge"];
  retireVolatileProjectSession?: ChairRecoveryCustodyServiceOptions["retireVolatileProjectSession"];
}>): LaunchCustodyService {
  const database = options.database;
  const clock = options.clock;
  const fault = options.fault ?? (() => undefined);
  const fabricSocketPath = options.fabricSocketPath;
  const adapterContracts = options.adapterContracts;
  const adapterEffects = options.adapterEffects;
  const daemonInstanceGeneration = () => 1;
  const providerAgentCustody = new ProviderAgentCustodyAdapter({
    database,
    providerActionAdmission: options.providerActionAdmission,
    clock,
    fault,
    randomCapability: options.randomCapability,
    fabricSocketPath,
    agentEffects: options.agentEffects,
    daemonInstanceGeneration,
  });
  const providerAgentCustodyRecovery = new ProviderAgentCustodyRecoveryAdapter({
    database,
    agentEffects: options.agentEffects,
    custody: providerAgentCustody.recoveryPort(),
  });
  const chairLiveHandoffCustody = new ChairLiveHandoffCustodyAdapter({
    database,
    providerActionAdmission: options.providerActionAdmission,
    clock,
    fault,
    adapterContracts,
    adapterEffects,
    ...(options.retireVolatileChairBridge === undefined ? {} : { retireVolatileChairBridge: options.retireVolatileChairBridge }),
  });
  const chairLiveHandoffCustodyRecovery = new ChairLiveHandoffCustodyRecoveryAdapter({
    database,
    custody: chairLiveHandoffCustody.recoveryPort(),
  });
  const chairRecoveryCustody = new ChairRecoveryCustodyService({
    database,
    providerActionAdmission: options.providerActionAdmission,
    clock,
    fault,
    randomCapability: options.randomCapability,
    randomAttestationChallenge: options.randomAttestationChallenge ?? (() => "unused-recovery-attestation-challenge"),
    fabricSocketPath,
    adapterContracts,
    adapterEffects,
    daemonInstanceGeneration,
    ...(options.retireVolatileProjectSession === undefined ? {} : { retireVolatileProjectSession: options.retireVolatileProjectSession }),
    reconcileUnknownLaunchUsage: (input) => reconcileUnknownLaunchUsageOwner(database, clock, input),
  });
  const launchSettlement = new LaunchSettlement({
    database,
    clock,
    adapterContracts,
    adapterEffects,
    chairLoss: {
      observeChairBridgeLoss: (input) => chairRecoveryCustody.observeChairBridgeLoss(input),
    },
  });
  const launchService = new LaunchService({
    database,
    providerActionAdmission: options.providerActionAdmission,
    clock,
    fault,
    randomCapability: options.randomCapability,
    randomAttestationChallenge: options.randomAttestationChallenge ?? (() => "unused-launch-attestation-challenge"),
    fabricSocketPath,
    adapterContracts,
    adapterEffects,
    settlement: launchSettlement,
  });
  return new LaunchCustodyService({
    database,
    clock,
    providerAgentCustody,
    providerAgentCustodyRecovery,
    chairLiveHandoffCustody,
    chairLiveHandoffCustodyRecovery,
    chairRecoveryCustody,
    launchService,
    launchSettlement,
  });
}

function createFixture(options: Readonly<{
  dispatch?: (handle: Parameters<LaunchCustodyService["dispatchPrepared"]>[0]) => Promise<unknown>;
  lookup?: (input: Parameters<LaunchCustodyService["lookup"]>[0]) => Promise<unknown>;
  fault?: (label: string) => void;
  persistent?: boolean;
  retainedChairBridge?: boolean;
  recoverChair?: (handle: Parameters<LaunchCustodyService["dispatchPreparedChairRecovery"]>[0]) => Promise<unknown>;
  lookupChairRecovery?: (input: { adapterId: string; actionId: string }) => Promise<unknown>;
  promoteSuccessor?: (input: Record<string, unknown>) => Promise<boolean>;
  lookupSuccessor?: (input: Record<string, unknown>) => Promise<"child" | "chair" | "missing">;
  retireChair?: (entry: { agentId: string; providerSessionRef: string }) => void;
  launchResources?: Readonly<Record<string, number>>;
}> = {}): {
  database: Database.Database;
  databasePath: string;
  root: string;
  service: LaunchCustodyService;
  intent: LaunchCustodyIntent;
  providerActionAdmission: ProviderActionAdmissionCoordinator;
} {
  const root = realpathSync(mkdtempSync(join(tmpdir(), "afab-launch-custody-")));
  directories.push(root);
  const databasePath = options.persistent === true ? join(root, "fabric.sqlite3") : ":memory:";
  const database = new Database(databasePath);
  databases.push(database);
  applyMigrations(database);

  const launchResources = options.launchResources ?? { provider_calls: 1 };
  const resourceLimits = Object.fromEntries(
    Object.entries(launchResources).map(([unit, amount]) => [unit, Math.max(10, amount)]),
  );
  const resourcePlan = {
    schemaVersion: 1,
    projectId: "project_01",
    projectSessionId: "session_launch_01",
    runId: "run_launch_01",
    budgetRef: "budget_launch_01",
    scopes: {
      project: { scopeId: "scope_project_01", limits: resourceLimits },
      projectSession: { scopeId: "scope_session_launch_01", limits: resourceLimits },
      coordinationRun: { scopeId: "scope_run_launch_01", limits: resourceLimits },
    },
    launchReservation: { amounts: launchResources },
  };
  const resourcePlanText = canonicalJson(resourcePlan);
  writeFileSync(join(root, "launch-resources.json"), resourcePlanText, { mode: 0o600 });
  const resourcePlanRef = parseArtifactRef({ path: "launch-resources.json", digest: digest(resourcePlanText) }, "test.resourcePlanRef");

  const chairAuthority = {
    ...TEST_AUTHORITY_V2_FIELDS,
    workspaceRoots: ["."],
    sourcePaths: ["."],
    artifactPaths: [".agent-run/AFAB-LAUNCH"],
    actions: ["fabric.v1.run-status.read"],
    deniedPaths: [],
    deniedActions: [],
    disclosure: { level: "forbidden" },
    expiresAt: "2027-01-02T00:00:00.000Z",
    budget: resourceLimits,
  };
  const normalisedAuthority = normaliseLaunchChairAuthority(chairAuthority, root);
  const authorityRef = digest(canonicalJson(normalisedAuthority));
  const launchPacket = {
    schemaVersion: 1,
    projectId: "project_01",
    projectSessionId: "session_launch_01",
    runId: "run_launch_01",
    chairAgentId: "chair_launch_01",
    projectRunDirectory: ".agent-run/AFAB-LAUNCH",
    topologyMode: "coordinated",
    budgetRef: "budget_launch_01",
    resourcePlanRef,
    chairAuthority,
    provider: {
      adapterId: "claude-agent-sdk",
      actionId: "provider_launch_01",
      contractDigest,
      inputSchemaId: contract.inputSchemaId,
      input: { model: "claude-opus-4-1" },
    },
  };
  const packetText = canonicalJson(launchPacket);
  writeFileSync(join(root, "launch-packet.json"), packetText, { mode: 0o600 });
  const launchPacketRef = parseArtifactRef({ path: "launch-packet.json", digest: digest(packetText) }, "test.launchPacketRef");

  database.prepare(`
    INSERT INTO projects(
      project_id, canonical_root, trust_record_digest, revision,
      authority_generation, created_at, updated_at
    ) VALUES ('project_01', ?, ?, 1, 1, ?, ?)
  `).run(root, trustDigest, now - 1_000, now - 1_000);
  database.prepare(`
    INSERT INTO project_sessions(
      project_session_id, project_id, mode, state, revision, generation,
      authority_ref, budget_ref, launch_packet_path, launch_packet_digest,
      membership_revision, origin_kind, origin_operator_id, created_at, updated_at
    ) VALUES (
      'session_launch_01', 'project_01', 'coordinated', 'awaiting_launch', 2, 1,
      ?, 'budget_launch_01', ?, ?, 1, 'operator-launch', 'operator_01', ?, ?
    )
  `).run(authorityRef, launchPacketRef.path, launchPacketRef.digest, now - 900, now - 900);
  database.exec(`
    INSERT INTO operator_principals(
      operator_id, project_id, project_session_id, authenticated_subject_hash,
      project_authority_generation, principal_generation, state, created_at, updated_at
    ) VALUES (
      'operator_01', 'project_01', NULL, 'subject-hash', 1, 1, 'active', ${now - 800}, ${now - 800}
    );
    INSERT INTO operator_capabilities(
      capability_id, token_hash, operator_id, project_id, project_session_id,
      project_authority_generation, session_generation, principal_generation,
      kind, operations_json, issued_at, expires_at
    ) VALUES (
      'operator_cap_launch_01', '${sha256("operator-secret")}', 'operator_01', 'project_01',
      'session_launch_01', 1, 1, 1, 'session', '["launch","read"]',
      ${now - 700}, ${now + 60_000}
    );
  `);

  const clock = () => now;
  const providerActionAdmission = new ProviderActionAdmissionCoordinator({
    database,
    clock,
    ...(options.fault === undefined ? {} : { fault: options.fault }),
  });
  let capabilityGeneration = 0;
  const service = buildLaunchCustodyService({
    database,
    providerActionAdmission,
    clock,
    ...(options.fault === undefined ? {} : { fault: options.fault }),
    randomCapability: () => `chair-secret-canary-${String(++capabilityGeneration).padStart(2, "0")}`,
    randomAttestationChallenge: () => attestationChallenge,
    fabricSocketPath: "/private/agent-fabric.sock",
    adapterContracts: {
      inspect: async (adapterId) => {
        if (adapterId !== "claude-agent-sdk") throw new Error("unknown adapter");
        return contract;
      },
    },
    adapterEffects: {
      dispatch: options.dispatch ?? (async () => { throw new Error("not expected during preparation"); }),
      lookup: options.lookup ?? (async () => { throw new Error("not expected during preparation"); }),
      hasRetainedChairBridge: () => options.retainedChairBridge ?? true,
      ...(options.recoverChair === undefined ? {} : { recoverChair: options.recoverChair }),
      ...(options.lookupChairRecovery === undefined
        ? {}
        : { lookupChairRecovery: options.lookupChairRecovery }),
      ...(options.lookupSuccessor === undefined
        ? {}
        : { lookupRetainedSuccessorBridge: options.lookupSuccessor }),
      ...(options.promoteSuccessor === undefined
        ? {}
        : { promoteRetainedSuccessorBridge: options.promoteSuccessor }),
    },
    ...(options.retireChair === undefined ? {} : { retireVolatileChairBridge: options.retireChair }),
  });
  const intent: LaunchCustodyIntent = parseProjectSessionLaunchIntent({
    kind: "project-session-launch",
    projectId: "project_01",
    projectSessionId: "session_launch_01",
    expectedProjectRevision: 1,
    expectedSessionRevision: 2,
    expectedSessionGeneration: 1,
    trustRecordDigest: trustDigest,
    launchPacketRef,
    authorityRef,
    budgetRef: "budget_launch_01",
    resourcePlanRef,
    providerAdapterId: "claude-agent-sdk",
    providerActionId: "provider_launch_01",
    providerContractDigest: contractDigest,
    resourceStateDigest: computeLaunchResourceStateDigest(database, "project_01", "session_launch_01"),
  });
  return { database, databasePath, root, service, intent, providerActionAdmission };
}

function restartedLiveHandoffService(
  fixture: ReturnType<typeof createFixture>,
  options: Readonly<{
    promote?: (input: Record<string, unknown>) => Promise<boolean>;
    lookup?: (input: Record<string, unknown>) => Promise<"child" | "chair" | "missing">;
    retireChair?: (entry: { agentId: string; providerSessionRef: string }) => void;
  }> = {},
): LaunchCustodyService {
  return buildLaunchCustodyService({
    database: fixture.database,
    providerActionAdmission: fixture.providerActionAdmission,
    clock: () => now,
    randomCapability: () => "unused-live-handoff-restart-secret",
    fabricSocketPath: "/private/agent-fabric.sock",
    adapterContracts: { inspect: async () => contract },
    adapterEffects: {
      dispatch: async () => { throw new Error("live handoff restart must not dispatch launch"); },
      lookup: async () => { throw new Error("live handoff restart must not lookup launch"); },
      ...(options.promote === undefined ? {} : { promoteRetainedSuccessorBridge: options.promote }),
      ...(options.lookup === undefined ? {} : { lookupRetainedSuccessorBridge: options.lookup }),
    },
    agentEffects: {
      dispatch: async () => { throw new Error("live handoff restart must not dispatch an agent"); },
      attachWithoutBridge: async () => { throw new Error("live handoff restart must not attach an agent"); },
      lookup: async () => { throw new Error("live handoff restart must not lookup an agent action"); },
      hasRetainedBridge: () => true,
    },
    ...(options.retireChair === undefined ? {} : { retireVolatileChairBridge: options.retireChair }),
  });
}

function closeTrackedDatabase(database: Database.Database): void {
  const index = databases.indexOf(database);
  if (index >= 0) databases.splice(index, 1);
  database.close();
}

function probeRecoveryCapabilityIdentityGuards(database: Database.Database): {
  identityUpdateBlocked: boolean;
  deleteBlocked: boolean;
  mutableMetadataAllowed: boolean;
  canonicalRevocationAllowed: boolean;
} {
  const recovery = database.prepare(`
    SELECT new_capability_hash FROM chair_bridge_recovery_custody WHERE path='rebind'
  `).get() as { new_capability_hash: string };
  let identityUpdateBlocked = false;
  database.exec("SAVEPOINT probe_recovery_identity_update");
  try {
    database.prepare(`
      UPDATE capabilities SET principal_generation=principal_generation+1 WHERE token_hash=?
    `).run(recovery.new_capability_hash);
  } catch (error: unknown) {
    identityUpdateBlocked = error instanceof Error && /INVARIANT_chair_bridge_loss_freezes_grants/u.test(error.message);
  } finally {
    database.exec("ROLLBACK TO probe_recovery_identity_update");
    database.exec("RELEASE probe_recovery_identity_update");
  }
  let deleteBlocked = false;
  database.exec("SAVEPOINT probe_recovery_identity_delete");
  try {
    database.prepare(`DELETE FROM capabilities WHERE token_hash=?`).run(recovery.new_capability_hash);
  } catch (error: unknown) {
    deleteBlocked = error instanceof Error && /INVARIANT_chair_bridge_loss_freezes_grants/u.test(error.message);
  } finally {
    database.exec("ROLLBACK TO probe_recovery_identity_delete");
    database.exec("RELEASE probe_recovery_identity_delete");
  }
  let mutableMetadataAllowed = true;
  database.exec("SAVEPOINT probe_recovery_mutable_metadata");
  try {
    database.prepare(`
      UPDATE capabilities SET expires_at=expires_at+1, revoked_at=COALESCE(revoked_at, ?)
       WHERE token_hash=?
    `).run(now, recovery.new_capability_hash);
  } catch {
    mutableMetadataAllowed = false;
  } finally {
    database.exec("ROLLBACK TO probe_recovery_mutable_metadata");
    database.exec("RELEASE probe_recovery_mutable_metadata");
  }
  let canonicalRevocationAllowed = true;
  database.exec("SAVEPOINT probe_recovery_canonical_revocation");
  try {
    const result = database.prepare(`
      UPDATE capabilities SET revoked_at=?, principal_generation=principal_generation+1
       WHERE token_hash=? AND revoked_at IS NULL
    `).run(now, recovery.new_capability_hash);
    canonicalRevocationAllowed = result.changes === 1;
  } catch {
    canonicalRevocationAllowed = false;
  } finally {
    database.exec("ROLLBACK TO probe_recovery_canonical_revocation");
    database.exec("RELEASE probe_recovery_canonical_revocation");
  }
  return { identityUpdateBlocked, deleteBlocked, mutableMetadataAllowed, canonicalRevocationAllowed };
}

async function prepareFixture(fixture: ReturnType<typeof createFixture>): Promise<{
  inspection: Awaited<ReturnType<LaunchCustodyService["inspect"]>>;
  handle: ReturnType<LaunchCustodyService["prepareInTransaction"]>;
}> {
  const inspection = await fixture.service.inspect(fixture.intent);
  let handle: ReturnType<LaunchCustodyService["prepareInTransaction"]> | undefined;
  fixture.database.transaction(() => {
    fixture.database.prepare(`
      INSERT INTO operator_commands(
        operator_id, command_id, capability_id, project_id, project_session_id,
        operation, expected_revision, payload_hash, provenance_json, before_json,
        after_json, evidence_json, result_json, status, created_at
      ) VALUES (
        'operator_01', 'commit_launch_01', 'operator_cap_launch_01', 'project_01',
        'session_launch_01', 'launch', 2, 'payload-hash', '{}', '{}', '{}', '[]', '{}',
        'committed', ${now}
      )
    `).run();
    handle = fixture.service.prepareInTransaction({
      inspection,
      operatorId: "operator_01",
      operatorCommandId: "commit_launch_01",
      principal: operatorPrincipal(),
    });
  }).immediate();
  if (handle === undefined) throw new Error("launch handle was not prepared");
  return { inspection, handle };
}

function operatorPrincipal() {
  return {
    operatorId: "operator_01" as never,
    projectId: "project_01" as never,
    projectAuthorityGeneration: 1,
    principalGeneration: 1,
  };
}

function recoveryTicket(
  fixture: ReturnType<typeof createFixture>,
  inspection: Awaited<ReturnType<LaunchCustodyService["inspectChairRecovery"]>>,
) {
  return fixture.service.preflightChairRecovery({ inspection, principal: operatorPrincipal() });
}

function handoffTicket(
  fixture: ReturnType<typeof createFixture>,
  inspection: Awaited<ReturnType<LaunchCustodyService["inspectChairLiveHandoff"]>>,
  operatorCommandId: string,
) {
  return fixture.service.preflightChairLiveHandoff({
    inspection,
    operatorCommandId,
    principal: operatorPrincipal(),
  });
}

function seedLiveHandoffSuccessor(fixture: ReturnType<typeof createFixture>): {
  intent: ChairLiveHandoffIntent;
  successorCapabilityHash: string;
} {
  const chair = fixture.database.prepare(`
    SELECT agent.authority_id, authority.authority_json
      FROM agents agent JOIN authorities authority USING(authority_id)
     WHERE agent.run_id='run_launch_01' AND agent.agent_id='chair_launch_01'
  `).get() as { authority_id: string; authority_json: string };
  const successorAuthorityId = "authority_successor_live_01";
  const successorAuthority = {
    ...(JSON.parse(chair.authority_json) as Record<string, unknown>),
    sourcePaths: [fixture.root],
    artifactPaths: [join(fixture.root, ".agent-run/AFAB-LAUNCH/successor")],
    budget: { provider_calls: 2 },
  };
  const successorAuthorityJson = canonicalJson(successorAuthority);
  const successorAuthorityHash = sha256(successorAuthorityJson);
  const successorCapabilityHash = sha256("successor-live-capability-secret");
  const handoffRef = parseArtifactRef({
    path: ".agent-run/AFAB-LAUNCH/chair-handoff.json",
    digest: digest("live-chair-handoff"),
  }, "test.liveHandoffRef");
  const successorResult = canonicalJson({
    agentId: "successor_live_01",
    authorityId: successorAuthorityId,
    adapterId: "claude-agent-sdk",
    actionId: "successor_live_action_01",
    providerSessionRef: "claude-successor-live-session",
    providerSessionGeneration: 1,
    bridgeState: "active",
    bridgeGeneration: 1,
    evidenceDigest: digest("successor-live-activation"),
  });
  const successorPayload = canonicalJson({ payload: {} });
  fixture.database.transaction(() => {
    fixture.database.prepare(`
      INSERT INTO authorities(authority_id, run_id, parent_authority_id, authority_json, authority_hash, created_at)
      VALUES (?, 'run_launch_01', ?, ?, ?, ?)
    `).run(successorAuthorityId, chair.authority_id, successorAuthorityJson, successorAuthorityHash, now);
    fixture.database.prepare(`
      INSERT INTO authority_budget(authority_id, unit_key, granted, reserved, consumed, usage_unknown)
      VALUES (?, 'provider_calls', 2, 0, 0, 0)
    `).run(successorAuthorityId);
    fixture.database.prepare(`
      INSERT INTO agents(run_id, agent_id, parent_agent_id, authority_id, provider_session_ref, lifecycle)
      VALUES ('run_launch_01', 'successor_live_01', 'chair_launch_01', ?, 'claude-successor-live-session', 'ready')
    `).run(successorAuthorityId);
    fixture.database.prepare(`INSERT INTO mailbox_state(run_id, recipient_id) VALUES ('run_launch_01', 'successor_live_01')`).run();
    fixture.database.prepare(`
      INSERT INTO capabilities(token_hash, run_id, agent_id, principal_generation, expires_at)
      VALUES (?, 'run_launch_01', 'successor_live_01', 1, ?)
    `).run(successorCapabilityHash, now + 60_000);
    admitProviderActionFixture(fixture.database, {
      runId: "run_launch_01",
      adapterId: "claude-agent-sdk",
      actionId: "successor_live_action_01",
      operation: "spawn",
      targetAgentId: "successor_live_01",
      providerSessionGeneration: 1,
      identityHash: sha256("successor-live-identity"),
      payloadHash: sha256(successorPayload),
      payloadJson: successorPayload,
      status: "terminal",
      historyJson: '["prepared","dispatched","accepted","terminal"]',
      executionCount: 1,
      effectCount: 1,
      idempotencyProven: true,
      resultJson: successorResult,
      updatedAt: now,
    });
    fixture.database.prepare(`
      INSERT INTO provider_agent_custody(
        run_id, action_id, operation, actor_agent_id, target_agent_id, authority_id,
        adapter_id, bridge_contract_digest, bridge_capable, capability_hash,
        capability_expires_at, principal_generation, requested_provider_session_ref,
        intent_digest, created_at
      ) VALUES (
        'run_launch_01', 'successor_live_action_01', 'spawn', 'chair_launch_01', 'successor_live_01', ?,
        'claude-agent-sdk', ?, 1, ?, ?, 1, NULL, ?, ?
      )
    `).run(
      successorAuthorityId,
      digest("successor-live-bridge-contract"),
      successorCapabilityHash,
      now + 60_000,
      digest("successor-live-intent"),
      now,
    );
    fixture.database.prepare(`
      INSERT INTO agent_bridge_state(
        run_id, agent_id, adapter_id, action_id, provider_session_ref,
        provider_session_generation, bridge_state, bridge_generation,
        capability_hash, activation_evidence_digest, revision, created_at, updated_at
      ) VALUES (
        'run_launch_01', 'successor_live_01', 'claude-agent-sdk', 'successor_live_action_01',
        'claude-successor-live-session', 1, 'active', 1, ?, ?, 1, ?, ?
      )
    `).run(successorCapabilityHash, digest("successor-live-activation"), now, now);
    fixture.database.prepare(`
      INSERT INTO provider_state(run_id, agent_id, provider_session_generation)
      VALUES ('run_launch_01', 'successor_live_01', 1)
    `).run();
    fixture.database.prepare(`
      INSERT INTO artifacts(
        artifact_id, project_id, project_session_id, run_id, task_id,
        publisher_kind, publisher_ref, publisher_agent_id, source_kind, evidence_kind,
        relative_path, sha256, registry_state, quarantine_reason, revision, created_at
      ) VALUES (
        'artifact_live_handoff_01', 'project_01', 'session_launch_01', 'run_launch_01', NULL,
        'agent', 'chair_launch_01', 'chair_launch_01', 'run-file', 'artifact',
        ?, ?, 'active', NULL, 1, ?
      )
    `).run(handoffRef.path, handoffRef.digest, now);
  })();
  const session = fixture.database.prepare(`
    SELECT revision, generation, membership_revision FROM project_sessions WHERE project_session_id='session_launch_01'
  `).get() as { revision: number; generation: number; membership_revision: number };
  const run = fixture.database.prepare(`
    SELECT revision, chair_generation, chair_lease_id FROM runs WHERE run_id='run_launch_01'
  `).get() as { revision: number; chair_generation: number; chair_lease_id: string };
  const bridge = fixture.database.prepare(`
    SELECT revision, bridge_generation FROM launched_chair_bridge_state
  `).get() as { revision: number; bridge_generation: number };
  return {
    successorCapabilityHash,
    intent: {
      kind: "chair-live-handoff",
      schemaVersion: 1,
      projectSessionId: "session_launch_01" as never,
      coordinationRunId: "run_launch_01" as never,
      handoffRef,
      predecessorAgentId: "chair_launch_01" as never,
      successorAgentId: "successor_live_01" as never,
      successorAuthorityId,
      successorAuthorityDigest: `sha256:${successorAuthorityHash}` as Sha256Digest,
      expectedSessionRevision: session.revision,
      expectedSessionGeneration: session.generation,
      expectedMembershipRevision: session.membership_revision,
      expectedRunRevision: run.revision,
      expectedChairGeneration: run.chair_generation,
      expectedChairLeaseId: run.chair_lease_id,
      expectedBridgeRevision: bridge.revision,
      expectedChairBridgeGeneration: bridge.bridge_generation,
      expectedPredecessorPrincipalGeneration: 1,
      expectedSuccessorPrincipalGeneration: 1,
      expectedSuccessorBridgeRevision: 1,
      expectedSuccessorBridgeGeneration: 1,
      providerAdapterId: "claude-agent-sdk",
      providerContractDigest: contractDigest,
    },
  };
}

async function prepareLiveHandoffFixture(
  options: NonNullable<Parameters<typeof createFixture>[0]> = {},
): Promise<{
  fixture: ReturnType<typeof createFixture>;
  seeded: ReturnType<typeof seedLiveHandoffSuccessor>;
  inspection: Awaited<ReturnType<LaunchCustodyService["inspectChairLiveHandoff"]>>;
}> {
  const fixture = createFixture({
    retainedChairBridge: true,
    dispatch: async () => ({
      schemaVersion: 1,
      providerAdapterId: "claude-agent-sdk",
      providerActionId: "provider_launch_01",
      providerContractDigest: contractDigest,
      observationKind: "dispatch-return",
      observedAt: "2027-01-01T00:00:00.000Z",
      outcome: {
        kind: "terminal-success",
        providerSessionRef: "claude-chair-live-helper",
        providerSessionGeneration: 2,
        effectDigest: digest("provider-live-helper-predecessor"),
        resourceUsage: { provider_calls: 1 },
      },
    }),
    ...options,
  });
  const { handle } = await prepareFixture(fixture);
  await fixture.service.dispatchPrepared(handle);
  const seeded = seedLiveHandoffSuccessor(fixture);
  const inspection = await fixture.service.inspectChairLiveHandoff(seeded.intent);
  return { fixture, seeded, inspection };
}

afterEach(() => {
  for (const database of databases.splice(0)) database.close();
  for (const directory of directories.splice(0)) rmSync(directory, { recursive: true, force: true });
});

describe("launch custody", () => {
  it("retries daemon-owned launch-intent preparation after an interruption without persisting partial custody", async () => {
    let armed = true;
    const fixture = createFixture({
      fault: (label) => {
        if (label === "launch:intent-prepare:complete" && armed) {
          armed = false;
          throw new Error("fault:launch:intent-prepare:complete");
        }
      },
    });
    const request = {
      projectId: fixture.intent.projectId,
      projectSessionId: fixture.intent.projectSessionId,
      expectedSessionGeneration: fixture.intent.expectedSessionGeneration,
      launchPacketRef: fixture.intent.launchPacketRef,
    };

    await expect(fixture.service.prepareLaunchIntent(request))
      .rejects.toThrow("fault:launch:intent-prepare:complete");
    expect(fixture.database.prepare(`
      SELECT
        (SELECT COUNT(*) FROM runs) AS runs,
        (SELECT COUNT(*) FROM operator_commands) AS commands,
        (SELECT COUNT(*) FROM project_session_launch_custody) AS custody
    `).get()).toEqual({ runs: 0, commands: 0, custody: 0 });
    await expect(fixture.service.prepareLaunchIntent(request)).resolves.toEqual(fixture.intent);
  });

  it("binds the exact provider-session attestation contract into launch custody", () => {
    expect(parseLaunchAdapterContract(contract)).toEqual(contract);
    expect(() => parseLaunchAdapterContract({
      ...contract,
      attestation: { ...contract.attestation, method: "wrapper-mailbox-probe-v1" },
    })).toThrow(/attestation/u);
    expect(() => parseLaunchAdapterContract({
      ...contract,
      attestation: { ...contract.attestation, wrapperMayAttest: true },
    })).toThrow(/unknown field/u);
  });

  it("inspects closed artifacts and atomically prepares one hash-only chair launch", async () => {
    const fixture = createFixture();
    const { handle } = await prepareFixture(fixture);

    expect(handle).toMatchObject({
      providerAdapterId: "claude-agent-sdk",
      providerActionId: "provider_launch_01",
      capability: "chair-secret-canary-01",
      socketPath: "/private/agent-fabric.sock",
      attestationChallenge,
      attestationChallengeDigest,
      expectedPrincipal: {
        agentId: "chair_launch_01",
        projectSessionId: "session_launch_01",
        runId: "run_launch_01",
        principalGeneration: 1,
      },
    });
    expect(fixture.database.prepare(`
      SELECT state, revision FROM project_sessions WHERE project_session_id='session_launch_01'
    `).get()).toEqual({ state: "launching", revision: 3 });
    expect(fixture.database.prepare(`
      SELECT lifecycle_state, chair_agent_id FROM runs WHERE run_id='run_launch_01'
    `).get()).toEqual({ lifecycle_state: "launching", chair_agent_id: "chair_launch_01" });
    expect(fixture.database.prepare(`
      SELECT status, execution_count, effect_count FROM provider_actions
       WHERE adapter_id='claude-agent-sdk' AND action_id='provider_launch_01'
    `).get()).toEqual({ status: "prepared", execution_count: 0, effect_count: 0 });
    expect(fixture.database.prepare(`
      SELECT run_id, state FROM provider_action_pair_preflights
       WHERE adapter_id='claude-agent-sdk' AND action_id='provider_launch_01'
    `).get()).toEqual({ run_id: "run_launch_01", state: "admitted" });
    expect(fixture.database.pragma("foreign_key_check")).toEqual([]);
    expect(fixture.database.prepare(`
      SELECT COUNT(*) AS count FROM project_session_launch_custody
       WHERE attestation_challenge_digest=?
    `).get(attestationChallengeDigest)).toEqual({ count: 1 });
    expect(fixture.database.prepare(`
      SELECT COUNT(*) AS count FROM capabilities WHERE token_hash=?
    `).get(sha256("chair-secret-canary-01"))).toEqual({ count: 1 });
    expect(fixture.database.prepare(`
      SELECT COUNT(*) AS count FROM capabilities WHERE token_hash='chair-secret-canary-01'
    `).get()).toEqual({ count: 0 });
    expect(fixture.database.prepare(`
      SELECT operation_id, state FROM resource_reservations
       WHERE reservation_id=(SELECT reservation_id FROM project_session_launch_custody)
    `).get()).toEqual({ operation_id: "provider_launch_01", state: "reserved" });
    expect(fixture.service.launchProviderActionJournalRefForCommand("operator_01", "commit_launch_01")).toMatchObject({
      journalState: "prepared",
      journalRevision: 1,
      outcomeKind: null,
      outcomeDigest: null,
    });
    expect(fixture.service.seatProvisioningDescriptorForCommand("operator_01", "commit_launch_01"))
      .toStrictEqual({
        schemaVersion: 1,
        projectSessionId: "session_launch_01",
        sessionRevision: 3,
        sessionGeneration: 1,
        coordinationRunId: "run_launch_01",
        runRevision: 1,
        chairAgentId: "chair_launch_01",
        chairGeneration: 1,
        chairLeaseId: "chair:run_launch_01:1",
      });
    expect(databaseContains(fixture.database, "chair-secret-canary-01")).toBe(false);
    expect(databaseContains(fixture.database, attestationChallenge)).toBe(false);
  });

  it("routes operator preview and commit through custody instead of the generic effect port", async () => {
    const fixture = createFixture();
    const operatorStore = new OperatorStore({ database: fixture.database, clock: () => now });
    let genericEffects = 0;
    const actions = new OperatorActionStore({
      database: fixture.database,
      operatorStore,
      statePort: { read: async () => { throw new Error("generic state port must not inspect launch"); } },
      effectPort: {
        dispatch: async () => { genericEffects += 1; throw new Error("generic effect port must not launch"); },
        observe: async () => { genericEffects += 1; throw new Error("generic effect port must not launch"); },
      },
      launchCustody: fixture.service,
      clock: () => now,
    });
    const context = {
      operatorId: "operator_01",
      projectId: "project_01",
      projectAuthorityGeneration: 1,
      principalGeneration: 1,
    } as never;
    const command = {
      credential: { capabilityId: "operator_cap_launch_01", token: "operator-secret" },
      commandId: "preview_launch_vertical_01",
      expectedRevision: 2,
      actor: "operator_01",
      provenance: {
        kind: "console-direct-input",
        clientId: "console_launch_vertical_01",
        inputEventId: "input_preview_launch_vertical_01",
      },
      evidenceRefs: [],
    };
    const preview = await actions.preview(context, {
      command,
      projectId: "project_01",
      intent: fixture.intent,
    } as unknown as OperatorActionPreviewRequest, { allowLaunchIntent: true });
    expect(actions.replayLaunchPreview(context, {
      command,
      projectId: "project_01" as never,
      projectSessionId: "session_launch_01" as never,
      expectedSessionGeneration: 1,
      launchPacketRef: fixture.intent.launchPacketRef,
    } as never)).toEqual(preview);
    expect(() => actions.replayLaunchPreview(context, {
      command,
      projectId: "project_01" as never,
      projectSessionId: "session_launch_01" as never,
      expectedSessionGeneration: 1,
      launchPacketRef: { ...fixture.intent.launchPacketRef, digest: digest("changed packet") },
    } as never)).toThrowError(/reused with changed input/u);
    const receipt = await actions.commit(context, {
      command: {
        ...command,
        commandId: "commit_launch_vertical_01",
        provenance: { ...command.provenance, inputEventId: "input_commit_launch_vertical_01" },
      },
      projectId: "project_01",
      previewId: preview.previewId,
      expectedPreviewRevision: preview.previewRevision,
      previewDigest: preview.previewDigest,
      expectedIntentDigest: preview.intentDigest,
      confirmation: { kind: "explicit", confirmationId: "confirm_launch_vertical_01" },
    } as unknown as OperatorActionCommitRequest);

    expect(genericEffects).toBe(0);
    expect(receipt.launchProviderActionJournalRef).toMatchObject({ journalState: "prepared", outcomeKind: null });
    expect(actions.status({
      credential: command.credential,
      projectId: "project_01",
      commandId: "commit_launch_vertical_01",
    } as never)).toMatchObject({
      status: "ambiguous",
      launchProviderActionJournalRef: { journalState: "ambiguous", outcomeKind: "ambiguous" },
    });
  });

  it("rolls back every launch-owned row when any preparation statement faults", async () => {
    const labels = [
      "launch:prepare:session",
      "launch:prepare:run",
      "provider-action-admission:after-preflight-insert",
      "launch:prepare:authority",
      "launch:prepare:chair",
      "launch:prepare:scopes",
      "launch:prepare:reservation",
      "launch:prepare:provider-action",
      "launch:prepare:memberships",
      "launch:prepare:custody",
    ];
    for (const target of labels) {
      let armed = true;
      const fixture = createFixture({
        fault: (label) => {
          if (label === target && armed) {
            armed = false;
            throw new Error(`fault:${target}`);
          }
        },
      });
      await expect(prepareFixture(fixture)).rejects.toThrow(`fault:${target}`);
      expect(fixture.database.prepare("SELECT COUNT(*) AS count FROM runs").get(), target).toEqual({ count: 0 });
      expect(fixture.database.prepare("SELECT COUNT(*) AS count FROM provider_actions").get(), target).toEqual({ count: 0 });
      expect(fixture.database.prepare(`
        SELECT state FROM provider_action_pair_preflights
         WHERE adapter_id=? AND action_id=?
      `).get(fixture.intent.providerAdapterId, fixture.intent.providerActionId), target)
        .toBeUndefined();
      expect(fixture.database.prepare("SELECT COUNT(*) AS count FROM resource_reservations").get(), target).toEqual({ count: 0 });
      expect(fixture.database.prepare("SELECT COUNT(*) AS count FROM project_session_launch_custody").get(), target).toEqual({ count: 0 });
      expect(fixture.database.prepare(`
        SELECT state, revision FROM project_sessions WHERE project_session_id='session_launch_01'
      `).get(), target).toEqual({ state: "awaiting_launch", revision: 2 });
      expect(fixture.database.prepare("SELECT COUNT(*) AS count FROM operator_commands").get(), target).toEqual({ count: 0 });
      await expect(prepareFixture(fixture)).resolves.toMatchObject({
        handle: { providerActionId: fixture.intent.providerActionId },
      });
      expect(fixture.database.prepare(`
        SELECT state FROM provider_action_pair_preflights
         WHERE adapter_id=? AND action_id=?
      `).get(fixture.intent.providerAdapterId, fixture.intent.providerActionId), target)
        .toEqual({ state: "admitted" });
    }
  });

  it("leaves no launch run or preflight when the atomic operator preparation rolls back", async () => {
    const fixture = createFixture();
    const failure = new ProjectFabricCoreError("DEDUPE_CONFLICT", "launch command is permanently reused");
    let prepareCalls = 0;
    const actions = new OperatorActionStore({
      database: fixture.database,
      operatorStore: new OperatorStore({ database: fixture.database, clock: () => now }),
      statePort: { read: async () => { throw new Error("generic state port must not inspect launch"); } },
      effectPort: {
        dispatch: async () => { throw new Error("generic effect port must not launch"); },
        observe: async () => { throw new Error("generic effect port must not launch"); },
      },
      launchCustody: {
        readCurrentState: fixture.service.readCurrentState.bind(fixture.service),
        inspect: fixture.service.inspect.bind(fixture.service),
        prepareInTransaction: () => {
          prepareCalls += 1;
          throw failure;
        },
        dispatchPrepared: fixture.service.dispatchPrepared.bind(fixture.service),
        launchProviderActionJournalRefForCommand:
          fixture.service.launchProviderActionJournalRefForCommand.bind(fixture.service),
        seatProvisioningDescriptorForCommand:
          fixture.service.seatProvisioningDescriptorForCommand.bind(fixture.service),
      },
      clock: () => now,
    });
    const context = operatorPrincipal() as never;
    const command = {
      credential: { capabilityId: "operator_cap_launch_01", token: "operator-secret" },
      commandId: "preview_launch_terminal_preflight_01",
      expectedRevision: 2,
      actor: "operator_01",
      provenance: {
        kind: "console-direct-input",
        clientId: "console_launch_terminal_preflight_01",
        inputEventId: "input_preview_launch_terminal_preflight_01",
      },
      evidenceRefs: [],
    };
    const preview = await actions.preview(context, {
      command,
      projectId: "project_01",
      intent: fixture.intent,
    } as unknown as OperatorActionPreviewRequest, { allowLaunchIntent: true });
    const commit = {
      command: {
        ...command,
        commandId: "commit_launch_terminal_preflight_01",
        provenance: { ...command.provenance, inputEventId: "input_commit_launch_terminal_preflight_01" },
      },
      projectId: "project_01",
      previewId: preview.previewId,
      expectedPreviewRevision: preview.previewRevision,
      previewDigest: preview.previewDigest,
      expectedIntentDigest: preview.intentDigest,
      confirmation: { kind: "explicit", confirmationId: "confirm_launch_terminal_preflight_01" },
    } as unknown as OperatorActionCommitRequest;

    await expect(actions.commit(context, commit)).rejects.toBe(failure);

    expect(fixture.database.prepare(`
      SELECT state FROM provider_action_pair_preflights
       WHERE adapter_id=? AND action_id=?
    `).get(fixture.intent.providerAdapterId, fixture.intent.providerActionId)).toBeUndefined();
    expect(fixture.database.prepare("SELECT COUNT(*) AS count FROM runs").get()).toEqual({ count: 0 });
    await expect(actions.commit(context, commit)).rejects.toBe(failure);
    expect(prepareCalls).toBe(2);
  });

  it("cleans a prepared crash without adapter I/O and preserves immutable custody", async () => {
    const fixture = createFixture();
    await prepareFixture(fixture);

    await expect(fixture.service.recover()).resolves.toEqual({
      preparedFailed: 1,
      lookedUp: 0,
      activated: 0,
      failed: 1,
      ambiguous: 0,
      recoveryRequired: 0,
    });
    expect(fixture.database.prepare(`
      SELECT state FROM project_sessions WHERE project_session_id='session_launch_01'
    `).get()).toEqual({ state: "launch_failed" });
    expect(fixture.database.prepare(`
      SELECT status, execution_count, effect_count FROM provider_actions
       WHERE adapter_id='claude-agent-sdk' AND action_id='provider_launch_01'
    `).get()).toEqual({ status: "terminal", execution_count: 0, effect_count: 0 });
    expect(fixture.database.prepare(`
      SELECT revoked_at FROM capabilities
       WHERE token_hash=(SELECT capability_hash FROM project_session_launch_custody)
    `).get()).toEqual({ revoked_at: now });
    expect(fixture.database.prepare(`
      SELECT status FROM run_chair_leases WHERE lease_id='chair:run_launch_01:1'
    `).get()).toEqual({ status: "revoked" });
    expect(fixture.database.prepare(`
      SELECT state FROM resource_reservations
       WHERE reservation_id=(SELECT reservation_id FROM project_session_launch_custody)
    `).get()).toEqual({ state: "released" });
    expect(() => fixture.database.prepare(`
      UPDATE project_session_launch_custody SET created_at=created_at+1
    `).run()).toThrow(/INVARIANT_launch_custody_immutable/u);
  });

  it("reconciles a complete dispatch return without waiting for restart", async () => {
    const outcome = {
      schemaVersion: 1,
      providerAdapterId: "claude-agent-sdk",
      providerActionId: "provider_launch_01",
      providerContractDigest: contractDigest,
      observationKind: "dispatch-return",
      observedAt: "2027-01-01T00:00:00.000Z",
      outcome: {
        kind: "terminal-success",
        providerSessionRef: "claude-session-dispatch-01",
        providerSessionGeneration: 2,
        effectDigest: digest("provider-dispatch-effect"),
        resourceUsage: { provider_calls: 1 },
      },
    };
    const fixture = createFixture({ dispatch: async () => outcome });
    const { handle } = await prepareFixture(fixture);

    await expect(fixture.service.dispatchPrepared(handle)).resolves.toEqual(outcome);
    expect(fixture.database.prepare(`
      SELECT state FROM project_sessions WHERE project_session_id='session_launch_01'
    `).get()).toEqual({ state: "active" });
    expect(fixture.database.prepare(`
      SELECT status, execution_count, effect_count, provider_session_generation
        FROM provider_actions WHERE adapter_id='claude-agent-sdk' AND action_id='provider_launch_01'
    `).get()).toEqual({
      status: "terminal",
      execution_count: 1,
      effect_count: 1,
      provider_session_generation: 2,
    });
    expect(fixture.service.launchProviderActionJournalRefForCommand("operator_01", "commit_launch_01")).toMatchObject({
      journalState: "terminal",
      journalRevision: 3,
      outcomeKind: "terminal-success",
    });
    expect(fixture.database.prepare(`
      SELECT state, bridge_generation, principal_generation, provider_session_generation
        FROM launched_chair_bridge_state WHERE coordination_run_id='run_launch_01'
    `).get()).toEqual({
      state: "active",
      bridge_generation: 1,
      principal_generation: 1,
      provider_session_generation: 2,
    });
  });

  it("persists and fences one exact live chair-bridge loss idempotently", async () => {
    const outcome = {
      schemaVersion: 1,
      providerAdapterId: "claude-agent-sdk",
      providerActionId: "provider_launch_01",
      providerContractDigest: contractDigest,
      observationKind: "dispatch-return",
      observedAt: "2027-01-01T00:00:00.000Z",
      outcome: {
        kind: "terminal-success",
        providerSessionRef: "claude-chair-live-loss-01",
        providerSessionGeneration: 3,
        effectDigest: digest("provider-live-loss-effect"),
        resourceUsage: { provider_calls: 1 },
      },
    };
    const fixture = createFixture({
      dispatch: async () => outcome,
      retainedChairBridge: true,
    });
    const { handle } = await prepareFixture(fixture);
    await fixture.service.dispatchPrepared(handle);
    const loss = {
      projectSessionId: "session_launch_01",
      runId: "run_launch_01",
      agentId: "chair_launch_01",
      principalGeneration: 1,
      adapterId: "claude-agent-sdk",
      actionId: "provider_launch_01",
      providerSessionRef: "claude-chair-live-loss-01",
      providerSessionGeneration: 3,
      bridgeGeneration: 1,
      reason: "retained adapter socket closed",
    };

    expect(() => fixture.service.observeChairBridgeLoss({
      ...loss,
      bridgeGeneration: 2,
    })).toThrow(/does not match retained custody/u);
    expect(fixture.database.prepare(`
      SELECT state FROM launched_chair_bridge_state WHERE coordination_run_id='run_launch_01'
    `).get()).toEqual({ state: "active" });
    expect(fixture.database.prepare(`
      SELECT COUNT(*) AS count FROM chair_bridge_losses WHERE coordination_run_id='run_launch_01'
    `).get()).toEqual({ count: 0 });
    expect(fixture.service.observeChairBridgeLoss(loss)).toBe(true);
    expect(fixture.service.observeChairBridgeLoss(loss)).toBe(false);
    expect(fixture.database.prepare(`
      SELECT COUNT(*) AS count FROM chair_bridge_losses WHERE coordination_run_id='run_launch_01'
    `).get()).toEqual({ count: 1 });
    expect(fixture.database.prepare(`
      SELECT state, bridge_generation, revision FROM launched_chair_bridge_state
       WHERE coordination_run_id='run_launch_01'
    `).get()).toEqual({ state: "lost", bridge_generation: 1, revision: 2 });
    expect(fixture.database.prepare(`
      SELECT state FROM project_sessions WHERE project_session_id='session_launch_01'
    `).get()).toEqual({ state: "recovery_required" });
    expect(fixture.database.prepare(`
      SELECT lifecycle_state FROM runs WHERE run_id='run_launch_01'
    `).get()).toEqual({ lifecycle_state: "recovery_required" });
    expect(fixture.database.prepare(`
      SELECT status FROM run_chair_leases WHERE run_id='run_launch_01'
    `).get()).toEqual({ status: "frozen" });
    expect(fixture.database.prepare(`
      SELECT revoked_at FROM capabilities
       WHERE token_hash=(SELECT capability_hash FROM project_session_launch_custody)
    `).get()).toEqual({ revoked_at: now });
    expect(fixture.database.prepare(`
      SELECT reason FROM delivery_freezes WHERE run_id='run_launch_01' AND agent_id='chair_launch_01'
    `).get()).toMatchObject({ reason: expect.stringMatching(/^chair-bridge-loss:/u) });
    fixture.database.prepare(`
      INSERT INTO operator_capabilities(
        capability_id, token_hash, operator_id, project_id, project_session_id,
        project_authority_generation, session_generation, principal_generation,
        kind, operations_json, issued_at, expires_at
      ) VALUES (
        'operator_cap_recovery_transition', ?, 'operator_01', 'project_01',
        'session_launch_01', 1, 1, 1, 'session', '["read","decide"]', ?, ?
      )
    `).run(sha256("recovery-transition-secret"), now - 1, now + 60_000);
    const before = fixture.database.prepare(`
      SELECT
        (SELECT state FROM project_sessions WHERE project_session_id='session_launch_01') AS session_state,
        (SELECT revision FROM project_sessions WHERE project_session_id='session_launch_01') AS session_revision,
        (SELECT lifecycle_state FROM runs WHERE run_id='run_launch_01') AS run_state,
        (SELECT revision FROM runs WHERE run_id='run_launch_01') AS run_revision,
        (SELECT status FROM run_chair_leases WHERE run_id='run_launch_01') AS lease_status,
        (SELECT state FROM launched_chair_bridge_state WHERE coordination_run_id='run_launch_01') AS bridge_state,
        (SELECT COUNT(*) FROM chair_bridge_losses WHERE coordination_run_id='run_launch_01') AS losses,
        (SELECT COUNT(*) FROM chair_bridge_recovery_custody recovery
          JOIN chair_bridge_losses loss ON loss.loss_id=recovery.loss_id
          WHERE loss.coordination_run_id='run_launch_01') AS recoveries,
        (SELECT COUNT(*) FROM operator_commands WHERE project_session_id='session_launch_01') AS commands
    `).get();
    const sessionRevision = (before as { session_revision: number }).session_revision;
    const sessions = new ProjectSessionStore({
      database: fixture.database,
      operatorStore: new OperatorStore({ database: fixture.database, clock: () => now }),
      clock: () => now,
    });
    for (const target of ["reconciling", "quarantined"] as const) {
      expect(() => sessions.transitionProjectSession({
        operatorId: "operator_01" as OperatorId,
        projectId: "project_01" as ProjectId,
        projectAuthorityGeneration: 1,
        principalGeneration: 1,
      }, {
        command: {
          credential: { capabilityId: "operator_cap_recovery_transition", token: "recovery-transition-secret" },
          commandId: `forbidden_lost_bridge_${target}`,
          expectedRevision: sessionRevision,
          actor: "operator_01",
          provenance: {
            kind: "console-direct-input",
            clientId: "console_recovery",
            inputEventId: `input_${target}`,
          },
          evidenceRefs: [],
        },
        projectSessionId: "session_launch_01",
        expectedGeneration: 1,
        transition: { to: target, reason: "generic transition must not steal recovery custody" },
      } as unknown as ProjectSessionTransitionRequest)).toThrowError(
        expect.objectContaining({ code: "RECOVERY_REQUIRED" }),
      );
      expect(fixture.database.prepare(`
        SELECT
          (SELECT state FROM project_sessions WHERE project_session_id='session_launch_01') AS session_state,
          (SELECT revision FROM project_sessions WHERE project_session_id='session_launch_01') AS session_revision,
          (SELECT lifecycle_state FROM runs WHERE run_id='run_launch_01') AS run_state,
          (SELECT revision FROM runs WHERE run_id='run_launch_01') AS run_revision,
          (SELECT status FROM run_chair_leases WHERE run_id='run_launch_01') AS lease_status,
          (SELECT state FROM launched_chair_bridge_state WHERE coordination_run_id='run_launch_01') AS bridge_state,
          (SELECT COUNT(*) FROM chair_bridge_losses WHERE coordination_run_id='run_launch_01') AS losses,
          (SELECT COUNT(*) FROM chair_bridge_recovery_custody recovery
            JOIN chair_bridge_losses loss ON loss.loss_id=recovery.loss_id
            WHERE loss.coordination_run_id='run_launch_01') AS recoveries,
          (SELECT COUNT(*) FROM operator_commands WHERE project_session_id='session_launch_01') AS commands
      `).get()).toEqual(before);
    }
    expect(() => fixture.database.prepare(`
      INSERT INTO capabilities(token_hash, run_id, agent_id, principal_generation, expires_at)
      VALUES ('forbidden-after-chair-loss', 'run_launch_01', 'chair_launch_01', 2, ${now + 60_000})
    `).run()).toThrow(/INVARIANT_chair_bridge_loss_freezes_grants/u);
    expect(() => fixture.database.prepare(`
      INSERT INTO authorities(authority_id, run_id, parent_authority_id, authority_json, authority_hash, created_at)
      VALUES ('forbidden-authority-after-chair-loss', 'run_launch_01', NULL, '{}', 'hash', ${now})
    `).run()).toThrow(/INVARIANT_chair_bridge_loss_freezes_grants/u);
  });

  it("terminalises an exact lost chair through versioned abandon recovery custody", async () => {
    const outcome = {
      schemaVersion: 1,
      providerAdapterId: "claude-agent-sdk",
      providerActionId: "provider_launch_01",
      providerContractDigest: contractDigest,
      observationKind: "dispatch-return" as const,
      observedAt: "2027-01-01T00:00:00.000Z",
      outcome: {
        kind: "terminal-success" as const,
        providerSessionRef: "claude-chair-abandon-01",
        providerSessionGeneration: 3,
        effectDigest: digest("provider-abandon-effect"),
        resourceUsage: { provider_calls: "unknown", concurrent_turns: "unknown" },
      },
    };
    const fixture = createFixture({
      dispatch: async () => outcome,
      retainedChairBridge: true,
      launchResources: { provider_calls: 1, concurrent_turns: 1 },
    });
    const localSubjectHash = digest("local operator subject");
    fixture.database.prepare(`
      UPDATE operator_principals SET authenticated_subject_hash=? WHERE operator_id='operator_01'
    `).run(localSubjectHash);
    fixture.database.prepare(`
      INSERT INTO operator_capabilities(
        capability_id, token_hash, operator_id, project_id, project_session_id,
        project_authority_generation, session_generation, principal_generation,
        kind, operations_json, issued_at, expires_at
      ) VALUES (
        'operator_cap_project_01', ?, 'operator_01', 'project_01', NULL,
        1, NULL, 1, 'project-launch', '["read","launch"]', ?, ?
      )
    `).run(sha256("operator-project-secret"), now - 1, now + 60_000);
    const { handle } = await prepareFixture(fixture);
    await fixture.service.dispatchPrepared(handle);
    fixture.service.observeChairBridgeLoss({
      projectSessionId: "session_launch_01",
      runId: "run_launch_01",
      agentId: "chair_launch_01",
      principalGeneration: 1,
      adapterId: "claude-agent-sdk",
      actionId: "provider_launch_01",
      providerSessionRef: "claude-chair-abandon-01",
      providerSessionGeneration: 3,
      bridgeGeneration: 1,
      reason: "provider bridge closed",
    });
    const loss = fixture.database.prepare(`SELECT * FROM chair_bridge_losses`).get() as Record<string, unknown>;
    const bridge = fixture.database.prepare(`SELECT * FROM launched_chair_bridge_state`).get() as Record<string, unknown>;
    const session = fixture.database.prepare(`SELECT revision, generation FROM project_sessions`).get() as Record<string, number>;
    const run = fixture.database.prepare(`SELECT revision, chair_generation FROM runs`).get() as Record<string, number>;
    fixture.database.exec(`
      INSERT INTO messages(
        message_id,run_id,sender_id,dedupe_key,payload_hash,audience_json,kind,body,
        requires_ack,conversation_id,hop_count,created_at
      ) VALUES (
        'informational_message','run_launch_01','chair_launch_01','informational-abandon',
        'informational-hash','{}','event','informational only',0,'informational-conversation',0,${now}
      );
      INSERT INTO deliveries(
        delivery_id,message_id,run_id,recipient_id,mailbox_sequence,state,attempt_count
      ) VALUES (
        'informational_delivery','informational_message','run_launch_01','chair_launch_01',999,'ready',0
      );
    `);
    const intent: ChairBridgeRecoveryIntent = {
      kind: "chair-bridge-recovery",
      schemaVersion: 1,
      path: "abandon",
      projectSessionId: "session_launch_01" as never,
      coordinationRunId: "run_launch_01" as never,
      lossId: String(loss.loss_id),
      recoveryManifestDigest: String(loss.recovery_manifest_digest) as Sha256Digest,
      expectedSessionRevision: Number(session.revision),
      expectedSessionGeneration: Number(session.generation),
      expectedRunRevision: Number(run.revision),
      expectedChairGeneration: Number(run.chair_generation),
      expectedPrincipalGeneration: Number(loss.principal_generation),
      expectedBridgeRevision: Number(bridge.revision),
      expectedLostBridgeGeneration: Number(loss.lost_bridge_generation),
      expectedProviderSessionGeneration: Number(loss.provider_session_generation),
      providerAdapterId: "claude-agent-sdk",
      providerContractDigest: String(loss.provider_contract_digest) as Sha256Digest,
      reason: "operator accepted terminal provider loss",
    };
    const operatorStore = new OperatorStore({ database: fixture.database, clock: () => now });
    const issued = operatorStore.openLocalOperatorConsoleTakeoverCapability({
      projectId: "project_01",
      canonicalRoot: fixture.root,
      trustRecordDigest: trustDigest,
      authenticatedSubjectHash: localSubjectHash,
      projectCapability: {
        capabilityId: "operator_cap_project_01",
        token: "operator-project-secret",
      },
      projectSessionId: "session_launch_01",
      expiresAt: new Date(now + 30_000).toISOString(),
    });
    expect(issued).toMatchObject({
      kind: "takeover",
      actions: ["read", "takeover"],
      recoveryIntent: {
        ...intent,
        reason: "operator confirmed terminal retained-chair loss",
      },
    });
    let genericEffects = 0;
    const actions = new OperatorActionStore({
      database: fixture.database,
      operatorStore,
      statePort: { read: async () => { throw new Error("generic state must not inspect recovery"); } },
      effectPort: {
        dispatch: async () => { genericEffects += 1; throw new Error("generic effect must not recover chair"); },
        observe: async () => { genericEffects += 1; throw new Error("generic effect must not recover chair"); },
      },
      chairRecoveryCustody: fixture.service,
      clock: () => now,
    });
    const context = {
      operatorId: "operator_01",
      projectId: "project_01",
      projectAuthorityGeneration: 1,
      principalGeneration: 1,
    } as never;
    const previewCommand = {
      credential: issued.credential,
      commandId: "preview_recovery_abandon_01",
      expectedRevision: issued.recoveryIntent.expectedBridgeRevision,
      actor: "operator_01",
      provenance: {
        kind: "console-direct-input",
        clientId: "console_recovery_01",
        inputEventId: "input_recovery_preview_01",
      },
      evidenceRefs: [],
    };
    const preview = await actions.preview(context, {
      command: previewCommand,
      projectId: "project_01",
      intent: issued.recoveryIntent,
    } as unknown as OperatorActionPreviewRequest);
    const receipt = await actions.commit(context, {
      command: {
        ...previewCommand,
        commandId: "recovery_abandon_01",
        provenance: { ...previewCommand.provenance, inputEventId: "input_recovery_commit_01" },
      },
      projectId: "project_01",
      previewId: preview.previewId,
      expectedPreviewRevision: preview.previewRevision,
      previewDigest: preview.previewDigest,
      expectedIntentDigest: preview.intentDigest,
      confirmation: { kind: "explicit", confirmationId: "confirm_recovery_abandon_01" },
    } as unknown as OperatorActionCommitRequest);
    expect(receipt.afterStateDigest).not.toEqual(receipt.beforeStateDigest);
    expect(genericEffects).toBe(0);
    expect(actions.status({
      credential: previewCommand.credential,
      projectId: "project_01",
      commandId: "recovery_abandon_01",
    } as never)).toMatchObject({ status: "committed" });
    const reconcileRequest = {
      command: {
        ...previewCommand,
        commandId: "reconcile_recovery_abandon_01",
        provenance: { ...previewCommand.provenance, inputEventId: "input_recovery_reconcile_01" },
      },
      projectId: "project_01",
      targetCommandId: "recovery_abandon_01",
      expectedStatus: "committed" as const,
      expectedAttemptGeneration: 1,
      mode: "observe-only" as const,
    };
    const reconciled = await actions.reconcile(context, reconcileRequest as never);
    expect(reconciled).toMatchObject({ status: "committed", commandId: "recovery_abandon_01" });
    await expect(actions.reconcile(context, reconcileRequest as never)).resolves.toEqual(reconciled);
    expect(genericEffects).toBe(0);
    expect(fixture.database.prepare(`SELECT state FROM launched_chair_bridge_state`).get()).toEqual({ state: "abandoned" });
    expect(fixture.database.prepare(`SELECT state FROM project_sessions`).get()).toEqual({ state: "cancelled" });
    expect(fixture.database.prepare(`SELECT lifecycle_state FROM runs`).get()).toEqual({ lifecycle_state: "cancelled" });
    expect(fixture.database.prepare(`
      SELECT used, reserved, usage_unknown FROM resource_dimensions
       WHERE scope_id='scope_project_01' AND unit_key='provider_calls'
    `).get()).toEqual({ used: 1, reserved: 0, usage_unknown: 0 });
    expect(fixture.database.prepare(`
      SELECT used, reserved, usage_unknown FROM resource_dimensions
       WHERE scope_id='scope_project_01' AND unit_key='concurrent_turns'
    `).get()).toEqual({ used: 0, reserved: 0, usage_unknown: 0 });
    expect(fixture.database.prepare(`SELECT path FROM chair_bridge_loss_resolutions`).get()).toEqual({ path: "abandon" });
    expect(fixture.database.prepare(`SELECT state FROM deliveries WHERE delivery_id='informational_delivery'`).get())
      .toEqual({ state: "ready" });
  });

  it("rebinds a lost chair with fresh secret custody, native evidence and duplicate-safe settlement", async () => {
    let recoveryEffects = 0;
    let dispatchedCapabilityGuards: ReturnType<typeof probeRecoveryCapabilityIdentityGuards> | undefined;
    const outcome = {
      schemaVersion: 1,
      providerAdapterId: "claude-agent-sdk",
      providerActionId: "provider_launch_01",
      providerContractDigest: contractDigest,
      observationKind: "dispatch-return" as const,
      observedAt: "2027-01-01T00:00:00.000Z",
      outcome: {
        kind: "terminal-success" as const,
        providerSessionRef: "claude-chair-rebind-01",
        providerSessionGeneration: 3,
        effectDigest: digest("provider-before-rebind-effect"),
        resourceUsage: { provider_calls: 1 },
      },
    };
    const fixture = createFixture({
      dispatch: async () => outcome,
      retainedChairBridge: true,
      recoverChair: async (recovery) => {
        recoveryEffects += 1;
        expect(fixture.database.prepare(`
          SELECT state FROM chair_bridge_recovery_custody WHERE recovery_id=?
        `).get(recovery.recoveryId)).toEqual({ state: "dispatched" });
        dispatchedCapabilityGuards = probeRecoveryCapabilityIdentityGuards(fixture.database);
        return {
          schemaVersion: 1,
          recoveryId: recovery.recoveryId,
          providerAdapterId: "claude-agent-sdk",
          providerActionId: "provider_rebind_01",
          providerContractDigest: contractDigest,
          providerSessionRef: "claude-chair-rebind-01",
          providerSessionGeneration: 4,
          activationEvidenceDigest: digest("provider-after-rebind-effect"),
        };
      },
    });
    const { handle } = await prepareFixture(fixture);
    await fixture.service.dispatchPrepared(handle);
    fixture.service.observeChairBridgeLoss({
      projectSessionId: "session_launch_01",
      runId: "run_launch_01",
      agentId: "chair_launch_01",
      principalGeneration: 1,
      adapterId: "claude-agent-sdk",
      actionId: "provider_launch_01",
      providerSessionRef: "claude-chair-rebind-01",
      providerSessionGeneration: 3,
      bridgeGeneration: 1,
      reason: "provider bridge closed",
    });
    const loss = fixture.database.prepare(`SELECT * FROM chair_bridge_losses`).get() as Record<string, unknown>;
    const bridge = fixture.database.prepare(`SELECT * FROM launched_chair_bridge_state`).get() as Record<string, unknown>;
    const session = fixture.database.prepare(`SELECT revision, generation FROM project_sessions`).get() as Record<string, unknown>;
    const run = fixture.database.prepare(`SELECT revision, chair_generation FROM runs`).get() as Record<string, unknown>;
    const intent: ChairBridgeRecoveryIntent = {
      kind: "chair-bridge-recovery",
      schemaVersion: 1,
      path: "rebind",
      projectSessionId: "session_launch_01" as never,
      coordinationRunId: "run_launch_01" as never,
      lossId: String(loss.loss_id),
      recoveryManifestDigest: String(loss.recovery_manifest_digest) as Sha256Digest,
      expectedSessionRevision: Number(session.revision),
      expectedSessionGeneration: Number(session.generation),
      expectedRunRevision: Number(run.revision),
      expectedChairGeneration: Number(run.chair_generation),
      expectedPrincipalGeneration: 1,
      expectedBridgeRevision: Number(bridge.revision),
      expectedLostBridgeGeneration: 1,
      expectedProviderSessionGeneration: 3,
      providerAdapterId: "claude-agent-sdk",
      providerContractDigest: contractDigest,
      providerActionId: "provider_rebind_01" as never,
    };
    const inspection = await fixture.service.inspectChairRecovery(intent);
    const recovery = fixture.database.transaction(() => fixture.service.prepareChairRecoveryInTransaction({
      inspection,
      operatorId: "operator_01",
      operatorCommandId: "recovery_rebind_01",
      providerActionTicket: recoveryTicket(fixture, inspection),
    }))();
    expect(recovery).toMatchObject({
      capability: "chair-secret-canary-02",
      attestationChallenge,
      socketPath: "/private/agent-fabric.sock",
    });
    fixture.database.exec("SAVEPOINT invalid_rebind_capability_identity");
    try {
      const chair = fixture.database.prepare(`
        SELECT authority_id FROM agents WHERE run_id='run_launch_01' AND agent_id='chair_launch_01'
      `).get() as { authority_id: string };
      fixture.database.prepare(`
        INSERT INTO agents(run_id, agent_id, parent_agent_id, authority_id, lifecycle)
        VALUES ('run_launch_01', 'wrong_rebind_agent', 'chair_launch_01', ?, 'ready')
      `).run(chair.authority_id);
      const fresh = fixture.database.prepare(`
        SELECT new_capability_hash, new_principal_generation
          FROM chair_bridge_recovery_custody
         WHERE operator_command_id='recovery_rebind_01'
      `).get() as { new_capability_hash: string; new_principal_generation: number };
      fixture.database.prepare(`
        INSERT INTO runs(
          run_id, chair_agent_id, workspace_root, project_run_directory, created_at,
          project_session_id, lifecycle_state, revision, chair_generation, chair_lease_id,
          authority_ref, budget_ref, dependency_revision, topology_slot
        ) SELECT 'foreign_rebind_run', 'foreign_rebind_agent', workspace_root, NULL, ?,
                 project_session_id, 'closed', 1, 1, 'foreign-rebind-chair-lease',
                 authority_ref, budget_ref, 1, NULL
            FROM runs WHERE run_id='run_launch_01'
      `).run(now);
      fixture.database.prepare(`
        INSERT INTO authorities(authority_id, run_id, parent_authority_id, authority_json, authority_hash, created_at)
        VALUES ('foreign_rebind_authority', 'foreign_rebind_run', NULL, '{}', 'foreign-rebind-hash', ?)
      `).run(now);
      fixture.database.prepare(`
        INSERT INTO agents(run_id, agent_id, parent_agent_id, authority_id, lifecycle)
        VALUES ('foreign_rebind_run', 'foreign_rebind_agent', NULL, 'foreign_rebind_authority', 'ready')
      `).run();

      let updateBlocked = false;
      fixture.database.exec("SAVEPOINT rebind_capability_update");
      try {
        fixture.database.prepare(`
          UPDATE capabilities SET agent_id='wrong_rebind_agent' WHERE token_hash=?
        `).run(fresh.new_capability_hash);
      } catch (error: unknown) {
        updateBlocked = error instanceof Error && /INVARIANT_chair_bridge_loss_freezes_grants/u.test(error.message);
      } finally {
        fixture.database.exec("ROLLBACK TO rebind_capability_update");
        fixture.database.exec("RELEASE rebind_capability_update");
      }

      let sameRunInsertBlocked = false;
      fixture.database.exec("SAVEPOINT rebind_capability_same_run_insert");
      try {
        fixture.database.prepare(`DELETE FROM capabilities WHERE token_hash=?`).run(fresh.new_capability_hash);
        fixture.database.prepare(`
          INSERT INTO capabilities(token_hash, run_id, agent_id, principal_generation, expires_at)
          VALUES (?, 'run_launch_01', 'wrong_rebind_agent', ?, ?)
        `).run(fresh.new_capability_hash, fresh.new_principal_generation, now + 60_000);
      } catch (error: unknown) {
        sameRunInsertBlocked = error instanceof Error && /INVARIANT_chair_bridge_loss_freezes_grants/u.test(error.message);
      } finally {
        fixture.database.exec("ROLLBACK TO rebind_capability_same_run_insert");
        fixture.database.exec("RELEASE rebind_capability_same_run_insert");
      }

      let wrongRunInsertBlocked = false;
      fixture.database.exec("SAVEPOINT rebind_capability_wrong_run_insert");
      try {
        fixture.database.prepare(`DELETE FROM capabilities WHERE token_hash=?`).run(fresh.new_capability_hash);
        fixture.database.prepare(`
          INSERT INTO capabilities(token_hash, run_id, agent_id, principal_generation, expires_at)
          VALUES (?, 'foreign_rebind_run', 'foreign_rebind_agent', ?, ?)
        `).run(fresh.new_capability_hash, fresh.new_principal_generation, now + 60_000);
      } catch (error: unknown) {
        wrongRunInsertBlocked = error instanceof Error && /INVARIANT_chair_bridge_loss_freezes_grants/u.test(error.message);
      } finally {
        fixture.database.exec("ROLLBACK TO rebind_capability_wrong_run_insert");
        fixture.database.exec("RELEASE rebind_capability_wrong_run_insert");
      }
      expect({ updateBlocked, sameRunInsertBlocked, wrongRunInsertBlocked }).toEqual({
        updateBlocked: true,
        sameRunInsertBlocked: true,
        wrongRunInsertBlocked: true,
      });
    } finally {
      fixture.database.exec("ROLLBACK TO invalid_rebind_capability_identity");
      fixture.database.exec("RELEASE invalid_rebind_capability_identity");
    }
    const concurrentInspection = await fixture.service.inspectChairRecovery({
      ...intent,
      providerActionId: "provider_rebind_concurrent_01" as never,
    });
    expect(() => fixture.database.transaction(() => fixture.service.prepareChairRecoveryInTransaction({
      inspection: concurrentInspection,
      operatorId: "operator_01",
      operatorCommandId: "recovery_rebind_concurrent_01",
      providerActionTicket: recoveryTicket(fixture, concurrentInspection),
    }))()).toThrowError(expect.objectContaining({ code: "CONFLICT" }));
    expect(databaseContains(fixture.database, "chair-secret-canary-02")).toBe(false);
    expect(databaseContains(fixture.database, attestationChallenge)).toBe(false);
    const committed = await fixture.service.dispatchPreparedChairRecovery(recovery);
    await expect(fixture.service.dispatchPreparedChairRecovery(recovery)).resolves.toEqual(committed);
    expect(recoveryEffects).toBe(1);
    expect(dispatchedCapabilityGuards).toEqual({
      identityUpdateBlocked: true,
      deleteBlocked: true,
      mutableMetadataAllowed: true,
      canonicalRevocationAllowed: false,
    });
    expect(probeRecoveryCapabilityIdentityGuards(fixture.database)).toEqual({
      identityUpdateBlocked: true,
      deleteBlocked: true,
      mutableMetadataAllowed: true,
      canonicalRevocationAllowed: true,
    });
    expect(fixture.database.prepare(`
      SELECT chair_agent_id, provider_action_id, provider_session_generation,
             principal_generation, bridge_generation, state
        FROM launched_chair_bridge_state
    `).get()).toEqual({
      chair_agent_id: "chair_launch_01",
      provider_action_id: "provider_rebind_01",
      provider_session_generation: 4,
      principal_generation: 2,
      bridge_generation: 2,
      state: "active",
    });
    expect(fixture.database.prepare(`SELECT state, generation FROM project_sessions`).get()).toEqual({
      state: "active",
      generation: Number(session.generation) + 1,
    });
    expect(fixture.database.prepare(`SELECT lifecycle_state, chair_generation FROM runs`).get()).toEqual({
      lifecycle_state: "active",
      chair_generation: Number(run.chair_generation) + 1,
    });
  });

  it("journals takeover before promotion and reconciles a post-promotion crash by lookup only", async () => {
    let promotions = 0;
    let lookups = 0;
    let crashAfterPromotion = true;
    const outcome = {
      schemaVersion: 1,
      providerAdapterId: "claude-agent-sdk",
      providerActionId: "provider_launch_01",
      providerContractDigest: contractDigest,
      observationKind: "dispatch-return" as const,
      observedAt: "2027-01-01T00:00:00.000Z",
      outcome: {
        kind: "terminal-success" as const,
        providerSessionRef: "claude-chair-takeover-old",
        providerSessionGeneration: 2,
        effectDigest: digest("provider-before-takeover-effect"),
        resourceUsage: { provider_calls: 1 },
      },
    };
    const fixture = createFixture({
      dispatch: async () => outcome,
      retainedChairBridge: true,
      fault: (label) => {
        if (label === "chair-recovery:takeover:after-adapter" && crashAfterPromotion) {
          crashAfterPromotion = false;
          throw new Error("simulated daemon crash after bridge promotion");
        }
      },
      lookupSuccessor: async (input) => {
        lookups += 1;
        expect(input).toMatchObject({
          agentId: "successor_01",
          providerSessionRef: "claude-successor-session",
          sourceBridgeGeneration: 1,
          chairBridgeGeneration: 2,
        });
        return "chair";
      },
      promoteSuccessor: async (input) => {
        promotions += 1;
        expect(fixture.database.prepare(`
          SELECT state FROM chair_bridge_recovery_custody
           WHERE operator_command_id='recovery_takeover_01'
        `).get()).toEqual({ state: "dispatched" });
        expect(input).toMatchObject({
          agentId: "successor_01",
          providerSessionRef: "claude-successor-session",
          sourceBridgeGeneration: 1,
          chairBridgeGeneration: 2,
        });
        return true;
      },
    });
    const { handle } = await prepareFixture(fixture);
    await fixture.service.dispatchPrepared(handle);
    const authority = fixture.database.prepare(`
      SELECT authority_id FROM agents WHERE run_id='run_launch_01' AND agent_id='chair_launch_01'
    `).get() as { authority_id: string };
    const successorCapabilityHash = sha256("successor-capability-secret");
    const successorResult = canonicalJson({
      agentId: "successor_01",
      authorityId: authority.authority_id,
      adapterId: "claude-agent-sdk",
      actionId: "successor_action_01",
      providerSessionRef: "claude-successor-session",
      providerSessionGeneration: 1,
      bridgeState: "active",
      bridgeGeneration: 1,
      evidenceDigest: digest("successor-activation"),
    });
    fixture.database.transaction(() => {
      fixture.database.prepare(`
        INSERT INTO agents(run_id, agent_id, parent_agent_id, authority_id, provider_session_ref, lifecycle)
        VALUES ('run_launch_01', 'successor_01', 'chair_launch_01', ?, 'claude-successor-session', 'ready')
      `).run(authority.authority_id);
      fixture.database.prepare(`INSERT INTO mailbox_state(run_id, recipient_id) VALUES ('run_launch_01', 'successor_01')`).run();
      fixture.database.prepare(`
        INSERT INTO capabilities(token_hash, run_id, agent_id, principal_generation, expires_at)
        VALUES (?, 'run_launch_01', 'successor_01', 1, ?)
      `).run(successorCapabilityHash, now + 60_000);
      admitProviderActionFixture(fixture.database, {
        runId: "run_launch_01",
        adapterId: "claude-agent-sdk",
        actionId: "successor_action_01",
        operation: "spawn",
        targetAgentId: "successor_01",
        providerSessionGeneration: 1,
        identityHash: sha256("successor-identity"),
        payloadHash: sha256("{}"),
        payloadJson: "{}",
        status: "terminal",
        historyJson: '["prepared","dispatched","accepted","terminal"]',
        executionCount: 1,
        effectCount: 1,
        idempotencyProven: true,
        resultJson: successorResult,
        updatedAt: now,
      });
      fixture.database.prepare(`
        INSERT INTO provider_agent_custody(
          run_id, action_id, operation, actor_agent_id, target_agent_id, authority_id,
          adapter_id, bridge_contract_digest, bridge_capable, capability_hash,
          capability_expires_at, principal_generation, requested_provider_session_ref,
          intent_digest, created_at
        ) VALUES (
          'run_launch_01', 'successor_action_01', 'spawn', 'chair_launch_01', 'successor_01', ?,
          'claude-agent-sdk', ?, 1, ?, ?, 1, NULL, ?, ?
        )
      `).run(authority.authority_id, digest("successor-bridge-contract"), successorCapabilityHash, now + 60_000, digest("successor-intent"), now);
      fixture.database.prepare(`
        INSERT INTO agent_bridge_state(
          run_id, agent_id, adapter_id, action_id, provider_session_ref,
          provider_session_generation, bridge_state, bridge_generation,
          capability_hash, activation_evidence_digest, revision, created_at, updated_at
        ) VALUES (
          'run_launch_01', 'successor_01', 'claude-agent-sdk', 'successor_action_01',
          'claude-successor-session', 1, 'active', 1, ?, ?, 1, ?, ?
        )
      `).run(successorCapabilityHash, digest("successor-activation"), now, now);
    })();
    fixture.service.observeChairBridgeLoss({
      projectSessionId: "session_launch_01",
      runId: "run_launch_01",
      agentId: "chair_launch_01",
      principalGeneration: 1,
      adapterId: "claude-agent-sdk",
      actionId: "provider_launch_01",
      providerSessionRef: "claude-chair-takeover-old",
      providerSessionGeneration: 2,
      bridgeGeneration: 1,
      reason: "provider bridge closed",
    });
    const loss = fixture.database.prepare(`SELECT * FROM chair_bridge_losses`).get() as Record<string, unknown>;
    const bridge = fixture.database.prepare(`SELECT * FROM launched_chair_bridge_state`).get() as Record<string, unknown>;
    const session = fixture.database.prepare(`SELECT revision, generation FROM project_sessions`).get() as Record<string, unknown>;
    const run = fixture.database.prepare(`SELECT revision, chair_generation FROM runs`).get() as Record<string, unknown>;
    const intent: ChairBridgeRecoveryIntent = {
      kind: "chair-bridge-recovery",
      schemaVersion: 1,
      path: "takeover",
      projectSessionId: "session_launch_01" as never,
      coordinationRunId: "run_launch_01" as never,
      lossId: String(loss.loss_id),
      recoveryManifestDigest: String(loss.recovery_manifest_digest) as Sha256Digest,
      expectedSessionRevision: Number(session.revision),
      expectedSessionGeneration: Number(session.generation),
      expectedRunRevision: Number(run.revision),
      expectedChairGeneration: Number(run.chair_generation),
      expectedPrincipalGeneration: 1,
      expectedBridgeRevision: Number(bridge.revision),
      expectedLostBridgeGeneration: 1,
      expectedProviderSessionGeneration: 2,
      providerAdapterId: "claude-agent-sdk",
      providerContractDigest: contractDigest,
      successorAgentId: "successor_01",
      expectedSuccessorPrincipalGeneration: 1,
      expectedSuccessorBridgeGeneration: 1,
      expectedSuccessorRevision: 1,
    };
    fixture.database.prepare(`UPDATE capabilities SET expires_at=? WHERE token_hash=?`)
      .run(now, successorCapabilityHash);
    await expect(fixture.service.inspectChairRecovery(intent)).rejects.toThrow();
    fixture.database.prepare(`UPDATE capabilities SET expires_at=? WHERE token_hash=?`)
      .run(now + 60_000, successorCapabilityHash);
    const inspection = await fixture.service.inspectChairRecovery(intent);
    const recovery = fixture.database.transaction(() => fixture.service.prepareChairRecoveryInTransaction({
      inspection,
      operatorId: "operator_01",
      operatorCommandId: "recovery_takeover_01",
      providerActionTicket: recoveryTicket(fixture, inspection),
    }))();
    await expect(fixture.service.dispatchPreparedChairRecovery(recovery)).resolves.toMatchObject({
      status: "ambiguous",
      path: "takeover",
    });
    expect(fixture.database.prepare(`SELECT state FROM chair_bridge_recovery_custody`).get())
      .toEqual({ state: "ambiguous" });
    await expect(fixture.service.reconcileChairRecovery("operator_01", "recovery_takeover_01"))
      .resolves.toMatchObject({ status: "committed", path: "takeover" });
    expect(promotions).toBe(1);
    expect(lookups).toBe(1);
    expect(fixture.database.prepare(`
      SELECT chair_agent_id, provider_action_id, bridge_generation, state
        FROM launched_chair_bridge_state
    `).get()).toEqual({
      chair_agent_id: "successor_01",
      provider_action_id: "successor_action_01",
      bridge_generation: 2,
      state: "active",
    });
    expect(fixture.database.prepare(`SELECT chair_agent_id, lifecycle_state FROM runs`).get()).toEqual({
      chair_agent_id: "successor_01",
      lifecycle_state: "active",
    });
    expect(fixture.database.prepare(`SELECT bridge_state FROM agent_bridge_state WHERE agent_id='successor_01'`).get())
      .toEqual({ bridge_state: "none" });
  });

  it("performs zero adapter I/O for prepared recovery after restart and revokes only the fresh capability", async () => {
    let recoveryEffects = 0;
    const outcome = {
      schemaVersion: 1,
      providerAdapterId: "claude-agent-sdk",
      providerActionId: "provider_launch_01",
      providerContractDigest: contractDigest,
      observationKind: "dispatch-return" as const,
      observedAt: "2027-01-01T00:00:00.000Z",
      outcome: {
        kind: "terminal-success" as const,
        providerSessionRef: "prepared-recovery-session",
        providerSessionGeneration: 2,
        effectDigest: digest("prepared-recovery-effect"),
        resourceUsage: { provider_calls: 1 },
      },
    };
    const fixture = createFixture({
      dispatch: async () => outcome,
      retainedChairBridge: true,
      recoverChair: async () => { recoveryEffects += 1; throw new Error("must not dispatch"); },
    });
    const { handle } = await prepareFixture(fixture);
    await fixture.service.dispatchPrepared(handle);
    fixture.service.observeChairBridgeLoss({
      projectSessionId: "session_launch_01", runId: "run_launch_01", agentId: "chair_launch_01",
      principalGeneration: 1, adapterId: "claude-agent-sdk", actionId: "provider_launch_01",
      providerSessionRef: "prepared-recovery-session", providerSessionGeneration: 2,
      bridgeGeneration: 1, reason: "provider bridge closed",
    });
    const loss = fixture.database.prepare(`SELECT * FROM chair_bridge_losses`).get() as Record<string, unknown>;
    const session = fixture.database.prepare(`SELECT revision, generation FROM project_sessions`).get() as Record<string, unknown>;
    const run = fixture.database.prepare(`SELECT revision, chair_generation FROM runs`).get() as Record<string, unknown>;
    const bridge = fixture.database.prepare(`SELECT revision FROM launched_chair_bridge_state`).get() as Record<string, unknown>;
    const intent: ChairBridgeRecoveryIntent = {
      kind: "chair-bridge-recovery", schemaVersion: 1, path: "rebind",
      projectSessionId: "session_launch_01" as never, coordinationRunId: "run_launch_01" as never,
      lossId: String(loss.loss_id), recoveryManifestDigest: String(loss.recovery_manifest_digest) as Sha256Digest,
      expectedSessionRevision: Number(session.revision), expectedSessionGeneration: Number(session.generation),
      expectedRunRevision: Number(run.revision), expectedChairGeneration: Number(run.chair_generation),
      expectedPrincipalGeneration: 1, expectedBridgeRevision: Number(bridge.revision),
      expectedLostBridgeGeneration: 1, expectedProviderSessionGeneration: 2,
      providerAdapterId: "claude-agent-sdk", providerContractDigest: contractDigest,
      providerActionId: "prepared_recovery_action" as never,
    };
    const inspection = await fixture.service.inspectChairRecovery(intent);
    fixture.database.transaction(() => fixture.service.prepareChairRecoveryInTransaction({
      inspection, operatorId: "operator_01", operatorCommandId: "prepared_recovery_command",
      providerActionTicket: recoveryTicket(fixture, inspection),
    }))();
    const recovered = await fixture.service.recover();
    expect(recoveryEffects).toBe(0);
    expect(recovered).toMatchObject({ failed: expect.any(Number), recoveryRequired: expect.any(Number) });
    expect(fixture.database.prepare(`SELECT state FROM chair_bridge_recovery_custody`).get()).toEqual({ state: "no-effect" });
    expect(fixture.database.prepare(`SELECT state FROM launched_chair_bridge_state`).get()).toEqual({ state: "lost" });
    expect(fixture.database.prepare(`
      SELECT revoked_at FROM capabilities
       WHERE token_hash=(SELECT new_capability_hash FROM chair_bridge_recovery_custody)
    `).get()).toEqual({ revoked_at: now });
  });

  it("recovers an ambiguous rebind by pair-keyed lookup without a second provider effect", async () => {
    let recoveryEffects = 0;
    let lookups = 0;
    const outcome = {
      schemaVersion: 1,
      providerAdapterId: "claude-agent-sdk",
      providerActionId: "provider_launch_01",
      providerContractDigest: contractDigest,
      observationKind: "dispatch-return" as const,
      observedAt: "2027-01-01T00:00:00.000Z",
      outcome: {
        kind: "terminal-success" as const,
        providerSessionRef: "ambiguous-recovery-session",
        providerSessionGeneration: 2,
        effectDigest: digest("ambiguous-recovery-before"),
        resourceUsage: { provider_calls: 1 },
      },
    };
    const fixture = createFixture({
      dispatch: async () => outcome,
      retainedChairBridge: true,
      recoverChair: async () => {
        recoveryEffects += 1;
        throw new Error("connection dropped after provider acceptance");
      },
      lookupChairRecovery: async ({ actionId }) => {
        lookups += 1;
        const unsigned = {
          schemaVersion: 1 as const,
          kind: "provider-session-fabric-attestation" as const,
          method: "provider-session-random-challenge-v1" as const,
          bridgeContract: "agent-fabric-session-bridge-v1" as const,
          providerAdapterId: "claude-agent-sdk",
          providerActionId: actionId,
          providerContractDigest: contractDigest,
          providerSessionRef: "ambiguous-recovery-session",
          providerSessionGeneration: 3,
          providerTurnRef: "ambiguous-recovery-turn",
          challengeDigest: chairLaunchChallengeDigest(attestationChallenge),
          providerInvocationRef: "ambiguous-recovery-tool-call",
        };
        return {
          actionId,
          operation: "recover_chair",
          status: "terminal",
          executionCount: 1,
          effectCount: 1,
          result: {
            resumeReference: "ambiguous-recovery-session",
            providerSessionGeneration: 3,
            fabricContinuity: {
              ...unsigned,
              attestationDigest: chairLaunchAttestationDigest(unsigned),
            },
          },
        };
      },
    });
    const { handle } = await prepareFixture(fixture);
    await fixture.service.dispatchPrepared(handle);
    fixture.service.observeChairBridgeLoss({
      projectSessionId: "session_launch_01", runId: "run_launch_01", agentId: "chair_launch_01",
      principalGeneration: 1, adapterId: "claude-agent-sdk", actionId: "provider_launch_01",
      providerSessionRef: "ambiguous-recovery-session", providerSessionGeneration: 2,
      bridgeGeneration: 1, reason: "provider bridge closed",
    });
    const loss = fixture.database.prepare(`SELECT * FROM chair_bridge_losses`).get() as Record<string, unknown>;
    const session = fixture.database.prepare(`SELECT revision, generation FROM project_sessions`).get() as Record<string, unknown>;
    const run = fixture.database.prepare(`SELECT revision, chair_generation FROM runs`).get() as Record<string, unknown>;
    const bridge = fixture.database.prepare(`SELECT revision FROM launched_chair_bridge_state`).get() as Record<string, unknown>;
    const intent: ChairBridgeRecoveryIntent = {
      kind: "chair-bridge-recovery", schemaVersion: 1, path: "rebind",
      projectSessionId: "session_launch_01" as never, coordinationRunId: "run_launch_01" as never,
      lossId: String(loss.loss_id), recoveryManifestDigest: String(loss.recovery_manifest_digest) as Sha256Digest,
      expectedSessionRevision: Number(session.revision), expectedSessionGeneration: Number(session.generation),
      expectedRunRevision: Number(run.revision), expectedChairGeneration: Number(run.chair_generation),
      expectedPrincipalGeneration: 1, expectedBridgeRevision: Number(bridge.revision),
      expectedLostBridgeGeneration: 1, expectedProviderSessionGeneration: 2,
      providerAdapterId: "claude-agent-sdk", providerContractDigest: contractDigest,
      providerActionId: "ambiguous_recovery_action" as never,
    };
    const inspection = await fixture.service.inspectChairRecovery(intent);
    const recovery = fixture.database.transaction(() => fixture.service.prepareChairRecoveryInTransaction({
      inspection, operatorId: "operator_01", operatorCommandId: "ambiguous_recovery_command",
      providerActionTicket: recoveryTicket(fixture, inspection),
    }))();
    await expect(fixture.service.dispatchPreparedChairRecovery(recovery)).resolves.toMatchObject({ status: "ambiguous" });
    expect(probeRecoveryCapabilityIdentityGuards(fixture.database)).toEqual({
      identityUpdateBlocked: true,
      deleteBlocked: true,
      mutableMetadataAllowed: true,
      canonicalRevocationAllowed: false,
    });
    await fixture.service.recover();
    expect(recoveryEffects).toBe(1);
    expect(lookups).toBe(1);
    expect(fixture.database.prepare(`SELECT state FROM chair_bridge_recovery_custody`).get()).toEqual({ state: "terminal" });
    expect(fixture.database.prepare(`
      SELECT status, effect_count, idempotency_proven FROM provider_actions
       WHERE action_id='ambiguous_recovery_action'
    `).get()).toEqual({ status: "terminal", effect_count: 1, idempotency_proven: 1 });
    expect(fixture.database.prepare(`SELECT state, provider_session_generation FROM launched_chair_bridge_state`).get())
      .toEqual({ state: "active", provider_session_generation: 3 });
  });

  it("keeps unresolved chair rebind custody out of generic startup recovery", async () => {
    const outcome = {
      schemaVersion: 1,
      providerAdapterId: "claude-agent-sdk",
      providerActionId: "provider_launch_01",
      providerContractDigest: contractDigest,
      observationKind: "dispatch-return" as const,
      observedAt: "2027-01-01T00:00:00.000Z",
      outcome: {
        kind: "terminal-success" as const,
        providerSessionRef: "startup-unresolved-rebind-session",
        providerSessionGeneration: 2,
        effectDigest: digest("startup-unresolved-rebind-effect"),
        resourceUsage: { provider_calls: 1 },
      },
    };
    const fixture = createFixture({
      dispatch: async () => outcome,
      retainedChairBridge: true,
      persistent: true,
      recoverChair: async () => { throw new Error("connection dropped after provider acceptance"); },
    });
    const { handle } = await prepareFixture(fixture);
    await fixture.service.dispatchPrepared(handle);
    fixture.service.observeChairBridgeLoss({
      projectSessionId: "session_launch_01", runId: "run_launch_01", agentId: "chair_launch_01",
      principalGeneration: 1, adapterId: "claude-agent-sdk", actionId: "provider_launch_01",
      providerSessionRef: "startup-unresolved-rebind-session", providerSessionGeneration: 2,
      bridgeGeneration: 1, reason: "provider bridge closed",
    });
    const loss = fixture.database.prepare(`SELECT * FROM chair_bridge_losses`).get() as Record<string, unknown>;
    const session = fixture.database.prepare(`SELECT revision, generation FROM project_sessions`).get() as Record<string, unknown>;
    const run = fixture.database.prepare(`SELECT revision, chair_generation FROM runs`).get() as Record<string, unknown>;
    const bridge = fixture.database.prepare(`SELECT revision FROM launched_chair_bridge_state`).get() as Record<string, unknown>;
    const intent: ChairBridgeRecoveryIntent = {
      kind: "chair-bridge-recovery", schemaVersion: 1, path: "rebind",
      projectSessionId: "session_launch_01" as never, coordinationRunId: "run_launch_01" as never,
      lossId: String(loss.loss_id), recoveryManifestDigest: String(loss.recovery_manifest_digest) as Sha256Digest,
      expectedSessionRevision: Number(session.revision), expectedSessionGeneration: Number(session.generation),
      expectedRunRevision: Number(run.revision), expectedChairGeneration: Number(run.chair_generation),
      expectedPrincipalGeneration: 1, expectedBridgeRevision: Number(bridge.revision),
      expectedLostBridgeGeneration: 1, expectedProviderSessionGeneration: 2,
      providerAdapterId: "claude-agent-sdk", providerContractDigest: contractDigest,
      providerActionId: "startup_unresolved_rebind_action" as never,
    };
    const inspection = await fixture.service.inspectChairRecovery(intent);
    const recovery = fixture.database.transaction(() => fixture.service.prepareChairRecoveryInTransaction({
      inspection, operatorId: "operator_01", operatorCommandId: "startup_unresolved_rebind_command",
      providerActionTicket: recoveryTicket(fixture, inspection),
    }))();
    await expect(fixture.service.dispatchPreparedChairRecovery(recovery)).resolves.toMatchObject({ status: "ambiguous" });
    closeTrackedDatabase(fixture.database);

    const callsPath = join(fixture.root, "unresolved-rebind-calls.jsonl");
    const runtime = await openFabric({
      databasePath: fixture.databasePath,
      workspaceRoots: [fixture.root],
      fabricSocketPath: join(fixture.root, "fabric.sock"),
      adapters: {
        "claude-agent-sdk": {
          command: [process.execPath, "--import", "tsx", recoveryAdapter],
          environment: {
            LAUNCH_RECOVERY_CALLS_PATH: callsPath,
            LAUNCH_RECOVERY_CONTRACT_JSON: canonicalJson(contract),
          },
        },
      },
    });
    let startup: Awaited<ReturnType<typeof runtime.recoverStartupState>> | undefined;
    let directError: unknown;
    try {
      startup = await runtime.recoverStartupState();
      try {
        await runtime.reconcileProviderAction("run_launch_01", "chair_launch_01", {
          adapterId: "claude-agent-sdk",
          actionId: "startup_unresolved_rebind_action",
          commandId: "generic_rebind_reconcile_forbidden_01",
        });
      } catch (error: unknown) {
        directError = error;
      }
    } finally {
      await runtime.close();
    }
    expect(startup).toMatchObject({ actionsQuarantined: 0 });
    expect(directError).toMatchObject({ code: "CAPABILITY_FORBIDDEN" });
    const calls = readFileSync(callsPath, "utf8").trim().split("\n")
      .map((line) => JSON.parse(line) as { method: string });
    expect(calls.filter(({ method }) => method === "lookup_action")).toHaveLength(1);
    const state = new Database(fixture.databasePath, { readonly: true });
    expect(state.prepare(`
      SELECT c.state, p.status, p.execution_count, p.effect_count
        FROM chair_bridge_recovery_custody c
        JOIN provider_actions p
          ON p.adapter_id=c.provider_adapter_id AND p.action_id=c.provider_action_id
       WHERE c.operator_command_id='startup_unresolved_rebind_command'
    `).get()).toEqual({ state: "ambiguous", status: "ambiguous", execution_count: 1, effect_count: 0 });
    state.close();
  });

  it("fences an activated launched chair on restart without a retained bridge or generic status call", async () => {
    const outcome = {
      schemaVersion: 1,
      providerAdapterId: "claude-agent-sdk",
      providerActionId: "provider_launch_01",
      providerContractDigest: contractDigest,
      observationKind: "dispatch-return",
      observedAt: "2027-01-01T00:00:00.000Z",
      outcome: {
        kind: "terminal-success",
        providerSessionRef: "claude-chair-restart-loss-01",
        providerSessionGeneration: 4,
        effectDigest: digest("provider-restart-loss-effect"),
        resourceUsage: { provider_calls: 1 },
      },
    };
    const fixture = createFixture({ dispatch: async () => outcome, retainedChairBridge: true, persistent: true });
    const { handle } = await prepareFixture(fixture);
    await fixture.service.dispatchPrepared(handle);
    closeTrackedDatabase(fixture.database);
    const callsPath = join(fixture.root, "chair-restart-calls.jsonl");
    const runtime = await openFabric({
      databasePath: fixture.databasePath,
      workspaceRoots: [fixture.root],
      fabricSocketPath: join(fixture.root, "fabric.sock"),
      adapters: {
        "claude-agent-sdk": {
          command: [process.execPath, "--import", "tsx", recoveryAdapter],
          environment: {
            LAUNCH_RECOVERY_CALLS_PATH: callsPath,
            LAUNCH_RECOVERY_CONTRACT_JSON: canonicalJson(contract),
          },
        },
      },
    });

    await runtime.recoverStartupState();
    await runtime.recoverStartupState();
    await runtime.close();
    expect(() => readFileSync(callsPath, "utf8")).toThrow();
    const state = new Database(fixture.databasePath, { readonly: true });
    expect(state.prepare(`
      SELECT COUNT(*) AS count FROM chair_bridge_losses WHERE coordination_run_id='run_launch_01'
    `).get()).toEqual({ count: 1 });
    expect(state.prepare(`
      SELECT s.state, r.lifecycle_state, b.state AS bridge_state
        FROM project_sessions s
        JOIN runs r ON r.project_session_id=s.project_session_id
        JOIN launched_chair_bridge_state b ON b.coordination_run_id=r.run_id
       WHERE r.run_id='run_launch_01'
    `).get()).toEqual({
      state: "recovery_required",
      lifecycle_state: "recovery_required",
      bridge_state: "lost",
    });
    state.close();
  });

  it("persists dispatched before I/O and recovers by lookup-only terminal success", async () => {
    const fixture = createFixture();
    const { handle } = await prepareFixture(fixture);
    await expect(fixture.service.dispatchPrepared(handle)).resolves.toMatchObject({
      outcome: { kind: "ambiguous", reasonCode: "adapter-error" },
    });
    expect(fixture.database.prepare(`
      SELECT status, execution_count FROM provider_actions
       WHERE adapter_id='claude-agent-sdk' AND action_id='provider_launch_01'
    `).get()).toEqual({ status: "ambiguous", execution_count: 1 });

    const lookupOutcome = {
      schemaVersion: 1,
      providerAdapterId: "claude-agent-sdk",
      providerActionId: "provider_launch_01",
      providerContractDigest: contractDigest,
      observationKind: "lookup",
      observedAt: "2027-01-01T00:00:00.000Z",
      outcome: {
        kind: "terminal-success",
        providerSessionRef: "claude-session-launch-01",
        providerSessionGeneration: 1,
        effectDigest: digest("provider-launch-effect"),
        resourceUsage: { provider_calls: 1 },
      },
    };
    const restarted = buildLaunchCustodyService({
      database: fixture.database,
      providerActionAdmission: fixture.providerActionAdmission,
      clock: () => now,
      randomCapability: () => "unused-restart-secret",
      fabricSocketPath: "/private/agent-fabric.sock",
      adapterContracts: { inspect: async () => contract },
      adapterEffects: {
        dispatch: async () => { throw new Error("restart must not dispatch"); },
        lookup: async () => lookupOutcome,
      },
    });
    await expect(restarted.recover()).resolves.toEqual({
      preparedFailed: 0,
      lookedUp: 1,
      activated: 1,
      failed: 0,
      ambiguous: 0,
      recoveryRequired: 0,
    });
    expect(fixture.database.prepare(`
      SELECT state FROM project_sessions WHERE project_session_id='session_launch_01'
    `).get()).toEqual({ state: "active" });
    expect(fixture.database.prepare(`
      SELECT provider_session_ref FROM agents WHERE run_id='run_launch_01' AND agent_id='chair_launch_01'
    `).get()).toEqual({ provider_session_ref: "claude-session-launch-01" });
    expect(fixture.database.prepare(`
      SELECT state FROM resource_reservations
       WHERE reservation_id=(SELECT reservation_id FROM project_session_launch_custody)
    `).get()).toEqual({ state: "reconciled" });
    expect(fixture.database.prepare(`
      SELECT used, reserved FROM resource_dimensions WHERE scope_id='scope_run_launch_01' AND unit_key='provider_calls'
    `).get()).toEqual({ used: 1, reserved: 0 });
  });

  it("marks unknown launch usage at every ancestor without restoring unproved capacity", async () => {
    const outcome = {
      schemaVersion: 1,
      providerAdapterId: "claude-agent-sdk",
      providerActionId: "provider_launch_01",
      providerContractDigest: contractDigest,
      observationKind: "dispatch-return",
      observedAt: "2027-01-01T00:00:00.000Z",
      outcome: {
        kind: "terminal-success",
        providerSessionRef: "claude-session-unknown-01",
        providerSessionGeneration: 1,
        effectDigest: digest("provider-unknown-effect"),
        resourceUsage: { provider_calls: "unknown" },
      },
    };
    const fixture = createFixture({ dispatch: async () => outcome });
    const { handle } = await prepareFixture(fixture);
    await fixture.service.dispatchPrepared(handle);

    expect(fixture.database.prepare(`
      SELECT COUNT(*) AS count FROM resource_dimensions
       WHERE unit_key='provider_calls' AND usage_unknown=1 AND reserved=0
    `).get()).toEqual({ count: 3 });
    expect(fixture.database.prepare(`
      SELECT state FROM resource_reservations
       WHERE reservation_id=(SELECT reservation_id FROM project_session_launch_custody)
    `).get()).toEqual({ state: "reconciled" });

    const reservation = fixture.database.prepare(`
      SELECT reservation.reservation_id, reservation.revision
        FROM resource_reservations reservation
        JOIN project_session_launch_custody custody
          ON custody.reservation_id=reservation.reservation_id
    `).get() as { reservation_id: string; revision: number };
    expect(fixture.service.reconcileUnknownLaunchUsage({
      projectId: "project_01",
      projectSessionId: "session_launch_01",
      coordinationRunId: "run_launch_01",
      providerAdapterId: "claude-agent-sdk",
      providerActionId: "provider_launch_01",
      reservationId: reservation.reservation_id,
      expectedReservationRevision: reservation.revision,
      observedUsage: { provider_calls: 1 },
      evidenceDigest: digest("exact authenticated launch usage"),
    })).toEqual({
      reservationId: reservation.reservation_id,
      revision: reservation.revision + 1,
      reconciledUsage: { provider_calls: 1 },
    });
    expect(fixture.database.prepare(`
      SELECT COUNT(*) AS count FROM resource_dimensions
       WHERE unit_key='provider_calls' AND usage_unknown=1
    `).get()).toEqual({ count: 0 });
    expect(fixture.database.prepare(`
      SELECT used, reserved, usage_unknown FROM resource_dimensions
       WHERE scope_id='scope_project_01' AND unit_key='provider_calls'
    `).get()).toEqual({ used: 1, reserved: 0, usage_unknown: 0 });
  });

  it("enters recovery-required on exact launch overrun without truncating the reservation", async () => {
    const outcome = {
      schemaVersion: 1,
      providerAdapterId: "claude-agent-sdk",
      providerActionId: "provider_launch_01",
      providerContractDigest: contractDigest,
      observationKind: "dispatch-return",
      observedAt: "2027-01-01T00:00:00.000Z",
      outcome: {
        kind: "terminal-success",
        providerSessionRef: "claude-session-overrun-01",
        providerSessionGeneration: 1,
        effectDigest: digest("provider-overrun-effect"),
        resourceUsage: { provider_calls: 2 },
      },
    };
    const fixture = createFixture({ dispatch: async () => outcome });
    const { handle } = await prepareFixture(fixture);
    await fixture.service.dispatchPrepared(handle);

    expect(fixture.database.prepare(`
      SELECT state FROM project_sessions WHERE project_session_id='session_launch_01'
    `).get()).toEqual({ state: "recovery_required" });
    expect(fixture.database.prepare(`
      SELECT state FROM resource_reservations
       WHERE reservation_id=(SELECT reservation_id FROM project_session_launch_custody)
    `).get()).toEqual({ state: "reserved" });
    expect(fixture.database.prepare(`
      SELECT status FROM run_chair_leases WHERE project_session_id='session_launch_01'
    `).get()).toEqual({ status: "frozen" });
    expect(fixture.database.prepare(`
      SELECT reserved, used FROM resource_dimensions
       WHERE scope_id='scope_run_launch_01' AND unit_key='provider_calls'
    `).get()).toEqual({ reserved: 1, used: 0 });
  });

  it("accepts only a registered, digest-valid no-effect proof and cleans the failed attempt", async () => {
    const proof = { effectCount: 0 };
    const outcome = {
      schemaVersion: 1,
      providerAdapterId: "claude-agent-sdk",
      providerActionId: "provider_launch_01",
      providerContractDigest: contractDigest,
      observationKind: "dispatch-return",
      observedAt: "2027-01-01T00:00:00.000Z",
      outcome: {
        kind: "terminal-no-effect",
        failureCode: "provider-rejected",
        noEffectProof: {
          schemaId: "provider-no-effect.v1",
          proof,
          digest: digest(canonicalJson(proof)),
        },
      },
    };
    const fixture = createFixture({ dispatch: async () => outcome });
    const { handle } = await prepareFixture(fixture);
    await expect(fixture.service.dispatchPrepared(handle)).resolves.toEqual(outcome);
    expect(fixture.database.prepare(`
      SELECT state FROM project_sessions WHERE project_session_id='session_launch_01'
    `).get()).toEqual({ state: "launch_failed" });
    expect(fixture.database.prepare(`
      SELECT state FROM resource_reservations
       WHERE reservation_id=(SELECT reservation_id FROM project_session_launch_custody)
    `).get()).toEqual({ state: "released" });
  });

  it("normalises incomplete acceptance to ambiguity and never replays the one-use handoff", async () => {
    const fixture = createFixture({ dispatch: async () => ({ status: "accepted" }) });
    const { handle } = await prepareFixture(fixture);
    await expect(fixture.service.dispatchPrepared(handle)).resolves.toMatchObject({
      outcome: { kind: "ambiguous", reasonCode: "malformed" },
    });
    expect(fixture.database.prepare(`
      SELECT state FROM project_sessions WHERE project_session_id='session_launch_01'
    `).get()).toEqual({ state: "launch_ambiguous" });
    expect(fixture.database.prepare(`
      SELECT state FROM resource_reservations
       WHERE reservation_id=(SELECT reservation_id FROM project_session_launch_custody)
    `).get()).toEqual({ state: "reserved" });
    await expect(fixture.service.dispatchPrepared(handle)).rejects.toMatchObject({ code: "DEDUPE_CONFLICT" });
  });

  it("routes startup recovery through launch custody without generic redispatch", async () => {
    const prepared = createFixture({ persistent: true });
    await prepareFixture(prepared);
    closeTrackedDatabase(prepared.database);

    const preparedRuntime = await openFabric({
      databasePath: prepared.databasePath,
      workspaceRoots: [prepared.root],
      fabricSocketPath: join(prepared.root, "fabric.sock"),
    });
    await expect(preparedRuntime.reconcileProviderAction("run_launch_01", "chair_launch_01", {
      adapterId: "claude-agent-sdk",
      actionId: "provider_launch_01",
      commandId: "generic_launch_reconcile_forbidden_01",
    })).rejects.toMatchObject({ code: "CAPABILITY_FORBIDDEN" });
    await preparedRuntime.recoverStartupState();
    await preparedRuntime.close();
    const preparedState = new Database(prepared.databasePath, { readonly: true });
    expect(preparedState.prepare(`
      SELECT s.state, p.status, p.execution_count
        FROM project_sessions s
        JOIN project_session_launch_custody c ON c.project_session_id=s.project_session_id
        JOIN provider_actions p
          ON p.adapter_id=c.provider_adapter_id AND p.action_id=c.provider_action_id
    `).get()).toEqual({ state: "launch_failed", status: "terminal", execution_count: 0 });
    expect(preparedState.prepare(`
      SELECT run_id, state FROM provider_action_pair_preflights
       WHERE adapter_id='claude-agent-sdk' AND action_id='provider_launch_01'
    `).get()).toEqual({ run_id: "run_launch_01", state: "admitted" });
    expect(preparedState.pragma("foreign_key_check")).toEqual([]);
    preparedState.close();

    const dispatched = createFixture({ persistent: true });
    await prepareFixture(dispatched);
    dispatched.database.prepare(`
      UPDATE provider_actions
         SET status='dispatched', history_json='["prepared","dispatched"]', execution_count=1
    `).run();
    closeTrackedDatabase(dispatched.database);
    const callsPath = join(dispatched.root, "adapter-calls.jsonl");
    const dispatchedRuntime = await openFabric({
      databasePath: dispatched.databasePath,
      workspaceRoots: [dispatched.root],
      fabricSocketPath: join(dispatched.root, "fabric.sock"),
      adapters: {
        "claude-agent-sdk": {
          command: [process.execPath, "--import", "tsx", recoveryAdapter],
          environment: {
            LAUNCH_RECOVERY_CALLS_PATH: callsPath,
            LAUNCH_RECOVERY_CONTRACT_JSON: canonicalJson(contract),
          },
        },
      },
    });
    await dispatchedRuntime.recoverStartupState();
    await dispatchedRuntime.close();
    const calls = readFileSync(callsPath, "utf8").trim().split("\n").map((line) => JSON.parse(line) as { method: string });
    expect(calls.map(({ method }) => method)).toEqual(["capabilities", "lookup_action"]);
    const dispatchedState = new Database(dispatched.databasePath, { readonly: true });
    expect(dispatchedState.prepare(`
      SELECT s.state, p.status, p.execution_count
        FROM project_sessions s
        JOIN project_session_launch_custody c ON c.project_session_id=s.project_session_id
        JOIN provider_actions p
          ON p.adapter_id=c.provider_adapter_id AND p.action_id=c.provider_action_id
    `).get()).toEqual({ state: "launch_ambiguous", status: "ambiguous", execution_count: 1 });
    dispatchedState.close();
  });

  it("retries only the exact proved failure with a fresh packet, run and provider action", async () => {
    const fixture = createFixture();
    await prepareFixture(fixture);
    await fixture.service.recover();

    const plan = JSON.parse(readFileSync(join(fixture.root, "launch-resources.json"), "utf8")) as Record<string, unknown>;
    plan.runId = "run_launch_02";
    const scopes = plan.scopes as Record<string, Record<string, unknown>>;
    if (scopes.coordinationRun === undefined) throw new Error("retry plan has no coordination run scope");
    scopes.coordinationRun.scopeId = "scope_run_launch_02";
    const planText = canonicalJson(plan);
    writeFileSync(join(fixture.root, "launch-resources-02.json"), planText, { mode: 0o600 });
    const planRef = parseArtifactRef({ path: "launch-resources-02.json", digest: digest(planText) }, "test.retryPlanRef");

    const packet = JSON.parse(readFileSync(join(fixture.root, "launch-packet.json"), "utf8")) as Record<string, unknown>;
    packet.runId = "run_launch_02";
    packet.chairAgentId = "chair_launch_02";
    packet.projectRunDirectory = ".agent-run/AFAB-LAUNCH-RETRY";
    packet.resourcePlanRef = planRef;
    const provider = packet.provider as Record<string, unknown>;
    provider.actionId = "provider_launch_02";
    const packetText = canonicalJson(packet);
    writeFileSync(join(fixture.root, "launch-packet-02.json"), packetText, { mode: 0o600 });
    const packetRef = parseArtifactRef({ path: "launch-packet-02.json", digest: digest(packetText) }, "test.retryPacketRef");
    const retryIntent = parseProjectSessionLaunchIntent({
      ...fixture.intent,
      expectedSessionRevision: 4,
      launchPacketRef: packetRef,
      resourcePlanRef: planRef,
      providerActionId: "provider_launch_02",
      resourceStateDigest: computeLaunchResourceStateDigest(fixture.database, "project_01", "session_launch_01"),
      retryOf: {
        providerAdapterId: "claude-agent-sdk",
        providerActionId: "provider_launch_01",
      },
    });
    await expect(fixture.service.inspect(parseProjectSessionLaunchIntent({
      ...retryIntent,
      retryOf: {
        providerAdapterId: "claude-agent-sdk",
        providerActionId: "another_failed_action",
      },
    }))).rejects.toMatchObject({ code: "CONFLICT" });

    const inspection = await fixture.service.inspect(retryIntent);
    fixture.database.transaction(() => {
      fixture.database.prepare(`
        INSERT INTO operator_commands(
          operator_id, command_id, capability_id, project_id, project_session_id,
          operation, expected_revision, payload_hash, provenance_json, before_json,
          after_json, evidence_json, result_json, status, created_at
        ) VALUES (
          'operator_01', 'commit_launch_02', 'operator_cap_launch_01', 'project_01',
          'session_launch_01', 'launch', 4, 'payload-hash-02', '{}', '{}', '{}', '[]', '{}',
          'committed', ${now}
        )
      `).run();
      fixture.service.prepareInTransaction({
        inspection,
        operatorId: "operator_01",
        operatorCommandId: "commit_launch_02",
        principal: operatorPrincipal(),
      });
    }).immediate();
    expect(fixture.database.prepare(`
      SELECT state, revision, launch_packet_path FROM project_sessions
       WHERE project_session_id='session_launch_01'
    `).get()).toEqual({ state: "launching", revision: 5, launch_packet_path: "launch-packet-02.json" });
    expect(fixture.database.prepare(`
      SELECT COUNT(*) AS count FROM project_session_launch_custody
    `).get()).toEqual({ count: 2 });
    expect(fixture.database.prepare(`
      SELECT retry_of_provider_action_id FROM project_session_launch_custody
       WHERE custody_attempt_generation=2
    `).get()).toEqual({ retry_of_provider_action_id: "provider_launch_01" });
    expect(fixture.database.prepare(`
      SELECT run_id, state FROM provider_action_pair_preflights
       ORDER BY action_id
    `).all()).toEqual([
      { run_id: "run_launch_01", state: "admitted" },
      { run_id: "run_launch_02", state: "admitted" },
    ]);
    expect(fixture.database.pragma("foreign_key_check")).toEqual([]);
  });

  it("rejects extra artifact fields and symlink escape before any launch row", async () => {
    const fixture = createFixture();
    const packetPath = join(fixture.root, fixture.intent.launchPacketRef.path);
    const packet = JSON.parse(await import("node:fs/promises").then(({ readFile }) => readFile(packetPath, "utf8"))) as Record<string, unknown>;
    packet.unreviewed = true;
    const changed = canonicalJson(packet);
    writeFileSync(packetPath, changed, { mode: 0o600 });
    fixture.database.prepare(`
      UPDATE project_sessions SET launch_packet_digest=?, revision=revision+1
       WHERE project_session_id='session_launch_01'
    `).run(digest(changed));
    await expect(fixture.service.inspect({
      ...fixture.intent,
      expectedSessionRevision: 3,
      launchPacketRef: { ...fixture.intent.launchPacketRef, digest: digest(changed) },
    })).rejects.toMatchObject({ code: "PROTOCOL_INVALID" });

    const outside = join(fixture.root, "outside-packet.json");
    writeFileSync(outside, "{}", { mode: 0o600 });
    symlinkSync(outside, join(fixture.root, "linked-packet.json"));
    fixture.database.prepare(`
      UPDATE project_sessions
         SET launch_packet_path='linked-packet.json', launch_packet_digest=?, revision=revision+1
       WHERE project_session_id='session_launch_01'
    `).run(digest("{}"));
    await expect(fixture.service.inspect({
      ...fixture.intent,
      expectedSessionRevision: 4,
      launchPacketRef: parseArtifactRef({ path: "linked-packet.json", digest: digest("{}") }, "test.linkedPacketRef"),
    })).rejects.toMatchObject({ code: "CAPABILITY_FORBIDDEN" });
    expect(fixture.database.prepare("SELECT COUNT(*) AS count FROM runs").get()).toEqual({ count: 0 });
  });

  it("rejects every stale live-handoff generation, authority, provider and artifact binding", async () => {
    const { fixture, seeded } = await prepareLiveHandoffFixture();
    const cases: ReadonlyArray<readonly [string, ChairLiveHandoffIntent, string]> = [
      ["session generation", { ...seeded.intent, expectedSessionGeneration: seeded.intent.expectedSessionGeneration + 1 }, "STALE_REVISION"],
      ["chair generation", { ...seeded.intent, expectedChairGeneration: seeded.intent.expectedChairGeneration + 1 }, "STALE_REVISION"],
      ["successor bridge revision", { ...seeded.intent, expectedSuccessorBridgeRevision: seeded.intent.expectedSuccessorBridgeRevision + 1 }, "STALE_REVISION"],
      ["predecessor principal", { ...seeded.intent, expectedPredecessorPrincipalGeneration: seeded.intent.expectedPredecessorPrincipalGeneration + 1 }, "STALE_REVISION"],
      ["successor authority", { ...seeded.intent, successorAuthorityId: "authority_wrong" }, "STALE_REVISION"],
      ["successor authority digest", { ...seeded.intent, successorAuthorityDigest: digest("wrong-successor-authority") }, "STALE_REVISION"],
      ["provider adapter", { ...seeded.intent, providerAdapterId: "wrong-adapter" }, "STALE_REVISION"],
      ["provider contract", { ...seeded.intent, providerContractDigest: digest("wrong-provider-contract") }, "STALE_REVISION"],
      ["handoff artifact", {
        ...seeded.intent,
        handoffRef: { ...seeded.intent.handoffRef, digest: digest("wrong-handoff-artifact") },
      }, "ARTIFACT_DIGEST_INVALID"],
    ];
    for (const [_label, intent, code] of cases) {
      await expect(fixture.service.inspectChairLiveHandoff(intent)).rejects.toMatchObject({ code });
    }
    expect(fixture.database.prepare("SELECT COUNT(*) AS count FROM chair_live_handoff_custody").get())
      .toEqual({ count: 0 });
  });

  it("rolls back failed prepare and commit transactions, then reconciles commit by lookup", async () => {
    let prepareFaultArmed = true;
    const prepared = await prepareLiveHandoffFixture({
      fault: (label) => {
        if (label === "chair-live-handoff:prepared" && prepareFaultArmed) {
          prepareFaultArmed = false;
          throw new Error("prepare crash");
        }
      },
    });
    const preparedTicket = handoffTicket(
      prepared.fixture,
      prepared.inspection,
      "commit_live_handoff_prepare_crash_01",
    );
    expect(() => prepared.fixture.database.transaction(() => {
      prepared.fixture.service.prepareChairLiveHandoffInTransaction({
        inspection: prepared.inspection,
        operatorId: "operator_01",
        operatorCommandId: "commit_live_handoff_prepare_crash_01",
        providerActionTicket: preparedTicket,
      });
    }).immediate()).toThrow("prepare crash");
    expect(prepared.fixture.database.prepare(`
      SELECT (SELECT COUNT(*) FROM chair_live_handoff_custody) AS custody,
             (SELECT COUNT(*) FROM provider_actions WHERE operation='promote_retained_bridge') AS actions,
             (SELECT COUNT(*) FROM delivery_freezes WHERE run_id='run_launch_01') AS freezes
    `).get()).toEqual({ custody: 0, actions: 0, freezes: 0 });
    expect(prepared.fixture.database.prepare(`
      SELECT lifecycle_state, revision FROM runs WHERE run_id='run_launch_01'
    `).get()).toEqual({ lifecycle_state: "active", revision: prepared.seeded.intent.expectedRunRevision });
    expect(prepared.fixture.database.prepare("SELECT status FROM run_chair_leases WHERE lease_id=?")
      .get(prepared.seeded.intent.expectedChairLeaseId)).toEqual({ status: "active" });
    expect(prepared.fixture.database.transaction(() => (
      prepared.fixture.service.prepareChairLiveHandoffInTransaction({
        inspection: prepared.inspection,
        operatorId: "operator_01",
        operatorCommandId: "commit_live_handoff_prepare_crash_01",
        providerActionTicket: preparedTicket,
      })
    )).immediate()).toMatchObject({ promotionActionId: preparedTicket.actionRef.actionId });
    expect(prepared.fixture.database.prepare(`
      SELECT state FROM provider_action_pair_preflights
       WHERE adapter_id=? AND action_id=?
    `).get(preparedTicket.actionRef.adapterId, preparedTicket.actionRef.actionId))
      .toEqual({ state: "admitted" });

    let promotions = 0;
    const committing = await prepareLiveHandoffFixture({
      fault: (label) => {
        if (label === "chair-live-handoff:committing") throw new Error("commit crash");
      },
      promoteSuccessor: async () => { promotions += 1; return true; },
    });
    const handoff = committing.fixture.database.transaction(() => (
      committing.fixture.service.prepareChairLiveHandoffInTransaction({
        inspection: committing.inspection,
        operatorId: "operator_01",
        operatorCommandId: "commit_live_handoff_commit_crash_01",
        providerActionTicket: handoffTicket(
          committing.fixture,
          committing.inspection,
          "commit_live_handoff_commit_crash_01",
        ),
      })
    )).immediate();
    await expect(committing.fixture.service.dispatchPreparedChairLiveHandoff(handoff))
      .rejects.toThrow("commit crash");
    expect(promotions).toBe(1);
    expect(committing.fixture.database.prepare(`
      SELECT state FROM chair_live_handoff_custody WHERE custody_id=?
    `).get(handoff.custodyId)).toEqual({ state: "dispatched" });
    expect(committing.fixture.database.prepare(`
      SELECT chair_agent_id, bridge_generation FROM launched_chair_bridge_state
    `).get()).toEqual({
      chair_agent_id: "chair_launch_01",
      bridge_generation: committing.seeded.intent.expectedChairBridgeGeneration,
    });
    let restartPromotions = 0;
    let restartLookups = 0;
    const restarted = restartedLiveHandoffService(committing.fixture, {
      promote: async () => { restartPromotions += 1; throw new Error("restart promoted"); },
      lookup: async () => { restartLookups += 1; return "chair"; },
    });
    await expect(restarted.reconcileChairLiveHandoff(
      "operator_01",
      "commit_live_handoff_commit_crash_01",
    )).resolves.toMatchObject({ status: "committed" });
    expect({ restartPromotions, restartLookups }).toEqual({ restartPromotions: 0, restartLookups: 1 });
  });

  it("promotes one retained child through distinct live-handoff custody and retires predecessor authority", async () => {
    let promotions = 0;
    let retiredPredecessor = 0;
    const outcome = {
      schemaVersion: 1,
      providerAdapterId: "claude-agent-sdk",
      providerActionId: "provider_launch_01",
      providerContractDigest: contractDigest,
      observationKind: "dispatch-return" as const,
      observedAt: "2027-01-01T00:00:00.000Z",
      outcome: {
        kind: "terminal-success" as const,
        providerSessionRef: "claude-chair-live-old",
        providerSessionGeneration: 2,
        effectDigest: digest("provider-live-handoff-predecessor"),
        resourceUsage: { provider_calls: 1 },
      },
    };
    const fixture = createFixture({
      dispatch: async () => outcome,
      retainedChairBridge: true,
      promoteSuccessor: async (input) => {
        promotions += 1;
        expect(input).toMatchObject({
          agentId: "successor_live_01",
          sourceActionId: "successor_live_action_01",
          sourceBridgeGeneration: 1,
          chairBridgeGeneration: 2,
        });
        expect(String(input.promotionActionId)).not.toBe(String(input.sourceActionId));
        expect(fixture.database.prepare(`
          SELECT state FROM chair_live_handoff_custody
        `).get()).toEqual({ state: "dispatched" });
        return true;
      },
      retireChair: (entry) => {
        retiredPredecessor += 1;
        expect(entry).toMatchObject({ agentId: "chair_launch_01", providerSessionRef: "claude-chair-live-old" });
      },
    });
    const { handle: launchHandle } = await prepareFixture(fixture);
    await fixture.service.dispatchPrepared(launchHandle);
    const seeded = seedLiveHandoffSuccessor(fixture);
    const inspection = await fixture.service.inspectChairLiveHandoff(seeded.intent);
    let handoff = fixture.database.transaction(() => (
      fixture.service.prepareChairLiveHandoffInTransaction({
        inspection,
        operatorId: "operator_01",
        operatorCommandId: "commit_live_handoff_01",
        providerActionTicket: handoffTicket(fixture, inspection, "commit_live_handoff_01"),
      })
    )).immediate();
    expect(promotions).toBe(0);
    expect(fixture.database.prepare(`
      SELECT lifecycle_state, revision FROM runs WHERE run_id='run_launch_01'
    `).get()).toEqual({
      lifecycle_state: "reconciling",
      revision: seeded.intent.expectedRunRevision + 1,
    });
    expect(fixture.database.prepare(`
      SELECT status FROM run_chair_leases WHERE lease_id=?
    `).get(seeded.intent.expectedChairLeaseId)).toEqual({ status: "frozen" });

    await expect(fixture.service.dispatchPreparedChairLiveHandoff(handoff)).resolves.toMatchObject({
      status: "committed",
    });
    expect(promotions).toBe(1);
    expect(fixture.database.prepare(`
      SELECT chair_agent_id, chair_generation, lifecycle_state, revision
        FROM runs WHERE run_id='run_launch_01'
    `).get()).toEqual({
      chair_agent_id: "successor_live_01",
      chair_generation: seeded.intent.expectedChairGeneration + 1,
      lifecycle_state: "active",
      revision: seeded.intent.expectedRunRevision + 2,
    });
    expect(fixture.database.prepare(`
      SELECT generation, revision, membership_revision FROM project_sessions
       WHERE project_session_id='session_launch_01'
    `).get()).toEqual({
      generation: seeded.intent.expectedSessionGeneration + 1,
      revision: seeded.intent.expectedSessionRevision + 1,
      membership_revision: seeded.intent.expectedMembershipRevision + 1,
    });
    expect(fixture.database.prepare(`
      SELECT chair_agent_id, provider_action_id, bridge_generation, state
        FROM launched_chair_bridge_state
    `).get()).toMatchObject({
      chair_agent_id: "successor_live_01",
      bridge_generation: 2,
      state: "active",
    });
    expect(fixture.database.prepare(`
      SELECT bridge_state, capability_hash, provider_session_ref
        FROM agent_bridge_state WHERE agent_id='successor_live_01'
    `).get()).toEqual({ bridge_state: "none", capability_hash: null, provider_session_ref: null });
    expect(fixture.database.prepare(`
      SELECT revoked_at FROM capabilities
       WHERE run_id='run_launch_01' AND agent_id='chair_launch_01'
    `).get()).toEqual({ revoked_at: now });
    expect(fixture.database.prepare(`
      SELECT revoked_at FROM capabilities WHERE token_hash=?
    `).get(seeded.successorCapabilityHash)).toEqual({ revoked_at: null });
    expect(fixture.database.prepare(`
      SELECT COUNT(*) AS count FROM run_chair_leases WHERE status='active'
    `).get()).toEqual({ count: 1 });
    expect(fixture.database.prepare(`
      SELECT COUNT(*) AS count FROM delivery_freezes WHERE run_id='run_launch_01'
    `).get()).toEqual({ count: 0 });
    expect(retiredPredecessor).toBe(1);
    expect(fixture.service.chairLiveHandoffStatus("operator_01", "commit_live_handoff_01"))
      .toMatchObject({ status: "committed", custodyId: handoff.custodyId });
  });

  it("restores the predecessor without authority drift when promotion lookup proves no effect", async () => {
    let promotions = 0;
    let lookups = 0;
    const fixture = createFixture({
      retainedChairBridge: true,
      dispatch: async () => ({
        schemaVersion: 1,
        providerAdapterId: "claude-agent-sdk",
        providerActionId: "provider_launch_01",
        providerContractDigest: contractDigest,
        observationKind: "dispatch-return",
        observedAt: "2027-01-01T00:00:00.000Z",
        outcome: {
          kind: "terminal-success",
          providerSessionRef: "claude-chair-live-no-effect",
          providerSessionGeneration: 2,
          effectDigest: digest("provider-live-no-effect-predecessor"),
          resourceUsage: { provider_calls: 1 },
        },
      }),
      promoteSuccessor: async () => { promotions += 1; return false; },
      lookupSuccessor: async () => { lookups += 1; return "child"; },
    });
    const { handle: launchHandle } = await prepareFixture(fixture);
    await fixture.service.dispatchPrepared(launchHandle);
    const seeded = seedLiveHandoffSuccessor(fixture);
    const inspection = await fixture.service.inspectChairLiveHandoff(seeded.intent);
    const handoff = fixture.database.transaction(() => (
      fixture.service.prepareChairLiveHandoffInTransaction({
        inspection,
        operatorId: "operator_01",
        operatorCommandId: "commit_live_handoff_no_effect_01",
        providerActionTicket: handoffTicket(fixture, inspection, "commit_live_handoff_no_effect_01"),
      })
    )).immediate();

    await expect(fixture.service.dispatchPreparedChairLiveHandoff(handoff)).resolves.toMatchObject({
      status: "no-effect",
    });
    expect({ promotions, lookups }).toEqual({ promotions: 1, lookups: 1 });
    expect(fixture.database.prepare(`
      SELECT chair_agent_id, chair_generation, lifecycle_state, revision
        FROM runs WHERE run_id='run_launch_01'
    `).get()).toEqual({
      chair_agent_id: "chair_launch_01",
      chair_generation: seeded.intent.expectedChairGeneration,
      lifecycle_state: "active",
      revision: seeded.intent.expectedRunRevision + 2,
    });
    expect(fixture.database.prepare(`
      SELECT generation, revision, membership_revision FROM project_sessions
       WHERE project_session_id='session_launch_01'
    `).get()).toEqual({
      generation: seeded.intent.expectedSessionGeneration,
      revision: seeded.intent.expectedSessionRevision,
      membership_revision: seeded.intent.expectedMembershipRevision,
    });
    expect(fixture.database.prepare("SELECT status FROM run_chair_leases WHERE lease_id=?")
      .get(seeded.intent.expectedChairLeaseId)).toEqual({ status: "active" });
    expect(fixture.database.prepare(`
      SELECT bridge_state FROM agent_bridge_state WHERE agent_id='successor_live_01'
    `).get()).toEqual({ bridge_state: "active" });
    expect(fixture.database.prepare(`
      SELECT state FROM chair_live_handoff_custody WHERE custody_id=?
    `).get(handoff.custodyId)).toEqual({ state: "no-effect" });
    expect(fixture.database.prepare(`
      SELECT status, execution_count, effect_count FROM provider_actions WHERE action_id=?
    `).get(handoff.promotionActionId)).toEqual({ status: "terminal", execution_count: 1, effect_count: 0 });
    expect(fixture.database.prepare("SELECT COUNT(*) AS count FROM delivery_freezes WHERE run_id='run_launch_01'").get())
      .toEqual({ count: 0 });
  });

  it("keeps prepared restart inert and recovers durable dispatched custody by lookup only", async () => {
    const preparedFixture = createFixture({
      retainedChairBridge: true,
      dispatch: async () => ({
        schemaVersion: 1,
        providerAdapterId: "claude-agent-sdk",
        providerActionId: "provider_launch_01",
        providerContractDigest: contractDigest,
        observationKind: "dispatch-return",
        observedAt: "2027-01-01T00:00:00.000Z",
        outcome: {
          kind: "terminal-success",
          providerSessionRef: "claude-chair-live-prepared-restart",
          providerSessionGeneration: 2,
          effectDigest: digest("provider-live-prepared-restart"),
          resourceUsage: { provider_calls: 1 },
        },
      }),
    });
    const { handle: preparedLaunch } = await prepareFixture(preparedFixture);
    await preparedFixture.service.dispatchPrepared(preparedLaunch);
    const preparedSeed = seedLiveHandoffSuccessor(preparedFixture);
    const preparedInspection = await preparedFixture.service.inspectChairLiveHandoff(preparedSeed.intent);
    preparedFixture.database.transaction(() => {
      preparedFixture.service.prepareChairLiveHandoffInTransaction({
        inspection: preparedInspection,
        operatorId: "operator_01",
        operatorCommandId: "commit_live_handoff_prepared_restart_01",
        providerActionTicket: handoffTicket(
          preparedFixture,
          preparedInspection,
          "commit_live_handoff_prepared_restart_01",
        ),
      });
    }).immediate();
    let preparedIo = 0;
    const preparedRestart = restartedLiveHandoffService(preparedFixture, {
      promote: async () => { preparedIo += 1; throw new Error("prepared restart promoted"); },
      lookup: async () => { preparedIo += 1; throw new Error("prepared restart looked up"); },
    });
    await expect(preparedRestart.recover()).resolves.toEqual({
      preparedFailed: 1,
      lookedUp: 0,
      activated: 0,
      failed: 1,
      ambiguous: 0,
      recoveryRequired: 0,
    });
    expect(preparedIo).toBe(0);
    expect(preparedFixture.database.prepare(`
      SELECT state FROM chair_live_handoff_custody
       WHERE operator_command_id='commit_live_handoff_prepared_restart_01'
    `).get()).toEqual({ state: "no-effect" });
    expect(preparedFixture.database.prepare(`
      SELECT lifecycle_state FROM runs WHERE run_id='run_launch_01'
    `).get()).toEqual({ lifecycle_state: "active" });

    let promotions = 0;
    const dispatchedFixture = createFixture({
      retainedChairBridge: true,
      dispatch: async () => ({
        schemaVersion: 1,
        providerAdapterId: "claude-agent-sdk",
        providerActionId: "provider_launch_01",
        providerContractDigest: contractDigest,
        observationKind: "dispatch-return",
        observedAt: "2027-01-01T00:00:00.000Z",
        outcome: {
          kind: "terminal-success",
          providerSessionRef: "claude-chair-live-dispatched-restart",
          providerSessionGeneration: 2,
          effectDigest: digest("provider-live-dispatched-restart"),
          resourceUsage: { provider_calls: 1 },
        },
      }),
      fault: (label) => {
        if (label === "chair-live-handoff:dispatched") throw new Error("crash after durable dispatch");
      },
      promoteSuccessor: async () => { promotions += 1; return true; },
    });
    const { handle: dispatchedLaunch } = await prepareFixture(dispatchedFixture);
    await dispatchedFixture.service.dispatchPrepared(dispatchedLaunch);
    const dispatchedSeed = seedLiveHandoffSuccessor(dispatchedFixture);
    const dispatchedInspection = await dispatchedFixture.service.inspectChairLiveHandoff(dispatchedSeed.intent);
    const dispatched = dispatchedFixture.database.transaction(() => (
      dispatchedFixture.service.prepareChairLiveHandoffInTransaction({
        inspection: dispatchedInspection,
        operatorId: "operator_01",
        operatorCommandId: "commit_live_handoff_dispatched_restart_01",
        providerActionTicket: handoffTicket(
          dispatchedFixture,
          dispatchedInspection,
          "commit_live_handoff_dispatched_restart_01",
        ),
      })
    )).immediate();
    await expect(dispatchedFixture.service.dispatchPreparedChairLiveHandoff(dispatched))
      .rejects.toThrow("crash after durable dispatch");
    expect(promotions).toBe(0);
    expect(dispatchedFixture.database.prepare(`
      SELECT state FROM chair_live_handoff_custody WHERE custody_id=?
    `).get(dispatched.custodyId)).toEqual({ state: "dispatched" });
    expect(dispatchedFixture.database.prepare(`
      SELECT status, execution_count FROM provider_actions WHERE action_id=?
    `).get(dispatched.promotionActionId)).toEqual({ status: "dispatched", execution_count: 1 });

    let restartPromotions = 0;
    let restartLookups = 0;
    const dispatchedRestart = restartedLiveHandoffService(dispatchedFixture, {
      promote: async () => { restartPromotions += 1; throw new Error("restart promoted"); },
      lookup: async () => { restartLookups += 1; return "chair"; },
    });
    await expect(dispatchedRestart.recover()).resolves.toMatchObject({
      lookedUp: 1,
      activated: 1,
      ambiguous: 0,
      recoveryRequired: 0,
    });
    expect({ restartPromotions, restartLookups }).toEqual({ restartPromotions: 0, restartLookups: 1 });
    expect(dispatchedFixture.database.prepare(`
      SELECT execution_count, effect_count FROM provider_actions WHERE action_id=?
    `).get(dispatched.promotionActionId)).toEqual({ execution_count: 1, effect_count: 1 });
  });

  it("keeps ambiguous promotion fenced until lookup proves the successor chair", async () => {
    let promotions = 0;
    let lookups = 0;
    const fixture = createFixture({
      retainedChairBridge: true,
      dispatch: async () => ({
        schemaVersion: 1,
        providerAdapterId: "claude-agent-sdk",
        providerActionId: "provider_launch_01",
        providerContractDigest: contractDigest,
        observationKind: "dispatch-return",
        observedAt: "2027-01-01T00:00:00.000Z",
        outcome: {
          kind: "terminal-success",
          providerSessionRef: "claude-chair-live-ambiguous",
          providerSessionGeneration: 2,
          effectDigest: digest("provider-live-ambiguous-predecessor"),
          resourceUsage: { provider_calls: 1 },
        },
      }),
      fault: (label) => {
        if (label === "chair-live-handoff:after-adapter") throw new Error("transport vanished after promotion");
      },
      promoteSuccessor: async () => { promotions += 1; return true; },
      lookupSuccessor: async () => { lookups += 1; return "chair"; },
    });
    const { handle: launchHandle } = await prepareFixture(fixture);
    await fixture.service.dispatchPrepared(launchHandle);
    const seeded = seedLiveHandoffSuccessor(fixture);
    const inspection = await fixture.service.inspectChairLiveHandoff(seeded.intent);
    const handoff = fixture.database.transaction(() => (
      fixture.service.prepareChairLiveHandoffInTransaction({
        inspection,
        operatorId: "operator_01",
        operatorCommandId: "commit_live_handoff_ambiguous_01",
        providerActionTicket: handoffTicket(fixture, inspection, "commit_live_handoff_ambiguous_01"),
      })
    )).immediate();
    await expect(fixture.service.dispatchPreparedChairLiveHandoff(handoff)).resolves.toMatchObject({
      status: "ambiguous",
    });
    expect({ promotions, lookups }).toEqual({ promotions: 1, lookups: 0 });
    expect(fixture.database.prepare(`
      SELECT lifecycle_state FROM runs WHERE run_id='run_launch_01'
    `).get()).toEqual({ lifecycle_state: "reconciling" });
    expect(() => fixture.database.prepare(`
      INSERT INTO capabilities(token_hash, run_id, agent_id, principal_generation, expires_at)
      VALUES ('forbidden-live-grant', 'run_launch_01', 'successor_live_01', 1, ?)
    `).run(now + 60_000)).toThrow(/INVARIANT_chair_live_handoff_freezes_grants/u);
    fixture.database.prepare(`
      INSERT INTO operator_capabilities(
        capability_id, token_hash, operator_id, project_id, project_session_id,
        project_authority_generation, session_generation, principal_generation,
        kind, operations_json, issued_at, expires_at
      ) VALUES (
        'operator_cap_live_mutation_01', ?, 'operator_01', 'project_01', 'session_launch_01',
        1, ?, 1, 'session', '["read","decide"]', ?, ?
      )
    `).run(
      sha256("operator-live-mutation-secret"),
      seeded.intent.expectedSessionGeneration,
      now - 1,
      now + 60_000,
    );
    const sessions = new ProjectSessionStore({
      database: fixture.database,
      operatorStore: new OperatorStore({ database: fixture.database, clock: () => now }),
      clock: () => now,
    });
    expect(() => sessions.transitionProjectSession({
      operatorId: "operator_01" as OperatorId,
      projectId: "project_01" as ProjectId,
      projectAuthorityGeneration: 1,
      principalGeneration: 1,
    }, {
      command: {
        credential: { capabilityId: "operator_cap_live_mutation_01", token: "operator-live-mutation-secret" },
        commandId: "forbidden_live_handoff_session_mutation_01",
        expectedRevision: seeded.intent.expectedSessionRevision,
        actor: "operator_01",
        provenance: {
          kind: "console-direct-input",
          clientId: "console_live_handoff_01",
          inputEventId: "input_live_handoff_mutation_01",
        },
        evidenceRefs: [],
      },
      projectSessionId: "session_launch_01",
      expectedGeneration: seeded.intent.expectedSessionGeneration,
      transition: { to: "visibility_degraded", reason: "generic mutation must not steal live handoff custody" },
    } as unknown as ProjectSessionTransitionRequest)).toThrowError(
      expect.objectContaining({ code: "LIFECYCLE_PRECONDITION_FAILED" }),
    );
    await expect(fixture.service.inspectChairRecovery({
      kind: "chair-bridge-recovery",
      schemaVersion: 1,
      path: "rebind",
      projectSessionId: seeded.intent.projectSessionId,
      coordinationRunId: seeded.intent.coordinationRunId,
      lossId: "loss_live_handoff_forbidden_01",
      recoveryManifestDigest: digest("loss-live-handoff-forbidden"),
      expectedSessionRevision: seeded.intent.expectedSessionRevision,
      expectedSessionGeneration: seeded.intent.expectedSessionGeneration,
      expectedRunRevision: seeded.intent.expectedRunRevision + 1,
      expectedChairGeneration: seeded.intent.expectedChairGeneration,
      expectedPrincipalGeneration: seeded.intent.expectedPredecessorPrincipalGeneration,
      expectedBridgeRevision: seeded.intent.expectedBridgeRevision,
      expectedLostBridgeGeneration: seeded.intent.expectedChairBridgeGeneration,
      expectedProviderSessionGeneration: 2,
      providerAdapterId: seeded.intent.providerAdapterId,
      providerContractDigest: seeded.intent.providerContractDigest,
      providerActionId: "provider_recovery_forbidden_01" as never,
    })).rejects.toMatchObject({ code: "CONFLICT" });
    await expect(fixture.service.reconcileChairLiveHandoff(
      "operator_01",
      "commit_live_handoff_ambiguous_01",
    )).resolves.toMatchObject({ status: "committed" });
    expect({ promotions, lookups }).toEqual({ promotions: 1, lookups: 1 });
    expect(fixture.database.prepare(`
      SELECT chair_agent_id, lifecycle_state FROM runs WHERE run_id='run_launch_01'
    `).get()).toEqual({ chair_agent_id: "successor_live_01", lifecycle_state: "active" });
  });

  it("routes live handoff preview and commit through takeover custody with zero generic effects", async () => {
    const outcome = {
      schemaVersion: 1,
      providerAdapterId: "claude-agent-sdk",
      providerActionId: "provider_launch_01",
      providerContractDigest: contractDigest,
      observationKind: "dispatch-return" as const,
      observedAt: "2027-01-01T00:00:00.000Z",
      outcome: {
        kind: "terminal-success" as const,
        providerSessionRef: "claude-chair-live-operator-old",
        providerSessionGeneration: 2,
        effectDigest: digest("provider-live-operator-predecessor"),
        resourceUsage: { provider_calls: 1 },
      },
    };
    const fixture = createFixture({ dispatch: async () => outcome, retainedChairBridge: true, promoteSuccessor: async () => true });
    const { handle: launchHandle } = await prepareFixture(fixture);
    await fixture.service.dispatchPrepared(launchHandle);
    const { intent } = seedLiveHandoffSuccessor(fixture);
    fixture.database.prepare(`
      INSERT INTO operator_capabilities(
        capability_id, token_hash, operator_id, project_id, project_session_id,
        project_authority_generation, session_generation, principal_generation,
        kind, operations_json, issued_at, expires_at, handoff_digest,
        old_chair_generation, expected_run_id, expected_run_revision,
        expected_session_revision, cas_target_revision
      ) VALUES (
        'operator_cap_live_handoff_01', ?, 'operator_01', 'project_01', 'session_launch_01',
        1, ?, 1, 'takeover', '["takeover","read"]', ?, ?, ?, ?,
        'run_launch_01', ?, ?, ?
      )
    `).run(
      sha256("operator-live-handoff-secret"),
      intent.expectedSessionGeneration,
      now - 1,
      now + 60_000,
      intent.handoffRef.digest,
      intent.expectedChairGeneration,
      intent.expectedRunRevision,
      intent.expectedSessionRevision,
      intent.expectedBridgeRevision,
    );
    const operatorStore = new OperatorStore({ database: fixture.database, clock: () => now });
    let genericEffects = 0;
    const actions = new OperatorActionStore({
      database: fixture.database,
      operatorStore,
      statePort: { read: async () => { throw new Error("generic state must not inspect live handoff"); } },
      effectPort: {
        dispatch: async () => { genericEffects += 1; throw new Error("generic effect must not hand off chair"); },
        observe: async () => { genericEffects += 1; throw new Error("generic effect must not hand off chair"); },
      },
      chairLiveHandoffCustody: fixture.service,
      clock: () => now,
    });
    const context = {
      operatorId: "operator_01",
      projectId: "project_01",
      projectAuthorityGeneration: 1,
      principalGeneration: 1,
    } as never;
    const previewCommand = {
      credential: { capabilityId: "operator_cap_live_handoff_01", token: "operator-live-handoff-secret" },
      commandId: "preview_live_handoff_01",
      expectedRevision: intent.expectedBridgeRevision,
      actor: "operator_01",
      provenance: {
        kind: "console-direct-input",
        clientId: "console_live_handoff_01",
        inputEventId: "input_live_handoff_preview_01",
      },
      evidenceRefs: [],
    };
    const preview = await actions.preview(context, {
      command: previewCommand,
      projectId: "project_01",
      intent,
    } as unknown as OperatorActionPreviewRequest);
    const receipt = await actions.commit(context, {
      command: {
        ...previewCommand,
        commandId: "commit_live_handoff_operator_01",
        provenance: { ...previewCommand.provenance, inputEventId: "input_live_handoff_commit_01" },
      },
      projectId: "project_01",
      previewId: preview.previewId,
      expectedPreviewRevision: preview.previewRevision,
      previewDigest: preview.previewDigest,
      expectedIntentDigest: preview.intentDigest,
      confirmation: { kind: "explicit", confirmationId: "confirm_live_handoff_01" },
    } as unknown as OperatorActionCommitRequest);
    expect(genericEffects).toBe(0);
    expect(receipt.afterStateDigest).not.toEqual(receipt.beforeStateDigest);
    fixture.database.prepare(`
      INSERT INTO operator_capabilities(
        capability_id, token_hash, operator_id, project_id, project_session_id,
        project_authority_generation, session_generation, principal_generation,
        kind, operations_json, issued_at, expires_at
      ) VALUES (
        'operator_cap_live_status_01', ?, 'operator_01', 'project_01', 'session_launch_01',
        1, ?, 1, 'session', '["read"]', ?, ?
      )
    `).run(
      sha256("operator-live-status-secret"),
      intent.expectedSessionGeneration + 1,
      now,
      now + 60_000,
    );
    expect(actions.status({
      credential: { capabilityId: "operator_cap_live_status_01", token: "operator-live-status-secret" },
      projectId: "project_01",
      commandId: "commit_live_handoff_operator_01",
    } as never)).toMatchObject({ status: "committed" });
  });
});

describe("launch custody recovery — cross-family ordering coordinator (issue #354, S4e2)", () => {
  it("continues past a family error, still runs the retained-chair audit, keeps exact counters, and raises only the final aggregate", async () => {
    const calls: string[] = [];
    const liveHandoffError = new Error("live handoff recovery failed");
    const chairRecoveryError = new Error("chair recovery failed");

    const families = {
      chairLiveHandoffCustodyRecovery: {
        recoverChairLiveHandoffCustody: async () => {
          calls.push("live-handoff");
          throw liveHandoffError;
        },
      },
      chairRecoveryCustody: {
        recoverChairRecoveryCustody: async () => {
          calls.push("chair-recovery");
          throw chairRecoveryError;
        },
        auditRetainedChairBridges: (result: { recoveryRequired: number }, errors: unknown[]) => {
          calls.push("retained-chair-audit");
          result.recoveryRequired += 1;
          void errors;
        },
      },
      providerAgentCustodyRecovery: {
        recoverAgentCustody: async (result: { activated: number }) => {
          calls.push("provider-agent");
          result.activated += 1;
        },
      },
      launchSettlement: {
        recoverLaunchCustody: async (result: { lookedUp: number }) => {
          calls.push("launch");
          result.lookedUp += 1;
        },
      },
    };

    await expect(recoverLaunchCustodyFamilies(families)).rejects.toMatchObject({
      name: "AggregateError",
      errors: [liveHandoffError, chairRecoveryError],
      message: "launch custody recovery left one or more sessions unfenced",
    });

    // Exact original ordering (plan §2 "S4e"): live handoff, chair recovery, provider-agent,
    // launch, then retained-chair audit — every family is attempted despite the two errors above,
    // and the audit still runs after the aggregate would otherwise short-circuit.
    expect(calls).toEqual(["live-handoff", "chair-recovery", "provider-agent", "launch", "retained-chair-audit"]);
  });

  it("returns exact counter totals with no error when every family succeeds", async () => {
    const families = {
      chairLiveHandoffCustodyRecovery: {
        recoverChairLiveHandoffCustody: async (result: { preparedFailed: number }) => {
          result.preparedFailed += 1;
        },
      },
      chairRecoveryCustody: {
        recoverChairRecoveryCustody: async (result: { failed: number }) => {
          result.failed += 1;
        },
        auditRetainedChairBridges: (result: { recoveryRequired: number }) => {
          result.recoveryRequired += 1;
        },
      },
      providerAgentCustodyRecovery: {
        recoverAgentCustody: async (result: { activated: number }) => {
          result.activated += 1;
        },
      },
      launchSettlement: {
        recoverLaunchCustody: async (result: { lookedUp: number }) => {
          result.lookedUp += 1;
        },
      },
    };

    await expect(recoverLaunchCustodyFamilies(families)).resolves.toEqual({
      preparedFailed: 1,
      lookedUp: 1,
      activated: 1,
      failed: 1,
      ambiguous: 0,
      recoveryRequired: 1,
    });
  });
});
