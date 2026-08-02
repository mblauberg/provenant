import { access, chmod, mkdtemp, readFile, rm, writeFile } from "node:fs/promises";
import { createConnection } from "node:net";
import { performance } from "node:perf_hooks";
import { tmpdir } from "node:os";
import { join } from "node:path";

import { describe, expect, it, vi } from "vitest";

import { probeProviderInterface, runProbe } from "../../src/adapters/provider-interface.ts";
import { waitForFile, waitUntil } from "../shared/deadline-wait.ts";

async function writeExecutable(directory: string, name: string, source: string): Promise<string> {
  const executable = join(directory, name);
  await writeFile(executable, `#!/usr/bin/env node\n${source}\n`, { mode: 0o755 });
  await chmod(executable, 0o755);
  return executable;
}

async function killProcessFromFile(path: string): Promise<void> {
  try {
    const pid = Number(await waitForNonEmptyFile(path, 1_000));
    if (Number.isInteger(pid)) process.kill(pid, "SIGKILL");
  } catch {
    // The Unix process group may already have closed the helper.
  }
}

async function waitForNonEmptyFile(path: string, timeoutMs = 5_000): Promise<string> {
  await waitForFile(path, { timeoutMs });
  return await waitUntil(async () => {
    try {
      const contents = await readFile(path, "utf8");
      return contents.length > 0 ? contents : undefined;
    } catch (error: unknown) {
      if ((error as NodeJS.ErrnoException).code === "ENOENT") return undefined;
      throw error;
    }
  }, timeoutMs, `File ${path} contents`);
}

async function waitForFileContents(path: string, expected: string, timeoutMs = 5_000): Promise<void> {
  await waitForFile(path, { timeoutMs });
  await waitUntil(async () => {
    try {
      return (await readFile(path, "utf8")) === expected;
    } catch (error: unknown) {
      if ((error as NodeJS.ErrnoException).code === "ENOENT") return false;
      throw error;
    }
  }, timeoutMs, `File ${path} contents`);
}

async function expectUnixSocketClosed(path: string): Promise<void> {
  await new Promise<void>((resolve, reject) => {
    const socket = createConnection(path);
    socket.once("connect", () => {
      socket.destroy();
      reject(new Error(`identity-bearing helper socket remained live: ${path}`));
    });
    socket.once("error", (error: NodeJS.ErrnoException) => {
      if (error.code === "ENOENT" || error.code === "ECONNREFUSED") resolve();
      else reject(error);
    });
  });
}

async function expectIncompleteBefore(
  probe: Promise<unknown>,
  causeMessage?: string,
  timeoutMs = 1_500,
): Promise<void> {
  const expected = causeMessage === undefined
    ? { code: "ADAPTER_INTERFACE_PROBE_INCOMPLETE" }
    : {
      code: "ADAPTER_INTERFACE_PROBE_INCOMPLETE",
      cause: expect.objectContaining({ message: causeMessage }),
    };
  await expectRejectedBefore(probe, expected, timeoutMs);
}

async function expectRejectedBefore(
  probe: Promise<unknown>,
  expected: object,
  timeoutMs = 1_500,
): Promise<void> {
  let outcome: { status: "fulfilled"; value: unknown } | { status: "rejected"; reason: unknown } | undefined;
  void probe.then(
    (value) => { outcome = { status: "fulfilled", value }; },
    (reason: unknown) => { outcome = { status: "rejected", reason }; },
  );
  const observed = await waitUntil(
    () => outcome,
    timeoutMs,
    "provider probe settlement",
  );
  if (observed.status === "fulfilled") {
    throw new Error(`provider probe unexpectedly resolved: ${JSON.stringify(observed.value)}`);
  }
  expect(observed.reason).toMatchObject(expected);
}

async function resolveBefore<T>(probe: Promise<T>, timeoutMs = 1_500): Promise<T> {
  let outcome: { status: "fulfilled"; value: T } | { status: "rejected"; reason: unknown } | undefined;
  void probe.then(
    (value) => { outcome = { status: "fulfilled", value }; },
    (reason: unknown) => { outcome = { status: "rejected", reason }; },
  );
  const observed = await waitUntil(() => outcome, timeoutMs, "provider probe settlement");
  if (observed.status === "rejected") throw observed.reason;
  return observed.value;
}

