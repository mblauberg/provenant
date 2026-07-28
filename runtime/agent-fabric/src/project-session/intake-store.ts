import type {
  ChairMutationContext,
  ArtifactRef,
  Intake,
  IntakeChairRequestSeed,
  IntakeDraft,
  IntakeDraftCreateRequest,
  IntakeRevisionRequest,
  IntakeSubmission,
  TaskRequest,
  TaskRequestCommit,
} from "@local/agent-fabric-protocol";
import {
  assertChairMutationAuthority,
  parseIdentifier,
  parseIntake,
  parseIntakeRevisionRequest,
  parseTaskRequest,
} from "@local/agent-fabric-protocol";
import type Database from "better-sqlite3";

import type { OperatorStore } from "../operator/store.js";
import {
  ProjectFabricCoreError,
  type AuthenticatedAgentContext,
  type AuthenticatedOperatorContext,
  type CoreServiceOptions,
} from "./contracts.js";
import { canonicalJson, digest, integer, isRow, nullableText, row, sha256, text } from "../persistence/row-codec.js";

export interface IntakeTaskRequestCommitter {
  commitTaskRequest(request: TaskRequest): TaskRequestCommit;
}

export class IntakeStore {
  readonly #database: Database.Database;
  readonly #operatorStore: OperatorStore;
  readonly #clock: () => number;
  readonly #fault: (label: string) => void;
  readonly #requestCommitter: IntakeTaskRequestCommitter | undefined;
  readonly #registryV1: boolean;

  constructor(options: CoreServiceOptions & {
    operatorStore: OperatorStore;
    requestCommitter?: IntakeTaskRequestCommitter;
  }) {
    this.#database = options.database;
    this.#operatorStore = options.operatorStore;
    this.#clock = options.clock ?? Date.now;
    this.#fault = options.fault ?? (() => undefined);
    this.#requestCommitter = options.requestCommitter;
    this.#registryV1 = (this.#database.prepare("PRAGMA table_info(artifacts)").all() as Array<{ name?: unknown }>)
      .some(({ name }) => name === "project_id");
  }

