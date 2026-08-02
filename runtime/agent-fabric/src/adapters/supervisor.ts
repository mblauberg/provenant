import { createHash } from "node:crypto";
import { isAbsolute } from "node:path";

import { AdapterProcessTransport } from "./process.js";
import { verifySpawnWrapperProvenance } from "./compatibility.js";
import { verifyNpmInstallAttestation } from "./npm-install-attestation-verifier.js";
import type { ProviderIdentityAssurance } from "./provider-identity.js";
import { DEFAULT_PROVIDER_TURN_TIMEOUT_MS, providerTurnResponseTimeoutMs } from "./provider-deadlines.js";
import {
  chairActionKey,
  chairTransportKey,
  enforceModelPolicy,
  isLongProviderOperation,
  isReleaseRequest,
  isRetainedChairLoss,
  isRetainedChildLoss,
  providerSessionGenerations,
  providerSessionReferences,
  validExpectedPrincipal,
  type AdapterModelPolicy,
} from "./adapter-request-policy.js";
import {
  chairLaunchChallengeDigest,
  parseAgentProvisionProviderResult,
  parseChairLaunchProviderResult,
  ProviderAdapterError,
  type AgentBridgeHandoff,
  type AgentProvisionProviderResult,
  type ChairLaunchHandoff,
  type ChairLaunchProviderResult,
  type AgentFabricPrincipalBinding,
} from "./providers/types.js";

export type AdapterProcessDefinition = {
  command: string[];
  environment: Record<string, string>;
  modelPolicy?: AdapterModelPolicy;
  wrapperProvenance?: { repositoryCommit: string; wrapperPath: string };
  npmInstallProductRoot?: string;
  providerAssurance?: ProviderIdentityAssurance;
};

export type AdapterSupervisorOptions = {
  controlTimeoutMs?: number;
  providerTurnTimeoutMs?: number;
  bridgeHealthIntervalMs?: number;
  verifyNpmInstall?: typeof verifyNpmInstallAttestation;
};

export type AdapterChairLaunchRequest = {
  schemaVersion: 1;
  actionId: string;
  providerContractDigest: string;
  payload: Record<string, unknown>;
};

export type AdapterChairRecoveryRequest = {
  schemaVersion: 1;
  recoveryId: string;
  lossId: string;
  actionId: string;
  providerContractDigest: string;
  resumeReference: string;
  expectedProviderSessionGeneration: number;
  nextProviderSessionGeneration: number;
  bridgeGeneration: number;
  payload: Record<string, unknown>;
};

export type AdapterAgentProvisionRequest = {
  schemaVersion: 1;
  runId: string;
  operation: "spawn" | "attach";
  actionId: string;
  targetAgentId: string;
  authorityId: string;
  bridgeGeneration: number;
  bridgeContractDigest: string;
  payload: Record<string, unknown>;
  providerSessionRef?: string;
  lifecycleAttestation?: Readonly<{ custodyId: string; checkpointDigest: string; challengeDigest: string }>;
};

export type RetainedChildBridge = Readonly<{
  runId: string;
  agentId: string;
  adapterId: string;
  actionId: string;
  providerSessionRef: string;
  providerSessionGeneration: number;
  bridgeGeneration: number;
}>;

export type RetainedChairBridge = Readonly<{
  projectSessionId: string;
  runId: string;
  agentId: string;
  principalGeneration: number;
  adapterId: string;
  actionId: string;
  providerSessionRef: string;
  providerSessionGeneration: number;
  bridgeGeneration: number;
}>;

const DEFAULT_CONTROL_TIMEOUT_MS = 30_000;
const DEFAULT_BRIDGE_HEALTH_INTERVAL_MS = 250;

function parseBridgeHealth(value: unknown, kind: "chair" | "child"): boolean {
  return typeof value === "object" && value !== null && !Array.isArray(value) &&
    Object.keys(value).length === 3 &&
    Reflect.get(value, "schemaVersion") === 1 &&
    Reflect.get(value, "kind") === kind &&
    typeof Reflect.get(value, "live") === "boolean" &&
    Reflect.get(value, "live") === true;
}

export class AdapterSupervisor {
  readonly #definitions: Record<string, AdapterProcessDefinition>;
  readonly #transports = new Map<string, AdapterProcessTransport>();
  readonly #openingTransports = new Set<AdapterProcessTransport>();
  readonly #chairTransports = new Map<string, AdapterProcessTransport>();
  readonly #knownChairSessions = new Map<string, number>();
  readonly #chairSessionByAction = new Map<string, string>();
  readonly #chairEntryBySession = new Map<string, RetainedChairBridge>();
  readonly #chairProjectBySession = new Map<string, string>();
  readonly #consumedChairHandoffHashes = new Set<string>();
  readonly #childTransports = new Map<string, AdapterProcessTransport>();
  readonly #knownChildSessions = new Map<string, number>();
  readonly #childSessionByAction = new Map<string, string>();
  readonly #childEntryBySession = new Map<string, RetainedChildBridge>();
  readonly #childPrincipalBySession = new Map<string, AgentFabricPrincipalBinding>();
  readonly #lostChildSessions = new Set<string>();
  readonly #consumedChildHandoffHashes = new Set<string>();
  readonly #bridgeTransitions = new Set<string>();
  #chairLossHandler: ((entry: RetainedChairBridge, reason: string) => void) | undefined;
  #childLossHandler: ((entry: RetainedChildBridge, reason: string) => void) | undefined;
  readonly #controlTimeoutMs: number;
  readonly #providerTurnTimeoutMs: number;
  readonly #bridgeHealthIntervalMs: number;
  readonly #verifyNpmInstall: typeof verifyNpmInstallAttestation;
  readonly #bridgeHealthTimer: NodeJS.Timeout;
  #bridgeHealthAuditInFlight = false;
  #closing = false;

  #assertOpen(): void {
    if (this.#closing) throw new ProviderAdapterError("PROVIDER_CLOSED", "adapter supervisor is closed");
  }

