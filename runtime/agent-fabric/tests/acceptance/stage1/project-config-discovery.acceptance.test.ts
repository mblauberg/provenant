import { existsSync } from "node:fs";
import { mkdir, mkdtemp, realpath, symlink, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";

import { describe, expect, it } from "vitest";

import { discoverProjectConfigPath, loadFabricConfig } from "../../../src/config/index.ts";

async function writeYaml(path: string, value: string): Promise<void> {
  await mkdir(join(path, ".."), { recursive: true });
  await writeFile(path, value);
}

async function writeGlobalConfig(path: string, workspaceRoot: string): Promise<void> {
  await writeYaml(path, [
    "schemaVersion: 1",
    "allowedProfiles: [headless, paired-visible]",
    `workspaceRoots: [${JSON.stringify(workspaceRoot)}]`,
    "limits:",
    "  maximumConcurrentProviderTurns: 8",
    "",
  ].join("\n"));
}

describe(".provenant project configuration discovery", () => {
  it("discovers the nearest project layer and applies it through the untrusted merge", async () => {
    const root = await mkdtemp(join(tmpdir(), "fabric-project-discovery-"));
    const projectRoot = join(root, "project");
    const workingDirectory = join(projectRoot, "src", "nested");
    const globalPath = join(root, "global.yaml");
    const projectPath = join(projectRoot, ".provenant", "agent-fabric.yaml");
    await mkdir(workingDirectory, { recursive: true });
    await writeGlobalConfig(globalPath, projectRoot);
    await writeYaml(projectPath, [
      "schemaVersion: 1",
      "namedExecutionProfile: paired-visible",
      "limits:",
      "  maximumConcurrentProviderTurns: 2",
      "",
    ].join("\n"));

    expect(discoverProjectConfigPath(workingDirectory)).toBe(await realpath(projectPath));
    await expect(loadFabricConfig({ globalPath, workingDirectory })).resolves.toMatchObject({
      executionProfile: "paired-visible",
      limits: { maximumConcurrentProviderTurns: 2 },
    });
  });

  it("does not discover a project layer when no ancestor has .provenant/", async () => {
    const root = await mkdtemp(join(tmpdir(), "fabric-project-no-discovery-"));
    const workingDirectory = join(root, "project", "src");
    const globalPath = join(root, "global.yaml");
    await mkdir(workingDirectory, { recursive: true });
    await writeGlobalConfig(globalPath, join(root, "project"));

    expect(discoverProjectConfigPath(workingDirectory)).toBeUndefined();
    await expect(loadFabricConfig({ globalPath, workingDirectory })).resolves.toMatchObject({
      limits: { maximumConcurrentProviderTurns: 8 },
    });
  });

  it("treats a nearest empty .provenant/ directory as a no-op and stops there", async () => {
    const root = await mkdtemp(join(tmpdir(), "fabric-project-empty-"));
    const outerRoot = join(root, "outer");
    const innerRoot = join(outerRoot, "inner");
    const workingDirectory = join(innerRoot, "src");
    const globalPath = join(root, "global.yaml");
    await mkdir(join(outerRoot, ".provenant"), { recursive: true });
    await mkdir(workingDirectory, { recursive: true });
    await writeGlobalConfig(globalPath, outerRoot);
    await writeYaml(join(outerRoot, ".provenant", "agent-fabric.yaml"), "schemaVersion: 1\nnamedExecutionProfile: paired-visible\n");
    await mkdir(join(innerRoot, ".provenant"));

    expect(discoverProjectConfigPath(workingDirectory)).toBeUndefined();
    await expect(loadFabricConfig({ globalPath, workingDirectory })).resolves.toMatchObject({
      limits: { maximumConcurrentProviderTurns: 8 },
    });
  });

  it("rejects malformed discovered YAML through the project layer", async () => {
    const root = await mkdtemp(join(tmpdir(), "fabric-project-malformed-"));
    const projectRoot = join(root, "project");
    const workingDirectory = join(projectRoot, "src");
    const globalPath = join(root, "global.yaml");
    await mkdir(workingDirectory, { recursive: true });
    await writeGlobalConfig(globalPath, projectRoot);
    await writeYaml(join(projectRoot, ".provenant", "agent-fabric.yaml"), "schemaVersion: [\n");

    await expect(loadFabricConfig({ globalPath, workingDirectory })).rejects.toThrow();
  });

  it("accepts a discovered narrowing but refuses a widened workspace root", async () => {
    const root = await mkdtemp(join(tmpdir(), "fabric-project-boundary-"));
    const projectRoot = join(root, "project");
    const workingDirectory = join(projectRoot, "src");
    const globalPath = join(root, "global.yaml");
    const projectPath = join(projectRoot, ".provenant", "agent-fabric.yaml");
    await mkdir(workingDirectory, { recursive: true });
    await writeGlobalConfig(globalPath, projectRoot);
    await writeYaml(projectPath, [
      "schemaVersion: 1",
      "namedExecutionProfile: paired-visible",
      "limits:",
      "  maximumConcurrentProviderTurns: 2",
      "",
    ].join("\n"));
    await expect(loadFabricConfig({ globalPath, workingDirectory })).resolves.toMatchObject({
      executionProfile: "paired-visible",
      limits: { maximumConcurrentProviderTurns: 2 },
    });

    await writeYaml(projectPath, "schemaVersion: 1\nworkspaceRoots: [\"/\"]\n");
    await expect(loadFabricConfig({ globalPath, workingDirectory })).rejects.toMatchObject({
      code: "CONFIG_WIDENING_FORBIDDEN",
    });
  });

  it("rejects a trusted field in a discovered layer with the field name", async () => {
    const root = await mkdtemp(join(tmpdir(), "fabric-project-trusted-field-"));
    const projectRoot = join(root, "project");
    const workingDirectory = join(projectRoot, "src");
    const globalPath = join(root, "global.yaml");
    await mkdir(workingDirectory, { recursive: true });
    await writeGlobalConfig(globalPath, projectRoot);
    await writeYaml(join(projectRoot, ".provenant", "agent-fabric.yaml"), [
      "schemaVersion: 1",
      "listener: tcp://0.0.0.0:9999",
      "",
    ].join("\n"));

    await expect(loadFabricConfig({ globalPath, workingDirectory })).rejects.toMatchObject({
      code: "CONFIG_UNTRUSTED_FIELD",
      field: "listener",
    });
  });

  it("uses the nearest project root when project roots are nested", async () => {
    const root = await mkdtemp(join(tmpdir(), "fabric-project-nested-"));
    const outerRoot = join(root, "outer");
    const innerRoot = join(outerRoot, "inner");
    const workingDirectory = join(innerRoot, "src");
    const globalPath = join(root, "global.yaml");
    await mkdir(workingDirectory, { recursive: true });
    await writeGlobalConfig(globalPath, outerRoot);
    await writeYaml(join(outerRoot, ".provenant", "agent-fabric.yaml"), "schemaVersion: 1\nnamedExecutionProfile: headless\nlimits:\n  maximumConcurrentProviderTurns: 7\n");
    await writeYaml(join(innerRoot, ".provenant", "agent-fabric.yaml"), "schemaVersion: 1\nnamedExecutionProfile: paired-visible\nlimits:\n  maximumConcurrentProviderTurns: 3\n");

    await expect(loadFabricConfig({ globalPath, workingDirectory })).resolves.toMatchObject({
      executionProfile: "paired-visible",
      limits: { maximumConcurrentProviderTurns: 3 },
    });
  });

  it("discovers a project root that is not a git repository", async () => {
    const root = await mkdtemp(join(tmpdir(), "fabric-project-non-git-"));
    const projectRoot = join(root, "project");
    const workingDirectory = join(projectRoot, "src");
    const globalPath = join(root, "global.yaml");
    await mkdir(workingDirectory, { recursive: true });
    await writeGlobalConfig(globalPath, projectRoot);
    await writeYaml(join(projectRoot, ".provenant", "agent-fabric.yaml"), "schemaVersion: 1\nnamedExecutionProfile: paired-visible\n");

    expect(existsSync(join(projectRoot, ".git"))).toBe(false);
    await expect(loadFabricConfig({ globalPath, workingDirectory })).resolves.toMatchObject({
      executionProfile: "paired-visible",
    });
  });

  it("canonicalises a symlinked project root before discovering its layer", async () => {
    const root = await mkdtemp(join(tmpdir(), "fabric-project-symlink-"));
    const actualRoot = join(root, "actual");
    const linkedRoot = join(root, "linked");
    const workingDirectory = join(linkedRoot, "src");
    const globalPath = join(root, "global.yaml");
    await mkdir(join(actualRoot, "src"), { recursive: true });
    await symlink(actualRoot, linkedRoot, "dir");
    await writeGlobalConfig(globalPath, actualRoot);
    const actualConfigPath = join(actualRoot, ".provenant", "agent-fabric.yaml");
    await writeYaml(actualConfigPath, "schemaVersion: 1\nnamedExecutionProfile: paired-visible\n");

    expect(discoverProjectConfigPath(workingDirectory)).toBe(await realpath(actualConfigPath));
    await expect(loadFabricConfig({ globalPath, workingDirectory })).resolves.toMatchObject({
      executionProfile: "paired-visible",
    });
  });
});
