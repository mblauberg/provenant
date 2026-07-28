import type { ValidateFunction } from "ajv";
import { Ajv2020 } from "ajv/dist/2020.js";
import { createHash } from "node:crypto";
import {
  parseProjectSessionLaunchCurrentState,
  parseProjectSessionLaunchIntent,
  parseArtifactRef,
  type ProjectSessionLaunchCurrentState,
  type ProjectSessionLaunchIntent,
  type LaunchProviderActionJournalRefV1,
  type McpSeatProvisioningDescriptorV1,
  type ArtifactRef,
} from "@local/agent-fabric-protocol";
import type Database from "better-sqlite3";
import { isAbsolute } from "node:path";
import { realpathSync } from "node:fs";

import { ProjectFabricCoreError, type AuthenticatedOperatorContext } from "./contracts.js";
import { exactDigest, jsonEvidenceDigest } from "./provider-agent-custody.js";
import { assertSafeLaunchProviderInput } from "./provider-input-safety.js";
import {
  canonicalJson,
  integer,
  isRow,
  row,
  sha256,
  text,
  type Row,
} from "../persistence/row-codec.js";
import {
  ProviderActionAdmissionCoordinator,
  type ProviderActionTicket,
} from "../application/provider-action-admission.js";
import { assertProviderActionOwner, ProviderActionOwnerError } from "../application/provider-action-owner.js";
import {
  assertSameArtifact,
  computeLaunchResourceStateDigest,
  forbidden,
  isDeterministicClosedPreflightFailure,
  isProvedNoEffectOutcome,
  jsonArtifact,
  launchProviderActionJournalRefForCommand as launchProviderActionJournalRefForCommandImpl,
  parsePacket,
  parsePlan,
  readArtifact,
  sameAmounts,
  seatProvisioningDescriptorForCommand as seatProvisioningDescriptorForCommandImpl,
  stale,
  type Digest,
  type LaunchAdapterContract,
  type LaunchAdapterOutcome,
  type LaunchCustodyIntent,
  type LaunchDispatchHandle,
  type LaunchInspection,
} from "./launch-contracts.js";
import type { LaunchSettlement } from "./launch-settlement.js";

/**
 * Byte-moved from `launch-custody.ts` (issue #354, S4e, plan §2 "S4e"): the project-session
 * launch family's public workflow — prepare intent, inspect/preflight/prepare-in-transaction,
 * dispatch, lookup, and the read/journal/seat-provisioning projections. Settlement of the
 * dispatch/lookup outcome is delegated to the injected `LaunchSettlement` (see
 * `launch-settlement.ts`), preserving the exact original transaction boundary: settlement runs
 * inside `this.#database.transaction(...)` at the same call sites the monolithic service used.
 */

export type LaunchServiceAdapterEffectsPort = Readonly<{
  dispatch(handle: LaunchDispatchHandle): Promise<unknown>;
  lookup(input: Readonly<{
    providerAdapterId: string;
    providerActionId: string;
    providerContractDigest: Digest;
    attestationChallengeDigest: Digest;
  }>): Promise<unknown>;
}>;

export type LaunchServiceOptions = Readonly<{
  database: Database.Database;
  providerActionAdmission: ProviderActionAdmissionCoordinator;
  clock: () => number;
  fault: (label: string) => void;
  randomCapability: () => string;
  randomAttestationChallenge: () => string;
  fabricSocketPath: string;
  adapterContracts: { inspect(adapterId: string): Promise<LaunchAdapterContract> };
  adapterEffects: LaunchServiceAdapterEffectsPort;
  settlement: LaunchSettlement;
}>;

export class LaunchService {
  readonly #database: Database.Database;
  readonly #providerActionAdmission: ProviderActionAdmissionCoordinator;
  readonly #clock: () => number;
  readonly #fault: (label: string) => void;
  readonly #randomCapability: () => string;
  readonly #randomAttestationChallenge: () => string;
  readonly #fabricSocketPath: string;
  readonly #adapterContracts: LaunchServiceOptions["adapterContracts"];
  readonly #adapterEffects: LaunchServiceAdapterEffectsPort;
  readonly #settlement: LaunchSettlement;
  readonly #consumedHandles = new Set<string>();

