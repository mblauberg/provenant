import type {
  ArtifactRef,
  JsonValue,
  OperatorActionIntent,
  ScopedGate,
  Sha256Digest,
} from "@local/agent-fabric-protocol";
import {
  assertPromotionIntentGate,
  parseArtifactRef,
  parseIdentifier,
  parseSha256Digest,
} from "@local/agent-fabric-protocol";
import type Database from "better-sqlite3";

import type {
  OperatorActionCurrentState,
  OperatorEffectOutcome,
  OperatorEffectRequest,
} from "./action-store.js";
import { ProjectFabricCoreError } from "../project-session/contracts.js";
import { canonicalJson, integer, isRow, nullableText, row, sha256, text, type Row } from "../persistence/row-codec.js";

export type RegisteredEffectPort = Readonly<{
  integrationId: string;
  generation: number;
  contractDigest: Sha256Digest;
  operations: Readonly<Record<string, Readonly<{
    contractDigest: Sha256Digest;
    targets: Readonly<Record<string, Readonly<{ revision: number }>>>;
  }>>>;
  dispatch(input: Readonly<{
    custodyId: string;
    operationId: string;
    targetId: string;
    targetRevision: number;
    requestArtifactRef: ArtifactRef;
    idempotencyKey: string;
  }>): Promise<unknown>;
  lookup(input: Readonly<{ custodyId: string; idempotencyKey: string }>): Promise<unknown>;
}>;

export type ExternalEffectEvidencePort = Readonly<{
  inspectArtifact(ref: ArtifactRef): Promise<ArtifactRef | null>;
  inspectAcceptedDeliveryReceipt(ref: ArtifactRef): Promise<Readonly<{
    status: "accepted";
    receiptRef: ArtifactRef;
    artifactDigest: Sha256Digest;
    promotionAction: string;
    target: string;
  }> | null>;
}>;

export type ExternalEffectServiceOptions = Readonly<{
  database: Database.Database;
  registry: readonly RegisteredEffectPort[];
  evidence: ExternalEffectEvidencePort;
  gates: Readonly<{ getGate(gateId: string): ScopedGate }>;
  clock?: () => number;
  fault?: (label: string) => void;
}>;

const externalEffectDispatchHandleBrand: unique symbol = Symbol("externalEffectDispatchHandle");
export type ExternalEffectDispatchHandle = Readonly<{
  custodyId: string;
  [externalEffectDispatchHandleBrand]: true;
}>;

export type ExternalEffectRecoveryResult = Readonly<{
  custodyId: string;
  priorState: "prepared" | "dispatching" | "ambiguous" | "failed";
  outcome: OperatorEffectOutcome;
}>;

export class ExternalEffectService {
  readonly #database: Database.Database;
  readonly #registry: ReadonlyMap<string, RegisteredEffectPort>;
  readonly #gates: ExternalEffectServiceOptions["gates"];
  readonly #evidence: ExternalEffectEvidencePort;
  readonly #clock: () => number;
  readonly #fault: (label: string) => void;
  readonly #preparedHandles = new Map<string, ExternalEffectDispatchHandle>();
  readonly #handleRequests = new WeakMap<ExternalEffectDispatchHandle, OperatorEffectRequest>();
  readonly #inFlightLookups = new Map<string, Promise<OperatorEffectOutcome>>();

  constructor(options: ExternalEffectServiceOptions) {
    this.#database = options.database;
    this.#registry = snapshotRegistry(options.registry);
    this.#gates = options.gates;
    this.#evidence = options.evidence;
    this.#clock = options.clock ?? Date.now;
    this.#fault = options.fault ?? (() => undefined);
  }

  async readCurrentState(intent: OperatorActionIntent): Promise<OperatorActionCurrentState> {
    if (intent.kind === "promotion") {
      this.#promotionPort(intent.releaseBinding.promotionAction, intent.releaseBinding.target);
      const gate = this.#gates.getGate(intent.gateId);
      return { kind: "promotion", revision: gate.revision, gate };
    }
    if (intent.kind !== "registered-external-effect") {
      throw new ProjectFabricCoreError("CAPABILITY_FORBIDDEN", "intent is outside registered external-effect custody");
    }
    const port = this.#registry.get(intent.integrationId);
    if (port === undefined) {
      throw new ProjectFabricCoreError("CAPABILITY_FORBIDDEN", "registered external-effect integration is unavailable");
    }
    const operationContracts: Record<string, Sha256Digest> = {};
    const targetRevisions: Record<string, number> = {};
    for (const [operationId, operation] of Object.entries(port.operations)) {
      operationContracts[operationId] = operation.contractDigest;
      for (const [targetId, target] of Object.entries(operation.targets)) {
        const prior = targetRevisions[targetId];
        if (prior !== undefined && prior !== target.revision) {
          throw new ProjectFabricCoreError("CONFLICT", "registered external-effect target has conflicting revisions");
        }
        targetRevisions[targetId] = target.revision;
      }
    }
    const targetRevision = targetRevisions[intent.targetId];
    if (targetRevision === undefined) {
      throw new ProjectFabricCoreError("CAPABILITY_FORBIDDEN", "registered external-effect target is unavailable");
    }
    return {
      kind: "registered-external-effect",
      revision: targetRevision,
      state: {
        integrationId: parseIdentifier<"IntegrationId">(port.integrationId, "externalEffect.integrationId"),
        integrationGeneration: port.generation,
        operationContracts,
        targetRevisions,
      },
    };
  }

