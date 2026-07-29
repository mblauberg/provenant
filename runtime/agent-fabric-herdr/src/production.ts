import { execFile } from "node:child_process";
import { createHash } from "node:crypto";
import { lstat, readFile, realpath } from "node:fs/promises";
import { isAbsolute } from "node:path";

import { parseIdentifier, parseSha256Digest } from "@local/agent-fabric-protocol";

import type { FabricActionJournalPort, FabricDirectSteerPort } from "./contracts.js";
import { DirectSteerService } from "./direct-steer.js";
import { HerdrEffectEvidenceJournal } from "./effect-journal.js";
import { HerdrAdapter } from "./herdr-adapter.js";
import { HerdrCliBoundary, type HerdrCommandPort, type HerdrCommandRequest } from "./native-boundary.js";

export type ProductionHerdrIntegrationOptions = Readonly<{
  executable: string;
  expectedProtocol: number;
  stateDirectory: string;
  projectId: string;
  projectSessionId: string;
  canonicalProjectRoot: string;
  consoleExecutable: string;
  consoleExecutableDigest: string;
  observerExecutable?: string;
  observerExecutableDigest?: string;
  observerSocketPath?: string;
  observerCapabilityFile?: string;
  observerCursorDirectory?: string;
  fabricJournal: FabricActionJournalPort;
  fabricDirectSteer: FabricDirectSteerPort;
  clock?: () => number;
}>;

export type ProductionHerdrIntegration = {
  boundary: HerdrCliBoundary;
  adapter: HerdrAdapter;
  directSteer: DirectSteerService;
};

export async function createProductionHerdrIntegration(
  options: ProductionHerdrIntegrationOptions,
): Promise<ProductionHerdrIntegration> {
  if (!Number.isSafeInteger(options.expectedProtocol) || options.expectedProtocol < 1) {
    throw new TypeError("Herdr expected protocol is invalid");
  }
  parseIdentifier<"ProjectId">(options.projectId, "productionHerdr.projectId");
  parseIdentifier<"ProjectSessionId">(options.projectSessionId, "productionHerdr.projectSessionId");
  await verifyConfiguredExecutable(options.executable, "Herdr executable");
  await verifyPinnedExecutable(options.consoleExecutable, options.consoleExecutableDigest, "Console executable");
  const observerConfiguration = [
    options.observerExecutable,
    options.observerExecutableDigest,
    options.observerSocketPath,
    options.observerCapabilityFile,
    options.observerCursorDirectory,
  ];
  const observerEnabled = observerConfiguration.some((value) => value !== undefined);
  if (observerEnabled && observerConfiguration.some((value) => value === undefined)) {
    throw new TypeError("Herdr observer executable, digest, socket, capability and cursor directory must be configured together");
  }
  if (
    options.observerExecutable !== undefined && options.observerExecutableDigest !== undefined &&
    options.observerSocketPath !== undefined && options.observerCapabilityFile !== undefined &&
    options.observerCursorDirectory !== undefined
  ) {
    await verifyPinnedExecutable(options.observerExecutable, options.observerExecutableDigest, "observer executable");
    await verifyCanonicalSocket(options.observerSocketPath, "Fabric observer socket");
    await verifyPrivateRegularFile(options.observerCapabilityFile, "Fabric observer capability file");
    await verifyPrivateDirectory(options.observerCursorDirectory, "Fabric observer cursor directory");
  }
  await verifyCanonicalDirectory(options.stateDirectory, "Herdr integration state directory");
  await verifyCanonicalDirectory(options.canonicalProjectRoot, "Herdr project root");

  const processBoundary = new SealedHerdrCommandPort(options.executable);

  const effectJournal = new HerdrEffectEvidenceJournal({ stateDirectory: options.stateDirectory });
  const observerBoundaryOptions =
    options.observerExecutable !== undefined && options.observerSocketPath !== undefined &&
    options.observerCapabilityFile !== undefined && options.observerCursorDirectory !== undefined
      ? {
          observerExecutable: options.observerExecutable,
          observerSocketPath: options.observerSocketPath,
          observerCapabilityFile: options.observerCapabilityFile,
          observerCursorDirectory: options.observerCursorDirectory,
        }
      : {};
  const boundaryOptions = {
    executable: options.executable,
    expectedProtocol: options.expectedProtocol,
    projectId: options.projectId,
    projectSessionId: options.projectSessionId,
    canonicalProjectRoot: options.canonicalProjectRoot,
    consoleExecutable: options.consoleExecutable,
    process: processBoundary,
    effectJournal,
    verifyExecutable: async (path: string) => {
      if (path === options.consoleExecutable) {
        await verifyPinnedExecutable(path, options.consoleExecutableDigest, "Console executable");
        return;
      }
      if (
        path === options.observerExecutable && options.observerExecutableDigest !== undefined &&
        options.observerSocketPath !== undefined && options.observerCapabilityFile !== undefined &&
        options.observerCursorDirectory !== undefined
      ) {
        await verifyPinnedExecutable(path, options.observerExecutableDigest, "observer executable");
        await verifyCanonicalSocket(options.observerSocketPath, "Fabric observer socket");
        await verifyPrivateRegularFile(options.observerCapabilityFile, "Fabric observer capability file");
        await verifyPrivateDirectory(options.observerCursorDirectory, "Fabric observer cursor directory");
        return;
      }
      throw new TypeError("Herdr attempted to launch an unapproved executable");
    },
    ...observerBoundaryOptions,
    ...(options.clock === undefined ? {} : { clock: options.clock }),
  };
  const boundary = new HerdrCliBoundary(boundaryOptions);
  await boundary.probe();
  const adapter = new HerdrAdapter({
    journal: options.fabricJournal,
    control: boundary,
    presence: boundary,
  });
  const directSteer = new DirectSteerService({ fabric: options.fabricDirectSteer, adapter });
  return { boundary, adapter, directSteer };
}

