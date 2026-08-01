import { randomBytes } from "node:crypto";
import { spawn, type ChildProcess } from "node:child_process";
import { existsSync } from "node:fs";
import { open, readFile } from "node:fs/promises";
import { createInterface } from "node:readline";
import { fileURLToPath } from "node:url";

import type { FabricOpenOptions } from "../domain/types.js";
import type { OptionalGitHubHostedChecksConfiguration } from "../operator/github-hosted-checks.js";
import type { TrustedGitConfiguration } from "../operator/trusted-git-registry.js";
import { FabricRemoteError } from "../transport/ndjson-rpc.js";
import { BootstrapSpawnPhaseError } from "./bootstrap-client.js";
import type { HerdrDaemonProcessConfiguration } from "./herdr-composition.js";
import { isRecord } from "./protocol.js";

export type DaemonBootstrapEnvironment = {
  mode: "production-election" | "test-forced-process-locks";
  actionId: string;
  electionGeneration: number;
  daemonInstanceGeneration: number;
};

export type DaemonChildStartOptions = {
  databasePath: string;
  productRoot: string;
  stateDirectory: string;
  runtimeDirectory: string;
  socketPath: string;
  lifecycleReceiptAuthorityId?: string;
  adapters: NonNullable<FabricOpenOptions["adapters"]>;
  executionProfile: string;
  maximumConcurrentProviderTurns: number;
  workspaceRoots: string[];
  githubHostedChecks?: OptionalGitHubHostedChecksConfiguration;
  trustedGitConfiguration?: TrustedGitConfiguration;
  herdr?: HerdrDaemonProcessConfiguration;
};

export type ChildExit = { code: number | null; signal: NodeJS.Signals | null };

export type SpawnedDaemonChild = {
  bootstrapCapability: string;
  pid: number;
  ready: Promise<void>;
  exit: Promise<ChildExit>;
  isRunning(): boolean;
  release(): void;
  terminate(): Promise<void>;
};

export function childEnvironment(
  options: DaemonChildStartOptions,
  bootstrapCapability: string,
  lockPaths: string[],
  capabilityKey: string,
  bootstrap: DaemonBootstrapEnvironment,
): NodeJS.ProcessEnv {
  const environment: NodeJS.ProcessEnv = {
    AGENT_FABRIC_DATABASE_PATH: options.databasePath,
    AGENT_FABRIC_PRODUCT_ROOT: options.productRoot,
    AGENT_FABRIC_SOCKET_PATH: options.socketPath,
    AGENT_FABRIC_STATE_DIRECTORY: options.stateDirectory,
    AGENT_FABRIC_RUNTIME_DIRECTORY: options.runtimeDirectory,
    AGENT_FABRIC_BOOTSTRAP_CAPABILITY: bootstrapCapability,
    AGENT_FABRIC_BOOTSTRAP_MODE: bootstrap.mode,
    AGENT_FABRIC_BOOTSTRAP_ACTION_ID: bootstrap.actionId,
    AGENT_FABRIC_BOOTSTRAP_ELECTION_GENERATION: String(bootstrap.electionGeneration),
    AGENT_FABRIC_BOOTSTRAP_CUSTODY: "parent-pipe-v1",
    AGENT_FABRIC_DAEMON_INSTANCE_GENERATION: String(bootstrap.daemonInstanceGeneration),
    ...(bootstrap.mode === "test-forced-process-locks"
      ? { AGENT_FABRIC_DAEMON_LOCK_PATHS_JSON: JSON.stringify(lockPaths) }
      : {}),
    AGENT_FABRIC_CAPABILITY_KEY: capabilityKey,
    AGENT_FABRIC_EXECUTION_PROFILE: options.executionProfile,
    AGENT_FABRIC_MAXIMUM_CONCURRENT_PROVIDER_TURNS: String(options.maximumConcurrentProviderTurns),
    AGENT_FABRIC_WORKSPACE_ROOTS_JSON: JSON.stringify(options.workspaceRoots),
    AGENT_FABRIC_ADAPTERS_JSON: JSON.stringify(options.adapters),
    ...(options.lifecycleReceiptAuthorityId === undefined
      ? {}
      : { AGENT_FABRIC_LIFECYCLE_RECEIPT_AUTHORITY_ID: options.lifecycleReceiptAuthorityId }),
    AGENT_FABRIC_GITHUB_HOSTED_CHECKS_JSON: JSON.stringify(options.githubHostedChecks ?? { enabled: false }),
    AGENT_FABRIC_TRUSTED_GIT_JSON: JSON.stringify(options.trustedGitConfiguration ?? {}),
    AGENT_FABRIC_HERDR_JSON: JSON.stringify(options.herdr ?? { enabled: false }),
    PATH: process.env.PATH ?? "/usr/bin:/bin",
    TMPDIR: process.env.TMPDIR ?? "/tmp",
  };
  for (const key of ["HOME", "USER", "LOGNAME", "CODEX_HOME", "HTTP_PROXY", "HTTPS_PROXY", "NO_PROXY", "SSL_CERT_FILE"] as const) {
    const value = process.env[key];
    if (value !== undefined) environment[key] = value;
  }
  const testFixtureKeys = [
    "AGENT_FABRIC_TEST_CUTOVER_RACE_FIXTURE_PATH",
    "AGENT_FABRIC_TEST_IDLE_STOP_ATTEMPT_SOCKET_PATH",
    "AGENT_FABRIC_TEST_BOOTSTRAP_SOCKET_BARRIER_PATH",
    "AGENT_FABRIC_TEST_DATABASE_INSPECTION_ATTEMPTS",
    "AGENT_FABRIC_TEST_BOOTSTRAP_MALFORMED_RESULT_COUNT",
  ] as const;
  if (process.env.NODE_ENV === "test") {
    const testFixtures = testFixtureKeys.flatMap((key) => {
      const value = process.env[key];
      return value === undefined ? [] : [[key, value] as const];
    });
    if (testFixtures.length > 0) {
      environment.NODE_ENV = "test";
      for (const [key, value] of testFixtures) environment[key] = value;
    }
  }
  return environment;
}