  prepareInTransaction(request: OperatorEffectRequest): ExternalEffectDispatchHandle {
    const parent = this.#parentCustody(request);
    const custodyId = text(parent, "custody_id");
    const binding = this.#bindingForIntent(request.intent, custodyId);
    const existing = this.#database.prepare(`
      SELECT * FROM operator_external_effect_bindings WHERE custody_id=?
    `).get(custodyId);
    if (isRow(existing)) {
      this.#assertStoredBinding(existing, binding);
    } else {
      try {
        this.#database.prepare(`
          INSERT INTO operator_external_effect_bindings(
            custody_id, effect_kind, integration_id, integration_generation,
            operation_id, contract_digest, target_id, target_revision,
            request_artifact_path, request_artifact_digest, idempotency_key,
            release_gate_id, release_gate_revision, release_binding_digest,
            lookup_generation, lookup_evidence_digest, created_at
          ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, 0, NULL, ?)
        `).run(
          custodyId,
          binding.effectKind,
          binding.integrationId,
          binding.integrationGeneration,
          binding.operationId,
          binding.contractDigest,
          binding.targetId,
          binding.targetRevision,
          binding.requestArtifactRef.path,
          binding.requestArtifactRef.digest,
          binding.idempotencyKey,
          binding.releaseGateId,
          binding.releaseGateRevision,
          binding.releaseBindingDigest,
          this.#clock(),
        );
      } catch (error: unknown) {
        if (error instanceof Error && /(?:UNIQUE|INVARIANT_)/u.test(error.message)) {
          throw new ProjectFabricCoreError("DEDUPE_CONFLICT", "external-effect custody binding conflicts", {
            cause: error,
          });
        }
        throw error;
      }
    }
    const prior = this.#preparedHandles.get(custodyId);
    if (prior !== undefined) return prior;
    const handle: ExternalEffectDispatchHandle = Object.freeze({
      custodyId,
      [externalEffectDispatchHandleBrand]: true as const,
    });
    this.#preparedHandles.set(custodyId, handle);
    this.#handleRequests.set(handle, request);
    return handle;
  }

  async dispatchPrepared(handle: ExternalEffectDispatchHandle): Promise<OperatorEffectOutcome> {
    const request = this.#handleRequests.get(handle);
    if (
      request === undefined ||
      handle[externalEffectDispatchHandleBrand] !== true ||
      this.#preparedHandles.get(handle.custodyId) !== handle
    ) {
      throw new ProjectFabricCoreError("DEDUPE_CONFLICT", "external-effect dispatch handle is stale or foreign");
    }
    this.#preparedHandles.delete(handle.custodyId);
    this.#handleRequests.delete(handle);
    const parent = row(this.#database.prepare(`
      SELECT * FROM operator_effect_custody WHERE custody_id=?
    `).get(handle.custodyId), "external-effect generic custody");
    if (text(parent, "state") !== "dispatching") {
      throw new ProjectFabricCoreError("STALE_REVISION", "external-effect custody is not dispatching");
    }
    this.#assertRequestParentIdentity(request, parent);
    const binding = this.#storedBinding(handle.custodyId);
    const port = this.#livePort(binding);
    if (request.intent.kind === "registered-external-effect") {
      let observedArtifact: ArtifactRef | null;
      try {
        observedArtifact = await this.#evidence.inspectArtifact(binding.requestArtifactRef);
      } catch {
        return this.#settleNoEffect(binding, "external-contract-unknown");
      }
      if (!sameArtifact(observedArtifact, binding.requestArtifactRef)) {
        return this.#settleNoEffect(binding, "external-contract-stale");
      }
    } else if (request.intent.kind === "promotion") {
      if (!this.#promotionCurrent(request.intent, binding)) {
        return this.#settleNoEffect(binding, "release-binding-mismatch");
      }
      let receipt: Awaited<ReturnType<ExternalEffectEvidencePort["inspectAcceptedDeliveryReceipt"]>>;
      try {
        receipt = await this.#evidence.inspectAcceptedDeliveryReceipt(binding.requestArtifactRef);
      } catch {
        return this.#settleNoEffect(binding, "release-binding-mismatch");
      }
      if (
        receipt === null ||
        receipt.status !== "accepted" ||
        !sameArtifact(receipt.receiptRef, request.intent.releaseBinding.acceptedDeliveryReceiptRef) ||
        receipt.artifactDigest !== request.intent.releaseBinding.artifactDigest ||
        receipt.promotionAction !== request.intent.releaseBinding.promotionAction ||
        receipt.target !== request.intent.releaseBinding.target ||
        !this.#promotionCurrent(request.intent, binding)
      ) {
        return this.#settleNoEffect(binding, "release-binding-mismatch");
      }
    } else {
      throw new ProjectFabricCoreError("CAPABILITY_FORBIDDEN", "intent is outside external-effect custody");
    }
    let raw: unknown;
    try {
      raw = await port.dispatch({
        custodyId: binding.custodyId,
        operationId: binding.operationId,
        targetId: binding.targetId,
        targetRevision: binding.targetRevision,
        requestArtifactRef: binding.requestArtifactRef,
        idempotencyKey: binding.idempotencyKey,
      });
    } catch {
      return this.#settleAmbiguous(binding, digestValue({ kind: "adapter-dispatch-threw" }, "externalEffect.adapterFailure"));
    }
    this.#fault("external-effect:after-dispatch");
    return this.#settleRawOutcome(binding, raw);
  }

  async observe(request: OperatorEffectRequest): Promise<OperatorEffectOutcome> {
    const parent = this.#findParentCustody(request);
    const custodyId = text(parent, "custody_id");
    const state = text(parent, "state");
    const binding = this.#storedBinding(custodyId);
    if (state === "prepared") {
      return this.#settleNoEffect(
        binding,
        binding.effectKind === "promotion" ? "release-binding-mismatch" : "external-contract-stale",
      );
    }
    if (state === "dispatching" || state === "ambiguous" || state === "failed") return await this.#lookup(binding);
    const stored = nullableText(parent, "outcome_json");
    if (stored === null) throw new ProjectFabricCoreError("CONFLICT", "terminal external-effect custody has no outcome");
    return parseStoredOutcome(stored);
  }

  async recover(): Promise<readonly ExternalEffectRecoveryResult[]> {
    const pending = this.#database.prepare(`
      SELECT binding.custody_id, custody.state
        FROM operator_external_effect_bindings binding
        JOIN operator_effect_custody custody ON custody.custody_id=binding.custody_id
       WHERE custody.state IN ('prepared','dispatching','ambiguous','failed')
       ORDER BY custody.created_at, binding.custody_id
    `).all().filter(isRow);
    const results: ExternalEffectRecoveryResult[] = [];
    for (const item of pending) {
      const custodyId = text(item, "custody_id");
      const state = text(item, "state");
      const binding = this.#storedBinding(custodyId);
      if (state === "prepared") {
        results.push({
          custodyId,
          priorState: state,
          outcome: this.#settleNoEffect(
            binding,
            binding.effectKind === "promotion" ? "release-binding-mismatch" : "external-contract-stale",
          ),
        });
      } else if (state === "dispatching" || state === "ambiguous" || state === "failed") {
        results.push({ custodyId, priorState: state, outcome: await this.#lookup(binding) });
      }
    }
    return results;
  }

  #promotionPort(operationId: string, targetId: string): RegisteredEffectPort {
    const matching = [...this.#registry.values()].filter((port) =>
      port.operations[operationId]?.targets[targetId] !== undefined);
    if (matching.length !== 1) {
      throw new ProjectFabricCoreError(
        "CAPABILITY_FORBIDDEN",
        matching.length === 0
          ? "registered promotion integration is unavailable"
          : "registered promotion route is ambiguous",
      );
    }
    const port = matching[0];
    if (port === undefined) throw new Error("promotion route disappeared");
    return port;
  }

  #parentCustody(request: OperatorEffectRequest): Row {
    const parent = this.#findParentCustody(request);
    if (text(parent, "state") !== "prepared") {
      throw new ProjectFabricCoreError("DEDUPE_CONFLICT", "external-effect generic custody is not prepared");
    }
    return parent;
  }

  #findParentCustody(request: OperatorEffectRequest): Row {
    if (
      request.operatorId === undefined ||
      request.projectId === undefined ||
      request.projectSessionId === undefined ||
      request.principalGeneration === undefined ||
      request.operation !== "external-effect"
    ) {
      throw new ProjectFabricCoreError("CAPABILITY_FORBIDDEN", "external-effect custody requires an exact operator scope");
    }
    const parent = row(this.#database.prepare(`
      SELECT * FROM operator_effect_custody
       WHERE operator_id=? AND project_id=? AND project_session_id=?
         AND principal_generation=? AND command_id=? AND operation='external-effect'
    `).get(
      request.operatorId,
      request.projectId,
      request.projectSessionId,
      request.principalGeneration,
      request.commandId,
    ), "external-effect generic custody");
    this.#assertRequestParentIdentity(request, parent);
    return parent;
  }

  #assertRequestParentIdentity(request: OperatorEffectRequest, parent: Row): void {
    if (
      request.operatorId === undefined ||
      request.projectId === undefined ||
      request.projectSessionId === undefined ||
      request.principalGeneration === undefined ||
      text(parent, "operator_id") !== request.operatorId ||
      text(parent, "project_id") !== request.projectId ||
      text(parent, "project_session_id") !== request.projectSessionId ||
      integer(parent, "principal_generation") !== request.principalGeneration ||
      text(parent, "command_id") !== request.commandId ||
      text(parent, "operation") !== "external-effect" ||
      text(parent, "intent_digest") !== request.intentDigest ||
      text(parent, "before_state_digest") !== request.beforeStateDigest ||
      text(parent, "intent_json") !== canonicalJson(request.intent)
    ) {
      throw new ProjectFabricCoreError("DEDUPE_CONFLICT", "external-effect generic custody identity changed");
    }
  }

  #storedBinding(custodyId: string): EffectBinding {
    const stored = row(this.#database.prepare(`
      SELECT * FROM operator_external_effect_bindings WHERE custody_id=?
    `).get(custodyId), "external-effect typed binding");
    const effectKind = text(stored, "effect_kind");
    if (effectKind !== "registered-external-effect" && effectKind !== "promotion") {
      throw new Error("stored external-effect kind is invalid");
    }
    return {
      custodyId,
      effectKind,
      integrationId: text(stored, "integration_id"),
      integrationGeneration: integer(stored, "integration_generation"),
      operationId: text(stored, "operation_id"),
      contractDigest: parseSha256Digest(text(stored, "contract_digest"), "externalEffect.binding.contractDigest"),
      targetId: text(stored, "target_id"),
      targetRevision: integer(stored, "target_revision"),
      requestArtifactRef: parseArtifactRef({
        path: text(stored, "request_artifact_path"),
        digest: parseSha256Digest(
          text(stored, "request_artifact_digest"),
          "externalEffect.binding.requestArtifactDigest",
        ),
      }, "externalEffect.binding.requestArtifactRef"),
      idempotencyKey: text(stored, "idempotency_key"),
      releaseGateId: nullableText(stored, "release_gate_id"),
      releaseGateRevision: stored.release_gate_revision === null ? null : integer(stored, "release_gate_revision"),
      releaseBindingDigest: stored.release_binding_digest === null
        ? null
        : parseSha256Digest(
          text(stored, "release_binding_digest"),
          "externalEffect.binding.releaseBindingDigest",
        ),
      lookupGeneration: integer(stored, "lookup_generation"),
      lookupEvidenceDigest: stored.lookup_evidence_digest === null
        ? null
        : parseSha256Digest(
          text(stored, "lookup_evidence_digest"),
          "externalEffect.binding.lookupEvidenceDigest",
        ),
    };
  }

  #livePort(binding: EffectBinding): RegisteredEffectPort {
    const port = this.#registry.get(binding.integrationId);
    const operation = port?.operations[binding.operationId];
    const target = operation?.targets[binding.targetId];
    if (
      port === undefined ||
      port.generation !== binding.integrationGeneration ||
      operation === undefined ||
      operation.contractDigest !== binding.contractDigest ||
      target === undefined ||
      target.revision !== binding.targetRevision
    ) {
      throw new ProjectFabricCoreError("STALE_REVISION", "external-effect registry binding changed");
    }
    return port;
  }

  #lookup(binding: EffectBinding): Promise<OperatorEffectOutcome> {
    const inFlight = this.#inFlightLookups.get(binding.custodyId);
    if (inFlight !== undefined) return inFlight;
    const promise = this.#lookupOnce(binding);
    this.#inFlightLookups.set(binding.custodyId, promise);
    const clear = (): void => {
      if (this.#inFlightLookups.get(binding.custodyId) === promise) {
        this.#inFlightLookups.delete(binding.custodyId);
      }
    };
    void promise.then(clear, clear);
    return promise;
  }

  async #lookupOnce(binding: EffectBinding): Promise<OperatorEffectOutcome> {
    let port: RegisteredEffectPort;
    try {
      port = this.#livePort(binding);
    } catch {
      return this.#settleLookupAmbiguous(
        binding,
        digestValue({ kind: "registered-integration-unavailable" }, "externalEffect.lookupEvidence"),
      );
    }
    let raw: unknown;
    try {
      raw = await port.lookup({ custodyId: binding.custodyId, idempotencyKey: binding.idempotencyKey });
    } catch {
      return this.#settleLookupAmbiguous(
        binding,
        digestValue({ kind: "adapter-lookup-threw" }, "externalEffect.lookupEvidence"),
      );
    }
    this.#fault("external-effect:after-lookup");
    const rawEvidence = boundedRawDigest(raw);
    const lookupEvidence = digestValue({
      custodyId: binding.custodyId,
      lookupGeneration: binding.lookupGeneration + 1,
      rawOutcomeDigest: rawEvidence.digest,
    }, "externalEffect.lookupEvidence");
    return this.#database.transaction(() => {
      this.#advanceLookup(binding, lookupEvidence);
      return this.#settleRawOutcome(binding, raw);
    })();
  }

  #settleLookupAmbiguous(binding: EffectBinding, lookupEvidence: Sha256Digest): OperatorEffectOutcome {
    return this.#database.transaction(() => {
      this.#advanceLookup(binding, lookupEvidence);
      return this.#settleAmbiguous(binding, lookupEvidence);
    })();
  }

  #advanceLookup(binding: EffectBinding, evidenceDigest: Sha256Digest): void {
    const changed = this.#database.prepare(`
      UPDATE operator_external_effect_bindings
         SET lookup_generation=lookup_generation+1, lookup_evidence_digest=?
       WHERE custody_id=? AND lookup_generation=?
    `).run(evidenceDigest, binding.custodyId, binding.lookupGeneration);
    if (changed.changes !== 1) {
      throw new ProjectFabricCoreError("STALE_REVISION", "external-effect lookup generation changed concurrently");
    }
  }

  #settleRawOutcome(binding: EffectBinding, raw: unknown): OperatorEffectOutcome {
    const rawEvidence = boundedRawDigest(raw);
    const closed = rawEvidence.bounded ? parseClosedAdapterOutcome(raw, binding) : null;
    if (closed === null) return this.#settleAmbiguous(binding, rawEvidence.digest);
    if (closed.outcome === "ambiguous") return this.#settleAmbiguous(binding, closed.evidenceDigest, rawEvidence.digest);
    const effectRef = this.#effectRef(binding, closed.outcome, closed.evidenceDigest, rawEvidence.digest);
    const outcome: OperatorEffectOutcome = {
      status: "committed",
      afterState: {
        schemaVersion: 1,
        externalEffect: closed.outcome,
        custodyId: binding.custodyId,
        evidenceDigest: closed.evidenceDigest,
        rawOutcomeDigest: rawEvidence.digest,
      },
      ...(closed.outcome === "committed" ? { effectRef } : {}),
    };
    this.#storeGenericOutcome(binding.custodyId, closed.outcome === "no-effect" ? "no-effect" : "terminal", outcome);
    return outcome;
  }

  #settleNoEffect(
    binding: EffectBinding,
    code: "external-contract-unknown" | "external-contract-stale" | "release-binding-mismatch",
  ): OperatorEffectOutcome {
    const outcome: OperatorEffectOutcome = { status: "rejected", code, evidenceRefs: [] };
    this.#storeGenericOutcome(binding.custodyId, "no-effect", outcome);
    return outcome;
  }

  #promotionCurrent(
    intent: Extract<OperatorActionIntent, { kind: "promotion" }>,
    binding: EffectBinding,
  ): boolean {
    if (
      binding.effectKind !== "promotion" ||
      binding.releaseGateId !== intent.gateId ||
      binding.releaseGateRevision !== intent.expectedGateRevision ||
      binding.releaseBindingDigest !== digestValue(intent.releaseBinding, "externalEffect.releaseBindingDigest") ||
      binding.operationId !== intent.releaseBinding.promotionAction ||
      binding.targetId !== intent.releaseBinding.target ||
      !sameArtifact(binding.requestArtifactRef, intent.releaseBinding.acceptedDeliveryReceiptRef)
    ) return false;
    try {
      assertPromotionIntentGate(intent, this.#gates.getGate(intent.gateId));
      return true;
    } catch {
      return false;
    }
  }

  #settleAmbiguous(
    binding: EffectBinding,
    evidenceDigest: Sha256Digest,
    rawOutcomeDigest: Sha256Digest = evidenceDigest,
  ): OperatorEffectOutcome {
    const outcome: OperatorEffectOutcome = {
      status: "ambiguous",
      effectRef: this.#effectRef(binding, "ambiguous", evidenceDigest, rawOutcomeDigest),
    };
    this.#storeGenericOutcome(binding.custodyId, "ambiguous", outcome);
    return outcome;
  }

  #effectRef(
    binding: EffectBinding,
    outcome: "committed" | "no-effect" | "ambiguous",
    evidenceDigest: Sha256Digest,
    rawOutcomeDigest: Sha256Digest,
  ): ArtifactRef {
    return parseArtifactRef({
      path: `.agent-fabric/operator-effects/${binding.custodyId}.json`,
      digest: digestValue({
        custodyId: binding.custodyId,
        outcome,
        evidenceDigest,
        rawOutcomeDigest,
      }, "externalEffect.effectRef"),
    }, "externalEffect.effectRef");
  }

  #storeGenericOutcome(
    custodyId: string,
    state: "terminal" | "no-effect" | "ambiguous",
    outcome: OperatorEffectOutcome,
  ): void {
    const effectRef = outcome.status === "ambiguous" ||
      (outcome.status === "committed" && outcome.effectRef !== undefined)
      ? outcome.effectRef
      : null;
    const changed = this.#database.prepare(`
      UPDATE operator_effect_custody
         SET state=?, effect_path=?, effect_digest=?, outcome_json=?, updated_at=?
       WHERE custody_id=? AND state IN ('prepared','dispatching','ambiguous','failed')
    `).run(
      state,
      effectRef?.path ?? null,
      effectRef?.digest ?? null,
      canonicalJson(outcome),
      this.#clock(),
      custodyId,
    );
    if (changed.changes !== 1) {
      throw new ProjectFabricCoreError("STALE_REVISION", "external-effect custody outcome changed concurrently");
    }
  }

  #bindingForIntent(intent: OperatorActionIntent, custodyId: string): EffectBinding {
    if (intent.kind === "registered-external-effect") {
      const port = this.#registry.get(intent.integrationId);
      if (port === undefined) {
        throw new ProjectFabricCoreError("CAPABILITY_FORBIDDEN", "registered external-effect integration is unavailable");
      }
      const operation = port.operations[intent.operationId];
      const target = operation?.targets[intent.targetId];
      if (
        port.generation !== intent.expectedIntegrationGeneration ||
        operation === undefined ||
        operation.contractDigest !== intent.contractDigest ||
        target === undefined ||
        target.revision !== intent.expectedTargetRevision
      ) {
        throw new ProjectFabricCoreError("STALE_REVISION", "registered external-effect binding changed");
      }
      return {
        custodyId,
        effectKind: intent.kind,
        integrationId: port.integrationId,
        integrationGeneration: port.generation,
        operationId: intent.operationId,
        contractDigest: operation.contractDigest,
        targetId: intent.targetId,
        targetRevision: target.revision,
        requestArtifactRef: intent.requestArtifactRef,
        idempotencyKey: intent.idempotencyKey,
        releaseGateId: null,
        releaseGateRevision: null,
        releaseBindingDigest: null,
        lookupGeneration: 0,
        lookupEvidenceDigest: null,
      };
    }
    if (intent.kind !== "promotion") {
      throw new ProjectFabricCoreError("CAPABILITY_FORBIDDEN", "intent is outside external-effect custody");
    }
    const gate = this.#gates.getGate(intent.gateId);
    try {
      assertPromotionIntentGate(intent, gate);
    } catch (error: unknown) {
      throw new ProjectFabricCoreError("GATE_BLOCKED", "promotion release binding is not current", {
        cause: error,
      });
    }
    const port = this.#promotionPort(intent.releaseBinding.promotionAction, intent.releaseBinding.target);
    const operation = port.operations[intent.releaseBinding.promotionAction];
    const target = operation?.targets[intent.releaseBinding.target];
    if (operation === undefined || target === undefined) throw new Error("promotion route disappeared");
    const releaseBindingDigest = digestValue(intent.releaseBinding, "externalEffect.releaseBindingDigest");
    return {
      custodyId,
      effectKind: intent.kind,
      integrationId: port.integrationId,
      integrationGeneration: port.generation,
      operationId: intent.releaseBinding.promotionAction,
      contractDigest: operation.contractDigest,
      targetId: intent.releaseBinding.target,
      targetRevision: target.revision,
      requestArtifactRef: intent.releaseBinding.acceptedDeliveryReceiptRef,
      idempotencyKey: `promotion:${sha256(canonicalJson({
        gateId: intent.gateId,
        gateRevision: intent.expectedGateRevision,
        releaseBindingDigest,
      }))}`,
      releaseGateId: intent.gateId,
      releaseGateRevision: intent.expectedGateRevision,
      releaseBindingDigest,
      lookupGeneration: 0,
      lookupEvidenceDigest: null,
    };
  }

  #assertStoredBinding(stored: Row, expected: EffectBinding): void {
    if (
      text(stored, "custody_id") !== expected.custodyId ||
      text(stored, "effect_kind") !== expected.effectKind ||
      text(stored, "integration_id") !== expected.integrationId ||
      integer(stored, "integration_generation") !== expected.integrationGeneration ||
      text(stored, "operation_id") !== expected.operationId ||
      text(stored, "contract_digest") !== expected.contractDigest ||
      text(stored, "target_id") !== expected.targetId ||
      integer(stored, "target_revision") !== expected.targetRevision ||
      text(stored, "request_artifact_path") !== expected.requestArtifactRef.path ||
      text(stored, "request_artifact_digest") !== expected.requestArtifactRef.digest ||
      text(stored, "idempotency_key") !== expected.idempotencyKey ||
      nullableText(stored, "release_gate_id") !== expected.releaseGateId ||
      (stored.release_gate_revision === null ? null : integer(stored, "release_gate_revision")) !== expected.releaseGateRevision ||
      nullableText(stored, "release_binding_digest") !== expected.releaseBindingDigest
    ) {
      throw new ProjectFabricCoreError("DEDUPE_CONFLICT", "external-effect custody binding changed");
    }
  }
}

type EffectBinding = Readonly<{
  custodyId: string;
  effectKind: "registered-external-effect" | "promotion";
  integrationId: string;
  integrationGeneration: number;
  operationId: string;
  contractDigest: Sha256Digest;
  targetId: string;
  targetRevision: number;
  requestArtifactRef: ArtifactRef;
  idempotencyKey: string;
  releaseGateId: string | null;
  releaseGateRevision: number | null;
  releaseBindingDigest: Sha256Digest | null;
  lookupGeneration: number;
  lookupEvidenceDigest: Sha256Digest | null;
}>;

function digestValue(value: unknown, path: string): Sha256Digest {
  return parseSha256Digest(`sha256:${sha256(canonicalJson(value))}`, path);
}

type ClosedAdapterOutcome = Readonly<{
  schemaVersion: 1;
  custodyId: string;
  idempotencyKey: string;
  outcome: "committed" | "no-effect" | "ambiguous";
  evidenceDigest: Sha256Digest;
}>;

function parseClosedAdapterOutcome(raw: unknown, binding: EffectBinding): ClosedAdapterOutcome | null {
  if (!isRow(raw)) return null;
  const keys = Object.keys(raw).sort();
  if (keys.join("\u0000") !== [
    "custodyId",
    "evidenceDigest",
    "idempotencyKey",
    "outcome",
    "schemaVersion",
  ].join("\u0000")) return null;
  if (
    raw.schemaVersion !== 1 ||
    raw.custodyId !== binding.custodyId ||
    raw.idempotencyKey !== binding.idempotencyKey ||
    (raw.outcome !== "committed" && raw.outcome !== "no-effect" && raw.outcome !== "ambiguous")
  ) return null;
  let evidenceDigest: Sha256Digest;
  try {
    evidenceDigest = parseSha256Digest(raw.evidenceDigest, "externalEffect.adapterOutcome.evidenceDigest");
  } catch {
    return null;
  }
  return {
    schemaVersion: 1,
    custodyId: binding.custodyId,
    idempotencyKey: binding.idempotencyKey,
    outcome: raw.outcome,
    evidenceDigest,
  };
}

function boundedRawDigest(raw: unknown): Readonly<{ digest: Sha256Digest; bounded: boolean }> {
  const transcript: string[] = [];
  let remainingBytes = 64 * 1024;
  let bounded = true;
  let nodes = 0;
  const seen = new WeakSet<object>();
  const append = (value: string): void => {
    if (remainingBytes <= 0) {
      bounded = false;
      return;
    }
    const bytes = Buffer.byteLength(value, "utf8");
    if (bytes <= remainingBytes) {
      transcript.push(value);
      remainingBytes -= bytes;
      return;
    }
    const prefix = value.slice(0, Math.min(value.length, remainingBytes));
    transcript.push(prefix, `#truncated:${String(bytes)}`);
    remainingBytes = 0;
    bounded = false;
  };
  const visit = (value: unknown, depth: number): void => {
    nodes += 1;
    if (nodes > 4_096 || depth > 32) {
      bounded = false;
      append(`#limit:${String(nodes)}:${String(depth)}`);
      return;
    }
    if (value === null) {
      append("null;");
      return;
    }
    if (typeof value === "string") {
      append(`string:${String(Buffer.byteLength(value, "utf8"))}:`);
      append(value);
      append(";");
      return;
    }
    if (typeof value === "boolean") {
      append(value ? "true;" : "false;");
      return;
    }
    if (typeof value === "number") {
      if (!Number.isFinite(value)) bounded = false;
      append(`number:${Number.isFinite(value) ? String(value) : "non-finite"};`);
      return;
    }
    if (typeof value !== "object") {
      bounded = false;
      append(`unsupported:${typeof value};`);
      return;
    }
    if (seen.has(value)) {
      bounded = false;
      append("cycle;");
      return;
    }
    seen.add(value);
    if (Array.isArray(value)) {
      append(`array:${String(value.length)}:[`);
      for (let index = 0; index < value.length && remainingBytes > 0 && nodes <= 4_096; index += 1) {
        visit(value[index], depth + 1);
      }
      if (remainingBytes <= 0 || nodes > 4_096) bounded = false;
      append("]; ");
      seen.delete(value);
      return;
    }
    const keys = Object.keys(value).sort();
    append(`object:${String(keys.length)}:{`);
    for (const key of keys) {
      if (remainingBytes <= 0 || nodes > 4_096) {
        bounded = false;
        break;
      }
      append(`key:${String(Buffer.byteLength(key, "utf8"))}:`);
      append(key);
      append("=");
      visit(Reflect.get(value, key), depth + 1);
    }
    append("};");
    seen.delete(value);
  };
  try {
    visit(raw, 0);
  } catch {
    bounded = false;
    append("inspection-threw;");
  }
  return {
    digest: parseSha256Digest(
      `sha256:${sha256(transcript.join(""))}`,
      "externalEffect.rawOutcomeDigest",
    ),
    bounded,
  };
}

