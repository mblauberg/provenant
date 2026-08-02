import { mkdtemp, rm } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";

import { describe, expect, it } from "vitest";

import { connectFabricDaemon, startFabricDaemon } from "../../src/index.ts";

describe("daemon shutdown with attached clients", () => {
  it("closes attached sockets and exits without waiting for clients to disconnect first", async () => {
    const root = await mkdtemp(join(tmpdir(), "fabric-daemon-shutdown-"));
    const stateDirectory = join(root, "state");
    const runtimeDirectory = join(root, "runtime");
    const daemon = await startFabricDaemon({
      databasePath: join(stateDirectory, "fabric.sqlite3"),
      stateDirectory,
      runtimeDirectory,
      socketPath: join(runtimeDirectory, "fabric.sock"),
    });
    const attached = await connectFabricDaemon({
      socketPath: daemon.address.path,
      capability: daemon.bootstrapCapability,
    });
    try {
      await expect(daemon.stop()).resolves.toBeUndefined();
    } finally {
      await attached.close();
      await daemon.stop();
      await rm(root, { recursive: true, force: true });
    }
  });
});
