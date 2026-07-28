import type Database from "better-sqlite3";
import { createHash } from "node:crypto";

import { canonicalJson, integer, row, text } from "../persistence/row-codec.js";

export type GenerationLossObservationClassification =
  | "replay"
  | "reordered-observation"
  | "context-advance"
  | "generation-advance";

export type GenerationLossCheckpoint =
  | Readonly<{ state: "absent" | "invalid"; ref: null; digest: null }>
  | Readonly<{ state: "last-validated"; ref: string; digest: string }>;

export type GenerationLossSource = Readonly<{
  oldProviderSessionRef: string;
  newProviderSessionRef: string;
  sourceActionRef: Readonly<{ adapterId: string; actionId: string }>;
  sourceAdapterContractDigest: string;
  sourcePrincipalGeneration: number;
  sourceBridgeGeneration: number;
  bridgeOwnerKind: "chair" | "child";
  sourceBridgeRowId: string;
  sourceBridgeRevision: number;
  sourceCapabilityHash: string;
  sourceProjectSessionGeneration: number | null;
  sourceRunGeneration: number | null;
  sourceChairLeaseGeneration: number | null;
  checkpoint: GenerationLossCheckpoint;
}>;

export type RecordGenerationLossObservationInput = Readonly<{
  sourceEventId: string;
  projectSessionId: string;
  runId: string;
  agentId: string;
  providerGeneration: number;
  contextRevision: number;
  evidenceDigest: string;
  observedAt: number;
  lossSource?: GenerationLossSource;
}>;

export type GenerationLossObservationRecord = Readonly<{
  observationId: string;
  sourceEventId: string;
  projectSessionId: string;
  runId: string;
  agentId: string;
  providerGeneration: number;
  contextRevision: number;
  classification: GenerationLossObservationClassification;
  evidenceDigest: string;
  observedAt: number;
  generationLossId: string | null;
}>;

export type GenerationLossHead = Readonly<{
  projectSessionId: string;
  runId: string;
  agentId: string;
  generationLossId: string;
  revision: number;
  state: "open" | "recovery-in-progress" | "recovered-adopted" | "abandoned";
  abandonKind: "none" | "direct-open" | "recovery-attempt";
  semanticDigest: string;
  sourceRefDigest: string;
  journalDigest: string;
  terminal: boolean;
}>;

type IdentityHighWater = Readonly<{
  providerGeneration: number;
  principalGeneration: number;
  revision: number;
}>;

type ContextHighWater = Readonly<{
  contextRevision: number;
  revision: number;
}>;

const DIGEST = /^sha256:[0-9a-f]{64}$/u;

function lifecycleDigest(domain: string, value: unknown): string {
  return `sha256:${createHash("sha256")
    .update(`agent-fabric.lifecycle.v1\0${domain}\0`)
    .update(canonicalJson(value))
    .digest("hex")}`;
}

function requiredId(value: string, name: string): void {
  if (value.length === 0 || Buffer.byteLength(value, "utf8") > 256 || value.includes("\0")) {
    throw new Error(`${name} must be a nonempty bounded identifier`);
  }
}

function requiredDigest(value: string, name: string): void {
  if (!DIGEST.test(value)) throw new Error(`${name} must be a sha256-prefixed digest`);
}

function positiveInteger(value: number, name: string): void {
  if (!Number.isSafeInteger(value) || value < 1) throw new Error(`${name} must be a positive safe integer`);
}

function nonnegativeInteger(value: number, name: string): void {
  if (!Number.isSafeInteger(value) || value < 0) throw new Error(`${name} must be a nonnegative safe integer`);
}

function nullablePositiveInteger(value: number | null, name: string): void {
  if (value !== null) positiveInteger(value, name);
}

function storedContextRevision(value: unknown): number {
  if (value === null) return 0;
  if (typeof value === "number") {
    nonnegativeInteger(value, "provider_state.context_revision");
    return value;
  }
  if (typeof value === "string" && /^(0|[1-9][0-9]{0,15})$/u.test(value)) {
    const parsed = Number(value);
    nonnegativeInteger(parsed, "provider_state.context_revision");
    return parsed;
  }
  throw new Error("provider_state.context_revision is invalid");
}