  createDraft(context: AuthenticatedOperatorContext, request: IntakeDraftCreateRequest): IntakeDraft {
    return this.#operatorStore.executeCommand(
      context,
      request.command,
      {
        projectId: context.projectId,
        requiredAction: "launch",
        commandPayload: {
          intakeId: request.intakeId,
          dedupeKey: request.dedupeKey,
          summary: request.summary,
          artifactRefs: request.artifactRefs,
          gateIds: request.gateIds,
        },
      },
      () => {
        const existing = this.#database.prepare("SELECT revision FROM intakes WHERE intake_id=?").get(request.intakeId);
        if (existing === undefined) return { revision: 0, value: { intakeId: request.intakeId, state: "absent" } };
        const stored = row(existing, "intake");
        return { revision: integer(stored, "revision"), value: this.get(request.intakeId) };
      },
      () => {
        this.#fault("intake:draft");
        const artifactBindings = this.#resolveArtifactBindings(
          context.projectId,
          null,
          null,
          request.artifactRefs,
        );
        const now = this.#clock();
        const payload = {
          summary: request.summary,
          artifactRefs: request.artifactRefs,
          gateIds: request.gateIds,
        };
        const payloadJson = canonicalJson(payload);
        const payloadDigest = digest(payload);
        this.#database.prepare(`
          INSERT INTO intakes(
            intake_id, project_id, project_session_id, coordination_run_id,
            dedupe_key, state, revision, chair_request_id, chair_request_revision,
            summary, artifact_refs_json, gate_ids_json, payload_digest, created_at, updated_at
          ) VALUES (?, ?, NULL, NULL, ?, 'draft', 1, NULL, NULL, ?, ?, ?, ?, ?, ?)
        `).run(
          request.intakeId,
          context.projectId,
          request.dedupeKey,
          request.summary,
          canonicalJson(request.artifactRefs),
          canonicalJson(request.gateIds),
          payloadDigest,
          now,
          now,
        );
        this.#database.prepare(`
          INSERT INTO intake_revisions(intake_id, revision, state, payload_json, payload_digest, actor_ref, created_at)
          VALUES (?, 1, 'draft', ?, ?, ?, ?)
        `).run(request.intakeId, payloadJson, payloadDigest, context.operatorId, now);
        for (const artifact of artifactBindings) {
          this.#insertArtifactBinding(request.intakeId, 1, artifact);
        }
        return this.get(request.intakeId) as IntakeDraft;
      },
    );
  }

  get(intakeId: string): Intake {
    const stored = row(this.#database.prepare("SELECT * FROM intakes WHERE intake_id=?").get(intakeId), "intake");
    const state = text(stored, "state");
    const common = {
      intakeId: text(stored, "intake_id"),
      projectId: text(stored, "project_id"),
      revision: integer(stored, "revision"),
      state,
      dedupeKey: text(stored, "dedupe_key"),
      summary: text(stored, "summary"),
      artifactRefs: JSON.parse(text(stored, "artifact_refs_json")),
      gateIds: JSON.parse(text(stored, "gate_ids_json")),
    };
    if (state === "draft") return parseIntake(common);
    const acceptedScope = state === "accepted" && this.#registryV1
      ? row(this.#database.prepare(`
          SELECT artifact.relative_path, artifact.sha256
            FROM artifacts artifact JOIN intakes intake
              ON intake.accepted_scope_artifact_id=artifact.artifact_id
           WHERE intake.intake_id=? AND intake.accepted_scope_state='bound'
             AND artifact.registry_state='active'
        `).get(intakeId), "accepted intake scope")
      : undefined;
    const chairRequestSeed = this.#chairRequestSeed(stored);
    return parseIntake({
      ...common,
      projectSessionId: text(stored, "project_session_id"),
      coordinationRunId: text(stored, "coordination_run_id"),
      ...(chairRequestSeed === undefined ? {} : { chairRequestSeed }),
      ...(acceptedScope === undefined ? {} : {
        acceptedScopeRef: {
          path: text(acceptedScope, "relative_path"),
          digest: text(acceptedScope, "sha256"),
        },
      }),
    });
  }

  #chairRequestSeed(stored: ReturnType<typeof row>): IntakeChairRequestSeed | undefined {
    const requestId = nullableText(stored, "chair_request_id");
    if (requestId === null) return undefined;
    const priorValue = this.#database.prepare(`
      SELECT message.conversation_id, context.context_json
        FROM messages message
        LEFT JOIN message_contexts context ON context.message_id=message.message_id
       WHERE message.message_id=? AND message.run_id=?
    `).get(requestId, text(stored, "coordination_run_id"));
    if (!isRow(priorValue) || typeof priorValue.context_json !== "string") return undefined;
    const prior = parseTaskRequest(JSON.parse(text(priorValue, "context_json")));
    if (prior.request.conversationId !== text(priorValue, "conversation_id")) {
      throw new ProjectFabricCoreError("RECOVERY_REQUIRED", "persisted intake discussion correlation changed");
    }
    const chairValue = this.#database.prepare(`
      SELECT run.chair_agent_id, agent.provider_session_ref
        FROM runs run
        JOIN agents agent
          ON agent.run_id=run.run_id AND agent.agent_id=run.chair_agent_id
       WHERE run.run_id=? AND run.project_session_id=?
    `).get(text(stored, "coordination_run_id"), text(stored, "project_session_id"));
    if (!isRow(chairValue)) return undefined;
    const providerSessionRef = nullableText(chairValue, "provider_session_ref");
    if (providerSessionRef === null) return undefined;
    return {
      conversationId: prior.request.conversationId,
      targetAgentId: parseIdentifier<"AgentId">(
        text(chairValue, "chair_agent_id"),
        "intake.chairRequestSeed.targetAgentId",
      ),
      targetProviderSessionRef: parseIdentifier<"ProviderSessionRef">(
        providerSessionRef,
        "intake.chairRequestSeed.targetProviderSessionRef",
      ),
      baseRevision: prior.task.baseRevision,
    };
  }

  submit(context: AuthenticatedOperatorContext, request: IntakeSubmission): Intake {
    if (this.#requestCommitter === undefined) {
      throw new Error("IntakeStore requires the atomic task-request service for submission");
    }
    const session = row(this.#database.prepare(`
      SELECT project_id, generation FROM project_sessions WHERE project_session_id=?
    `).get(request.projectSessionId), "project session");
    if (text(session, "project_id") !== context.projectId) throw new Error("intake session is outside the operator project");
    return this.#operatorStore.executeCommand(
      context,
      request.command,
      {
        projectId: context.projectId,
        projectSessionId: request.projectSessionId,
        sessionGeneration: integer(session, "generation"),
        requiredAction: "decide",
        commandPayload: {
          intakeId: request.intakeId,
          expectedRevision: request.expectedRevision,
          projectSessionId: request.projectSessionId,
          coordinationRunId: request.coordinationRunId,
          summary: request.summary,
          artifactRefs: request.artifactRefs,
          gateIds: request.gateIds,
          chairRequest: request.chairRequest,
        },
      },
      () => {
        const intake = this.get(request.intakeId);
        return { revision: intake.revision, value: intake };
      },
      () => {
        const current = this.get(request.intakeId);
        if (current.projectId !== context.projectId) {
          throw new ProjectFabricCoreError("WRONG_PROJECT", "intake is outside the operator project");
        }
        if (current.state !== "draft" || current.revision !== request.expectedRevision) {
          throw new Error("intake revision or state changed");
        }
        if (
          request.chairRequest.projectSessionId !== request.projectSessionId ||
          request.chairRequest.coordinationRunId !== request.coordinationRunId ||
          request.chairRequest.request.intakeBinding?.intakeId !== request.intakeId ||
          request.chairRequest.request.intakeBinding.intakeRevision !== request.expectedRevision + 1
        ) throw new Error("chair request does not bind the next intake revision");
        if (this.#database.prepare(`
          SELECT 1 FROM runs WHERE project_session_id=? AND run_id=?
        `).get(request.projectSessionId, request.coordinationRunId) === undefined) {
          throw new ProjectFabricCoreError("NOT_FOUND", "intake coordination run was not found in the target session");
        }
        const artifactBindings = this.#resolveArtifactBindings(
          context.projectId,
          request.projectSessionId,
          request.coordinationRunId,
          request.artifactRefs,
        );
        const commit = this.#requestCommitter?.commitTaskRequest(request.chairRequest);
        if (commit === undefined) throw new Error("task request commit is unavailable");
        this.#fault("intake:request");
        const revision = request.expectedRevision + 1;
        const payload = {
          summary: request.summary,
          artifactRefs: request.artifactRefs,
          gateIds: request.gateIds,
          chairRequest: commit,
        };
        const payloadDigest = digest(payload);
        this.#database.prepare(`
          UPDATE intakes
             SET project_session_id=?, coordination_run_id=?, state='awaiting-chair', revision=?,
                 chair_request_id=?, chair_request_revision=?, summary=?, artifact_refs_json=?,
                 gate_ids_json=?, payload_digest=?, updated_at=?
           WHERE intake_id=? AND state='draft' AND revision=?
        `).run(
          request.projectSessionId,
          request.coordinationRunId,
          revision,
          request.chairRequest.request.messageId,
          commit.requestRevision,
          request.summary,
          canonicalJson(request.artifactRefs),
          canonicalJson(request.gateIds),
          payloadDigest,
          this.#clock(),
          request.intakeId,
          request.expectedRevision,
        );
        this.#database.prepare(`
          INSERT INTO intake_revisions(intake_id, revision, state, payload_json, payload_digest, actor_ref, created_at)
          VALUES (?, ?, 'awaiting-chair', ?, ?, ?, ?)
        `).run(
          request.intakeId,
          revision,
          canonicalJson(payload),
          payloadDigest,
          context.operatorId,
          this.#clock(),
        );
        for (const artifact of artifactBindings) {
          this.#insertArtifactBinding(request.intakeId, revision, artifact);
        }
        for (const gateId of request.gateIds) {
          const gate = row(this.#database.prepare(`
            SELECT revision FROM scoped_gates WHERE gate_id=? AND project_session_id=? AND coordination_run_id=?
          `).get(gateId, request.projectSessionId, request.coordinationRunId), "intake gate");
          this.#database.prepare(`
            INSERT INTO intake_gate_bindings(intake_id, intake_revision, gate_id, gate_revision)
            VALUES (?, ?, ?, ?)
          `).run(request.intakeId, revision, gateId, integer(gate, "revision"));
        }
        this.#fault("intake:bound");
        return this.get(request.intakeId);
      },
    );
  }

  revise(
    context: AuthenticatedOperatorContext | AuthenticatedAgentContext,
    input: IntakeRevisionRequest,
  ): Intake {
    const request = parseIntakeRevisionRequest(input);
    if (request.origin === "chair") {
      if (!("agentId" in context)) {
        throw new ProjectFabricCoreError("CAPABILITY_FORBIDDEN", "chair intake revision requires an agent principal");
      }
      return this.#executeChairRevision(context, request);
    }
    if (!("operatorId" in context)) {
      throw new ProjectFabricCoreError("CAPABILITY_FORBIDDEN", "operator intake revision requires an operator principal");
    }
    const current = this.#assertBoundTarget(request);
    const session = row(this.#database.prepare(`
      SELECT project_id, generation FROM project_sessions WHERE project_session_id=?
    `).get(request.projectSessionId), "intake project session");
    const projectId = text(session, "project_id");
    if (current.projectId !== projectId) {
      throw new ProjectFabricCoreError("WRONG_PROJECT", "intake is outside the requested project session");
    }
    return this.#operatorStore.executeCommand(
      context,
      request.command,
      {
        projectId,
        projectSessionId: request.projectSessionId,
        sessionGeneration: integer(session, "generation"),
        requiredAction: "decide",
        commandPayload: {
          origin: request.origin,
          intakeId: request.intakeId,
          projectSessionId: request.projectSessionId,
          coordinationRunId: request.coordinationRunId,
          expectedRevision: request.expectedRevision,
          state: request.state,
          summary: request.summary,
          artifactRefs: request.artifactRefs,
          gateIds: request.gateIds,
          ...(request.acceptedScopeRef === undefined ? {} : { acceptedScopeRef: request.acceptedScopeRef }),
          ...(request.chairRequest === undefined ? {} : { chairRequest: request.chairRequest }),
        },
      },
      () => {
        const intake = this.get(request.intakeId);
        return { revision: intake.revision, value: intake };
      },
      () => this.#applyRevision(context.operatorId, request),
    );
  }

  #executeChairRevision(
    context: AuthenticatedAgentContext,
    request: Extract<IntakeRevisionRequest, { origin: "chair" }>,
  ): Intake {
    const execute = this.#database.transaction((): Intake => {
      this.#assertChairAuthority(context, request.command);
      const payloadHash = sha256(canonicalJson(request));
      const existing = this.#database.prepare(`
        SELECT payload_hash, result_json FROM commands
         WHERE run_id=? AND actor_agent_id=? AND command_id=?
      `).get(context.coordinationRunId, context.agentId, request.command.commandId);
      if (existing !== undefined) {
        const stored = row(existing, "intake chair command");
        if (text(stored, "payload_hash") !== payloadHash) {
          throw new ProjectFabricCoreError("DEDUPE_CONFLICT", "chair command ID was reused with changed input");
        }
        return parseIntake(JSON.parse(text(stored, "result_json")));
      }
      const result = this.#applyRevision(context.agentId, request);
      this.#database.prepare(`
        INSERT INTO commands(run_id, actor_agent_id, command_id, payload_hash, result_json, created_at)
        VALUES (?, ?, ?, ?, ?, ?)
      `).run(
        context.coordinationRunId,
        context.agentId,
        request.command.commandId,
        payloadHash,
        canonicalJson(result),
        this.#clock(),
      );
      return result;
    });
    return execute();
  }

  #assertChairAuthority(context: AuthenticatedAgentContext, command: ChairMutationContext): void {
    if (
      context.projectSessionId !== command.projectSessionId ||
      context.coordinationRunId !== command.coordinationRunId
    ) {
      throw new ProjectFabricCoreError("WRONG_PROJECT", "agent context is bound to another session or run");
    }
    const run = row(this.#database.prepare(`
      SELECT chair_agent_id, chair_generation, revision
        FROM runs WHERE run_id=? AND project_session_id=?
    `).get(context.coordinationRunId, context.projectSessionId), "intake coordination run");
    const lease = row(this.#database.prepare(`
      SELECT lease_id, holder_agent_id, generation, status
        FROM run_chair_leases
       WHERE project_session_id=? AND run_id=? AND generation=?
    `).get(
      context.projectSessionId,
      context.coordinationRunId,
      integer(run, "chair_generation"),
    ), "intake chair lease");
    if (
      text(run, "chair_agent_id") !== context.agentId ||
      text(lease, "holder_agent_id") !== context.agentId ||
      text(lease, "status") !== "active"
    ) {
      throw new ProjectFabricCoreError("TASK_NOT_OWNER", "authenticated agent is not the active intake chair");
    }
    const capabilityValue = this.#database.prepare(`
      SELECT expires_at, revoked_at FROM capabilities
       WHERE run_id=? AND agent_id=? AND principal_generation=?
    `).get(
      context.coordinationRunId,
      context.agentId,
      context.principalGeneration,
    );
    if (!isRow(capabilityValue)) {
      throw new ProjectFabricCoreError("STALE_PRINCIPAL_GENERATION", "intake chair principal generation is stale");
    }
    const capability = capabilityValue;
    if (capability.revoked_at !== null || integer(capability, "expires_at") <= this.#clock()) {
      throw new ProjectFabricCoreError("STALE_PRINCIPAL_GENERATION", "intake chair principal is expired or revoked");
    }
    try {
      assertChairMutationAuthority(command, {
        agentId: context.agentId,
        projectSessionId: context.projectSessionId,
        coordinationRunId: context.coordinationRunId,
        principalGeneration: context.principalGeneration,
        chairLeaseId: parseIdentifier<"LeaseId">(text(lease, "lease_id"), "intake.chairLeaseId"),
        chairLeaseGeneration: integer(lease, "generation"),
        runRevision: integer(run, "revision"),
      });
    } catch (error: unknown) {
      throw new ProjectFabricCoreError(
        "STALE_LEASE_GENERATION",
        error instanceof Error ? error.message : String(error),
      );
    }
  }

  #assertBoundTarget(request: IntakeRevisionRequest): Exclude<Intake, IntakeDraft> {
    const current = this.get(request.intakeId);
    if (current.state === "draft") {
      throw new ProjectFabricCoreError("CONFLICT", "draft intake must be submitted before revision");
    }
    if (
      current.projectSessionId !== request.projectSessionId ||
      current.coordinationRunId !== request.coordinationRunId
    ) {
      throw new ProjectFabricCoreError("WRONG_PROJECT", "intake is bound to another session or coordination run");
    }
    if (this.#database.prepare(`
      SELECT 1 FROM runs WHERE project_session_id=? AND run_id=?
    `).get(request.projectSessionId, request.coordinationRunId) === undefined) {
      throw new ProjectFabricCoreError("NOT_FOUND", "intake coordination run was not found in the target session");
    }
    return current;
  }

  #applyRevision(actorRef: string, request: IntakeRevisionRequest): Intake {
    const current = this.#assertBoundTarget(request);
    if (current.revision !== request.expectedRevision) {
      throw new ProjectFabricCoreError("STALE_REVISION", "intake revision changed", {
        expected: request.expectedRevision,
        actual: current.revision,
        current,
      });
    }
    const gateBindings = request.gateIds.map((gateId) => {
      const gateValue = this.#database.prepare(`
        SELECT project_session_id, coordination_run_id, revision FROM scoped_gates WHERE gate_id=?
      `).get(gateId);
      if (!isRow(gateValue)) {
        throw new ProjectFabricCoreError("NOT_FOUND", "intake gate was not found");
      }
      const gate = gateValue;
      if (
        text(gate, "project_session_id") !== request.projectSessionId ||
        text(gate, "coordination_run_id") !== request.coordinationRunId
      ) {
        throw new ProjectFabricCoreError("WRONG_PROJECT", "intake gate is bound to another session or run");
      }
      return { gateId, revision: integer(gate, "revision") };
    });
    const revision = request.expectedRevision + 1;
    const artifactBindings = this.#resolveArtifactBindings(
      current.projectId,
      request.projectSessionId,
      request.coordinationRunId,
      request.artifactRefs,
    );
    const acceptedScopeArtifactId = request.acceptedScopeRef === undefined
      ? null
      : artifactBindings.find((binding) =>
          binding.path === request.acceptedScopeRef?.path && binding.digest === request.acceptedScopeRef.digest
        )?.artifactId ?? null;
    if (request.state === "accepted" && acceptedScopeArtifactId === null) {
      throw new ProjectFabricCoreError("CONFLICT", "accepted scope does not resolve to one exact active artifact binding");
    }
    const chairRequestCommit = request.chairRequest === undefined
      ? undefined
      : this.#commitRevisedChairRequest(request);
    const payload = {
      state: request.state,
      summary: request.summary,
      artifactRefs: request.artifactRefs,
      gateIds: request.gateIds,
      ...(request.acceptedScopeRef === undefined ? {} : { acceptedScopeRef: request.acceptedScopeRef }),
      ...(chairRequestCommit === undefined ? {} : { chairRequest: chairRequestCommit }),
    };
    const payloadJson = canonicalJson(payload);
    const payloadDigest = digest(payload);
    this.#fault("intake:revise:before-update");
    const changed = this.#database.prepare(this.#registryV1 ? `
      UPDATE intakes
         SET state=?, revision=?, summary=?, artifact_refs_json=?, gate_ids_json=?,
             chair_request_id=COALESCE(?, chair_request_id),
             chair_request_revision=COALESCE(?, chair_request_revision),
             payload_digest=?, updated_at=?, accepted_scope_artifact_id=?, accepted_scope_state=?
       WHERE intake_id=? AND project_id=? AND project_session_id=?
         AND coordination_run_id=? AND revision=? AND state<>'draft'
    ` : `
      UPDATE intakes
         SET state=?, revision=?, summary=?, artifact_refs_json=?, gate_ids_json=?,
             chair_request_id=COALESCE(?, chair_request_id),
             chair_request_revision=COALESCE(?, chair_request_revision),
             payload_digest=?, updated_at=?
       WHERE intake_id=? AND project_id=? AND project_session_id=?
         AND coordination_run_id=? AND revision=? AND state<>'draft'
    `).run(
      request.state,
      revision,
      request.summary,
      canonicalJson(request.artifactRefs),
      canonicalJson(request.gateIds),
      request.chairRequest?.request.messageId ?? null,
      chairRequestCommit?.requestRevision ?? null,
      payloadDigest,
      this.#clock(),
      ...(this.#registryV1
        ? [acceptedScopeArtifactId, request.state === "accepted" ? "bound" : "not-applicable"]
        : []),
      request.intakeId,
      current.projectId,
      request.projectSessionId,
      request.coordinationRunId,
      request.expectedRevision,
    );
    if (changed.changes !== 1) {
      throw new ProjectFabricCoreError("STALE_REVISION", "intake revision changed before commit");
    }
    if (this.#registryV1) this.#database.prepare(`
      INSERT INTO intake_revisions(
        intake_id, revision, state, payload_json, payload_digest, actor_ref, created_at,
        accepted_scope_artifact_id, accepted_scope_state
      ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)
    `).run(
      request.intakeId,
      revision,
      request.state,
      payloadJson,
      payloadDigest,
      actorRef,
      this.#clock(),
      acceptedScopeArtifactId,
      request.state === "accepted" ? "bound" : "not-applicable",
    );
    else this.#database.prepare(`
      INSERT INTO intake_revisions(intake_id, revision, state, payload_json, payload_digest, actor_ref, created_at)
      VALUES (?, ?, ?, ?, ?, ?, ?)
    `).run(request.intakeId, revision, request.state, payloadJson, payloadDigest, actorRef, this.#clock());
    for (const artifact of artifactBindings) {
      this.#insertArtifactBinding(request.intakeId, revision, artifact);
    }
    for (const binding of gateBindings) {
      this.#database.prepare(`
        INSERT INTO intake_gate_bindings(intake_id, intake_revision, gate_id, gate_revision)
        VALUES (?, ?, ?, ?)
      `).run(request.intakeId, revision, binding.gateId, binding.revision);
    }
    this.#fault("intake:revise:after-bindings");
    const priorAcceptedScope = current.state === "accepted" ? current.acceptedScopeRef : undefined;
    if (
      (priorAcceptedScope?.path ?? null) !== (request.acceptedScopeRef?.path ?? null) ||
      (priorAcceptedScope?.digest ?? null) !== (request.acceptedScopeRef?.digest ?? null)
    ) {
      this.#database.prepare("UPDATE projects SET revision=revision+1, updated_at=? WHERE project_id=?")
        .run(this.#clock(), current.projectId);
    }
    return this.get(request.intakeId);
  }

  #commitRevisedChairRequest(request: IntakeRevisionRequest): TaskRequestCommit {
    if (request.chairRequest === undefined) throw new Error("revised chair request is absent");
    if (this.#requestCommitter === undefined) {
      throw new ProjectFabricCoreError("CAPABILITY_FORBIDDEN", "revised chair request requires the atomic request service");
    }
    const intake = row(this.#database.prepare(`
      SELECT chair_request_id FROM intakes WHERE intake_id=? AND coordination_run_id=?
    `).get(request.intakeId, request.coordinationRunId), "intake chair request binding");
    const priorRequestId = nullableText(intake, "chair_request_id");
    if (priorRequestId === null) {
      throw new ProjectFabricCoreError("CONFLICT", "bound intake has no persisted chair discussion");
    }
    const priorRequest = row(this.#database.prepare(`
      SELECT conversation_id FROM messages WHERE message_id=? AND run_id=?
    `).get(priorRequestId, request.coordinationRunId), "persisted intake chair request");
    if (text(priorRequest, "conversation_id") !== request.chairRequest.request.conversationId) {
      throw new ProjectFabricCoreError("CONFLICT", "revised chair request changed the intake discussion correlation");
    }
    const run = row(this.#database.prepare(`
      SELECT chair_agent_id FROM runs WHERE run_id=? AND project_session_id=?
    `).get(request.coordinationRunId, request.projectSessionId), "intake coordination run");
    if (text(run, "chair_agent_id") !== request.chairRequest.request.targetAgentId) {
      throw new ProjectFabricCoreError("TASK_NOT_OWNER", "revised discussion request does not target the current chair");
    }
    return this.#requestCommitter.commitTaskRequest(request.chairRequest);
  }

  #resolveArtifactBindings(
    projectId: string,
    projectSessionId: string | null,
    coordinationRunId: string | null,
    artifactRefs: readonly ArtifactRef[],
  ): Array<{ artifactId: string; path: string; digest: string }> {
    if (!this.#registryV1) {
      return artifactRefs.map((artifact) => ({ artifactId: "", path: artifact.path, digest: artifact.digest }));
    }
    return artifactRefs.map((artifact) => {
      const values = this.#database.prepare(`
        SELECT artifact_id, project_session_id, run_id, source_kind
          FROM artifacts
         WHERE project_id=? AND relative_path=? AND sha256=? AND registry_state='active'
      `).all(projectId, artifact.path, artifact.digest).map((value) => row(value, "intake artifact candidate"));
      const ranked = values.flatMap((candidate) => {
        const candidateSession = nullableText(candidate, "project_session_id");
        const candidateRun = nullableText(candidate, "run_id");
        const tier = coordinationRunId !== null && candidateRun === coordinationRunId && candidateSession === projectSessionId
          ? 0
          : candidateRun === null && projectSessionId !== null && candidateSession === projectSessionId
            ? 1
            : candidateRun === null && candidateSession === null
              ? 2
              : undefined;
        return tier === undefined ? [] : [{ candidate, tier }];
      }).sort((left, right) => left.tier - right.tier ||
        text(left.candidate, "artifact_id").localeCompare(text(right.candidate, "artifact_id")));
      const bestTier = ranked[0]?.tier;
      const best = bestTier === undefined ? [] : ranked.filter(({ tier }) => tier === bestTier);
      if (best.length > 1) {
        throw new ProjectFabricCoreError("CONFLICT", "artifact ref resolves to multiple registry rows at one scope tier");
      }
      const selected = best[0]?.candidate;
      if (selected !== undefined) {
        return { artifactId: text(selected, "artifact_id"), path: artifact.path, digest: artifact.digest };
      }
      throw new ProjectFabricCoreError(
        "NOT_FOUND",
        artifact.path.startsWith("private/git-diffs/")
          ? "private Git diff has no fixed-producer registry row"
          : "artifact ref has no exact active registry row",
      );
    });
  }

  #insertArtifactBinding(
    intakeId: string,
    revision: number,
    artifact: { artifactId: string; path: string; digest: string },
  ): void {
    if (this.#registryV1) {
      this.#database.prepare(`
        INSERT INTO intake_artifact_bindings(intake_id, intake_revision, artifact_id, relative_path, sha256)
        VALUES (?, ?, ?, ?, ?)
      `).run(intakeId, revision, artifact.artifactId, artifact.path, artifact.digest);
      return;
    }
    this.#database.prepare(`
      INSERT INTO intake_artifact_bindings(intake_id, intake_revision, relative_path, sha256)
      VALUES (?, ?, ?, ?)
    `).run(intakeId, revision, artifact.path, artifact.digest);
  }
}