async function loadOrCreateCapabilityKey(stateDirectory: string): Promise<string> {
  const path = `${stateDirectory}/capability.key`;
  try {
    const key = (await readFile(path, "utf8")).trim();
    if (!/^[A-Za-z0-9_-]{43}$/u.test(key)) throw new Error("agent fabric capability key is invalid");
    return key;
  } catch (error: unknown) {
    if (isRecord(error) && error.code !== "ENOENT") throw error;
    const key = randomBytes(32).toString("base64url");
    const handle = await open(path, "wx", 0o600);
    try {
      await handle.writeFile(`${key}\n`);
    } finally {
      await handle.close();
    }
    return key;
  }
}

async function waitUntilReady(child: ChildProcess, stderr: () => string): Promise<void> {
  if (child.stdout === null) throw new Error("daemon stdout is unavailable");
  const lines = createInterface({ input: child.stdout, crlfDelay: Infinity });
  await new Promise<void>((resolve, reject) => {
    const timeout = setTimeout(() => reject(new Error("agent fabric daemon startup timed out")), 10_000);
    const onExit = (code: number | null, signal: NodeJS.Signals | null): void => {
      clearTimeout(timeout);
      reject(new Error(`agent fabric daemon exited before ready (${String(code)}, ${String(signal)}): ${stderr()}`));
    };
    child.once("exit", onExit);
    lines.once("line", (line) => {
      clearTimeout(timeout);
      child.off("exit", onExit);
      try {
        const value: unknown = JSON.parse(line);
        if (isRecord(value) && value.ready === false && isRecord(value.error) && typeof value.error.code === "string" && typeof value.error.message === "string") {
          reject(new FabricRemoteError(value.error.code, value.error.message, {
            preserved: value.error.preserved === true,
          }));
          return;
        }
        if (!isRecord(value) || value.ready !== true) {
          reject(new Error("agent fabric daemon returned an invalid ready message"));
          return;
        }
        resolve();
      } catch (error: unknown) {
        reject(error instanceof Error ? error : new Error(String(error)));
      }
    });
  });
  lines.close();
}