describe("provider non-answer interface conformance", () => {
  it.skipIf(process.platform === "win32")("accepts a valid Codex response followed by runner termination", async () => {
    const directory = await mkdtemp(join(tmpdir(), "agent-fabric-provider-interface-"));
    const response = '{"id":1,"result":{"userAgent":"runner-terminated"}}\n';
    const executable = await writeExecutable(directory, "provider", [
      `process.stdout.write(${JSON.stringify(response)});`,
      "setInterval(() => undefined, 1_000);",
    ].join("\n"));
    try {
      await expect(probeProviderInterface({ adapterId: "codex-app-server", executable }))
        .resolves.toMatchObject({ adapterId: "codex-app-server", conformant: true, version: "runner-terminated" });
    } finally {
      await rm(directory, { recursive: true, force: true });
    }
  });

  it.skipIf(process.platform === "win32")("rejects a late same-group response after the leader exits abnormally", async () => {
    const directory = await mkdtemp(join(tmpdir(), "agent-fabric-provider-interface-"));
    const response = '{"id":1,"result":{"userAgent":"late-response"}}\n';
    const helperSource = [
      `process.once("SIGTERM", () => { process.stdout.write(${JSON.stringify(response)}, () => process.exit(0)); });`,
      "setInterval(() => undefined, 1_000);",
    ].join("\n");
    const executable = await writeExecutable(directory, "provider", [
      'const { spawn } = require("node:child_process");',
      `spawn(process.execPath, ["-e", ${JSON.stringify(helperSource)}], { stdio: ["ignore", "inherit", "inherit"] });`,
      "setTimeout(() => process.exit(23), 250);",
    ].join("\n"));
    let observed: Awaited<ReturnType<typeof runProbe>> | undefined;
    try {
      await expect(probeProviderInterface({ adapterId: "codex-app-server", executable }, async (input) => {
        observed = await runProbe(input);
        return observed;
      })).rejects.toMatchObject({
        code: "ADAPTER_INTERFACE_PROBE_INCOMPLETE",
      });
      expect(observed).toMatchObject({ stdout: response, exitCode: 23, signal: null });
    } finally {
      await rm(directory, { recursive: true, force: true });
    }
  });

  it.skipIf(process.platform === "win32")("cleans Unix descendants through public timeout and output probes", async () => {
    const directory = await mkdtemp(join(tmpdir(), "agent-fabric-provider-interface-"));
    const timeoutMarkers = ["help", "version"].map((flag) => join(directory, `timeout-provider-${flag}.term`));
    const timeoutPidFiles = timeoutMarkers.map((marker) => `${marker}.pid`);
    const outputMarkers = ["help", "version"].map((flag) => join(directory, `output-provider-${flag}.term`));
    const outputPidFiles = outputMarkers.map((marker) => `${marker}.pid`);
    const activeRuns: Promise<unknown>[] = [];
    const runUsing = (prefix: string, outputLimit: boolean) => {
      return (input: Parameters<typeof runProbe>[0]): ReturnType<typeof runProbe> => {
        const flag = input.args[0]?.replace(/^-+/u, "");
        if (flag !== "help" && flag !== "version") throw new Error("unexpected provider probe fixture argument");
        const marker = join(directory, `${prefix}-${flag}.term`);
        const helperSource = [
          'const fs = require("node:fs");',
          `process.once("SIGTERM", () => { fs.writeFileSync(${JSON.stringify(`${marker}.tmp`)}, "term"); fs.renameSync(${JSON.stringify(`${marker}.tmp`)}, ${JSON.stringify(marker)}); process.exit(0); });`,
          `fs.writeFileSync(${JSON.stringify(`${marker}.pid.tmp`)}, String(process.pid)); fs.renameSync(${JSON.stringify(`${marker}.pid.tmp`)}, ${JSON.stringify(`${marker}.pid`)});`,
          'if (process.send) process.send("ready");',
          "setInterval(() => undefined, 1_000);",
        ].join("\n");
        const parentSource = [
          'const { spawn } = require("node:child_process");',
          `const helper = spawn(process.execPath, ["-e", ${JSON.stringify(helperSource)}], { stdio: ["ignore", "inherit", "inherit", "ipc"] });`,
          outputLimit ? `helper.once("message", () => process.stdout.write("x".repeat(${1024 * 1024 + 1})));` : 'helper.once("message", () => undefined);',
          "setInterval(() => undefined, 1_000);",
        ].join("\n");
        const probe = runProbe({
          ...input,
          executable: process.execPath,
          args: ["-e", parentSource],
          timeoutMs: 2_000,
        });
        activeRuns.push(probe);
        return probe;
      };
    };
    try {
      const timeoutProbe = probeProviderInterface(
        { adapterId: "claude-agent-sdk", executable: process.execPath },
        runUsing("timeout-provider", false),
      );
      void timeoutProbe.catch(() => undefined);
      await Promise.all(timeoutPidFiles.map((pidFile) => waitForNonEmptyFile(pidFile)));
      await expectIncompleteBefore(timeoutProbe, undefined, 4_000);
      await Promise.allSettled(activeRuns);

      activeRuns.length = 0;
      const outputProbe = probeProviderInterface(
        { adapterId: "claude-agent-sdk", executable: process.execPath },
        runUsing("output-provider", true),
      );
      void outputProbe.catch(() => undefined);
      await Promise.all(outputPidFiles.map((pidFile) => waitForNonEmptyFile(pidFile)));
      await expectIncompleteBefore(outputProbe, "provider interface probe exceeded output limit", 4_000);
      await Promise.allSettled(activeRuns);
    } finally {
      await Promise.all([...timeoutPidFiles, ...outputPidFiles].map((pidFile) => killProcessFromFile(pidFile)));
      await Promise.allSettled(activeRuns);
      await rm(directory, { recursive: true, force: true });
    }
  });

  it("waits for observed exit after a first-line provider resists SIGTERM", async () => {
    const response = '{"id":1,"result":{"userAgent":"sigterm-resistant"}}\n';
    const startedAt = performance.now();
    await expect(runProbe({
      executable: process.execPath,
      args: ["-e", [
        "process.once(\"SIGTERM\", () => setTimeout(() => process.exit(23), 120));",
        "setInterval(() => undefined, 1_000);",
        `process.stdout.write(${JSON.stringify(response)});`,
      ].join("\n")],
      stdin: "",
      closeOnFirstLine: true,
      timeoutMs: 2_000,
    })).resolves.toEqual({ stdout: response, stderr: "", exitCode: 0, signal: null });
    expect(performance.now() - startedAt).toBeGreaterThanOrEqual(80);
  });

  it.skipIf(process.platform === "win32")("settles cleanly when an escaped descendant with inherited pipes outlives a clean leader", async () => {
    const directory = await mkdtemp(join(tmpdir(), "agent-fabric-provider-interface-"));
    const helperPidFile = join(directory, "escaped-helper.pid");
    const response = '{"id":1,"result":{"userAgent":"escaped-helper"}}\n';
    let settled = false;
    try {
      const helperSource = [
        'const fs = require("node:fs");',
        "setInterval(() => undefined, 1_000);",
      ].join("\n");
      const probe = runProbe({
        executable: process.execPath,
        args: ["-e", [
          'const { spawn } = require("node:child_process");',
          `const helper = spawn(process.execPath, ["-e", ${JSON.stringify(helperSource)}], { detached: true, stdio: ["ignore", "inherit", "inherit"] });`,
          `require("node:fs").writeFileSync(${JSON.stringify(helperPidFile)}, String(helper.pid));`,
          `process.stdout.write(${JSON.stringify(response)});`,
          "setTimeout(() => process.exit(0), 20);",
        ].join("\n")],
        stdin: "",
        closeOnFirstLine: true,
        timeoutMs: 500,
      }).then((result) => {
        settled = true;
        return result;
      });
      void probe.catch(() => { settled = true; });
      await waitForNonEmptyFile(helperPidFile);
      const result = await resolveBefore(probe);
      expect(result).toEqual({ stdout: response, stderr: "", exitCode: 0, signal: null });
      expect(settled).toBe(true);
    } finally {
      await killProcessFromFile(helperPidFile);
      await rm(directory, { recursive: true, force: true });
    }
  });

  it.skipIf(process.platform === "win32")("bounds timeout and output failures when escaped descendants withhold pipe closure", async () => {
    const directory = await mkdtemp(join(tmpdir(), "agent-fabric-provider-interface-"));
    const cases = [
      { name: "timeout", outputLimit: false, causeMessage: undefined },
      { name: "output", outputLimit: true, causeMessage: "provider interface probe exceeded output limit" },
    ] as const;
    const pidFiles = cases.map(({ name }) => join(directory, `${name}.pid`));
    const probes: Promise<unknown>[] = [];
    const runWithEscapedPipe = (pidFile: string, outputLimit: boolean): Promise<unknown> => {
      const helperSource = "setInterval(() => undefined, 1_000);";
      const leaderSource = [
        'const { spawn } = require("node:child_process");',
        `const helper = spawn(process.execPath, ["-e", ${JSON.stringify(helperSource)}], { detached: true, stdio: ["ignore", "inherit", "inherit"] });`,
        `require("node:fs").writeFileSync(${JSON.stringify(`${pidFile}.tmp`)}, String(helper.pid)); require("node:fs").renameSync(${JSON.stringify(`${pidFile}.tmp`)}, ${JSON.stringify(pidFile)});`,
        outputLimit ? `process.stdout.write("x".repeat(${1024 * 1024 + 1}));` : "",
        "setInterval(() => undefined, 1_000);",
      ].join("\n");
      const probe = runProbe({
        executable: process.execPath,
        args: ["-e", leaderSource],
        timeoutMs: 1_000,
      });
      probes.push(probe);
      return probe;
    };
    try {
      for (const [index, testCase] of cases.entries()) {
        const pidFile = pidFiles[index];
        if (pidFile === undefined) throw new Error(`missing escaped helper pid file for ${testCase.name}`);
        const probe = runWithEscapedPipe(pidFile, testCase.outputLimit);
        await waitForNonEmptyFile(pidFile, 10_000);
        await expectRejectedBefore(probe, { message: testCase.causeMessage ?? "provider interface probe timed out" }, 1_500);
      }
    } finally {
      await Promise.all(pidFiles.map((pidFile) => killProcessFromFile(pidFile)));
      await Promise.allSettled(probes);
      await rm(directory, { recursive: true, force: true });
    }
  });

  it("waits for the direct child to close before rejecting a timeout", async () => {
    const directory = await mkdtemp(join(tmpdir(), "agent-fabric-provider-interface-"));
    const terminationMarker = join(directory, "term.txt");
    const startedAt = performance.now();
    try {
      const probe = runProbe({
        executable: process.execPath,
        args: ["-e", [
          `process.once("SIGTERM", () => { require("node:fs").writeFileSync(${JSON.stringify(terminationMarker)}, "term"); setTimeout(() => process.exit(23), 120); });`,
          "setInterval(() => undefined, 1_000);",
        ].join("\n")],
        // 2_000 like every other probe in this file, not 200. The child only
        // installs its SIGTERM handler once Node has finished starting, which
        // takes past 200ms whenever the suite runs its files in parallel. A
        // shorter deadline signals the child before the handler exists, so it
        // dies to the default action, never writes the marker, and the test
        // reads a startup race as a custody failure.
        timeoutMs: 2_000,
      });
      await waitForFile(terminationMarker);
      await expect(probe).rejects.toThrow("provider interface probe timed out");
      expect(performance.now() - startedAt).toBeGreaterThanOrEqual(80);
    } finally {
      await rm(directory, { recursive: true, force: true });
    }
  });

  it("waits for the direct child to close after an output-limit failure", async () => {
    const directory = await mkdtemp(join(tmpdir(), "agent-fabric-provider-interface-"));
    const terminationMarker = join(directory, "term.txt");
    const startedAt = performance.now();
    try {
      const probe = runProbe({
        executable: process.execPath,
        args: ["-e", [
          `process.once("SIGTERM", () => { require("node:fs").writeFileSync(${JSON.stringify(terminationMarker)}, "term"); setTimeout(() => process.exit(23), 120); });`,
          `process.stdout.write("x".repeat(${1024 * 1024 + 1}));`,
          "setInterval(() => undefined, 1_000);",
        ].join("\n")],
        timeoutMs: 2_000,
      });
      await waitForFile(terminationMarker);
      await expect(probe).rejects.toThrow("provider interface probe exceeded output limit");
      expect(performance.now() - startedAt).toBeGreaterThanOrEqual(80);
    } finally {
      await rm(directory, { recursive: true, force: true });
    }
  });

  it("retains a direct-child stdin error until observed close", async () => {
    await expect(runProbe({
      executable: process.execPath,
      args: ["-e", "process.exit(0)"],
      stdin: "x".repeat(16 * 1024 * 1024),
      timeoutMs: 2_000,
    })).rejects.toMatchObject({ code: expect.stringMatching(/EPIPE|ERR_STREAM_DESTROYED/u) });
  });

  it("preserves the first-line bytes and observed clean exit", async () => {
    const response = '{"id":1,"result":{"userAgent":"clean-exit"}}\n';
    await expect(runProbe({
      executable: process.execPath,
      args: ["-e", [
        "process.once(\"SIGTERM\", () => setTimeout(() => process.exit(0), 10));",
        `process.stdin.on("end", () => process.stdout.write(${JSON.stringify(response)}));`,
        "process.stdin.resume();",
        "setInterval(() => undefined, 1_000);",
      ].join("\n")],
      stdin: "",
      closeOnFirstLine: true,
      timeoutMs: 2_000,
    })).resolves.toEqual({ stdout: response, stderr: "", exitCode: 0, signal: null });
  });

  it("escalates a first-line provider after bounded SIGTERM grace", async () => {
    const response = '{"id":1,"result":{"userAgent":"sigkill-escalation"}}\n';
    await expect(runProbe({
      executable: process.execPath,
      args: [
        "-e",
        [
          "process.once(\"SIGTERM\", () => setTimeout(() => process.exit(29), 1_000));",
          "setInterval(() => undefined, 1_000);",
          `process.stdout.write(${JSON.stringify(response)});`,
        ].join("\n"),
      ],
      stdin: "",
      closeOnFirstLine: true,
      timeoutMs: 2_000,
    })).resolves.toEqual({ stdout: response, stderr: "", exitCode: 0, signal: null });
  });

  it.skipIf(process.platform === "win32")("cleans a SIGTERM-resistant same-group helper after a clean leader exit", async () => {
    const directory = await mkdtemp(join(tmpdir(), "agent-fabric-provider-interface-"));
    const terminationMarker = join(directory, "helper-term.txt");
    const helperSocket = join(directory, "helper.sock");
    const helperReady = join(directory, "helper.ready");
    const response = '{"id":1,"result":{"userAgent":"group-owned"}}\n';
    let settled = false;
    try {
      const helperSource = [
        `const fs = require("node:fs"); const net = require("node:net"); const server = net.createServer((socket) => socket.destroy()); server.listen(${JSON.stringify(helperSocket)}, () => { fs.writeFileSync(${JSON.stringify(`${helperReady}.tmp`)}, "ready"); fs.renameSync(${JSON.stringify(`${helperReady}.tmp`)}, ${JSON.stringify(helperReady)}); process.send?.("ready"); });`,
        `process.once("SIGTERM", () => { fs.writeFileSync(${JSON.stringify(`${terminationMarker}.tmp`)}, "terminated"); fs.renameSync(${JSON.stringify(`${terminationMarker}.tmp`)}, ${JSON.stringify(terminationMarker)}); });`,
        "setInterval(() => undefined, 1_000);",
      ].join("\n");
      const probe = runProbe({
        executable: process.execPath,
        args: ["-e", [
          "const { spawn } = require(\"node:child_process\");",
          `const helper = spawn(process.execPath, ["-e", ${JSON.stringify(helperSource)}], { stdio: ["ignore", "ignore", "ignore", "ipc"] });`,
          `helper.once("message", () => { process.stdout.write(${JSON.stringify(response)}); process.exit(0); });`,
        ].join("\n")],
        stdin: "",
        timeoutMs: 2_000,
      }).then((result) => {
        settled = true;
        return result;
      });
      void probe.catch(() => { settled = true; });
      await waitForFileContents(helperReady, "ready");
      await waitForFileContents(terminationMarker, "terminated");
      expect(settled).toBe(false);
      const result = await probe;
      await waitForFile(terminationMarker);
      expect(result.stdout).toBe(response);
      expect(result).toEqual({ stdout: response, stderr: "", exitCode: 0, signal: null });
      await expectUnixSocketClosed(helperSocket);
      expect(settled).toBe(true);
    } finally {
      await rm(directory, { recursive: true, force: true });
    }
  });

  it("bounds the direct-child fallback after a successful final KILL", async () => {
    vi.stubEnv("AGENT_FABRIC_PROVIDER_PROBE_FORCE_DIRECT_CHILD", "1");
    const response = '{"id":1,"result":{"userAgent":"direct-child-fallback"}}\n';
    try {
      await expect(runProbe({
        executable: process.execPath,
        args: ["-e", [
          "process.once(\"SIGTERM\", () => undefined);",
          `process.stdout.write(${JSON.stringify(response)});`,
          "setInterval(() => undefined, 1_000);",
        ].join("\n")],
        closeOnFirstLine: true,
        timeoutMs: 2_000,
      })).resolves.toEqual({ stdout: response, stderr: "", exitCode: 0, signal: null });
    } finally {
      vi.unstubAllEnvs();
    }
  });

  it("fails closed when final KILL has no observed provider exit", async () => {
    vi.stubEnv("AGENT_FABRIC_PROVIDER_PROBE_SUPPRESS_EXIT_OBSERVATION", "1");
    const response = '{"id":1,"result":{"userAgent":"unobserved-exit"}}\n';
    try {
      await expect(runProbe({
        executable: process.execPath,
        args: ["-e", [
          "process.once(\"SIGTERM\", () => undefined);",
          `process.stdout.write(${JSON.stringify(response)});`,
          "setInterval(() => undefined, 1_000);",
        ].join("\n")],
        closeOnFirstLine: true,
        timeoutMs: 2_000,
      })).rejects.toThrow(/provider (?:direct-child )?exit was not observed/u);
    } finally {
      vi.unstubAllEnvs();
    }
  });

  it("fails once on synthetic EPERM without later KILL escalation", async () => {
    vi.stubEnv("AGENT_FABRIC_PROVIDER_PROBE_FORCE_SIGNAL_ERROR", "1");
    const directory = await mkdtemp(join(tmpdir(), "agent-fabric-provider-interface-"));
    const marker = join(directory, "term.txt");
    const pidFile = join(directory, "provider.pid");
    const response = '{"id":1,"result":{"userAgent":"eperm"}}\n';
    const executable = await writeExecutable(directory, "provider", [
      'const fs = require("node:fs");',
      `fs.writeFileSync(${JSON.stringify(pidFile)}, String(process.pid));`,
      `process.once("SIGTERM", () => fs.writeFileSync(${JSON.stringify(marker)}, "term"));`,
      `process.stdout.write(${JSON.stringify(response)});`,
      "setInterval(() => undefined, 1_000);",
    ].join("\n"));
    try {
      await expect(runProbe({ executable, args: [], closeOnFirstLine: true, timeoutMs: 2_000 }))
        .rejects.toMatchObject({ code: "EPERM" });
      await expect(access(marker)).rejects.toMatchObject({ code: "ENOENT" });
    } finally {
      await killProcessFromFile(pidFile);
      vi.unstubAllEnvs();
      await rm(directory, { recursive: true, force: true });
    }
  });

  it("retains a spawn error until the child close notification", async () => {
    await expect(runProbe({
      executable: join(tmpdir(), "agent-fabric-provider-interface-missing"),
      args: [],
      timeoutMs: 2_000,
    })).rejects.toMatchObject({ code: "ENOENT" });
  });

  it("preserves a natural non-zero leader exit without first-line custody", async () => {
    await expect(runProbe({
      executable: process.execPath,
      args: ["-e", "process.exit(23)"],
      timeoutMs: 2_000,
    })).resolves.toEqual({ stdout: "", stderr: "", exitCode: 23, signal: null });
  });

  it("keeps parent custody signalling behind the private shim IPC", async () => {
    const kill = vi.spyOn(process, "kill");
    try {
      await expect(runProbe({
        executable: process.execPath,
        args: ["-e", "process.exit(17)"],
        timeoutMs: 2_000,
      })).resolves.toMatchObject({ exitCode: 17, signal: null });
      expect(kill).not.toHaveBeenCalled();
    } finally {
      kill.mockRestore();
    }
  });

  it.each([
    ["claude-agent-sdk", "--print --output-format stream-json"],
    ["agy", "--print --model --mode --log-file"],
    ["cursor-agent", "--print --output-format --model"],
  ] as const)("accepts the required %s headless flags", async (adapterId, stdout) => {
    const run = vi.fn(async () => ({ stdout, stderr: "", exitCode: 0 }));
    await expect(probeProviderInterface({ adapterId, executable: "/provider" }, run))
      .resolves.toMatchObject({ adapterId, conformant: true });
  });

  it("accepts complete option tokens followed by separate or equals values", async () => {
    const run = vi.fn(async () => ({
      stdout: "--print=true\n--model=<MODEL>\n--mode MODE\n--log-file=PATH",
      stderr: "",
      exitCode: 0,
    }));
    await expect(probeProviderInterface({ adapterId: "agy", executable: "/agy" }, run))
      .resolves.toMatchObject({ adapterId: "agy", conformant: true });
  });

  it("rejects prefixed and suffixed lookalike option names", async () => {
    const run = vi.fn(async () => ({
      stdout: "--sprint prefix--model --mode-extra --log-file-suffix",
      stderr: "",
      exitCode: 0,
    }));
    await expect(probeProviderInterface({ adapterId: "agy", executable: "/agy" }, run))
      .rejects.toMatchObject({ code: "ADAPTER_INTERFACE_PROBE_INCOMPLETE" });
  });

  it("proves the Codex app-server initialize handshake", async () => {
    const run = vi.fn(async () => ({ stdout: '{"id":1,"result":{"userAgent":"probe"}}\n', stderr: "", exitCode: 0 }));
    await expect(probeProviderInterface({ adapterId: "codex-app-server", executable: "/codex" }, run))
      .resolves.toMatchObject({ adapterId: "codex-app-server", conformant: true, probe: "app-server-initialize" });
  });

  it.each([
    { exitCode: 23, signal: null },
    { exitCode: 0, signal: "SIGTERM" as const },
  ])("rejects an injected result despite the old forged runner marker (%j)", async ({ exitCode, signal }) => {
    const run = vi.fn(async () => ({
      stdout: '{"id":1,"result":{"userAgent":"signalled"}}\n',
      stderr: "",
      exitCode,
      signal,
      runnerTermination: "first-line" as const,
    }));
    await expect(probeProviderInterface({ adapterId: "codex-app-server", executable: "/codex" }, run))
      .rejects.toMatchObject({ code: "ADAPTER_INTERFACE_PROBE_INCOMPLETE" });
  });

  it("classifies a Codex authentication response as an incomplete probe", async () => {
    const run = vi.fn(async () => ({
      stdout: '{"id":1,"error":{"message":"authentication required"}}\n',
      stderr: "",
      exitCode: 0,
    }));
    await expect(probeProviderInterface({ adapterId: "codex-app-server", executable: "/codex" }, run))
      .rejects.toMatchObject({
        code: "ADAPTER_INTERFACE_PROBE_INCOMPLETE",
        cause: { message: "authentication required" },
      });
  });

  it("classifies a Codex protocol error response as an interface mismatch", async () => {
    const run = vi.fn(async () => ({
      stdout: '{"id":1,"error":{"code":-32601,"message":"method not found"}}\n',
      stderr: "",
      exitCode: 0,
    }));
    await expect(probeProviderInterface({ adapterId: "codex-app-server", executable: "/codex" }, run))
      .rejects.toMatchObject({
        code: "ADAPTER_INTERFACE_MISMATCH",
        cause: { code: -32601, message: "method not found" },
      });
  });

  it("classifies an auth-shaped response with the wrong request id as an interface mismatch", async () => {
    const run = vi.fn(async () => ({
      stdout: '{"id":2,"error":{"message":"authentication required"}}\n',
      stderr: "",
      exitCode: 0,
    }));
    await expect(probeProviderInterface({ adapterId: "codex-app-server", executable: "/codex" }, run))
      .rejects.toMatchObject({
        code: "ADAPTER_INTERFACE_MISMATCH",
        cause: { message: "authentication required" },
      });
  });

  it("classifies a malformed Codex initialize result as an interface mismatch", async () => {
    const run = vi.fn(async () => ({ stdout: '{"id":1,"result":null}\n', stderr: "", exitCode: 0 }));
    await expect(probeProviderInterface({ adapterId: "codex-app-server", executable: "/codex" }, run))
      .rejects.toMatchObject({
        code: "ADAPTER_INTERFACE_MISMATCH",
        cause: expect.objectContaining({ message: "Codex initialize response is invalid" }),
      });
  });

  it("classifies a runner failure as an incomplete probe and preserves its cause", async () => {
    const cause = Object.assign(new Error("spawn failed"), { code: "ENOENT" });
    const run = vi.fn(async () => await Promise.reject(cause));
    await expect(probeProviderInterface({ adapterId: "codex-app-server", executable: "/missing" }, run))
      .rejects.toMatchObject({ code: "ADAPTER_INTERFACE_PROBE_INCOMPLETE", cause });
  });

  it("proves the Kiro ACP v1 initialize handshake", async () => {
    const run = vi.fn(async (input: { args: string[] }) => input.args.includes("--help")
      ? { stdout: "--model <MODEL> --effort <EFFORT> --agent-engine <ENGINE>", stderr: "", exitCode: 0 }
      : { stdout: '{"jsonrpc":"2.0","id":1,"result":{"protocolVersion":1}}\n', stderr: "", exitCode: 0 });
    await expect(probeProviderInterface({ adapterId: "kiro-acp", executable: "/kiro" }, run))
      .resolves.toMatchObject({ adapterId: "kiro-acp", conformant: true, probe: "acp-v1-initialize" });
  });

  it("proves the OpenCode ACP v1 initialize handshake without a model turn", async () => {
    let disposableCwd: string | undefined;
    const run = vi.fn(async (input: { cwd?: string }) => {
      disposableCwd = input.cwd;
      return ({
      stdout: '{"jsonrpc":"2.0","id":1,"result":{"protocolVersion":1,"agentInfo":{"name":"OpenCode","version":"1.17.18"}}}\n',
      stderr: "",
      exitCode: 0,
      });
    });
    await expect(probeProviderInterface({ adapterId: "opencode-acp", executable: "/opencode" }, run))
      .resolves.toMatchObject({ adapterId: "opencode-acp", conformant: true, probe: "acp-v1-initialize", version: "1.17.18" });
    expect(disposableCwd).toEqual(expect.any(String));
    expect(run).toHaveBeenCalledWith(expect.objectContaining({ args: ["acp", "--pure", "--cwd", disposableCwd], cwd: disposableCwd }));
    await expect(access(disposableCwd!)).rejects.toMatchObject({ code: "ENOENT" });
  });

  it("rejects Kiro ACP when the effort interface disappears", async () => {
    const run = vi.fn(async (input: { args: string[] }) => input.args.includes("--help")
      ? { stdout: "--model <MODEL> --agent-engine <ENGINE>", stderr: "", exitCode: 0 }
      : { stdout: '{"jsonrpc":"2.0","id":1,"result":{"protocolVersion":1}}\n', stderr: "", exitCode: 0 });
    await expect(probeProviderInterface({ adapterId: "kiro-acp", executable: "/kiro" }, run))
      .rejects.toMatchObject({ code: "ADAPTER_INTERFACE_PROBE_INCOMPLETE" });
  });

  it("fails closed when a required interface disappears", async () => {
    const run = vi.fn(async () => ({ stdout: "--print --model", stderr: "", exitCode: 0 }));
    await expect(probeProviderInterface({ adapterId: "agy", executable: "/agy" }, run))
      .rejects.toMatchObject({ code: "ADAPTER_INTERFACE_PROBE_INCOMPLETE" });
  });
});
