import { access, chmod, mkdir, mkdtemp, readFile, realpath, rm, symlink } from "node:fs/promises";
import { homedir, tmpdir } from "node:os";
import { join } from "node:path";
import { promisify } from "node:util";
import { execFile } from "node:child_process";

import { afterEach, describe, expect, it } from "vitest";

import { runProjectActivate, runProjectStatus, resolveProjectRoots } from "../../src/cli/project.ts";
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
      seatExists: false,
      fabricReady: false,
      missingDependencies: ["active Fabric seat"],
    });
    expect(result.message).toContain("Trusted project root");
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

  it("does not report an inert config root", async () => {
    const value = await fixture();

    await expect(resolveProjectRoots(value.project)).resolves.not.toHaveProperty("configRoot");
  });

  it("refuses unsupported filesystem-wide roots with an actionable message", async () => {
    const value = await fixture();

    await expect(runProjectActivate(homedir(), value.paths)).rejects.toThrow(
      /project activation refused.*(filesystem-root|home-wide authority).*no trust was added/u,
    );
  });

  it("preserves the trust owner's symbolic-link-root refusal", async () => {
    const value = await fixture();
    const linked = join(value.root, "linked-project");
    await symlink(value.project, linked);

    await expect(runProjectActivate(linked, value.paths)).rejects.toThrow(
      /symbolic-link root/u,
    );
  });

  it("preserves the trust owner's lexical ancestor refusal", async () => {
    const value = await fixture();

    await expect(runProjectActivate(`${value.project}/..`, value.paths)).rejects.toThrow(
      /lexical ancestor broadening/u,
    );
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
});
