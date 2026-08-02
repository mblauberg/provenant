import { access, chmod, lstat, mkdir, mkdtemp, readFile, realpath, rename, rm, symlink, writeFile } from "node:fs/promises";
import { homedir, tmpdir } from "node:os";
import { join } from "node:path";
import { promisify } from "node:util";
import { execFile } from "node:child_process";

import { afterEach, describe, expect, it, vi } from "vitest";

import { runProjectActivate, runProjectStatus, resolveProjectRoots } from "../../src/cli/project.ts";
import { runWorkspaceTrust, trustedWorkspaceIdentity } from "../../src/cli/workspace-trust.ts";
import { projectKey } from "../../src/cli/seat-store.ts";
import { parseCliJson, runSourceCli } from "../support/cli-process.ts";

const execFileAsync = promisify(execFile);
const temporaryDirectories: string[] = [];

async function fixture() {
  const root = await mkdtemp(join(tmpdir(), "fabric-project-cli-"));
  temporaryDirectories.push(root);
  const stateDirectory = join(root, "state");
  const runtimeDirectory = join(stateDirectory, "runtime");
  const project = join(root, "project");
  await mkdir(runtimeDirectory, { recursive: true, mode: 0o700 });
  await mkdir(project, { mode: 0o700 });
  await chmod(stateDirectory, 0o700);
  return {
    root,
    project,
    paths: {
      stateDirectory,
      runtimeDirectory,
      databasePath: join(stateDirectory, "fabric-v1.sqlite3"),
      socketPath: join(runtimeDirectory, "fabric-v1.sock"),
    },
  };
}

afterEach(async () => {
  await Promise.all(temporaryDirectories.splice(0).map(async (path) => await rm(path, { recursive: true, force: true })));
});

