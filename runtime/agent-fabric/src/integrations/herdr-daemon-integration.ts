import type {
  AgentId,
  CoordinationRunId,
  JsonValue,
  ProjectId,
  ProjectSessionId,
  ProviderActionId,
} from "@local/agent-fabric-protocol";
import type Database from "better-sqlite3";

import {
  HerdrFabricPorts,
  HERDR_CONTROL_ADAPTER_ID,
  type DirectSteerIntent,
  type FabricSteerReference,
  type HerdrActionEvidence,
  type HerdrActionRecord,
  type HerdrRecoverySummary,
} from "./herdr-fabric-ports.js";
import { canonicalJson, integer, isRow, nullableText, text } from "../persistence/row-codec.js";
import { ProviderActionAdmissionCoordinator } from "../application/provider-action-admission.js";

export type HerdrDaemonRuntime = Readonly<{
  execute(actionId: ProviderActionId, intent: JsonValue): Promise<HerdrActionRecord>;
  lookupAction(actionId: ProviderActionId): Promise<HerdrActionEvidence>;
  reconcilePresence(identity: JsonValue): Promise<JsonValue>;
  restoreControlBinding?(intent: JsonValue): Promise<void>;
}>;

export type HerdrDaemonRuntimeFactoryInput = Readonly<{
  projectId: ProjectId;
  projectSessionId: ProjectSessionId;
  canonicalProjectRoot: string;
  fabricJournal: HerdrFabricPorts;
  fabricDirectSteer: HerdrFabricPorts;
}>;

export type HerdrDaemonIntegrationConfiguration =
  | Readonly<{ mode: "disabled" }>
  | Readonly<{
      mode: "enabled";
      createIntegration(input: HerdrDaemonRuntimeFactoryInput): Promise<HerdrDaemonRuntime>;
    }>;

export type HerdrDaemonActionRequest = Readonly<{
  actionId: ProviderActionId;
  projectId: ProjectId;
  projectSessionId: ProjectSessionId;
  coordinationRunId: CoordinationRunId;
  targetAgentId: AgentId | null;
  intent: JsonValue;
}>;

export type HerdrDaemonActionResult =
  | HerdrActionRecord
  | Readonly<{
      status: "unavailable";
      integration: "herdr-control-v1";
      reason: "disabled" | "unavailable";
    }>;

export type HerdrDirectSteerResult =
  | HerdrDaemonActionResult
  | Readonly<{
      status: "rejected";
      reason:
        | "unknown-reference"
        | "stale-reference"
        | "scope-mismatch"
        | "target-mismatch"
        | "answer-bearing-reference";
    }>;

export type HerdrDirectSteerRequest = Readonly<{
  actionId: ProviderActionId;
  fireAndForget: boolean;
  targetAgentId: AgentId;
  paneRef: string;
  reference: FabricSteerReference;
  prompt: string;
}>;

export type HerdrDaemonIntegrationOptions = Readonly<{
  database: Database.Database;
  providerActionAdmission: ProviderActionAdmissionCoordinator;
  configuration?: HerdrDaemonIntegrationConfiguration;
  clock?: () => number;
}>;

export type HerdrPresencePassResult = Readonly<{
  status: "completed" | "skipped-overlap";
  observed: number;
  degraded: number;
}>;

type HerdrPresenceRegistration = {
  projectId: ProjectId;
  projectSessionId: ProjectSessionId;
  coordinationRunId: CoordinationRunId;
  agentId: AgentId;
  intent: JsonValue;
};

/** One daemon-owned composition seam for all optional Herdr effects. */
export class HerdrDaemonIntegration {
  readonly #database: Database.Database;
  readonly #configuration: HerdrDaemonIntegrationConfiguration;
  readonly #ports: HerdrFabricPorts;
  readonly #runtimes = new Map<ProjectSessionId, Promise<HerdrDaemonRuntime>>();
  readonly #clock: () => number;
  #presencePassActive = false;

