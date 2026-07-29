import { createHash } from "node:crypto";
import { mkdtemp, realpath, rm } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";

import type {
  AgentId,
  CoordinationRunId,
  ProjectId,
  ProjectSessionId,
  ProviderActionId,
  ProviderSessionRef,
} from "@local/agent-fabric-protocol";
import { describe, expect, it, vi } from "vitest";

import type { AgentEnsurePaneIntent, ConsoleEnsurePaneIntent } from "../src/contracts.js";
import { HerdrEffectEvidenceJournal } from "../src/effect-journal.js";
import { HerdrCliBoundary, type HerdrCommandRequest } from "../src/native-boundary.js";

describe("sealed Herdr CLI boundary", () => {
  it("ensures one project-bound Console pane and journals only bounded effect evidence", async () => {
    const root = await realpath(await mkdtemp(join(tmpdir(), "fabric-herdr-native-")));
    const state = join(root, "state");
    const project = join(root, "project");
    await Promise.all([
      import("node:fs/promises").then(({ mkdir }) => mkdir(state, { mode: 0o700 })),
      import("node:fs/promises").then(({ mkdir }) => mkdir(project, { mode: 0o700 })),
    ]);
    const actionId = "herdr-console-01" as ProviderActionId;
    const intent: ConsoleEnsurePaneIntent = {
      kind: "console.ensure-pane",
      projectId: "project-01" as ProjectId,
      projectSessionId: "session-01" as ProjectSessionId,
      profileId: "agent-fabric-console",
    };
    const calls: HerdrCommandRequest[] = [];
    const process = {
      run: async (request: HerdrCommandRequest): Promise<Buffer> => {
        calls.push(request);
        if (request.arguments[0] === "api") {
          return response({ snapshot: { version: "fixture-version", protocol: 16, agents: [], panes: [] } });
        }
        return response({ agent: { pane_id: "w5:p9" } });
      },
    };
    const journal = new HerdrEffectEvidenceJournal({ stateDirectory: state });
    const boundary = new HerdrCliBoundary({
      executable: "/opt/homebrew/bin/herdr",
      expectedProtocol: 16,
      projectId: intent.projectId,
      projectSessionId: intent.projectSessionId,
      canonicalProjectRoot: project,
      consoleExecutable: "/opt/agent-fabric/runtime/agent-fabric-console/dist/cli.js",
      process,
      effectJournal: journal,
    });

    const receipt = await boundary.ensureConsolePane(actionId, intent);

    const name = `fabric-console-${createHash("sha256").update(`${intent.projectId}\0${intent.projectSessionId}`).digest("hex").slice(0, 16)}`;
    expect(receipt).toEqual({
      status: "applied",
      operation: "console.ensure-pane",
      paneRef: "w5:p9",
      detail: { identityEvidence: "pane-presence-only" },
    });
    expect(calls).toEqual([
      {
        executable: "/opt/homebrew/bin/herdr",
        arguments: ["api", "snapshot"],
        timeoutMs: 5_000,
        maximumOutputBytes: 1_048_576,
      },
      {
        executable: "/opt/homebrew/bin/herdr",
        arguments: [
          "agent", "start", name,
          "--cwd", project,
          "--no-focus",
          "--",
          "/opt/agent-fabric/runtime/agent-fabric-console/dist/cli.js",
          "--project", project,
          "--herdr",
        ],
        timeoutMs: 10_000,
        maximumOutputBytes: 262_144,
      },
    ]);
    await expect(journal.lookupAction(actionId)).resolves.toEqual({ status: "observed", receipt });
    await expect(boundary.lookupAction(actionId)).resolves.toEqual({ status: "observed", receipt });
    expect(JSON.stringify(receipt)).not.toContain(project);
    await rm(root, { recursive: true, force: true });
  });

  it("uses structured provider-session presence without treating a pane as full Fabric identity", async () => {
    const root = await realpath(await mkdtemp(join(tmpdir(), "fabric-herdr-presence-")));
    const { mkdir } = await import("node:fs/promises");
    const state = join(root, "state");
    const project = join(root, "project");
    await mkdir(state, { mode: 0o700 });
    await mkdir(project, { mode: 0o700 });
    const intent: AgentEnsurePaneIntent = {
      kind: "agent.ensure-pane",
      identity: {
        projectId: "project-01" as ProjectId,
        projectSessionId: "session-01" as ProjectSessionId,
        coordinationRunId: "run-01" as CoordinationRunId,
        agentId: "agent-peer" as AgentId,
        provider: "codex",
        modelFamily: "openai",
        providerSessionRef: "thread-01" as ProviderSessionRef,
        providerSessionGeneration: 3,
      },
      paneClass: "paired-primary",
      surface: "provider-tui",
      placement: "beside-chair",
    };
    const process = {
      run: async (): Promise<Buffer> => response({
        snapshot: {
          version: "fixture-version",
          protocol: 16,
          agents: [{
            agent: "codex",
            cwd: project,
            agent_session: { source: "herdr:codex", agent: "codex", kind: "id", value: "thread-01" },
            pane_id: "w5:p7",
          }],
          panes: [],
        },
      }),
    };
    const journal = new HerdrEffectEvidenceJournal({ stateDirectory: state });
    const boundary = new HerdrCliBoundary({
      executable: "/opt/homebrew/bin/herdr",
      expectedProtocol: 16,
      projectId: intent.identity.projectId,
      projectSessionId: intent.identity.projectSessionId,
      canonicalProjectRoot: project,
      consoleExecutable: "/opt/agent-fabric/runtime/agent-fabric-console/dist/cli.js",
      process,
      effectJournal: journal,
      clock: () => Date.parse("2027-01-01T00:00:00Z"),
    });

    const receipt = await boundary.ensureAgentPane("herdr-agent-01" as ProviderActionId, intent);
    const observation = await boundary.observeAgent(intent.identity.agentId);

    expect(receipt).toEqual({
      status: "applied",
      operation: "agent.ensure-pane",
      paneRef: "w5:p7",
      detail: {
        identityEvidence: "provider-session-ref-only",
        readiness: "identity-unverified",
      },
    });
    expect(observation).toEqual({
      state: "present",
      paneRef: "w5:p7",
      observedAt: "2027-01-01T00:00:00.000Z",
      identity: null,
    });
    await rm(root, { recursive: true, force: true });
  });

  it("restores a persisted Fabric presence registration after daemon restart without replaying pane creation", async () => {
    const root = await realpath(await mkdtemp(join(tmpdir(), "fabric-herdr-presence-restart-")));
    const { mkdir } = await import("node:fs/promises");
    const state = join(root, "state");
    const project = join(root, "project");
    await mkdir(state, { mode: 0o700 });
    await mkdir(project, { mode: 0o700 });
    const intent: AgentEnsurePaneIntent = {
      kind: "agent.ensure-pane",
      identity: {
        projectId: "project-01" as ProjectId,
        projectSessionId: "session-01" as ProjectSessionId,
        coordinationRunId: "run-01" as CoordinationRunId,
        agentId: "agent-peer" as AgentId,
        provider: "codex",
        modelFamily: "openai",
        providerSessionRef: "thread-01" as ProviderSessionRef,
        providerSessionGeneration: 3,
      },
      paneClass: "paired-primary",
      surface: "provider-tui",
      placement: "beside-chair",
    };
    const calls: HerdrCommandRequest[] = [];
    const boundary = new HerdrCliBoundary({
      executable: "/opt/homebrew/bin/herdr",
      expectedProtocol: 16,
      projectId: intent.identity.projectId,
      projectSessionId: intent.identity.projectSessionId,
      canonicalProjectRoot: project,
      consoleExecutable: "/opt/agent-fabric/runtime/agent-fabric-console/dist/cli.js",
      process: {
        run: async (request) => {
          calls.push(request);
          return response({
            snapshot: {
              version: "fixture-version",
              protocol: 16,
              agents: [{
                agent: "codex",
                cwd: project,
                agent_session: { source: "herdr:codex", agent: "codex", kind: "id", value: "thread-01" },
                pane_id: "w5:p7",
              }],
              panes: [],
            },
          });
        },
      },
      effectJournal: new HerdrEffectEvidenceJournal({ stateDirectory: state }),
      clock: () => Date.parse("2027-01-01T00:00:00Z"),
    });

    (boundary as unknown as { restorePresenceRegistration(value: AgentEnsurePaneIntent): void })
      .restorePresenceRegistration(intent);

    await expect(boundary.observeAgent(intent.identity.agentId)).resolves.toMatchObject({
      state: "present",
      paneRef: "w5:p7",
    });
    await expect(boundary.observeAgent(intent.identity.agentId)).resolves.toMatchObject({
      state: "present",
      paneRef: "w5:p7",
    });
    expect(calls.map((call) => call.arguments)).toEqual([["api", "snapshot"]]);
    await rm(root, { recursive: true, force: true });
  });

  it("restores a persisted Console pane binding from structured presence without replaying launch", async () => {
    const root = await realpath(await mkdtemp(join(tmpdir(), "fabric-herdr-console-restart-")));
    const { mkdir } = await import("node:fs/promises");
    const state = join(root, "state");
    const project = join(root, "project");
    await mkdir(state, { mode: 0o700 });
    await mkdir(project, { mode: 0o700 });
    const name = `fabric-console-${createHash("sha256").update("project-01\0session-01").digest("hex").slice(0, 16)}`;
    const calls: HerdrCommandRequest[] = [];
    const boundary = new HerdrCliBoundary({
      executable: "/opt/homebrew/bin/herdr",
      expectedProtocol: 16,
      projectId: "project-01",
      projectSessionId: "session-01",
      canonicalProjectRoot: project,
      consoleExecutable: "/opt/agent-fabric/runtime/agent-fabric-console/dist/cli.js",
      process: {
        run: async (request) => {
          calls.push(request);
          return response({
            snapshot: {
              version: "fixture-version",
              protocol: 16,
              agents: [{ name, cwd: project, pane_id: "w5:p9", tab_id: "w5:t2" }],
              panes: [],
            },
          });
        },
      },
      effectJournal: new HerdrEffectEvidenceJournal({ stateDirectory: state }),
    });

    await (boundary as unknown as {
      restoreConsolePresenceRegistration(value: ConsoleEnsurePaneIntent): Promise<void>;
    }).restoreConsolePresenceRegistration({
      kind: "console.ensure-pane",
      projectId: "project-01" as ProjectId,
      projectSessionId: "session-01" as ProjectSessionId,
      profileId: "agent-fabric-console",
    });
    await expect(boundary.projectAttention("attention-after-restart" as ProviderActionId, {
      kind: "attention.project",
      projectId: "project-01" as ProjectId,
      projectSessionId: "session-01" as ProjectSessionId,
      itemId: "attention-01",
      revision: 1,
      label: "FYI",
      title: "Restored",
    })).resolves.toMatchObject({ status: "applied", paneRef: "w5:p9" });
    expect(calls[0]?.arguments).toEqual(["api", "snapshot"]);
    expect(calls.some((call) => call.arguments[0] === "agent" && call.arguments[1] === "start")).toBe(false);
    await rm(root, { recursive: true, force: true });
  });

  it("uses fixed wake and validated fire-and-forget injection commands without claiming delivery", async () => {
    const root = await realpath(await mkdtemp(join(tmpdir(), "fabric-herdr-input-")));
    const { mkdir } = await import("node:fs/promises");
    const state = join(root, "state");
    const project = join(root, "project");
    await mkdir(state, { mode: 0o700 });
    await mkdir(project, { mode: 0o700 });
    const identity: AgentEnsurePaneIntent["identity"] = {
      projectId: "project-01" as ProjectId,
      projectSessionId: "session-01" as ProjectSessionId,
      coordinationRunId: "run-01" as CoordinationRunId,
      agentId: "agent-peer" as AgentId,
      provider: "codex",
      modelFamily: "openai",
      providerSessionRef: "thread-01" as ProviderSessionRef,
      providerSessionGeneration: 3,
    };
    const calls: HerdrCommandRequest[] = [];
    const process = {
      run: async (request: HerdrCommandRequest): Promise<Buffer> => {
        calls.push(request);
        if (request.arguments[0] === "api") {
          return response({ snapshot: { version: "fixture-version", protocol: 16, agents: [{
            agent: "codex",
            cwd: project,
            agent_session: { source: "herdr:codex", agent: "codex", kind: "id", value: "thread-01" },
            pane_id: "w5:p7",
          }], panes: [] } });
        }
        return response({ accepted: true });
      },
    };
    const journal = new HerdrEffectEvidenceJournal({ stateDirectory: state });
    const boundary = new HerdrCliBoundary({
      executable: "/opt/homebrew/bin/herdr",
      expectedProtocol: 16,
      projectId: identity.projectId,
      projectSessionId: identity.projectSessionId,
      canonicalProjectRoot: project,
      consoleExecutable: "/opt/agent-fabric/runtime/agent-fabric-console/dist/cli.js",
      process,
      effectJournal: journal,
    });
    await boundary.ensureAgentPane("bind-agent-01" as ProviderActionId, {
      kind: "agent.ensure-pane",
      identity,
      paneClass: "paired-primary",
      surface: "provider-tui",
      placement: "beside-chair",
    });
    calls.length = 0;

    const wake = await boundary.wakeAgent("wake-agent-01" as ProviderActionId, {
      kind: "agent.wake",
      agentId: identity.agentId,
      paneRef: "w5:p7" as never,
    });
    vi.useFakeTimers();
    try {
      const steering = boundary.injectDirectSteer("steer-agent-01" as ProviderActionId, {
        kind: "steer.inject-fire-and-forget",
        targetAgentId: identity.agentId,
        paneRef: "w5:p7" as never,
        reference: {
          kind: "task",
          projectId: identity.projectId,
          projectSessionId: identity.projectSessionId,
          coordinationRunId: identity.coordinationRunId,
          taskId: "task-01" as never,
          expectedRevision: 2,
        },
        validatedReferenceDigest: `sha256:${"a".repeat(64)}` as never,
        prompt: "Pause after the current check.",
      });
      await vi.advanceTimersByTimeAsync(149);
      expect(calls.map((call) => call.arguments)).toEqual([
        ["agent", "focus", "w5:p7"],
        ["pane", "run", "w5:p7", "Pause after the current check."],
      ]);
      await vi.advanceTimersByTimeAsync(1);
      await steering;
    } finally {
      vi.useRealTimers();
    }

    expect(wake).toEqual({
      status: "applied",
      operation: "agent.wake",
      paneRef: "w5:p7",
      detail: { deliveryEvidence: "none", signal: "focus-only" },
    });
    expect(calls.map((call) => call.arguments)).toEqual([
      ["agent", "focus", "w5:p7"],
      ["pane", "run", "w5:p7", "Pause after the current check."],
      ["pane", "send-keys", "w5:p7", "enter"],
    ]);
    await expect(journal.lookupAction("steer-agent-01" as ProviderActionId)).resolves.toEqual({
      status: "observed",
      receipt: {
        status: "dispatched-unconfirmed",
        operation: "steer.inject-fire-and-forget",
        referenceValidation: "verified",
        deliveryEvidence: "none",
        canSatisfyExpectedResult: false,
        canCloseBarrier: false,
      },
    });
    await rm(root, { recursive: true, force: true });
  });

  it("projects a bounded non-authoritative native notification through fixed Herdr arguments", async () => {
    const root = await realpath(await mkdtemp(join(tmpdir(), "fabric-herdr-notification-")));
    const { mkdir } = await import("node:fs/promises");
    const state = join(root, "state");
    const project = join(root, "project");
    await mkdir(state, { mode: 0o700 });
    await mkdir(project, { mode: 0o700 });
    const calls: HerdrCommandRequest[] = [];
    const journal = new HerdrEffectEvidenceJournal({ stateDirectory: state });
    const boundary = new HerdrCliBoundary({
      executable: "/opt/homebrew/bin/herdr",
      expectedProtocol: 16,
      projectId: "project-01",
      projectSessionId: "session-01",
      canonicalProjectRoot: project,
      consoleExecutable: "/opt/agent-fabric/runtime/agent-fabric-console/dist/cli.js",
      process: {
        run: async (request) => {
          calls.push(request);
          return response({ reason: "shown" });
        },
      },
      effectJournal: journal,
    });

    const receipt = await boundary.showNotification("notify-01" as ProviderActionId, {
      kind: "notification.show",
      attentionItemId: "attention-01",
      attentionRevision: 3,
      title: "Approval required",
      body: "Review the exact Console item.",
      focusTarget: null,
    });

    expect(calls.map((call) => call.arguments)).toEqual([[
      "notification", "show", "Approval required",
      "--body", "Review the exact Console item.",
      "--position", "top-right",
      "--sound", "request",
    ]]);
    expect(receipt).toEqual({
      status: "applied",
      operation: "notification.show",
      detail: {
        reason: "shown",
        authoritative: false,
        attentionItemId: "attention-01",
        attentionRevision: 3,
      },
    });
    await rm(root, { recursive: true, force: true });
  });

  it("uses fixed pane arrangement, metadata, attention and focus operations", async () => {
    const root = await realpath(await mkdtemp(join(tmpdir(), "fabric-herdr-control-")));
    const { mkdir } = await import("node:fs/promises");
    const state = join(root, "state");
    const project = join(root, "project");
    await mkdir(state, { mode: 0o700 });
    await mkdir(project, { mode: 0o700 });
    const consoleName = `fabric-console-${createHash("sha256").update("project-01\0session-01").digest("hex").slice(0, 16)}`;
    const snapshot = {
      version: "fixture-version",
      protocol: 16,
      agents: [
        { name: consoleName, cwd: "/another/project", pane_id: "w9:p9", tab_id: "w9:t1" },
        {
          agent: "codex",
          cwd: project,
          agent_session: { source: "herdr:codex", agent: "codex", kind: "id", value: "thread-01" },
          pane_id: "w5:p7",
          tab_id: "w5:t2",
        },
      ],
      panes: [],
    };
    const calls: HerdrCommandRequest[] = [];
    const boundary = new HerdrCliBoundary({
      executable: "/opt/homebrew/bin/herdr",
      expectedProtocol: 16,
      projectId: "project-01",
      projectSessionId: "session-01",
      canonicalProjectRoot: project,
      consoleExecutable: "/opt/agent-fabric/runtime/agent-fabric-console/dist/cli.js",
      process: {
        run: async (request) => {
          calls.push(request);
          if (request.arguments[0] === "api") return response({ snapshot });
          if (request.arguments[0] === "agent" && request.arguments[1] === "start") {
            expect(request.arguments[2]).toBe(consoleName);
            return response({
              type: "agent_started",
              agent: { pane_id: "w5:p9", tab_id: "w5:t2" },
              argv: request.arguments.slice(7),
            });
          }
          return response({ accepted: true });
        },
      },
      effectJournal: new HerdrEffectEvidenceJournal({ stateDirectory: state }),
    });
    await boundary.ensureConsolePane("bind-console-01" as ProviderActionId, {
      kind: "console.ensure-pane",
      projectId: "project-01" as ProjectId,
      projectSessionId: "session-01" as ProjectSessionId,
      profileId: "agent-fabric-console",
    });
    await boundary.ensureAgentPane("bind-agent-control-01" as ProviderActionId, {
      kind: "agent.ensure-pane",
      identity: {
        projectId: "project-01" as ProjectId,
        projectSessionId: "session-01" as ProjectSessionId,
        coordinationRunId: "run-01" as CoordinationRunId,
        agentId: "agent-peer" as AgentId,
        provider: "codex",
        modelFamily: "openai",
        providerSessionRef: "thread-01" as ProviderSessionRef,
        providerSessionGeneration: 3,
      },
      paneClass: "paired-primary",
      surface: "provider-tui",
      placement: "beside-chair",
    });
    calls.length = 0;

    await boundary.arrangePanes("arrange-01" as ProviderActionId, {
      kind: "panes.arrange",
      paneRefs: ["w5:p9" as never, "w5:p7" as never],
      layout: "side-by-side",
    });
    await boundary.projectAgentMetadata("metadata-01" as ProviderActionId, {
      kind: "agent.project-metadata",
      agentId: "agent-peer" as AgentId,
      paneRef: "w5:p7" as never,
      metadata: {
        role: "worker",
        provider: "codex",
        modelFamily: "openai",
        taskLabel: "Review changes",
        lifecycle: "working",
        contextPressure: "low",
      },
    });
    await boundary.projectAttention("attention-01" as ProviderActionId, {
      kind: "attention.project",
      projectId: "project-01" as ProjectId,
      projectSessionId: "session-01" as ProjectSessionId,
      itemId: "gate-01",
      revision: 4,
      label: "Decision",
      title: "Choose recovery path",
    });
    await boundary.focusTarget("focus-01" as ProviderActionId, {
      kind: "target.focus",
      target: { kind: "agent-pane", agentId: "agent-peer" as AgentId, paneRef: "w5:p7" as never },
    });

    expect(calls.map((call) => call.arguments.slice(0, 3))).toEqual([
      ["pane", "move", "w5:p7"],
      ["pane", "report-metadata", "w5:p7"],
      ["pane", "report-metadata", "w5:p9"],
      ["agent", "focus", "w5:p7"],
    ]);
    expect(calls.flatMap((call) => call.arguments)).not.toContain("sh");
    await expect(boundary.focusTarget("focus-item-01" as ProviderActionId, {
      kind: "target.focus",
      target: { kind: "console-item", view: "attention", itemId: "gate-01", revision: 4 },
    })).rejects.toThrow("exact Console-item focus is unavailable");
    await rm(root, { recursive: true, force: true });
  });

  it("starts only the fixed read-only observer command when no provider TUI is requested", async () => {
    const root = await realpath(await mkdtemp(join(tmpdir(), "fabric-herdr-observer-")));
    const { mkdir } = await import("node:fs/promises");
    const state = join(root, "state");
    const project = join(root, "project");
    await mkdir(state, { mode: 0o700 });
    await mkdir(project, { mode: 0o700 });
    const calls: HerdrCommandRequest[] = [];
    const identity = {
      projectId: "project-01" as ProjectId,
      projectSessionId: "session-01" as ProjectSessionId,
      coordinationRunId: "run-01" as CoordinationRunId,
      agentId: "agent-observed" as AgentId,
      provider: "codex",
      modelFamily: "openai",
      providerSessionRef: "thread-observed" as ProviderSessionRef,
      providerSessionGeneration: 2,
    };
    const boundary = new HerdrCliBoundary({
      executable: "/opt/homebrew/bin/herdr",
      expectedProtocol: 16,
      projectId: identity.projectId,
      projectSessionId: identity.projectSessionId,
      canonicalProjectRoot: project,
      consoleExecutable: "/opt/agent-fabric/runtime/agent-fabric-console/dist/cli.js",
      observerExecutable: "/opt/agent-fabric/runtime/agent-fabric/dist/cli/main.js",
      observerSocketPath: join(root, "fabric.sock"),
      observerCapabilityFile: join(root, "observer.cap"),
      observerCursorDirectory: join(root, "cursors"),
      process: {
        run: async (request) => {
          calls.push(request);
          if (request.arguments[0] === "api") return response({ snapshot: { version: "fixture-version", protocol: 16, agents: [], panes: [] } });
          return response({ agent: { pane_id: "w5:pA" } });
        },
      },
      effectJournal: new HerdrEffectEvidenceJournal({ stateDirectory: state }),
    });

    const receipt = await boundary.ensureAgentPane("observer-01" as ProviderActionId, {
      kind: "agent.ensure-pane",
      identity,
      paneClass: "selected-long-running-worker",
      surface: "observer",
      placement: "workspace-default",
    });

    expect(receipt).toMatchObject({
      status: "applied",
      operation: "agent.ensure-pane",
      paneRef: "w5:pA",
      detail: { identityEvidence: "observer-presence-only", readiness: "identity-unverified" },
    });
    expect(calls.at(-1)?.arguments).toEqual([
      "agent", "start", expect.stringMatching(/^fabric-observer-[0-9a-f]{16}$/u),
      "--cwd", project,
      "--no-focus",
      "--",
      "/opt/agent-fabric/runtime/agent-fabric/dist/cli/main.js",
      "observe",
      "--socket", join(root, "fabric.sock"),
      "--capability-file", join(root, "observer.cap"),
      "--run-id", "run-01",
      "--cursor", expect.stringMatching(new RegExp(`^${join(root, "cursors").replaceAll("/", "\\/")}\\/observer-[0-9a-f]{16}\\.json$`, "u")),
      "--interval-ms", "1000",
    ]);
    await rm(root, { recursive: true, force: true });
  });

  it.each([
    ["malformed", Buffer.from("{}")],
    ["oversized", Buffer.alloc(1_048_577, 0x78)],
  ] as const)("rejects %s Herdr output without effect evidence", async (_case, output) => {
    const root = await realpath(await mkdtemp(join(tmpdir(), "fabric-herdr-invalid-")));
    const { mkdir } = await import("node:fs/promises");
    const state = join(root, "state");
    const project = join(root, "project");
    await mkdir(state, { mode: 0o700 });
    await mkdir(project, { mode: 0o700 });
    const journal = new HerdrEffectEvidenceJournal({ stateDirectory: state });
    const boundary = new HerdrCliBoundary({
      executable: "/opt/homebrew/bin/herdr",
      expectedProtocol: 16,
      projectId: "project-01",
      projectSessionId: "session-01",
      canonicalProjectRoot: project,
      consoleExecutable: "/opt/agent-fabric/runtime/agent-fabric-console/dist/cli.js",
      process: { run: async () => output },
      effectJournal: journal,
    });

    await expect(boundary.ensureConsolePane("invalid-output-01" as ProviderActionId, {
      kind: "console.ensure-pane",
      projectId: "project-01" as ProjectId,
      projectSessionId: "session-01" as ProjectSessionId,
      profileId: "agent-fabric-console",
    })).rejects.toThrow();
    await expect(journal.lookupAction("invalid-output-01" as ProviderActionId)).resolves.toEqual({ status: "unknown" });
    await rm(root, { recursive: true, force: true });
  });

  it("rejects a project-session retarget before contacting Herdr", async () => {
    const root = await realpath(await mkdtemp(join(tmpdir(), "fabric-herdr-retarget-")));
    const { mkdir } = await import("node:fs/promises");
    const state = join(root, "state");
    const project = join(root, "project");
    await mkdir(state, { mode: 0o700 });
    await mkdir(project, { mode: 0o700 });
    let calls = 0;
    const boundary = new HerdrCliBoundary({
      executable: "/opt/homebrew/bin/herdr",
      expectedProtocol: 16,
      projectId: "project-01",
      projectSessionId: "session-01",
      canonicalProjectRoot: project,
      consoleExecutable: "/opt/agent-fabric/runtime/agent-fabric-console/dist/cli.js",
      process: { run: async () => { calls += 1; return response({}); } },
      effectJournal: new HerdrEffectEvidenceJournal({ stateDirectory: state }),
    });

    await expect(boundary.ensureConsolePane("retarget-01" as ProviderActionId, {
      kind: "console.ensure-pane",
      projectId: "project-other" as ProjectId,
      projectSessionId: "session-01" as ProjectSessionId,
      profileId: "agent-fabric-console",
    })).rejects.toThrow("another project session");
    expect(calls).toBe(0);
    await rm(root, { recursive: true, force: true });
  });
});

function response(result: unknown): Buffer {
  if (typeof result === "object" && result !== null && !Array.isArray(result)) {
    const record = result as Record<string, unknown>;
    if (record.snapshot !== undefined) result = { type: "session_snapshot", ...record };
    else if (record.agent !== undefined) result = { type: "agent_started", argv: [], ...record };
    else if (record.reason !== undefined) result = { type: "notification_show", shown: record.reason === "shown", ...record };
    else result = { type: "ok", ...record };
  }
  return Buffer.from(JSON.stringify({ id: "fixture", result }));
}