  constructor(options: LaunchServiceOptions) {
    this.#database = options.database;
    this.#providerActionAdmission = options.providerActionAdmission;
    this.#clock = options.clock;
    this.#fault = options.fault;
    this.#randomCapability = options.randomCapability;
    this.#randomAttestationChallenge = options.randomAttestationChallenge;
    this.#fabricSocketPath = options.fabricSocketPath;
    if (!isAbsolute(this.#fabricSocketPath)) throw new TypeError("Fabric socket path must be absolute");
    this.#adapterContracts = options.adapterContracts;
    this.#adapterEffects = options.adapterEffects;
    this.#settlement = options.settlement;
  }

  releaseProviderActionPreflightAfterRollback(ticket: ProviderActionTicket, failure: unknown): void {
    if (ticket.disposition !== "resolving" || ticket.scope.kind !== "run-action") return;
    if (!isDeterministicClosedPreflightFailure(failure)) return;
    const actionExists = this.#database.prepare(`
      SELECT 1 FROM provider_actions WHERE run_id=? AND adapter_id=? AND action_id=?
    `).get(ticket.scope.runId, ticket.actionRef.adapterId, ticket.actionRef.actionId) !== undefined;
    if (actionExists) return;
    try {
      this.#providerActionAdmission.release(ticket, failure);
    } catch {
      // The outer preparation failure remains authoritative if release races.
    }
  }

  async prepareLaunchIntent(input: Readonly<{
    projectId: string;
    projectSessionId: string;
    expectedSessionGeneration: number;
    launchPacketRef: ArtifactRef;
  }>): Promise<LaunchCustodyIntent> {
    this.#fault("launch:intent-prepare:begin");
    const project = row(this.#database.prepare(`
      SELECT canonical_root, trust_record_digest, revision
        FROM projects WHERE project_id=?
    `).get(input.projectId), "launch project");
    const root = realpathSync(text(project, "canonical_root"));
    if (root !== text(project, "canonical_root")) forbidden("trusted project root is not canonical");
    const session = row(this.#database.prepare(`
      SELECT state, revision, generation, budget_ref, launch_packet_path, launch_packet_digest
        FROM project_sessions WHERE project_session_id=? AND project_id=?
    `).get(input.projectSessionId, input.projectId), "launch project session");
    if (integer(session, "generation") !== input.expectedSessionGeneration) {
      stale("launch project-session generation changed");
    }
    const sessionState = text(session, "state");
    let retryOf: ProjectSessionLaunchIntent["retryOf"];
    if (sessionState === "awaiting_launch") {
      assertSameArtifact(input.launchPacketRef, parseArtifactRef({
        path: text(session, "launch_packet_path"),
        digest: exactDigest(session.launch_packet_digest, "stored launch packet digest"),
      }, "stored launch packet"), "stored launch packet");
    } else if (sessionState === "launch_failed") {
      const failed = row(this.#database.prepare(`
        SELECT provider_adapter_id, provider_action_id
          FROM project_session_launch_custody
         WHERE project_session_id=?
         ORDER BY custody_attempt_generation DESC LIMIT 1
      `).get(input.projectSessionId), "failed launch custody");
      retryOf = {
        providerAdapterId: text(failed, "provider_adapter_id"),
        providerActionId: text(failed, "provider_action_id") as never,
      };
    } else {
      throw new ProjectFabricCoreError(
        "LIFECYCLE_PRECONDITION_FAILED",
        "launch preparation requires awaiting_launch or proved launch_failed",
      );
    }
    const packet = parsePacket(jsonArtifact(root, input.launchPacketRef, "launch packet"), root);
    parsePlan(jsonArtifact(root, packet.resourcePlanRef, "launch resource plan"));
    const contract = await this.#adapterContracts.inspect(packet.provider.adapterId);
    const intent = parseProjectSessionLaunchIntent({
      kind: "project-session-launch",
      projectId: input.projectId,
      projectSessionId: input.projectSessionId,
      expectedProjectRevision: integer(project, "revision"),
      expectedSessionRevision: integer(session, "revision"),
      expectedSessionGeneration: integer(session, "generation"),
      trustRecordDigest: exactDigest(project.trust_record_digest, "launch trust record digest"),
      launchPacketRef: input.launchPacketRef,
      authorityRef: `sha256:${sha256(canonicalJson(packet.chairAuthority))}`,
      budgetRef: text(session, "budget_ref"),
      resourcePlanRef: packet.resourcePlanRef,
      providerAdapterId: packet.provider.adapterId,
      providerActionId: packet.provider.actionId,
      providerContractDigest: `sha256:${sha256(canonicalJson(contract))}`,
      resourceStateDigest: computeLaunchResourceStateDigest(
        this.#database,
        input.projectId,
        input.projectSessionId,
      ),
      ...(retryOf === undefined ? {} : { retryOf }),
    });
    await this.inspect(intent);
    this.#fault("launch:intent-prepare:complete");
    return intent;
  }

  async inspect(intent: LaunchCustodyIntent): Promise<LaunchInspection> {
    const project = row(this.#database.prepare(`
      SELECT canonical_root, trust_record_digest, revision FROM projects WHERE project_id=?
    `).get(intent.projectId), "launch project");
    const root = realpathSync(text(project, "canonical_root"));
    if (root !== text(project, "canonical_root")) forbidden("trusted project root is not canonical");
    if (integer(project, "revision") !== intent.expectedProjectRevision) stale("launch project revision changed");
    if (project.trust_record_digest !== intent.trustRecordDigest) stale("launch trust record changed");
    const session = row(this.#database.prepare(`
      SELECT * FROM project_sessions WHERE project_session_id=? AND project_id=?
    `).get(intent.projectSessionId, intent.projectId), "launch project session");
    if (
      integer(session, "revision") !== intent.expectedSessionRevision ||
      integer(session, "generation") !== intent.expectedSessionGeneration
    ) stale("launch project-session revision or generation changed");
    const sessionState = text(session, "state");
    if (intent.retryOf === undefined && sessionState !== "awaiting_launch") {
      throw new ProjectFabricCoreError("LIFECYCLE_PRECONDITION_FAILED", "initial launch requires awaiting_launch");
    }
    if (intent.retryOf !== undefined && sessionState !== "launch_failed") {
      throw new ProjectFabricCoreError("LIFECYCLE_PRECONDITION_FAILED", "launch retry requires proved launch_failed");
    }
    let failedAttempt: Row | undefined;
    if (intent.retryOf === undefined) {
      assertSameArtifact(intent.launchPacketRef, parseArtifactRef({
        path: text(session, "launch_packet_path"),
        digest: exactDigest(session.launch_packet_digest, "stored launch packet digest"),
      }, "stored launch packet"), "stored launch packet");
    } else {
      failedAttempt = row(this.#database.prepare(`
        SELECT c.*, p.status, p.execution_count, p.effect_count, p.idempotency_proven, p.result_json
          FROM project_session_launch_custody c
          JOIN provider_actions p
            ON p.adapter_id=c.provider_adapter_id AND p.action_id=c.provider_action_id
         WHERE c.project_session_id=?
         ORDER BY c.custody_attempt_generation DESC LIMIT 1
      `).get(intent.projectSessionId), "failed launch custody");
      if (
        text(failedAttempt, "provider_adapter_id") !== intent.retryOf.providerAdapterId ||
        text(failedAttempt, "provider_action_id") !== intent.retryOf.providerActionId ||
        text(failedAttempt, "status") !== "terminal" ||
        integer(failedAttempt, "effect_count") !== 0 ||
        integer(failedAttempt, "idempotency_proven") !== 1 ||
        !isProvedNoEffectOutcome(failedAttempt)
      ) {
        throw new ProjectFabricCoreError("CONFLICT", "launch retry does not bind the exact proved failed attempt");
      }
      assertSameArtifact(parseArtifactRef({
        path: text(session, "launch_packet_path"),
        digest: exactDigest(session.launch_packet_digest, "stored failed launch packet digest"),
      }, "stored failed launch packet"), parseArtifactRef({
        path: text(failedAttempt, "launch_packet_path"),
        digest: exactDigest(failedAttempt.launch_packet_digest, "failed custody packet digest"),
      }, "failed custody packet"), "failed attempt packet");
    }
    if (text(session, "budget_ref") !== intent.budgetRef) stale("launch budget reference changed");
    const packet = parsePacket(jsonArtifact(root, intent.launchPacketRef, "launch packet"), root);
    const plan = parsePlan(jsonArtifact(root, intent.resourcePlanRef, "launch resource plan"));
    assertSameArtifact(packet.resourcePlanRef, intent.resourcePlanRef, "packet resource plan");
    if (
      packet.projectId !== intent.projectId || packet.projectSessionId !== intent.projectSessionId ||
      plan.projectId !== intent.projectId || plan.projectSessionId !== intent.projectSessionId ||
      packet.runId !== plan.runId || packet.topologyMode !== text(session, "mode") ||
      packet.budgetRef !== intent.budgetRef || plan.budgetRef !== intent.budgetRef ||
      packet.provider.adapterId !== intent.providerAdapterId ||
      packet.provider.actionId !== intent.providerActionId ||
      packet.provider.contractDigest !== intent.providerContractDigest
    ) stale("launch packet, plan, intent or session identity changed");
    if (
      failedAttempt !== undefined &&
      (
        packet.runId === text(failedAttempt, "coordination_run_id") ||
        (
          packet.provider.adapterId === text(failedAttempt, "provider_adapter_id") &&
          packet.provider.actionId === text(failedAttempt, "provider_action_id")
        )
      )
    ) {
      throw new ProjectFabricCoreError("DEDUPE_CONFLICT", "launch retry requires a new run and provider action identity");
    }
    if (`sha256:${sha256(canonicalJson(packet.chairAuthority))}` !== intent.authorityRef) {
      stale("launch chair authority digest changed");
    }
    if (!sameAmounts(packet.chairAuthority.budget, plan.scopes.coordinationRun.limits)) {
      forbidden("launch chair authority budget must equal coordination-run limits");
    }
    if (Date.parse(packet.chairAuthority.expiresAt) <= this.#clock()) {
      throw new ProjectFabricCoreError("CAPABILITY_EXPIRED", "launch chair authority is expired");
    }
    const contract = await this.#adapterContracts.inspect(intent.providerAdapterId);
    if (
      contract.schemaVersion !== 1 ||
      packet.provider.inputSchemaId !== contract.inputSchemaId ||
      `sha256:${sha256(canonicalJson(contract))}` !== intent.providerContractDigest
    ) stale("launch provider contract changed");
    assertSafeLaunchProviderInput(packet.provider.input);
    const ajv = new Ajv2020({ allErrors: true, strict: true });
    let validate: ValidateFunction;
    try {
      validate = ajv.compile(contract.publicPayloadSchema);
    } catch {
      throw new ProjectFabricCoreError("PROTOCOL_INVALID", "registered launch input schema is invalid");
    }
    if (!validate(packet.provider.input)) {
      throw new ProjectFabricCoreError("PROTOCOL_INVALID", "launch provider input does not match its registered strict schema");
    }
    const resourceStateDigest = computeLaunchResourceStateDigest(this.#database, intent.projectId, intent.projectSessionId);
    if (resourceStateDigest !== intent.resourceStateDigest) stale("launch resource state changed");
    const launchBindingDigest = `sha256:${sha256(canonicalJson({
      intent,
      packet,
      plan,
      projectRevision: integer(project, "revision"),
      sessionRevision: integer(session, "revision"),
      sessionGeneration: integer(session, "generation"),
    }))}` as Digest;
    return {
      intent,
      canonicalProjectRoot: root,
      packet,
      plan,
      launchBindingDigest,
      inspectedProjectRevision: integer(project, "revision"),
      inspectedSessionRevision: integer(session, "revision"),
      inspectedSessionGeneration: integer(session, "generation"),
    };
  }

  #preflightLaunchInCurrentTransaction(input: Readonly<{
    inspection: LaunchInspection;
    principal: AuthenticatedOperatorContext;
  }>): ProviderActionTicket {
    if (!this.#database.inTransaction) throw new Error("launch preflight requires the operator command transaction");
    const { intent, packet, plan } = input.inspection;
    return this.#providerActionAdmission.preflight({
      actionRef: {
        adapterId: intent.providerAdapterId,
        actionId: intent.providerActionId,
      },
      scope: { kind: "run-action", runId: packet.runId },
      principal: input.principal,
      canonicalInput: {
        schemaVersion: 1,
        operation: "launch-chair",
        intent,
        packet,
        resourcePlan: plan,
      },
    });
  }

  prepareInTransaction(input: Readonly<{
    inspection: LaunchInspection;
    operatorId: string;
    operatorCommandId: string;
    principal: AuthenticatedOperatorContext;
  }>): LaunchDispatchHandle {
    if (!this.#database.inTransaction) throw new Error("launch preparation requires the operator command transaction");
    const { inspection } = input;
    const { intent, packet } = inspection;
    this.#revalidateInspection(inspection);
    const command = row(this.#database.prepare(`
      SELECT project_id, project_session_id, operation, status
        FROM operator_commands WHERE operator_id=? AND command_id=?
    `).get(input.operatorId, input.operatorCommandId), "launch operator preparation");
    if (
      text(command, "project_id") !== intent.projectId ||
      text(command, "project_session_id") !== intent.projectSessionId ||
      text(command, "operation") !== "launch" || text(command, "status") !== "committed"
    ) forbidden("operator preparation does not own this launch");
    const existing = this.#database.prepare(`
      SELECT 1 FROM project_session_launch_custody
       WHERE operator_id=? AND operator_command_id=?
    `).get(input.operatorId, input.operatorCommandId);
    if (existing !== undefined) {
      throw new ProjectFabricCoreError("DEDUPE_CONFLICT", "launch secret cannot be recovered or redisclosed on replay");
    }
    const attempt = integer(row(this.#database.prepare(`
      SELECT COALESCE(MAX(custody_attempt_generation), 0) + 1 AS generation
        FROM project_session_launch_custody WHERE project_session_id=?
    `).get(intent.projectSessionId), "launch attempt generation"), "generation");
    if ((intent.retryOf === undefined && attempt !== 1) || (intent.retryOf !== undefined && attempt < 2)) {
      throw new ProjectFabricCoreError("CONFLICT", "launch attempt generation does not match retry state");
    }
    const now = this.#clock();
    const authorityId = `launch-authority:${packet.runId}:${String(attempt)}`;
    const chairLeaseId = `chair:${packet.runId}:1`;
    const reservationId = `launch-reservation:${sha256(`${intent.providerAdapterId}\0${intent.providerActionId}`).slice(0, 40)}`;
    const capability = this.#randomCapability();
    if (typeof capability !== "string" || capability.length < 16) throw new Error("random launch capability is too short");
    const capabilityHash = sha256(capability);
    const attestationChallenge = this.#randomAttestationChallenge();
    if (!/^[0-9a-f]{64}$/u.test(attestationChallenge)) {
      throw new Error("random launch attestation challenge must contain exactly 32 bytes");
    }
    const attestationChallengeDigest = `sha256:${createHash("sha256")
      .update(Buffer.from(attestationChallenge, "hex"))
      .digest("hex")}` as Digest;
    const expiresAt = Date.parse(packet.chairAuthority.expiresAt);

    const changed = this.#database.prepare(`
      UPDATE project_sessions
         SET state='launching', membership_revision=membership_revision+1,
             revision=revision+1,
             launch_packet_path=?, launch_packet_digest=?, updated_at=?
       WHERE project_session_id=? AND project_id=? AND revision=? AND generation=?
         AND state=?
    `).run(
      intent.launchPacketRef.path,
      intent.launchPacketRef.digest,
      now,
      intent.projectSessionId,
      intent.projectId,
      intent.expectedSessionRevision,
      intent.expectedSessionGeneration,
      intent.retryOf === undefined ? "awaiting_launch" : "launch_failed",
    );
    if (changed.changes !== 1) stale("launch session changed during commit");
    this.#fault("launch:prepare:session");

    this.#database.prepare(`
      INSERT INTO runs(
        run_id, chair_agent_id, workspace_root, project_run_directory, created_at,
        project_session_id, lifecycle_state, revision, chair_generation, chair_lease_id,
        authority_ref, budget_ref, dependency_revision, topology_slot
        , project_run_directory_basis
      ) VALUES (?, ?, ?, ?, ?, ?, 'launching', 1, 1, ?, ?, ?, 1, ?, 'project-relative')
    `).run(
      packet.runId,
      packet.chairAgentId,
      inspection.canonicalProjectRoot,
      packet.projectRunDirectory,
      now,
      intent.projectSessionId,
      chairLeaseId,
      intent.authorityRef,
      intent.budgetRef,
      packet.topologyMode === "coordinated" ? 1 : null,
    );
    this.#fault("launch:prepare:run");
    const providerActionTicket = this.#preflightLaunchInCurrentTransaction({
      inspection,
      principal: input.principal,
    });
    const authorityJson = canonicalJson(packet.chairAuthority);
    this.#database.prepare(`
      INSERT INTO authorities(authority_id, run_id, parent_authority_id, authority_json, authority_hash, created_at)
      VALUES (?, ?, NULL, ?, ?, ?)
    `).run(authorityId, packet.runId, authorityJson, sha256(authorityJson), now);
    const insertBudget = this.#database.prepare(`
      INSERT INTO authority_budget(authority_id, unit_key, granted, reserved, consumed, usage_unknown)
      VALUES (?, ?, ?, 0, 0, 0)
    `);
    for (const [unit, amount] of Object.entries(packet.chairAuthority.budget)) insertBudget.run(authorityId, unit, amount);
    this.#fault("launch:prepare:authority");
    this.#database.prepare(`
      INSERT INTO agents(run_id, agent_id, parent_agent_id, authority_id, provider_session_ref, lifecycle)
      VALUES (?, ?, NULL, ?, NULL, 'ready')
    `).run(packet.runId, packet.chairAgentId, authorityId);
    this.#database.prepare("INSERT INTO mailbox_state(run_id, recipient_id) VALUES (?, ?)")
      .run(packet.runId, packet.chairAgentId);
    this.#database.prepare(`
      INSERT INTO agent_adapter_bindings(run_id, agent_id, adapter_id, contract_version, bound_at)
      VALUES (?, ?, ?, 1, ?)
    `).run(packet.runId, packet.chairAgentId, intent.providerAdapterId, now);
    this.#database.prepare(`
      INSERT INTO run_chair_leases(
        project_session_id, run_id, lease_id, holder_agent_id, generation, status, updated_at
      ) VALUES (?, ?, ?, ?, 1, 'active', ?)
    `).run(intent.projectSessionId, packet.runId, chairLeaseId, packet.chairAgentId, now);
    this.#database.prepare(`
      INSERT INTO capabilities(token_hash, run_id, agent_id, principal_generation, expires_at)
      VALUES (?, ?, ?, 1, ?)
    `).run(capabilityHash, packet.runId, packet.chairAgentId, expiresAt);
    this.#fault("launch:prepare:chair");

    this.#ensureScopes(inspection, now);
    this.#fault("launch:prepare:scopes");
    this.#reserve(inspection, reservationId, now);
    this.#fault("launch:prepare:reservation");

    const publicPayload = {
      schemaVersion: 1,
      providerContractDigest: intent.providerContractDigest,
      inputSchemaId: packet.provider.inputSchemaId,
      input: packet.provider.input,
    };
    const payloadJson = canonicalJson(publicPayload);
    this.#providerActionAdmission.admitUnroutedInCurrentTransaction(providerActionTicket, {
      runId: packet.runId,
      actionId: intent.providerActionId,
      adapterId: intent.providerAdapterId,
      operation: "launch-chair",
      targetAgentId: packet.chairAgentId,
      identityHash: sha256(canonicalJson({ adapterId: intent.providerAdapterId, actionId: intent.providerActionId })),
      payloadHash: sha256(payloadJson),
      payloadJson,
      status: "prepared",
      historyJson: '["prepared"]',
      executionCount: 0,
      updatedAt: now,
    }, "launch", () => {
      this.#database.prepare(`
        INSERT INTO project_session_launch_custody(
          project_session_id, custody_attempt_generation, coordination_run_id,
          chair_agent_id, chair_lease_id, operator_id, operator_command_id,
          provider_adapter_id, provider_action_id, capability_hash, capability_expires_at,
          attestation_challenge_digest,
          reservation_id, launch_packet_path, launch_packet_digest, authority_ref,
          budget_ref, resource_plan_path, resource_plan_digest, expected_project_revision,
          expected_session_revision, expected_session_generation, trust_record_digest,
          provider_contract_digest, resource_state_digest, launch_binding_digest,
          retry_of_provider_adapter_id, retry_of_provider_action_id, created_at
        ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
      `).run(
        intent.projectSessionId,
        attempt,
        packet.runId,
        packet.chairAgentId,
        chairLeaseId,
        input.operatorId,
        input.operatorCommandId,
        intent.providerAdapterId,
        intent.providerActionId,
        capabilityHash,
        expiresAt,
        attestationChallengeDigest,
        reservationId,
        intent.launchPacketRef.path,
        intent.launchPacketRef.digest,
        intent.authorityRef,
        intent.budgetRef,
        intent.resourcePlanRef.path,
        intent.resourcePlanRef.digest,
        intent.expectedProjectRevision,
        intent.expectedSessionRevision,
        intent.expectedSessionGeneration,
        intent.trustRecordDigest,
        intent.providerContractDigest,
        intent.resourceStateDigest,
        inspection.launchBindingDigest,
        intent.retryOf?.providerAdapterId ?? null,
        intent.retryOf?.providerActionId ?? null,
        now,
      );
    });
    this.#fault("launch:prepare:provider-action");

    const insertMembership = this.#database.prepare(`
      INSERT INTO project_session_memberships(
        project_session_id, coordination_run_id, member_kind, member_id, member_adapter_id,
        required, state, revision, created_at, updated_at
      ) VALUES (?, ?, ?, ?, ?, 1, 'active', 1, ?, ?)
    `);
    insertMembership.run(intent.projectSessionId, packet.runId, "coordination-run", packet.runId, "", now, now);
    insertMembership.run(intent.projectSessionId, packet.runId, "lease", chairLeaseId, "", now, now);
    insertMembership.run(
      intent.projectSessionId,
      packet.runId,
      "provider-action",
      intent.providerActionId,
      intent.providerAdapterId,
      now,
      now,
    );
    this.#database.prepare("INSERT INTO run_metadata(run_id, execution_profile) VALUES (?, 'headless')")
      .run(packet.runId);
    this.#fault("launch:prepare:memberships");

    this.#fault("launch:prepare:custody");
    return {
      schemaVersion: 1,
      providerAdapterId: intent.providerAdapterId,
      providerActionId: intent.providerActionId,
      providerContractDigest: intent.providerContractDigest,
      publicPayload: packet.provider.input,
      capability,
      socketPath: this.#fabricSocketPath,
      attestationChallenge,
      attestationChallengeDigest,
      expectedPrincipal: {
        agentId: packet.chairAgentId,
        projectSessionId: intent.projectSessionId,
        runId: packet.runId,
        principalGeneration: 1,
      },
    };
  }

  async dispatchPrepared(handle: LaunchDispatchHandle): Promise<LaunchAdapterOutcome> {
    const launchRun = row(this.#database.prepare(`
      SELECT coordination_run_id FROM project_session_launch_custody
       WHERE provider_adapter_id=? AND provider_action_id=?
    `).get(handle.providerAdapterId, handle.providerActionId), "launch custody owner");
    assertProviderActionOwner(this.#database, {
      runId: text(launchRun, "coordination_run_id"),
      adapterId: handle.providerAdapterId,
      actionId: handle.providerActionId,
    }, "launch");
    const key = `${handle.providerAdapterId}\0${handle.providerActionId}`;
    if (this.#consumedHandles.has(key)) {
      throw new ProjectFabricCoreError("DEDUPE_CONFLICT", "launch handoff is one-use");
    }
    const changed = this.#database.prepare(`
      UPDATE provider_actions
         SET status='dispatched', history_json='["prepared","dispatched"]',
             execution_count=1, journal_revision=journal_revision+1, updated_at=?
       WHERE adapter_id=? AND action_id=? AND status='prepared'
         AND EXISTS (
           SELECT 1 FROM project_session_launch_custody c
            WHERE c.provider_adapter_id=provider_actions.adapter_id
              AND c.provider_action_id=provider_actions.action_id
              AND c.provider_contract_digest=?
         )
    `).run(this.#clock(), handle.providerAdapterId, handle.providerActionId, handle.providerContractDigest);
    if (changed.changes !== 1) throw new ProjectFabricCoreError("CONFLICT", "launch action is not prepared");
    this.#consumedHandles.add(key);
    const custody = row(this.#database.prepare(`
      SELECT * FROM project_session_launch_custody
       WHERE provider_adapter_id=? AND provider_action_id=? AND provider_contract_digest=?
    `).get(
      handle.providerAdapterId,
      handle.providerActionId,
      handle.providerContractDigest,
    ), "launch custody");
    let contract: LaunchAdapterContract;
    try {
      contract = await this.#adapterContracts.inspect(handle.providerAdapterId);
      if (`sha256:${sha256(canonicalJson(contract))}` !== handle.providerContractDigest) {
        throw new Error("launch provider contract changed");
      }
    } catch (error: unknown) {
      if (error instanceof ProviderActionOwnerError) throw error;
      const outcome = this.#settlement.ambiguousOutcome(
        custody,
        "conflict",
        jsonEvidenceDigest(error instanceof Error ? error.message : error),
        "dispatch-return",
      );
      this.#database.transaction(() => this.#settlement.applyOutcome(custody, outcome))();
      return outcome;
    }
    let raw: unknown;
    try {
      assertProviderActionOwner(this.#database, {
        runId: text(custody, "coordination_run_id"),
        adapterId: handle.providerAdapterId,
        actionId: handle.providerActionId,
      }, "launch");
      raw = await this.#adapterEffects.dispatch(handle);
    } catch (error: unknown) {
      if (error instanceof ProviderActionOwnerError) throw error;
      raw = this.#settlement.ambiguousOutcome(
        custody,
        "adapter-error",
        jsonEvidenceDigest(error instanceof Error ? error.message : error),
        "dispatch-return",
      );
    }
    const outcome = this.#settlement.normaliseOutcome(custody, raw, "dispatch-return", contract);
    this.#database.transaction(() => this.#settlement.applyOutcome(custody, outcome))();
    return outcome;
  }

  async lookup(input: Readonly<{
    providerAdapterId: string;
    providerActionId: string;
    providerContractDigest: Digest;
  }>): Promise<unknown> {
    const custody = row(this.#database.prepare(`
      SELECT coordination_run_id,attestation_challenge_digest FROM project_session_launch_custody
       WHERE provider_adapter_id=? AND provider_action_id=? AND provider_contract_digest=?
    `).get(input.providerAdapterId, input.providerActionId, input.providerContractDigest), "launch custody");
    assertProviderActionOwner(this.#database, {
      runId: text(custody, "coordination_run_id"),
      adapterId: input.providerAdapterId,
      actionId: input.providerActionId,
    }, "launch");
    return await this.#adapterEffects.lookup({
      ...input,
      attestationChallengeDigest: exactDigest(
        custody.attestation_challenge_digest,
        "custody attestation challenge digest",
      ),
    });
  }

  async readCurrentState(intent: LaunchCustodyIntent): Promise<ProjectSessionLaunchCurrentState> {
    const inspection = await this.inspect(intent);
    const session = row(this.#database.prepare(`
      SELECT state, launch_packet_path, launch_packet_digest
        FROM project_sessions WHERE project_session_id=? AND project_id=?
    `).get(intent.projectSessionId, intent.projectId), "launch project session");
    const sessionState = text(session, "state");
    const common = {
      schemaVersion: 1 as const,
      projectId: intent.projectId,
      projectRevision: inspection.inspectedProjectRevision,
      projectSessionId: intent.projectSessionId,
      sessionRevision: inspection.inspectedSessionRevision,
      sessionGeneration: inspection.inspectedSessionGeneration,
      currentLaunchPacketRef: {
        path: text(session, "launch_packet_path"),
        digest: exactDigest(session.launch_packet_digest, "stored launch packet digest"),
      },
      trustRecordDigest: intent.trustRecordDigest,
      providerAdapterId: intent.providerAdapterId,
      providerContractDigest: intent.providerContractDigest,
      resourceStateDigest: intent.resourceStateDigest,
    };
    if (sessionState === "awaiting_launch") {
      return parseProjectSessionLaunchCurrentState({
        ...common,
        sessionState: "awaiting_launch",
        provedFailedAttempt: null,
      });
    }
    if (sessionState !== "launch_failed") throw new ProjectFabricCoreError("CONFLICT", "launch state is not inspectable");
    const failed = row(this.#database.prepare(`
      SELECT c.provider_adapter_id, c.provider_action_id, p.status, p.execution_count,
             p.effect_count, p.result_json
        FROM project_session_launch_custody c
        JOIN provider_actions p
          ON p.adapter_id=c.provider_adapter_id AND p.action_id=c.provider_action_id
       WHERE c.project_session_id=?
       ORDER BY c.custody_attempt_generation DESC LIMIT 1
    `).get(intent.projectSessionId), "proved failed launch attempt");
    if (text(failed, "status") !== "terminal" || integer(failed, "effect_count") !== 0) {
      throw new ProjectFabricCoreError("CONFLICT", "latest launch attempt is not a proved failure");
    }
    return parseProjectSessionLaunchCurrentState({
      ...common,
      sessionState: "launch_failed",
      provedFailedAttempt: {
        providerAdapterId: text(failed, "provider_adapter_id"),
        providerActionId: text(failed, "provider_action_id") as never,
      },
    });
  }

  launchProviderActionJournalRefForCommand(
    operatorId: string,
    commandId: string,
  ): LaunchProviderActionJournalRefV1 {
    return launchProviderActionJournalRefForCommandImpl(this.#database, operatorId, commandId);
  }

  seatProvisioningDescriptorForCommand(
    operatorId: string,
    commandId: string,
  ): McpSeatProvisioningDescriptorV1 {
    return seatProvisioningDescriptorForCommandImpl(this.#database, operatorId, commandId);
  }

  #revalidateInspection(inspection: LaunchInspection): void {
    const { intent } = inspection;
    const project = row(this.#database.prepare(`
      SELECT canonical_root, trust_record_digest, revision FROM projects WHERE project_id=?
    `).get(intent.projectId), "launch project");
    const session = row(this.#database.prepare(`
      SELECT revision, generation, state, budget_ref, launch_packet_path, launch_packet_digest
        FROM project_sessions WHERE project_session_id=? AND project_id=?
    `).get(intent.projectSessionId, intent.projectId), "launch session");
    if (
      integer(project, "revision") !== inspection.inspectedProjectRevision ||
      integer(session, "revision") !== inspection.inspectedSessionRevision ||
      integer(session, "generation") !== inspection.inspectedSessionGeneration ||
      project.trust_record_digest !== intent.trustRecordDigest ||
      text(session, "budget_ref") !== intent.budgetRef ||
      computeLaunchResourceStateDigest(this.#database, intent.projectId, intent.projectSessionId) !== intent.resourceStateDigest
    ) stale("launch binding changed after preview");
    if (text(project, "canonical_root") !== inspection.canonicalProjectRoot) stale("launch project root changed");
    readArtifact(inspection.canonicalProjectRoot, intent.launchPacketRef, "launch packet");
    readArtifact(inspection.canonicalProjectRoot, intent.resourcePlanRef, "launch resource plan");
    if (intent.retryOf === undefined) {
      if (text(session, "state") !== "awaiting_launch") stale("launch session state changed");
      assertSameArtifact(intent.launchPacketRef, parseArtifactRef({
        path: text(session, "launch_packet_path"),
        digest: exactDigest(session.launch_packet_digest, "stored launch packet digest"),
      }, "stored launch packet"), "stored launch packet");
    } else {
      if (text(session, "state") !== "launch_failed") stale("launch retry state changed");
      const failed = row(this.#database.prepare(`
        SELECT c.provider_adapter_id, c.provider_action_id, p.status,
               p.execution_count, p.effect_count, p.idempotency_proven, p.result_json
          FROM project_session_launch_custody c
          JOIN provider_actions p
            ON p.adapter_id=c.provider_adapter_id AND p.action_id=c.provider_action_id
         WHERE c.project_session_id=?
         ORDER BY c.custody_attempt_generation DESC LIMIT 1
      `).get(intent.projectSessionId), "failed launch custody");
      if (
        text(failed, "provider_adapter_id") !== intent.retryOf.providerAdapterId ||
        text(failed, "provider_action_id") !== intent.retryOf.providerActionId ||
        text(failed, "status") !== "terminal" ||
        integer(failed, "effect_count") !== 0 ||
        integer(failed, "idempotency_proven") !== 1 ||
        !isProvedNoEffectOutcome(failed)
      ) stale("proved launch failure changed before retry commit");
    }
  }

  #ensureScopes(inspection: LaunchInspection, now: number): void {
    const { intent, packet, plan } = inspection;
    const definitions = [
      {
        scope: plan.scopes.project,
        kind: "project",
        parent: null,
        projectSessionId: null,
        runId: null,
        owner: intent.projectId,
      },
      {
        scope: plan.scopes.projectSession,
        kind: "project-session",
        parent: plan.scopes.project.scopeId,
        projectSessionId: intent.projectSessionId,
        runId: null,
        owner: intent.projectSessionId,
      },
      {
        scope: plan.scopes.coordinationRun,
        kind: "coordination-run",
        parent: plan.scopes.projectSession.scopeId,
        projectSessionId: intent.projectSessionId,
        runId: packet.runId,
        owner: packet.runId,
      },
    ] as const;
    for (const definition of definitions) {
      const existing = this.#database.prepare(`
        SELECT project_id, project_session_id, coordination_run_id, parent_scope_id,
               scope_kind, owner_ref, state
          FROM resource_scopes WHERE scope_id=?
      `).get(definition.scope.scopeId);
      if (isRow(existing)) {
        const limits = Object.fromEntries(this.#database.prepare(`
          SELECT unit_key, limit_value FROM resource_dimensions WHERE scope_id=? ORDER BY unit_key
        `).all(definition.scope.scopeId).filter(isRow).map((dimension) => [
          text(dimension, "unit_key"),
          integer(dimension, "limit_value"),
        ]));
        if (
          text(existing, "project_id") !== intent.projectId ||
          existing.project_session_id !== definition.projectSessionId ||
          existing.coordination_run_id !== definition.runId ||
          existing.parent_scope_id !== definition.parent ||
          text(existing, "scope_kind") !== definition.kind ||
          text(existing, "owner_ref") !== definition.owner ||
          text(existing, "state") !== "active" ||
          !sameAmounts(limits, definition.scope.limits)
        ) {
          throw new ProjectFabricCoreError("CONFLICT", `${definition.kind} resource scope changed before retry`);
        }
        continue;
      }
      this.#database.prepare(`
        INSERT INTO resource_scopes(
          scope_id, project_id, project_session_id, coordination_run_id,
          parent_scope_id, scope_kind, owner_ref, state, revision
        ) VALUES (?, ?, ?, ?, ?, ?, ?, 'active', 1)
      `).run(
        definition.scope.scopeId,
        intent.projectId,
        definition.projectSessionId,
        definition.runId,
        definition.parent,
        definition.kind,
        definition.owner,
      );
      const insertDimension = this.#database.prepare(`
        INSERT INTO resource_dimensions(scope_id, unit_key, limit_value, used, reserved, usage_unknown)
        VALUES (?, ?, ?, 0, 0, 0)
      `);
      for (const [unit, limit] of Object.entries(definition.scope.limits)) {
        insertDimension.run(definition.scope.scopeId, unit, limit);
      }
    }
    void now;
  }

  #reserve(inspection: LaunchInspection, reservationId: string, now: number): void {
    const { intent, packet, plan } = inspection;
    const path = [
      { scopeId: plan.scopes.project.scopeId, kind: "project", projectId: intent.projectId },
      { scopeId: plan.scopes.projectSession.scopeId, kind: "project-session", projectSessionId: intent.projectSessionId },
      { scopeId: plan.scopes.coordinationRun.scopeId, kind: "coordination-run", coordinationRunId: packet.runId },
    ];
    for (const scope of path) {
      for (const [unit, amount] of Object.entries(plan.launchReservation.amounts)) {
        const changed = this.#database.prepare(`
          UPDATE resource_dimensions
             SET reserved=reserved+?
           WHERE scope_id=? AND unit_key=? AND usage_unknown=0
             AND limit_value-used-reserved>=?
        `).run(amount, scope.scopeId, unit, amount);
        if (changed.changes !== 1) {
          throw new ProjectFabricCoreError("RESOURCE_EXHAUSTED", `${unit} changed during launch admission`);
        }
      }
    }
    this.#database.prepare(`
      INSERT INTO resource_reservations(
        reservation_id, project_session_id, coordination_run_id, leaf_scope_id,
        operation_id, actor_agent_id, state, revision, generation, identity_hash,
        path_json, amounts_json, created_at, updated_at
      ) VALUES (?, ?, ?, ?, ?, ?, 'reserved', 1, 1, ?, ?, ?, ?, ?)
    `).run(
      reservationId,
      intent.projectSessionId,
      packet.runId,
      plan.scopes.coordinationRun.scopeId,
      intent.providerActionId,
      packet.chairAgentId,
      sha256(canonicalJson({ reservationId, path, amounts: plan.launchReservation.amounts })),
      canonicalJson(path),
      canonicalJson(plan.launchReservation.amounts),
      now,
      now,
    );
    const insert = this.#database.prepare(`
      INSERT INTO resource_reservation_dimensions(
        reservation_id, scope_id, unit_key, amount, consumed, released, usage_unknown
      ) VALUES (?, ?, ?, ?, 0, 0, 0)
    `);
    for (const scope of path) {
      for (const [unit, amount] of Object.entries(plan.launchReservation.amounts)) {
        insert.run(reservationId, scope.scopeId, unit, amount);
      }
    }
  }
}