class SealedHerdrCommandPort implements HerdrCommandPort {
  readonly #executable: string;

  constructor(executable: string) {
    this.#executable = executable;
  }

  async run(request: HerdrCommandRequest): Promise<Buffer> {
    if (!isAbsolute(request.executable) || request.executable.includes("\0")) {
      throw new TypeError("Herdr executable path is invalid");
    }
    if (request.executable !== this.#executable) throw new TypeError("Herdr process target changed");
    await verifyConfiguredExecutable(this.#executable, "Herdr executable");
    if (!Number.isSafeInteger(request.timeoutMs) || request.timeoutMs < 1 || request.timeoutMs > 30_000) {
      throw new TypeError("Herdr command deadline is outside the trusted bound");
    }
    if (!Number.isSafeInteger(request.maximumOutputBytes) || request.maximumOutputBytes < 1 || request.maximumOutputBytes > 1_048_576) {
      throw new TypeError("Herdr command output bound is invalid");
    }
    return new Promise((resolveOutput, rejectOutput) => {
      execFile(request.executable, [...request.arguments], {
        cwd: "/",
        encoding: null,
        env: fixedHerdrEnvironment(),
        maxBuffer: request.maximumOutputBytes,
        timeout: request.timeoutMs,
        windowsHide: true,
        shell: false,
      }, (error, stdout) => {
        if (error === null) {
          resolveOutput(Buffer.isBuffer(stdout) ? stdout : Buffer.from(stdout));
          return;
        }
        if (error.killed) {
          rejectOutput(new TypeError("Herdr command exceeded its deadline"));
          return;
        }
        if (error.code === "ERR_CHILD_PROCESS_STDIO_MAXBUFFER") {
          rejectOutput(new TypeError("Herdr command exceeded its output bound"));
          return;
        }
        rejectOutput(new TypeError("Herdr command is unavailable"));
      });
    });
  }
}

async function verifyConfiguredExecutable(path: string, label: string): Promise<void> {
  if (!isAbsolute(path) || path.includes("\0")) throw new TypeError(`${label} must be an absolute path`);
  const canonical = await realpath(path).catch(() => null);
  if (canonical === null) throw new TypeError(`${label} is missing or does not resolve`);
  const info = await lstat(canonical);
  if (!info.isFile() || (info.mode & 0o111) === 0) throw new TypeError(`${label} does not resolve to an executable regular file`);
  const currentUid = process.getuid?.();
  if (
    (currentUid !== undefined && info.uid !== currentUid && info.uid !== 0) ||
    (info.mode & 0o022) !== 0
  ) throw new TypeError(`${label} does not resolve to an owner-controlled executable`);
}

async function verifyPinnedExecutable(path: string, expectedDigest: string, label: string): Promise<void> {
  await verifyConfiguredExecutable(path, label);
  const expected = parseSha256Digest(expectedDigest, `${label}.expectedDigest`);
  const actual = parseSha256Digest(`sha256:${createHash("sha256").update(await readFile(path)).digest("hex")}`, `${label}.actualDigest`);
  if (actual !== expected) throw new TypeError(`${label} digest changed`);
}

async function verifyCanonicalDirectory(path: string, label: string): Promise<void> {
  if (!isAbsolute(path) || path.includes("\0")) throw new TypeError(`${label} must be an absolute path`);
  const canonical = await realpath(path).catch(() => null);
  if (canonical === null || canonical !== path) throw new TypeError(`${label} is missing or non-canonical`);
  if (!(await lstat(path)).isDirectory()) throw new TypeError(`${label} is not a directory`);
}

async function verifyCanonicalSocket(path: string, label: string): Promise<void> {
  if (!isAbsolute(path) || path.includes("\0")) throw new TypeError(`${label} must be an absolute path`);
  const canonical = await realpath(path).catch(() => null);
  if (canonical === null || canonical !== path || !(await lstat(path)).isSocket()) {
    throw new TypeError(`${label} is missing, non-canonical or not a socket`);
  }
}

async function verifyPrivateDirectory(path: string, label: string): Promise<void> {
  await verifyCanonicalDirectory(path, label);
  if (((await lstat(path)).mode & 0o077) !== 0) throw new TypeError(`${label} is not private`);
}

async function verifyPrivateRegularFile(path: string, label: string): Promise<void> {
  if (!isAbsolute(path) || path.includes("\0")) throw new TypeError(`${label} must be an absolute path`);
  const canonical = await realpath(path).catch(() => null);
  if (canonical === null || canonical !== path) throw new TypeError(`${label} is missing or non-canonical`);
  const info = await lstat(path);
  if (!info.isFile() || info.size < 1 || info.size > 64 * 1024 || (info.mode & 0o077) !== 0) {
    throw new TypeError(`${label} is not a bounded private regular file`);
  }
}

function fixedHerdrEnvironment(): NodeJS.ProcessEnv {
  const environment: NodeJS.ProcessEnv = {
    HOME: process.env.HOME ?? "/",
    LANG: "C.UTF-8",
    LC_ALL: "C.UTF-8",
    NO_COLOR: "1",
    PATH: "/opt/homebrew/bin:/usr/bin:/bin",
  };
  for (const name of ["XDG_RUNTIME_DIR"] as const) {
    const value = process.env[name];
    if (typeof value === "string" && isAbsolute(value)) environment[name] = value;
  }
  return environment;
}