  constructor(options: HerdrDaemonIntegrationOptions) {
    this.#database = options.database;
    this.#configuration = options.configuration ?? { mode: "disabled" };
    this.#clock = options.clock ?? Date.now;
    this.#ports = new HerdrFabricPorts({
      database: options.database,
      providerActionAdmission: options.providerActionAdmission,
      ...(options.clock === undefined ? {} : { clock: options.clock }),
    });
    if (this.#configuration.mode === "enabled") {
      this.#recordControlIdentity("unavailable", "Herdr control is configured but not yet opened");
    }
  }

  async executeAction(request: HerdrDaemonActionRequest): Promise<HerdrDaemonActionResult> {
    if (isDirectSteerIntent(request.intent)) {
      throw new TypeError("direct Herdr steering must use the reference-validated fire-and-forget seam");
    }
    if (this.#configuration.mode === "disabled") {
      this.#ports.validateAction(request);
      return { status: "unavailable", integration: "herdr-control-v1", reason: "disabled" };
    }
    const prepared = this.#ports.prepareAction({
      actionId: request.actionId,
      projectId: request.projectId,
      projectSessionId: request.projectSessionId,
      coordinationRunId: request.coordinationRunId,
      targetAgentId: request.targetAgentId,
      intent: request.intent,
    });
    if (prepared.status === "terminal") return prepared;
    let runtime: HerdrDaemonRuntime;
    try {
      runtime = await this.#runtime(request.projectId, request.projectSessionId);
    } catch {
      return { status: "unavailable", integration: "herdr-control-v1", reason: "unavailable" };
    }
    const result = await runtime.execute(request.actionId, request.intent);
    this.#reconcileActionRecoveryStates();
    return result;
  }

  async executeDirectSteer(request: HerdrDirectSteerRequest): Promise<HerdrDirectSteerResult> {
    if (!request.fireAndForget) throw new TypeError("direct Herdr steering requires explicit fire-and-forget acknowledgement");
    if (!/^[A-Za-z0-9][A-Za-z0-9:._-]{0,127}$/u.test(request.paneRef)) {
      throw new TypeError("direct Herdr steering pane reference is invalid");
    }
    if (
      Buffer.byteLength(request.prompt, "utf8") < 1 || Buffer.byteLength(request.prompt, "utf8") > 4_096 ||
      /[\u0000-\u0008\u000b\u000c\u000e-\u001f\u007f\u009b]/u.test(request.prompt)
    ) throw new TypeError("direct Herdr steering prompt is unsafe or exceeds its bound");
    const validation = await this.#ports.validateSteerReference(request.reference);
    if (validation.status === "rejected") return { status: "rejected", reason: validation.code };
    if (validation.targetAgentId !== request.targetAgentId) {
      return { status: "rejected", reason: "target-mismatch" };
    }
    if (
      validation.purpose !== "steer" || validation.requiresAck || validation.expectsResult ||
      validation.dependentBarrierId !== null
    ) return { status: "rejected", reason: "answer-bearing-reference" };
    if (this.#configuration.mode === "disabled") {
      return { status: "unavailable", integration: "herdr-control-v1", reason: "disabled" };
    }
    const intent: DirectSteerIntent = {
      kind: "steer.inject-fire-and-forget",
      targetAgentId: request.targetAgentId,
      paneRef: request.paneRef,
      reference: request.reference,
      validatedReferenceDigest: validation.referenceDigest,
      prompt: request.prompt,
    };
    const prepared = await this.#ports.prepareDirectSteerAction(request.actionId, intent);
    if (prepared.status === "terminal") return prepared;
    let runtime: HerdrDaemonRuntime;
    try {
      runtime = await this.#runtime(request.reference.projectId, request.reference.projectSessionId);
    } catch {
      return { status: "unavailable", integration: "herdr-control-v1", reason: "unavailable" };
    }
    const result = await runtime.execute(request.actionId, intent);
    this.#reconcileActionRecoveryStates();
    return result;
  }

  async recover(): Promise<HerdrRecoverySummary> {
    const summary: { observed: number; terminal: number; ambiguous: number; prepared: number } = {
      observed: 0,
      terminal: 0,
      ambiguous: 0,
      prepared: 0,
    };
    if (this.#configuration.mode === "disabled") {
      const counts = this.#database.prepare(`
        SELECT status, COUNT(*) AS count FROM provider_actions
         WHERE adapter_id=? AND status IN ('prepared','dispatched','ambiguous')
         GROUP BY status
      `).all(HERDR_CONTROL_ADAPTER_ID);
      for (const count of counts) {
        if (!isRow(count)) continue;
        if (text(count, "status") === "prepared") summary.prepared += integer(count, "count");
        else summary.ambiguous += integer(count, "count");
      }
      this.#reconcileActionRecoveryStates();
      return summary;
    }
    const sessions = this.#database.prepare(`
      SELECT DISTINCT project.project_id, session.project_session_id
        FROM provider_actions action
        JOIN runs run ON run.run_id=action.run_id
        JOIN project_sessions session ON session.project_session_id=run.project_session_id
        JOIN projects project ON project.project_id=session.project_id
       WHERE action.adapter_id=? AND action.status IN ('prepared','dispatched','ambiguous')
       ORDER BY session.project_session_id
    `).all(HERDR_CONTROL_ADAPTER_ID);
    for (const value of sessions) {
      if (!isRow(value)) continue;
      try {
        const runtime = await this.#runtime(
          text(value, "project_id") as ProjectId,
          text(value, "project_session_id") as ProjectSessionId,
        );
        const recovered = await this.#ports.recover(runtime);
        summary.observed += recovered.observed;
        summary.terminal += recovered.terminal;
        summary.ambiguous += recovered.ambiguous;
        summary.prepared += recovered.prepared;
      } catch {
        const projectSessionId = text(value, "project_session_id");
        const counts = this.#database.prepare(`
          SELECT status, COUNT(*) AS count
            FROM provider_actions action
            JOIN runs run ON run.run_id=action.run_id
           WHERE action.adapter_id=? AND run.project_session_id=?
             AND action.status IN ('prepared','dispatched','ambiguous')
           GROUP BY status
        `).all(HERDR_CONTROL_ADAPTER_ID, projectSessionId);
        for (const count of counts) {
          if (!isRow(count)) continue;
          if (text(count, "status") === "prepared") summary.prepared += integer(count, "count");
          else summary.ambiguous += integer(count, "count");
        }
      }
    }
    this.#reconcileActionRecoveryStates();
    return summary;
  }

  async runPresencePass(): Promise<HerdrPresencePassResult> {
    if (this.#presencePassActive) {
      return { status: "skipped-overlap", observed: 0, degraded: 0 };
    }
    this.#presencePassActive = true;
    try {
      return await this.#performPresencePass();
    } finally {
      this.#presencePassActive = false;
    }
  }

  async #performPresencePass(): Promise<HerdrPresencePassResult> {
    const registrations = this.#presenceRegistrations();
    const priorPresence = this.#priorPresence();
    const presence: JsonValue[] = [];
    const degradedCurrent = new Set<string>();
    let hadRuntimeFailure = false;
    let degraded = 0;
    for (const registration of registrations) {
      let registrationRuntimeFailure = false;
      let parsed: ReturnType<typeof parsePresenceReconciliation>;
      try {
        const runtime = await this.#runtime(registration.projectId, registration.projectSessionId);
        parsed = parsePresenceReconciliation(await runtime.reconcilePresence(registration.intent));
      } catch {
        hadRuntimeFailure = true;
        registrationRuntimeFailure = true;
        parsed = {
          state: "unavailable",
          paneRef: null,
          readiness: "visibility-degraded",
        };
      }
      if (parsed.state !== "available") {
        degraded += 1;
        degradedCurrent.add(registration.coordinationRunId);
      }
      const previous = priorPresence.find((entry) =>
        entry.coordinationRunId === registration.coordinationRunId &&
        entry.agentId === registration.agentId
      );
      if (parsed.state === "unavailable" && registrationRuntimeFailure && previous?.state === "available") {
        presence.push(previous);
        continue;
      }
      const observation = {
        projectId: registration.projectId,
        projectSessionId: registration.projectSessionId,
        coordinationRunId: registration.coordinationRunId,
        agentId: registration.agentId,
        state: parsed.state,
        paneRef: parsed.paneRef,
        readiness: parsed.readiness,
      };
      const unchanged = typeof previous?.observedAt === "number" &&
        canonicalJson(previous) === canonicalJson({ ...observation, observedAt: previous.observedAt });
      presence.push(unchanged ? previous : { ...observation, observedAt: this.#clock() });
    }
    const state = this.#configuration.mode === "disabled"
      ? "unavailable"
      : hadRuntimeFailure && priorPresence.length > 0 ? "stale"
      : presence.length === 0 || (degraded > 0 && presence.length === degraded) ? "unavailable" : "available";
    this.#database.transaction(() => {
      const previouslyDegraded = this.#priorDegradedRunIds();
      const ownedDegraded = new Set(previouslyDegraded);
      const recoveredSessions = new Set<string>();
      for (const runId of new Set(registrations.map((registration) => registration.coordinationRunId))) {
        const run = this.#database.prepare(`
          SELECT lifecycle_state, project_session_id FROM runs WHERE run_id=?
        `).get(runId);
        if (!isRow(run)) continue;
        const projectSessionId = text(run, "project_session_id");
        if (degradedCurrent.has(runId)) {
          if (text(run, "lifecycle_state") === "active") {
            this.#database.prepare(`
              UPDATE runs SET lifecycle_state='visibility_degraded', revision=revision+1
               WHERE run_id=? AND lifecycle_state='active'
            `).run(runId);
            this.#database.prepare(`
              UPDATE project_sessions
                 SET state='visibility_degraded', revision=revision+1, updated_at=?
               WHERE project_session_id=? AND state='active'
            `).run(this.#clock(), projectSessionId);
            ownedDegraded.add(runId);
          } else if (text(run, "lifecycle_state") === "recovery_required") {
            ownedDegraded.add(runId);
          } else if (previouslyDegraded.has(runId)) {
            ownedDegraded.add(runId);
          }
          continue;
        }
        if (!previouslyDegraded.has(runId)) continue;
        this.#database.prepare(`
          UPDATE runs SET lifecycle_state='active', revision=revision+1
           WHERE run_id=? AND lifecycle_state='visibility_degraded'
        `).run(runId);
        ownedDegraded.delete(runId);
        recoveredSessions.add(projectSessionId);
      }
      for (const projectSessionId of recoveredSessions) {
        const stillDegraded = [...ownedDegraded].some((runId) => isRow(this.#database.prepare(`
          SELECT 1 FROM runs WHERE run_id=? AND project_session_id=?
        `).get(runId, projectSessionId)));
        if (stillDegraded) continue;
        this.#database.prepare(`
          UPDATE project_sessions
             SET state='active', revision=revision+1, updated_at=?
           WHERE project_session_id=? AND state='visibility_degraded'
        `).run(this.#clock(), projectSessionId);
      }
      const contractBody = {
        schemaVersion: 1,
        operationFamily: HERDR_CONTROL_ADAPTER_ID,
        mode: this.#configuration.mode,
        detail: state === "available"
          ? "Herdr control and presence available"
          : state === "stale" ? "Herdr presence is stale after integration loss" : "Herdr control or presence unavailable",
        presence,
        degradedRunIds: [...ownedDegraded].sort(),
        recoveryRunIds: [...this.#priorRecoveryRunIds()].sort(),
        recoverySessionIds: [...this.#priorRecoverySessionIds()].sort(),
      };
      const contract = {
        ...contractBody,
        generation: this.#availabilityGeneration(state, contractBody),
      };
      this.#database.prepare(`
        INSERT INTO integration_availability(
          integration_id, state, discovered_contract_json, checked_at
        ) VALUES (?, ?, ?, ?)
        ON CONFLICT(integration_id) DO UPDATE SET
          state=excluded.state,
          discovered_contract_json=excluded.discovered_contract_json,
          checked_at=excluded.checked_at
        WHERE integration_availability.state IS NOT excluded.state
           OR integration_availability.discovered_contract_json IS NOT excluded.discovered_contract_json
      `).run(HERDR_CONTROL_ADAPTER_ID, state, canonicalJson(contract), this.#clock());
    })();
    return { status: "completed", observed: presence.length, degraded };
  }

  #presenceRegistrations(): HerdrPresenceRegistration[] {
    const values = this.#database.prepare(`
      WITH ranked_registrations AS (
        SELECT action.run_id, action.target_agent_id, action.payload_json,
               run.project_session_id, project.project_id,
               agent.provider_session_ref,
               COALESCE(state.provider_session_generation, 1) AS provider_session_generation,
               binding.adapter_id,
               ROW_NUMBER() OVER (
                 PARTITION BY action.run_id, action.target_agent_id
                 ORDER BY action.updated_at DESC, action.action_id DESC
               ) AS registration_rank
          FROM provider_actions action
          JOIN runs run ON run.run_id=action.run_id
          JOIN project_sessions session ON session.project_session_id=run.project_session_id
          JOIN projects project ON project.project_id=session.project_id
          JOIN agents agent ON agent.run_id=action.run_id AND agent.agent_id=action.target_agent_id
          LEFT JOIN provider_state state ON state.run_id=agent.run_id AND state.agent_id=agent.agent_id
          LEFT JOIN agent_adapter_bindings binding ON binding.run_id=agent.run_id AND binding.agent_id=agent.agent_id
         WHERE action.adapter_id=? AND action.operation='herdr:agent.ensure-pane'
           AND action.status='terminal' AND action.target_agent_id IS NOT NULL
      )
      SELECT * FROM ranked_registrations
       WHERE registration_rank=1
       ORDER BY run_id, target_agent_id
       LIMIT 256
    `).all(HERDR_CONTROL_ADAPTER_ID);
    const seen = new Set<string>();
    const registrations: HerdrPresenceRegistration[] = [];
    for (const value of values) {
      if (!isRow(value)) continue;
      const runId = text(value, "run_id") as CoordinationRunId;
      const agentId = text(value, "target_agent_id") as AgentId;
      const key = `${runId}\0${agentId}`;
      if (seen.has(key)) continue;
      seen.add(key);
      let payload: unknown;
      try {
        payload = JSON.parse(text(value, "payload_json"));
      } catch {
        continue;
      }
      if (!isJsonObjectValue(payload) || payload.kind !== "agent.ensure-pane" || !isJsonObjectValue(payload.identity)) {
        continue;
      }
      const projectId = text(value, "project_id") as ProjectId;
      const projectSessionId = text(value, "project_session_id") as ProjectSessionId;
      const providerSessionRef = nullableText(value, "provider_session_ref");
      const provider = nullableText(value, "adapter_id");
      if (
        payload.identity.projectId !== projectId ||
        payload.identity.projectSessionId !== projectSessionId ||
        payload.identity.coordinationRunId !== runId ||
        payload.identity.agentId !== agentId ||
        providerSessionRef === null || payload.identity.providerSessionRef !== providerSessionRef ||
        provider === null || payload.identity.provider !== provider ||
        payload.identity.providerSessionGeneration !== integer(value, "provider_session_generation")
      ) continue;
      registrations.push({
        projectId,
        projectSessionId,
        coordinationRunId: runId,
        agentId,
        intent: payload,
      });
    }
    return registrations;
  }

  #availabilityGeneration(state: string, contractBody: Record<string, JsonValue>): number {
    const value = this.#database.prepare(`
      SELECT state, discovered_contract_json FROM integration_availability
       WHERE integration_id=?
    `).get(HERDR_CONTROL_ADAPTER_ID);
    if (!isRow(value)) return 1;
    try {
      const contract: unknown = JSON.parse(text(value, "discovered_contract_json"));
      if (isJsonObjectValue(contract) && Number.isSafeInteger(contract.generation) && Number(contract.generation) >= 1) {
        const generation = Number(contract.generation);
        if (
          text(value, "state") === state &&
          canonicalJson(contract) === canonicalJson({ ...contractBody, generation })
        ) return generation;
        return generation + 1;
      }
    } catch {
      // A malformed prior optional-integration row cannot confer a generation.
    }
    return 1;
  }

  #priorDegradedRunIds(): Set<string> {
    const value = this.#database.prepare(`
      SELECT discovered_contract_json FROM integration_availability
       WHERE integration_id=?
    `).get(HERDR_CONTROL_ADAPTER_ID);
    if (!isRow(value)) return new Set();
    try {
      const contract: unknown = JSON.parse(text(value, "discovered_contract_json"));
      if (
        !isJsonObjectValue(contract) || !Array.isArray(contract.degradedRunIds) ||
        contract.degradedRunIds.length > 256 ||
        !contract.degradedRunIds.every((runId) => typeof runId === "string")
      ) return new Set();
      return new Set(contract.degradedRunIds as string[]);
    } catch {
      return new Set();
    }
  }

  #priorRecoveryRunIds(): Set<string> {
    return this.#priorContractIdentifiers("recoveryRunIds");
  }

  #priorRecoverySessionIds(): Set<string> {
    return this.#priorContractIdentifiers("recoverySessionIds");
  }

  #priorContractIdentifiers(field: "recoveryRunIds" | "recoverySessionIds"): Set<string> {
    const value = this.#database.prepare(`
      SELECT discovered_contract_json FROM integration_availability
       WHERE integration_id=?
    `).get(HERDR_CONTROL_ADAPTER_ID);
    if (!isRow(value)) return new Set();
    try {
      const contract: unknown = JSON.parse(text(value, "discovered_contract_json"));
      if (
        !isJsonObjectValue(contract) || !Array.isArray(contract[field]) || contract[field].length > 256 ||
        !contract[field].every((identifier) => typeof identifier === "string")
      ) return new Set();
      return new Set(contract[field] as string[]);
    } catch {
      return new Set();
    }
  }

  #priorPresence(): Array<Record<string, JsonValue>> {
    const value = this.#database.prepare(`
      SELECT discovered_contract_json FROM integration_availability
       WHERE integration_id=?
    `).get(HERDR_CONTROL_ADAPTER_ID);
    if (!isRow(value)) return [];
    try {
      const contract: unknown = JSON.parse(text(value, "discovered_contract_json"));
      if (!isJsonObjectValue(contract) || !Array.isArray(contract.presence) || contract.presence.length > 256) {
        return [];
      }
      return contract.presence.filter(isJsonObjectValue);
    } catch {
      return [];
    }
  }

  async #runtime(projectId: ProjectId, projectSessionId: ProjectSessionId): Promise<HerdrDaemonRuntime> {
    if (this.#configuration.mode === "disabled") throw new TypeError("Herdr integration is disabled");
    const configuration = this.#configuration;
    const existing = this.#runtimes.get(projectSessionId);
    if (existing !== undefined) return await existing;
    const identity = this.#database.prepare(`
      SELECT project.project_id, project.canonical_root
        FROM project_sessions session
        JOIN projects project ON project.project_id=session.project_id
       WHERE session.project_session_id=?
    `).get(projectSessionId);
    if (!isRow(identity) || text(identity, "project_id") !== projectId) {
      throw new TypeError("Herdr runtime binding names another project session");
    }
    const created = (async () => {
      const runtime = await configuration.createIntegration({
        projectId,
        projectSessionId,
        canonicalProjectRoot: text(identity, "canonical_root"),
        fabricJournal: this.#ports,
        fabricDirectSteer: this.#ports,
      });
      this.#recordControlIdentity("available", "Herdr control available before the next presence observation");
      await this.#restoreControlBindings(projectSessionId, runtime);
      return runtime;
    })();
    this.#runtimes.set(projectSessionId, created);
    try {
      return await created;
    } catch (error: unknown) {
      this.#runtimes.delete(projectSessionId);
      throw error;
    }
  }

  #recordControlIdentity(
    state: "available" | "unavailable",
    detail: string,
  ): void {
    const existing = this.#database.prepare(`
      SELECT discovered_contract_json FROM integration_availability
       WHERE integration_id=?
    `).get(HERDR_CONTROL_ADAPTER_ID);
    let contract: Record<string, JsonValue> = {
      schemaVersion: 1,
      operationFamily: HERDR_CONTROL_ADAPTER_ID,
      mode: "enabled",
      detail,
      presence: [],
      degradedRunIds: [],
      recoveryRunIds: [],
      recoverySessionIds: [],
    };
    if (isRow(existing)) {
      try {
        const parsed: unknown = JSON.parse(text(existing, "discovered_contract_json"));
        if (isJsonObjectValue(parsed) && parsed.schemaVersion === 1 && parsed.operationFamily === HERDR_CONTROL_ADAPTER_ID) {
          contract = {
            ...parsed,
            mode: "enabled",
            detail,
          };
        }
      } catch {
        // Replace malformed optional-integration discovery with the closed control identity.
      }
    }
    this.#database.prepare(`
      INSERT INTO integration_availability(
        integration_id,state,discovered_contract_json,checked_at
      ) VALUES (?, ?, ?, ?)
      ON CONFLICT(integration_id) DO UPDATE SET
        state=excluded.state,
        discovered_contract_json=excluded.discovered_contract_json,
        checked_at=excluded.checked_at
      WHERE integration_availability.state IS NOT excluded.state
         OR integration_availability.discovered_contract_json IS NOT excluded.discovered_contract_json
    `).run(HERDR_CONTROL_ADAPTER_ID, state, canonicalJson(contract), this.#clock());
  }

  async #restoreControlBindings(
    projectSessionId: ProjectSessionId,
    runtime: HerdrDaemonRuntime,
  ): Promise<void> {
    if (runtime.restoreControlBinding === undefined) return;
    const values = this.#database.prepare(`
      WITH ranked_bindings AS (
        SELECT action.payload_json,
               ROW_NUMBER() OVER (
                 PARTITION BY action.operation, COALESCE(action.target_agent_id, '')
                 ORDER BY action.updated_at DESC, action.action_id DESC
               ) AS binding_rank
          FROM provider_actions action
          JOIN runs run ON run.run_id=action.run_id
         WHERE action.adapter_id=? AND run.project_session_id=? AND action.status='terminal'
           AND action.operation IN ('herdr:console.ensure-pane','herdr:agent.ensure-pane')
      )
      SELECT payload_json FROM ranked_bindings
       WHERE binding_rank=1
       ORDER BY payload_json
       LIMIT 256
    `).all(HERDR_CONTROL_ADAPTER_ID, projectSessionId);
    for (const value of values) {
      if (!isRow(value)) continue;
      try {
        const intent: unknown = JSON.parse(text(value, "payload_json"));
        if (!isJsonObjectValue(intent)) continue;
        await runtime.restoreControlBinding(intent);
      } catch {
        // One stale optional pane binding cannot disable unrelated Fabric paths.
      }
    }
  }

  #reconcileActionRecoveryStates(): void {
    const unresolved = new Set(this.#database.prepare(`
      SELECT DISTINCT action.run_id
        FROM provider_actions action
       WHERE action.adapter_id=? AND action.status IN ('dispatched','ambiguous')
       ORDER BY action.run_id
    `).all(HERDR_CONTROL_ADAPTER_ID).filter(isRow).map((value) => text(value, "run_id")));
    this.#database.transaction(() => {
      const ownedRuns = this.#priorRecoveryRunIds();
      const ownedSessions = this.#priorRecoverySessionIds();
      const degradedRuns = this.#priorDegradedRunIds();
      const touchedSessions = new Set<string>();
      for (const runId of unresolved) {
        const run = this.#database.prepare(`
          SELECT lifecycle_state, project_session_id FROM runs WHERE run_id=?
        `).get(runId);
        if (!isRow(run)) continue;
        const projectSessionId = text(run, "project_session_id");
        const runState = text(run, "lifecycle_state");
        if (runState === "active" || runState === "visibility_degraded") {
          this.#database.prepare(`
            UPDATE runs SET lifecycle_state='recovery_required', revision=revision+1
             WHERE run_id=? AND lifecycle_state=?
          `).run(runId, runState);
          ownedRuns.add(runId);
        }
        const session = this.#database.prepare(`
          SELECT state FROM project_sessions WHERE project_session_id=?
        `).get(projectSessionId);
        if (isRow(session) && (text(session, "state") === "active" || text(session, "state") === "visibility_degraded")) {
          const state = text(session, "state");
          this.#database.prepare(`
            UPDATE project_sessions
               SET state='recovery_required', revision=revision+1, updated_at=?
             WHERE project_session_id=? AND state=?
          `).run(this.#clock(), projectSessionId, state);
          ownedSessions.add(projectSessionId);
        }
      }
      for (const runId of [...ownedRuns]) {
        if (unresolved.has(runId)) continue;
        const run = this.#database.prepare(`
          SELECT lifecycle_state, project_session_id FROM runs WHERE run_id=?
        `).get(runId);
        ownedRuns.delete(runId);
        if (!isRow(run)) continue;
        const projectSessionId = text(run, "project_session_id");
        touchedSessions.add(projectSessionId);
        if (text(run, "lifecycle_state") !== "recovery_required") continue;
        this.#database.prepare(`
          UPDATE runs SET lifecycle_state=?, revision=revision+1
           WHERE run_id=? AND lifecycle_state='recovery_required'
        `).run(degradedRuns.has(runId) ? "visibility_degraded" : "active", runId);
      }
      for (const projectSessionId of touchedSessions) {
        if (!ownedSessions.has(projectSessionId)) continue;
        const stillOwned = [...ownedRuns].some((runId) => isRow(this.#database.prepare(`
          SELECT 1 FROM runs WHERE run_id=? AND project_session_id=?
        `).get(runId, projectSessionId)));
        if (stillOwned) continue;
        const hasDegraded = [...degradedRuns].some((runId) => isRow(this.#database.prepare(`
          SELECT 1 FROM runs WHERE run_id=? AND project_session_id=?
        `).get(runId, projectSessionId)));
        this.#database.prepare(`
          UPDATE project_sessions SET state=?, revision=revision+1, updated_at=?
           WHERE project_session_id=? AND state='recovery_required'
        `).run(hasDegraded ? "visibility_degraded" : "active", this.#clock(), projectSessionId);
        ownedSessions.delete(projectSessionId);
      }
      this.#persistRecoveryOwnership(ownedRuns, ownedSessions);
    })();
  }

  #persistRecoveryOwnership(ownedRuns: Set<string>, ownedSessions: Set<string>): void {
    const existing = this.#database.prepare(`
      SELECT state, discovered_contract_json, checked_at FROM integration_availability
       WHERE integration_id=?
    `).get(HERDR_CONTROL_ADAPTER_ID);
    if (!isRow(existing) && ownedRuns.size === 0 && ownedSessions.size === 0) return;
    let state = "unavailable";
    let checkedAt = this.#clock();
    let priorContractJson: string | null = null;
    let contract: Record<string, JsonValue> = {
      schemaVersion: 1,
      generation: 1,
      operationFamily: HERDR_CONTROL_ADAPTER_ID,
      mode: this.#configuration.mode,
      detail: "Herdr action recovery state recorded before presence observation",
      presence: [],
      degradedRunIds: [],
    };
    if (isRow(existing)) {
      state = text(existing, "state");
      checkedAt = integer(existing, "checked_at");
      priorContractJson = text(existing, "discovered_contract_json");
      try {
        const parsed: unknown = JSON.parse(priorContractJson);
        if (isJsonObjectValue(parsed) && parsed.schemaVersion === 1 && parsed.operationFamily === HERDR_CONTROL_ADAPTER_ID) {
          contract = parsed;
        }
      } catch {
        // Replace a malformed optional-integration contract with a closed one.
      }
    }
    contract.recoveryRunIds = [...ownedRuns].sort();
    contract.recoverySessionIds = [...ownedSessions].sort();
    const contractJson = canonicalJson(contract);
    if (priorContractJson === contractJson) return;
    this.#database.prepare(`
      INSERT INTO integration_availability(
        integration_id, state, discovered_contract_json, checked_at
      ) VALUES (?, ?, ?, ?)
      ON CONFLICT(integration_id) DO UPDATE SET
        state=excluded.state,
        discovered_contract_json=excluded.discovered_contract_json,
        checked_at=excluded.checked_at
      WHERE integration_availability.state IS NOT excluded.state
         OR integration_availability.discovered_contract_json IS NOT excluded.discovered_contract_json
    `).run(HERDR_CONTROL_ADAPTER_ID, state, contractJson, checkedAt);
  }
}

function isDirectSteerIntent(value: JsonValue): value is DirectSteerIntent {
  return isJsonObject(value) && value.kind === "steer.inject-fire-and-forget";
}

function isJsonObject(value: JsonValue): value is { readonly [key: string]: JsonValue } {
  return value !== null && typeof value === "object" && !Array.isArray(value);
}

function isJsonObjectValue(value: unknown): value is Record<string, JsonValue> {
  return value !== null && typeof value === "object" && !Array.isArray(value);
}

function parsePresenceReconciliation(value: JsonValue): {
  state: "available" | "unavailable";
  paneRef: string | null;
  readiness: string;
} {
  if (
    !isJsonObject(value) || typeof value.readiness !== "string" || typeof value.ready !== "boolean" ||
    !["ready", "identity-unverified", "identity-conflict", "visibility-degraded"].includes(value.readiness) ||
    Buffer.byteLength(value.readiness, "utf8") > 64
  ) {
    throw new TypeError("Herdr presence reconciliation is malformed");
  }
  const paneRef = value.paneRef;
  if (paneRef !== null && (typeof paneRef !== "string" || !/^[A-Za-z0-9][A-Za-z0-9:._-]{0,127}$/u.test(paneRef))) {
    throw new TypeError("Herdr presence pane reference is invalid");
  }
  if (
    (value.readiness === "ready" && (!value.ready || paneRef === null)) ||
    (value.readiness !== "ready" && value.ready) ||
    (value.readiness === "visibility-degraded" && paneRef !== null) ||
    ((value.readiness === "identity-unverified" || value.readiness === "identity-conflict") && paneRef === null)
  ) throw new TypeError("Herdr presence readiness evidence is inconsistent");
  return {
    state: value.readiness === "ready" || value.readiness === "identity-unverified" ? "available" : "unavailable",
    paneRef,
    readiness: value.readiness,
  };
}
