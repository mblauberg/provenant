import { mkdir, mkdtemp, readFile, realpath, rm, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { describe, expect, it, vi } from "vitest";
import { parse, stringify } from "yaml";

import { composeDaemonAdapters, composeDaemonConfiguration } from "../../src/daemon/composition.ts";
import { runWorkspaceTrust } from "../../src/cli/workspace-trust.ts";
import { FabricError } from "../../src/errors.ts";
import { commitFixtureRepository, writeWrapperPackageScaffold } from "../support/fixture-repository.ts";
import {
  createPortableActivatedPrimaryFixture,
  createPrimaryCompatibilityFixture,
} from "../support/primary-adapter-testkit.ts";

describe("daemon trusted adapter composition", () => {
  it("composes only the explicitly activated and runtime-conformant adapters", async () => {
    const fixture = await createPortableActivatedPrimaryFixture();
    const verifyProvider = vi.fn(async () => ({}) as never);
    try {
      const adapters = await composeDaemonAdapters({
        globalConfigPath: fixture.configPath,
        compatibilityPath: fixture.compatibilityPath,
        compatibilitySchemaPath: fixture.schemaPath,
        agentsHome: fixture.directory,
        verifyProvider,
      });
      expect(Object.keys(adapters).sort()).toEqual(
        ["claude-agent-sdk", "codex-app-server"],
      );
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
      });
    } finally {
      await rm(fixture.directory, { recursive: true, force: true });
    }
  });

  it("refuses a project layer that selects an execution profile", async () => {
    const compatibilityFixture = await createPortableActivatedPrimaryFixture();
    const directory = await mkdtemp(join(tmpdir(), "fabric-machine-composition-"));
    const portableRoot = join(directory, "portable");
    const machineRoot = join(directory, "machine");
    const projectRoot = join(machineRoot, "project");
    const stateDirectory = join(directory, "state");
    const runtimeDirectory = join(stateDirectory, "runtime");
    await Promise.all([
      mkdir(portableRoot, { recursive: true }), mkdir(join(projectRoot, ".provenant"), { recursive: true }),
      mkdir(runtimeDirectory, { recursive: true, mode: 0o700 }),
    ]);
    const globalConfigPath = join(directory, "global.yaml");
    const projectConfigPath = join(projectRoot, ".provenant", "agent-fabric.yaml");
    await writeFile(globalConfigPath, stringify({
      schemaVersion: 1, allowedAdapters: [], activeAdapters: [], adapters: {},
      allowedProfiles: ["paired-visible"], workspaceRoots: [portableRoot],
      limits: { maximumConcurrentProviderTurns: 8 },
    }));
    await writeFile(projectConfigPath, stringify({
      schemaVersion: 1, namedExecutionProfile: "paired-visible", workspaceRoots: [projectRoot],
    }), { mode: 0o600 });
    const paths = {
      stateDirectory, runtimeDirectory,
      databasePath: join(stateDirectory, "fabric.sqlite3"), socketPath: join(runtimeDirectory, "fabric.sock"),
    };
    try {
      await runWorkspaceTrust(["trust", projectRoot, "--profiles", "headless"], paths);
      await expect(composeDaemonConfiguration({
        globalConfigPath, projectRoot,
        compatibilityPath: compatibilityFixture.compatibilityPath,
        compatibilitySchemaPath: compatibilityFixture.schemaPath,
        agentsHome: directory, stateDirectory,
      })).rejects.toMatchObject({ code: "CONFIG_UNTRUSTED_FIELD", field: "namedExecutionProfile" });
    } finally {
      await Promise.all([
        rm(directory, { recursive: true, force: true }),
        rm(compatibilityFixture.directory, { recursive: true, force: true }),
      ]);
    }
  });
});
