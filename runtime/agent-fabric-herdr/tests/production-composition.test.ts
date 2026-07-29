import { createHash } from "node:crypto";
import { chmod, mkdir, mkdtemp, realpath, rm, symlink, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";

import { describe, expect, it } from "vitest";

import type {
  FabricActionJournalPort,
  FabricDirectSteerPort,
} from "../src/contracts.js";
import { createProductionHerdrIntegration } from "../src/production.js";

describe("production Herdr composition", () => {
  it("accepts changed provider bytes and version when the runtime protocol remains compatible", async () => {
    const root = await realpath(await mkdtemp(join(tmpdir(), "fabric-herdr-production-")));
    try {
      const stateDirectory = join(root, "state");
      const projectRoot = join(root, "project");
      await mkdir(stateDirectory, { mode: 0o700 });
      await mkdir(projectRoot, { mode: 0o700 });
      const executable = join(root, "herdr");
      const providerTarget = join(root, "herdr-auto-updated-fixture");
      const consoleExecutable = join(root, "console-fixture");
      const body = "#!/bin/sh\n" +
        "if [ \"$1 $2\" = \"api snapshot\" ]; then printf '%s' '{\"id\":\"fixture\",\"result\":{\"type\":\"session_snapshot\",\"snapshot\":{\"version\":\"auto-updated-fixture\",\"protocol\":16,\"agents\":[],\"panes\":[]}}}'; exit 0; fi\n" +
        "exit 9\n";
      const consoleBody = "#!/bin/sh\nexit 0\n";
      await writeFile(providerTarget, body, { encoding: "utf8", mode: 0o700 });
      await symlink(providerTarget, executable);
      await writeFile(consoleExecutable, consoleBody, { encoding: "utf8", mode: 0o700 });
      await chmod(providerTarget, 0o700);
      await chmod(consoleExecutable, 0o700);

      await chmod(providerTarget, 0o777);
      await expect(createProductionHerdrIntegration({
        executable,
        expectedProtocol: 16,
        stateDirectory,
        projectId: "project-01",
        projectSessionId: "session-01",
        canonicalProjectRoot: projectRoot,
        consoleExecutable,
        consoleExecutableDigest: digest(consoleBody),
        fabricJournal: unusedFabricJournal(),
        fabricDirectSteer: unusedDirectSteer(),
      })).rejects.toThrow("owner-controlled executable");
      await chmod(providerTarget, 0o700);

      const integration = await createProductionHerdrIntegration({
        executable,
        expectedProtocol: 16,
        stateDirectory,
        projectId: "project-01",
        projectSessionId: "session-01",
        canonicalProjectRoot: projectRoot,
        consoleExecutable,
        consoleExecutableDigest: digest(consoleBody),
        fabricJournal: unusedFabricJournal(),
        fabricDirectSteer: unusedDirectSteer(),
      });

      await expect(integration.boundary.probe()).resolves.toEqual({
        version: "auto-updated-fixture",
        protocol: 16,
      });
      expect(integration).toMatchObject({
        boundary: expect.any(Object),
        adapter: expect.any(Object),
        directSteer: expect.any(Object),
      });
      await expect(integration.boundary.observeAgent("unknown-agent" as never)).resolves.toMatchObject({
        state: "unavailable",
        reason: "agent has no Fabric-bound Herdr presence registration",
      });

      await expect(createProductionHerdrIntegration({
        executable,
        expectedProtocol: 17,
        stateDirectory,
        projectId: "project-01",
        projectSessionId: "session-01",
        canonicalProjectRoot: projectRoot,
        consoleExecutable,
        consoleExecutableDigest: digest(consoleBody),
        fabricJournal: unusedFabricJournal(),
        fabricDirectSteer: unusedDirectSteer(),
      })).rejects.toThrow("snapshot is malformed or incompatible");

      const observerExecutable = join(root, "observer-fixture");
      const observerCapabilityFile = join(root, "observer.cap");
      const observerCursorDirectory = join(root, "observer-cursors");
      await writeFile(observerExecutable, consoleBody, { encoding: "utf8", mode: 0o700 });
      await writeFile(observerCapabilityFile, "afc_fixture_only", { encoding: "utf8", mode: 0o600 });
      await mkdir(observerCursorDirectory, { mode: 0o700 });
      await expect(createProductionHerdrIntegration({
        executable,
        expectedProtocol: 16,
        stateDirectory,
        projectId: "project-01",
        projectSessionId: "session-01",
        canonicalProjectRoot: projectRoot,
        consoleExecutable,
        consoleExecutableDigest: digest(consoleBody),
        observerExecutable,
        observerExecutableDigest: `sha256:${"0".repeat(64)}`,
        observerSocketPath: join(root, "fabric.sock"),
        observerCapabilityFile,
        observerCursorDirectory,
        fabricJournal: unusedFabricJournal(),
        fabricDirectSteer: unusedDirectSteer(),
      })).rejects.toThrow("observer executable digest changed");
    } finally {
      await rm(root, { recursive: true, force: true });
    }
  });
});

function digest(value: string): string {
  return `sha256:${createHash("sha256").update(value).digest("hex")}`;
}

function unusedFabricJournal(): FabricActionJournalPort {
  return {
    readAction: async () => null,
    markDispatched: async () => { throw new Error("unused"); },
    completeAction: async () => { throw new Error("unused"); },
    markAmbiguous: async () => { throw new Error("unused"); },
  };
}

function unusedDirectSteer(): FabricDirectSteerPort {
  return {
    validateSteerReference: async () => ({ status: "rejected", code: "unknown-reference", reason: "unused" }),
    prepareDirectSteerAction: async () => { throw new Error("unused"); },
  };
}
