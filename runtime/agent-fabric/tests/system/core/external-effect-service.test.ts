import Database from "better-sqlite3";
import {
  parseArtifactRef,
  parseIdentifier,
  parseSha256Digest,
  parseTimestamp,
  type OperatorActionIntent,
  type ScopedGate,
} from "@local/agent-fabric-protocol";
import { afterEach, describe, expect, it } from "vitest";

import { applyMigrations } from "../../../src/core/migrations.ts";
import { ProviderActionAdmissionCoordinator } from "../../../src/application/provider-action-admission.ts";
import {
  ExternalEffectService,
  type ExternalEffectEvidencePort,
  type RegisteredEffectPort,
} from "../../../src/operator/external-effect-service.ts";
import type { OperatorEffectRequest } from "../../../src/operator/action-store.ts";
import { createProductionOperatorActionPorts } from "../../../src/operator/production-action-ports.ts";
import { canonicalJson, sha256 } from "../../../src/persistence/row-codec.ts";

const databases: Database.Database[] = [];
const digest = parseSha256Digest(`sha256:${"a".repeat(64)}`, "test.digest");
const requestArtifact = parseArtifactRef({ path: "requests/effect.json", digest }, "test.requestArtifact");
const integrationId = parseIdentifier<"IntegrationId">("integration_01", "test.integrationId");
const projectSessionId = parseIdentifier<"ProjectSessionId">("session_01", "test.projectSessionId");
const coordinationRunId = parseIdentifier<"CoordinationRunId">("run_01", "test.coordinationRunId");
const gateId = parseIdentifier<"GateId">("gate_release_01", "test.gateId");

function releaseGate(revision = 3): ScopedGate {
  return {
    gateId,
    projectSessionId,
    coordinationRunId,
    scope: { kind: "release" },
    affectedTaskIds: [],
    dependencyRevision: 1,
    blockedOperationIds: [],
    enforcementPoints: ["operation"],
    question: "Release?",
    reason: "Exact promotion boundary",
    options: ["approve", "reject"],
    recommendation: "approve",
    consequences: ["promotes artifact"],
    evidenceRefs: [requestArtifact],
    revision,
    createdByRef: "policy:release",
    expectedApproverRef: "operator_01",
    status: "approved",
    resolution: {
      operatorId: parseIdentifier<"OperatorId">("operator_01", "test.operatorId"),
      decidedAt: parseTimestamp("2027-01-01T00:00:00Z", "test.decidedAt"),
      evidenceRefs: [requestArtifact],
      kind: "typed-console",
      confirmationCommandId: parseIdentifier<"CommandId">("confirm_release_01", "test.commandId"),
    },
    releaseBinding: {
      acceptedDeliveryReceiptRef: requestArtifact,
      artifactDigest: digest,
      promotionAction: "release",
      target: "local:test",
    },
  };
}

function promotionIntent(): Extract<OperatorActionIntent, { kind: "promotion" }> {
  const gate = releaseGate();
  if (gate.releaseBinding === undefined) throw new Error("release fixture lacks a binding");
  return {
    kind: "promotion",
    projectSessionId,
    coordinationRunId,
    gateId,
    expectedGateRevision: gate.revision,
    expectedGateStatus: "approved",
    releaseBinding: gate.releaseBinding,
  };
}

function registeredIntent(overrides: Partial<Extract<OperatorActionIntent, { kind: "registered-external-effect" }>> = {}):
  Extract<OperatorActionIntent, { kind: "registered-external-effect" }> {
  return {
    kind: "registered-external-effect",
    integrationId,
    expectedIntegrationGeneration: 2,
    operationId: "notify.create",
    contractDigest: digest,
    requestArtifactRef: requestArtifact,
    targetId: "target_01",
    expectedTargetRevision: 5,
    idempotencyKey: "effect_01",
    ...overrides,
  };
}

function effectPort(overrides: Partial<RegisteredEffectPort> = {}): RegisteredEffectPort {
  return {
    integrationId,
    generation: 2,
    contractDigest: digest,
    operations: {
      "notify.create": {
        contractDigest: digest,
        targets: { target_01: { revision: 5 } },
      },
      release: {
        contractDigest: digest,
        targets: { "local:test": { revision: 8 } },
      },
    },
    dispatch: async () => ({ status: "unused" }),
    lookup: async () => ({ status: "unused" }),
    ...overrides,
  };
}

