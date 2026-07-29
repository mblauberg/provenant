import { createHash } from "node:crypto";
import { isAbsolute, join } from "node:path";
import { setTimeout as delay } from "node:timers/promises";

import { parseTimestamp } from "@local/agent-fabric-protocol";
import type { AgentId, ProviderActionId } from "@local/agent-fabric-protocol";

import type {
  AgentEnsurePaneIntent,
  ArrangePanesIntent,
  ConsoleEnsurePaneIntent,
  DirectSteerIntent,
  FocusTargetIntent,
  HerdrEffectLookup,
  HerdrEffectReceipt,
  HerdrPaneObservation,
  HerdrControlPort,
  HerdrPresencePort,
  ProjectAgentMetadataIntent,
  ProjectAttentionIntent,
  ShowNotificationIntent,
  WakeAgentIntent,
} from "./contracts.js";
import type { HerdrEffectEvidenceJournal } from "./effect-journal.js";

const CREDENTIAL_PATTERN = /\b(?:afb|afc|afop)_[A-Za-z0-9_-]{8,}|\bghp_[A-Za-z0-9_]{8,}|\bgithub_pat_[A-Za-z0-9_]{8,}/u;

export type HerdrCommandRequest = Readonly<{
  executable: string;
  arguments: readonly string[];
  timeoutMs: number;
  maximumOutputBytes: number;
}>;

export interface HerdrCommandPort {
  run(request: HerdrCommandRequest): Promise<Buffer>;
}

export type HerdrCliBoundaryOptions = Readonly<{
  executable: string;
  expectedProtocol: number;
  projectId: string;
  projectSessionId: string;
  canonicalProjectRoot: string;
  consoleExecutable: string;
  observerExecutable?: string;
  observerSocketPath?: string;
  observerCapabilityFile?: string;
  observerCursorDirectory?: string;
  process: HerdrCommandPort;
  effectJournal: HerdrEffectEvidenceJournal;
  verifyExecutable?: (path: string) => Promise<void>;
  clock?: () => number;
}>;

export class HerdrCliBoundary implements HerdrControlPort, HerdrPresencePort {
  readonly #options: HerdrCliBoundaryOptions;
  readonly #expectedAgents = new Map<AgentId, AgentEnsurePaneIntent["identity"]>();
  readonly #agentPanes = new Map<AgentId, string>();
  readonly #presenceKinds = new Map<AgentId, "provider-session" | "observer">();
  readonly #observerNames = new Map<AgentId, string>();
  readonly #paneTabs = new Map<string, string>();
  #consolePane: string | null = null;
  #presenceSnapshotCache: { expiresAt: number; value: Promise<Record<string, unknown>> } | null = null;

  constructor(options: HerdrCliBoundaryOptions) {
    assertOptions(options);
    this.#options = options;
  }

  async probe(): Promise<{ version: string; protocol: number }> {
    const snapshot = await this.#snapshot();
    return { version: snapshot.version as string, protocol: snapshot.protocol as number };
  }

  async lookupAction(actionId: ProviderActionId): Promise<HerdrEffectLookup> {
    return this.#options.effectJournal.lookupAction(actionId).catch(() => ({ status: "unknown" }));
  }

