import { copyFile, mkdir, mkdtemp, readFile, realpath, rm, symlink, writeFile } from "node:fs/promises";
import { homedir, tmpdir } from "node:os";
import { join, parse } from "node:path";
import { fileURLToPath } from "node:url";

import { afterEach, describe, expect, it, vi } from "vitest";

import type { BootstrapMcpSeatResult } from "../../src/core/contracts.ts";

const daemon = vi.hoisted((): { result: BootstrapMcpSeatResult | undefined; canonicalRoots: string[] } => ({
  result: undefined,
  canonicalRoots: [],
}));

vi.mock("../../src/daemon/client.js", () => ({
  startFabricDaemon: vi.fn(async () => ({
    address: { path: join(tmpdir(), "fabric-bootstrap-trust-guidance-missing.sock") },
    bootstrapCapability: "unused-bootstrap-capability",
    ownsProcess: false,
    pid: 4242,
    release: vi.fn(),
  })),
  connectFabricDaemon: vi.fn(async () => ({
    bootstrapMcpSeat: vi.fn(async (input: { canonicalRoot: string }) => {
      if (daemon.result === undefined) throw new Error("test bootstrap result is missing");
      daemon.canonicalRoots.push(input.canonicalRoot);
      return daemon.result;
    }),
    close: vi.fn(async () => undefined),
  })),
}));

import { bootstrapMcpSeat } from "../../src/cli/mcp-bootstrap.ts";
import { runWorkspaceTrust } from "../../src/cli/workspace-trust.ts";

const temporaryDirectories: string[] = [];
const sourceProductRoot = fileURLToPath(new URL("../../../..", import.meta.url));

async function createBootstrapProduct(root: string): Promise<string> {
  const product = join(root, "product");
  await mkdir(join(product, "config"), { recursive: true });
  await mkdir(join(product, "runtime", "agent-fabric", "schemas"), { recursive: true });
  await writeFile(join(product, "config", "agent-fabric.yaml"), [
    "schemaVersion: 1",
    "allowedAdapters: []",
    "activeAdapters: []",
    "allowedProfiles:",
    "  - headless",
    "workspaceRoots:",
    "  - \"${AGENTS_HOME}\"",
    "limits:",
    "  maximumConcurrentProviderTurns: 8",
    "",
  ].join("\n"));
  await Promise.all([
    copyFile(
      join(sourceProductRoot, "config", "adapter-compatibility.yaml"),
      join(product, "config", "adapter-compatibility.yaml"),
    ),
    copyFile(
      join(sourceProductRoot, "runtime", "agent-fabric", "schemas", "adapter-compatibility.schema.json"),
      join(product, "runtime", "agent-fabric", "schemas", "adapter-compatibility.schema.json"),
    ),
  ]);
  return product;
}

function paths(root: string) {
  return {
    stateDirectory: join(root, "state"),
    runtimeDirectory: join(root, "r"),
    databasePath: join(root, "state", "f.db"),
    socketPath: join(root, "r", "f.sock"),
  };
}

async function bootstrapFailure(cwd: string, root: string, now?: Date): Promise<Error & { code?: string }> {
  try {
    await bootstrapMcpSeat({
      environment: {
        AGENT_FABRIC_SEAT: "codex",
        AGENT_FABRIC_PRODUCT_ROOT: join(root, "product"),
      },
      cwd,
      paths: paths(root),
      ...(now === undefined ? {} : { now }),
    });
    throw new Error("expected bootstrap to reject an untrusted workspace");
  } catch (error: unknown) {
    return error as Error & { code?: string };
  }
}

async function recordedTrustPaths(stateDirectory: string): Promise<string[]> {
  try {
    const registry = JSON.parse(await readFile(join(stateDirectory, "trusted-workspaces.json"), "utf8")) as {
      entries?: Array<{ canonicalPath?: unknown }>;
    };
    return (registry.entries ?? []).map((entry) => {
      if (typeof entry.canonicalPath !== "string") throw new Error("test trust entry is missing its canonical path");
      return entry.canonicalPath;
    });
  } catch (error: unknown) {
    if (typeof error === "object" && error !== null && "code" in error && error.code === "ENOENT") return [];
    throw error;
  }
}