function fixture(options: Readonly<{
  registry?: readonly RegisteredEffectPort[];
  gate?: ScopedGate;
  evidence?: ExternalEffectEvidencePort;
}> = {}): {
  database: Database.Database;
  service: ExternalEffectService;
} {
  const database = new Database(":memory:");
  databases.push(database);
  applyMigrations(database);
  database.exec(`
    INSERT INTO projects(
      project_id, canonical_root, trust_record_digest, revision,
      authority_generation, created_at, updated_at
    ) VALUES ('project_01', '/project/one', '${digest}', 1, 1, 1, 1);
    INSERT INTO project_sessions(
      project_session_id, project_id, mode, state, revision, generation,
      authority_ref, budget_ref, launch_packet_path, launch_packet_digest,
      membership_revision, origin_kind, origin_operator_id, created_at, updated_at
    ) VALUES (
      'session_01', 'project_01', 'coordinated', 'active', 2, 1,
      '${digest}', 'budget_01', 'launch.json', '${digest}',
      1, 'operator-launch', 'operator_01', 1, 1
    );
    INSERT INTO runs(
      run_id, chair_agent_id, workspace_root, project_run_directory, created_at,
      project_session_id, lifecycle_state, revision, chair_generation,
      chair_lease_id, authority_ref, budget_ref, dependency_revision, topology_slot
    ) VALUES (
      'run_01', 'chair_01', '/project/one', '.agent-run/AFAB-004', 1,
      'session_01', 'active', 1, 1, 'chair:run_01:1', '${digest}',
      'budget_01', 1, 1
    );
  `);
  if (options.gate !== undefined) {
    const gate = options.gate;
    if (!("resolution" in gate)) {
      throw new Error("release gate fixture must be resolved");
    }
    database.prepare(`
      INSERT INTO scoped_gates(
        gate_id, project_session_id, coordination_run_id, dedupe_key,
        scope_kind, scope_task_id, dependency_revision,
        blocked_operation_ids_json, enforcement_points_json, question, reason,
        options_json, recommendation, consequences_json, evidence_refs_json,
        created_by_ref, expected_approver_ref, resolved_by_operator_id,
        resolution_json, deadline, default_action, status, human_required,
        release_binding_json, revision,
        created_at, updated_at
      ) VALUES (
        ?, ?, ?, 'release:01', 'release', NULL, ?, '[]', '["operation"]', ?, ?,
        ?, ?, ?, ?, ?, ?, 'operator_01', ?, NULL, NULL, 'approved', 1,
        ?, ?, 1, 1
      )
    `).run(
      gate.gateId,
      gate.projectSessionId,
      gate.coordinationRunId,
      gate.dependencyRevision,
      gate.question,
      gate.reason,
      canonicalJson(gate.options),
      gate.recommendation,
      canonicalJson(gate.consequences),
      canonicalJson(gate.evidenceRefs),
      gate.createdByRef,
      gate.expectedApproverRef,
      canonicalJson(gate.resolution),
      canonicalJson(gate.releaseBinding),
      gate.revision,
    );
  }
  const evidence: ExternalEffectEvidencePort = options.evidence ?? {
    inspectArtifact: async (ref) => ref,
    inspectAcceptedDeliveryReceipt: async () => null,
  };
  return {
    database,
    service: new ExternalEffectService({
      database,
      registry: options.registry ?? [effectPort()],
      evidence,
      gates: {
        getGate: (requestedGateId: string): ScopedGate => {
          if (options.gate === undefined || options.gate.gateId !== requestedGateId) {
            throw new Error("gate lookup is not expected");
          }
          return options.gate;
        },
      },
    }),
  };
}

function effectRequest(
  commandId: string,
  intent: OperatorActionIntent,
): OperatorEffectRequest {
  return {
    commandId,
    operatorId: "operator_01",
    projectId: "project_01",
    projectSessionId: "session_01",
    principalGeneration: 1,
    operation: "external-effect",
    intent,
    intentDigest: parseSha256Digest(
      `sha256:${sha256(canonicalJson(intent))}`,
      "test.intentDigest",
    ),
    beforeStateDigest: digest,
    attemptGeneration: 1,
  };
}

