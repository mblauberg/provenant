import { renameSync, writeFileSync } from "node:fs";
import { rm } from "node:fs/promises";
import { join } from "node:path";

import { createLifecycleFixture } from "./lifecycle-testkit.ts";

const handoffPath = process.env.RETAINED_LIFECYCLE_HANDOFF;
if (handoffPath === undefined) throw new Error("RETAINED_LIFECYCLE_HANDOFF is required");

let fixture: Awaited<ReturnType<typeof createLifecycleFixture>> | undefined;
try {
  fixture = await createLifecycleFixture({
    retainedAgents: true,
    retainedDaemonStarted: async ({ pid, directory, releaseDaemonHandle }) => {
      // Deliberately release only the daemon handle. This drops the worker's
      // child/pipe custody so the test proves the registry owns the exact PID.
      releaseDaemonHandle();
      const runtimeDirectory = join(directory, "runtime");
      const handoff = {
        daemonPid: pid,
        directory,
        runtimeDirectory,
        stateDirectory: join(directory, "state"),
        socketPath: join(runtimeDirectory, "fabric.sock"),
        daemonHandleReleased: true,
      };
      const temporaryHandoffPath = `${handoffPath}.tmp-${process.pid}`;
      writeFileSync(temporaryHandoffPath, `${JSON.stringify(handoff)}\n`);
      renameSync(temporaryHandoffPath, handoffPath);
    },
  });
  await new Promise<void>(() => undefined);
} finally {
  if (fixture !== undefined) {
    await fixture.fabric.close().catch(() => undefined);
    await rm(fixture.directory, { recursive: true, force: true });
  }
}
