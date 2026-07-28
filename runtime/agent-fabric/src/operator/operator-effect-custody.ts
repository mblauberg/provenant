import type {
  ArtifactRef,
  OperatorActionIntent,
  Sha256Digest,
} from "@local/agent-fabric-protocol";
import { parseArtifactRef, parseSha256Digest } from "@local/agent-fabric-protocol";
import type Database from "better-sqlite3";

import type {
  OperatorEffectOutcome,
  OperatorEffectRequest,
} from "./action-store.js";
import type {
  ExternalEffectDispatchHandle,
} from "./external-effect-service.js";
import { ProjectFabricCoreError } from "../project-session/contracts.js";
import { canonicalJson, sha256 } from "../persistence/row-codec.js";
import { ProviderActionOwnerError } from "../application/provider-action-owner.js";
import type {
  TypedGitAdministrativeRequest,
  TypedGitEffectRequest,
} from "./typed-git-service.js";

export type Row = Record<string, unknown>;

export type EffectScope = {
  operatorId: string;
  projectId: string;
  projectSessionId: string;
  principalGeneration: number;
  operation: string;
};

function isRow(value: unknown): value is Row {
  return typeof value === "object" && value !== null && !Array.isArray(value);
}

function row(value: unknown, label: string): Row {
  if (!isRow(value)) throw new ProjectFabricCoreError("NOT_FOUND", `${label} was not found`);
  return value;
}

function text(value: Row, field: string): string {
  const candidate = value[field];
  if (typeof candidate !== "string") throw new Error(`${field} is not text`);
  return candidate;
}

function integer(value: Row, field: string): number {
  const candidate = value[field];
  if (typeof candidate !== "number" || !Number.isSafeInteger(candidate)) throw new Error(`${field} is not an integer`);
  return candidate;
}

function nullableText(value: Row, field: string): string | null {
  const candidate = value[field];
  if (candidate !== null && typeof candidate !== "string") throw new Error(`${field} is not nullable text`);
  return candidate;
}

function unsupported(): never {
  throw new ProjectFabricCoreError("CAPABILITY_FORBIDDEN", "operator action runtime is unavailable for this intent");
}

function isTypedGitAdministration(
  intent: OperatorActionIntent,
): intent is Extract<OperatorActionIntent, { kind: "git-authorise" | "git-operation-draft" | "git-custody-resolve" }> {
  return intent.kind === "git-authorise" || intent.kind === "git-operation-draft" || intent.kind === "git-custody-resolve";
}

function digestValue(value: unknown, path: string): Sha256Digest {
  return parseSha256Digest(`sha256:${sha256(canonicalJson(value))}`, path);
}

/**
 * Closed Git owned-effect surface used by shared custody: the exact subset of `TypedGitService`
 * invoked while preparing, dispatching, or observing a custody row. Read-only lookups
 * (`status`, `reconcileConflict`, `readCurrentState`, `readAdministrativeCurrentState`) stay on
 * the facade's state router and never appear here.
 */
export type OperatorEffectCustodyGitPort = {
  prepare(request: TypedGitEffectRequest): void;
  dispatch(request: TypedGitEffectRequest): Promise<OperatorEffectOutcome>;
  observe(request: TypedGitEffectRequest): Promise<OperatorEffectOutcome>;
  prepareAdministrative(request: TypedGitAdministrativeRequest): void;
  administrativeOutcome(
    intent: Extract<OperatorActionIntent, { kind: "git-authorise" | "git-operation-draft" | "git-custody-resolve" }>,
  ): OperatorEffectOutcome;
};

/** Closed external-effect owned-effect surface used by shared custody prepare/dispatch/observe. */
export type OperatorEffectCustodyExternalPort = {
  prepareInTransaction(request: OperatorEffectRequest): ExternalEffectDispatchHandle | undefined;
  dispatchPrepared(handle: ExternalEffectDispatchHandle): Promise<OperatorEffectOutcome>;
  observe(request: OperatorEffectRequest & { effectRef: ArtifactRef | null }): Promise<OperatorEffectOutcome>;
};