async function stopStartedDaemon(result: Awaited<ReturnType<typeof bootstrapMcpSeat>>): Promise<void> {
  const daemon = result.receipt.actions.find((action) => action.action === "daemon");
  if (daemon?.action !== "daemon" || daemon.outcome !== "started") return;
  try {
    process.kill(daemon.pid, "SIGTERM");
  } catch (error: unknown) {
    if (!(typeof error === "object" && error !== null && "code" in error && error.code === "ESRCH")) throw error;
  }
  for (let attempt = 0; attempt < 200; attempt += 1) {
    try {
      process.kill(daemon.pid, 0);
    } catch (error: unknown) {
      if (typeof error === "object" && error !== null && "code" in error && error.code === "ESRCH") return;
      throw error;
    }
    await new Promise<void>((resolve) => setTimeout(resolve, 10));
  }
  throw new Error(`test daemon ${String(daemon.pid)} did not stop`);
}

function bootstrapResult(canonicalRoot: string): BootstrapMcpSeatResult {
  return {
    projectId: `project-${canonicalRoot}`,
    canonicalRoot,
    bootstrapRunDirectory: ".agent-run/bootstrap",
    expectedPreviousGeneration: null,
    generation: "a".repeat(64),
    projectSessionId: "session-test",
    sessionRevision: 1,
    sessionGeneration: 1,
    runId: "run-test",
    runRevision: 1,
    chairAgentId: "codex-test-agent",
    chairGeneration: 1,
    chairLeaseId: "chair:test:1",
    expiresAt: "2099-01-01T00:00:00.000Z",
    credentials: [{
      seat: "codex",
      agentId: "codex-test-agent",
      expectedPrincipalGeneration: 1,
      capability: `afc_${"c".repeat(43)}`,
      authorityId: "authority-test",
    }],
  };
}

async function bootstrapSuccess(
  cwd: string,
  root: string,
  canonicalRoot = cwd,
): Promise<Awaited<ReturnType<typeof bootstrapMcpSeat>>> {
  let result: Awaited<ReturnType<typeof bootstrapMcpSeat>> | undefined;
  const product = await createBootstrapProduct(root);
  daemon.result = bootstrapResult(await realpath(canonicalRoot));
  try {
    result = await bootstrapMcpSeat({
      environment: {
        AGENT_FABRIC_SEAT: "codex",
        AGENT_FABRIC_PRODUCT_ROOT: product,
      },
      cwd,
      paths: paths(root),
    });
    return result;
  } finally {
    if (result !== undefined) await stopStartedDaemon(result);
  }
}

afterEach(async () => {
  daemon.result = undefined;
  daemon.canonicalRoots = [];
  await Promise.all(temporaryDirectories.splice(0).map(
    async (path) => await rm(path, { recursive: true, force: true }),
  ));
});

