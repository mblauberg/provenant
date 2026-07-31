import { mkdtemp, readFile, rm, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { fileURLToPath } from "node:url";

import { describe, expect, it, vi } from "vitest";

import { AdapterSupervisor } from "../../src/adapters/supervisor.ts";
import { settle } from "../shared/deadline-wait.ts";

const fixturePath = fileURLToPath(new URL("../support/supervisor-fixture.ts", import.meta.url));
const ATTESTATION_CHALLENGE = "cd".repeat(32);
const EXPECTED_CHAIR_PRINCIPAL = {
  agentId: "chair",
  projectSessionId: "session-1",
  runId: "run-1",
  principalGeneration: 1,
} as const;

describe("persistent adapter supervision", () => {
  it("refuses a drifted npm install immediately before adapter process spawn", async () => {
    const directory = await mkdtemp(join(tmpdir(), "agent-fabric-supervisor-npm-attestation-"));
    const countPath = join(directory, "starts.txt");
    const verifyNpmInstall = vi.fn(async () => {
      throw new Error("NPM_INSTALL_ATTESTATION_MISMATCH: fixture drift");
    });
    const supervisor = new AdapterSupervisor({
      fake: {
        command: [process.execPath, "--import", "tsx", fixturePath],
        environment: { SUPERVISOR_COUNT_PATH: countPath },
        npmInstallProductRoot: "/fixture/product",
      },
    }, { verifyNpmInstall });
    try {
      await expect(supervisor.request("fake", "one", {})).rejects.toThrow("NPM_INSTALL_ATTESTATION_MISMATCH");
      expect(verifyNpmInstall).toHaveBeenCalledWith("/fixture/product");
      await expect(readFile(countPath, "utf8")).rejects.toMatchObject({ code: "ENOENT" });
    } finally {
      await supervisor.close();
      await rm(directory, { recursive: true, force: true });
    }
  });

  it("reuses one healthy adapter process across requests", async () => {
    const directory = await mkdtemp(join(tmpdir(), "agent-fabric-supervisor-"));
    const countPath = join(directory, "starts.txt");
    const supervisor = new AdapterSupervisor({ fake: { command: [process.execPath, "--import", "tsx", fixturePath], environment: { SUPERVISOR_COUNT_PATH: countPath } } });
    try {
      const first = await supervisor.request("fake", "one", {});
      const second = await supervisor.request("fake", "two", {});
      expect(second).toMatchObject({ method: "two", pid: (first as { pid: number }).pid });
      expect(await readFile(countPath, "utf8")).toBe("1");
    } finally {
      await supervisor.close();
      await rm(directory, { recursive: true, force: true });
    }
  });

  it("launches a chair through a typed private environment without serialising the handoff", async () => {
    const directory = await mkdtemp(join(tmpdir(), "agent-fabric-supervisor-chair-launch-"));
    const countPath = join(directory, "starts.txt");
    const observationPath = join(directory, "launch-observation.json");
    const supervisor = new AdapterSupervisor({
      fake: {
        command: [process.execPath, "--import", "tsx", fixturePath],
        environment: {
          SUPERVISOR_COUNT_PATH: countPath,
          SUPERVISOR_OBSERVATION_PATH: observationPath,
        },
      },
    });
    const request = {
      schemaVersion: 1 as const,
      actionId: "chair-launch-1",
      providerContractDigest: `sha256:${"b".repeat(64)}`,
      payload: { cwd: "/workspace/project", modelFamily: "fixture", model: "provider-test" },
    };
    const privateHandoff = {
      capability: "chair-capability-secret-canary",
      socketPath: "/private/agent-fabric.sock",
      attestationChallenge: ATTESTATION_CHALLENGE,
      expectedPrincipal: EXPECTED_CHAIR_PRINCIPAL,
    };
    try {
      const launchChair: unknown = Reflect.get(supervisor, "launchChair");
      expect(launchChair).toBeTypeOf("function");
      const launched = await Reflect.apply(launchChair as (...arguments_: unknown[]) => unknown, supervisor, [
          "fake",
          request,
          privateHandoff,
        ]);
      expect(launched).toMatchObject({
        resumeReference: "fixture-chair-session",
        providerSessionGeneration: 1,
        fabricContinuity: {
          schemaVersion: 1,
          kind: "provider-session-fabric-attestation",
          method: "provider-session-random-challenge-v1",
          bridgeContract: "agent-fabric-session-bridge-v1",
          providerAdapterId: "fake",
          providerActionId: request.actionId,
          providerContractDigest: request.providerContractDigest,
          providerSessionRef: "fixture-chair-session",
          providerSessionGeneration: 1,
          providerTurnRef: "fixture-provider-turn",
          providerInvocationRef: "fixture-provider-tool-call",
          challengeDigest: expect.stringMatching(/^sha256:/u),
          attestationDigest: expect.stringMatching(/^sha256:/u),
        },
      });
      expect(JSON.parse(await readFile(observationPath, "utf8"))).toEqual({
        privateCapabilityPresent: true,
        privateSocketPresent: true,
        privateChallengePresent: true,
        requestContainsPrivate: false,
      });
      await expect(supervisor.launchChair("fake", request, { ...privateHandoff })).rejects.toMatchObject({
        code: "PRIVATE_HANDOFF_UNAVAILABLE",
      });
      const liveLookup = await supervisor.request("fake", "lookup_action", { actionId: request.actionId });
      expect(liveLookup).toMatchObject({ method: "lookup_action", pid: expect.any(Number) });
      await expect(supervisor.request("fake", "dispatch", {
        actionId: "later-turn-missing-generation",
        operation: "send_turn",
        payload: { resumeReference: "fixture-chair-session", prompt: "must not dispatch" },
      })).rejects.toMatchObject({ code: "STALE_LEASE_GENERATION" });
      await expect(supervisor.request("fake", "dispatch", {
        actionId: "later-turn-stale-generation",
        operation: "send_turn",
        payload: {
          resumeReference: "fixture-chair-session",
          providerSessionGeneration: 2,
          prompt: "must not dispatch",
        },
      })).rejects.toMatchObject({ code: "STALE_LEASE_GENERATION" });
      await expect(supervisor.request("fake", "dispatch", {
        actionId: "later-turn-conflicting-generation",
        operation: "send_turn",
        providerSessionGeneration: 1,
        payload: {
          resumeReference: "fixture-chair-session",
          providerSessionGeneration: 2,
          prompt: "must not dispatch",
        },
      })).rejects.toMatchObject({ code: "STALE_LEASE_GENERATION" });
      await expect(supervisor.request("fake", "dispatch", {
        actionId: "later-turn-conflicting-reference",
        operation: "send_turn",
        resumeReference: "fixture-chair-session",
        payload: {
          resumeReference: "another-chair-session",
          providerSessionGeneration: 1,
          prompt: "must not dispatch",
        },
      })).rejects.toMatchObject({ code: "STALE_LEASE_GENERATION" });
      const laterTurn = await supervisor.request("fake", "dispatch", {
        actionId: "later-turn-1",
        operation: "send_turn",
        payload: {
          resumeReference: "fixture-chair-session",
          providerSessionGeneration: 1,
          prompt: "continue",
        },
      });
      expect(laterTurn).toMatchObject({ method: "dispatch", pid: (liveLookup as { pid: number }).pid });
      await expect(supervisor.request("fake", "dispatch", {
        actionId: "recoverable-provider-error",
        operation: "send_turn",
        payload: {
          resumeReference: "fixture-chair-session",
          providerSessionGeneration: 1,
          prompt: "provider rejects this turn",
        },
      })).rejects.toMatchObject({ name: "PROVIDER_TURN_FAILED" });
      await expect(supervisor.request("fake", "dispatch", {
        actionId: "later-turn-after-error",
        operation: "send_turn",
        payload: {
          resumeReference: "fixture-chair-session",
          providerSessionGeneration: 1,
          prompt: "bridge remains",
        },
      })).resolves.toMatchObject({ method: "dispatch" });
      await expect(supervisor.request("fake", "dispatch", {
        actionId: "release-chair-1",
        operation: "release",
        payload: { resumeReference: "fixture-chair-session", providerSessionGeneration: 1 },
      })).resolves.toMatchObject({ method: "dispatch" });
      await expect(supervisor.request("fake", "dispatch", {
        actionId: "later-turn-after-release",
        operation: "send_turn",
        payload: {
          resumeReference: "fixture-chair-session",
          providerSessionGeneration: 1,
          prompt: "must not reconstruct",
        },
      })).rejects.toMatchObject({ code: "CHAIR_BRIDGE_LOST" });
      await expect(supervisor.request("fake", "lookup_action", {
        actionId: request.actionId,
      })).rejects.toMatchObject({ code: "CHAIR_BRIDGE_LOST" });
      expect(await readFile(countPath, "utf8")).toBe("1");
    } finally {
      await supervisor.close();
      await rm(directory, { recursive: true, force: true });
    }
  });

  it("recovers and deeply retains an exact higher-generation chair bridge", async () => {
    const directory = await mkdtemp(join(tmpdir(), "agent-fabric-supervisor-chair-recovery-"));
    const countPath = join(directory, "starts.txt");
    const supervisor = new AdapterSupervisor({
      fake: {
        command: [process.execPath, "--import", "tsx", fixturePath],
        environment: { SUPERVISOR_COUNT_PATH: countPath },
      },
    });
    const principal = { ...EXPECTED_CHAIR_PRINCIPAL, principalGeneration: 2 } as const;
    try {
      await expect(supervisor.recoverChair("fake", {
        schemaVersion: 1,
        recoveryId: "chair-recovery-1",
        lossId: "chair-loss-1",
        actionId: "chair-recover-action-1",
        providerContractDigest: `sha256:${"7".repeat(64)}`,
        resumeReference: "fixture-recovered-chair-session",
        expectedProviderSessionGeneration: 1,
        nextProviderSessionGeneration: 2,
        bridgeGeneration: 2,
        payload: { cwd: "/workspace/project", modelFamily: "fixture", model: "provider-test" },
      }, {
        capability: "recovered-chair-capability-canary",
        socketPath: "/private/recovered-chair.sock",
        attestationChallenge: ATTESTATION_CHALLENGE,
        expectedPrincipal: principal,
      })).resolves.toMatchObject({
        resumeReference: "fixture-recovered-chair-session",
        providerSessionGeneration: 2,
      });
      expect(supervisor.hasRetainedChairBridge({
        ...principal,
        adapterId: "fake",
        actionId: "chair-recover-action-1",
        providerSessionRef: "fixture-recovered-chair-session",
        providerSessionGeneration: 2,
        bridgeGeneration: 2,
      })).toBe(true);
      expect(await readFile(countPath, "utf8")).toBe("1");
    } finally {
      await supervisor.close();
      await rm(directory, { recursive: true, force: true });
    }
  });

  it("rejects an explicit Codex recovery model before opening a transport", async () => {
    const directory = await mkdtemp(join(tmpdir(), "agent-fabric-supervisor-codex-recovery-policy-"));
    const countPath = join(directory, "starts.txt");
    const supervisor = new AdapterSupervisor({
      "codex-app-server": {
        command: [process.execPath, "--import", "tsx", fixturePath],
        environment: { SUPERVISOR_COUNT_PATH: countPath },
        modelPolicy: {
          allowedFamilies: ["openai"],
          allowedModelPatterns: ["gpt-*", "codex*"],
          requiresExplicitModel: false,
        },
      },
    });
    try {
      await expect(supervisor.recoverChair("codex-app-server", {
        schemaVersion: 1,
        recoveryId: "codex-chair-recovery-policy",
        lossId: "codex-chair-loss-policy",
        actionId: "codex-chair-recover-policy",
        providerContractDigest: `sha256:${"8".repeat(64)}`,
        resumeReference: "codex-recovered-chair-session",
        expectedProviderSessionGeneration: 1,
        nextProviderSessionGeneration: 2,
        bridgeGeneration: 2,
        payload: { cwd: "/workspace/project", modelFamily: "openai", model: "gpt-5.6-sol" },
      }, {
        capability: "codex-recovery-policy-capability",
        socketPath: "/private/codex-recovery-policy.sock",
        attestationChallenge: ATTESTATION_CHALLENGE,
        expectedPrincipal: { ...EXPECTED_CHAIR_PRINCIPAL, principalGeneration: 2 },
      })).rejects.toMatchObject({ code: "MODEL_NOT_ALLOWED" });
      await expect(readFile(countPath, "utf8")).rejects.toMatchObject({ code: "ENOENT" });
    } finally {
      await supervisor.close();
      await rm(directory, { recursive: true, force: true });
    }
  });

  it("promotes one exact retained child and observes the idempotent chair binding", async () => {
    const directory = await mkdtemp(join(tmpdir(), "agent-fabric-supervisor-chair-promotion-"));
    const countPath = join(directory, "starts.txt");
    const supervisor = new AdapterSupervisor({
      fake: {
        command: [process.execPath, "--import", "tsx", fixturePath],
        environment: { SUPERVISOR_COUNT_PATH: countPath, SUPERVISOR_DELAY_MS: "10" },
      },
    }, { bridgeHealthIntervalMs: 1 });
    const childLoss = vi.fn();
    const chairLoss = vi.fn();
    supervisor.setChildBridgeLossHandler(childLoss);
    supervisor.setChairBridgeLossHandler(chairLoss);
    const principal = { agentId: "successor", projectSessionId: "session-1", runId: "run-1", principalGeneration: 2 } as const;
    const exact = {
      ...principal,
      adapterId: "fake",
      actionId: "successor-spawn-action",
      sourceActionId: "successor-spawn-action",
      promotionActionId: "successor-chair-promotion-action",
      providerSessionRef: "fixture-child-session",
      providerSessionGeneration: 1,
      sourceBridgeGeneration: 3,
      chairBridgeGeneration: 4,
    } as const;
    try {
      await supervisor.provisionAgent("fake", {
        schemaVersion: 1,
        runId: principal.runId,
        operation: "spawn",
        actionId: exact.actionId,
        targetAgentId: principal.agentId,
        authorityId: "successor-authority",
        bridgeGeneration: exact.sourceBridgeGeneration,
        bridgeContractDigest: `sha256:${"8".repeat(64)}`,
        payload: {},
      }, {
        capability: "successor-capability-canary",
        socketPath: "/private/successor.sock",
        expectedPrincipal: principal,
      });
      await expect(supervisor.lookupRetainedSuccessorBridge(exact)).resolves.toBe("child");
      await expect(supervisor.promoteRetainedChildBridgeToChair(exact)).resolves.toBe(true);
      await expect(supervisor.lookupRetainedSuccessorBridge(exact)).resolves.toBe("chair");
      await expect(supervisor.promoteRetainedChildBridgeToChair(exact)).resolves.toBe(true);
      const promoted = {
        projectSessionId: principal.projectSessionId,
        runId: principal.runId,
        agentId: principal.agentId,
        principalGeneration: principal.principalGeneration,
        adapterId: exact.adapterId,
        actionId: exact.promotionActionId,
        providerSessionRef: exact.providerSessionRef,
        providerSessionGeneration: exact.providerSessionGeneration,
        bridgeGeneration: exact.chairBridgeGeneration,
      };
      expect(supervisor.hasRetainedChairBridge(promoted)).toBe(true);
      supervisor.retireChairBridge(promoted);
      expect(supervisor.hasRetainedChairBridge(promoted)).toBe(false);
      expect(childLoss).not.toHaveBeenCalled();
      expect(chairLoss).not.toHaveBeenCalled();
      expect(await readFile(countPath, "utf8")).toBe("1");
    } finally {
      await supervisor.close();
      await rm(directory, { recursive: true, force: true });
    }
  });

  it("tombstones a released retained child instead of reconstructing its provider session", async () => {
    const directory = await mkdtemp(join(tmpdir(), "agent-fabric-supervisor-child-release-"));
    const countPath = join(directory, "starts.txt");
    const supervisor = new AdapterSupervisor({
      fake: {
        command: [process.execPath, "--import", "tsx", fixturePath],
        environment: { SUPERVISOR_COUNT_PATH: countPath },
      },
    });
    const principal = { agentId: "child", projectSessionId: "session-1", runId: "run-1", principalGeneration: 2 } as const;
    try {
      await supervisor.provisionAgent("fake", {
        schemaVersion: 1,
        runId: principal.runId,
        operation: "spawn",
        actionId: "child-release-spawn",
        targetAgentId: principal.agentId,
        authorityId: "child-authority",
        bridgeGeneration: 1,
        bridgeContractDigest: `sha256:${"9".repeat(64)}`,
        payload: {},
      }, {
        capability: "child-release-capability",
        socketPath: "/private/child-release.sock",
        expectedPrincipal: principal,
      });
      await expect(supervisor.request("fake", "release", {
        actionId: "child-release-action",
        resumeReference: "fixture-child-session",
        providerSessionGeneration: 1,
      })).resolves.toMatchObject({ method: "release" });
      await expect(supervisor.request("fake", "dispatch", {
        actionId: "child-turn-after-release",
        operation: "send_turn",
        payload: {
          resumeReference: "fixture-child-session",
          providerSessionGeneration: 1,
          prompt: "must not reconstruct",
        },
      })).rejects.toMatchObject({ code: "AGENT_BRIDGE_LOST" });
      await expect(supervisor.request("fake", "lookup_action", {
        actionId: "child-release-spawn",
      })).rejects.toMatchObject({ code: "AGENT_BRIDGE_LOST" });
      expect(await readFile(countPath, "utf8")).toBe("1");
    } finally {
      await supervisor.close();
      await rm(directory, { recursive: true, force: true });
    }
  });

  it("refuses terminal success when the dedicated chair adapter tears down immediately", async () => {
    const directory = await mkdtemp(join(tmpdir(), "agent-fabric-supervisor-chair-teardown-"));
    const countPath = join(directory, "starts.txt");
    const supervisor = new AdapterSupervisor({
      fake: {
        command: [process.execPath, "--import", "tsx", fixturePath],
        environment: { SUPERVISOR_COUNT_PATH: countPath, SUPERVISOR_EXIT_AFTER_LAUNCH: "1" },
      },
    });
    try {
      await expect(supervisor.launchChair("fake", {
        schemaVersion: 1,
        actionId: "chair-launch-immediate-teardown",
        providerContractDigest: `sha256:${"e".repeat(64)}`,
        payload: { cwd: "/workspace/project", modelFamily: "fixture", model: "provider-test" },
      }, {
        capability: "teardown-capability-canary",
        socketPath: "/private/teardown.sock",
        attestationChallenge: ATTESTATION_CHALLENGE,
        expectedPrincipal: EXPECTED_CHAIR_PRINCIPAL,
      })).rejects.toMatchObject({ code: "CHAIR_LAUNCH_FAILED" });
    } finally {
      await supervisor.close();
      await rm(directory, { recursive: true, force: true });
    }
  });

  it("reports one exact retained-chair loss when the adapter socket closes passively", async () => {
    const directory = await mkdtemp(join(tmpdir(), "agent-fabric-supervisor-chair-passive-loss-"));
    const countPath = join(directory, "starts.txt");
    const supervisor = new AdapterSupervisor({
      fake: {
        command: [process.execPath, "--import", "tsx", fixturePath],
        environment: {
          SUPERVISOR_COUNT_PATH: countPath,
          SUPERVISOR_PASSIVE_EXIT_DELAY_MS: "500",
        },
      },
    });
    const loss = vi.fn();
    supervisor.setChairBridgeLossHandler(loss);
    try {
      await supervisor.launchChair("fake", {
        schemaVersion: 1,
        actionId: "chair-launch-passive-loss",
        providerContractDigest: `sha256:${"c".repeat(64)}`,
        payload: { cwd: "/workspace/project", modelFamily: "fixture", model: "provider-test" },
      }, {
        capability: "passive-loss-capability-canary",
        socketPath: "/private/passive-loss.sock",
        attestationChallenge: ATTESTATION_CHALLENGE,
        expectedPrincipal: EXPECTED_CHAIR_PRINCIPAL,
      });
      await vi.waitFor(() => expect(loss).toHaveBeenCalledOnce(), { timeout: 3_000 });
      expect(loss).toHaveBeenCalledWith({
        ...EXPECTED_CHAIR_PRINCIPAL,
        adapterId: "fake",
        actionId: "chair-launch-passive-loss",
        providerSessionRef: "fixture-chair-session",
        providerSessionGeneration: 1,
        bridgeGeneration: 1,
      }, "retained adapter transport closed");
      await settle(50, "no second chair bridge loss callback follows the first");
      expect(loss).toHaveBeenCalledOnce();
    } finally {
      await supervisor.close();
      await rm(directory, { recursive: true, force: true });
    }
  });

  it("rejects chair activation when the inner retained bridge is already closed", async () => {
    const directory = await mkdtemp(join(tmpdir(), "agent-fabric-supervisor-inner-chair-initial-"));
    const countPath = join(directory, "starts.txt");
    const supervisor = new AdapterSupervisor({
      fake: {
        command: [process.execPath, "--import", "tsx", fixturePath],
        environment: {
          SUPERVISOR_COUNT_PATH: countPath,
          SUPERVISOR_INNER_BRIDGE_INITIAL_LIVE: "0",
        },
      },
    }, { bridgeHealthIntervalMs: 10 });
    try {
      await expect(supervisor.launchChair("fake", {
        schemaVersion: 1,
        actionId: "chair-inner-closed-initial",
        providerContractDigest: `sha256:${"4".repeat(64)}`,
        payload: { cwd: "/workspace/project", modelFamily: "fixture", model: "provider-test" },
      }, {
        capability: "inner-chair-initial-capability",
        socketPath: "/private/inner-chair-initial.sock",
        attestationChallenge: ATTESTATION_CHALLENGE,
        expectedPrincipal: EXPECTED_CHAIR_PRINCIPAL,
      })).rejects.toMatchObject({ code: "CHAIR_LAUNCH_FAILED" });
    } finally {
      await supervisor.close();
      await rm(directory, { recursive: true, force: true });
    }
  });

  it("reports inner chair bridge loss while the wrapper process remains live and idle", async () => {
    const directory = await mkdtemp(join(tmpdir(), "agent-fabric-supervisor-inner-chair-live-"));
    const countPath = join(directory, "starts.txt");
    const supervisor = new AdapterSupervisor({
      fake: {
        command: [process.execPath, "--import", "tsx", fixturePath],
        environment: {
          SUPERVISOR_COUNT_PATH: countPath,
          SUPERVISOR_INNER_BRIDGE_LOSS_DELAY_MS: "30",
        },
      },
    }, { bridgeHealthIntervalMs: 5 });
    const loss = vi.fn();
    supervisor.setChairBridgeLossHandler(loss);
    try {
      await supervisor.launchChair("fake", {
        schemaVersion: 1,
        actionId: "chair-inner-loss-idle",
        providerContractDigest: `sha256:${"5".repeat(64)}`,
        payload: { cwd: "/workspace/project", modelFamily: "fixture", model: "provider-test" },
      }, {
        capability: "inner-chair-loss-capability",
        socketPath: "/private/inner-chair-loss.sock",
        attestationChallenge: ATTESTATION_CHALLENGE,
        expectedPrincipal: EXPECTED_CHAIR_PRINCIPAL,
      });
      await vi.waitFor(() => expect(loss).toHaveBeenCalledOnce(), { timeout: 1_000 });
      expect(loss.mock.calls[0]?.[1]).toContain("inner retained chair bridge");
      expect(await readFile(countPath, "utf8")).toBe("1");
    } finally {
      await supervisor.close();
      await rm(directory, { recursive: true, force: true });
    }
  });

  it("rejects and supervises the deep child bridge independently of the live wrapper", async () => {
    const directory = await mkdtemp(join(tmpdir(), "af-inner-child-"));
    const countPath = join(directory, "starts.txt");
    const lossMarkerPath = join(directory, "lose-inner-bridge");
    const supervisor = new AdapterSupervisor({
      fake: {
        command: [process.execPath, "--import", "tsx", fixturePath],
        environment: {
          SUPERVISOR_COUNT_PATH: countPath,
          SUPERVISOR_INNER_BRIDGE_LOSS_MARKER_PATH: lossMarkerPath,
        },
      },
    }, { bridgeHealthIntervalMs: 5 });
    const loss = vi.fn();
    let resolveLoss!: () => void;
    const lossObserved = new Promise<void>((resolve) => { resolveLoss = resolve; });
    supervisor.setChildBridgeLossHandler((entry, reason) => {
      loss(entry, reason);
      resolveLoss();
    });
    const expectedPrincipal = { agentId: "child", projectSessionId: "session-1", runId: "run-1", principalGeneration: 2 };
    try {
      const provisioned = await supervisor.provisionAgent("fake", {
        schemaVersion: 1,
        runId: "run-1",
        operation: "spawn",
        actionId: "child-inner-loss-idle",
        targetAgentId: "child",
        authorityId: "child-authority",
        bridgeGeneration: 3,
        bridgeContractDigest: `sha256:${"6".repeat(64)}`,
        payload: {},
      }, {
        capability: "inner-child-loss-capability",
        socketPath: "/private/inner-child-loss.sock",
        expectedPrincipal,
      });
      expect(supervisor.hasRetainedChildBridge({
        runId: "run-1",
        agentId: "child",
        adapterId: "fake",
        actionId: "child-inner-loss-idle",
        providerSessionRef: provisioned.providerSessionRef,
        providerSessionGeneration: provisioned.providerSessionGeneration,
        bridgeGeneration: provisioned.bridgeGeneration,
      })).toBe(true);
      await writeFile(lossMarkerPath, "", { mode: 0o600 });
      await lossObserved;
      expect(loss).toHaveBeenCalledOnce();
      expect(loss.mock.calls[0]?.[1]).toContain("inner retained child bridge");
      expect(await readFile(countPath, "utf8")).toBe("1");
    } finally {
      await supervisor.close();
      await rm(directory, { recursive: true, force: true });
    }
  });

  it("refuses an invalid private socket before starting a dedicated adapter", async () => {
    const directory = await mkdtemp(join(tmpdir(), "agent-fabric-supervisor-chair-invalid-"));
    const countPath = join(directory, "starts.txt");
    const supervisor = new AdapterSupervisor({
      fake: {
        command: [process.execPath, "--import", "tsx", fixturePath],
        environment: { SUPERVISOR_COUNT_PATH: countPath },
      },
    });
    try {
      await expect(supervisor.launchChair("fake", {
        schemaVersion: 1,
        actionId: "chair-launch-invalid",
        providerContractDigest: `sha256:${"d".repeat(64)}`,
        payload: { cwd: "/workspace/project", modelFamily: "fixture", model: "provider-test" },
      }, {
        capability: "invalid-socket-capability-canary",
        socketPath: "relative/fabric.sock",
        attestationChallenge: ATTESTATION_CHALLENGE,
        expectedPrincipal: EXPECTED_CHAIR_PRINCIPAL,
      })).rejects.toMatchObject({ code: "PRIVATE_HANDOFF_UNAVAILABLE" });
      await expect(supervisor.launchChair("fake", {
        schemaVersion: 1,
        actionId: "chair-launch-invalid-challenge",
        providerContractDigest: `sha256:${"d".repeat(64)}`,
        payload: { cwd: "/workspace/project", modelFamily: "fixture", model: "provider-test" },
      }, {
        capability: "invalid-challenge-capability-canary",
        socketPath: "/private/fabric.sock",
        attestationChallenge: "not-a-32-byte-challenge",
        expectedPrincipal: EXPECTED_CHAIR_PRINCIPAL,
      })).rejects.toMatchObject({ code: "PRIVATE_HANDOFF_UNAVAILABLE" });
      await expect(readFile(countPath, "utf8")).rejects.toMatchObject({ code: "ENOENT" });
    } finally {
      await supervisor.close();
      await rm(directory, { recursive: true, force: true });
    }
  });
});
