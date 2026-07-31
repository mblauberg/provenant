import { spawn } from "node:child_process";
import { mkdtemp, readFile, rm, stat, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { fileURLToPath } from "node:url";

import { describe, expect, it } from "vitest";

import { waitForFile } from "../../shared/deadline-wait.ts";
import { runSourceCli } from "../../support/cli-process.ts";
import { createPrimaryCompatibilityFixture } from "../../support/primary-adapter-testkit.ts";

const packageRoot = fileURLToPath(new URL("../../../", import.meta.url));
const sourceCli = fileURLToPath(new URL("../../../src/cli/main.ts", import.meta.url));

describe("foreground daemon CLI", () => {
  it("runs core-only from resolved paths and publishes a private discovery receipt", async () => {
    const root = await mkdtemp(join(tmpdir(), "fabric-cli-daemon-"));
    const stateDirectory = join(root, "state");
    const runtimeDirectory = join(root, "runtime");
    const discoveryPath = join(runtimeDirectory, "fabric-v1.discovery.json");
    const child = spawn(process.execPath, ["--import", "tsx", sourceCli, "daemon", "run", "--no-adapters"], {
      cwd: packageRoot,
      env: {
        ...process.env,
        AGENT_FABRIC_STATE_DIRECTORY: stateDirectory,
        AGENT_FABRIC_RUNTIME_DIRECTORY: runtimeDirectory,
      },
      stdio: ["ignore", "pipe", "pipe"],
    });
    const stderr: Buffer[] = [];
    const stdout: Buffer[] = [];
    child.stderr.on("data", (chunk: Buffer) => stderr.push(chunk));
    child.stdout.on("data", (chunk: Buffer) => stdout.push(chunk));
    const exitPromise = new Promise<{ code: number | null; signal: NodeJS.Signals | null }>((resolve) => {
      child.once("close", (code, signal) => resolve({ code, signal }));
    });
    try {
      await waitForFile(discoveryPath, { timeoutMs: 8_000, pollIntervalMs: 20 });
      const receipt: unknown = JSON.parse(await readFile(discoveryPath, "utf8"));
      expect(receipt).toMatchObject({
        schemaVersion: 1,
        socketPath: join(runtimeDirectory, "fabric-v1.sock"),
        pid: expect.any(Number),
        bootstrapCapability: expect.stringMatching(/^afb_[A-Za-z0-9_-]{43}$/u),
      });
      expect((await stat(discoveryPath)).mode & 0o777).toBe(0o600);
      expect((await stat(join(runtimeDirectory, "fabric-v1.sock"))).isSocket()).toBe(true);
    } finally {
      if (child.exitCode === null && child.signalCode === null) child.kill("SIGTERM");
      const exit = await exitPromise;
      await rm(root, { recursive: true, force: true });
      expect(exit).toEqual({ code: 0, signal: null });
      expect(Buffer.concat(stderr).toString("utf8")).toBe("");
      const visible = Buffer.concat(stdout).toString("utf8");
      expect(visible).toMatch(/agent-fabric ready pid=\d+ protocol=1 .*AEST \(UTC\+10\)/u);
      expect(visible).toContain("agent-fabric stopped");
      expect(visible).not.toMatch(/af[bc]_[A-Za-z0-9_-]{43}/u);
    }
  });

  it("fails closed when explicit trusted configuration selects disabled adapters", async () => {
    const fixture = await createPrimaryCompatibilityFixture();
    const root = fixture.directory;
    try {
      const trustedConfigPath = join(root, "agent-fabric.yaml");
      await writeFile(trustedConfigPath, `${JSON.stringify({
        schemaVersion: 1,
        allowedAdapters: ["claude-agent-sdk"],
        activeAdapters: ["claude-agent-sdk"],
        allowedProfiles: ["headless"],
        adapters: {
          "claude-agent-sdk": { command: [process.execPath, fixture.artifactPaths[0]] },
        },
        workspaceRoots: [root],
        limits: { maximumConcurrentProviderTurns: 1 },
      }, null, 2)}\n`, "utf8");
      const result = await runSourceCli(
        [
          "daemon",
          "run",
          "--trusted-config",
          trustedConfigPath,
          "--compatibility",
          fixture.compatibilityPath,
          "--compatibility-schema",
          fixture.schemaPath,
          "--agents-home",
          root,
        ],
        {
          environment: {
            AGENT_FABRIC_STATE_DIRECTORY: join(root, "state"),
            AGENT_FABRIC_RUNTIME_DIRECTORY: join(root, "runtime"),
          },
        },
      );
      expect(result.exitCode).toBe(1);
      expect(result.stderr).toMatch(/not activated|disabled/u);
    } finally {
      await rm(root, { recursive: true, force: true });
    }
  });
});