  /** Rehydrates only the expected observation binding; it performs no pane mutation. */
  restorePresenceRegistration(intent: AgentEnsurePaneIntent): void {
    if (
      intent.kind !== "agent.ensure-pane" ||
      intent.identity.projectId !== this.#options.projectId ||
      intent.identity.projectSessionId !== this.#options.projectSessionId
    ) throw new TypeError("Herdr presence registration is bound to another project session");
    if (
      !Number.isSafeInteger(intent.identity.providerSessionGeneration) ||
      intent.identity.providerSessionGeneration < 1 ||
      Buffer.byteLength(intent.identity.providerSessionRef, "utf8") < 1 ||
      Buffer.byteLength(intent.identity.providerSessionRef, "utf8") > 512 ||
      /[\u0000-\u001f\u007f\u009b]/u.test(intent.identity.providerSessionRef)
    ) throw new TypeError("Herdr presence registration identity is invalid");
    if (intent.surface === "observer") {
      if (
        this.#options.observerExecutable === undefined || this.#options.observerSocketPath === undefined ||
        this.#options.observerCapabilityFile === undefined || this.#options.observerCursorDirectory === undefined
      ) throw new TypeError("Herdr observer presence registration is not configured");
      this.#presenceKinds.set(intent.identity.agentId, "observer");
      this.#observerNames.set(
        intent.identity.agentId,
        `fabric-observer-${createHash("sha256").update(`${intent.identity.coordinationRunId}\0${intent.identity.agentId}`).digest("hex").slice(0, 16)}`,
      );
    } else if (intent.surface === "provider-tui") {
      this.#presenceKinds.set(intent.identity.agentId, "provider-session");
    } else {
      throw new TypeError("Herdr presence registration surface is invalid");
    }
    this.#expectedAgents.set(intent.identity.agentId, intent.identity);
  }

  /** Rehydrates a Console pane binding from structured Herdr presence only. */
  async restoreConsolePresenceRegistration(intent: ConsoleEnsurePaneIntent): Promise<void> {
    if (
      intent.kind !== "console.ensure-pane" || intent.profileId !== "agent-fabric-console" ||
      intent.projectId !== this.#options.projectId || intent.projectSessionId !== this.#options.projectSessionId
    ) throw new TypeError("Herdr Console presence registration is bound to another project session");
    const snapshot = await this.#snapshotForPresence();
    const paneRef = findNamedPane(
      snapshot,
      consoleName(intent.projectId, intent.projectSessionId),
      this.#options.canonicalProjectRoot,
    );
    this.#consolePane = paneRef;
    if (paneRef === null) return;
    const paneTab = findPaneTab(snapshot, paneRef);
    if (paneTab !== null) this.#paneTabs.set(paneRef, paneTab);
  }

  async ensureConsolePane(actionId: ProviderActionId, intent: ConsoleEnsurePaneIntent): Promise<HerdrEffectReceipt> {
    if (intent.projectId !== this.#options.projectId || intent.projectSessionId !== this.#options.projectSessionId) {
      throw new TypeError("Herdr Console intent is bound to another project session");
    }
    const name = consoleName(intent.projectId, intent.projectSessionId);
    const snapshot = await this.#snapshot();
    let paneRef = findNamedPane(snapshot, name, this.#options.canonicalProjectRoot);
    if (paneRef === null) {
      await this.#options.verifyExecutable?.(this.#options.consoleExecutable);
      const result = parseCommandResponse(await this.#options.process.run({
        executable: this.#options.executable,
        arguments: [
          "agent", "start", name,
          "--cwd", this.#options.canonicalProjectRoot,
          "--no-focus",
          "--",
          this.#options.consoleExecutable,
          "--project", this.#options.canonicalProjectRoot,
          "--herdr",
        ],
        timeoutMs: 10_000,
        maximumOutputBytes: 262_144,
      }), 262_144);
      const created = paneIdentityFromResult(result);
      paneRef = created.paneRef;
      if (created.tabId !== null) this.#paneTabs.set(paneRef, created.tabId);
    }
    this.#consolePane = paneRef;
    const consoleTab = findPaneTab(snapshot, paneRef);
    if (consoleTab !== null) this.#paneTabs.set(paneRef, consoleTab);
    const receipt: HerdrEffectReceipt = {
      status: "applied",
      operation: "console.ensure-pane",
      paneRef: paneRef as never,
      detail: { identityEvidence: "pane-presence-only" },
    };
    await this.#options.effectJournal.record(actionId, receipt);
    return receipt;
  }
  async ensureAgentPane(actionId: ProviderActionId, intent: AgentEnsurePaneIntent): Promise<HerdrEffectReceipt> {
    if (intent.identity.projectId !== this.#options.projectId || intent.identity.projectSessionId !== this.#options.projectSessionId) {
      throw new TypeError("Herdr agent-pane intent is bound to another project session");
    }
    const snapshot = await this.#snapshot();
    let paneRef: string | null;
    let identityEvidence: "provider-session-ref-only" | "observer-presence-only";
    if (intent.surface === "provider-tui") {
      paneRef = findProviderSessionPane(snapshot, intent.identity.providerSessionRef);
      if (paneRef === null) throw new TypeError("Herdr has no pane for the exact provider session reference");
      identityEvidence = "provider-session-ref-only";
      this.#presenceKinds.set(intent.identity.agentId, "provider-session");
    } else {
      if (
        this.#options.observerExecutable === undefined || this.#options.observerSocketPath === undefined ||
        this.#options.observerCapabilityFile === undefined || this.#options.observerCursorDirectory === undefined
      ) throw new TypeError("Herdr observer pane command is not configured");
      const name = `fabric-observer-${createHash("sha256").update(`${intent.identity.coordinationRunId}\0${intent.identity.agentId}`).digest("hex").slice(0, 16)}`;
      this.#observerNames.set(intent.identity.agentId, name);
      paneRef = findNamedPane(snapshot, name, this.#options.canonicalProjectRoot);
      if (paneRef === null) {
        await this.#options.verifyExecutable?.(this.#options.observerExecutable);
        const result = await this.#run([
          "agent", "start", name,
          "--cwd", this.#options.canonicalProjectRoot,
          "--no-focus",
          "--",
          this.#options.observerExecutable,
          "observe",
          "--socket", this.#options.observerSocketPath,
          "--capability-file", this.#options.observerCapabilityFile,
          "--run-id", intent.identity.coordinationRunId,
          "--cursor", join(
            this.#options.observerCursorDirectory,
            `observer-${createHash("sha256").update(`${intent.identity.coordinationRunId}\0${intent.identity.agentId}`).digest("hex").slice(0, 16)}.json`,
          ),
          "--interval-ms", "1000",
        ], 10_000, 262_144);
        const created = paneIdentityFromResult(result);
        paneRef = created.paneRef;
        if (created.tabId !== null) this.#paneTabs.set(paneRef, created.tabId);
      }
      identityEvidence = "observer-presence-only";
      this.#presenceKinds.set(intent.identity.agentId, "observer");
    }
    this.#expectedAgents.set(intent.identity.agentId, intent.identity);
    this.#agentPanes.set(intent.identity.agentId, paneRef);
    const paneTab = findPaneTab(snapshot, paneRef);
    if (paneTab !== null) this.#paneTabs.set(paneRef, paneTab);
    const receipt: HerdrEffectReceipt = {
      status: "applied",
      operation: "agent.ensure-pane",
      paneRef: paneRef as never,
      detail: {
        identityEvidence,
        readiness: "identity-unverified",
      },
    };
    await this.#options.effectJournal.record(actionId, receipt);
    return receipt;
  }
  async arrangePanes(actionId: ProviderActionId, intent: ArrangePanesIntent): Promise<HerdrEffectReceipt> {
    const paneRefs = [...intent.paneRefs];
    if (paneRefs.length < 1 || paneRefs.length > 16 || new Set(paneRefs).size !== paneRefs.length) {
      throw new TypeError("Herdr pane arrangement must contain 1-16 unique panes");
    }
    const bound = new Set([...(this.#consolePane === null ? [] : [this.#consolePane]), ...this.#agentPanes.values()]);
    if (paneRefs.some((paneRef) => !validPaneId(paneRef) || !bound.has(paneRef))) {
      throw new TypeError("Herdr pane arrangement contains an unbound pane");
    }
    if (intent.layout === "side-by-side" && paneRefs.length > 1) {
      const anchor = paneRefs[0] as string;
      const tabId = this.#paneTabs.get(anchor);
      if (tabId === undefined) throw new TypeError("Herdr pane arrangement anchor has no structured tab identity");
      for (const paneRef of paneRefs.slice(1)) {
        await this.#run([
          "pane", "move", paneRef,
          "--tab", tabId,
          "--split", "right",
          "--target-pane", anchor,
          "--ratio", "0.5",
          "--no-focus",
        ], 10_000, 262_144);
      }
    }
    const receipt: HerdrEffectReceipt = {
      status: "applied",
      operation: "panes.arrange",
      detail: { layout: intent.layout, paneCount: paneRefs.length },
    };
    await this.#options.effectJournal.record(actionId, receipt);
    return receipt;
  }

  async projectAgentMetadata(actionId: ProviderActionId, intent: ProjectAgentMetadataIntent): Promise<HerdrEffectReceipt> {
    assertBoundAgentPane(this.#agentPanes, intent.agentId, intent.paneRef);
    boundedText(intent.metadata.provider, "agent metadata provider", 128);
    boundedText(intent.metadata.modelFamily, "agent metadata model family", 128);
    boundedText(intent.metadata.lifecycle, "agent metadata lifecycle", 128);
    if (Buffer.byteLength(intent.metadata.taskLabel, "utf8") > 512) throw new TypeError("agent metadata task label exceeds its bound");
    const title = `${intent.metadata.role}: ${intent.metadata.taskLabel}`;
    const displayAgent = `${intent.metadata.provider}/${intent.metadata.modelFamily}`;
    const customStatus = `${intent.metadata.lifecycle} context=${intent.metadata.contextPressure}`;
    await this.#run([
      "pane", "report-metadata", intent.paneRef,
      "--source", "agent-fabric",
      "--agent", intent.agentId,
      "--title", title,
      "--display-agent", displayAgent,
      "--custom-status", customStatus,
      "--ttl-ms", "300000",
    ], 5_000, 262_144);
    const receipt: HerdrEffectReceipt = {
      status: "applied",
      operation: "agent.project-metadata",
      paneRef: intent.paneRef,
      detail: { authority: "fabric", presenceOnly: true },
    };
    await this.#options.effectJournal.record(actionId, receipt);
    return receipt;
  }

  async projectAttention(actionId: ProviderActionId, intent: ProjectAttentionIntent): Promise<HerdrEffectReceipt> {
    if (intent.projectId !== this.#options.projectId || intent.projectSessionId !== this.#options.projectSessionId) {
      throw new TypeError("Herdr attention intent is bound to another project session");
    }
    if (this.#consolePane === null) throw new TypeError("Herdr Console pane is not registered");
    boundedText(intent.itemId, "attention item", 128);
    boundedText(intent.title, "attention title", 512);
    if (!Number.isSafeInteger(intent.revision) || intent.revision < 0) throw new TypeError("attention revision is invalid");
    await this.#run([
      "pane", "report-metadata", this.#consolePane,
      "--source", "agent-fabric",
      "--title", `[${intent.label}] ${intent.title}`,
      "--custom-status", `attention=${intent.itemId} revision=${String(intent.revision)}`,
      "--ttl-ms", "300000",
    ], 5_000, 262_144);
    const receipt: HerdrEffectReceipt = {
      status: "applied",
      operation: "attention.project",
      paneRef: this.#consolePane as never,
      detail: { authoritative: false, itemId: intent.itemId, revision: intent.revision },
    };
    await this.#options.effectJournal.record(actionId, receipt);
    return receipt;
  }

  async focusTarget(actionId: ProviderActionId, intent: FocusTargetIntent): Promise<HerdrEffectReceipt> {
    if (intent.target.kind === "console-item") {
      throw new TypeError("exact Console-item focus is unavailable without a contract-tested deep link");
    }
    assertBoundAgentPane(this.#agentPanes, intent.target.agentId, intent.target.paneRef);
    await this.#run(["agent", "focus", intent.target.paneRef], 5_000, 262_144);
    const receipt: HerdrEffectReceipt = {
      status: "applied",
      operation: "target.focus",
      paneRef: intent.target.paneRef,
      detail: { target: "agent-pane" },
    };
    await this.#options.effectJournal.record(actionId, receipt);
    return receipt;
  }
  async wakeAgent(actionId: ProviderActionId, intent: WakeAgentIntent): Promise<HerdrEffectReceipt> {
    assertBoundAgentPane(this.#agentPanes, intent.agentId, intent.paneRef);
    await this.#run(["agent", "focus", intent.paneRef], 5_000, 262_144);
    const receipt: HerdrEffectReceipt = {
      status: "applied",
      operation: "agent.wake",
      paneRef: intent.paneRef,
      detail: { deliveryEvidence: "none", signal: "focus-only" },
    };
    await this.#options.effectJournal.record(actionId, receipt);
    return receipt;
  }
  async showNotification(actionId: ProviderActionId, intent: ShowNotificationIntent): Promise<HerdrEffectReceipt> {
    if (intent.focusTarget !== null) {
      throw new TypeError("actionable Herdr notifications are unavailable without a contract-tested Console deep link");
    }
    boundedText(intent.attentionItemId, "notification attention item", 128);
    boundedText(intent.title, "notification title", 256);
    boundedText(intent.body, "notification body", 1_024);
    if (!Number.isSafeInteger(intent.attentionRevision) || intent.attentionRevision < 0) {
      throw new TypeError("notification attention revision is invalid");
    }
    const result = await this.#run([
      "notification", "show", intent.title,
      "--body", intent.body,
      "--position", "top-right",
      "--sound", "request",
    ], 5_000, 262_144);
    const reason = result.reason;
    if (
      !exactKeys(result, ["reason", "shown", "type"]) || result.type !== "notification_show" ||
      typeof result.shown !== "boolean" ||
      (reason !== "shown" && reason !== "disabled" && reason !== "rate_limited" && reason !== "no_foreground_client" && reason !== "busy") ||
      result.shown !== (reason === "shown")
    ) {
      throw new TypeError("Herdr notification response reason is invalid");
    }
    const receipt: HerdrEffectReceipt = {
      status: "applied",
      operation: "notification.show",
      detail: {
        reason,
        authoritative: false,
        attentionItemId: intent.attentionItemId,
        attentionRevision: intent.attentionRevision,
      },
    };
    await this.#options.effectJournal.record(actionId, receipt);
    return receipt;
  }
  async injectDirectSteer(actionId: ProviderActionId, intent: DirectSteerIntent): Promise<void> {
    assertBoundAgentPane(this.#agentPanes, intent.targetAgentId, intent.paneRef);
    const identity = this.#expectedAgents.get(intent.targetAgentId);
    if (
      identity === undefined || intent.reference.projectId !== identity.projectId ||
      intent.reference.projectSessionId !== identity.projectSessionId ||
      intent.reference.coordinationRunId !== identity.coordinationRunId
    ) throw new TypeError("Herdr direct steer reference is bound to another Fabric target");
    const promptBytes = Buffer.byteLength(intent.prompt, "utf8");
    if (promptBytes < 1 || promptBytes > 4_096 || /[\u0000-\u0008\u000b\u000c\u000e-\u001f\u007f\u009b]/u.test(intent.prompt) || CREDENTIAL_PATTERN.test(intent.prompt)) {
      throw new TypeError("Herdr direct steer prompt is unsafe or exceeds its bound");
    }
    await this.#run(["pane", "run", intent.paneRef, intent.prompt], 5_000, 262_144);
    await delay(150);
    await this.#run(["pane", "send-keys", intent.paneRef, "enter"], 5_000, 262_144);
    await this.#options.effectJournal.record(actionId, {
      status: "dispatched-unconfirmed",
      operation: "steer.inject-fire-and-forget",
      referenceValidation: "verified",
      deliveryEvidence: "none",
      canSatisfyExpectedResult: false,
      canCloseBarrier: false,
    });
  }
  async observeAgent(agentId: AgentId): Promise<HerdrPaneObservation> {
    const observedAt = parseTimestamp(new Date((this.#options.clock ?? Date.now)()).toISOString(), "herdrPresence.observedAt");
    const identity = this.#expectedAgents.get(agentId);
    if (identity === undefined) {
      return { state: "unavailable", observedAt, reason: "agent has no Fabric-bound Herdr presence registration" };
    }
    try {
      const snapshot = await this.#snapshotForPresence();
      const paneRef = this.#presenceKinds.get(agentId) === "observer"
        ? findNamedPane(snapshot, this.#observerNames.get(agentId) ?? "", this.#options.canonicalProjectRoot)
        : findProviderSessionPane(snapshot, identity.providerSessionRef);
      if (paneRef === null) return { state: "absent", observedAt, reason: "exact provider-session pane is absent" };
      this.#agentPanes.set(agentId, paneRef);
      const paneTab = findPaneTab(snapshot, paneRef);
      if (paneTab !== null) this.#paneTabs.set(paneRef, paneTab);
      return { state: "present", paneRef: paneRef as never, observedAt, identity: null };
    } catch {
      return { state: "unavailable", observedAt, reason: "Herdr structured presence is unavailable" };
    }
  }

  async #snapshot(): Promise<Record<string, unknown>> {
    const result = await this.#run(["api", "snapshot"], 5_000, 1_048_576);
    const snapshot = exactKeys(result, ["snapshot", "type"]) && result.type === "session_snapshot" && isRecord(result.snapshot)
      ? result.snapshot
      : null;
    if (
      snapshot === null || typeof snapshot.version !== "string" ||
      Buffer.byteLength(snapshot.version, "utf8") < 1 || Buffer.byteLength(snapshot.version, "utf8") > 256 ||
      snapshot.protocol !== this.#options.expectedProtocol ||
      !Array.isArray(snapshot.agents) || !Array.isArray(snapshot.panes) ||
      snapshot.agents.length > 256 || snapshot.panes.length > 256
    ) throw new TypeError("Herdr snapshot is malformed or incompatible");
    return snapshot;
  }

  async #snapshotForPresence(): Promise<Record<string, unknown>> {
    const now = performance.now();
    if (this.#presenceSnapshotCache !== null && this.#presenceSnapshotCache.expiresAt > now) {
      return await this.#presenceSnapshotCache.value;
    }
    const value = this.#snapshot();
    this.#presenceSnapshotCache = { expiresAt: now + 200, value };
    return await value;
  }

  async #run(arguments_: readonly string[], timeoutMs: number, maximumOutputBytes: number): Promise<Record<string, unknown>> {
    return parseCommandResponse(await this.#options.process.run({
      executable: this.#options.executable,
      arguments: arguments_,
      timeoutMs,
      maximumOutputBytes,
    }), maximumOutputBytes);
  }
}