function sameArtifact(actual: ArtifactRef | null, expected: ArtifactRef): boolean {
  return actual !== null && actual.path === expected.path && actual.digest === expected.digest;
}

function parseStoredOutcome(serialised: string): OperatorEffectOutcome {
  const parsed: unknown = JSON.parse(serialised);
  if (!isRow(parsed) || typeof parsed.status !== "string") throw new Error("stored external-effect outcome is invalid");
  if (parsed.status === "committed") {
    if (!isJsonValue(parsed.afterState)) throw new Error("stored external-effect after-state is invalid");
    const effectRef = parsed.effectRef === undefined
      ? undefined
      : parseArtifactRef(parsed.effectRef, "externalEffect.storedOutcome.effectRef");
    return {
      status: "committed",
      afterState: parsed.afterState,
      ...(effectRef === undefined ? {} : { effectRef }),
    };
  }
  if (parsed.status === "ambiguous") {
    return {
      status: "ambiguous",
      effectRef: parseArtifactRef(parsed.effectRef, "externalEffect.storedOutcome.effectRef"),
    };
  }
  if (parsed.status === "rejected") {
    const code = parsed.code;
    if (
      code !== "external-contract-unknown" &&
      code !== "external-contract-stale" &&
      code !== "release-binding-mismatch"
    ) throw new Error("stored external-effect rejection code is invalid");
    if (!Array.isArray(parsed.evidenceRefs)) throw new Error("stored external-effect evidence is invalid");
    return {
      status: "rejected",
      code,
      evidenceRefs: parsed.evidenceRefs.map((ref, index) =>
        parseArtifactRef(ref, `externalEffect.storedOutcome.evidenceRefs[${String(index)}]`)),
    };
  }
  throw new Error("stored external-effect outcome status is invalid");
}

