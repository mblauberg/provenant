import {
  parseOperatorCapabilityGrant,
  type ChairBridgeRecoveryIntent,
  type OperatorCapabilityGrant,
} from "@local/agent-fabric-protocol";
import { randomBytes } from "node:crypto";
import type Database from "better-sqlite3";
import { isAbsolute, normalize, resolve } from "node:path";

import { ProjectFabricCoreError } from "../project-session/contracts.js";
import { canonicalJson, integer, nullableText, row, sha256, text } from "../persistence/row-codec.js";
import type {
  AuthenticatedOperatorCredential,
  LocalOperatorTakeoverCapabilityInput,
  LocalOperatorTakeoverCapabilityResult,
} from "./store.js";

const SHA256_DIGEST = /^sha256:[0-9a-f]{64}$/u;

function deterministicIdentifier(prefix: string, value: unknown): string {
  return `${prefix}:${sha256(canonicalJson(value))}`;
}

function exactDigest(value: string, field: string): void {
  if (!SHA256_DIGEST.test(value)) {
    throw new ProjectFabricCoreError("PROTOCOL_INVALID", `${field} must be a lowercase sha256 digest`);
  }
}

function exactCanonicalRoot(value: string): void {
  if (!isAbsolute(value) || value === "/" || normalize(value) !== value || resolve(value) !== value) {
    throw new ProjectFabricCoreError("PROTOCOL_INVALID", "canonicalRoot must be an exact normalized absolute project root");
  }
}

function futureTimestamp(value: string, now: number, field: string): { millis: number; canonical: string } {
  const millis = Date.parse(value);
  if (!Number.isFinite(millis) || millis <= now) {
    throw new ProjectFabricCoreError("PROTOCOL_INVALID", `${field} must be a future timestamp`);
  }
  return { millis, canonical: new Date(millis).toISOString() };
}

