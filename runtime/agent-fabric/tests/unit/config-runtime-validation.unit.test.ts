import { mkdir, mkdtemp, realpath, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { dirname, join } from "node:path";

import { describe, expect, it } from "vitest";

import { loadFabricConfig } from "../../src/config/index.ts";

async function writeJson(path: string, value: unknown): Promise<void> {
  await mkdir(dirname(path), { recursive: true });
  await writeFile(path, `${JSON.stringify(value, null, 2)}\n`);
}

function validGlobal(root: string): Record<string, unknown> {
  return {
    schemaVersion: 1,
    adapters: { codex: { command: ["codex", "app-server"] } },
    allowedAdapters: ["codex"],
    activeAdapters: ["codex"],
    allowedProfiles: ["paired-visible"],
    workspaceRoots: [root],
    limits: { maximumConcurrentProviderTurns: 8 },
  };
}

describe("runtime configuration schema validation", () => {
  it.each([
    ["a zero global limit", { limits: { maximumConcurrentProviderTurns: 0 } }, "/limits/maximumConcurrentProviderTurns"],
    ["a limit above the hard cap", { limits: { maximumConcurrentProviderTurns: 9 } }, "/limits/maximumConcurrentProviderTurns"],
    ["a fractional limit", { limits: { maximumConcurrentProviderTurns: 1.5 } }, "/limits/maximumConcurrentProviderTurns"],
    ["an unknown limits field", { limits: { maximumConcurrentProviderTurns: 8, unlimited: true } }, "/limits/unlimited"],
    ["an unknown adapter field", { adapters: { codex: { command: ["codex"], shell: true } } }, "/adapters/codex/shell"],
  ])("rejects %s using the published schema", async (_label, override, field) => {
    const root = await mkdtemp(join(tmpdir(), "fabric-runtime-config-"));
    const globalPath = join(root, "global.yaml");
    await writeJson(globalPath, { ...validGlobal(root), ...override });

    await expect(loadFabricConfig({ globalPath })).rejects.toMatchObject({
      code: "CONFIG_UNTRUSTED_FIELD",
      field,
    });
  });

  it("validates project configuration with the project definition from the same schema", async () => {
    const root = await mkdtemp(join(tmpdir(), "fabric-runtime-project-config-"));
    const projectRoot = join(root, "project");
    const globalPath = join(root, "global.yaml");
    const projectPath = join(projectRoot, ".agents", "agent-fabric.yaml");
    await writeJson(globalPath, validGlobal(projectRoot));
    await writeJson(projectPath, {
      schemaVersion: 1,
      namedExecutionProfile: "paired-visible",
      limits: { maximumConcurrentProviderTurns: 2, surprise: true },
    });

    await expect(loadFabricConfig({ globalPath, projectPath })).rejects.toMatchObject({
      code: "CONFIG_UNTRUSTED_FIELD",
      field: "/limits/surprise",
    });
  });

  it("rejects a zero project limit consistently with the runtime minimum", async () => {
    const root = await mkdtemp(join(tmpdir(), "fabric-runtime-project-zero-limit-"));
    const projectRoot = join(root, "project");
    const globalPath = join(root, "global.yaml");
    const projectPath = join(projectRoot, ".agents", "agent-fabric.yaml");
    await writeJson(globalPath, validGlobal(projectRoot));
    await writeJson(projectPath, {
      schemaVersion: 1,
      limits: { maximumConcurrentProviderTurns: 0 },
    });

    await expect(loadFabricConfig({ globalPath, projectPath })).rejects.toMatchObject({
      code: "CONFIG_UNTRUSTED_FIELD",
      field: "/limits/maximumConcurrentProviderTurns",
    });
  });

  // This case is not independently discriminating: because the local entry
  // restates the product command exactly, the pre-ADR-0019 overlay would have
  // satisfied it too. It pins the surrounding intersection behaviour. The
  // discriminating coverage for adapter-command ownership lives in
  // split-root-config-layer.unit.test.ts ("cannot admit an adapter command the
  // product does not ship" and "refuses to let the instance swap the command
  // behind an active product adapter").
  it("keeps product adapter commands while intersecting authority sets and limits", async () => {
    const root = await mkdtemp(join(tmpdir(), "fabric-runtime-local-config-"));
    const globalRoot = join(root, "global-root");
    const localRoot = join(globalRoot, "local-root");
    const globalPath = join(root, "global.yaml");
    const localPath = join(root, "local.yaml");
    await writeJson(globalPath, {
      ...validGlobal(globalRoot),
      adapters: {
        codex: { command: ["codex", "global"] },
        gemini: { command: ["gemini"] },
      },
      allowedAdapters: ["codex", "gemini"],
      activeAdapters: ["codex"],
      allowedProfiles: ["headless"],
    });
    await writeJson(localPath, {
      schemaVersion: 1,
      // Restating the product's exact entry is permitted, so an instance file
      // may be authored as a narrowed copy of the product's (ADR 0019).
      adapters: {
        codex: { command: ["codex", "global"] },
      },
      allowedAdapters: ["codex", "claude"],
      activeAdapters: ["codex", "claude"],
      allowedProfiles: ["headless", "paired-visible"],
      workspaceRoots: [localRoot],
      limits: { maximumConcurrentProviderTurns: 6 },
    });

    await expect(loadFabricConfig({ globalPath, localPath })).resolves.toMatchObject({
      adapterIds: ["codex"],
      adapterCommands: {
        codex: ["codex", "global"],
      },
      workspaceRoots: [join(await realpath(root), "global-root", "local-root")],
      limits: { maximumConcurrentProviderTurns: 6 },
    });
  });

  it("keeps the trusted allow-list separate from an empty active adapter set", async () => {
    const root = await mkdtemp(join(tmpdir(), "fabric-runtime-core-only-"));
    const globalPath = join(root, "global.yaml");
    await writeJson(globalPath, {
      ...validGlobal(root),
      allowedAdapters: ["codex", "claude"],
      activeAdapters: [],
      adapters: {
        codex: { command: ["codex", "wrapper"] },
        claude: { command: ["claude", "wrapper"] },
      },
    });

    await expect(loadFabricConfig({ globalPath })).resolves.toMatchObject({
      adapterIds: [],
      adapterCommands: {},
    });
  });

  it("supports a trusted multi-adapter active subset", async () => {
    const root = await mkdtemp(join(tmpdir(), "fabric-runtime-active-pair-"));
    const globalPath = join(root, "global.yaml");
    await writeJson(globalPath, {
      ...validGlobal(root),
      allowedAdapters: ["codex", "claude", "agy"],
      activeAdapters: ["codex", "claude"],
      adapters: {
        codex: { command: ["node", "codex-wrapper.js"] },
        claude: { command: ["node", "claude-wrapper.js"] },
        agy: { command: ["node", "agy-wrapper.js"] },
      },
    });

    await expect(loadFabricConfig({ globalPath })).resolves.toMatchObject({
      adapterIds: ["codex", "claude"],
      adapterCommands: {
        codex: ["node", "codex-wrapper.js"],
        claude: ["node", "claude-wrapper.js"],
      },
    });
  });

  it("rejects a trusted active adapter outside the allow-list", async () => {
    const root = await mkdtemp(join(tmpdir(), "fabric-runtime-active-widening-"));
    const globalPath = join(root, "global.yaml");
    await writeJson(globalPath, {
      ...validGlobal(root),
      activeAdapters: ["claude"],
      adapters: { claude: { command: ["node", "claude-wrapper.js"] } },
    });

    await expect(loadFabricConfig({ globalPath })).rejects.toMatchObject({
      code: "CONFIG_WIDENING_FORBIDDEN",
    });
  });

  it("admits machine roots before project and run layers narrow the combined maximum", async () => {
    const root = await mkdtemp(join(tmpdir(), "fabric-runtime-machine-root-"));
    const portableRoot = join(root, "portable");
    const machineRoot = join(root, "machine");
    const narrowed = join(machineRoot, "project");
    await Promise.all([mkdir(portableRoot, { recursive: true }), mkdir(narrowed, { recursive: true })]);
    const globalPath = join(root, "global.yaml");
    const projectPath = join(root, "project.yaml");
    await writeJson(globalPath, validGlobal(portableRoot));
    await writeJson(projectPath, { schemaVersion: 1, workspaceRoots: [narrowed] });

    await expect(loadFabricConfig({
      globalPath,
      projectPath,
      additionalWorkspaceRoots: [machineRoot],
    })).resolves.toMatchObject({ workspaceRoots: [await realpath(narrowed)] });
  });
});