describe("MCP bootstrap workspace-trust guidance", () => {
  it("automatically enrols the nearest repository root from a nested repository subdirectory", async () => {
    const temporaryRoot = await mkdtemp(join(tmpdir(), "fabric-repository-bootstrap-"));
    temporaryDirectories.push(temporaryRoot);
    const fixtureRoot = await realpath(temporaryRoot);
    const outer = join(fixtureRoot, "outer");
    const inner = join(outer, "inner-repo");
    const project = join(inner, "src");
    await mkdir(join(outer, ".git"), { recursive: true });
    await mkdir(join(inner, ".git"), { recursive: true });
    await mkdir(project, { recursive: true });

    const result = await bootstrapSuccess(project, fixtureRoot, inner);

    expect(result.canonicalRoot).toBe(await realpath(inner));
    expect(daemon.canonicalRoots).toEqual([await realpath(inner)]);
    await expect(recordedTrustPaths(join(fixtureRoot, "state"))).resolves.toEqual([await realpath(inner)]);
    await expect(recordedTrustPaths(join(fixtureRoot, "state"))).resolves.not.toContain(await realpath(outer));
  });

  it("automatically enrols a plain non-Git directory exactly", async () => {
    const temporaryRoot = await mkdtemp(join(tmpdir(), "fabric-plain-bootstrap-"));
    temporaryDirectories.push(temporaryRoot);
    const root = await realpath(temporaryRoot);
    const project = join(root, "plain-project");
    await mkdir(project);

    const result = await bootstrapSuccess(project, root);

    expect(result.canonicalRoot).toBe(await realpath(project));
    await expect(recordedTrustPaths(join(root, "state"))).resolves.toEqual([await realpath(project)]);
  });

  it("withholds a trust command for a linked worktree", async () => {
    const temporaryRoot = await mkdtemp(join(tmpdir(), "fabric-worktree-bootstrap-"));
    temporaryDirectories.push(temporaryRoot);
    const mainRepository = join(temporaryRoot, "main");
    const worktree = join(temporaryRoot, "linked");
    const worktreeGitDirectory = join(mainRepository, ".git", "worktrees", "linked");
    await mkdir(worktreeGitDirectory, { recursive: true });
    await mkdir(worktree);
    await writeFile(join(worktree, ".git"), `gitdir: ${worktreeGitDirectory}\n`);
    const before = await recordedTrustPaths(join(temporaryRoot, "state"));

    const failure = await bootstrapFailure(await realpath(worktree), temporaryRoot);

    expect(failure).toMatchObject({
      code: "WORKSPACE_NOT_TRUSTED",
      message: expect.stringMatching(/linked worktree.*user-only/iu),
    });
    expect(failure.message).not.toContain("workspace trust");
    await expect(recordedTrustPaths(join(temporaryRoot, "state"))).resolves.toEqual(before);
  });

  it("withholds a trust command for a collection of repositories", async () => {
    const temporaryRoot = await mkdtemp(join(tmpdir(), "fabric-collection-bootstrap-"));
    temporaryDirectories.push(temporaryRoot);
    const collection = join(temporaryRoot, "projects");
    await mkdir(join(collection, "one", ".git"), { recursive: true });
    await mkdir(join(collection, "two", ".git"), { recursive: true });
    const before = await recordedTrustPaths(join(temporaryRoot, "state"));

    const failure = await bootstrapFailure(await realpath(collection), temporaryRoot);

    expect(failure).toMatchObject({
      code: "WORKSPACE_NOT_TRUSTED",
      message: expect.stringMatching(/parent collection.*several repositories.*policy.*specific project/iu),
    });
    expect(failure.message).not.toContain("workspace trust");
    await expect(recordedTrustPaths(join(temporaryRoot, "state"))).resolves.toEqual(before);
  });

  it("automatically enrols a repository with multiple top-level submodules", async () => {
    const temporaryRoot = await mkdtemp(join(tmpdir(), "fabric-submodule-bootstrap-"));
    temporaryDirectories.push(temporaryRoot);
    const root = await realpath(temporaryRoot);
    const repository = join(root, "repository");
    await mkdir(join(repository, ".git"), { recursive: true });
    for (const name of ["first-submodule", "second-submodule"]) {
      const submodule = join(repository, name);
      await mkdir(submodule);
      await writeFile(join(submodule, ".git"), "gitdir: ../.git/modules/child\n");
    }

    const result = await bootstrapSuccess(repository, root);

    expect(result.canonicalRoot).toBe(await realpath(repository));
    await expect(recordedTrustPaths(join(root, "state"))).resolves.toEqual([await realpath(repository)]);
  });

  it("does not replace an expired trust record during automatic enrolment", async () => {
    const temporaryRoot = await mkdtemp(join(tmpdir(), "fabric-expired-trust-bootstrap-"));
    temporaryDirectories.push(temporaryRoot);
    const root = await realpath(temporaryRoot);
    const repository = join(root, "repository root");
    await mkdir(join(repository, ".git"), { recursive: true });
    const issuedAt = new Date("2026-08-01T00:00:00.000Z");
    const expiredAt = new Date("2026-08-01T00:01:00.000Z");
    const pathsForTrust = paths(root);
    await runWorkspaceTrust(
      ["trust", repository, "--expires-at", expiredAt.toISOString()],
      pathsForTrust,
      issuedAt,
    );
    const registryPath = join(root, "state", "trusted-workspaces.json");
    const before = await readFile(registryPath, "utf8");

    const failure = await bootstrapFailure(
      repository,
      root,
      new Date("2026-08-01T00:02:00.000Z"),
    );

    expect(failure).toMatchObject({
      code: "WORKSPACE_NOT_TRUSTED",
      message: expect.stringContaining("workspace trust"),
    });
    expect(failure.message).toContain(`workspace trust '${repository}'`);
    await expect(readFile(registryPath, "utf8")).resolves.toBe(before);
  });

  it("returns a typed refusal when automatic trust registration cannot write", async () => {
    const temporaryRoot = await mkdtemp(join(tmpdir(), "fabric-trust-write-failure-"));
    temporaryDirectories.push(temporaryRoot);
    const project = join(temporaryRoot, "project");
    await mkdir(project);
    const blockedStatePath = join(temporaryRoot, "state");
    await writeFile(blockedStatePath, "blocked state path\n");

    const failure = await bootstrapFailure(project, temporaryRoot);

    expect(failure).toMatchObject({
      code: "WORKSPACE_NOT_TRUSTED",
      message: expect.stringContaining("workspace trust"),
    });
    await expect(readFile(blockedStatePath, "utf8")).resolves.toBe("blocked state path\n");
  });

  it("explains that home-wide trust is forbidden", async () => {
    const temporaryRoot = await mkdtemp(join(tmpdir(), "fabric-home-bootstrap-"));
    temporaryDirectories.push(temporaryRoot);
    const before = await recordedTrustPaths(join(temporaryRoot, "state"));

    const failure = await bootstrapFailure(await realpath(homedir()), temporaryRoot);

    expect(failure).toMatchObject({
      code: "WORKSPACE_NOT_TRUSTED",
      message: expect.stringMatching(/exact path can never be trusted.*home-wide trust.*forbidden by policy/iu),
    });
    expect(failure.message).not.toContain("workspace trust");
    await expect(recordedTrustPaths(join(temporaryRoot, "state"))).resolves.toEqual(before);
  });

  it("withholds a trust command when the repository root is home", async () => {
    const temporaryRoot = await mkdtemp(join(tmpdir(), "fabric-home-repository-bootstrap-"));
    temporaryDirectories.push(temporaryRoot);
    const home = join(temporaryRoot, "home");
    const project = join(home, "project");
    await mkdir(join(home, ".git"), { recursive: true });
    await mkdir(project);
    const previousHome = process.env.HOME;
    process.env.HOME = home;
    try {
      const before = await recordedTrustPaths(join(temporaryRoot, "state"));
      const failure = await bootstrapFailure(project, temporaryRoot);

      expect(failure).toMatchObject({
        code: "WORKSPACE_NOT_TRUSTED",
        message: expect.stringMatching(/exact path can never be trusted.*home-wide trust.*forbidden by policy/iu),
      });
      expect(failure.message).not.toContain("workspace trust");
      await expect(recordedTrustPaths(join(temporaryRoot, "state"))).resolves.toEqual(before);
    } finally {
      if (previousHome === undefined) delete process.env.HOME;
      else process.env.HOME = previousHome;
    }
  });

  it("withholds a trust command for the filesystem root", async () => {
    const temporaryRoot = await mkdtemp(join(tmpdir(), "fabric-root-bootstrap-"));
    temporaryDirectories.push(temporaryRoot);
    const before = await recordedTrustPaths(join(temporaryRoot, "state"));

    const failure = await bootstrapFailure(
      await realpath(parse(homedir()).root),
      temporaryRoot,
    );

    expect(failure).toMatchObject({
      code: "WORKSPACE_NOT_TRUSTED",
      message: expect.stringMatching(/filesystem root.*can never be trusted.*forbidden by policy/iu),
    });
    expect(failure.message).not.toContain("workspace trust");
    await expect(recordedTrustPaths(join(temporaryRoot, "state"))).resolves.toEqual(before);
  });

  it("withholds a trust command when the repository boundary cannot be inspected", async () => {
    const temporaryRoot = await mkdtemp(join(tmpdir(), "fabric-unreadable-git-bootstrap-"));
    temporaryDirectories.push(temporaryRoot);
    const project = join(temporaryRoot, "project");
    await mkdir(project);
    await symlink("missing-gitdir", join(project, ".git"));
    const before = await recordedTrustPaths(join(temporaryRoot, "state"));

    const failure = await bootstrapFailure(project, temporaryRoot);

    expect(failure).toMatchObject({
      code: "WORKSPACE_NOT_TRUSTED",
      message: expect.stringMatching(/cannot safely determine.*trust boundary.*no trust command/iu),
    });
    expect(failure.message).not.toContain("workspace trust");
    await expect(recordedTrustPaths(join(temporaryRoot, "state"))).resolves.toEqual(before);
  });

  it("withholds a trust command when a gitfile target is missing", async () => {
    const temporaryRoot = await mkdtemp(join(tmpdir(), "fabric-missing-gitdir-bootstrap-"));
    temporaryDirectories.push(temporaryRoot);
    const project = join(temporaryRoot, "project");
    await mkdir(project);
    await writeFile(join(project, ".git"), "gitdir: missing-gitdir\n");
    const before = await recordedTrustPaths(join(temporaryRoot, "state"));

    const failure = await bootstrapFailure(project, temporaryRoot);

    expect(failure).toMatchObject({
      code: "WORKSPACE_NOT_TRUSTED",
      message: expect.stringMatching(/cannot safely determine.*trust boundary.*no trust command/iu),
    });
    expect(failure.message).not.toContain("workspace trust");
    await expect(recordedTrustPaths(join(temporaryRoot, "state"))).resolves.toEqual(before);
  });
});