/** Closed project/daemon lifecycle owned-effect surface used only by `#dispatchOwned`/`#observeOwned`. */
export type OperatorEffectCustodyLifecyclePort = {
  drainProject(
    request: OperatorEffectRequest & { intent: Extract<OperatorActionIntent, { kind: "project-session-drain" }> },
  ): OperatorEffectOutcome;
  stopProject(
    request: OperatorEffectRequest & { intent: Extract<OperatorActionIntent, { kind: "project-session-stop" }> },
  ): OperatorEffectOutcome;
  drainDaemon(
    request: OperatorEffectRequest & { intent: Extract<OperatorActionIntent, { kind: "daemon-drain" }> },
  ): OperatorEffectOutcome;
  stopDaemon(
    request: OperatorEffectRequest & { intent: Extract<OperatorActionIntent, { kind: "daemon-stop" }> },
  ): Promise<OperatorEffectOutcome>;
  observeProjectDrain(
    request: OperatorEffectRequest & { intent: Extract<OperatorActionIntent, { kind: "project-session-drain" }> },
  ): OperatorEffectOutcome;
  observeProjectStop(
    request: OperatorEffectRequest & { intent: Extract<OperatorActionIntent, { kind: "project-session-stop" }> },
  ): OperatorEffectOutcome;
  observeDaemonDrain(
    commandId: string,
    daemonInstanceGeneration: number,
    request: OperatorEffectRequest,
  ): OperatorEffectOutcome;
  observeDaemonStop(
    request: OperatorEffectRequest & { intent: Extract<OperatorActionIntent, { kind: "daemon-stop" }> },
  ): OperatorEffectOutcome;
};

/** Closed operator-control owned-effect surface used only by `#dispatchOwned`/`#observeOwned`/`#dispatch`. */
export type OperatorEffectCustodyControlPort = {
  assertPersistedControlActionOwners(request: OperatorEffectRequest): void;
  dispatchControl(
    request: OperatorEffectRequest & { intent: Extract<OperatorActionIntent, { kind: "control" }> },
  ): Promise<OperatorEffectOutcome>;
  observeControl(
    request: OperatorEffectRequest & {
      effectRef: ArtifactRef | null;
      intent: Extract<OperatorActionIntent, { kind: "control" }>;
    },
  ): Promise<OperatorEffectOutcome>;
};

export interface OperatorEffectCustodyHostPort {
  /** The facade's state router (`#read`); the shared custody prepare/dispatch/observe fences read through it. */
  read(intent: OperatorActionIntent): Promise<unknown>;
}

/**
 * Shared operator-effect custody (S4h): the generic scope/principal, stable identity, atomic
 * prepare, dispatch claim/I/O/settlement, and observe/no-effect fences that every owned-effect
 * family shares, plus the closed routing into those families. Byte-for-byte moved out of
 * `production-action-ports.ts` behind explicit control/lifecycle/Git/external ports; the facade
 * keeps the state router (`#read`) and the thin `effectPort.status`/`reconcileGit` pass-throughs.
 */
export class OperatorEffectCustodyActions {
  readonly #database: Database.Database;
  readonly #clock: () => number;
  readonly #fault: (label: string) => void;
  readonly #host: OperatorEffectCustodyHostPort;
  readonly #git: OperatorEffectCustodyGitPort | undefined;
  readonly #external: OperatorEffectCustodyExternalPort | undefined;
  readonly #lifecycle: OperatorEffectCustodyLifecyclePort;
  readonly #control: OperatorEffectCustodyControlPort;
  readonly #externalEffectHandles = new Map<string, ExternalEffectDispatchHandle>();