function seedGenericCustody(
  database: Database.Database,
  request: OperatorEffectRequest,
  custodyId: string,
  state = "prepared",
): void {
  database.prepare(`
    INSERT INTO operator_effect_custody(
      custody_id, operator_id, project_id, project_session_id, principal_generation,
      command_id, operation, intent_digest, before_state_digest, intent_json, state,
      effect_path, effect_digest, outcome_json, created_at, updated_at
    ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, NULL, NULL, NULL, 1, 1)
  `).run(
    custodyId,
    request.operatorId,
    request.projectId,
    request.projectSessionId,
    request.principalGeneration,
    request.commandId,
    request.operation,
    request.intentDigest,
    request.beforeStateDigest,
    canonicalJson(request.intent),
    state,
  );
}

afterEach(() => {
  for (const database of databases.splice(0)) database.close();
});

describe("registered external-effect and promotion custody", () => {
  it("keeps broad external-effect authority unavailable when no integration is registered", async () => {
    const { database, service } = fixture({ registry: [] });

    await expect(service.readCurrentState(registeredIntent())).rejects.toMatchObject({
      code: "CAPABILITY_FORBIDDEN",
    });
    expect(database.prepare("SELECT count(*) AS count FROM operator_effect_custody").get())
      .toStrictEqual({ count: 0 });
  });

  it("projects one closed registered integration contract for operator preview", async () => {
    const { service } = fixture();

    await expect(service.readCurrentState(registeredIntent())).resolves.toStrictEqual({
      kind: "registered-external-effect",
      revision: 5,
      state: {
        integrationId,
        integrationGeneration: 2,
        operationContracts: { "notify.create": digest, release: digest },
        targetRevisions: { target_01: 5, "local:test": 8 },
      },
    });
  });

  it("composes registered effects through the production operator owner with atomic generic and specialised custody", async () => {
    let dispatches = 0;
    const port = effectPort({
      dispatch: async (input) => {
        dispatches += 1;
        return {
          schemaVersion: 1,
          custodyId: input.custodyId,
          idempotencyKey: input.idempotencyKey,
          outcome: "committed",
          evidenceDigest: digest,
        };
      },
    });
    const { database, service } = fixture({ registry: [port] });
    const production = createProductionOperatorActionPorts({
      database,
      clock: () => Date.now(),
      providerActionAdmission: new ProviderActionAdmissionCoordinator({ database, clock: () => Date.now() }),
      adapter: {
        capabilities: async () => { throw new Error("provider adapter is not used"); },
        dispatch: async () => { throw new Error("provider adapter is not used"); },
        lookup: async () => { throw new Error("provider adapter is not used"); },
      },
      externalEffects: service,
    });
    const intent = registeredIntent();
    const current = await production.statePort.read(intent);
    const request: OperatorEffectRequest = {
      ...effectRequest("external_production_01", intent),
      beforeStateDigest: parseSha256Digest(
        `sha256:${sha256(canonicalJson(current))}`,
        "test.beforeStateDigest",
      ),
    };

    production.effectPort.prepare?.(request);
    expect(database.prepare(`
      SELECT custody.state, binding.effect_kind
        FROM operator_effect_custody custody
        JOIN operator_external_effect_bindings binding USING(custody_id)
       WHERE custody.command_id='external_production_01'
    `).get()).toStrictEqual({ state: "prepared", effect_kind: "registered-external-effect" });

    await expect(production.effectPort.dispatch(request)).resolves.toMatchObject({
      status: "committed",
    });
    expect(dispatches).toBe(1);
    await expect(production.effectPort.dispatch(request)).resolves.toMatchObject({
      status: "committed",
    });
    expect(dispatches).toBe(1);
  });

  it("re-reads the exact approved release gate for promotion preview", async () => {
    const gate = releaseGate();
    const { service } = fixture({ gate });

    await expect(service.readCurrentState(promotionIntent())).resolves.toStrictEqual({
      kind: "promotion",
      revision: gate.revision,
      gate,
    });
  });

  it("binds a prepared generic custody row to one immutable registered effect", () => {
    const { database, service } = fixture();
    const request = effectRequest("external_prepare_01", registeredIntent());
    seedGenericCustody(database, request, "custody_external_01");

    expect(service.prepareInTransaction(request)).toMatchObject({ custodyId: "custody_external_01" });
    expect(database.prepare(`
      SELECT effect_kind, integration_id, integration_generation, operation_id,
             contract_digest, target_id, target_revision, request_artifact_path,
             request_artifact_digest, idempotency_key, release_gate_id,
             release_gate_revision, release_binding_digest, lookup_generation,
             lookup_evidence_digest
        FROM operator_external_effect_bindings
    `).get()).toStrictEqual({
      effect_kind: "registered-external-effect",
      integration_id: integrationId,
      integration_generation: 2,
      operation_id: "notify.create",
      contract_digest: digest,
      target_id: "target_01",
      target_revision: 5,
      request_artifact_path: requestArtifact.path,
      request_artifact_digest: requestArtifact.digest,
      idempotency_key: "effect_01",
      release_gate_id: null,
      release_gate_revision: null,
      release_binding_digest: null,
      lookup_generation: 0,
      lookup_evidence_digest: null,
    });
    expect(() => database.prepare(`
      UPDATE operator_external_effect_bindings SET target_revision=6 WHERE custody_id='custody_external_01'
    `).run()).toThrow(/INVARIANT_external_effect_binding_immutable/u);
    expect(() => database.prepare(`
      UPDATE operator_external_effect_bindings SET lookup_generation=2,
        lookup_evidence_digest='${digest}' WHERE custody_id='custody_external_01'
    `).run()).toThrow(/INVARIANT_external_effect_lookup_cas/u);
    expect(() => database.prepare(`
      DELETE FROM operator_external_effect_bindings WHERE custody_id='custody_external_01'
    `).run()).toThrow(/INVARIANT_external_effect_binding_immutable/u);
  });

  it("conflicts a second command that reuses one registered idempotency identity", () => {
    const { database, service } = fixture();
    const first = effectRequest("external_idempotency_01", registeredIntent());
    seedGenericCustody(database, first, "custody_external_idempotency_01");
    const firstHandle = service.prepareInTransaction(first);
    expect(service.prepareInTransaction(first)).toBe(firstHandle);

    const changed = effectRequest("external_idempotency_02", registeredIntent());
    seedGenericCustody(database, changed, "custody_external_idempotency_02");
    expect(() => service.prepareInTransaction(changed)).toThrowError(expect.objectContaining({
      code: "DEDUPE_CONFLICT",
    }));
    expect(database.prepare("SELECT count(*) AS count FROM operator_external_effect_bindings").get())
      .toStrictEqual({ count: 1 });
  });

  it.each([
    ["generation", registeredIntent({ expectedIntegrationGeneration: 3 })],
    ["contract", registeredIntent({ contractDigest: parseSha256Digest(`sha256:${"b".repeat(64)}`, "test.otherDigest") })],
    ["operation", registeredIntent({ operationId: "notify.missing" })],
    ["target revision", registeredIntent({ expectedTargetRevision: 6 })],
  ])("rejects a stale registered %s before adapter I/O", (_label, intent) => {
    let dispatches = 0;
    const port = effectPort({ dispatch: async () => { dispatches += 1; return null; } });
    const { database, service } = fixture({ registry: [port] });
    const request = effectRequest(`external_stale_${String(_label).replaceAll(" ", "_")}`, intent);
    seedGenericCustody(database, request, `custody_external_stale_${String(_label).replaceAll(" ", "_")}`);

    expect(() => service.prepareInTransaction(request)).toThrow();
    expect(dispatches).toBe(0);
    expect(database.prepare("SELECT count(*) AS count FROM operator_external_effect_bindings").get())
      .toStrictEqual({ count: 0 });
  });

  it("dispatches one exact registered effect and persists only a bounded closed outcome", async () => {
    let dispatches = 0;
    const port = effectPort({
      dispatch: async (input) => {
        dispatches += 1;
        expect(input).toStrictEqual({
          custodyId: "custody_external_dispatch_01",
          operationId: "notify.create",
          targetId: "target_01",
          targetRevision: 5,
          requestArtifactRef: requestArtifact,
          idempotencyKey: "effect_01",
        });
        return {
          schemaVersion: 1,
          custodyId: input.custodyId,
          idempotencyKey: input.idempotencyKey,
          outcome: "committed",
          evidenceDigest: digest,
        };
      },
    });
    const { database, service } = fixture({ registry: [port] });
    const request = effectRequest("external_dispatch_01", registeredIntent());
    seedGenericCustody(database, request, "custody_external_dispatch_01");
    const handle = service.prepareInTransaction(request);
    database.prepare(`
      UPDATE operator_effect_custody SET state='dispatching' WHERE custody_id=? AND state='prepared'
    `).run(handle.custodyId);

    await expect(service.dispatchPrepared(handle)).resolves.toMatchObject({
      status: "committed",
      afterState: {
        schemaVersion: 1,
        externalEffect: "committed",
        custodyId: handle.custodyId,
        evidenceDigest: digest,
        rawOutcomeDigest: expect.stringMatching(/^sha256:[a-f0-9]{64}$/u),
      },
    });
    expect(dispatches).toBe(1);
    expect(database.prepare(`
      SELECT state, effect_path, effect_digest, outcome_json
        FROM operator_effect_custody WHERE custody_id=?
    `).get(handle.custodyId)).toMatchObject({
      state: "terminal",
      effect_path: `.agent-fabric/operator-effects/${handle.custodyId}.json`,
      effect_digest: expect.stringMatching(/^sha256:[a-f0-9]{64}$/u),
      outcome_json: expect.not.stringContaining("requests/effect.json"),
    });
  });

  it("rejects a changed request artifact digest before adapter dispatch", async () => {
    let dispatches = 0;
    const port = effectPort({ dispatch: async () => { dispatches += 1; return null; } });
    const changedArtifact = parseArtifactRef({
      path: requestArtifact.path,
      digest: `sha256:${"b".repeat(64)}`,
    }, "test.changedArtifact");
    const evidence: ExternalEffectEvidencePort = {
      inspectArtifact: async () => changedArtifact,
      inspectAcceptedDeliveryReceipt: async () => null,
    };
    const { database, service } = fixture({ registry: [port], evidence });
    const request = effectRequest("external_artifact_stale_01", registeredIntent());
    seedGenericCustody(database, request, "custody_external_artifact_stale_01");
    const handle = service.prepareInTransaction(request);
    database.prepare("UPDATE operator_effect_custody SET state='dispatching' WHERE custody_id=?")
      .run(handle.custodyId);

    await expect(service.dispatchPrepared(handle)).resolves.toStrictEqual({
      status: "rejected",
      code: "external-contract-stale",
      evidenceRefs: [],
    });
    expect(dispatches).toBe(0);
    expect(database.prepare("SELECT state FROM operator_effect_custody WHERE custody_id=?").get(handle.custodyId))
      .toStrictEqual({ state: "no-effect" });
  });

  it("dispatches promotion only after re-reading exact gate and accepted delivery evidence", async () => {
    const gate = releaseGate();
    let dispatches = 0;
    let receiptReads = 0;
    const port = effectPort({
      dispatch: async (input) => {
        dispatches += 1;
        expect(input).toMatchObject({
          custodyId: "custody_promotion_01",
          operationId: "release",
          targetId: "local:test",
          targetRevision: 8,
          requestArtifactRef: requestArtifact,
          idempotencyKey: expect.stringMatching(/^promotion:[a-f0-9]{64}$/u),
        });
        return {
          schemaVersion: 1,
          custodyId: input.custodyId,
          idempotencyKey: input.idempotencyKey,
          outcome: "committed",
          evidenceDigest: digest,
        };
      },
    });
    const evidence: ExternalEffectEvidencePort = {
      inspectArtifact: async () => { throw new Error("promotion uses accepted delivery evidence"); },
      inspectAcceptedDeliveryReceipt: async (ref) => {
        receiptReads += 1;
        return {
          status: "accepted",
          receiptRef: ref,
          artifactDigest: digest,
          promotionAction: "release",
          target: "local:test",
        };
      },
    };
    const { database, service } = fixture({ registry: [port], gate, evidence });
    const request = effectRequest("promotion_dispatch_01", promotionIntent());
    seedGenericCustody(database, request, "custody_promotion_01");
    const handle = service.prepareInTransaction(request);
    database.prepare(`
      UPDATE operator_effect_custody SET state='dispatching' WHERE custody_id=? AND state='prepared'
    `).run(handle.custodyId);

    await expect(service.dispatchPrepared(handle)).resolves.toMatchObject({
      status: "committed",
      afterState: { externalEffect: "committed", custodyId: handle.custodyId },
    });
    expect({ dispatches, receiptReads }).toStrictEqual({ dispatches: 1, receiptReads: 1 });
    expect(database.prepare(`
      SELECT effect_kind, release_gate_id, release_gate_revision,
             release_binding_digest, operation_id, target_id, target_revision
        FROM operator_external_effect_bindings WHERE custody_id=?
    `).get(handle.custodyId)).toMatchObject({
      effect_kind: "promotion",
      release_gate_id: gate.gateId,
      release_gate_revision: gate.revision,
      release_binding_digest: expect.stringMatching(/^sha256:[a-f0-9]{64}$/u),
      operation_id: "release",
      target_id: "local:test",
      target_revision: 8,
    });
  });

  it.each([
    ["receipt", { receiptRef: parseArtifactRef({ path: requestArtifact.path, digest: `sha256:${"b".repeat(64)}` }, "test.otherReceipt") }],
    ["artifact", { artifactDigest: parseSha256Digest(`sha256:${"b".repeat(64)}`, "test.otherArtifactDigest") }],
    ["action", { promotionAction: "deploy" }],
    ["target", { target: "production" }],
  ])("rejects a promotion with changed %s evidence before adapter dispatch", async (_label, changed) => {
    const gate = releaseGate();
    let dispatches = 0;
    const port = effectPort({ dispatch: async () => { dispatches += 1; return null; } });
    const evidence: ExternalEffectEvidencePort = {
      inspectArtifact: async () => null,
      inspectAcceptedDeliveryReceipt: async (ref) => ({
        status: "accepted",
        receiptRef: ref,
        artifactDigest: digest,
        promotionAction: "release",
        target: "local:test",
        ...changed,
      }),
    };
    const { database, service } = fixture({ registry: [port], gate, evidence });
    const request = effectRequest(`promotion_stale_${_label}`, promotionIntent());
    seedGenericCustody(database, request, `custody_promotion_stale_${_label}`);
    const handle = service.prepareInTransaction(request);
    database.prepare("UPDATE operator_effect_custody SET state='dispatching' WHERE custody_id=?")
      .run(handle.custodyId);

    await expect(service.dispatchPrepared(handle)).resolves.toMatchObject({
      status: "rejected",
      code: "release-binding-mismatch",
    });
    expect(dispatches).toBe(0);
  });

  it("rejects a release gate revision changed after evidence read and before dispatch", async () => {
    const gate = releaseGate();
    let currentGate = gate;
    let dispatches = 0;
    const port = effectPort({ dispatch: async () => { dispatches += 1; return null; } });
    const evidence: ExternalEffectEvidencePort = {
      inspectArtifact: async () => null,
      inspectAcceptedDeliveryReceipt: async (ref) => {
        currentGate = releaseGate(4);
        return {
          status: "accepted",
          receiptRef: ref,
          artifactDigest: digest,
          promotionAction: "release",
          target: "local:test",
        };
      },
    };
    const { database } = fixture({ registry: [port], gate, evidence });
    const service = new ExternalEffectService({
      database,
      registry: [port],
      evidence,
      gates: { getGate: () => currentGate },
    });
    const request = effectRequest("promotion_gate_changed_01", promotionIntent());
    seedGenericCustody(database, request, "custody_promotion_gate_changed_01");
    const handle = service.prepareInTransaction(request);
    database.prepare("UPDATE operator_effect_custody SET state='dispatching' WHERE custody_id=?")
      .run(handle.custodyId);

    await expect(service.dispatchPrepared(handle)).resolves.toMatchObject({
      status: "rejected",
      code: "release-binding-mismatch",
    });
    expect(dispatches).toBe(0);
  });

  it("turns malformed oversized secret-bearing adapter output into bounded ambiguity", async () => {
    const canary = "RAW_SECRET_CANARY_7f72";
    const port = effectPort({
      dispatch: async (input) => ({
        schemaVersion: 1,
        custodyId: input.custodyId,
        idempotencyKey: input.idempotencyKey,
        outcome: "committed",
        evidenceDigest: digest,
        secretCanary: canary.repeat(8_000),
      }),
    });
    const { database, service } = fixture({ registry: [port] });
    const request = effectRequest("external_malformed_01", registeredIntent());
    seedGenericCustody(database, request, "custody_external_malformed_01");
    const handle = service.prepareInTransaction(request);
    database.prepare("UPDATE operator_effect_custody SET state='dispatching' WHERE custody_id=?")
      .run(handle.custodyId);

    const outcome = await service.dispatchPrepared(handle);
    expect(outcome).toMatchObject({ status: "ambiguous" });
    expect(JSON.stringify(outcome)).not.toContain(canary);
    const persisted = database.prepare(`
      SELECT custody.outcome_json, custody.effect_path, custody.effect_digest,
             binding.lookup_evidence_digest
        FROM operator_effect_custody custody
        JOIN operator_external_effect_bindings binding USING(custody_id)
       WHERE custody.custody_id=?
    `).get(handle.custodyId);
    expect(JSON.stringify(persisted)).not.toContain(canary);
    expect(Buffer.byteLength(JSON.stringify(persisted), "utf8")).toBeLessThan(4_096);
  });

  it("recovers prepared custody as no-effect without adapter dispatch or lookup", async () => {
    let dispatches = 0;
    let lookups = 0;
    const port = effectPort({
      dispatch: async () => { dispatches += 1; throw new Error("must not dispatch"); },
      lookup: async () => { lookups += 1; throw new Error("must not lookup"); },
    });
    const evidence: ExternalEffectEvidencePort = {
      inspectArtifact: async (ref) => ref,
      inspectAcceptedDeliveryReceipt: async () => null,
    };
    const { database, service } = fixture({ registry: [port], evidence });
    const request = effectRequest("external_prepared_recovery_01", registeredIntent());
    seedGenericCustody(database, request, "custody_external_prepared_recovery_01");
    service.prepareInTransaction(request);
    const restarted = new ExternalEffectService({
      database,
      registry: [port],
      evidence,
      gates: { getGate: () => { throw new Error("gate lookup is not expected"); } },
    });

    await expect(restarted.recover()).resolves.toMatchObject([{
      custodyId: "custody_external_prepared_recovery_01",
      priorState: "prepared",
      outcome: { status: "rejected", code: "external-contract-stale", evidenceRefs: [] },
    }]);
    expect({ dispatches, lookups }).toStrictEqual({ dispatches: 0, lookups: 0 });
    expect(database.prepare(`
      SELECT state, effect_path, effect_digest FROM operator_effect_custody WHERE custody_id=?
    `).get("custody_external_prepared_recovery_01")).toStrictEqual({
      state: "no-effect",
      effect_path: null,
      effect_digest: null,
    });
  });

  it("recovers a crash after dispatch by one lookup and never repeats the effect", async () => {
    let dispatches = 0;
    let lookups = 0;
    const port = effectPort({
      dispatch: async (input) => {
        dispatches += 1;
        return {
          schemaVersion: 1,
          custodyId: input.custodyId,
          idempotencyKey: input.idempotencyKey,
          outcome: "committed",
          evidenceDigest: digest,
        };
      },
      lookup: async (input) => {
        lookups += 1;
        return {
          schemaVersion: 1,
          custodyId: input.custodyId,
          idempotencyKey: input.idempotencyKey,
          outcome: "committed",
          evidenceDigest: digest,
        };
      },
    });
    const evidence: ExternalEffectEvidencePort = {
      inspectArtifact: async (ref) => ref,
      inspectAcceptedDeliveryReceipt: async () => null,
    };
    const { database } = fixture({ registry: [port], evidence });
    const crashing = new ExternalEffectService({
      database,
      registry: [port],
      evidence,
      gates: { getGate: () => { throw new Error("gate lookup is not expected"); } },
      fault: (label) => {
        if (label === "external-effect:after-dispatch") throw new Error("simulated process crash");
      },
    });
    const request = effectRequest("external_dispatch_crash_01", registeredIntent());
    seedGenericCustody(database, request, "custody_external_dispatch_crash_01");
    const handle = crashing.prepareInTransaction(request);
    database.prepare(`
      UPDATE operator_effect_custody SET state='dispatching' WHERE custody_id=? AND state='prepared'
    `).run(handle.custodyId);
    await expect(crashing.dispatchPrepared(handle)).rejects.toThrow("simulated process crash");
    expect(database.prepare(`
      SELECT state, outcome_json FROM operator_effect_custody WHERE custody_id=?
    `).get(handle.custodyId)).toStrictEqual({ state: "dispatching", outcome_json: null });

    const restarted = new ExternalEffectService({
      database,
      registry: [port],
      evidence,
      gates: { getGate: () => { throw new Error("gate lookup is not expected"); } },
    });
    await expect(restarted.recover()).resolves.toMatchObject([{
      custodyId: handle.custodyId,
      priorState: "dispatching",
      outcome: { status: "committed", afterState: { externalEffect: "committed" } },
    }]);
    await expect(restarted.recover()).resolves.toStrictEqual([]);
    expect({ dispatches, lookups }).toStrictEqual({ dispatches: 1, lookups: 1 });
    expect(database.prepare(`
      SELECT custody.state, binding.lookup_generation, binding.lookup_evidence_digest
        FROM operator_effect_custody custody
        JOIN operator_external_effect_bindings binding USING(custody_id)
       WHERE custody.custody_id=?
    `).get(handle.custodyId)).toMatchObject({
      state: "terminal",
      lookup_generation: 1,
      lookup_evidence_digest: expect.stringMatching(/^sha256:[a-f0-9]{64}$/u),
    });
  });

  it("recovers failed post-dispatch custody by lookup only", async () => {
    let dispatches = 0;
    let lookups = 0;
    const port = effectPort({
      dispatch: async () => { dispatches += 1; throw new Error("must not redispatch"); },
      lookup: async (input) => {
        lookups += 1;
        return {
          schemaVersion: 1,
          custodyId: input.custodyId,
          idempotencyKey: input.idempotencyKey,
          outcome: "committed",
          evidenceDigest: digest,
        };
      },
    });
    const evidence: ExternalEffectEvidencePort = {
      inspectArtifact: async (ref) => ref,
      inspectAcceptedDeliveryReceipt: async () => null,
    };
    const { database, service } = fixture({ registry: [port], evidence });
    const request = effectRequest("external_failed_recovery_01", registeredIntent());
    seedGenericCustody(database, request, "custody_external_failed_recovery_01");
    service.prepareInTransaction(request);
    database.prepare(`
      UPDATE operator_effect_custody SET state='failed' WHERE custody_id='custody_external_failed_recovery_01'
    `).run();

    await expect(service.recover()).resolves.toMatchObject([{
      custodyId: "custody_external_failed_recovery_01",
      priorState: "failed",
      outcome: { status: "committed" },
    }]);
    expect({ dispatches, lookups }).toEqual({ dispatches: 0, lookups: 1 });
    expect(database.prepare(`
      SELECT state FROM operator_effect_custody WHERE custody_id='custody_external_failed_recovery_01'
    `).get()).toEqual({ state: "terminal" });
  });

  it("returns a terminal stored effect without dispatch or lookup on replay", async () => {
    let dispatches = 0;
    let lookups = 0;
    const port = effectPort({
      dispatch: async (input) => {
        dispatches += 1;
        return {
          schemaVersion: 1,
          custodyId: input.custodyId,
          idempotencyKey: input.idempotencyKey,
          outcome: "committed",
          evidenceDigest: digest,
        };
      },
      lookup: async () => { lookups += 1; throw new Error("terminal replay must not lookup"); },
    });
    const { database, service } = fixture({ registry: [port] });
    const request = effectRequest("external_terminal_replay_01", registeredIntent());
    seedGenericCustody(database, request, "custody_external_terminal_replay_01");
    const handle = service.prepareInTransaction(request);
    database.prepare("UPDATE operator_effect_custody SET state='dispatching' WHERE custody_id=?")
      .run(handle.custodyId);
    const committed = await service.dispatchPrepared(handle);

    await expect(service.observe({ ...request, attemptGeneration: 2 })).resolves.toStrictEqual(committed);
    await expect(service.recover()).resolves.toStrictEqual([]);
    expect({ dispatches, lookups }).toStrictEqual({ dispatches: 1, lookups: 0 });
  });

});