function assertOptions(options: HerdrCliBoundaryOptions): void {
  for (const [label, path] of [
    ["Herdr executable", options.executable],
    ["project root", options.canonicalProjectRoot],
    ["Console executable", options.consoleExecutable],
  ] as const) {
    if (!isAbsolute(path) || path.includes("\0")) throw new TypeError(`${label} must be an absolute path`);
  }
  if (options.observerExecutable !== undefined && (!isAbsolute(options.observerExecutable) || options.observerExecutable.includes("\0"))) {
    throw new TypeError("observer executable must be an absolute path");
  }
  const observerPaths = [options.observerSocketPath, options.observerCapabilityFile, options.observerCursorDirectory];
  if (observerPaths.some((path) => (path === undefined) !== (options.observerExecutable === undefined))) {
    throw new TypeError("observer executable, socket, capability and cursor paths must be configured together");
  }
  for (const path of observerPaths) {
    if (path !== undefined && (!isAbsolute(path) || path.includes("\0"))) throw new TypeError("observer path must be absolute");
  }
  if (!Number.isSafeInteger(options.expectedProtocol) || options.expectedProtocol < 1) throw new TypeError("Herdr expected protocol is invalid");
}

function consoleName(projectId: string, projectSessionId: string): string {
  return `fabric-console-${createHash("sha256").update(`${projectId}\0${projectSessionId}`).digest("hex").slice(0, 16)}`;
}