export function openLocalOperatorConsoleTakeoverCapability(options: Readonly<{
  database: Database.Database;
  clock: () => number;
  authenticateCredential(token: string): AuthenticatedOperatorCredential;
  issueCapability(grant: OperatorCapabilityGrant, token: string): void;
}>, input: LocalOperatorTakeoverCapabilityInput): LocalOperatorTakeoverCapabilityResult {
  exactCanonicalRoot(input.canonicalRoot);
  exactDigest(input.trustRecordDigest, "trustRecordDigest");
  exactDigest(input.authenticatedSubjectHash, "authenticatedSubjectHash");
  const now = options.clock();
  const expiresAt = futureTimestamp(input.expiresAt, now, "expiresAt");
  const token = `afop_${randomBytes(32).toString("base64url")}`;

  return options.database.transaction((): LocalOperatorTakeoverCapabilityResult => {
    const authenticated = options.authenticateCredential(input.projectCapability.token);
    if (
      authenticated.capabilityId !== input.projectCapability.capabilityId ||
      authenticated.kind !== "project-launch" ||
      authenticated.context.projectId !== input.projectId ||
      !authenticated.actions.includes("launch")
    ) {
      throw new ProjectFabricCoreError("CAPABILITY_FORBIDDEN", "credential is not the exact project capability");
    }
    const project = row(options.database.prepare(`
      SELECT canonical_root, trust_record_digest, authority_generation
        FROM projects WHERE project_id=?
    `).get(input.projectId), "project");
    if (
      text(project, "canonical_root") !== input.canonicalRoot ||
      nullableText(project, "trust_record_digest") !== input.trustRecordDigest
    ) {
      throw new ProjectFabricCoreError("CONFLICT", "trusted project binding changed");
    }
    const projectAuthorityGeneration = integer(project, "authority_generation");
    if (projectAuthorityGeneration !== authenticated.context.projectAuthorityGeneration) {
      throw new ProjectFabricCoreError("STALE_GENERATION", "project authority generation changed");
    }
    const principal = row(options.database.prepare(`
      SELECT authenticated_subject_hash, project_authority_generation, principal_generation, state
        FROM operator_principals WHERE operator_id=? AND project_id=?
    `).get(authenticated.context.operatorId, input.projectId), "operator principal");
    const principalGeneration = integer(principal, "principal_generation");
    if (
      text(principal, "authenticated_subject_hash") !== input.authenticatedSubjectHash ||
      text(principal, "state") !== "active"
    ) {
      throw new ProjectFabricCoreError("AUTHENTICATION_FAILED", "local operator subject binding changed");
    }
    if (
      integer(principal, "project_authority_generation") !== projectAuthorityGeneration ||
      principalGeneration !== authenticated.context.principalGeneration
    ) {
      throw new ProjectFabricCoreError("STALE_PRINCIPAL_GENERATION", "operator principal generation changed");
    }
    const projectCapability = row(options.database.prepare(`
      SELECT expires_at FROM operator_capabilities WHERE capability_id=?
    `).get(input.projectCapability.capabilityId), "project capability");
    if (expiresAt.millis > integer(projectCapability, "expires_at")) {
      throw new ProjectFabricCoreError("CAPABILITY_FORBIDDEN", "takeover capability cannot outlive the project capability");
    }

    const losses = options.database.prepare(`
      SELECT loss.*, session.project_id, session.state AS session_state,
             session.revision AS session_revision, session.generation AS session_generation,
             run.revision AS run_revision, run.chair_generation,
             run.lifecycle_state AS run_state,
             bridge.revision AS bridge_revision, bridge.state AS bridge_state
        FROM chair_bridge_losses loss
        JOIN project_sessions session ON session.project_session_id=loss.project_session_id
        JOIN runs run ON run.project_session_id=loss.project_session_id
                     AND run.run_id=loss.coordination_run_id
        JOIN launched_chair_bridge_state bridge
          ON bridge.project_session_id=loss.project_session_id
         AND bridge.coordination_run_id=loss.coordination_run_id
       WHERE loss.project_session_id=?
         AND NOT EXISTS (
           SELECT 1 FROM chair_bridge_loss_resolutions resolution
            WHERE resolution.loss_id=loss.loss_id
         )
       ORDER BY loss.loss_id
    `).all(input.projectSessionId);
    if (losses.length !== 1) {
      throw new ProjectFabricCoreError(
        "LIFECYCLE_PRECONDITION_FAILED",
        "project session must have exactly one unresolved chair bridge loss",
      );
    }
    const loss = row(losses[0], "unresolved chair bridge loss");
    if (text(loss, "project_id") !== input.projectId) {
      throw new ProjectFabricCoreError("WRONG_PROJECT", "chair bridge loss belongs to another project");
    }
    if (
      text(loss, "session_state") !== "recovery_required" ||
      text(loss, "run_state") !== "recovery_required" ||
      text(loss, "bridge_state") !== "lost"
    ) {
      throw new ProjectFabricCoreError("LIFECYCLE_PRECONDITION_FAILED", "chair bridge loss is not currently recoverable");
    }
    const sessionGeneration = integer(loss, "session_generation");
    const lossId = text(loss, "loss_id");
    const recoveryIntent: Extract<ChairBridgeRecoveryIntent, { path: "abandon" }> = {
      kind: "chair-bridge-recovery",
      schemaVersion: 1,
      path: "abandon",
      projectSessionId: input.projectSessionId as ChairBridgeRecoveryIntent["projectSessionId"],
      coordinationRunId: text(loss, "coordination_run_id") as ChairBridgeRecoveryIntent["coordinationRunId"],
      lossId,
      recoveryManifestDigest: text(loss, "recovery_manifest_digest") as ChairBridgeRecoveryIntent["recoveryManifestDigest"],
      expectedSessionRevision: integer(loss, "session_revision"),
      expectedSessionGeneration: sessionGeneration,
      expectedRunRevision: integer(loss, "run_revision"),
      expectedChairGeneration: integer(loss, "chair_generation"),
      expectedPrincipalGeneration: integer(loss, "principal_generation"),
      expectedBridgeRevision: integer(loss, "bridge_revision"),
      expectedLostBridgeGeneration: integer(loss, "lost_bridge_generation"),
      expectedProviderSessionGeneration: integer(loss, "provider_session_generation"),
      providerAdapterId: text(loss, "provider_adapter_id"),
      providerContractDigest: text(loss, "provider_contract_digest") as ChairBridgeRecoveryIntent["providerContractDigest"],
      reason: "operator confirmed terminal retained-chair loss",
    };
    const issuedAt = new Date(now).toISOString();
    const actions = ["read", "takeover"] as const;
    const capabilityId = deterministicIdentifier("capability:takeover-console", {
      expiresAt: expiresAt.canonical,
      lossId,
      nonce: randomBytes(32).toString("base64url"),
      operatorId: authenticated.context.operatorId,
      projectAuthorityGeneration,
      recoveryIntent,
    });
    options.database.prepare(`
      UPDATE operator_capabilities SET revoked_at=?
       WHERE operator_id=? AND project_id=? AND project_session_id=?
         AND kind='takeover' AND handoff_digest=? AND revoked_at IS NULL
    `).run(
      now,
      authenticated.context.operatorId,
      input.projectId,
      input.projectSessionId,
      recoveryIntent.recoveryManifestDigest,
    );
    options.issueCapability(parseOperatorCapabilityGrant({
      capabilityId,
      operatorId: authenticated.context.operatorId,
      projectId: input.projectId,
      projectAuthorityGeneration,
      principalGeneration,
      issuedAt,
      expiresAt: expiresAt.canonical,
      status: "active",
      kind: "takeover",
      projectSessionId: input.projectSessionId,
      sessionGeneration,
      actions: [...actions],
      takeoverBinding: {
        handoffDigest: recoveryIntent.recoveryManifestDigest,
        oldChairGeneration: recoveryIntent.expectedChairGeneration,
        expectedRunId: recoveryIntent.coordinationRunId,
        expectedRunRevision: recoveryIntent.expectedRunRevision,
        expectedSessionRevision: recoveryIntent.expectedSessionRevision,
        targetRevision: recoveryIntent.expectedBridgeRevision,
      },
    }), token);
    return {
      projectId: input.projectId,
      operatorId: authenticated.context.operatorId,
      capabilityId,
      projectSessionId: input.projectSessionId,
      projectAuthorityGeneration,
      sessionGeneration,
      principalGeneration,
      kind: "takeover",
      actions: [...actions],
      issuedAt,
      expiresAt: expiresAt.canonical,
      credential: { capabilityId, token },
      recoveryIntent,
    };
  }).immediate();
}