function isJsonValue(value: unknown): value is JsonValue {
  if (value === null || typeof value === "string" || typeof value === "boolean") return true;
  if (typeof value === "number") return Number.isFinite(value);
  if (Array.isArray(value)) return value.every(isJsonValue);
  return isRow(value) && Object.values(value).every(isJsonValue);
}

function snapshotRegistry(ports: readonly RegisteredEffectPort[]): ReadonlyMap<string, RegisteredEffectPort> {
  const registry = new Map<string, RegisteredEffectPort>();
  for (const candidate of ports) {
    const integrationId = parseIdentifier<"IntegrationId">(
      candidate.integrationId,
      "externalEffect.registry.integrationId",
    );
    if (registry.has(integrationId)) throw new TypeError(`duplicate external-effect integration: ${integrationId}`);
    if (!Number.isSafeInteger(candidate.generation) || candidate.generation < 1) {
      throw new TypeError("external-effect integration generation must be a positive safe integer");
    }
    const operations: Record<string, RegisteredEffectPort["operations"][string]> = {};
    for (const [operationId, operation] of Object.entries(candidate.operations)) {
      assertBoundedIdentity(operationId, "external-effect operation ID", 256);
      const targets: Record<string, { revision: number }> = {};
      for (const [targetId, target] of Object.entries(operation.targets)) {
        assertBoundedIdentity(targetId, "external-effect target ID", 512);
        if (!Number.isSafeInteger(target.revision) || target.revision < 1) {
          throw new TypeError("external-effect target revision must be a positive safe integer");
        }
        targets[targetId] = Object.freeze({ revision: target.revision });
      }
      if (Object.keys(targets).length === 0) throw new TypeError("external-effect operation must register a target");
      operations[operationId] = Object.freeze({
        contractDigest: parseSha256Digest(
          operation.contractDigest,
          `externalEffect.registry.operations.${operationId}.contractDigest`,
        ),
        targets: Object.freeze(targets),
      });
    }
    if (Object.keys(operations).length === 0) throw new TypeError("external-effect integration must register an operation");
    registry.set(integrationId, Object.freeze({
      integrationId,
      generation: candidate.generation,
      contractDigest: parseSha256Digest(candidate.contractDigest, "externalEffect.registry.contractDigest"),
      operations: Object.freeze(operations),
      dispatch: candidate.dispatch.bind(candidate),
      lookup: candidate.lookup.bind(candidate),
    }));
  }
  return registry;
}

function assertBoundedIdentity(value: string, label: string, maximumLength: number): void {
  if (value.length < 1 || value.length > maximumLength || /[\u0000-\u001f\u007f]/u.test(value)) {
    throw new TypeError(`${label} is invalid`);
  }
}