describe("project activation front doors", () => {
  it("trusts a non-Git directory through the existing workspace trust primitive", async () => {
    const value = await fixture();

    const result = await runProjectActivate(value.project, value.paths);

    expect(result).toMatchObject({
      action: "trusted",
      trusted: true,
      trustedRoot: await realpath(value.project),
      canonicalRepositoryRoot: await realpath(value.project),
      isGitRepository: false,
      gitProbe: "not-repository",
      gitProbeError: null,
      seatExists: false,
      fabricReady: false,
      fabricReadiness: "bootstrap a Fabric seat after activation",
      trustedWorkspaceRoots: [await realpath(value.project)],
      missingDependencies: ["active Fabric seat"],
    });
    expect(result.message).toContain("Trusted project root");
  });

  it("activates a nested non-Git directory exactly without inheriting a parent marker", async () => {
    const value = await fixture();
    const requested = join(value.project, "src");
    await mkdir(requested, { recursive: true, mode: 0o700 });
    await mkdir(join(value.project, ".provenant"), { mode: 0o700 });
    await writeFile(join(value.project, ".provenant", "agent-fabric.yaml"), "schemaVersion: 1\n");

    await expect(runProjectActivate(requested, value.paths)).resolves.toMatchObject({
      requestedPath: await realpath(requested),
      canonicalRepositoryRoot: await realpath(requested),
      trustedRoot: await realpath(requested),
      isGitRepository: false,
    });
  });

  it("reports an already trusted exact root as a clean no-op", async () => {
    const value = await fixture();
    const first = await runProjectActivate(value.project, value.paths);

    const second = await runProjectActivate(value.project, value.paths, new Date("2026-07-31T01:00:00.000Z"));

    expect(second).toMatchObject({
      action: "already-trusted",
      trustedRoot: first.trustedRoot,
      message: expect.stringContaining("already trusted; no changes made"),
    });
    expect(second.trustRecordDigest).toBe(first.trustRecordDigest);
    expect(second.trustRecordDigest).not.toBeNull();
  });

  it("keeps the canonical Git root separate from the requested project identity", async () => {
    const value = await fixture();
    const repositoryRoot = join(value.root, "repository");
    const configRoot = join(repositoryRoot, "nested-project");
    const workingDirectory = join(configRoot, "src");
    await mkdir(workingDirectory, { recursive: true, mode: 0o700 });
    await mkdir(join(configRoot, ".provenant"), { mode: 0o700 });
    await execFileAsync("git", ["init", "--quiet", repositoryRoot]);

    await expect(resolveProjectRoots(workingDirectory)).resolves.toEqual({
      requestedPath: await realpath(workingDirectory),
      canonicalRepositoryRoot: await realpath(repositoryRoot),
      isGitRepository: true,
      gitProbe: "repository",
      gitProbeError: null,
    });
  });

  it("refuses Git-root widening and leaves the trust registry unchanged", async () => {
    const value = await fixture();
    await runProjectActivate(value.project, value.paths);
    const registryPath = join(value.paths.stateDirectory, "trusted-workspaces.json");
    const registryBefore = await readFile(registryPath, "utf8");
    const repositoryRoot = join(value.root, "repository");
    const requestedProject = join(repositoryRoot, "nested-project");
    await mkdir(requestedProject, { recursive: true, mode: 0o700 });
    await execFileAsync("git", ["init", "--quiet", repositoryRoot]);
    const canonicalRequestedProject = await realpath(requestedProject);
    const canonicalRepositoryRoot = await realpath(repositoryRoot);

    await expect(runProjectActivate(requestedProject, value.paths)).rejects.toThrow(
      `requested project root ${canonicalRequestedProject} differs from Git repository root ${canonicalRepositoryRoot}; ` +
      `to trust the requested project deliberately, run provenant fabric workspace trust ${canonicalRequestedProject}; ` +
      `to trust the repository deliberately, run provenant fabric workspace trust ${canonicalRepositoryRoot}; no trust was added.`,
    );
    await expect(readFile(registryPath, "utf8")).resolves.toBe(registryBefore);
  });

  it("reports trust for the named directory separately from its trusted Git ancestor", async () => {
    const value = await fixture();
    const repositoryRoot = join(value.root, "repository");
    const requestedProject = join(repositoryRoot, "nested-project");
    await mkdir(requestedProject, { recursive: true, mode: 0o700 });
    await execFileAsync("git", ["init", "--quiet", repositoryRoot]);
    await runProjectActivate(repositoryRoot, value.paths);
    const repositoryPath = await realpath(repositoryRoot);
    const seatDirectory = join(value.paths.stateDirectory, "seats", projectKey(repositoryPath));
    await mkdir(seatDirectory, { recursive: true, mode: 0o700 });
    await writeFile(join(seatDirectory, "current.json"), `${JSON.stringify({
      schemaVersion: 1,
      projectKey: projectKey(repositoryPath),
      previousGeneration: null,
      generation: "a".repeat(64),
    })}\n`, { mode: 0o600 });

    const result = await runProjectStatus(requestedProject, value.paths);

    expect(result).toMatchObject({
      requestedPath: await realpath(requestedProject),
      canonicalRepositoryRoot: await realpath(repositoryRoot),
      trusted: false,
      status: "untrusted",
      trustedRoot: null,
      repositoryRootTrusted: true,
      trustedWorkspaceRoots: [await realpath(repositoryRoot)],
      seatExists: true,
      seat: { exists: true, generation: "a".repeat(64) },
      fabricReady: false,
      fabricReadiness: "trust the exact requested project directory before using Fabric",
    });
  });

  it("reports expired and identity-drifted trust records as untrusted status", async () => {
    const expired = await fixture();
    await runWorkspaceTrust(
      ["trust", expired.project, "--expires-at", "2020-01-01T00:00:00.000Z"],
      expired.paths,
      new Date("2019-01-01T00:00:00.000Z"),
    );
    await expect(runProjectStatus(expired.project, expired.paths)).resolves.toMatchObject({
      status: "untrusted",
      trusted: false,
      trustedRoot: null,
      missingDependencies: expect.arrayContaining(["workspace trust"]),
    });

    const drifted = await fixture();
    await runProjectActivate(drifted.project, drifted.paths);
    await rename(drifted.project, join(drifted.root, "project-original"));
    await mkdir(drifted.project, { mode: 0o700 });
    await expect(runProjectStatus(drifted.project, drifted.paths)).resolves.toMatchObject({
      status: "untrusted",
      trusted: false,
      trustedRoot: null,
      missingDependencies: expect.arrayContaining(["workspace trust"]),
    });
  });

  it("reports a trusted root that becomes an unmarked repository collection as untrusted status", async () => {
    const value = await fixture();
    await runProjectActivate(value.project, value.paths);
    await mkdir(join(value.project, "first-repo", ".git"), { recursive: true, mode: 0o700 });
    await mkdir(join(value.project, "second-repo", ".git"), { recursive: true, mode: 0o700 });

    await expect(runProjectStatus(value.project, value.paths)).resolves.toMatchObject({
      status: "untrusted",
      trusted: false,
      trustedRoot: null,
      fabricReady: false,
      missingDependencies: expect.arrayContaining(["workspace trust"]),
    });
  });

  it("keeps a seeded legacy trust record aligned with project custody digest", async () => {
    const value = await fixture();
    const now = new Date("2026-07-11T04:00:00.000Z");
    const canonicalPath = await realpath(value.project);
    const identity = await lstat(canonicalPath);
    const registryPath = join(value.paths.stateDirectory, "trusted-workspaces.json");
    await writeFile(registryPath, `${JSON.stringify({
      schemaVersion: 1,
      entries: [{
        canonicalPath,
        approvedAt: now.toISOString(),
        approvedBy: "local-operator",
        device: identity.dev,
        inode: identity.ino,
        allowedProfiles: ["headless"],
      }],
    }, null, 2)}\n`);
    await chmod(registryPath, 0o600);
    const direct = await runProjectStatus(value.project, value.paths);
    const identityResult = await trustedWorkspaceIdentity({
      stateDirectory: value.paths.stateDirectory,
      canonicalRoot: value.project,
    });

    expect(direct).toMatchObject({
      status: "trusted",
      trusted: true,
      trustedRoot: canonicalPath,
      trustRecordDigest: identityResult.trustRecordDigest,
      fabricReady: false,
      fabricReadiness: "bootstrap a Fabric seat after activation",
      missingDependencies: ["active Fabric seat"],
    });
  });

  it.each([
    "workspace trust record is expired",
    "workspace trust record does not allow the requested profile",
    "workspace trust record no longer matches the live root identity",
  ])("classifies %s as untrusted project status", async (message) => {
    const value = await fixture();
    await expect(runProjectStatus(value.project, value.paths, {
      identity: async () => {
        throw new Error(message);
      },
    })).resolves.toMatchObject({
      status: "untrusted",
      trusted: false,
      trustedRoot: null,
      missingDependencies: expect.arrayContaining(["workspace trust"]),
    });
  });

  it("does not swallow an unexpected trust inspection error", async () => {
    const value = await fixture();

    await expect(runProjectStatus(value.project, value.paths, {
      identity: async () => {
        throw new Error("unexpected trust inspection failure");
      },
    })).rejects.toThrow(/project status could not inspect.*unexpected trust inspection failure/u);
  });

  it("refuses activation when the Git repository probe is unavailable", async () => {
    const value = await fixture();
    vi.stubEnv("PATH", value.root);
    try {
      const roots = await resolveProjectRoots(value.project);
      expect(roots.gitProbe).toBe("unavailable");
      expect(roots.gitProbeError).not.toBeNull();
      await expect(runProjectActivate(value.project, value.paths)).rejects.toThrow(
        `project activation refused: Git repository probe was unavailable (${roots.gitProbeError}); ` +
        `to trust the requested project deliberately, run provenant fabric workspace trust ${roots.requestedPath}; no trust was added.`,
      );
      await expect(access(join(value.paths.stateDirectory, "trusted-workspaces.json"))).rejects.toThrow();
    } finally {
      vi.unstubAllEnvs();
    }
  });

  it("reports an unavailable Git repository probe in project status", async () => {
    const value = await fixture();
    vi.stubEnv("PATH", value.root);
    try {
      await expect(runProjectStatus(value.project, value.paths)).resolves.toMatchObject({
        isGitRepository: false,
        gitProbe: "unavailable",
        gitProbeError: expect.stringContaining("git"),
        missingDependencies: expect.arrayContaining(["Git repository probe unavailable"]),
      });
    } finally {
      vi.unstubAllEnvs();
    }
  });

  it("does not pass an explicitly symlinked project path to the trust writer", async () => {
    const value = await fixture();
    const linked = join(value.root, "linked-project");
    await symlink(value.project, linked);
    const received: string[] = [];

    await expect(runProjectActivate(linked, value.paths, new Date(), {
      trust: async (arguments_, paths, now) => {
        received.push(arguments_[1] ?? "");
        return await runWorkspaceTrust(arguments_, paths, now);
      },
    })).rejects.toThrow(/symbolic link/u);
    expect(received).toEqual([]);
  });

  it("revokes a trust record if the trust owner reports a different canonical path", async () => {
    const value = await fixture();
    const substitutedDirectory = join(value.root, "substituted");
    await mkdir(substitutedDirectory, { mode: 0o700 });
    const substituted = await realpath(substitutedDirectory);

    await expect(runProjectActivate(value.project, value.paths, new Date(), {
      trust: async (_arguments_, paths, now) => await runWorkspaceTrust(["trust", substituted], paths, now),
    })).rejects.toThrow(/trust recorded.*requested path/u);

    const registry = JSON.parse(await readFile(join(value.paths.stateDirectory, "trusted-workspaces.json"), "utf8")) as {
      entries: { canonicalPath: string }[];
    };
    expect(registry.entries).toEqual([]);
  });

  it("does not revoke pre-existing trust when the trust owner reports a mismatch", async () => {
    const value = await fixture();
    const substitutedDirectory = join(value.root, "pre-existing-substituted");
    await mkdir(substitutedDirectory, { mode: 0o700 });
    const substituted = await realpath(substitutedDirectory);
    const revoked: string[] = [];

    await expect(runProjectActivate(value.project, value.paths, new Date(), {
      trust: async () => ({ trusted: true, alreadyTrusted: true, entry: { canonicalPath: substituted } }),
      revoke: async (arguments_: string[]) => {
        revoked.push(arguments_[1] ?? "");
        return { revoked: true };
      },
    })).rejects.toThrow(/trust was not revoked because it pre-existed/u);
    expect(revoked).toEqual([]);
  });

  it("refuses a mismatch without a canonical path and revokes nothing", async () => {
    const value = await fixture();
    const revoked: string[] = [];

    await expect(runProjectActivate(value.project, value.paths, new Date(), {
      trust: async () => ({ trusted: true }),
      revoke: async (arguments_: string[]) => {
        revoked.push(arguments_[1] ?? "");
        return { revoked: true };
      },
    })).rejects.toThrow(/no canonical path was recorded; trust was not revoked/u);
    expect(revoked).toEqual([]);
  });

  it("reports rollback when status inspection fails after adding trust", async () => {
    const value = await fixture();

    await expect(runProjectActivate(value.project, value.paths, new Date(), {
      status: async () => {
        throw new Error("unexpected status inspection failure");
      },
    })).rejects.toThrow(/trust was added.*revoked.*unexpected status inspection failure/u);
    await expect(runWorkspaceTrust(["list"], value.paths)).resolves.toMatchObject({ entries: [] });
  });

  it("does not report successful activation when the live trust identity is replaced", async () => {
    const value = await fixture();
    const originalDirectory = join(value.root, "identity-original");
    const replacementDirectory = join(value.root, "identity-replacement");
    await mkdir(replacementDirectory, { mode: 0o700 });

    await expect(runProjectActivate(value.project, value.paths, new Date(), {
      trust: async (arguments_, paths, now) => {
        const result = await runWorkspaceTrust(arguments_, paths, now);
        await rename(value.project, originalDirectory);
        await rename(replacementDirectory, value.project);
        return result;
      },
    })).rejects.toThrow(
      /project activation failed after trust was added; trust was revoked: project status reported that workspace trust is not live/u,
    );
    await expect(runWorkspaceTrust(["list"], value.paths)).resolves.toMatchObject({ entries: [] });
  });

  it("rolls back when the guarded path becomes a symlink after trust is recorded", async () => {
    const value = await fixture();
    const originalDirectory = join(value.root, "project-original");
    const replacementDirectory = join(value.root, "project-replacement");
    await mkdir(replacementDirectory, { mode: 0o700 });

    await expect(runProjectActivate(value.project, value.paths, new Date(), {
      trust: async (arguments_, paths, now) => {
        const result = await runWorkspaceTrust(arguments_, paths, now);
        await rename(value.project, originalDirectory);
        await symlink(replacementDirectory, value.project);
        return result;
      },
    })).rejects.toThrow(/trust was added.*revoked.*symbolic-link/u);
    await expect(runWorkspaceTrust(["list"], value.paths)).resolves.toMatchObject({ entries: [] });
  });

  it("refuses unsupported filesystem-wide roots with an actionable message", async () => {
    const value = await fixture();

    await expect(runProjectActivate(homedir(), value.paths)).rejects.toThrow(
      /project activation refused.*(filesystem-root|home-wide authority).*no trust was added/u,
    );
  });

  it("does not create Fabric state when home-wide activation is refused", async () => {
    const value = await fixture();
    const stateDirectory = join(value.root, "refused-home-state");
    const runtimeDirectory = join(value.root, "refused-home-runtime");
    const paths = {
      stateDirectory,
      runtimeDirectory,
      databasePath: join(stateDirectory, "fabric-v1.sqlite3"),
      socketPath: join(runtimeDirectory, "fabric-v1.sock"),
    };

    await expect(runProjectActivate(homedir(), paths)).rejects.toThrow();
    await expect(access(stateDirectory), "D5: refused home-wide activation must not create the Fabric state directory").rejects.toThrow();
    await expect(access(runtimeDirectory), "D5: refused home-wide activation must not create the Fabric runtime directory").rejects.toThrow();
  });

  it("does not create Fabric state when symlink-root activation is refused", async () => {
    const value = await fixture();
    const stateDirectory = join(value.root, "refused-symlink-state");
    const runtimeDirectory = join(value.root, "refused-symlink-runtime");
    const paths = {
      stateDirectory,
      runtimeDirectory,
      databasePath: join(stateDirectory, "fabric-v1.sqlite3"),
      socketPath: join(runtimeDirectory, "fabric-v1.sock"),
    };
    const originalDirectory = join(value.root, "symlink-original");
    const replacementDirectory = join(value.root, "symlink-replacement");
    await mkdir(replacementDirectory, { mode: 0o700 });

    await expect(runProjectActivate(value.project, paths, new Date(), {
      trust: async (arguments_, trustPaths, now) => {
        await rename(value.project, originalDirectory);
        await symlink(replacementDirectory, value.project);
        return await runWorkspaceTrust(arguments_, trustPaths, now);
      },
    })).rejects.toThrow(/symbolic-link root/u);
    await expect(access(stateDirectory), "D5: refused symlink-root activation must not create the Fabric state directory").rejects.toThrow();
    await expect(access(runtimeDirectory), "D5: refused symlink-root activation must not create the Fabric runtime directory").rejects.toThrow();
  });

  it("refuses an explicitly symlinked project path through both public project APIs", async () => {
    const value = await fixture();
    const linked = join(value.root, "linked-project");
    await symlink(value.project, linked);

    await expect(resolveProjectRoots(linked)).rejects.toThrow(/symbolic link/u);
    await expect(runProjectActivate(linked, value.paths)).rejects.toThrow(/symbolic link/u);
  });

  it("canonicalizes a lexical project-path alias before activation", async () => {
    const value = await fixture();

    await expect(runProjectActivate(`${value.project}/..`, value.paths)).resolves.toMatchObject({
      action: "trusted",
      requestedPath: await realpath(value.root),
      trustedRoot: await realpath(value.root),
    });
  });

  it("keeps status read-only and reports missing trust and seat dependencies", async () => {
    const value = await fixture();

    const result = await runProjectStatus(value.project, value.paths);

    expect(result).toMatchObject({
      status: "untrusted",
      trusted: false,
      trustedRoot: null,
      seatExists: false,
      fabricReady: false,
      missingDependencies: ["workspace trust", "active Fabric seat"],
    });
    await expect(access(join(value.paths.stateDirectory, "trusted-workspaces.json"))).rejects.toThrow();
  });

  it("exposes project status through the main CLI dispatch", async () => {
    const value = await fixture();
    const result = await runSourceCli(["project", "status", value.project], {
      environment: {
        AGENT_FABRIC_STATE_DIRECTORY: value.paths.stateDirectory,
        AGENT_FABRIC_RUNTIME_DIRECTORY: value.paths.runtimeDirectory,
      },
    });

    expect(result.exitCode).toBe(0);
    expect(parseCliJson(result)).toMatchObject({
      canonicalRepositoryRoot: await realpath(value.project),
      status: "untrusted",
      fabricReady: false,
    });
  });

  it("rejects a project CLI invocation with more than one path", async () => {
    const value = await fixture();
    const result = await runSourceCli(["project", "status", value.project, "extra"], {
      environment: {
        AGENT_FABRIC_STATE_DIRECTORY: value.paths.stateDirectory,
        AGENT_FABRIC_RUNTIME_DIRECTORY: value.paths.runtimeDirectory,
      },
    });

    expect(result.exitCode).not.toBe(0);
    expect(result.stderr).toContain("project status accepts at most one path");
  });

  it("rejects a file path before project activation can grant trust", async () => {
    const value = await fixture();
    const file = join(value.root, "not-a-directory");
    await writeFile(file, "not a directory");

    await expect(runProjectActivate(file, value.paths)).rejects.toThrow(
      /project path is not a directory/u,
    );
  });

  it("activates through the main CLI dispatch and records the trusted root", async () => {
    const value = await fixture();
    const result = await runSourceCli(["project", "activate", value.project], {
      environment: {
        AGENT_FABRIC_STATE_DIRECTORY: value.paths.stateDirectory,
        AGENT_FABRIC_RUNTIME_DIRECTORY: value.paths.runtimeDirectory,
      },
    });

    expect(result.exitCode).toBe(0);
    expect(parseCliJson(result)).toMatchObject({
      action: "trusted",
      trusted: true,
      trustedRoot: await realpath(value.project),
    });
    expect(JSON.parse(await readFile(join(value.paths.stateDirectory, "trusted-workspaces.json"), "utf8"))).toMatchObject({
      entries: [expect.objectContaining({ canonicalPath: await realpath(value.project) })],
    });
  });

  it("does not create Fabric state when a Git-root-widening activation is refused", async () => {
    const value = await fixture();
    const stateDirectory = join(value.root, "not-created-state");
    const runtimeDirectory = join(value.root, "not-created-runtime");
    const repositoryRoot = join(value.root, "repository");
    const requestedProject = join(repositoryRoot, "nested-project");
    await mkdir(requestedProject, { recursive: true, mode: 0o700 });
    await execFileAsync("git", ["init", "--quiet", repositoryRoot]);

    const result = await runSourceCli(["project", "activate", requestedProject], {
      environment: {
        AGENT_FABRIC_STATE_DIRECTORY: stateDirectory,
        AGENT_FABRIC_RUNTIME_DIRECTORY: runtimeDirectory,
      },
    });

    expect(result.exitCode).not.toBe(0);
    await expect(access(stateDirectory)).rejects.toThrow();
    await expect(access(runtimeDirectory)).rejects.toThrow();
  });
});
