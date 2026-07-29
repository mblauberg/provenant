import { lstat, mkdtemp, rm } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";

import { afterEach, describe, expect, it, vi } from "vitest";

import { MCP_BOOTSTRAP_CREDENTIALS_FEATURE } from "@local/agent-fabric-protocol";
import {
  attachOrStartDaemon,
  BootstrapClientError,
} from "../../../src/daemon/bootstrap-client.ts";
import { BootstrapElection } from "../../../src/daemon/bootstrap-election.ts";

const cleanup: string[] = [];

afterEach(async () => {
  await Promise.allSettled(cleanup.splice(0).map((path) => rm(path, { recursive: true, force: true })));
});

describe("attachOrStartDaemon", () => {
  it("never grants destructive cleanup preservation semantics to inspection instability", () => {
    const error = new BootstrapClientError(
      "DATABASE_INSPECTION_UNSTABLE",
      "database changed during inspection",
      { cause: { preserved: true } },
    );

    expect(error).toMatchObject({
      code: "DATABASE_INSPECTION_UNSTABLE",
      preserved: false,
    });
  });

  it("fails typed build preflight before election or spawn", async () => {
    const root = await mkdtemp(join(tmpdir(), "fabric-bootstrap-preflight-"));
    cleanup.push(root);
    const runtimeDirectory = join(root, "runtime");
    const preflightError = Object.assign(new Error("protocol build is stale"), {
      code: "AGENT_FABRIC_PROTOCOL_BUILD_STALE",
    });
    const preflight = vi.fn().mockRejectedValue(preflightError);
    const spawn = vi.fn();

    await expect(attachOrStartDaemon({
      actionId: "bootstrap_preflight_01",
      socketPath: join(runtimeDirectory, "fabric.sock"),
      requiredProtocolVersion: 1,
      requiredFeatures: ["project-sessions.v1"],
      election: new BootstrapElection({ runtimeDirectory }),
      handshake: vi.fn().mockResolvedValue({
        status: "unavailable",
        reason: "absent",
        message: "missing",
      }),
      preflight,
      spawn,
    })).rejects.toBe(preflightError);
    expect(preflight).toHaveBeenCalledOnce();
    expect(spawn).not.toHaveBeenCalled();
    await expect(lstat(runtimeDirectory)).rejects.toMatchObject({ code: "ENOENT" });
  });

  it("returns a compatible initialized daemon without entering election or spawning", async () => {
    const root = await mkdtemp(join(tmpdir(), "fabric-bootstrap-client-"));
    cleanup.push(root);
    const socketPath = join(root, "runtime", "fabric.sock");
    const client = { close: vi.fn() };
    const handshake = vi.fn().mockResolvedValue({
      status: "compatible",
      client,
      protocolVersion: 1,
      daemonInstanceGeneration: 4,
      features: ["fabric-core.v1", "project-sessions.v1"],
    });
    const spawn = vi.fn();

    await expect(attachOrStartDaemon({
      actionId: "bootstrap_first_read_01",
      socketPath,
      requiredProtocolVersion: 1,
      requiredFeatures: ["project-sessions.v1"],
      election: new BootstrapElection({ runtimeDirectory: join(root, "runtime") }),
      handshake,
      spawn,
    })).resolves.toEqual({
      client,
      daemonInstanceGeneration: 4,
      electionGeneration: null,
      started: false,
    });
    expect(handshake).toHaveBeenCalledTimes(1);
    expect(spawn).not.toHaveBeenCalled();
  });

  it("rechecks under the election lock and suppresses spawn when an incumbent becomes ready", async () => {
    const root = await mkdtemp(join(tmpdir(), "fabric-bootstrap-recheck-"));
    cleanup.push(root);
    const socketPath = join(root, "runtime", "fabric.sock");
    const client = { id: "incumbent" };
    const handshake = vi.fn()
      .mockResolvedValueOnce({ status: "unavailable", reason: "absent", message: "missing" })
      .mockResolvedValueOnce({
        status: "compatible",
        client,
        protocolVersion: 1,
        daemonInstanceGeneration: 9,
        features: ["project-sessions.v1"],
      });
    const spawn = vi.fn();

    await expect(attachOrStartDaemon({
      actionId: "bootstrap_recheck_01",
      socketPath,
      requiredProtocolVersion: 1,
      requiredFeatures: ["project-sessions.v1"],
      election: new BootstrapElection({ runtimeDirectory: join(root, "runtime") }),
      handshake,
      spawn,
    })).resolves.toEqual({
      client,
      daemonInstanceGeneration: 9,
      electionGeneration: null,
      started: false,
    });
    expect(spawn).not.toHaveBeenCalled();
  });

  it("retains election ownership through exact ready receipt and authenticated generation handshake", async () => {
    const root = await mkdtemp(join(tmpdir(), "fabric-bootstrap-start-"));
    cleanup.push(root);
    const runtimeDirectory = join(root, "runtime");
    const socketPath = join(runtimeDirectory, "fabric.sock");
    const client = { id: "started" };
    let ready = false;
    const handshake = vi.fn().mockImplementation(async () => ready
      ? {
          status: "compatible",
          client,
          protocolVersion: 1,
          daemonInstanceGeneration: 12,
          features: ["fabric-core.v1", "project-sessions.v1"],
        }
      : { status: "unavailable", reason: "absent", message: "missing" });
    const spawn = vi.fn().mockImplementation(async (input: { actionId: string; electionGeneration: number; socketPath: string }) => ({
      ready: (async () => {
        ready = true;
        return {
          daemonInstanceGeneration: 12,
          socketPath: input.socketPath,
          protocolVersion: 1,
          features: ["fabric-core.v1", "project-sessions.v1"],
          evidence: {
            databaseOwned: true,
            migrationsComplete: true,
            recoveryComplete: true,
            socketBound: true,
          },
        };
      })(),
    }));

    await expect(attachOrStartDaemon({
      actionId: "bootstrap_start_01",
      socketPath,
      requiredProtocolVersion: 1,
      requiredFeatures: ["project-sessions.v1"],
      election: new BootstrapElection({ runtimeDirectory, waitTimeoutMs: 1_000 }),
      handshake,
      spawn,
    })).resolves.toEqual({
      client,
      daemonInstanceGeneration: 12,
      electionGeneration: 1,
      started: true,
    });
    expect(spawn).toHaveBeenCalledWith({
      actionId: "bootstrap_start_01",
      electionGeneration: 1,
      socketPath,
    });
  });

  it("returns a typed failure for a responsive incompatible incumbent without spawning", async () => {
    const root = await mkdtemp(join(tmpdir(), "fabric-bootstrap-incompatible-"));
    cleanup.push(root);
    const runtimeDirectory = join(root, "runtime");
    const socketPath = join(runtimeDirectory, "fabric.sock");
    const handshake = vi.fn()
      .mockResolvedValueOnce({ status: "unavailable", reason: "timeout", message: "bounded timeout" })
      .mockResolvedValueOnce({ status: "incompatible", responsive: true, message: "missing project-sessions.v1" });
    const spawn = vi.fn();

    await expect(attachOrStartDaemon({
      actionId: "bootstrap_incompatible_01",
      socketPath,
      requiredProtocolVersion: 1,
      requiredFeatures: ["project-sessions.v1"],
      election: new BootstrapElection({ runtimeDirectory }),
      handshake,
      spawn,
    })).rejects.toMatchObject({ code: "BOOTSTRAP_INCOMPATIBLE_INCUMBENT" });
    expect(spawn).not.toHaveBeenCalled();
  });

  it("rejects a v1 rpc incumbent before MCP bootstrap or seat rotation when the credential token is absent", async () => {
    const root = await mkdtemp(join(tmpdir(), "fabric-bootstrap-legacy-credentials-"));
    cleanup.push(root);
    const runtimeDirectory = join(root, "runtime");
    const socketPath = join(runtimeDirectory, "fabric.sock");
    const preBootstrap = vi.fn();
    const spawn = vi.fn();

    await expect(attachOrStartDaemon({
      actionId: "bootstrap_legacy_credentials_01",
      socketPath,
      requiredProtocolVersion: 1,
      requiredFeatures: ["rpc", MCP_BOOTSTRAP_CREDENTIALS_FEATURE],
      election: new BootstrapElection({ runtimeDirectory }),
      handshake: async () => ({
        status: "compatible",
        client: { legacyCredentialShape: "pre-0636854" },
        protocolVersion: 1,
        daemonInstanceGeneration: 5,
        features: ["rpc"],
      }),
      preBootstrap,
      spawn,
    })).rejects.toMatchObject({
      code: "BOOTSTRAP_INCOMPATIBLE_INCUMBENT",
      message: expect.stringContaining(MCP_BOOTSTRAP_CREDENTIALS_FEATURE),
    });
    expect({ preBootstrapCalls: preBootstrap.mock.calls.length, spawnCalls: spawn.mock.calls.length })
      .toEqual({ preBootstrapCalls: 0, spawnCalls: 0 });
  });

  it("preserves a typed child startup failure instead of classifying it as ambiguous", async () => {
    const root = await mkdtemp(join(tmpdir(), "fabric-bootstrap-typed-child-"));
    cleanup.push(root);
    const runtimeDirectory = join(root, "runtime");
    const socketPath = join(runtimeDirectory, "fabric.sock");

    await expect(attachOrStartDaemon({
      actionId: "bootstrap_typed_child_01",
      socketPath,
      requiredProtocolVersion: 1,
      requiredFeatures: ["project-sessions.v1"],
      election: new BootstrapElection({ runtimeDirectory }),
      handshake: async () => ({ status: "unavailable", reason: "absent", message: "missing" }),
      spawn: async () => ({
        ready: Promise.reject(Object.assign(new Error("existing database preserved"), {
          code: "SCHEMA_CUTOVER_REQUIRED",
        })),
      }),
    })).rejects.toMatchObject({ code: "SCHEMA_CUTOVER_REQUIRED" });
  });

  it("blocks blind spawn when private discovery makes unreachability ambiguous", async () => {
    const root = await mkdtemp(join(tmpdir(), "fabric-bootstrap-ambiguous-discovery-"));
    cleanup.push(root);
    const runtimeDirectory = join(root, "runtime");
    const socketPath = join(runtimeDirectory, "fabric.sock");
    const spawn = vi.fn();

    await expect(attachOrStartDaemon({
      actionId: "bootstrap_ambiguous_discovery_01",
      socketPath,
      requiredProtocolVersion: 1,
      requiredFeatures: ["project-sessions.v1"],
      election: new BootstrapElection({ runtimeDirectory }),
      handshake: async () => ({
        status: "unavailable",
        reason: "unreachable",
        message: "active discovery cannot be reconciled",
        reconciliationRequired: true,
      }),
      spawn,
    })).rejects.toMatchObject({ code: "BOOTSTRAP_RECONCILIATION_REQUIRED" });
    expect(spawn).not.toHaveBeenCalled();
  });

  it("coalesces twelve simultaneous first reads onto one spawned generation", async () => {
    const root = await mkdtemp(join(tmpdir(), "fabric-bootstrap-concurrent-"));
    cleanup.push(root);
    const runtimeDirectory = join(root, "runtime");
    const socketPath = join(runtimeDirectory, "fabric.sock");
    const election = new BootstrapElection({
      runtimeDirectory,
      leaseDurationMs: 2_000,
      waitTimeoutMs: 3_000,
      pollIntervalMs: 2,
    });
    const client = { id: "shared" };
    let ready = false;
    const handshake = vi.fn().mockImplementation(async () => ready
      ? {
          status: "compatible",
          client,
          protocolVersion: 1,
          daemonInstanceGeneration: 23,
          features: ["project-sessions.v1"],
        }
      : { status: "unavailable", reason: "absent", message: "missing" });
    const spawn = vi.fn().mockImplementation(async (input: { socketPath: string }) => ({
      ready: (async () => {
        await new Promise((resolve) => setTimeout(resolve, 25));
        ready = true;
        return {
          daemonInstanceGeneration: 23,
          socketPath: input.socketPath,
          protocolVersion: 1,
          features: ["project-sessions.v1"],
          evidence: {
            databaseOwned: true,
            migrationsComplete: true,
            recoveryComplete: true,
            socketBound: true,
          },
        };
      })(),
    }));

    const attached = await Promise.all(Array.from({ length: 12 }, (_, index) => attachOrStartDaemon({
      actionId: `bootstrap_concurrent_${String(index)}`,
      socketPath,
      requiredProtocolVersion: 1,
      requiredFeatures: ["project-sessions.v1"],
      election,
      handshake,
      spawn,
    })));

    expect(spawn).toHaveBeenCalledTimes(1);
    expect(attached.filter((item) => item.started)).toHaveLength(1);
    expect(new Set(attached.map((item) => item.daemonInstanceGeneration))).toEqual(new Set([23]));
    expect(new Set(attached.map((item) => item.electionGeneration))).toEqual(new Set([1]));
  });

  it("advances a confirmed ready generation only after exact stopped ownership proof", async () => {
    const root = await mkdtemp(join(tmpdir(), "fabric-bootstrap-stopped-"));
    cleanup.push(root);
    const runtimeDirectory = join(root, "runtime");
    const socketPath = join(runtimeDirectory, "fabric.sock");
    const election = new BootstrapElection({ runtimeDirectory });
    let runningGeneration: number | undefined;
    let stoppedGeneration: number | undefined;
    let terminalActionId = "bootstrap_stopped_01";
    const handshake = vi.fn().mockImplementation(async () => {
      if (runningGeneration !== undefined) {
        return {
          status: "compatible",
          client: { generation: runningGeneration },
          protocolVersion: 1,
          daemonInstanceGeneration: runningGeneration,
          features: ["project-sessions.v1"],
        };
      }
      return {
        status: "unavailable",
        reason: stoppedGeneration === undefined ? "absent" : "stale",
        message: stoppedGeneration === undefined ? "missing" : "proved stopped",
        ...(stoppedGeneration === undefined ? {} : {
          terminalEvidence: {
            state: "stopped",
            actionId: terminalActionId,
            electionGeneration: stoppedGeneration,
            daemonInstanceGeneration: stoppedGeneration,
            socketPath,
          },
        }),
      };
    });
    const spawn = vi.fn().mockImplementation(async (input: { electionGeneration: number; socketPath: string }) => {
      runningGeneration = input.electionGeneration;
      return {
        ready: Promise.resolve({
          daemonInstanceGeneration: input.electionGeneration,
          socketPath: input.socketPath,
          protocolVersion: 1,
          features: ["project-sessions.v1"],
          evidence: {
            databaseOwned: true,
            migrationsComplete: true,
            recoveryComplete: true,
            socketBound: true,
          },
        }),
      };
    });
    const options = {
      actionId: "bootstrap_stopped_01",
      socketPath,
      requiredProtocolVersion: 1,
      requiredFeatures: ["project-sessions.v1"],
      election,
      handshake,
      spawn,
    } as const;

    await expect(attachOrStartDaemon(options)).resolves.toMatchObject({
      daemonInstanceGeneration: 1,
      electionGeneration: 1,
      started: true,
    });
    const priorGeneration = runningGeneration;
    runningGeneration = undefined;

    await expect(attachOrStartDaemon(options)).rejects.toMatchObject({
      code: "BOOTSTRAP_READY_UNREACHABLE",
    });
    expect(spawn).toHaveBeenCalledTimes(1);
    stoppedGeneration = priorGeneration;

    terminalActionId = "wrong_bootstrap_action";
    await expect(attachOrStartDaemon(options)).rejects.toMatchObject({
      code: "BOOTSTRAP_TERMINAL_EVIDENCE_MISMATCH",
    });
    expect(spawn).toHaveBeenCalledTimes(1);
    terminalActionId = "bootstrap_stopped_01";

    await expect(attachOrStartDaemon(options)).resolves.toMatchObject({
      daemonInstanceGeneration: 2,
      electionGeneration: 2,
      started: true,
    });
    expect(spawn).toHaveBeenCalledTimes(2);
  });
});
