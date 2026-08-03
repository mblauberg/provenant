import { chmod, mkdir, mkdtemp, realpath, rm, symlink, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { dirname, join } from "node:path";

import { describe, expect, it } from "vitest";

import { composeDaemonConfiguration } from "../../../src/daemon/composition.ts";
import { loadFabricConfig } from "../../../src/config/index.ts";
import { fabricStatus } from "../../../src/cli/status.ts";
import { runWorkspaceTrust, trustedProjectConfigPath } from "../../../src/cli/workspace-trust.ts";
import { openFabric } from "../../../src/index.ts";
import { createPortableActivatedPrimaryFixture } from "../../support/primary-adapter-testkit.ts";

async function writeYaml(path: string, value: string, mode = 0o600): Promise<void> {
  await mkdir(dirname(path), { recursive: true });
  await writeFile(path, value, { mode });
}

async function writeGlobalConfig(path: string, workspaceRoot: string): Promise<void> {
  await writeYaml(path, [
    "schemaVersion: 1",
    "allowedAdapters: [alpha, beta]",
    "activeAdapters: [alpha, beta]",
    "allowedProfiles: [headless, paired-visible]",
    `workspaceRoots: [${JSON.stringify(workspaceRoot)}]`,
    "limits:",
    "  maximumConcurrentProviderTurns: 8",
    "",
  ].join("\n"), 0o644);
}

async function trustedPaths(root: string): Promise<{
  stateDirectory: string;
  runtimeDirectory: string;
  databasePath: string;
  socketPath: string;
}> {
  const stateDirectory = join(root, "state");
  const runtimeDirectory = join(stateDirectory, "runtime");
  await mkdir(runtimeDirectory, { recursive: true, mode: 0o700 });
  return {
    stateDirectory,
    runtimeDirectory,
    databasePath: join(stateDirectory, "fabric-v1.sqlite3"),
    socketPath: join(runtimeDirectory, "fabric-v1.sock"),
  };
}

async function enroll(root: string, paths: Awaited<ReturnType<typeof trustedPaths>>): Promise<void> {
  await runWorkspaceTrust(["trust", root, "--profiles", "headless"], paths);
}

describe("enrolled .provenant project configuration", () => {
  it("loads the exact enrolled-root layer when the caller resolves it", async () => {
    const root = await mkdtemp(join(tmpdir(), "fabric-project-enrolled-"));
    const projectRoot = join(root, "project");
    const workingDirectory = join(projectRoot, "src", "nested");
    const globalPath = join(root, "global.yaml");
    await mkdir(workingDirectory, { recursive: true });
    await writeGlobalConfig(globalPath, projectRoot);
    const paths = await trustedPaths(root);
    await enroll(projectRoot, paths);
    const projectPath = join(projectRoot, ".provenant", "agent-fabric.yaml");
    await writeYaml(projectPath, "schemaVersion: 1\nlimits:\n  maximumConcurrentProviderTurns: 2\n");
    await writeYaml(join(workingDirectory, ".provenant", "agent-fabric.yaml"), "schemaVersion: 1\nlimits:\n  maximumConcurrentProviderTurns: 1\n");

    await expect(trustedProjectConfigPath({
      stateDirectory: paths.stateDirectory,
      projectRoot: workingDirectory,
    })).resolves.toBeUndefined();
    await expect(trustedProjectConfigPath({
      stateDirectory: paths.stateDirectory,
      projectRoot,
    })).resolves.toBe(await realpath(projectPath));

    const symlinkParent = join(root, "symlink-parent");
    await symlink(root, symlinkParent);
    await expect(trustedProjectConfigPath({
      stateDirectory: paths.stateDirectory,
      projectRoot: join(symlinkParent, "project"),
    })).resolves.toBe(await realpath(projectPath));
    await expect(loadFabricConfig({
      globalPath,
      projectPath: await realpath(projectPath),
    })).resolves.toMatchObject({
      limits: { maximumConcurrentProviderTurns: 2 },
      workspaceRoots: [await realpath(projectRoot)],
    });
  });

  it("does not discover from cwd or hostile ancestors when no project path is handed in", async () => {
    const root = await mkdtemp(join(tmpdir(), "fabric-project-no-discovery-"));
    const projectRoot = join(root, "project");
    const globalPath = join(root, "global.yaml");
    await mkdir(projectRoot, { recursive: true });
    await writeGlobalConfig(globalPath, projectRoot);
    await writeYaml(join(root, ".provenant", "agent-fabric.yaml"), "schemaVersion: 1\nlimits:\n  maximumConcurrentProviderTurns: 1\n");
    const paths = await trustedPaths(root);
    await enroll(projectRoot, paths);

    await expect(trustedProjectConfigPath({
      stateDirectory: paths.stateDirectory,
      projectRoot,
    })).resolves.toBeUndefined();
    await expect(loadFabricConfig({ globalPath })).resolves.toMatchObject({
      limits: { maximumConcurrentProviderTurns: 8 },
    });
  });

  it("uses an explicit project path even when another project-shaped file exists", async () => {
    const root = await mkdtemp(join(tmpdir(), "fabric-project-explicit-"));
    const globalPath = join(root, "global.yaml");
    const explicitPath = join(root, "selected.yaml");
    await writeGlobalConfig(globalPath, root);
    await writeYaml(join(root, ".provenant", "agent-fabric.yaml"), "schemaVersion: 1\nlimits:\n  maximumConcurrentProviderTurns: 1\n");
    await writeYaml(explicitPath, "schemaVersion: 1\nlimits:\n  maximumConcurrentProviderTurns: 2\n");

    await expect(loadFabricConfig({ globalPath, projectPath: explicitPath })).resolves.toMatchObject({
      limits: { maximumConcurrentProviderTurns: 2 },
    });
  });

  it("refuses an untrusted layer from selecting an execution profile", async () => {
    const root = await mkdtemp(join(tmpdir(), "fabric-project-profile-"));
    const globalPath = join(root, "global.yaml");
    const projectPath = join(root, "project.yaml");
    await writeGlobalConfig(globalPath, root);
    await writeYaml(projectPath, "schemaVersion: 1\nnamedExecutionProfile: paired-visible\n");

    await expect(loadFabricConfig({ globalPath, projectPath })).rejects.toMatchObject({
      code: "CONFIG_UNTRUSTED_FIELD",
      field: "namedExecutionProfile",
    });
  });

  it("rejects a trusted-root project layer that tries to widen a workspace root", async () => {
    const root = await mkdtemp(join(tmpdir(), "fabric-project-boundary-"));
    const projectRoot = join(root, "project");
    const globalPath = join(root, "global.yaml");
    await mkdir(projectRoot, { recursive: true });
    await writeGlobalConfig(globalPath, projectRoot);
    const paths = await trustedPaths(root);
    await enroll(projectRoot, paths);
    const projectPath = join(projectRoot, ".provenant", "agent-fabric.yaml");
    await writeYaml(projectPath, "schemaVersion: 1\nworkspaceRoots: [\"/\"]\n");
    const resolved = await trustedProjectConfigPath({ stateDirectory: paths.stateDirectory, projectRoot });
    if (resolved === undefined) throw new Error("enrolled project configuration was not resolved");

    await expect(loadFabricConfig({ globalPath, projectPath: resolved })).rejects.toMatchObject({
      code: "CONFIG_WIDENING_FORBIDDEN",
    });
  });

  it("makes status use the same enrolled project layer and trusted roots", async () => {
    const root = await mkdtemp(join(tmpdir(), "fabric-project-status-"));
    const projectRoot = join(root, "project");
    const globalPath = join(root, "global.yaml");
    await mkdir(projectRoot, { recursive: true });
    await writeGlobalConfig(globalPath, projectRoot);
    const paths = await trustedPaths(root);
    await enroll(projectRoot, paths);
    const fabric = await openFabric({ databasePath: paths.databasePath, workspaceRoots: [projectRoot] });
    await fabric.close();
    await writeYaml(join(projectRoot, ".provenant", "agent-fabric.yaml"), "schemaVersion: 1\nallowListedAdapterId: alpha\n");

    const status = await fabricStatus([
      "--agents-home", root,
      "--trusted-config", globalPath,
      "--project", projectRoot,
    ], paths);
    expect(status).toMatchObject({
      configuredAdapters: ["alpha"],
      trustedWorkspaceRoots: [await realpath(projectRoot)],
      project: { path: projectRoot },
    });
  });

  it("makes daemon composition use the explicit project root rather than its process cwd", async () => {
    const root = await mkdtemp(join(tmpdir(), "fabric-project-daemon-"));
    const projectRoot = join(root, "project");
    const globalPath = join(root, "global.yaml");
    const compatibility = await createPortableActivatedPrimaryFixture();
    await mkdir(projectRoot, { recursive: true });
    await writeYaml(globalPath, [
      "schemaVersion: 1",
      "allowedAdapters: []",
      "activeAdapters: []",
      "allowedProfiles: [headless]",
      `workspaceRoots: [${JSON.stringify(projectRoot)}]`,
      "limits:",
      "  maximumConcurrentProviderTurns: 8",
      "",
    ].join("\n"), 0o644);
    await writeYaml(join(root, ".provenant", "agent-fabric.yaml"), "schemaVersion: 1\nlimits:\n  maximumConcurrentProviderTurns: 1\n");
    const paths = await trustedPaths(root);
    await enroll(projectRoot, paths);
    await writeYaml(join(projectRoot, ".provenant", "agent-fabric.yaml"), "schemaVersion: 1\nlimits:\n  maximumConcurrentProviderTurns: 2\n");

    await expect(composeDaemonConfiguration({
      globalConfigPath: globalPath,
      projectRoot,
      compatibilityPath: compatibility.compatibilityPath,
      compatibilitySchemaPath: compatibility.schemaPath,
      agentsHome: root,
      stateDirectory: paths.stateDirectory,
    })).resolves.toMatchObject({
      maximumConcurrentProviderTurns: 2,
      workspaceRoots: [await realpath(projectRoot)],
    });
    await rm(compatibility.directory, { recursive: true, force: true });
  });

  it("refuses symlinked project directories and files", async () => {
    const root = await mkdtemp(join(tmpdir(), "fabric-project-symlink-"));
    const projectRoot = join(root, "project");
    const outside = join(root, "outside");
    const paths = await trustedPaths(root);
    await mkdir(projectRoot, { recursive: true });
    await mkdir(outside, { recursive: true });
    await enroll(projectRoot, paths);
    await writeYaml(join(outside, "agent-fabric.yaml"), "schemaVersion: 1\n");
    await symlink(outside, join(projectRoot, ".provenant"));
    await expect(trustedProjectConfigPath({ stateDirectory: paths.stateDirectory, projectRoot })).rejects.toThrow(/non-symlink/u);

    await rm(join(projectRoot, ".provenant"), { recursive: true, force: true });
    await mkdir(join(projectRoot, ".provenant"));
    await symlink(join(outside, "agent-fabric.yaml"), join(projectRoot, ".provenant", "agent-fabric.yaml"));
    await expect(trustedProjectConfigPath({ stateDirectory: paths.stateDirectory, projectRoot })).rejects.toThrow(/private regular file/u);
  });

  it("fails closed on an inaccessible project directory", async () => {
    const root = await mkdtemp(join(tmpdir(), "fabric-project-inaccessible-"));
    const projectRoot = join(root, "project");
    const paths = await trustedPaths(root);
    await mkdir(join(projectRoot, ".provenant"), { recursive: true });
    await enroll(projectRoot, paths);
    await chmod(join(projectRoot, ".provenant"), 0o000);
    try {
      await expect(trustedProjectConfigPath({ stateDirectory: paths.stateDirectory, projectRoot })).rejects.toMatchObject({
        code: "EACCES",
      });
    } finally {
      await chmod(join(projectRoot, ".provenant"), 0o700);
    }
  });

  it("rejects a project file that is not private", async () => {
    const root = await mkdtemp(join(tmpdir(), "fabric-project-permissions-"));
    const projectRoot = join(root, "project");
    const globalPath = join(root, "global.yaml");
    const paths = await trustedPaths(root);
    await mkdir(projectRoot, { recursive: true });
    await enroll(projectRoot, paths);
    await writeGlobalConfig(globalPath, projectRoot);
    await writeYaml(join(projectRoot, ".provenant", "agent-fabric.yaml"), "schemaVersion: 1\n", 0o644);

    await expect(trustedProjectConfigPath({ stateDirectory: paths.stateDirectory, projectRoot })).rejects.toThrow(/private regular file/u);
    await expect(loadFabricConfig({
      globalPath,
      projectPath: join(projectRoot, ".provenant", "agent-fabric.yaml"),
    })).rejects.toThrow(/private regular file/u);
  });

  it("rejects a project file above the byte limit before YAML parsing", async () => {
    const root = await mkdtemp(join(tmpdir(), "fabric-project-size-"));
    const globalPath = join(root, "global.yaml");
    const projectPath = join(root, ".provenant", "agent-fabric.yaml");
    await writeGlobalConfig(globalPath, root);
    await writeYaml(projectPath, `schemaVersion: 1\n#${"x".repeat(65 * 1024)}\n`);

    await expect(loadFabricConfig({ globalPath, projectPath })).rejects.toThrow(/size limit/u);
  });
});