function parseCommandResponse(bytes: Buffer, maximumBytes: number): Record<string, unknown> {
  if (bytes.length < 1 || bytes.length > maximumBytes) throw new TypeError("Herdr command response exceeds its bound");
  const value: unknown = JSON.parse(bytes.toString("utf8"));
  if (!isRecord(value) || !exactKeys(value, ["id", "result"]) || typeof value.id !== "string" || !isRecord(value.result)) {
    throw new TypeError("Herdr command response has an invalid closed envelope");
  }
  return value.result;
}

function findNamedPane(snapshot: Record<string, unknown>, name: string, canonicalProjectRoot: string): string | null {
  const candidates = [...(snapshot.agents as unknown[]), ...(snapshot.panes as unknown[])];
  for (const candidate of candidates) {
    if (!isRecord(candidate)) continue;
    if (
      (candidate.name === name || candidate.label === name) && candidate.cwd === canonicalProjectRoot &&
      validPaneId(candidate.pane_id)
    ) return candidate.pane_id;
  }
  return null;
}

function findProviderSessionPane(snapshot: Record<string, unknown>, providerSessionRef: string): string | null {
  const candidates = [...(snapshot.agents as unknown[]), ...(snapshot.panes as unknown[])];
  for (const candidate of candidates) {
    if (!isRecord(candidate) || !validPaneId(candidate.pane_id) || !isRecord(candidate.agent_session)) continue;
    const session = candidate.agent_session;
    if (
      exactKeys(session, ["agent", "kind", "source", "value"]) &&
      (session.kind === "id" || session.kind === "path") &&
      typeof session.source === "string" && typeof session.agent === "string" &&
      session.value === providerSessionRef
    ) return candidate.pane_id;
  }
  return null;
}

