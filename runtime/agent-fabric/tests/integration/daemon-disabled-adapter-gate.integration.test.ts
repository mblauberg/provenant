import { mkdir, mkdtemp, readFile, realpath, rm, symlink, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { dirname, join } from "node:path";
import { describe, expect, it, vi } from "vitest";
import { parse, stringify } from "yaml";

import { composeDaemonAdapters, composeDaemonConfiguration, parseDaemonAdapters } from "../../src/daemon/composition.ts";
import { runWorkspaceTrust } from "../../src/cli/workspace-trust.ts";
import { FabricError } from "../../src/errors.ts";
import { commitFixtureRepository, writeWrapperPackageScaffold } from "../support/fixture-repository.ts";
import {
  createPortableActivatedPrimaryFixture,
  createPrimaryCompatibilityFixture,
  repositoryPath,
} from "../support/primary-adapter-testkit.ts";

describe("daemon trusted adapter composition", () => {
  it("retains partial and owner-controlled assurance as advisory adapter metadata", () => {
    const adapters = parseDaemonAdapters(JSON.stringify({
      "cursor-agent": { command: ["cursor"], environment: {}, providerAssurance: "partial-signed-helpers" },
      "opencode-acp": { command: ["opencode"], environment: {}, providerAssurance: "owner-controlled-install-root" },
    }));
    expect(adapters).toMatchObject({
      "cursor-agent": { providerAssurance: "partial-signed-helpers" },
      "opencode-acp": { providerAssurance: "owner-controlled-install-root" },
    });
  });

  it("skips an unavailable active optional adapter and reports its degradation", async () => {
    const fixture = await createPortableActivatedPrimaryFixture();
    try {
      const compatibility = parse(await readFile(fixture.compatibilityPath, "utf8")) as Record<string, any>;
      compatibility.adapters.agy = {
        enabled: true,
        delivery_stage: 4,
        implementation: {
          kind: "fixture-process",
          executable: "missing-agy",
          provider_identity: "apple-designated",
          wrapper_entrypoint: fixture.artifactPaths[1],
        },
        contract: { protocol: "agy-fixture" },
        runtime_range: { platforms: [process.platform] },
        model_family_constraints: { allowed: ["google"], requires_explicit_model: true },
        official_source_url: "https://example.invalid/agy-fixture",
      };
      await writeFile(fixture.compatibilityPath, stringify(compatibility));
      const config = parse(await readFile(fixture.configPath, "utf8")) as Record<string, any>;
      config.allowedAdapters.push("agy");
      config.activeAdapters.push("agy");
      config.adapters.agy = { command: [process.execPath, fixture.artifactPaths[1]] };
      await writeFile(fixture.configPath, stringify(config));

      const composed = await composeDaemonConfiguration({
        globalConfigPath: fixture.configPath,
        compatibilityPath: fixture.compatibilityPath,
        compatibilitySchemaPath: fixture.schemaPath,
        agentsHome: fixture.directory,
        verifyNpmInstall: async () => undefined,
        verifyProvider: async () => ({}) as never,
      });

      expect(Object.keys(composed.adapters).sort()).toEqual(["claude-agent-sdk", "codex-app-server"]);
      expect(composed.unavailableOptionalAdapters).toEqual([
        expect.objectContaining({ adapterId: "agy", executable: "missing-agy" }),
      ]);
    } finally {
      await rm(fixture.directory, { recursive: true, force: true });
    }
  });

  it("refuses adapter composition when npm install attestation is missing", async () => {
    const fixture = await createPortableActivatedPrimaryFixture();
    try {
      await expect(composeDaemonAdapters({
        globalConfigPath: fixture.configPath,
        compatibilityPath: fixture.compatibilityPath,
        compatibilitySchemaPath: fixture.schemaPath,
        agentsHome: fixture.directory,
        verifyProvider: async () => ({} as never),
      })).rejects.toMatchObject({ code: "NPM_INSTALL_ATTESTATION_MISMATCH" });
    } finally {
      await rm(fixture.directory, { recursive: true, force: true });
    }
  });

  it("composes only the explicitly activated and runtime-conformant adapters", async () => {
    const fixture = await createPortableActivatedPrimaryFixture();
    const verifyProvider = vi.fn(async () => ({
      identity: { assurance: "full-vendor-identity" },
    }) as never);
    try {
      const adapters = await composeDaemonAdapters({
        globalConfigPath: fixture.configPath,
        compatibilityPath: fixture.compatibilityPath,
        compatibilitySchemaPath: fixture.schemaPath,
        agentsHome: fixture.directory,
        verifyNpmInstall: async () => undefined,
        verifyProvider,
      });
      expect(Object.keys(adapters).sort()).toEqual(
        ["claude-agent-sdk", "codex-app-server"],
      );
      expect(adapters["claude-agent-sdk"]).toMatchObject({ providerAssurance: "full-vendor-identity" });
      expect(adapters["codex-app-server"]).toMatchObject({ providerAssurance: "full-vendor-identity" });
      expect(verifyProvider).toHaveBeenCalledTimes(2);
    } finally {
      await rm(fixture.directory, { recursive: true, force: true });
    }
  });

  it("rejects an adapter that fails its runtime interface capability check", async () => {
    const fixture = await createPortableActivatedPrimaryFixture();
    try {
      await expect(composeDaemonConfiguration({
        globalConfigPath: fixture.configPath,
        compatibilityPath: fixture.compatibilityPath,
        compatibilitySchemaPath: fixture.schemaPath,
        agentsHome: fixture.directory,
        verifyNpmInstall: async () => undefined,
        verifyProvider: async () => {
          throw new FabricError("ADAPTER_INTERFACE_MISMATCH", "fixture handshake failed");
        },
      })).rejects.toMatchObject({
        code: "ADAPTER_INTERFACE_MISMATCH",
        message: "fixture handshake failed",
      });
    } finally {
      await rm(fixture.directory, { recursive: true, force: true });
    }
  });

  it("expands the trusted AGENTS_HOME workspace root without binding config to one user", async () => {
    const fixture = await createPortableActivatedPrimaryFixture();
    const directory = fixture.directory;
    const agentsHome = join(directory, "agents-home");
    const literalRoot = join(directory, "literal-root");
    await Promise.all([mkdir(agentsHome), mkdir(literalRoot)]);
    const config = parse(await readFile(fixture.configPath, "utf8")) as Record<string, unknown>;
    config.workspaceRoots = ["${AGENTS_HOME}", literalRoot];
    await writeFile(fixture.configPath, stringify(config));
    try {
      const expectedRoots = [await realpath(agentsHome), await realpath(literalRoot)];
      await expect(composeDaemonConfiguration({
        globalConfigPath: fixture.configPath,
        compatibilityPath: fixture.compatibilityPath,
        compatibilitySchemaPath: fixture.schemaPath,
        agentsHome,
        stateDirectory: join(directory, "state"),
        verifyNpmInstall: async () => undefined,
        verifyProvider: async () => ({}) as never,
      })).resolves.toMatchObject({ workspaceRoots: expectedRoots });
    } finally {
      await rm(directory, { recursive: true, force: true });
    }
  });

  it("composes Codex with its configured provider executable and trusted model policy", async () => {
    const fixture = await createPrimaryCompatibilityFixture();
    const configPath = join(fixture.directory, "agent-fabric.yaml");
    const compatibility = parse(await readFile(fixture.compatibilityPath, "utf8")) as {
      adapters: Record<string, {
        enabled: boolean;
        implementation: Record<string, string>;
        model_family_constraints: Record<string, unknown>;
      }>;
    };
    const codex = compatibility.adapters["codex-app-server"];
    if (codex === undefined) throw new TypeError("Codex compatibility fixture is missing");
    const executable = codex.implementation.executable;
    if (executable === undefined) throw new TypeError("Codex fixture executable is missing");
    codex.enabled = true;
    codex.implementation.wrapper_entrypoint = executable;
    await writeWrapperPackageScaffold(fixture.directory);
    const fixtureCommit = await commitFixtureRepository(fixture.directory);
    codex.model_family_constraints = {
      allowed: ["openai"],
      requires_explicit_model: true,
    };
    await writeFile(fixture.compatibilityPath, stringify(compatibility));
    await writeFile(configPath, stringify({
      schemaVersion: 1,
      allowedAdapters: ["codex-app-server"],
      activeAdapters: ["codex-app-server"],
      allowedProfiles: ["headless"],
      adapters: { "codex-app-server": { command: [process.execPath, "/unverified/codex-wrapper.js", "--provider-executable", "/unverified/first", "--provider-executable", "/unverified/second"] } },
      workspaceRoots: [fixture.directory],
    }));
    try {
      const composed = await composeDaemonAdapters({
        globalConfigPath: configPath,
        compatibilityPath: fixture.compatibilityPath,
        compatibilitySchemaPath: fixture.schemaPath,
        agentsHome: fixture.directory,
        verifyNpmInstall: async () => undefined,
        verifyProvider: async () => ({}) as never,
      });
      expect(composed["codex-app-server"]).toMatchObject({
        command: [
          process.execPath,
          fixture.artifactPaths[0],
          "--provider-executable",
          fixture.artifactPaths[0],
          "--provider-identity-policy",
          "apple-designated",
        ],
        modelPolicy: { allowedFamilies: ["openai"], requiresExplicitModel: true },
        wrapperProvenance: {
          repositoryCommit: fixtureCommit,
          wrapperPath: "fixture-adapter",
        },
        npmInstallProductRoot: fixture.directory,
      });
    } finally {
      await rm(fixture.directory, { recursive: true, force: true });
    }
  });

  it("admits a tracked TypeScript wrapper through the canonical tsx loader identity", async () => {
    const fixture = await createPrimaryCompatibilityFixture();
    const wrapperPath = join(fixture.directory, "fixture-wrapper.ts");
    const loaderPath = join(fixture.directory, "node_modules", "tsx", "dist", "loader.mjs");
    await writeFile(wrapperPath, "export const portableFixtureWrapper = true;\n", { mode: 0o600 });
    await writeWrapperPackageScaffold(fixture.directory);
    const fixtureCommit = await commitFixtureRepository(fixture.directory);
    await symlink(repositoryPath("node_modules"), join(fixture.directory, "node_modules"), "dir");

    const compatibility = parse(await readFile(fixture.compatibilityPath, "utf8")) as {
      adapters: Record<string, { enabled: boolean; implementation: Record<string, string> }>;
    };
    for (const adapterId of ["claude-agent-sdk", "codex-app-server"]) {
      const adapter = compatibility.adapters[adapterId];
      if (adapter === undefined) throw new TypeError(`missing compatibility fixture: ${adapterId}`);
      adapter.enabled = true;
      adapter.implementation.wrapper_entrypoint = wrapperPath;
    }
    await writeFile(fixture.compatibilityPath, stringify(compatibility));
    const configPath = join(fixture.directory, "agent-fabric.yaml");
    await writeFile(configPath, stringify({
      schemaVersion: 1,
      allowedAdapters: ["claude-agent-sdk", "codex-app-server"],
      activeAdapters: ["claude-agent-sdk", "codex-app-server"],
      allowedProfiles: ["headless"],
      adapters: {
        "claude-agent-sdk": { command: [process.execPath, "--import", loaderPath, "--conditions=source", wrapperPath] },
        "codex-app-server": { command: [process.execPath, "--import", loaderPath, "--conditions=source", wrapperPath] },
      },
      workspaceRoots: [fixture.directory],
    }));

    try {
      const composed = await composeDaemonConfiguration({
        globalConfigPath: configPath,
        compatibilityPath: fixture.compatibilityPath,
        compatibilitySchemaPath: fixture.schemaPath,
        agentsHome: fixture.directory,
        verifyNpmInstall: async () => undefined,
        verifyProvider: async () => ({}) as never,
      });
      expect(composed.unavailableOptionalAdapters).toEqual([]);
      expect(composed.adapters["claude-agent-sdk"]).toMatchObject({
        command: [
          process.execPath, "--import", loaderPath, "--conditions=source", wrapperPath,
          "--provider-executable", join(fixture.directory, "fixture-adapter"),
          "--provider-identity-policy", "apple-designated",
        ],
        wrapperProvenance: { repositoryCommit: fixtureCommit, wrapperPath: "fixture-wrapper.ts" },
      });
    } finally {
      await rm(fixture.directory, { recursive: true, force: true });
    }
  });

  it.each(["wrong-package", "symlink-target"] as const)(
    "rejects a %s TypeScript loader during runtime composition admission",
    async (kind) => {
      const fixture = await createPrimaryCompatibilityFixture();
      const wrapperPath = join(fixture.directory, "fixture-wrapper.ts");
      const nodeModules = join(fixture.directory, "node_modules");
      const loaderPath = join(nodeModules, "tsx", "dist", "loader.mjs");
      let outsidePath: string | undefined;
      await writeFile(wrapperPath, "export const portableFixtureWrapper = true;\n", { mode: 0o600 });
      await writeWrapperPackageScaffold(fixture.directory);
      await commitFixtureRepository(fixture.directory);
      await mkdir(nodeModules, { recursive: true });
      if (kind === "wrong-package") {
        const wrongPackage = join(nodeModules, "wrong-package");
        await mkdir(join(wrongPackage, "dist"), { recursive: true });
        await writeFile(join(wrongPackage, "package.json"), JSON.stringify({ name: "wrong-package" }));
        await writeFile(join(wrongPackage, "dist", "loader.mjs"), "export {};\n");
      } else {
        outsidePath = join(dirname(fixture.directory), "tsx-runtime-outside");
        await mkdir(join(outsidePath, "dist"), { recursive: true });
        await writeFile(join(outsidePath, "package.json"), JSON.stringify({ name: "tsx" }));
        await writeFile(join(outsidePath, "dist", "loader.mjs"), "export {};\n");
        await symlink(outsidePath, join(nodeModules, "tsx"), "dir");
      }

      const compatibility = parse(await readFile(fixture.compatibilityPath, "utf8")) as {
        adapters: Record<string, { enabled: boolean; implementation: Record<string, string> }>;
      };
      for (const adapterId of ["claude-agent-sdk", "codex-app-server"]) {
        const adapter = compatibility.adapters[adapterId];
        if (adapter === undefined) throw new TypeError(`missing compatibility fixture: ${adapterId}`);
        adapter.enabled = true;
        adapter.implementation.wrapper_entrypoint = wrapperPath;
      }
      await writeFile(fixture.compatibilityPath, stringify(compatibility));
      const configPath = join(fixture.directory, "agent-fabric.yaml");
      const configuredLoader = kind === "wrong-package"
        ? join(nodeModules, "wrong-package", "dist", "loader.mjs")
        : loaderPath;
      await writeFile(configPath, stringify({
        schemaVersion: 1,
        allowedAdapters: ["claude-agent-sdk", "codex-app-server"],
        activeAdapters: ["claude-agent-sdk", "codex-app-server"],
        allowedProfiles: ["headless"],
        adapters: {
          "claude-agent-sdk": { command: [process.execPath, "--import", configuredLoader, "--conditions=source", wrapperPath] },
          "codex-app-server": { command: [process.execPath, "--import", configuredLoader, "--conditions=source", wrapperPath] },
        },
        workspaceRoots: [fixture.directory],
      }));

      try {
        await expect(composeDaemonConfiguration({
          globalConfigPath: configPath,
          compatibilityPath: fixture.compatibilityPath,
          compatibilitySchemaPath: fixture.schemaPath,
          agentsHome: fixture.directory,
          verifyNpmInstall: async () => undefined,
          verifyProvider: async () => ({}) as never,
        })).rejects.toMatchObject({ code: "ADAPTER_COMPATIBILITY_INVALID" });
      } finally {
        await rm(fixture.directory, { recursive: true, force: true });
        if (kind === "symlink-target" && outsidePath !== undefined) {
          await rm(outsidePath, { recursive: true, force: true });
        }
      }
    },
  );

  it("admits a machine-only root before project profile and path narrowing", async () => {
    const compatibilityFixture = await createPortableActivatedPrimaryFixture();
    const directory = await mkdtemp(join(tmpdir(), "fabric-machine-composition-"));
    const portableRoot = join(directory, "portable");
    const machineRoot = join(directory, "machine");
    const projectRoot = join(machineRoot, "project");
    const stateDirectory = join(directory, "state");
    const runtimeDirectory = join(stateDirectory, "runtime");
    await Promise.all([
      mkdir(portableRoot, { recursive: true }), mkdir(projectRoot, { recursive: true }),
      mkdir(runtimeDirectory, { recursive: true, mode: 0o700 }),
    ]);
    const globalConfigPath = join(directory, "global.yaml");
    const projectConfigPath = join(directory, "project.yaml");
    await writeFile(globalConfigPath, stringify({
      schemaVersion: 1, allowedAdapters: [], activeAdapters: [], adapters: {},
      allowedProfiles: ["paired-visible"], workspaceRoots: [portableRoot],
      limits: { maximumConcurrentProviderTurns: 8 },
    }));
    await writeFile(projectConfigPath, stringify({
      schemaVersion: 1, namedExecutionProfile: "paired-visible", workspaceRoots: [projectRoot],
    }));
    const paths = {
      stateDirectory, runtimeDirectory,
      databasePath: join(stateDirectory, "fabric.sqlite3"), socketPath: join(runtimeDirectory, "fabric.sock"),
    };
    try {
      await runWorkspaceTrust(["trust", machineRoot, "--profiles", "paired-visible"], paths);
      await expect(composeDaemonConfiguration({
        globalConfigPath, projectConfigPath,
        compatibilityPath: compatibilityFixture.compatibilityPath,
        compatibilitySchemaPath: compatibilityFixture.schemaPath,
        agentsHome: directory, stateDirectory,
      })).resolves.toMatchObject({ executionProfile: "paired-visible", workspaceRoots: [await realpath(projectRoot)] });
    } finally {
      await Promise.all([
        rm(directory, { recursive: true, force: true }),
        rm(compatibilityFixture.directory, { recursive: true, force: true }),
      ]);
    }
  });
});