function validateObservation(input: RecordGenerationLossObservationInput): void {
  requiredId(input.sourceEventId, "sourceEventId");
  requiredId(input.projectSessionId, "projectSessionId");
  requiredId(input.runId, "runId");
  requiredId(input.agentId, "agentId");
  positiveInteger(input.providerGeneration, "providerGeneration");
  nonnegativeInteger(input.contextRevision, "contextRevision");
  requiredDigest(input.evidenceDigest, "evidenceDigest");
  nonnegativeInteger(input.observedAt, "observedAt");
}

function validateLossSource(source: GenerationLossSource, principalGeneration: number): void {
  requiredId(source.oldProviderSessionRef, "oldProviderSessionRef");
  requiredId(source.newProviderSessionRef, "newProviderSessionRef");
  requiredId(source.sourceActionRef.adapterId, "sourceActionRef.adapterId");
  requiredId(source.sourceActionRef.actionId, "sourceActionRef.actionId");
  requiredDigest(source.sourceAdapterContractDigest, "sourceAdapterContractDigest");
  positiveInteger(source.sourcePrincipalGeneration, "sourcePrincipalGeneration");
  if (source.sourcePrincipalGeneration !== principalGeneration) {
    throw new Error("generation-loss source principal changed");
  }
  positiveInteger(source.sourceBridgeGeneration, "sourceBridgeGeneration");
  requiredId(source.sourceBridgeRowId, "sourceBridgeRowId");
  positiveInteger(source.sourceBridgeRevision, "sourceBridgeRevision");
  requiredId(source.sourceCapabilityHash, "sourceCapabilityHash");
  nullablePositiveInteger(source.sourceProjectSessionGeneration, "sourceProjectSessionGeneration");
  nullablePositiveInteger(source.sourceRunGeneration, "sourceRunGeneration");
  nullablePositiveInteger(source.sourceChairLeaseGeneration, "sourceChairLeaseGeneration");
  if (source.bridgeOwnerKind === "chair") {
    if (
      source.sourceProjectSessionGeneration === null ||
      source.sourceRunGeneration === null ||
      source.sourceChairLeaseGeneration === null
    ) {
      throw new Error("chair generation-loss source requires chair generation bindings");
    }
  } else if (
    source.sourceProjectSessionGeneration !== null ||
    source.sourceRunGeneration !== null ||
    source.sourceChairLeaseGeneration !== null
  ) {
    throw new Error("child generation-loss source cannot carry chair generation bindings");
  }
  if (source.checkpoint.state === "last-validated") {
    requiredId(source.checkpoint.ref, "checkpoint.ref");
    requiredDigest(source.checkpoint.digest, "checkpoint.digest");
  } else if (
    (source.checkpoint.state === "absent" || source.checkpoint.state === "invalid") &&
    (source.checkpoint.ref !== null || source.checkpoint.digest !== null)
  ) {
    throw new Error("generation-loss checkpoint arm is crossed");
  } else if (source.checkpoint.state !== "absent" && source.checkpoint.state !== "invalid") {
    throw new Error("generation-loss checkpoint state is invalid");
  }
}

export function generationLossRef(
  runId: string,
  agentId: string,
  generationLossId: string,
  revision: number,
): Readonly<Record<string, unknown>> {
  return {
    schemaVersion: 1,
    runId,
    agentId,
    generationLossId,
    generationLossRevision: revision,
  };
}

export function generationLossRevisionBody(input: Readonly<{
  generationLossId: string;
  revision: number;
  state: GenerationLossHead["state"];
  abandonKind: GenerationLossHead["abandonKind"];
  recoveryActionRef: Readonly<{ adapterId: string; actionId: string }> | null;
  activeRecoveryCustodyId: string | null;
  terminalEvidenceDigest: string | null;
}>): Readonly<Record<string, unknown>> {
  return {
    schemaVersion: 1,
    sourceKind: "generation-loss",
    generationLossId: input.generationLossId,
    revision: input.revision,
    state: input.state,
    abandonKind: input.abandonKind,
    recoveryActionRef: input.recoveryActionRef,
    activeRecoveryCustodyId: input.activeRecoveryCustodyId,
    terminalEvidenceDigest: input.terminalEvidenceDigest,
  };
}

function observationId(input: RecordGenerationLossObservationInput): string {
  const digest = createHash("sha256")
    .update(canonicalJson({
      schemaVersion: 1,
      runId: input.runId,
      agentId: input.agentId,
      sourceEventId: input.sourceEventId,
    }))
    .digest("hex");
  return `context-observation:${digest}`;
}

