import { createHash } from "node:crypto";
import { chmod, mkdir, mkdtemp, realpath, rm, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";

import Database from "better-sqlite3";
import { describe, expect, it } from "vitest";

import { forceStartFabricDaemonForTests } from "../../../src/daemon/client.ts";
import {
  composeHerdrDaemonIntegration,
  parseHerdrDaemonProcessConfiguration,
} from "../../../src/daemon/herdr-composition.ts";

describe("production daemon Herdr composition", () => {
  it("loads the optional production package through capability-checked daemon configuration", async () => {
    const directory = await realpath(await mkdtemp(join(tmpdir(), "fabric-herdr-daemon-production-")));
    try {
      const stateDirectory = join(directory, "state");
      const projectRoot = join(directory, "project");
      const executable = join(directory, "herdr-fixture");
      const consoleExecutable = join(directory, "console-fixture");
      await mkdir(stateDirectory, { mode: 0o700 });
      await mkdir(projectRoot, { mode: 0o700 });
      const body = "#!/bin/sh\n" +
        "if [ \"$1 $2\" = \"api snapshot\" ]; then printf '%s' '{\"id\":\"fixture\",\"result\":{\"type\":\"session_snapshot\",\"snapshot\":{\"version\":\"auto-updated-fixture\",\"protocol\":16,\"agents\":[],\"panes\":[]}}}'; exit 0; fi\n" +
        "exit 9\n";
      const consoleBody = "#!/bin/sh\nexit 0\n";
      await writeFile(executable, body, { encoding: "utf8", mode: 0o700 });
      await writeFile(consoleExecutable, consoleBody, { encoding: "utf8", mode: 0o700 });
      await chmod(executable, 0o700);
      await chmod(consoleExecutable, 0o700);
      const composition = composeHerdrDaemonIntegration({
        enabled: true,
        executable,
        expectedProtocol: 16,
        consoleExecutable,
        consoleExecutableDigest: digest(consoleBody),
      }, stateDirectory);
      if (composition.mode !== "enabled") throw new Error("expected enabled Herdr composition");
      const runtime = await composition.createIntegration({
        projectId: "project-01" as never,
        projectSessionId: "session-01" as never,
        canonicalProjectRoot: projectRoot,
        fabricJournal: {} as never,
        fabricDirectSteer: {} as never,
      });
      expect(runtime).toMatchObject({
        execute: expect.any(Function),
        lookupAction: expect.any(Function),
        reconcilePresence: expect.any(Function),
        restoreControlBinding: expect.any(Function),
      });
    } finally {
      await rm(directory, { recursive: true, force: true });
    }
  });

  it("accepts a complete observer integrity configuration without treating its digest as a path", () => {
    expect(parseHerdrDaemonProcessConfiguration(JSON.stringify({
      enabled: true,
      executable: "/opt/herdr/bin/herdr",
      expectedProtocol: 16,
      consoleExecutable: "/opt/fabric/bin/agent-fabric-console",
      consoleExecutableDigest: `sha256:${"2".repeat(64)}`,
      observerExecutable: "/opt/fabric/bin/agent-fabric",
      observerExecutableDigest: `sha256:${"3".repeat(64)}`,
      observerSocketPath: "/private/fabric/fabric.sock",
      observerCapabilityFile: "/private/fabric/observer.cap",
      observerCursorDirectory: "/private/fabric/cursors",
    }))).toMatchObject({
      enabled: true,
      observerExecutableDigest: `sha256:${"3".repeat(64)}`,
    });
  });

  it("passes an explicit portable-disabled Herdr mode into the elected daemon", async () => {
    const directory = await mkdtemp(join(tmpdir(), "fabric-herdr-daemon-disabled-"));
    const stateDirectory = join(directory, "state");
    const runtimeDirectory = join(directory, "runtime");
    const databasePath = join(stateDirectory, "fabric.sqlite3");
    const socketPath = join(runtimeDirectory, "fabric.sock");
    const daemon = await forceStartFabricDaemonForTests({
      databasePath,
      stateDirectory,
      runtimeDirectory,
      socketPath,
      workspaceRoots: [directory],
      herdr: { enabled: false },
    } as never);
    try {
      const database = new Database(databasePath, { readonly: true });
      try {
        const value = database.prepare(`
          SELECT state, discovered_contract_json FROM integration_availability
           WHERE integration_id='herdr-control-v1'
        `).get() as { state: string; discovered_contract_json: string } | undefined;
        expect(value).toBeDefined();
        expect(value?.state).toBe("unavailable");
        expect(JSON.parse(value?.discovered_contract_json ?? "{}")).toMatchObject({
          schemaVersion: 1,
          operationFamily: "herdr-control-v1",
          mode: "disabled",
          presence: [],
        });
      } finally {
        database.close();
      }
    } finally {
      await daemon.stop();
      await rm(directory, { recursive: true, force: true });
    }
  });
});

function digest(value: string): string {
  return `sha256:${createHash("sha256").update(value).digest("hex")}`;
}