  constructor(options: {
    database: Database.Database;
    clock: () => number;
    host: OperatorEffectCustodyHostPort;
    lifecycle: OperatorEffectCustodyLifecyclePort;
    control: OperatorEffectCustodyControlPort;
    git?: OperatorEffectCustodyGitPort;
    external?: OperatorEffectCustodyExternalPort;
    fault?: (label: string) => void;
  }) {
    this.#database = options.database;
    this.#clock = options.clock;
    this.#host = options.host;
    this.#lifecycle = options.lifecycle;
    this.#control = options.control;
    this.#git = options.git;
    this.#external = options.external;
    this.#fault = options.fault ?? (() => undefined);
  }

  effectScope(request: OperatorEffectRequest): EffectScope {
    let projectSessionId = request.projectSessionId;
    if (projectSessionId === undefined) {
      if (request.intent.kind === "control") projectSessionId = request.intent.target.projectSessionId;
      else if (request.intent.kind === "project-session-drain" || request.intent.kind === "project-session-stop") {
        projectSessionId = request.intent.projectSessionId;
      } else {
        const fallback = row(this.#database.prepare(`
          SELECT project_session_id FROM project_sessions ORDER BY project_session_id LIMIT 1
        `).get(), "operator effect authority session");
        projectSessionId = text(fallback, "project_session_id");
      }
    }
    const session = row(this.#database.prepare(`
      SELECT session.project_id, project.authority_generation
        FROM project_sessions session
        JOIN projects project ON project.project_id=session.project_id
       WHERE session.project_session_id=?
    `).get(projectSessionId), "operator effect authority session");
    const projectId = request.projectId ?? text(session, "project_id");
    if (projectId !== text(session, "project_id")) {
      throw new ProjectFabricCoreError("WRONG_PROJECT", "operator effect scope is bound to another project");
    }
    const operatorId = request.operatorId ?? "operator_direct_test";
    const livePrincipal = this.#database.prepare(`
      SELECT project_authority_generation, principal_generation, state
        FROM operator_principals
       WHERE operator_id=? AND project_id=?
    `).get(operatorId, projectId);
    let principalGeneration = request.principalGeneration;
    if (isRow(livePrincipal)) {
      if (text(livePrincipal, "state") !== "active") {
        throw new ProjectFabricCoreError("CAPABILITY_FORBIDDEN", "operator principal is not active");
      }
      if (integer(livePrincipal, "project_authority_generation") !== integer(session, "authority_generation")) {
        throw new ProjectFabricCoreError("STALE_GENERATION", "operator project authority generation changed");
      }
      const liveGeneration = integer(livePrincipal, "principal_generation");
      if (principalGeneration !== undefined && principalGeneration !== liveGeneration) {
        throw new ProjectFabricCoreError("STALE_PRINCIPAL_GENERATION", "operator principal generation changed");
      }
      principalGeneration = liveGeneration;
    }
    if (typeof principalGeneration !== "number" || !Number.isSafeInteger(principalGeneration) || principalGeneration < 1) {
      throw new ProjectFabricCoreError("CAPABILITY_FORBIDDEN", "operator effect has no live principal generation");
    }
    return {
      operatorId,
      projectId,
      projectSessionId,
      principalGeneration,
      operation: request.operation ?? (request.intent.kind === "control" ? request.intent.action : request.intent.kind),
    };
  }

  custodyId(scope: EffectScope, commandId: string): string {
    return `operator-effect-${sha256(canonicalJson({
      operatorId: scope.operatorId,
      projectId: scope.projectId,
      projectSessionId: scope.projectSessionId,
      principalGeneration: scope.principalGeneration,
      operation: scope.operation,
      commandId,
    })).slice(0, 48)}`;
  }

  effectCustody(scope: EffectScope, commandId: string): Row | null {
    const value = this.#database.prepare(`
      SELECT * FROM operator_effect_custody
       WHERE operator_id=? AND project_id=? AND project_session_id=? AND command_id=?
    `).get(scope.operatorId, scope.projectId, scope.projectSessionId, commandId);
    return isRow(value) ? value : null;
  }

  #assertCustodyIdentity(custody: Row, scope: EffectScope, request: OperatorEffectRequest): void {
    if (
      text(custody, "custody_id") !== this.custodyId(scope, request.commandId) ||
      integer(custody, "principal_generation") !== scope.principalGeneration ||
      text(custody, "operation") !== scope.operation ||
      text(custody, "intent_digest") !== request.intentDigest ||
      (request.operatorId !== undefined && text(custody, "before_state_digest") !== request.beforeStateDigest) ||
      text(custody, "intent_json") !== canonicalJson(request.intent)
    ) {
      throw new ProjectFabricCoreError("DEDUPE_CONFLICT", "operator effect custody identity changed");
    }
  }

  prepareEffect(request: OperatorEffectRequest): void {
    if (isTypedGitAdministration(request.intent)) {
      if (this.#git === undefined) unsupported();
      this.#git.prepareAdministrative(this.#typedGitAdministrativeRequest(request));
      return;
    }
    const scope = this.effectScope(request);
    const custodyId = this.custodyId(scope, request.commandId);
    const intentJson = canonicalJson(request.intent);
    const external = request.intent.kind === "registered-external-effect" || request.intent.kind === "promotion";
    const git = request.intent.kind === "git";
    if (external && this.#external === undefined) unsupported();
    if (git && this.#git === undefined) unsupported();
    const externalHandle = this.#database.transaction((): ExternalEffectDispatchHandle | undefined => {
      const existing = this.effectCustody(scope, request.commandId);
      if (existing !== null) {
        this.#assertCustodyIdentity(existing, scope, request);
        if (git) this.#git?.prepare(this.#typedGitRequest(request, scope));
        return external ? this.#external?.prepareInTransaction(request) : undefined;
      }
      this.#database.prepare(`
        INSERT INTO operator_effect_custody(
          custody_id, operator_id, project_id, project_session_id, principal_generation, command_id,
          operation, intent_digest, before_state_digest, intent_json, state,
          effect_path, effect_digest, outcome_json, created_at, updated_at
        ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, 'prepared', NULL, NULL, NULL, ?, ?)
      `).run(
        custodyId,
        scope.operatorId,
        scope.projectId,
        scope.projectSessionId,
        scope.principalGeneration,
        request.commandId,
        scope.operation,
        request.intentDigest,
        request.beforeStateDigest,
        intentJson,
        this.#clock(),
        this.#clock(),
      );
      const preparedExternal = external
        ? this.#external?.prepareInTransaction(request)
        : undefined;
      if (git) this.#git?.prepare(this.#typedGitRequest(request, scope));
      if (request.intent.kind === "daemon-stop") {
        const correlationDigest = `sha256:${sha256(canonicalJson({
          custodyId,
          operatorId: scope.operatorId,
          projectId: scope.projectId,
          projectSessionId: scope.projectSessionId,
          principalGeneration: scope.principalGeneration,
          commandId: request.commandId,
          operation: scope.operation,
          intentDigest: request.intentDigest,
        }))}`;
        try {
          this.#database.prepare(`
            INSERT INTO operator_daemon_stop_custody(
              daemon_instance_generation, observed_global_revision, custody_id,
              operator_id, project_id, project_session_id, principal_generation, command_id, operation,
              result_correlation_digest, state, result_json, updated_at
            ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, 'daemon-stop', ?, 'prepared', NULL, ?)
          `).run(
            request.intent.expectedDaemonGeneration,
            request.intent.expectedGlobalStateRevision,
            custodyId,
            scope.operatorId,
            scope.projectId,
            scope.projectSessionId,
            scope.principalGeneration,
            request.commandId,
            correlationDigest,
            this.#clock(),
          );
        } catch (error: unknown) {
          if (error instanceof Error && /UNIQUE constraint failed/u.test(error.message)) {
            throw new ProjectFabricCoreError("CONFLICT", "another operator command owns daemon stop custody");
          }
          throw error;
        }
      }
      return preparedExternal;
    })();
    if (externalHandle !== undefined) this.#externalEffectHandles.set(custodyId, externalHandle);
  }

  custodyEffectRef(custody: Row): ArtifactRef {
    return parseArtifactRef({
      path: `.agent-fabric/operator-effects/${text(custody, "custody_id")}.json`,
      digest: `sha256:${sha256(canonicalJson({
        custodyId: text(custody, "custody_id"),
        intentDigest: text(custody, "intent_digest"),
        beforeStateDigest: text(custody, "before_state_digest"),
      }))}`,
    }, "productionOperatorAction.custodyEffectRef");
  }

  #storedCustodyOutcome(custody: Row): OperatorEffectOutcome | null {
    const serialized = nullableText(custody, "outcome_json");
    if (serialized === null) return null;
    const value: unknown = JSON.parse(serialized);
    if (!isRow(value) || typeof value.status !== "string") throw new Error("operator effect custody outcome is invalid");
    return value as OperatorEffectOutcome;
  }

  storeCustodyOutcome(scope: EffectScope, commandId: string, outcome: OperatorEffectOutcome): void {
    const state = outcome.status === "committed"
      ? "terminal"
      : outcome.status === "rejected"
        ? "rejected"
        : outcome.status === "ambiguous"
          ? "ambiguous"
          : "dispatching";
    const effectRef = outcome.status === "ambiguous" || (outcome.status === "committed" && outcome.effectRef !== undefined)
      ? outcome.effectRef
      : null;
    this.#database.prepare(`
      UPDATE operator_effect_custody
         SET state=?, effect_path=?, effect_digest=?, outcome_json=?, updated_at=?
       WHERE operator_id=? AND project_id=? AND project_session_id=? AND command_id=?
    `).run(
      state,
      effectRef?.path ?? null,
      effectRef?.digest ?? null,
      canonicalJson(outcome),
      this.#clock(),
      scope.operatorId,
      scope.projectId,
      scope.projectSessionId,
      commandId,
    );
  }

  async dispatch(request: OperatorEffectRequest): Promise<OperatorEffectOutcome> {
    if (isTypedGitAdministration(request.intent)) {
      if (this.#git === undefined) unsupported();
      return this.#git.administrativeOutcome(request.intent);
    }
    if (request.intent.kind === "git") {
      if (this.#git === undefined) unsupported();
      const scope = this.effectScope(request);
      if (this.effectCustody(scope, request.commandId) === null) this.prepareEffect(request);
      return await this.#git.dispatch(this.#typedGitRequest(request, scope));
    }
    let effective = request;
    let scope = this.effectScope(effective);
    let custody = this.effectCustody(scope, effective.commandId);
    if (custody === null) {
      if (effective.operatorId === undefined) {
        const current = await this.#host.read(effective.intent);
        effective = { ...effective, beforeStateDigest: digestValue(current, "operatorEffect.directBeforeState") };
        scope = this.effectScope(effective);
      }
      this.prepareEffect(effective);
      custody = row(this.effectCustody(scope, effective.commandId), "operator effect custody");
    }
    this.#assertCustodyIdentity(custody, scope, effective);
    const existingOutcome = this.#storedCustodyOutcome(custody);
    const custodyState = text(custody, "state");
    if (["terminal", "rejected", "no-effect"].includes(custodyState) && existingOutcome !== null) return existingOutcome;
    if (custodyState !== "prepared") {
      return existingOutcome ?? { status: "ambiguous", effectRef: this.custodyEffectRef(custody) };
    }
    if (effective.intent.kind === "control") this.#control.assertPersistedControlActionOwners(effective);
    const current = await this.#host.read(effective.intent);
    if (effective.intent.kind === "control") this.#control.assertPersistedControlActionOwners(effective);
    const currentDigest = digestValue(current, "operatorEffect.currentState");
    if (currentDigest !== text(custody, "before_state_digest")) {
      const rejected: OperatorEffectOutcome = { status: "rejected", code: "state-changed", evidenceRefs: [] };
      this.#database.prepare(`
        UPDATE operator_effect_custody SET state='no-effect', outcome_json=?, updated_at=?
         WHERE custody_id=? AND state='prepared'
      `).run(canonicalJson(rejected), this.#clock(), text(custody, "custody_id"));
      return rejected;
    }
    const claimed = this.#database.prepare(`
      UPDATE operator_effect_custody SET state='dispatching', updated_at=?
       WHERE custody_id=? AND state='prepared'
    `).run(this.#clock(), text(custody, "custody_id"));
    if (claimed.changes !== 1) {
      const raced = row(this.effectCustody(scope, effective.commandId), "operator effect custody");
      return this.#storedCustodyOutcome(raced) ?? { status: "ambiguous", effectRef: this.custodyEffectRef(raced) };
    }
    try {
      const outcome = await this.#dispatchOwned(effective);
      this.#fault("operator-effect:after-owned-dispatch");
      this.storeCustodyOutcome(scope, effective.commandId, outcome);
      return outcome;
    } catch (error: unknown) {
      if (error instanceof ProviderActionOwnerError) throw error;
      if (effective.intent.kind === "project-session-drain" || effective.intent.kind === "project-session-stop") {
        const rejected: OperatorEffectOutcome = { status: "rejected", code: "state-changed", evidenceRefs: [] };
        this.#database.prepare(`
          UPDATE operator_effect_custody SET state='no-effect',outcome_json=?,updated_at=?
           WHERE custody_id=? AND state='dispatching'
        `).run(canonicalJson(rejected), this.#clock(), text(custody, "custody_id"));
      } else {
        this.#database.prepare(`
          UPDATE operator_effect_custody SET state='failed', updated_at=?
           WHERE custody_id=? AND state='dispatching'
        `).run(this.#clock(), text(custody, "custody_id"));
      }
      throw error;
    }
  }

  async observe(
    request: OperatorEffectRequest & { effectRef: ArtifactRef | null },
  ): Promise<OperatorEffectOutcome> {
    if (isTypedGitAdministration(request.intent)) {
      if (this.#git === undefined) unsupported();
      return this.#git.administrativeOutcome(request.intent);
    }
    if (request.intent.kind === "git") {
      if (this.#git === undefined) unsupported();
      return await this.#git.observe(this.#typedGitRequest(request, this.effectScope(request)));
    }
    const scope = this.effectScope(request);
    const custody = this.effectCustody(scope, request.commandId);
    if (custody === null) return { status: "rejected", code: "state-changed", evidenceRefs: [] };
    this.#assertCustodyIdentity(custody, scope, request);
    const state = text(custody, "state");
    const existing = this.#storedCustodyOutcome(custody);
    if (state === "prepared") {
      const rejected: OperatorEffectOutcome = { status: "rejected", code: "state-changed", evidenceRefs: [] };
      this.#database.transaction(() => {
        this.#database.prepare(`
          UPDATE operator_effect_custody SET state='no-effect', outcome_json=?, updated_at=?
           WHERE custody_id=? AND state='prepared'
        `).run(canonicalJson(rejected), this.#clock(), text(custody, "custody_id"));
        if (request.intent.kind === "daemon-stop") {
          this.#database.prepare(`
            UPDATE operator_daemon_stop_custody SET state='no-effect', result_json=?, updated_at=?
             WHERE custody_id=? AND state='prepared'
          `).run(
            canonicalJson({ provedNoEffect: true, reason: "dispatch-not-started" }),
            this.#clock(),
            text(custody, "custody_id"),
          );
        }
      })();
      return rejected;
    }
    if (["terminal", "rejected", "no-effect"].includes(state) && existing !== null) return existing;
    const outcome = await this.#observeOwned(request);
    this.storeCustodyOutcome(scope, request.commandId, outcome);
    return outcome;
  }

  async #dispatchOwned(request: OperatorEffectRequest): Promise<OperatorEffectOutcome> {
    if (request.intent.kind === "registered-external-effect" || request.intent.kind === "promotion") {
      if (this.#external === undefined) unsupported();
      const custodyId = this.custodyId(this.effectScope(request), request.commandId);
      const handle = this.#externalEffectHandles.get(custodyId);
      if (handle === undefined) {
        throw new ProjectFabricCoreError("RECOVERY_REQUIRED", "external-effect dispatch handle is unavailable");
      }
      this.#externalEffectHandles.delete(custodyId);
      return await this.#external.dispatchPrepared(handle);
    }
    if (request.intent.kind === "project-session-drain") {
      return this.#lifecycle.drainProject({ ...request, intent: request.intent });
    }
    if (request.intent.kind === "project-session-stop") {
      return this.#lifecycle.stopProject({ ...request, intent: request.intent });
    }
    if (request.intent.kind === "daemon-drain") {
      return this.#lifecycle.drainDaemon({ ...request, intent: request.intent });
    }
    if (request.intent.kind === "daemon-stop") {
      return await this.#lifecycle.stopDaemon({ ...request, intent: request.intent });
    }
    if (request.intent.kind !== "control") unsupported();
    return await this.#control.dispatchControl({ ...request, intent: request.intent });
  }

  #typedGitRequest(request: OperatorEffectRequest, scope: EffectScope): TypedGitEffectRequest {
    if (request.intent.kind !== "git") throw new TypeError("typed Git request requires a Git intent");
    return {
      commandId: request.commandId,
      previewId: request.previewId ?? request.commandId,
      operatorId: scope.operatorId,
      projectId: scope.projectId,
      projectSessionId: scope.projectSessionId,
      principalGeneration: scope.principalGeneration,
      operation: scope.operation,
      intent: request.intent,
      intentDigest: request.intentDigest,
      beforeStateDigest: request.beforeStateDigest,
      attemptGeneration: request.attemptGeneration,
    };
  }

  #typedGitAdministrativeRequest(request: OperatorEffectRequest): TypedGitAdministrativeRequest {
    if (!isTypedGitAdministration(request.intent)) throw new TypeError("typed Git administration requires its closed intent");
    if (request.operatorInputRecordDigest === undefined) {
      throw new ProjectFabricCoreError(
        "CAPABILITY_FORBIDDEN",
        "typed Git administration requires the exact independently attested operator input record",
      );
    }
    const scope = this.effectScope(request);
    return {
      commandId: request.commandId,
      previewId: request.previewId ?? request.commandId,
      operatorId: scope.operatorId,
      projectId: scope.projectId,
      projectSessionId: scope.projectSessionId,
      principalGeneration: scope.principalGeneration,
      operation: scope.operation,
      intent: request.intent,
      intentDigest: request.intentDigest,
      beforeStateDigest: request.beforeStateDigest,
      attemptGeneration: request.attemptGeneration,
      operatorInputRecordDigest: request.operatorInputRecordDigest,
    };
  }

  async #observeOwned(
    request: OperatorEffectRequest & { effectRef: ArtifactRef | null },
  ): Promise<OperatorEffectOutcome> {
    if (request.intent.kind === "registered-external-effect" || request.intent.kind === "promotion") {
      if (this.#external === undefined) unsupported();
      return await this.#external.observe(request);
    }
    if (request.intent.kind === "project-session-drain") {
      return this.#lifecycle.observeProjectDrain({ ...request, intent: request.intent });
    }
    if (request.intent.kind === "project-session-stop") {
      return this.#lifecycle.observeProjectStop({ ...request, intent: request.intent });
    }
    if (request.intent.kind === "daemon-drain") {
      return this.#lifecycle.observeDaemonDrain(
        request.commandId,
        request.intent.expectedDaemonGeneration,
        request,
      );
    }
    if (request.intent.kind === "daemon-stop") {
      return this.#lifecycle.observeDaemonStop({ ...request, intent: request.intent });
    }
    if (request.intent.kind !== "control") unsupported();
    return await this.#control.observeControl({ ...request, intent: request.intent });
  }
}
