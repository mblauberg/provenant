import { chmod, mkdir, mkdtemp, readFile, realpath, rm, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";

import { afterEach, describe, expect, it, vi } from "vitest";
import { MCP_BOOTSTRAP_CREDENTIALS_FEATURE, MCP_BOOTSTRAP_RESULT_SHAPE_FEATURE } from "@local/agent-fabric-protocol";

import type { BootstrapMcpSeatResult } from "../../src/core/contracts.ts";
import type { FabricPaths } from "../../src/cli/paths.ts";

const daemon = vi.hoisted((): {
  result: BootstrapMcpSeatResult | undefined;
  startError: Error | undefined;
  startErrorOnNextStart: boolean;
  bootstrapErrors: Error[];
  beforeResult: (() => Promise<void>) | undefined;
  custodyAdmission: (() => Promise<void>) | undefined;
  custodyCalls: number;
  bootstrapRequests: unknown[];
  requiredCapabilities: string[][];
} => ({
  result: undefined,
  startError: undefined,
  startErrorOnNextStart: false,
  bootstrapErrors: [],
  beforeResult: undefined,
  custodyAdmission: undefined,
  custodyCalls: 0,
  bootstrapRequests: [],
  requiredCapabilities: [],
}));

vi.mock("../../src/daemon/client.js", () => ({
  startFabricDaemon: vi.fn(async (_options: { configuration?: { projectConfigPath?: string } }) => {
    if (daemon.startError !== undefined) throw daemon.startError;
    if (daemon.startErrorOnNextStart) {
      daemon.startErrorOnNextStart = false;
      throw new Error("simulated concurrent daemon start failure");
    }
    return {
      address: { path: join(tmpdir(), "fabric-auto-enrol-no-daemon.sock") },
      bootstrapCapability: "unused-bootstrap-capability",
      ownsProcess: false,
      pid: 4242,
      release: vi.fn(),
    };
  }),
  connectFabricDaemon: vi.fn(async (options: { requiredCapabilities?: readonly string[] }) => {
    daemon.requiredCapabilities.push([...(options.requiredCapabilities ?? [])]);
    return {
    bootstrapMcpSeat: vi.fn(async (request: unknown) => {
      daemon.bootstrapRequests.push(request);
      const error = daemon.bootstrapErrors.shift();
      if (error !== undefined) throw error;
      if (daemon.result === undefined) throw new Error("fake bootstrap result is missing");
      await daemon.custodyAdmission?.();
      daemon.custodyCalls += 1;
      await daemon.beforeResult?.();
      return daemon.result;
    }),
    close: vi.fn(async () => undefined),
    };
  }),
}));

import { bootstrapMcpSeat } from "../../src/cli/mcp-bootstrap.ts";
import { projectBoundaryEvidenceDigest, resolveProjectBoundary } from "../../src/cli/project-boundary.ts";
import { readActiveSeatGeneration } from "../../src/cli/seat-store.ts";
import { waitForFile } from "../shared/deadline-wait.ts";
import { FabricRemoteError } from "../../src/transport/ndjson-rpc.ts";
import {
  ensureAutomaticBootstrapTrust,
  runWorkspaceTrust,
  trustedWorkspaceIdentity,
} from "../../src/cli/workspace-trust.ts";

const roots: string[] = [];

afterEach(async () => {
  daemon.result = undefined;
  daemon.startError = undefined;
  daemon.startErrorOnNextStart = false;
  daemon.bootstrapErrors = [];
  daemon.beforeResult = undefined;
  daemon.custodyAdmission = undefined;
  daemon.custodyCalls = 0;
  daemon.bootstrapRequests = [];
  daemon.requiredCapabilities = [];
  await Promise.all(roots.splice(0).map(async (root) => await rm(root, { recursive: true, force: true })));
});

async function fixture(): Promise<{ outer: string; inner: string; cwd: string; paths: FabricPaths }> {
  const root = await realpath(await mkdtemp(join(tmpdir(), "fabric-auto-enrol-")));
  roots.push(root);
  const outer = join(root, "outer");
  const inner = join(outer, "inner");
  const cwd = join(inner, "src");
  const stateDirectory = join(root, "state");
  await Promise.all([
    mkdir(join(outer, ".git"), { recursive: true }),
    mkdir(join(inner, ".git"), { recursive: true }),
    mkdir(cwd, { recursive: true }),
    mkdir(stateDirectory, { recursive: true, mode: 0o700 }),
  ]);
  daemon.result = {
    projectId: "project-one",
    canonicalRoot: inner,
    bootstrapRunDirectory: ".agent-run/bootstrap-one",
    custodyMutated: true,
    expectedPreviousGeneration: null,
    generation: "a".repeat(64),
    projectSessionId: "session-one",
    sessionRevision: 1,
    sessionGeneration: 1,
    runId: "run-one",
    runRevision: 1,
    chairAgentId: "codex-agent",
    chairGeneration: 1,
    chairLeaseId: "chair:run-one:1",
    expiresAt: "2099-01-01T00:00:00.000Z",
    credentials: [
      {
        seat: "codex",
        agentId: "codex-agent",
        expectedPrincipalGeneration: 1,
        capability: `afc_${"c".repeat(43)}`,
        authorityId: "authority-one",
      },
      {
        seat: "claude",
        agentId: "claude-agent",
        expectedPrincipalGeneration: 1,
        capability: `afc_${"d".repeat(43)}`,
        authorityId: "authority-two",
      },
    ],
  };
  return {
    outer,
    inner,
    cwd,
    paths: {
      stateDirectory,
      runtimeDirectory: join(root, "runtime"),
      databasePath: join(stateDirectory, "fabric-v1.sqlite3"),
      socketPath: join(root, "runtime", "fabric-v1.sock"),
    },
  };
}

async function waitForMarker(path: string): Promise<void> {
  await waitForFile(path, { timeoutMs: 2_000, pollIntervalMs: 5 });
}

describe("automatic exact-project enrolment", () => {
  it("enrols only the nearest Git root from a nested CWD and continues bootstrap", async () => {
    const value = await fixture();

    const installed = await bootstrapMcpSeat({
      environment: { AGENT_FABRIC_SEAT: "codex" },
      cwd: value.cwd,
      paths: value.paths,
      smokeDeadlineMs: 1,
    });

    expect(installed.canonicalRoot).toBe(value.inner);
    expect(installed.receipt).toMatchObject({
      canonicalRoot: value.inner,
      actions: expect.arrayContaining([expect.objectContaining({
        action: "workspace-trust",
        mutated: true,
        alreadyTrusted: false,
        establishmentKind: "automatic-bootstrap",
        boundaryKind: "git",
        bootstrapAttemptId: expect.any(String),
      })]),
    });
    const registry = JSON.parse(await readFile(join(value.paths.stateDirectory, "trusted-workspaces.json"), "utf8")) as {
      entries: { canonicalPath: string }[];
    };
    expect(registry.entries.map(({ canonicalPath }) => canonicalPath)).toEqual([value.inner]);
    expect(registry.entries.some(({ canonicalPath }) => canonicalPath === value.outer)).toBe(false);
    await expect(runWorkspaceTrust(["list"], value.paths)).resolves.toMatchObject({
      entries: [expect.objectContaining({ canonicalPath: value.inner, trusted: true })],
    });
    await expect(trustedWorkspaceIdentity({
      stateDirectory: value.paths.stateDirectory,
      canonicalRoot: value.inner,
      executionProfile: "headless",
    })).resolves.toMatchObject({ canonicalRoot: value.inner });
  });

  it("enrols a valid marked non-Git root exactly", async () => {
    const value = await fixture();
    const projectPath = join(value.outer, "..", "marked-project");
    await mkdir(join(projectPath, ".provenant"), { recursive: true });
    const project = await realpath(projectPath);
    await writeFile(join(project, ".provenant", "agent-fabric.yaml"), "schemaVersion: 1\n");
    daemon.result = { ...daemon.result!, canonicalRoot: project };

    const installed = await bootstrapMcpSeat({
      environment: { AGENT_FABRIC_SEAT: "codex" },
      cwd: project,
      paths: value.paths,
      smokeDeadlineMs: 1,
    });

    expect(installed.canonicalRoot).toBe(project);
    await expect(readFile(join(value.paths.stateDirectory, "trusted-workspaces.json"), "utf8").then(JSON.parse)).resolves.toMatchObject({
      entries: [expect.objectContaining({
        canonicalPath: project,
        establishmentKind: "automatic-bootstrap",
        allowedProfiles: ["headless"],
        boundaryKind: "project-marker",
        boundaryEvidenceDigest: expect.stringMatching(/^sha256:[0-9a-f]{64}$/u),
        bootstrapAttemptId: expect.any(String),
      })],
    });
  });

  it("refuses an unmarked non-Git root without mutating the trust registry", async () => {
    const value = await fixture();
    const projectPath = join(value.outer, "..", "unmarked-project");
    await mkdir(projectPath);
    const project = await realpath(projectPath);
    daemon.result = { ...daemon.result!, canonicalRoot: project };

    await expect(bootstrapMcpSeat({
      environment: { AGENT_FABRIC_SEAT: "codex" },
      cwd: project,
      paths: value.paths,
      smokeDeadlineMs: 1,
    })).rejects.toMatchObject({
      code: "WORKSPACE_NOT_TRUSTED",
      message: expect.stringContaining(`provenant project activate '${project}'`),
      receipt: {
        actions: [expect.objectContaining({
          action: "workspace-trust",
          outcome: "failed",
          boundaryEvidenceDigest: expect.stringMatching(/^sha256:[0-9a-f]{64}$/u),
          requestAttemptId: expect.any(String),
          bootstrapAttemptId: null,
        })],
      },
    });
    await expect(readFile(join(value.paths.stateDirectory, "trusted-workspaces.json"))).rejects.toMatchObject({ code: "ENOENT" });
  });

  it("keeps an existing explicit local-operator grant unchanged", async () => {
    const value = await fixture();
    await runWorkspaceTrust(["trust", value.inner, "--profiles", "paired-visible"], value.paths);

    const installed = await bootstrapMcpSeat({
      environment: { AGENT_FABRIC_SEAT: "codex" },
      cwd: value.cwd,
      paths: value.paths,
      smokeDeadlineMs: 1,
    });

    expect(installed.receipt).toMatchObject({
      actions: expect.arrayContaining([expect.objectContaining({
        action: "workspace-trust",
        mutated: false,
        alreadyTrusted: true,
        establishmentKind: "local-operator",
      })]),
    });
    await expect(readFile(join(value.paths.stateDirectory, "trusted-workspaces.json"), "utf8").then(JSON.parse)).resolves.toMatchObject({
      entries: [expect.objectContaining({
        canonicalPath: value.inner,
        approvedBy: "local-operator",
        allowedProfiles: ["paired-visible"],
      })],
    });
    const registry = JSON.parse(await readFile(join(value.paths.stateDirectory, "trusted-workspaces.json"), "utf8")) as {
      entries: Record<string, unknown>[];
    };
    expect(registry.entries[0]).not.toHaveProperty("establishmentKind");
  });

  it("serialises concurrent first-use grants into one mutation and one already-trusted result", async () => {
    const value = await fixture();

    const results = await Promise.all([
      bootstrapMcpSeat({
        environment: { AGENT_FABRIC_SEAT: "codex" },
        cwd: value.cwd,
        paths: value.paths,
        smokeDeadlineMs: 1,
      }),
      bootstrapMcpSeat({
        environment: { AGENT_FABRIC_SEAT: "claude" },
        cwd: value.cwd,
        paths: value.paths,
        smokeDeadlineMs: 1,
      }),
    ]);

    const trustActions = results.map((result) => {
      const action = result.receipt.actions.find((candidate) => candidate.action === "workspace-trust");
      if (action?.action !== "workspace-trust") throw new Error("workspace trust action is missing");
      return action;
    });
    expect(trustActions.filter((action) => action?.mutated === true)).toHaveLength(1);
    expect(trustActions.filter((action) => action?.alreadyTrusted === true)).toHaveLength(1);
    const grantAttemptIds = new Set(trustActions.map((action) => action.bootstrapAttemptId));
    expect(grantAttemptIds.size).toBe(1);
    expect([...grantAttemptIds][0]).toEqual(expect.any(String));
    expect(new Set(trustActions.map((action) => action.requestAttemptId)).size).toBe(2);
    const registry = JSON.parse(await readFile(join(value.paths.stateDirectory, "trusted-workspaces.json"), "utf8")) as {
      entries: { canonicalPath: string }[];
    };
    expect(registry.entries.map(({ canonicalPath }) => canonicalPath)).toEqual([value.inner]);
  });

  it("does not revoke the successful concurrent bootstrap when one request fails", async () => {
    const value = await fixture();
    daemon.startErrorOnNextStart = true;

    const outcomes = await Promise.all([
      bootstrapMcpSeat({
        environment: { AGENT_FABRIC_SEAT: "codex" },
        cwd: value.cwd,
        paths: value.paths,
        smokeDeadlineMs: 1,
      }).then(() => "success" as const, () => "failure" as const),
      bootstrapMcpSeat({
        environment: { AGENT_FABRIC_SEAT: "claude" },
        cwd: value.cwd,
        paths: value.paths,
        smokeDeadlineMs: 1,
      }).then(() => "success" as const, () => "failure" as const),
    ]);

    expect(outcomes.sort()).toEqual(["failure", "success"]);
    await expect(readFile(join(value.paths.stateDirectory, "trusted-workspaces.json"), "utf8").then(JSON.parse)).resolves.toMatchObject({
      entries: [expect.objectContaining({
        canonicalPath: value.inner,
        establishmentKind: "automatic-bootstrap",
      })],
    });
  });

  it("retains trust across a daemon-start failure and retries the exact bootstrap", async () => {
    const value = await fixture();
    daemon.startErrorOnNextStart = true;

    await expect(bootstrapMcpSeat({
      environment: { AGENT_FABRIC_SEAT: "codex" },
      cwd: value.cwd,
      paths: value.paths,
      smokeDeadlineMs: 1,
    })).rejects.toMatchObject({
      receipt: { actions: [expect.objectContaining({ action: "workspace-trust", trustRetained: true })] },
    });

    const retried = await bootstrapMcpSeat({
      environment: { AGENT_FABRIC_SEAT: "codex" },
      cwd: value.cwd,
      paths: value.paths,
      smokeDeadlineMs: 1,
    });
    expect(retried.receipt.actions).toEqual(expect.arrayContaining([expect.objectContaining({
      action: "workspace-trust",
      alreadyTrusted: true,
      mutated: false,
      trustRetained: true,
    })]));
  });

  it("negotiates the bootstrap result shape before invoking bootstrap", async () => {
    const value = await fixture();

    await bootstrapMcpSeat({
      environment: { AGENT_FABRIC_SEAT: "codex" },
      cwd: value.cwd,
      paths: value.paths,
      smokeDeadlineMs: 1,
    });

    expect(daemon.requiredCapabilities).toEqual([[
      MCP_BOOTSTRAP_CREDENTIALS_FEATURE,
      MCP_BOOTSTRAP_RESULT_SHAPE_FEATURE,
    ]]);
  });

  it("reconnects once and reports a lost bootstrap response as reconciled", async () => {
    const value = await fixture();
    daemon.bootstrapErrors = [new FabricRemoteError("DAEMON_DISCONNECTED", "lost bootstrap response")];

    const installed = await bootstrapMcpSeat({
      environment: { AGENT_FABRIC_SEAT: "codex" },
      cwd: value.cwd,
      paths: value.paths,
      smokeDeadlineMs: 1,
    });

    expect(daemon.requiredCapabilities).toHaveLength(2);
    expect(daemon.bootstrapRequests).toHaveLength(2);
    expect(daemon.bootstrapRequests[1]).toBe(daemon.bootstrapRequests[0]);
    expect(installed.receipt.actions).toEqual(expect.arrayContaining([expect.objectContaining({
      action: "custody",
      outcome: "reconciled",
      mutated: false,
    })]));
  });

  it("returns custody-ambiguous after two lost bootstrap responses without a custody claim", async () => {
    const value = await fixture();
    daemon.bootstrapErrors = [
      new FabricRemoteError("DAEMON_DISCONNECTED", "lost bootstrap response one"),
      new FabricRemoteError("DAEMON_REQUEST_TIMEOUT", "lost bootstrap response two"),
    ];

    const failure = await bootstrapMcpSeat({
      environment: { AGENT_FABRIC_SEAT: "codex" },
      cwd: value.cwd,
      paths: value.paths,
      smokeDeadlineMs: 1,
    }).then(() => undefined, (error: unknown) => error as Error & { receipt?: unknown });
    if (failure === undefined) throw new Error("expected custody ambiguity failure");

    expect(failure).toMatchObject({
      code: "CUSTODY_AMBIGUOUS",
      message: expect.stringContaining("custody-ambiguous"),
      receipt: {
        failure: { phase: "custody-ambiguous" },
        actions: [
          expect.objectContaining({ action: "workspace-trust", trustRetained: true }),
          expect.objectContaining({ action: "daemon" }),
        ],
      },
    });
    expect((failure.receipt as { actions: { action: string }[] }).actions.some(({ action }) => action === "custody")).toBe(false);
    await expect(readFile(join(value.paths.stateDirectory, "trusted-workspaces.json"), "utf8").then(JSON.parse)).resolves.toMatchObject({
      entries: [expect.objectContaining({ canonicalPath: value.inner, establishmentKind: "automatic-bootstrap" })],
    });
  });

  it("reconciles a malformed first response through one immutable replay", async () => {
    const value = await fixture();
    daemon.bootstrapErrors = [new FabricRemoteError("DAEMON_PROTOCOL_INVALID", "malformed committed result")];

    const installed = await bootstrapMcpSeat({
      environment: { AGENT_FABRIC_SEAT: "codex" },
      cwd: value.cwd,
      paths: value.paths,
      smokeDeadlineMs: 1,
    });

    expect(daemon.bootstrapRequests).toHaveLength(2);
    expect(daemon.bootstrapRequests[1]).toBe(daemon.bootstrapRequests[0]);
    expect(installed.receipt.actions).toEqual(expect.arrayContaining([expect.objectContaining({
      action: "custody",
      outcome: "reconciled",
      mutated: false,
    })]));
  });

  it("terminalises any replay failure as custody ambiguity", async () => {
    const value = await fixture();
    daemon.bootstrapErrors = [
      new FabricRemoteError("DAEMON_PROTOCOL_INVALID", "malformed committed result"),
      new FabricRemoteError("AUTHENTICATION_FAILED", "replay was rejected"),
    ];

    const failure = await bootstrapMcpSeat({
      environment: { AGENT_FABRIC_SEAT: "codex" },
      cwd: value.cwd,
      paths: value.paths,
      smokeDeadlineMs: 1,
    }).then(() => undefined, (error: unknown) => error as Error & { receipt?: unknown });
    if (failure === undefined) throw new Error("expected custody ambiguity failure");

    expect(failure).toMatchObject({
      code: "CUSTODY_AMBIGUOUS",
      receipt: { failure: { phase: "custody-ambiguous" } },
    });
    expect((failure.receipt as { actions: { action: string }[] }).actions.some(({ action }) => action === "custody")).toBe(false);
  });

  it("replays an automatic grant without mutating trust on the second request", async () => {
    const value = await fixture();
    const first = await bootstrapMcpSeat({
      environment: { AGENT_FABRIC_SEAT: "codex" },
      cwd: value.cwd,
      paths: value.paths,
      smokeDeadlineMs: 1,
    });
    daemon.result = { ...daemon.result!, custodyMutated: false };
    const second = await bootstrapMcpSeat({
      environment: { AGENT_FABRIC_SEAT: "codex" },
      cwd: value.cwd,
      paths: value.paths,
      smokeDeadlineMs: 1,
    });
    const trustAction = (receipt: typeof first.receipt) => {
      const action = receipt.actions.find((candidate) => candidate.action === "workspace-trust");
      if (action?.action !== "workspace-trust") throw new Error("workspace trust action is missing");
      return action;
    };
    const firstTrust = trustAction(first.receipt);
    const secondTrust = trustAction(second.receipt);
    expect(firstTrust).toMatchObject({ mutated: true, alreadyTrusted: false });
    expect(secondTrust).toMatchObject({ mutated: false, alreadyTrusted: true });
    expect(secondTrust.bootstrapAttemptId).toBe(firstTrust.bootstrapAttemptId);
    expect(secondTrust.requestAttemptId).not.toBe(firstTrust.requestAttemptId);
    expect(second.receipt.actions).toEqual(expect.arrayContaining([expect.objectContaining({
      action: "custody",
      outcome: "replayed",
      mutated: false,
    })]));
  });

  it("retains known custody but publishes no credentials after the trust binding changes", async () => {
    const value = await fixture();
    daemon.beforeResult = async () => {
      await rm(join(value.inner, ".git"), { recursive: true });
    };

    const failure = await bootstrapMcpSeat({
      environment: { AGENT_FABRIC_SEAT: "codex" },
      cwd: value.cwd,
      paths: value.paths,
      smokeDeadlineMs: 1,
    }).then(() => undefined, (error: unknown) => error as Error & { code?: string; receipt?: unknown });
    if (failure === undefined) throw new Error("expected post-custody boundary failure");

    expect(failure).toMatchObject({
      code: "POST_CUSTODY_BOUNDARY",
      receipt: {
        failure: { phase: "post-custody-boundary" },
        actions: expect.arrayContaining([expect.objectContaining({
          action: "custody",
          outcome: "committed",
          mutated: true,
        })]),
      },
    });
    await expect(readActiveSeatGeneration({
      stateDirectory: value.paths.stateDirectory,
      projectPath: value.inner,
    })).resolves.toBeNull();
  });

  it("does not consume automatic trust after its live Git evidence disappears", async () => {
    const value = await fixture();
    await ensureAutomaticBootstrapTrust({
      stateDirectory: value.paths.stateDirectory,
      bootstrapAttemptId: "attempt-live-evidence",
      cwd: value.cwd,
    });
    await rm(join(value.inner, ".git"), { recursive: true });

    await expect(trustedWorkspaceIdentity({
      stateDirectory: value.paths.stateDirectory,
      canonicalRoot: value.inner,
      executionProfile: "headless",
    })).rejects.toThrow(/live boundary evidence|boundary evidence/iu);
  });

  it("reports dead automatic Git trust as untrusted in list and inspect", async () => {
    const value = await fixture();
    await ensureAutomaticBootstrapTrust({
      stateDirectory: value.paths.stateDirectory,
      bootstrapAttemptId: "attempt-dead-marker",
      cwd: value.cwd,
    });
    await rm(join(value.inner, ".git"), { recursive: true });

    await expect(runWorkspaceTrust(["list"], value.paths)).resolves.toMatchObject({
      entries: [expect.objectContaining({ trusted: false })],
    });
    await expect(runWorkspaceTrust(["inspect", value.inner], value.paths)).resolves.toMatchObject({ trusted: false });
  });

  it("keeps the causal boundary digest when the boundary changes after refusal", async () => {
    const value = await fixture();
    const project = join(value.outer, "..", "causal-boundary-project");
    await mkdir(project);
    const boundary = await resolveProjectBoundary(project);
    const causalDigest = projectBoundaryEvidenceDigest(boundary);
    const failure = await bootstrapMcpSeat({
      environment: { AGENT_FABRIC_SEAT: "codex" },
      cwd: project,
      paths: value.paths,
      smokeDeadlineMs: 1,
    }).then(() => undefined, (error: unknown) => error as Error & { receipt?: { actions: { action: string; boundaryEvidenceDigest?: string }[] } });
    if (failure === undefined || failure.receipt === undefined) throw new Error("expected trust refusal receipt");

    await mkdir(join(project, ".git"));
    expect(failure.receipt.actions).toEqual([expect.objectContaining({
      action: "workspace-trust",
      boundaryEvidenceDigest: causalDigest,
    })]);
  });

  it("re-resolves boundary evidence after the registry lock and refuses a lock-time Git removal", async () => {
    const value = await fixture();
    await rm(join(value.inner, ".git"), { recursive: true });
    const gitTarget = join(value.inner, "git-target");
    await mkdir(join(gitTarget, ".git"), { recursive: true });
    await writeFile(join(value.inner, ".git"), "gitdir: git-target/.git\n");

    const shimDirectory = join(value.outer, "git-shim");
    const firstProbe = join(value.outer, "first-probe");
    const secondProbe = join(value.outer, "second-probe");
    const releaseFirst = join(value.outer, "release-first");
    const releaseSecond = join(value.outer, "release-second");
    const removed = join(value.outer, "removed");
    await mkdir(shimDirectory);
    const shim = join(shimDirectory, "git");
    await writeFile(shim, `#!/bin/sh
count_file=${join(value.outer, "git-count")}
count=0
if [ -f "$count_file" ]; then count=$(cat "$count_file"); fi
count=$((count + 1))
printf '%s' "$count" > "$count_file"
if [ "$count" = "1" ]; then
  touch ${firstProbe}
  while [ ! -f ${releaseFirst} ]; do sleep 0.01; done
  printf '%s\\n' ${value.inner}
  exit 0
fi
touch ${secondProbe}
while [ ! -f ${releaseSecond} ]; do sleep 0.01; done
if [ -f ${removed} ]; then
  printf '%s\\n' 'fatal: not a git repository' >&2
  exit 128
fi
printf '%s\\n' ${value.inner}
`);
    await chmod(shim, 0o700);
    const previousPath = process.env.PATH;
    process.env.PATH = `${shimDirectory}:${previousPath ?? ""}`;
    try {
      const pending = ensureAutomaticBootstrapTrust({
        stateDirectory: value.paths.stateDirectory,
        bootstrapAttemptId: "attempt-lock-race",
        cwd: value.cwd,
      });
      await waitForMarker(firstProbe);
      await writeFile(releaseFirst, "");
      await waitForMarker(secondProbe);
      await rm(join(value.inner, ".git"), { recursive: true });
      await writeFile(removed, "");
      await writeFile(releaseSecond, "");

      await expect(pending).rejects.toThrow(/inspect and repair|boundary evidence/iu);
      await expect(readFile(join(value.paths.stateDirectory, "trusted-workspaces.json"))).rejects.toMatchObject({ code: "ENOENT" });
    } finally {
      if (previousPath === undefined) delete process.env.PATH;
      else process.env.PATH = previousPath;
    }
  });

  it("contains a stale row created by a final-resolver interleaving", async () => {
    const value = await fixture();
    let interleavingObserved = false;
    daemon.custodyAdmission = async () => {
      await trustedWorkspaceIdentity({
        stateDirectory: value.paths.stateDirectory,
        canonicalRoot: value.inner,
        executionProfile: "headless",
      });
    };

    await expect(bootstrapMcpSeat({
      environment: { AGENT_FABRIC_SEAT: "codex" },
      cwd: value.cwd,
      paths: value.paths,
      smokeDeadlineMs: 1,
      testOnly: {
        beforeRegistryRename: async () => {
          interleavingObserved = true;
          await rm(join(value.inner, ".git"), { recursive: true });
        },
      },
    })).rejects.toThrow(/live boundary evidence|boundary evidence/iu);
    expect(interleavingObserved).toBe(true);
    await expect(runWorkspaceTrust(["list"], value.paths)).resolves.toMatchObject({
      entries: [expect.objectContaining({ canonicalPath: value.inner, trusted: false })],
    });
    await expect(runWorkspaceTrust(["inspect", value.inner], value.paths)).resolves.toMatchObject({ trusted: false });
    await expect(trustedWorkspaceIdentity({
      stateDirectory: value.paths.stateDirectory,
      canonicalRoot: value.inner,
      executionProfile: "headless",
    })).rejects.toThrow(/live boundary evidence|boundary evidence/iu);
    expect(daemon.custodyCalls).toBe(0);

    await mkdir(join(value.inner, ".git"));
    await expect(bootstrapMcpSeat({
      environment: { AGENT_FABRIC_SEAT: "codex" },
      cwd: value.cwd,
      paths: value.paths,
      smokeDeadlineMs: 1,
    })).resolves.toBeDefined();
    expect(daemon.custodyCalls).toBe(1);
  });

  it("reports a retained automatic grant when daemon start fails immediately after enrolment", async () => {
    const value = await fixture();
    daemon.startError = new Error("simulated daemon start failure");

    const failure = await bootstrapMcpSeat({
      environment: { AGENT_FABRIC_SEAT: "codex" },
      cwd: value.cwd,
      paths: value.paths,
      smokeDeadlineMs: 1,
    }).then(() => undefined, (error: unknown) => error as Error & { receipt?: unknown });

    expect(failure).toMatchObject({
      message: "simulated daemon start failure",
      receipt: {
        healthy: false,
        failure: { phase: "daemon-start", message: "simulated daemon start failure" },
        actions: expect.arrayContaining([expect.objectContaining({
          action: "workspace-trust",
          mutated: true,
          establishmentKind: "automatic-bootstrap",
          trustRetained: true,
        })]),
      },
    });
    await expect(readFile(join(value.paths.stateDirectory, "trusted-workspaces.json"), "utf8").then(JSON.parse)).resolves.toMatchObject({
      entries: [expect.objectContaining({ canonicalPath: value.inner, establishmentKind: "automatic-bootstrap" })],
    });
  });

  it("reports custody-referenced retention when bootstrap fails after the daemon custody call", async () => {
    const value = await fixture();
    daemon.result = { ...daemon.result!, chairAgentId: "missing-chair" };

    const failure = await bootstrapMcpSeat({
      environment: { AGENT_FABRIC_SEAT: "codex" },
      cwd: value.cwd,
      paths: value.paths,
      smokeDeadlineMs: 1,
    }).then(() => undefined, (error: unknown) => error as Error & { receipt?: unknown });

    expect(failure).toMatchObject({
      receipt: {
        healthy: false,
        failure: { phase: "post-custody" },
        actions: expect.arrayContaining([expect.objectContaining({
          action: "workspace-trust",
          mutated: true,
          trustRetained: true,
        }), expect.objectContaining({
          action: "custody",
          outcome: "committed",
          mutated: true,
          generation: daemon.result?.generation,
        }), expect.objectContaining({
          action: "daemon",
          mutated: false,
        })]),
        generation: daemon.result?.generation,
        mutated: true,
      },
    });
    await expect(readFile(join(value.paths.stateDirectory, "trusted-workspaces.json"), "utf8").then(JSON.parse)).resolves.toMatchObject({
      entries: [expect.objectContaining({ canonicalPath: value.inner, establishmentKind: "automatic-bootstrap" })],
    });
  });

});