  /**
   * Every adapter process spawn rechecks npm-installed execution bytes and
   * re-derives wrapper provenance immediately beforehand. Drift after daemon
   * composition therefore fails closed instead of executing.
   */
  async #openTransport(
    adapterId: string,
    definition: AdapterProcessDefinition,
    environment?: Record<string, string>,
  ): Promise<AdapterProcessTransport> {
    this.#assertOpen();
    if (definition.npmInstallProductRoot !== undefined) {
      await this.#verifyNpmInstall(definition.npmInstallProductRoot);
    }
    if (definition.wrapperProvenance !== undefined) {
      await verifySpawnWrapperProvenance({
        adapterId,
        command: definition.command,
        expected: definition.wrapperProvenance,
      });
    }
    // Accepted residual (verify->exec swap race): a narrow TOCTOU window remains
    // between these re-verifications and the transport's own exec of the
    // command below. An attacker with concurrent write access could swap the
    // loader or wrapper bytes inside that window. Full closure needs snapshot execution
    // (spawning from a verified, immutable copy) and is out of scope for #132.
    this.#assertOpen();
    const transport = new AdapterProcessTransport(environment === undefined ? definition : { ...definition, environment });
    this.#openingTransports.add(transport);
    return transport;
  }

  constructor(definitions: Record<string, AdapterProcessDefinition>, options: AdapterSupervisorOptions = {}) {
    this.#definitions = definitions;
    this.#controlTimeoutMs = options.controlTimeoutMs ?? DEFAULT_CONTROL_TIMEOUT_MS;
    this.#providerTurnTimeoutMs = options.providerTurnTimeoutMs ?? DEFAULT_PROVIDER_TURN_TIMEOUT_MS;
    this.#bridgeHealthIntervalMs = options.bridgeHealthIntervalMs ?? DEFAULT_BRIDGE_HEALTH_INTERVAL_MS;
    this.#verifyNpmInstall = options.verifyNpmInstall ?? verifyNpmInstallAttestation;
    if (!Number.isFinite(this.#bridgeHealthIntervalMs) || this.#bridgeHealthIntervalMs <= 0) {
      throw new TypeError("bridgeHealthIntervalMs must be positive");
    }
    this.#bridgeHealthTimer = setInterval(() => { void this.#auditRetainedBridgeHealth(); }, this.#bridgeHealthIntervalMs);
    this.#bridgeHealthTimer.unref();
  }

  setChildBridgeLossHandler(handler: (entry: RetainedChildBridge, reason: string) => void): void {
    this.#childLossHandler = handler;
  }

  setChairBridgeLossHandler(handler: (entry: RetainedChairBridge, reason: string) => void): void {
    this.#chairLossHandler = handler;
  }

  async #probeChairBridge(transport: AdapterProcessTransport, entry: RetainedChairBridge): Promise<boolean> {
    const value = await transport.request("retained_bridge_health", {
      schemaVersion: 1,
      kind: "chair",
      actionId: entry.actionId,
      agentId: entry.agentId,
      projectSessionId: entry.projectSessionId,
      runId: entry.runId,
      principalGeneration: entry.principalGeneration,
      providerSessionRef: entry.providerSessionRef,
      providerSessionGeneration: entry.providerSessionGeneration,
      bridgeGeneration: entry.bridgeGeneration,
    }, { timeoutMs: this.#controlTimeoutMs });
    return parseBridgeHealth(value, "chair");
  }

  async #probeChildBridge(
    transport: AdapterProcessTransport,
    entry: RetainedChildBridge,
    principal: AgentFabricPrincipalBinding,
  ): Promise<boolean> {
    const value = await transport.request("retained_bridge_health", {
      schemaVersion: 1,
      kind: "child",
      actionId: entry.actionId,
      agentId: principal.agentId,
      projectSessionId: principal.projectSessionId,
      runId: principal.runId,
      principalGeneration: principal.principalGeneration,
      providerSessionRef: entry.providerSessionRef,
      providerSessionGeneration: entry.providerSessionGeneration,
      bridgeGeneration: entry.bridgeGeneration,
    }, { timeoutMs: this.#controlTimeoutMs });
    return parseBridgeHealth(value, "child");
  }

  async #auditRetainedBridgeHealth(): Promise<void> {
    if (this.#bridgeHealthAuditInFlight) return;
    this.#bridgeHealthAuditInFlight = true;
    try {
      const chairAudits = [...this.#chairEntryBySession.entries()].map(async ([key, entry]) => {
        if (this.#bridgeTransitions.has(key)) return;
        const transport = this.#chairTransports.get(key);
        if (transport === undefined) return;
        let live = false;
        try { live = await this.#probeChairBridge(transport, entry); } catch { live = false; }
        if (!live && !this.#bridgeTransitions.has(key) && this.#chairTransports.get(key) === transport) {
          try {
            this.#loseChairBridge(key, "inner retained chair bridge is unavailable");
          } catch {
            // Preserve the retained maps so a later audit retries durable loss fencing.
          } finally {
            await transport.close().catch(() => undefined);
          }
        }
      });
      const childAudits = [...this.#childEntryBySession.entries()].map(async ([key, entry]) => {
        if (this.#bridgeTransitions.has(key)) return;
        const transport = this.#childTransports.get(key);
        const principal = this.#childPrincipalBySession.get(key);
        if (transport === undefined || principal === undefined) return;
        let live = false;
        try { live = await this.#probeChildBridge(transport, entry, principal); } catch { live = false; }
        if (!live && !this.#bridgeTransitions.has(key) && this.#childTransports.get(key) === transport) {
          try {
            this.#loseChildBridge(key, "inner retained child bridge is unavailable");
          } catch {
            // Preserve the retained maps so a later audit retries durable loss fencing.
          } finally {
            await transport.close().catch(() => undefined);
          }
        }
      });
      await Promise.allSettled([...chairAudits, ...childAudits]);
    } finally {
      this.#bridgeHealthAuditInFlight = false;
    }
  }

  async request(adapterId: string, method: string, params: Record<string, unknown>): Promise<unknown> {
    this.#assertOpen();
    const definition = this.#definitions[adapterId];
    if (definition === undefined) throw new Error(`adapter is not configured: ${adapterId}`);
    enforceModelPolicy(adapterId, definition.modelPolicy, method, params);
    const sessionRefs = [...new Set(providerSessionReferences(params))];
    if (sessionRefs.length > 1) {
      throw new ProviderAdapterError(
        "STALE_LEASE_GENERATION",
        `${adapterId} chair request contains conflicting provider-session references`,
      );
    }
    const sessionRef = sessionRefs[0];
    const sessionChairKey = sessionRef === undefined ? undefined : chairTransportKey(adapterId, sessionRef);
    const actionChairKey = method === "lookup_action" && typeof params.actionId === "string"
      ? this.#chairSessionByAction.get(chairActionKey(adapterId, params.actionId))
      : undefined;
    const actionChildKey = method === "lookup_action" && typeof params.actionId === "string"
      ? this.#childSessionByAction.get(chairActionKey(adapterId, params.actionId))
      : undefined;
    const sessionChildKey = sessionRef === undefined ? undefined : chairTransportKey(adapterId, sessionRef);
    if (sessionChildKey !== undefined && this.#lostChildSessions.has(sessionChildKey) && method !== "lookup_action") {
      throw new ProviderAdapterError(
        "AGENT_BRIDGE_LOST",
        `${adapterId} child session cannot be resumed without a newly provisioned bridge`,
      );
    }
    const childKey = actionChildKey ?? (
      sessionChildKey !== undefined && this.#knownChildSessions.has(sessionChildKey)
        ? sessionChildKey
        : undefined
    );
    if (sessionChairKey !== undefined && actionChairKey !== undefined && sessionChairKey !== actionChairKey) {
      throw new ProviderAdapterError(
        "STALE_LEASE_GENERATION",
        `${adapterId} chair lookup action does not match its provider-session reference`,
      );
    }
    const chairKey = actionChairKey ?? sessionChairKey;
    const actionRoutedLookup = actionChairKey !== undefined;
    const knownChairGeneration = chairKey === undefined ? undefined : this.#knownChairSessions.get(chairKey);
    const requestedGenerations = providerSessionGenerations(params);
    if (
      knownChairGeneration !== undefined &&
      (
        (requestedGenerations.length === 0 && !actionRoutedLookup) ||
        requestedGenerations.some((generation) => (
          typeof generation !== "number" ||
          !Number.isSafeInteger(generation) ||
          generation !== knownChairGeneration
        ))
      )
    ) {
      throw new ProviderAdapterError(
        "STALE_LEASE_GENERATION",
        `${adapterId} chair request does not match its retained provider-session generation`,
      );
    }
    const retainedChairTransport = chairKey === undefined ? undefined : this.#chairTransports.get(chairKey);
    if (chairKey !== undefined && knownChairGeneration !== undefined && retainedChairTransport === undefined) {
      throw new ProviderAdapterError(
        "CHAIR_BRIDGE_LOST",
        `${adapterId} chair session cannot be resumed without its retained bridge`,
      );
    }
    const retainedChildTransport = childKey === undefined ? undefined : this.#childTransports.get(childKey);
    const knownChildGeneration = childKey === undefined ? undefined : this.#knownChildSessions.get(childKey);
    if (
      childKey !== undefined && knownChildGeneration !== undefined &&
      (retainedChildTransport === undefined || retainedChildTransport.closed)
    ) {
      this.#loseChildBridge(childKey, "retained child bridge is unavailable");
      throw new ProviderAdapterError("AGENT_BRIDGE_LOST", `${adapterId} retained child bridge is unavailable`);
    }
    if (
      knownChildGeneration !== undefined &&
      requestedGenerations.length > 0 &&
      requestedGenerations.some((generation) => generation !== knownChildGeneration)
    ) {
      throw new ProviderAdapterError(
        "STALE_LEASE_GENERATION",
        `${adapterId} child request does not match its retained provider-session generation`,
      );
    }
    let transport = retainedChairTransport ?? retainedChildTransport ?? this.#transports.get(adapterId);
    if (transport === undefined || transport.closed) {
      if (retainedChairTransport?.closed === true && chairKey !== undefined) {
        this.#loseChairBridge(chairKey, "retained chair bridge is unavailable");
        throw new ProviderAdapterError("CHAIR_BRIDGE_LOST", `${adapterId} retained chair bridge is unavailable`);
      }
      transport = await this.#openTransport(adapterId, definition);
      this.#assertOpen();
      this.#openingTransports.delete(transport);
      this.#transports.set(adapterId, transport);
    }
    try {
      const result = await transport.request(method, params, {
        timeoutMs: isLongProviderOperation(method, params)
          ? providerTurnResponseTimeoutMs(this.#providerTurnTimeoutMs)
          : this.#controlTimeoutMs,
      });
      if (retainedChairTransport !== undefined && chairKey !== undefined && isReleaseRequest(method, params)) {
        this.#removeChairBridge(chairKey);
        await transport.close().catch(() => undefined);
      }
      if (retainedChildTransport !== undefined && childKey !== undefined && isReleaseRequest(method, params)) {
        this.#removeChildBridge(childKey);
        this.#lostChildSessions.add(childKey);
        await transport.close().catch(() => undefined);
      }
      return result;
    } catch (error: unknown) {
      this.#openingTransports.delete(transport);
      let fencingError: unknown;
      if (retainedChairTransport !== undefined && chairKey !== undefined) {
        if (!isRetainedChairLoss(error, transport)) throw error;
        try {
          this.#loseChairBridge(chairKey, error instanceof Error ? error.message : "retained chair bridge lost");
        } catch (failure: unknown) {
          fencingError = failure;
        }
      } else if (retainedChildTransport !== undefined && childKey !== undefined) {
        if (!isRetainedChildLoss(error, transport)) throw error;
        try {
          this.#loseChildBridge(childKey, error instanceof Error ? error.message : "retained child bridge lost");
        } catch (failure: unknown) {
          fencingError = failure;
        }
      } else {
        this.#transports.delete(adapterId);
      }
      await transport.close().catch(() => undefined);
      if (fencingError !== undefined) throw fencingError;
      if (retainedChairTransport !== undefined) {
        throw new ProviderAdapterError("CHAIR_BRIDGE_LOST", `${adapterId} retained chair bridge was lost`);
      }
      throw error;
    }
  }

  async provisionAgent(
    adapterId: string,
    request: AdapterAgentProvisionRequest,
    handoff: AgentBridgeHandoff,
  ): Promise<AgentProvisionProviderResult> {
    this.#assertOpen();
    const definition = this.#definitions[adapterId];
    if (definition === undefined) throw new Error(`adapter is not configured: ${adapterId}`);
    if (
      typeof handoff.capability !== "string" || handoff.capability.length === 0 ||
      typeof handoff.socketPath !== "string" || !isAbsolute(handoff.socketPath) ||
      !validExpectedPrincipal(handoff.expectedPrincipal) ||
      handoff.expectedPrincipal.agentId !== request.targetAgentId ||
      handoff.expectedPrincipal.runId !== request.runId ||
      ((handoff.lifecycleAttestation === undefined) !== (request.lifecycleAttestation === undefined)) ||
      (handoff.lifecycleAttestation !== undefined && request.lifecycleAttestation !== undefined &&
        (handoff.lifecycleAttestation.custodyId !== request.lifecycleAttestation.custodyId ||
         handoff.lifecycleAttestation.checkpointDigest !== request.lifecycleAttestation.checkpointDigest ||
         handoff.lifecycleAttestation.challengeDigest !== request.lifecycleAttestation.challengeDigest))
    ) {
      throw new ProviderAdapterError("PRIVATE_HANDOFF_UNAVAILABLE", "agent bridge private handoff is invalid");
    }
    enforceModelPolicy(adapterId, definition.modelPolicy, request.operation, { payload: request.payload });
    const handoffHash = createHash("sha256").update(handoff.capability).digest("hex");
    if (this.#consumedChildHandoffHashes.has(handoffHash)) {
      throw new ProviderAdapterError("PRIVATE_HANDOFF_UNAVAILABLE", "agent bridge private handoff was already consumed");
    }
    this.#consumedChildHandoffHashes.add(handoffHash);
    const transport = await this.#openTransport(adapterId, definition, {
        ...definition.environment,
        AGENT_FABRIC_HANDOFF_KIND: "agent",
        AGENT_FABRIC_CAPABILITY: handoff.capability,
        AGENT_FABRIC_SOCKET_PATH: handoff.socketPath,
        AGENT_FABRIC_EXPECTED_AGENT_ID: handoff.expectedPrincipal.agentId,
        AGENT_FABRIC_EXPECTED_PROJECT_SESSION_ID: handoff.expectedPrincipal.projectSessionId,
        AGENT_FABRIC_EXPECTED_RUN_ID: handoff.expectedPrincipal.runId,
        AGENT_FABRIC_EXPECTED_PRINCIPAL_GENERATION: String(handoff.expectedPrincipal.principalGeneration),
        ...(handoff.lifecycleAttestation === undefined ? {} : {
          AGENT_FABRIC_ATTESTATION_CHALLENGE: handoff.lifecycleAttestation.challenge,
          AGENT_FABRIC_ATTESTATION_CHALLENGE_DIGEST: handoff.lifecycleAttestation.challengeDigest,
          AGENT_FABRIC_LIFECYCLE_CUSTODY_ID: handoff.lifecycleAttestation.custodyId,
          AGENT_FABRIC_LIFECYCLE_CHECKPOINT_DIGEST: handoff.lifecycleAttestation.checkpointDigest,
        }),
    });
    try {
      this.#assertOpen();
      const publicRequest = {
        schemaVersion: request.schemaVersion,
        runId: request.runId,
        operation: request.operation,
        actionId: request.actionId,
        targetAgentId: request.targetAgentId,
        authorityId: request.authorityId,
        bridgeGeneration: request.bridgeGeneration,
        bridgeContractDigest: request.bridgeContractDigest,
        payload: request.payload,
        ...(request.providerSessionRef === undefined ? {} : { providerSessionRef: request.providerSessionRef }),
      };
      const result = parseAgentProvisionProviderResult(
        await transport.request("provision_agent", publicRequest, { timeoutMs: this.#providerTurnTimeoutMs }),
        {
          adapterId,
          actionId: request.actionId,
          targetAgentId: request.targetAgentId,
          bridgeGeneration: request.bridgeGeneration,
          bridgeContractDigest: request.bridgeContractDigest,
          ...(request.lifecycleAttestation === undefined ? {} : {
            lifecycleAttestation: request.lifecycleAttestation,
          }),
        },
      );
      await transport.request("capabilities", {}, { timeoutMs: this.#controlTimeoutMs });
      if (!await this.#probeChildBridge(transport, {
        runId: request.runId,
        agentId: request.targetAgentId,
        adapterId,
        actionId: request.actionId,
        providerSessionRef: result.providerSessionRef,
        providerSessionGeneration: result.providerSessionGeneration,
        bridgeGeneration: result.bridgeGeneration,
      }, handoff.expectedPrincipal)) {
        throw new ProviderAdapterError("AGENT_BRIDGE_LOST", "inner child bridge closed before retention");
      }
      this.#assertOpen();
      if (transport.closed) throw new ProviderAdapterError("AGENT_BRIDGE_LOST", "agent bridge closed before retention");
      const key = chairTransportKey(adapterId, result.providerSessionRef);
      if (this.#childTransports.has(key) || this.#chairTransports.has(key)) {
        throw new ProviderAdapterError("AGENT_BRIDGE_CONFLICT", "provider session already has a retained bridge");
      }
      const entry: RetainedChildBridge = Object.freeze({
        runId: request.runId,
        agentId: request.targetAgentId,
        adapterId,
        actionId: request.actionId,
        providerSessionRef: result.providerSessionRef,
        providerSessionGeneration: result.providerSessionGeneration,
        bridgeGeneration: result.bridgeGeneration,
      });
      this.#openingTransports.delete(transport);
      this.#childTransports.set(key, transport);
      this.#lostChildSessions.delete(key);
      this.#knownChildSessions.set(key, result.providerSessionGeneration);
      this.#childSessionByAction.set(chairActionKey(adapterId, request.actionId), key);
      this.#childEntryBySession.set(key, entry);
      this.#childPrincipalBySession.set(key, handoff.expectedPrincipal);
      transport.onClose(() => {
        if (this.#childTransports.get(key) === transport) {
          try { this.#loseChildBridge(key, "retained adapter transport closed"); } catch { /* periodic audit retries */ }
        }
      });
      return result;
    } catch (error: unknown) {
      this.#openingTransports.delete(transport);
      await transport.close().catch(() => undefined);
      throw error;
    }
  }

  hasRetainedChildBridge(entry: RetainedChildBridge): boolean {
    const key = chairTransportKey(entry.adapterId, entry.providerSessionRef);
    const current = this.#childEntryBySession.get(key);
    return current !== undefined && current.actionId === entry.actionId &&
      current.runId === entry.runId && current.agentId === entry.agentId &&
      current.providerSessionGeneration === entry.providerSessionGeneration &&
      current.bridgeGeneration === entry.bridgeGeneration &&
      this.#childTransports.get(key)?.closed === false;
  }

  #removeChildBridge(key: string): void {
    this.#childTransports.delete(key);
    this.#childEntryBySession.delete(key);
    this.#childPrincipalBySession.delete(key);
  }

  #purgeChildBridge(key: string): void {
    this.#removeChildBridge(key);
    this.#knownChildSessions.delete(key);
    this.#lostChildSessions.delete(key);
    for (const [actionKey, sessionKey] of [...this.#childSessionByAction.entries()]) {
      if (sessionKey === key) this.#childSessionByAction.delete(actionKey);
    }
  }

  #loseChildBridge(key: string, reason: string): void {
    const entry = this.#childEntryBySession.get(key);
    if (entry !== undefined) {
      this.#childLossHandler?.(entry, reason);
      this.#removeChildBridge(key);
      this.#lostChildSessions.add(key);
    }
  }

  #removeChairBridge(key: string): void {
    this.#chairTransports.delete(key);
    this.#chairEntryBySession.delete(key);
  }

  #purgeChairBridge(key: string): void {
    this.#removeChairBridge(key);
    this.#knownChairSessions.delete(key);
    this.#chairProjectBySession.delete(key);
    for (const [actionKey, sessionKey] of [...this.#chairSessionByAction.entries()]) {
      if (sessionKey === key) this.#chairSessionByAction.delete(actionKey);
    }
  }

  #loseChairBridge(key: string, reason: string): void {
    const entry = this.#chairEntryBySession.get(key);
    if (entry !== undefined) {
      this.#chairLossHandler?.(entry, reason);
      this.#chairTransports.delete(key);
      this.#chairEntryBySession.delete(key);
    }
  }

  /** Removes cleanly retired bridges without emitting a fabricated loss. */
  retireChairBridge(entry: RetainedChairBridge): void {
    const key = chairTransportKey(entry.adapterId, entry.providerSessionRef);
    const current = this.#chairEntryBySession.get(key);
    if (
      current === undefined || current.projectSessionId !== entry.projectSessionId ||
      current.runId !== entry.runId || current.agentId !== entry.agentId ||
      current.principalGeneration !== entry.principalGeneration || current.actionId !== entry.actionId ||
      current.providerSessionGeneration !== entry.providerSessionGeneration ||
      current.bridgeGeneration !== entry.bridgeGeneration
    ) return;
    this.#bridgeTransitions.add(key);
    const transport = this.#chairTransports.get(key);
    this.#purgeChairBridge(key);
    if (transport === undefined) {
      this.#bridgeTransitions.delete(key);
      return;
    }
    void transport.close()
      .catch(() => undefined)
      .finally(() => this.#bridgeTransitions.delete(key));
  }

  /** Removes cleanly retired bridges without emitting a fabricated loss. */
  retireProjectSessionBridges(projectSessionId: string): void {
    const transports = new Set<AdapterProcessTransport>();
    const transitionKeys: string[] = [];
    for (const [key, ownerProjectSessionId] of [...this.#chairProjectBySession.entries()]) {
      if (ownerProjectSessionId !== projectSessionId) continue;
      this.#bridgeTransitions.add(key);
      transitionKeys.push(key);
      const transport = this.#chairTransports.get(key);
      if (transport !== undefined) transports.add(transport);
      this.#purgeChairBridge(key);
    }
    for (const [key] of [...this.#childEntryBySession.entries()]) {
      if (this.#childPrincipalBySession.get(key)?.projectSessionId !== projectSessionId) continue;
      this.#bridgeTransitions.add(key);
      transitionKeys.push(key);
      const transport = this.#childTransports.get(key);
      if (transport !== undefined) transports.add(transport);
      this.#purgeChildBridge(key);
    }
    void Promise.allSettled([...transports].map(async (transport) => await transport.close()))
      .finally(() => {
        for (const key of transitionKeys) this.#bridgeTransitions.delete(key);
      });
  }

  async launchChair(
    adapterId: string,
    request: AdapterChairLaunchRequest,
    handoff: ChairLaunchHandoff,
  ): Promise<ChairLaunchProviderResult> {
    this.#assertOpen();
    const definition = this.#definitions[adapterId];
    if (definition === undefined) throw new Error(`adapter is not configured: ${adapterId}`);
    if (
      typeof handoff.capability !== "string" ||
      handoff.capability.length === 0 ||
      typeof handoff.socketPath !== "string" ||
      handoff.socketPath.length === 0 ||
      !isAbsolute(handoff.socketPath) ||
      typeof handoff.attestationChallenge !== "string" ||
      !/^[0-9a-f]{64}$/u.test(handoff.attestationChallenge) ||
      !validExpectedPrincipal(handoff.expectedPrincipal)
    ) {
      throw new ProviderAdapterError(
        "PRIVATE_HANDOFF_UNAVAILABLE",
        "chair launch private handoff is unavailable or invalid",
      );
    }
    enforceModelPolicy(adapterId, definition.modelPolicy, "spawn", { payload: request.payload });
    const handoffHash = createHash("sha256").update(handoff.capability).digest("hex");
    if (this.#consumedChairHandoffHashes.has(handoffHash)) {
      throw new ProviderAdapterError("PRIVATE_HANDOFF_UNAVAILABLE", "chair launch private handoff was already consumed");
    }
    this.#consumedChairHandoffHashes.add(handoffHash);
    const transport = await this.#openTransport(adapterId, definition, {
        ...definition.environment,
        AGENT_FABRIC_HANDOFF_KIND: "chair",
        AGENT_FABRIC_CAPABILITY: handoff.capability,
        AGENT_FABRIC_SOCKET_PATH: handoff.socketPath,
        AGENT_FABRIC_ATTESTATION_CHALLENGE: handoff.attestationChallenge,
        AGENT_FABRIC_EXPECTED_AGENT_ID: handoff.expectedPrincipal.agentId,
        AGENT_FABRIC_EXPECTED_PROJECT_SESSION_ID: handoff.expectedPrincipal.projectSessionId,
        AGENT_FABRIC_EXPECTED_RUN_ID: handoff.expectedPrincipal.runId,
        AGENT_FABRIC_EXPECTED_PRINCIPAL_GENERATION: String(handoff.expectedPrincipal.principalGeneration),
    });
    try {
      this.#assertOpen();
      const result = parseChairLaunchProviderResult(await transport.request("launch_chair", request, {
        timeoutMs: this.#providerTurnTimeoutMs,
      }), {
        providerAdapterId: adapterId,
        providerActionId: request.actionId,
        providerContractDigest: request.providerContractDigest,
        challengeDigest: chairLaunchChallengeDigest(handoff.attestationChallenge),
      });
      // This is a liveness handshake only; the provider-originated attestation
      // above remains the sole continuity proof.
      await transport.request("capabilities", {}, { timeoutMs: this.#controlTimeoutMs });
      const entry: RetainedChairBridge = Object.freeze({
        ...handoff.expectedPrincipal,
        adapterId,
        actionId: request.actionId,
        providerSessionRef: result.resumeReference,
        providerSessionGeneration: result.providerSessionGeneration,
        bridgeGeneration: 1,
      });
      if (!await this.#probeChairBridge(transport, entry)) {
        throw new ProviderAdapterError("CHAIR_BRIDGE_LOST", `${adapterId} inner chair bridge closed before retention`);
      }
      if (transport.closed) {
        throw new ProviderAdapterError("CHAIR_BRIDGE_LOST", `${adapterId} chair bridge closed before terminal handoff`);
      }
      this.#assertOpen();
      const key = chairTransportKey(adapterId, result.resumeReference);
      if (this.#knownChairSessions.has(key)) {
        throw new ProviderAdapterError("CHAIR_BRIDGE_CONFLICT", `${adapterId} already owns the provider chair session`);
      }
      this.#openingTransports.delete(transport);
      this.#chairTransports.set(key, transport);
      this.#knownChairSessions.set(key, result.providerSessionGeneration);
      this.#chairSessionByAction.set(chairActionKey(adapterId, request.actionId), key);
      this.#chairEntryBySession.set(key, entry);
      this.#chairProjectBySession.set(key, entry.projectSessionId);
      transport.onClose(() => {
        if (this.#chairTransports.get(key) === transport) {
          try { this.#loseChairBridge(key, "retained adapter transport closed"); } catch { /* periodic audit retries */ }
        }
      });
      return result;
    } catch {
      this.#openingTransports.delete(transport);
      await transport.close().catch(() => undefined);
      throw new ProviderAdapterError("CHAIR_LAUNCH_FAILED", `${adapterId} chair launch adapter handoff failed`);
    }
  }

  async recoverChair(
    adapterId: string,
    request: AdapterChairRecoveryRequest,
    handoff: ChairLaunchHandoff,
  ): Promise<ChairLaunchProviderResult> {
    this.#assertOpen();
    const definition = this.#definitions[adapterId];
    if (definition === undefined) throw new Error(`adapter is not configured: ${adapterId}`);
    if (
      !validExpectedPrincipal(handoff.expectedPrincipal) ||
      typeof handoff.capability !== "string" || handoff.capability.length === 0 ||
      typeof handoff.socketPath !== "string" || !isAbsolute(handoff.socketPath) ||
      !/^[0-9a-f]{64}$/u.test(handoff.attestationChallenge) ||
      request.nextProviderSessionGeneration !== request.expectedProviderSessionGeneration + 1 ||
      request.bridgeGeneration < 2
    ) throw new ProviderAdapterError("PRIVATE_HANDOFF_UNAVAILABLE", "chair recovery handoff is invalid");
    enforceModelPolicy(adapterId, definition.modelPolicy, "spawn", { payload: request.payload });
    const handoffHash = createHash("sha256").update(handoff.capability).digest("hex");
    if (this.#consumedChairHandoffHashes.has(handoffHash)) {
      throw new ProviderAdapterError("PRIVATE_HANDOFF_UNAVAILABLE", "chair recovery handoff was already consumed");
    }
    this.#consumedChairHandoffHashes.add(handoffHash);
    const transport = await this.#openTransport(adapterId, definition, {
        ...definition.environment,
        AGENT_FABRIC_HANDOFF_KIND: "chair",
        AGENT_FABRIC_CAPABILITY: handoff.capability,
        AGENT_FABRIC_SOCKET_PATH: handoff.socketPath,
        AGENT_FABRIC_ATTESTATION_CHALLENGE: handoff.attestationChallenge,
        AGENT_FABRIC_EXPECTED_AGENT_ID: handoff.expectedPrincipal.agentId,
        AGENT_FABRIC_EXPECTED_PROJECT_SESSION_ID: handoff.expectedPrincipal.projectSessionId,
        AGENT_FABRIC_EXPECTED_RUN_ID: handoff.expectedPrincipal.runId,
        AGENT_FABRIC_EXPECTED_PRINCIPAL_GENERATION: String(handoff.expectedPrincipal.principalGeneration),
    });
    try {
      this.#assertOpen();
      const result = parseChairLaunchProviderResult(await transport.request("recover_chair", request, {
        timeoutMs: this.#providerTurnTimeoutMs,
      }), {
        providerAdapterId: adapterId,
        providerActionId: request.actionId,
        providerContractDigest: request.providerContractDigest,
        challengeDigest: chairLaunchChallengeDigest(handoff.attestationChallenge),
      });
      if (
        result.resumeReference !== request.resumeReference ||
        result.providerSessionGeneration !== request.nextProviderSessionGeneration
      ) throw new ProviderAdapterError("CHAIR_BRIDGE_LOST", "chair recovery returned a stale provider session");
      await transport.request("capabilities", {}, { timeoutMs: this.#controlTimeoutMs });
      const entry: RetainedChairBridge = Object.freeze({
        ...handoff.expectedPrincipal,
        adapterId,
        actionId: request.actionId,
        providerSessionRef: result.resumeReference,
        providerSessionGeneration: result.providerSessionGeneration,
        bridgeGeneration: request.bridgeGeneration,
      });
      if (!await this.#probeChairBridge(transport, entry)) {
        throw new ProviderAdapterError("CHAIR_BRIDGE_LOST", "inner recovered chair bridge is unavailable");
      }
      this.#assertOpen();
      const key = chairTransportKey(adapterId, result.resumeReference);
      const prior = this.#chairTransports.get(key);
      if (prior !== undefined && prior !== transport) {
        this.#removeChairBridge(key);
        await prior.close().catch(() => undefined);
      }
      this.#openingTransports.delete(transport);
      this.#chairTransports.set(key, transport);
      this.#knownChairSessions.set(key, result.providerSessionGeneration);
      this.#chairSessionByAction.set(chairActionKey(adapterId, request.actionId), key);
      this.#chairEntryBySession.set(key, entry);
      this.#chairProjectBySession.set(key, entry.projectSessionId);
      transport.onClose(() => {
        if (this.#chairTransports.get(key) === transport) {
          try { this.#loseChairBridge(key, "retained recovered chair transport closed"); } catch { /* periodic audit retries */ }
        }
      });
      return result;
    } catch (error: unknown) {
      this.#openingTransports.delete(transport);
      await transport.close().catch(() => undefined);
      throw error;
    }
  }

  async promoteRetainedChildBridgeToChair(input: Readonly<{
    projectSessionId: string;
    runId: string;
    agentId: string;
    principalGeneration: number;
    adapterId: string;
    actionId: string;
    providerSessionRef: string;
    providerSessionGeneration: number;
    sourceBridgeGeneration: number;
    chairBridgeGeneration: number;
    sourceActionId?: string;
    promotionActionId?: string;
  }>): Promise<boolean> {
    const sourceActionId = input.sourceActionId ?? input.actionId;
    const chairActionId = input.promotionActionId ?? input.actionId;
    const key = chairTransportKey(input.adapterId, input.providerSessionRef);
    if (this.#bridgeTransitions.has(key)) return false;
    this.#bridgeTransitions.add(key);
    try {
      const retainedChairTransport = this.#chairTransports.get(key);
      const retainedChairEntry = this.#chairEntryBySession.get(key);
      if (
        retainedChairTransport !== undefined && retainedChairEntry !== undefined &&
        retainedChairEntry.actionId === chairActionId && retainedChairEntry.runId === input.runId &&
        retainedChairEntry.projectSessionId === input.projectSessionId && retainedChairEntry.agentId === input.agentId &&
        retainedChairEntry.principalGeneration === input.principalGeneration &&
        retainedChairEntry.providerSessionGeneration === input.providerSessionGeneration &&
        retainedChairEntry.bridgeGeneration === input.chairBridgeGeneration &&
        await this.#probeChairBridge(retainedChairTransport, retainedChairEntry)
      ) return true;
      const transport = this.#childTransports.get(key);
      const entry = this.#childEntryBySession.get(key);
      const principal = this.#childPrincipalBySession.get(key);
      if (
        transport === undefined || entry === undefined || principal === undefined ||
        entry.actionId !== sourceActionId || entry.runId !== input.runId || entry.agentId !== input.agentId ||
        entry.providerSessionGeneration !== input.providerSessionGeneration ||
        entry.bridgeGeneration !== input.sourceBridgeGeneration ||
        principal.projectSessionId !== input.projectSessionId ||
        principal.principalGeneration !== input.principalGeneration ||
        !await this.#probeChildBridge(transport, entry, principal)
      ) return false;
      const promoted = await transport.request("promote_retained_bridge", {
        schemaVersion: 1,
        actionId: chairActionId,
        sourceActionId,
        agentId: input.agentId,
        projectSessionId: input.projectSessionId,
        runId: input.runId,
        principalGeneration: input.principalGeneration,
        providerSessionRef: input.providerSessionRef,
        providerSessionGeneration: input.providerSessionGeneration,
        sourceBridgeGeneration: input.sourceBridgeGeneration,
        chairBridgeGeneration: input.chairBridgeGeneration,
      }, { timeoutMs: this.#controlTimeoutMs });
      if (
        typeof promoted !== "object" || promoted === null ||
        Reflect.get(promoted, "schemaVersion") !== 1 || Reflect.get(promoted, "promoted") !== true
      ) return false;
      this.#purgeChildBridge(key);
      const chairEntry: RetainedChairBridge = Object.freeze({
        projectSessionId: input.projectSessionId,
        runId: input.runId,
        agentId: input.agentId,
        principalGeneration: input.principalGeneration,
        adapterId: input.adapterId,
        actionId: chairActionId,
        providerSessionRef: input.providerSessionRef,
        providerSessionGeneration: input.providerSessionGeneration,
        bridgeGeneration: input.chairBridgeGeneration,
      });
      this.#chairTransports.set(key, transport);
      this.#knownChairSessions.set(key, input.providerSessionGeneration);
      this.#chairSessionByAction.set(chairActionKey(input.adapterId, chairActionId), key);
      this.#chairEntryBySession.set(key, chairEntry);
      this.#chairProjectBySession.set(key, chairEntry.projectSessionId);
      return await this.#probeChairBridge(transport, chairEntry);
    } finally {
      this.#bridgeTransitions.delete(key);
    }
  }

  async lookupRetainedSuccessorBridge(input: Readonly<{
    projectSessionId: string;
    runId: string;
    agentId: string;
    principalGeneration: number;
    adapterId: string;
    actionId: string;
    providerSessionRef: string;
    providerSessionGeneration: number;
    sourceBridgeGeneration: number;
    chairBridgeGeneration: number;
    sourceActionId?: string;
    promotionActionId?: string;
  }>): Promise<"child" | "chair" | "missing"> {
    const sourceActionId = input.sourceActionId ?? input.actionId;
    const chairActionId = input.promotionActionId ?? input.actionId;
    const key = chairTransportKey(input.adapterId, input.providerSessionRef);
    const chairTransport = this.#chairTransports.get(key);
    const chairEntry = this.#chairEntryBySession.get(key);
    if (
      chairTransport !== undefined && chairEntry !== undefined &&
      chairEntry.projectSessionId === input.projectSessionId && chairEntry.runId === input.runId &&
      chairEntry.agentId === input.agentId && chairEntry.principalGeneration === input.principalGeneration &&
      chairEntry.actionId === chairActionId &&
      chairEntry.providerSessionGeneration === input.providerSessionGeneration &&
      chairEntry.bridgeGeneration === input.chairBridgeGeneration
    ) {
      try {
        if (await this.#probeChairBridge(chairTransport, chairEntry)) return "chair";
      } catch { /* an unobservable retained bridge remains ambiguous */ }
      return "missing";
    }
    const childTransport = this.#childTransports.get(key);
    const childEntry = this.#childEntryBySession.get(key);
    const principal = this.#childPrincipalBySession.get(key);
    if (
      childTransport === undefined || childEntry === undefined || principal === undefined ||
      childEntry.runId !== input.runId || childEntry.agentId !== input.agentId ||
      childEntry.actionId !== sourceActionId ||
      childEntry.providerSessionGeneration !== input.providerSessionGeneration ||
      childEntry.bridgeGeneration !== input.sourceBridgeGeneration ||
      principal.projectSessionId !== input.projectSessionId || principal.principalGeneration !== input.principalGeneration
    ) return "missing";
    try {
      if (await this.#probeChildBridge(childTransport, childEntry, principal)) return "child";
      const recoveredChairEntry: RetainedChairBridge = Object.freeze({
        ...principal,
        adapterId: input.adapterId,
        actionId: chairActionId,
        providerSessionRef: input.providerSessionRef,
        providerSessionGeneration: input.providerSessionGeneration,
        bridgeGeneration: input.chairBridgeGeneration,
      });
      if (!await this.#probeChairBridge(childTransport, recoveredChairEntry)) return "missing";
      this.#purgeChildBridge(key);
      this.#chairTransports.set(key, childTransport);
      this.#knownChairSessions.set(key, input.providerSessionGeneration);
      this.#chairSessionByAction.set(chairActionKey(input.adapterId, chairActionId), key);
      this.#chairEntryBySession.set(key, recoveredChairEntry);
      this.#chairProjectBySession.set(key, recoveredChairEntry.projectSessionId);
      return "chair";
    } catch {
      return "missing";
    }
  }

  hasRetainedChairBridge(entry: RetainedChairBridge): boolean {
    const key = chairTransportKey(entry.adapterId, entry.providerSessionRef);
    const current = this.#chairEntryBySession.get(key);
    return current !== undefined && current.actionId === entry.actionId &&
      current.projectSessionId === entry.projectSessionId && current.runId === entry.runId &&
      current.agentId === entry.agentId && current.principalGeneration === entry.principalGeneration &&
      current.providerSessionGeneration === entry.providerSessionGeneration &&
      current.bridgeGeneration === entry.bridgeGeneration &&
      this.#chairTransports.get(key)?.closed === false;
  }

  async close(): Promise<void> {
    this.#closing = true;
    clearInterval(this.#bridgeHealthTimer);
    const transports = [...new Set([
      ...this.#openingTransports,
      ...this.#transports.values(),
      ...this.#chairTransports.values(),
      ...this.#childTransports.values(),
    ])];
    this.#openingTransports.clear();
    this.#transports.clear();
    this.#chairTransports.clear();
    this.#knownChairSessions.clear();
    this.#chairSessionByAction.clear();
    this.#chairEntryBySession.clear();
    this.#chairProjectBySession.clear();
    this.#lostChildSessions.clear();
    this.#knownChildSessions.clear();
    this.#childSessionByAction.clear();
    this.#childEntryBySession.clear();
    this.#childPrincipalBySession.clear();
    this.#childTransports.clear();
    await Promise.allSettled(transports.map((transport) => transport.close()));
  }
}