function findPaneTab(snapshot: Record<string, unknown>, paneRef: string): string | null {
  const candidates = [...(snapshot.agents as unknown[]), ...(snapshot.panes as unknown[])];
  for (const candidate of candidates) {
    if (!isRecord(candidate) || candidate.pane_id !== paneRef) continue;
    if (typeof candidate.tab_id === "string" && /^[A-Za-z0-9][A-Za-z0-9:._-]{0,127}$/u.test(candidate.tab_id)) return candidate.tab_id;
  }
  return null;
}

function paneIdentityFromResult(result: Record<string, unknown>): { paneRef: string; tabId: string | null } {
  if (
    result.type === "agent_started" && exactKeys(result, ["agent", "argv", "type"]) &&
    Array.isArray(result.argv) && result.argv.length <= 64 && result.argv.every((value) => typeof value === "string") &&
    isRecord(result.agent) && validPaneId(result.agent.pane_id)
  ) return {
    paneRef: result.agent.pane_id,
    tabId: validPaneId(result.agent.tab_id) ? result.agent.tab_id : null,
  };
  throw new TypeError("Herdr command did not return a bounded pane identity");
}

function validPaneId(value: unknown): value is string {
  return typeof value === "string" && /^[A-Za-z0-9][A-Za-z0-9:._-]{0,127}$/u.test(value) && !CREDENTIAL_PATTERN.test(value);
}

function assertBoundAgentPane(panes: ReadonlyMap<AgentId, string>, agentId: AgentId, paneRef: string): void {
  if (!validPaneId(paneRef) || panes.get(agentId) !== paneRef) {
    throw new TypeError("Herdr pane is not bound to the exact Fabric agent");
  }
}

function boundedText(value: string, label: string, maximumBytes: number): void {
  const bytes = Buffer.byteLength(value, "utf8");
  if (bytes < 1 || bytes > maximumBytes || /[\u0000-\u0008\u000b\u000c\u000e-\u001f\u007f\u009b]/u.test(value) || CREDENTIAL_PATTERN.test(value)) {
    throw new TypeError(`${label} is unsafe or exceeds its bound`);
  }
}


function isRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === "object" && value !== null && !Array.isArray(value);
}

function exactKeys(value: Record<string, unknown>, expected: readonly string[]): boolean {
  const actual = Object.keys(value).sort();
  const sorted = [...expected].sort();
  return actual.length === sorted.length && actual.every((key, index) => key === sorted[index]);
}