export class GenerationLossRepository {
  readonly #database: Database.Database;

  constructor(database: Database.Database) {
    this.#database = database;
  }

  recordObservationInCurrentTransaction(
    input: RecordGenerationLossObservationInput,
  ): GenerationLossObservationRecord {
    if (!this.#database.inTransaction) {
      throw new Error("generation-loss observation requires a transaction");
    }
    validateObservation(input);
    const run = row(this.#database.prepare(`
      SELECT project_session_id FROM runs WHERE run_id=?
    `).get(input.runId), "generation-loss observation run");
    if (text(run, "project_session_id") !== input.projectSessionId) {
      throw new Error("generation-loss observation belongs to another project session");
    }

    const existing = this.#database.prepare(`
      SELECT observation_id,source_event_id,provider_generation,context_revision,
             classification,evidence_digest,observed_at
        FROM provider_context_observation_audit
       WHERE run_id=? AND agent_id=? AND source_event_id=?
    `).get(input.runId, input.agentId, input.sourceEventId);
    if (existing !== undefined) {
      const stored = row(existing, "generation-loss observation replay");
      if (
        integer(stored, "provider_generation") !== input.providerGeneration ||
        integer(stored, "context_revision") !== input.contextRevision ||
        text(stored, "evidence_digest") !== input.evidenceDigest
      ) {
        throw new Error("generation-loss source event conflicts with its durable observation");
      }
      return this.#observationRecord(input, stored);
    }

    const { identity, context, persisted } = this.#inspectHighWater(input);
    let classification: GenerationLossObservationClassification;
    if (
      input.providerGeneration < identity.providerGeneration ||
      (
        input.providerGeneration === identity.providerGeneration &&
        input.contextRevision < context.contextRevision
      )
    ) {
      classification = "reordered-observation";
    } else if (
      input.providerGeneration === identity.providerGeneration &&
      input.contextRevision === context.contextRevision
    ) {
      classification = "replay";
    } else if (input.providerGeneration === identity.providerGeneration) {
      classification = "context-advance";
    } else {
      classification = "generation-advance";
    }

    let generationLossId: string | null = null;
    if (classification === "context-advance" || classification === "generation-advance") {
      const source = input.lossSource;
      if (source === undefined) {
        throw new Error("advancing context observation requires an immutable generation-loss source");
      }
      validateLossSource(source, identity.principalGeneration);
      if (source.sourceBridgeRowId !== `${input.runId}:${input.agentId}`) {
        throw new Error("generation-loss source bridge row id is crossed");
      }
      const sourceBridge = source.bridgeOwnerKind === "chair"
        ? this.#database.prepare(`
            SELECT 1
              FROM launched_chair_bridge_state bridge
              JOIN runs run
                ON run.project_session_id=bridge.project_session_id
               AND run.run_id=bridge.coordination_run_id
               AND run.chair_agent_id=bridge.chair_agent_id
              JOIN project_sessions session
                ON session.project_session_id=bridge.project_session_id
              JOIN provider_actions action
                ON action.run_id=bridge.coordination_run_id
               AND action.adapter_id=bridge.provider_adapter_id
               AND action.action_id=bridge.provider_action_id
             WHERE bridge.project_session_id=? AND bridge.coordination_run_id=?
               AND bridge.chair_agent_id=? AND bridge.state='active'
               AND bridge.provider_adapter_id=? AND bridge.provider_action_id=?
               AND bridge.provider_contract_digest=? AND bridge.provider_session_ref=?
               AND bridge.provider_session_generation=? AND bridge.principal_generation=?
               AND bridge.bridge_generation=? AND bridge.capability_hash=? AND bridge.revision=?
               AND run.revision=? AND run.chair_generation=? AND session.generation=?
               AND action.status='terminal'
          `).get(
            input.projectSessionId, input.runId, input.agentId,
            source.sourceActionRef.adapterId, source.sourceActionRef.actionId,
            source.sourceAdapterContractDigest, source.oldProviderSessionRef,
            identity.providerGeneration, source.sourcePrincipalGeneration,
            source.sourceBridgeGeneration, source.sourceCapabilityHash, source.sourceBridgeRevision,
            source.sourceRunGeneration, source.sourceChairLeaseGeneration,
            source.sourceProjectSessionGeneration,
          )
        : this.#database.prepare(`
            SELECT 1
              FROM agent_bridge_state bridge
              JOIN provider_agent_custody custody
                ON custody.run_id=bridge.run_id AND custody.adapter_id=bridge.adapter_id
               AND custody.action_id=bridge.action_id AND custody.target_agent_id=bridge.agent_id
              JOIN provider_actions action
                ON action.run_id=bridge.run_id AND action.adapter_id=bridge.adapter_id
               AND action.action_id=bridge.action_id
             WHERE bridge.run_id=? AND bridge.agent_id=? AND bridge.bridge_state='active'
               AND bridge.adapter_id=? AND bridge.action_id=?
               AND bridge.provider_session_ref=? AND bridge.provider_session_generation=?
               AND bridge.bridge_generation=? AND bridge.capability_hash=? AND bridge.revision=?
               AND custody.bridge_contract_digest=? AND custody.capability_hash=?
               AND custody.principal_generation=? AND action.status='terminal'
          `).get(
            input.runId, input.agentId, source.sourceActionRef.adapterId,
            source.sourceActionRef.actionId, source.oldProviderSessionRef,
            identity.providerGeneration, source.sourceBridgeGeneration,
            source.sourceCapabilityHash, source.sourceBridgeRevision,
            source.sourceAdapterContractDigest, source.sourceCapabilityHash,
            source.sourcePrincipalGeneration,
          );
      if (sourceBridge === undefined) {
        throw new Error("generation-loss source bridge no longer matches its exact predecessor");
      }
      const sourceCapability = this.#database.prepare(`
        SELECT principal_generation FROM capabilities
         WHERE token_hash=? AND run_id=? AND agent_id=?
           AND revoked_at IS NULL AND expires_at>?
      `).get(
        source.sourceCapabilityHash,
        input.runId,
        input.agentId,
        input.observedAt,
      );
      if (
        sourceCapability === undefined ||
        integer(row(sourceCapability, "generation-loss source capability"), "principal_generation") !==
          identity.principalGeneration
      ) {
        throw new Error("generation-loss source capability is not the active principal");
      }
      const activeCustody = this.#database.prepare(`
        SELECT custody_id FROM lifecycle_rotation_custody_heads
         WHERE run_id=? AND agent_id=? AND terminal=0
      `).get(input.runId, input.agentId);
      if (activeCustody !== undefined) throw new Error("agent has active lifecycle custody");
      const active = this.#database.prepare(`
        SELECT generation_loss_id FROM lifecycle_generation_loss_heads
         WHERE run_id=? AND agent_id=? AND terminal=0
      `).get(input.runId, input.agentId);
      if (active !== undefined) throw new Error("agent already has a nonterminal generation loss");
      generationLossId = `loss:${input.projectSessionId}:${input.runId}:${input.agentId}:${input.sourceEventId}`;
      this.#insertGenerationLoss(
        input,
        source,
        classification,
        identity,
        context,
        generationLossId,
      );
      this.#ratchetHighWater(input, identity, context, classification, persisted);
    }

    const id = observationId(input);
    this.#database.prepare(`
      INSERT INTO provider_context_observation_audit(
        observation_id,source_event_id,run_id,agent_id,provider_generation,
        context_revision,classification,evidence_digest,observed_at
      ) VALUES (?,?,?,?,?,?,?,?,?)
    `).run(
      id,
      input.sourceEventId,
      input.runId,
      input.agentId,
      input.providerGeneration,
      input.contextRevision,
      classification,
      input.evidenceDigest,
      input.observedAt,
    );
    return {
      observationId: id,
      sourceEventId: input.sourceEventId,
      projectSessionId: input.projectSessionId,
      runId: input.runId,
      agentId: input.agentId,
      providerGeneration: input.providerGeneration,
      contextRevision: input.contextRevision,
      classification,
      evidenceDigest: input.evidenceDigest,
      observedAt: input.observedAt,
      generationLossId,
    };
  }

  readHead(runId: string, agentId: string, generationLossId: string): GenerationLossHead {
    const stored = row(this.#database.prepare(`
      SELECT project_session_id,current_revision,state,abandon_kind_code,
             semantic_digest,source_ref_digest,journal_digest,terminal
        FROM lifecycle_generation_loss_heads
       WHERE run_id=? AND agent_id=? AND generation_loss_id=?
    `).get(runId, agentId, generationLossId), "generation-loss head");
    const state = text(stored, "state") as GenerationLossHead["state"];
    const abandonKind = text(stored, "abandon_kind_code") as GenerationLossHead["abandonKind"];
    return {
      projectSessionId: text(stored, "project_session_id"),
      runId,
      agentId,
      generationLossId,
      revision: integer(stored, "current_revision"),
      state,
      abandonKind,
      semanticDigest: text(stored, "semantic_digest"),
      sourceRefDigest: text(stored, "source_ref_digest"),
      journalDigest: text(stored, "journal_digest"),
      terminal: integer(stored, "terminal") === 1,
    };
  }

  finalizeAuthorizedDirectOpenInCurrentTransaction(input: Readonly<{
    sourceHead: GenerationLossHead;
    finalRevision: number;
    terminalEvidenceDigest: string;
    finalSemanticDigest: string;
    finalSourceRefDigest: string;
    authorityBatchId: string;
    authorityApplyId: string;
    authorityApplyDigest: string;
    recordedAt: number;
  }>): void {
    if (!this.#database.inTransaction) throw new Error("lifecycle terminal apply requires a transaction");
    const head = input.sourceHead;
    const finalBody = generationLossRevisionBody({
      generationLossId: head.generationLossId,
      revision: input.finalRevision,
      state: "abandoned",
      abandonKind: "direct-open",
      recoveryActionRef: null,
      activeRecoveryCustodyId: null,
      terminalEvidenceDigest: input.terminalEvidenceDigest,
    });
    const finalSemanticDigest = lifecycleDigest("generation-loss-semantic", finalBody);
    if (finalSemanticDigest !== input.finalSemanticDigest ||
        finalSemanticDigest !== input.finalSourceRefDigest) {
      throw new Error("prepared lifecycle terminal semantic changed");
    }
    const ownerRef = {
      kind: "generation-loss",
      generationLossRef: generationLossRef(
        head.runId,
        head.agentId,
        head.generationLossId,
        input.finalRevision,
      ),
      sourceRefDigest: input.finalSourceRefDigest,
    };
    const journal = {
      schemaVersion: 1,
      ownerRef,
      priorJournalDigest: head.journalDigest,
      semanticDigest: finalSemanticDigest,
      sourceRefDigest: input.finalSourceRefDigest,
      authorityBatchId: input.authorityBatchId,
      authorityApplyId: input.authorityApplyId,
      authorityApplyDigest: input.authorityApplyDigest,
      originFreshApplyId: null,
      originFreshApplyDigest: null,
      recordedAt: input.recordedAt,
    };
    const journalJson = canonicalJson(journal);
    const journalDigest = lifecycleDigest("generation-loss-journal", journal);
    this.#database.prepare(`
      INSERT INTO lifecycle_generation_loss_revisions(
        project_session_id,run_id,agent_id,generation_loss_id,revision,
        prior_revision,prior_journal_digest,state,abandon_kind_code,
        recovery_action_adapter_id,recovery_action_id,active_recovery_custody_id,
        terminal_evidence_digest,semantic_json,semantic_digest,source_ref_digest,
        origin_fresh_apply_id,origin_fresh_apply_digest,receipt_batch_id,
        receipt_apply_id,receipt_apply_digest,journal_json,journal_digest,recorded_at
      ) VALUES (?,?,?,?,?,?,?,'abandoned','direct-open',NULL,NULL,NULL,?,?,?,?,
                NULL,NULL,?,?,?,?,?,?)
    `).run(
      head.projectSessionId, head.runId, head.agentId, head.generationLossId,
      input.finalRevision, head.revision, head.journalDigest,
      input.terminalEvidenceDigest, canonicalJson(finalBody), finalSemanticDigest,
      input.finalSourceRefDigest, input.authorityBatchId, input.authorityApplyId,
      input.authorityApplyDigest, journalJson, journalDigest, input.recordedAt,
    );
    const changed = this.#database.prepare(`
      UPDATE lifecycle_generation_loss_heads
         SET current_revision=?,state='abandoned',abandon_kind_code='direct-open',
             semantic_digest=?,source_ref_digest=?,journal_digest=?,terminal=1,
             head_revision=head_revision+1
       WHERE run_id=? AND agent_id=? AND generation_loss_id=? AND current_revision=?
         AND journal_digest=? AND terminal=0
    `).run(
      input.finalRevision, finalSemanticDigest, input.finalSourceRefDigest, journalDigest,
      head.runId, head.agentId, head.generationLossId, head.revision, head.journalDigest,
    );
    if (changed.changes !== 1) throw new Error("generation-loss head compare-and-set failed");
  }

  #readIdentityHighWater(runId: string, agentId: string): IdentityHighWater {
    const stored = row(this.#database.prepare(`
      SELECT provider_generation,principal_generation,revision
        FROM agent_lifecycle_identity_high_water
       WHERE run_id=? AND agent_id=?
    `).get(runId, agentId), "generation-loss identity high-water");
    return {
      providerGeneration: integer(stored, "provider_generation"),
      principalGeneration: integer(stored, "principal_generation"),
      revision: integer(stored, "revision"),
    };
  }

  #inspectHighWater(input: RecordGenerationLossObservationInput): Readonly<{
    identity: IdentityHighWater;
    context: ContextHighWater;
    persisted: boolean;
  }> {
    const existing = this.#database.prepare(`
      SELECT provider_generation,principal_generation,revision
        FROM agent_lifecycle_identity_high_water
       WHERE run_id=? AND agent_id=?
    `).get(input.runId, input.agentId);
    if (existing !== undefined) {
      const identity = this.#readIdentityHighWater(input.runId, input.agentId);
      return {
        identity,
        context: this.#readContextHighWater(
          input.runId,
          input.agentId,
          identity.providerGeneration,
        ),
        persisted: true,
      };
    }

    const provider = row(this.#database.prepare(`
      SELECT provider_session_generation,context_revision
        FROM provider_state WHERE run_id=? AND agent_id=?
    `).get(input.runId, input.agentId), "generation-loss provider state bootstrap");
    const capability = row(this.#database.prepare(`
      SELECT principal_generation FROM capabilities
       WHERE run_id=? AND agent_id=? AND revoked_at IS NULL AND expires_at>?
       ORDER BY principal_generation DESC,token_hash DESC LIMIT 1
    `).get(input.runId, input.agentId, input.observedAt), "generation-loss active capability bootstrap");
    const identity: IdentityHighWater = {
      providerGeneration: integer(provider, "provider_session_generation"),
      principalGeneration: integer(capability, "principal_generation"),
      revision: 1,
    };
    positiveInteger(identity.providerGeneration, "provider_state.provider_session_generation");
    positiveInteger(identity.principalGeneration, "capabilities.principal_generation");
    const context: ContextHighWater = {
      contextRevision: storedContextRevision(provider.context_revision),
      revision: 1,
    };
    return { identity, context, persisted: false };
  }

  #readContextHighWater(runId: string, agentId: string, providerGeneration: number): ContextHighWater {
    const stored = row(this.#database.prepare(`
      SELECT context_revision,revision FROM agent_lifecycle_context_high_water
       WHERE run_id=? AND agent_id=? AND provider_generation=?
    `).get(runId, agentId, providerGeneration), "generation-loss context high-water");
    return {
      contextRevision: integer(stored, "context_revision"),
      revision: integer(stored, "revision"),
    };
  }

  #observationRecord(
    input: RecordGenerationLossObservationInput,
    stored: Readonly<Record<string, unknown>>,
  ): GenerationLossObservationRecord {
    const classification = text(stored, "classification") as GenerationLossObservationClassification;
    const generationLossId = classification === "context-advance" || classification === "generation-advance"
      ? `loss:${input.projectSessionId}:${input.runId}:${input.agentId}:${input.sourceEventId}`
      : null;
    return {
      observationId: text(stored, "observation_id"),
      sourceEventId: text(stored, "source_event_id"),
      projectSessionId: input.projectSessionId,
      runId: input.runId,
      agentId: input.agentId,
      providerGeneration: integer(stored, "provider_generation"),
      contextRevision: integer(stored, "context_revision"),
      classification,
      evidenceDigest: text(stored, "evidence_digest"),
      observedAt: integer(stored, "observed_at"),
      generationLossId,
    };
  }

  #insertGenerationLoss(
    input: RecordGenerationLossObservationInput,
    source: GenerationLossSource,
    lossKind: "context-advance" | "generation-advance",
    identity: IdentityHighWater,
    context: ContextHighWater,
    generationLossId: string,
  ): void {
    const creation = {
      schemaVersion: 1,
      generationLossId,
      sourceEventId: input.sourceEventId,
      lossKind,
      oldProvider: {
        sessionRef: source.oldProviderSessionRef,
        providerGeneration: identity.providerGeneration,
        contextRevision: context.contextRevision,
      },
      newProvider: {
        sessionRef: source.newProviderSessionRef,
        providerGeneration: input.providerGeneration,
        contextRevision: input.contextRevision,
        evidenceDigest: input.evidenceDigest,
      },
      sourceActionRef: source.sourceActionRef,
      sourceAdapterContractDigest: source.sourceAdapterContractDigest,
      sourcePrincipalGeneration: source.sourcePrincipalGeneration,
      sourceBridgeGeneration: source.sourceBridgeGeneration,
      bridgeOwnerKind: source.bridgeOwnerKind,
      sourceBridgeRowId: source.sourceBridgeRowId,
      sourceBridgeRevision: source.sourceBridgeRevision,
      sourceCapabilityHash: source.sourceCapabilityHash,
      sourceProjectSessionGeneration: source.sourceProjectSessionGeneration,
      sourceRunGeneration: source.sourceRunGeneration,
      sourceChairLeaseGeneration: source.sourceChairLeaseGeneration,
      checkpoint: source.checkpoint,
    };
    const creationJson = canonicalJson(creation);
    const creationDigest = lifecycleDigest("generation-loss-semantic", creation);
    this.#database.prepare(`
      INSERT INTO lifecycle_generation_losses(
        project_session_id,run_id,agent_id,generation_loss_id,loss_kind,
        old_provider_session_ref,new_provider_session_ref,old_provider_generation,
        new_provider_generation,old_context_revision,new_context_revision,
        source_custody_action_id,source_adapter_id,source_adapter_contract_digest,
        source_principal_generation,source_bridge_generation,bridge_owner_kind,
        source_bridge_row_id,source_bridge_revision,source_capability_hash,
        source_project_session_generation,source_run_generation,
        source_chair_lease_generation,checkpoint_state,checkpoint_ref,
        checkpoint_digest,loss_evidence_digest,creation_json,creation_digest,created_at
      ) VALUES (?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?)
    `).run(
      input.projectSessionId,
      input.runId,
      input.agentId,
      generationLossId,
      lossKind,
      source.oldProviderSessionRef,
      source.newProviderSessionRef,
      identity.providerGeneration,
      input.providerGeneration,
      context.contextRevision,
      input.contextRevision,
      source.sourceActionRef.actionId,
      source.sourceActionRef.adapterId,
      source.sourceAdapterContractDigest,
      source.sourcePrincipalGeneration,
      source.sourceBridgeGeneration,
      source.bridgeOwnerKind,
      source.sourceBridgeRowId,
      source.sourceBridgeRevision,
      source.sourceCapabilityHash,
      source.sourceProjectSessionGeneration,
      source.sourceRunGeneration,
      source.sourceChairLeaseGeneration,
      source.checkpoint.state,
      source.checkpoint.ref,
      source.checkpoint.digest,
      input.evidenceDigest,
      creationJson,
      creationDigest,
      input.observedAt,
    );
    const semantic = generationLossRevisionBody({
      generationLossId,
      revision: 1,
      state: "open",
      abandonKind: "none",
      recoveryActionRef: null,
      activeRecoveryCustodyId: null,
      terminalEvidenceDigest: null,
    });
    const semanticJson = canonicalJson(semantic);
    const semanticDigest = lifecycleDigest("generation-loss-semantic", semantic);
    const sourceRefDigest = semanticDigest;
    const journal = {
      schemaVersion: 1,
      ownerRef: {
        kind: "generation-loss",
        generationLossRef: generationLossRef(input.runId, input.agentId, generationLossId, 1),
        sourceRefDigest,
      },
      priorJournalDigest: null,
      semanticDigest,
      sourceRefDigest,
      authorityBatchId: null,
      authorityApplyId: null,
      authorityApplyDigest: null,
      originFreshApplyId: null,
      originFreshApplyDigest: null,
      recordedAt: input.observedAt,
    };
    const journalJson = canonicalJson(journal);
    const journalDigest = lifecycleDigest("generation-loss-journal", journal);
    this.#database.prepare(`
      INSERT INTO lifecycle_generation_loss_revisions(
        project_session_id,run_id,agent_id,generation_loss_id,revision,
        prior_revision,prior_journal_digest,state,abandon_kind_code,
        recovery_action_adapter_id,recovery_action_id,active_recovery_custody_id,
        terminal_evidence_digest,semantic_json,semantic_digest,source_ref_digest,
        origin_fresh_apply_id,origin_fresh_apply_digest,receipt_batch_id,
        receipt_apply_id,receipt_apply_digest,journal_json,journal_digest,recorded_at
      ) VALUES (?,?,?,?,1,NULL,NULL,'open','none',NULL,NULL,NULL,NULL,?,?,?,
                NULL,NULL,NULL,NULL,NULL,?,?,?)
    `).run(
      input.projectSessionId,
      input.runId,
      input.agentId,
      generationLossId,
      semanticJson,
      semanticDigest,
      sourceRefDigest,
      journalJson,
      journalDigest,
      input.observedAt,
    );
    this.#database.prepare(`
      INSERT INTO lifecycle_generation_loss_heads(
        project_session_id,run_id,agent_id,generation_loss_id,current_revision,
        state,abandon_kind_code,semantic_digest,source_ref_digest,journal_digest,
        terminal,head_revision
      ) VALUES (?,?,?,?,1,'open','none',?,?,?,0,1)
    `).run(
      input.projectSessionId,
      input.runId,
      input.agentId,
      generationLossId,
      semanticDigest,
      sourceRefDigest,
      journalDigest,
    );
  }

  #ratchetHighWater(
    input: RecordGenerationLossObservationInput,
    identity: IdentityHighWater,
    context: ContextHighWater,
    classification: "context-advance" | "generation-advance",
    persisted: boolean,
  ): void {
    if (!persisted) {
      this.#database.prepare(`
        INSERT INTO agent_lifecycle_identity_high_water(
          run_id,agent_id,provider_generation,principal_generation,revision
        ) VALUES (?,?,?,?,?)
      `).run(
        input.runId,
        input.agentId,
        identity.providerGeneration,
        identity.principalGeneration,
        identity.revision,
      );
      this.#database.prepare(`
        INSERT INTO agent_lifecycle_context_high_water(
          run_id,agent_id,provider_generation,context_revision,revision
        ) VALUES (?,?,?,?,?)
      `).run(
        input.runId,
        input.agentId,
        identity.providerGeneration,
        context.contextRevision,
        context.revision,
      );
    }
    if (classification === "context-advance") {
      const changed = this.#database.prepare(`
        UPDATE agent_lifecycle_context_high_water
           SET context_revision=?,revision=revision+1
         WHERE run_id=? AND agent_id=? AND provider_generation=?
           AND context_revision=? AND revision=?
      `).run(
        input.contextRevision,
        input.runId,
        input.agentId,
        identity.providerGeneration,
        context.contextRevision,
        context.revision,
      );
      if (changed.changes !== 1) throw new Error("generation-loss context high-water compare-and-set failed");
      return;
    }

    const nextIdentityRevision = identity.revision + 1;
    positiveInteger(nextIdentityRevision, "generation-loss identity high-water revision");
    const changed = this.#database.prepare(`
      UPDATE agent_lifecycle_identity_high_water
         SET provider_generation=?,revision=?
       WHERE run_id=? AND agent_id=? AND provider_generation=?
         AND principal_generation=? AND revision=?
    `).run(
      input.providerGeneration,
      nextIdentityRevision,
      input.runId,
      input.agentId,
      identity.providerGeneration,
      identity.principalGeneration,
      identity.revision,
    );
    if (changed.changes !== 1) {
      throw new Error("generation-loss identity high-water compare-and-set failed");
    }
    this.#database.prepare(`
      INSERT INTO agent_lifecycle_context_high_water(
        run_id,agent_id,provider_generation,context_revision,revision
      ) VALUES (?,?,?,?,1)
    `).run(input.runId, input.agentId, input.providerGeneration, input.contextRevision);
  }
}