export async function holdDaemonChildBeforeDiscovery(pid: number): Promise<void> {
  const barrierPath = process.env.NODE_ENV === "test"
    ? process.env.AGENT_FABRIC_TEST_BOOTSTRAP_CUSTODY_BARRIER_PATH
    : undefined;
  if (barrierPath === undefined) return;
  const barrier = await open(barrierPath, "wx", 0o600);
  try {
    await barrier.writeFile(`${String(pid)}\n`, "utf8");
  } finally {
    await barrier.close();
  }
  while (existsSync(barrierPath)) {
    await new Promise<void>((resolvePromise) => setTimeout(resolvePromise, 20));
  }
}

export async function spawnDaemonChild(
  options: DaemonChildStartOptions,
  bootstrap: DaemonBootstrapEnvironment,
): Promise<SpawnedDaemonChild> {
  const capabilityKey = await loadOrCreateCapabilityKey(options.stateDirectory);
  const lockPaths = bootstrap.mode === "test-forced-process-locks"
    ? [`${options.socketPath}.lock`, `${options.databasePath}.daemon.lock`]
    : [];
  const bootstrapCapability = `afb_${randomBytes(32).toString("base64url")}`;
  const sourceMode = import.meta.url.endsWith(".ts");
  const processUrl = new URL(sourceMode ? "./process.ts" : "./process.js", import.meta.url);
  const packageRoot = fileURLToPath(new URL("../..", import.meta.url));
  const args = sourceMode
    ? ["--import", "tsx", fileURLToPath(processUrl)]
    : [fileURLToPath(processUrl)];
  const child = spawn(process.execPath, args, {
    cwd: packageRoot,
    detached: true,
    env: childEnvironment(options, bootstrapCapability, lockPaths, capabilityKey, bootstrap),
    stdio: ["pipe", "pipe", "pipe"],
  });
  if (child.pid === undefined || child.stdin === null || child.stdout === null || child.stderr === null) {
    child.kill("SIGKILL");
    throw new Error("failed to start agent fabric daemon process");
  }
  child.stdin.on("error", () => undefined);
  let stderr = "";
  child.stderr.setEncoding("utf8");
  child.stderr.on("data", (chunk: string) => {
    stderr = `${stderr}${chunk}`.slice(-8192);
  });
  const exitPromise = new Promise<ChildExit>((resolvePromise) => {
    child.once("exit", (code, signal) => resolvePromise({ code, signal }));
  });
  const pid = child.pid;
  let released = false;
  let stopPromise: Promise<void> | undefined;
  const stopChild = async (): Promise<void> => {
    if (child.exitCode !== null || child.signalCode !== null) {
      await exitPromise;
      return;
    }
    child.kill("SIGTERM");
    const graceful = await new Promise<boolean>((resolve) => {
      const timeout = setTimeout(() => resolve(false), 2_000);
      void exitPromise.then(() => {
        clearTimeout(timeout);
        resolve(true);
      });
    });
    if (!graceful && child.exitCode === null && child.signalCode === null) {
      child.kill("SIGKILL");
    }
    await exitPromise;
  };
  const ready = (async (): Promise<void> => {
    try {
      await waitUntilReady(child, () => stderr);
    } catch (error: unknown) {
      child.kill("SIGKILL");
      await exitPromise;
      if (error instanceof FabricRemoteError) throw error;
      throw new BootstrapSpawnPhaseError(
        "spawn",
        error instanceof Error ? error.message : String(error),
        { cause: error },
      );
    }
  })();
  return {
    bootstrapCapability,
    pid,
    ready,
    exit: exitPromise,
    isRunning(): boolean {
      return child.exitCode === null && child.signalCode === null;
    },
    release(): void {
      if (released) return;
      released = true;
      child.stdin.end("release\n");
      child.unref();
      const stdout = child.stdout as typeof child.stdout & { unref?: () => void };
      const stderrStream = child.stderr as typeof child.stderr & { unref?: () => void };
      stdout.unref?.();
      stderrStream.unref?.();
    },
    terminate(): Promise<void> {
      stopPromise ??= stopChild();
      return stopPromise;
    },
  };
}
