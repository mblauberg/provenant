import { spawn } from "node:child_process";
import { mkdtemp, rm } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";

import { FabricError } from "../errors.js";

export type ProviderProbeResult = {
  stdout: string;
  stderr: string;
  exitCode: number | null;
  signal?: NodeJS.Signals | null;
};
export type ProviderProbeRunner = (input: {
  executable: string;
  args: string[];
  cwd?: string;
  stdin?: string;
  closeOnFirstLine?: boolean;
  timeoutMs: number;
}) => Promise<ProviderProbeResult>;

const MAX_OUTPUT = 1024 * 1024;
const PROBE_TIMEOUT_MS = 15_000;
const TERMINATION_GRACE_MS = 250;
const DRAIN_GRACE_MS = 250;
export const ADAPTER_INTERFACE_PROBE_INCOMPLETE = "ADAPTER_INTERFACE_PROBE_INCOMPLETE" as const;

type ProbeFailureCode = "ADAPTER_INTERFACE_MISMATCH" | typeof ADAPTER_INTERFACE_PROBE_INCOMPLETE;

function probeFailure(adapterId: string, code: ProbeFailureCode, cause: unknown): FabricError {
  return new FabricError(
    code,
    `provider non-answer interface probe failed: ${adapterId}`,
    { cause },
  );
}

async function observeProbe<T>(
  adapterId: string,
  operation: () => Promise<T>,
): Promise<T> {
  try {
    return await operation();
  } catch (error: unknown) {
    throw probeFailure(adapterId, ADAPTER_INTERFACE_PROBE_INCOMPLETE, error);
  }
}

function parseProbeResponse(adapterId: string, line: string | undefined): unknown {
  if (line === undefined) return undefined;
  try {
    return JSON.parse(line);
  } catch (error: unknown) {
    throw probeFailure(adapterId, "ADAPTER_INTERFACE_MISMATCH", error);
  }
}

function responseError(response: unknown): unknown {
  if (typeof response !== "object" || response === null) return undefined;
  const error = Reflect.get(response, "error");
  return typeof error === "object" && error !== null ? error : undefined;
}

function isAuthenticationError(error: unknown): boolean {
  if (typeof error !== "object" || error === null) return false;
  const message = Reflect.get(error, "message");
  return typeof message === "string" &&
    /\b(?:(?:un)?auth(?:enticated|entication|orized|orization)?|credential|log(?:ged|ging)?[ -]?in)\b/iu.test(message);
}

function exitedAbnormally(result: ProviderProbeResult): boolean {
  return result.exitCode !== 0 || result.signal != null;
}

type ProbeTerminationCause = "first-line" | "timeout" | "failure";
type ShimProviderExit = Readonly<{
  code: number | null;
  signal: NodeJS.Signals | null;
  terminationCauseAtExit: ProbeTerminationCause | "none";
}>;

type ShimErrorPayload = Readonly<{ message: string; code?: string }>;

type ShimMessage =
  | { type: "provider-exit"; exit: ShimProviderExit }
  | { type: "error"; error: ShimErrorPayload }
  | { type: "custody-error"; error: ShimErrorPayload }
  | { type: "finalising" }
  | { type: "finalised" };

const CUSTODY_DEADLINE_MS = TERMINATION_GRACE_MS + (DRAIN_GRACE_MS * 2) + 500;

/*
 * This is deliberately a per-probe process, not a reusable supervisor. Its
 * process group is the identity-bearing custody anchor: the parent never
 * signals a provider PID or PGID, and the shim exits with the owned group.
 */
const PROBE_SHIM_SOURCE = String.raw`
(() => {
  const { spawn } = require("node:child_process");
  const TERMINATION_GRACE_MS = 250;
  const DRAIN_GRACE_MS = 250;
  const isUnix = process.platform !== "win32";
  const forceDirectChild = process.env.AGENT_FABRIC_PROVIDER_PROBE_FORCE_DIRECT_CHILD === "1";
  const forceSignalFailure = process.env.AGENT_FABRIC_PROVIDER_PROBE_FORCE_SIGNAL_ERROR === "1";
  const ownsUnixProcessGroup = isUnix && !forceDirectChild;
  const suppressExitObservation = process.env.AGENT_FABRIC_PROVIDER_PROBE_SUPPRESS_EXIT_OBSERVATION === "1";
  let provider;
  let providerExit;
  let terminationCause;
  let terminationStarted = false;
  let finalisationStarted = false;
  let stdinEnded = false;
  let watchdog;
  let finalKillWaitingForProvider = false;
  let finalKillObservationTimer;
  let ownedGroupKillSent = false;
  const pendingStdin = [];

  const serializeError = (error) => {
    const payload = { message: error instanceof Error ? error.message : String(error) };
    if (error && typeof error.code === "string") payload.code = error.code;
    return payload;
  };

  const send = (message, callback) => {
    if (typeof process.send !== "function") {
      if (callback) callback();
      return;
    }
    try {
      process.send(message, undefined, undefined, callback);
    } catch (error) {
      if (callback) callback(error);
    }
  };

  const sendProviderExit = () => {
    if (providerExit !== undefined) send({ type: "provider-exit", exit: providerExit });
  };

  const killOwnedGroupAfterProviderExit = () => {
    if (ownedGroupKillSent) return;
    if (finalKillObservationTimer !== undefined) clearTimeout(finalKillObservationTimer);
    finalKillObservationTimer = undefined;
    if (providerExit === undefined) {
      reportCustodyFailure({ message: "provider exit was not observed before owned group KILL" });
      return;
    }
    ownedGroupKillSent = true;
    const outcome = signalOwned("SIGKILL");
    if (outcome !== "sent" && outcome !== "gone") {
      reportCustodyFailure(outcome.failure);
    } else if (outcome === "gone") {
      announceFinalisedThen(() => process.exit(0));
    }
  };

  const snapshotProviderExit = (code, signal) => {
    if (providerExit !== undefined) return;
    providerExit = {
      code,
      signal,
      terminationCauseAtExit: terminationCause === undefined ? "none" : terminationCause,
    };
    sendProviderExit();
    beginCleanup();
    if (finalKillWaitingForProvider) {
      finalKillWaitingForProvider = false;
      killOwnedGroupAfterProviderExit();
    }
  };

  const signalOwned = (signal) => {
    try {
      if (forceSignalFailure) return { failure: { message: "synthetic custody signal failure", code: "EPERM" } };
      if (ownsUnixProcessGroup) {
        process.kill(-process.pid, signal);
        return "sent";
      }
      // Windows has no group identity in this shim; direct-child cleanup is
      // bounded and makes no descendant-custody claim.
      if (provider !== undefined && provider.exitCode === null && provider.signalCode === null) {
        return provider.kill(signal) ? "sent" : "gone";
      }
      return "gone";
    } catch (error) {
      if (error && error.code === "ESRCH") return "gone";
      return { failure: serializeError(error) };
    }
  };

  const signalProviderDirect = (signal) => {
    try {
      if (forceSignalFailure) return { failure: { message: "synthetic custody signal failure", code: "EPERM" } };
      if (provider !== undefined && provider.exitCode === null && provider.signalCode === null) {
        return provider.kill(signal) ? "sent" : "gone";
      }
      return "gone";
    } catch (error) {
      if (error && error.code === "ESRCH") return "gone";
      return { failure: serializeError(error) };
    }
  };

  const announceFinalisedThen = (action) => {
    if (finalisationStarted) {
      action();
      return;
    }
    finalisationStarted = true;
    let called = false;
    const once = () => {
      if (called) return;
      called = true;
      action();
    };
    send({ type: "finalised" }, once);
    setImmediate(once);
  };

  const announceFinalisingThen = (action) => {
    if (finalisationStarted) {
      action();
      return;
    }
    finalisationStarted = true;
    let called = false;
    const once = () => {
      if (called) return;
      called = true;
      action();
    };
    send({ type: "finalising" }, once);
    setImmediate(once);
  };

  const exitWithoutFurtherSignal = (code) => {
    if (watchdog !== undefined) clearTimeout(watchdog);
    announceFinalisedThen(() => process.exit(code));
  };

  const reportCustodyFailure = (error) => {
    send({ type: "custody-error", error });
    exitWithoutFurtherSignal(1);
  };

  const finalKill = () => {
    if (!ownsUnixProcessGroup) {
      announceFinalisedThen(() => {
        const outcome = signalOwned("SIGKILL");
        if (outcome !== "sent" && outcome !== "gone") {
          reportCustodyFailure(outcome.failure);
        } else if (outcome === "gone") {
          if (providerExit === undefined) {
            reportCustodyFailure({ message: "provider direct-child exit was not observed after bounded KILL" });
          } else {
            process.exit(0);
          }
        } else {
          setTimeout(() => {
            if (providerExit === undefined) {
              reportCustodyFailure({ message: "provider direct-child exit was not observed after bounded KILL" });
            } else {
              process.exit(0);
            }
          }, DRAIN_GRACE_MS);
        }
      });
      return;
    }
    if (providerExit !== undefined) {
      announceFinalisingThen(killOwnedGroupAfterProviderExit);
      return;
    }
    finalKillWaitingForProvider = true;
    announceFinalisingThen(() => {
      const outcome = signalProviderDirect("SIGKILL");
      if (outcome !== "sent" && outcome !== "gone") {
        finalKillWaitingForProvider = false;
        reportCustodyFailure(outcome.failure);
        return;
      }
      if (providerExit !== undefined) {
        finalKillWaitingForProvider = false;
        killOwnedGroupAfterProviderExit();
        return;
      }
      finalKillObservationTimer = setTimeout(() => {
        finalKillObservationTimer = undefined;
        if (providerExit === undefined) {
          finalKillWaitingForProvider = false;
          reportCustodyFailure({ message: "provider exit was not observed after direct KILL" });
        } else {
          finalKillWaitingForProvider = false;
          killOwnedGroupAfterProviderExit();
        }
      }, DRAIN_GRACE_MS);
    });
  };

  function beginCleanup() {
    if (terminationStarted) return;
    terminationStarted = true;
    if (watchdog !== undefined) clearTimeout(watchdog);
    const outcome = signalOwned("SIGTERM");
    if (outcome !== "sent" && outcome !== "gone") {
      reportCustodyFailure(outcome.failure);
      return;
    }
    if (outcome === "gone") {
      const directOutcome = signalProviderDirect("SIGTERM");
      if (directOutcome !== "sent" && directOutcome !== "gone") {
        reportCustodyFailure(directOutcome.failure);
        return;
      }
      if (directOutcome === "sent") {
        setTimeout(finalKill, TERMINATION_GRACE_MS + DRAIN_GRACE_MS);
        return;
      }
      exitWithoutFurtherSignal(0);
      return;
    }
    setTimeout(finalKill, TERMINATION_GRACE_MS + DRAIN_GRACE_MS);
  }

  const requestTermination = (cause, error) => {
    if (error) send({ type: "error", error: serializeError(error) });
    if (providerExit !== undefined || terminationStarted) return;
    terminationCause = cause;
    if (provider === undefined) {
      beginCleanup();
      return;
    }
    beginCleanup();
  };

  const startProvider = (input) => {
    if (provider !== undefined) return;
    watchdog = setTimeout(() => requestTermination("timeout"), Math.max(1_000, Number(input.timeoutMs) + TERMINATION_GRACE_MS));
    provider = spawn(input.executable, input.args, {
      ...(input.cwd === undefined ? {} : { cwd: input.cwd }),
      env: {
        PATH: process.env.PATH || "/usr/bin:/bin",
        ...(process.env.HOME === undefined ? {} : { HOME: process.env.HOME }),
        ...(process.env.CODEX_HOME === undefined ? {} : { CODEX_HOME: process.env.CODEX_HOME }),
      },
      shell: false,
      stdio: ["pipe", "pipe", "pipe"],
    });
    provider.stdout.on("data", (chunk) => process.stdout.write(chunk));
    provider.stderr.on("data", (chunk) => process.stderr.write(chunk));
    provider.stdin.on("error", (error) => requestTermination("failure", error));
    provider.once("error", (error) => {
      if (providerExit === undefined) {
        send({ type: "error", error: serializeError(error) });
        if (terminationCause === undefined) terminationCause = "failure";
        beginCleanup();
      }
    });
    provider.once("exit", (code, signal) => {
      if (!suppressExitObservation) snapshotProviderExit(code, signal);
    });
    for (const chunk of pendingStdin) provider.stdin.write(chunk);
    pendingStdin.length = 0;
    if (stdinEnded) provider.stdin.end();
  };

  process.on("SIGTERM", () => undefined);
  process.stdin.on("data", (chunk) => {
    if (provider === undefined) pendingStdin.push(chunk);
    else provider.stdin.write(chunk);
  });
  process.stdin.on("end", () => {
    stdinEnded = true;
    if (provider !== undefined) provider.stdin.end();
  });
  process.stdin.on("error", (error) => requestTermination("failure", error));
  process.on("message", (message) => {
    if (!message || typeof message !== "object") return;
    if (message.type === "start") {
      startProvider(message.input);
    } else if (message.type === "terminate") {
      requestTermination(message.cause);
    }
  });
  process.on("disconnect", () => {
    if (!finalisationStarted && !terminationStarted) {
      requestTermination("failure", new Error("provider probe parent IPC disconnected"));
    }
  });
})();
`;

function shimError(payload: ShimErrorPayload): Error {
  const error = new Error(payload.message) as Error & { code?: string };
  if (payload.code !== undefined) error.code = payload.code;
  return error;
}

export const runProbe: ProviderProbeRunner = async (input) => await new Promise((resolve, reject) => {
  const child = spawn(process.execPath, ["-e", PROBE_SHIM_SOURCE], {
    detached: true,
    env: {
      PATH: process.env.PATH ?? "/usr/bin:/bin",
      ...(process.env.HOME === undefined ? {} : { HOME: process.env.HOME }),
      ...(process.env.CODEX_HOME === undefined ? {} : { CODEX_HOME: process.env.CODEX_HOME }),
      ...(process.env.AGENT_FABRIC_PROVIDER_PROBE_FORCE_DIRECT_CHILD === "1"
        ? { AGENT_FABRIC_PROVIDER_PROBE_FORCE_DIRECT_CHILD: "1" }
        : {}),
      ...(process.env.AGENT_FABRIC_PROVIDER_PROBE_FORCE_SIGNAL_ERROR === "1"
        ? { AGENT_FABRIC_PROVIDER_PROBE_FORCE_SIGNAL_ERROR: "1" }
        : {}),
      ...(process.env.AGENT_FABRIC_PROVIDER_PROBE_SUPPRESS_EXIT_OBSERVATION === "1"
        ? { AGENT_FABRIC_PROVIDER_PROBE_SUPPRESS_EXIT_OBSERVATION: "1" }
        : {}),
    },
    shell: false,
    stdio: ["pipe", "pipe", "pipe", "ipc"],
  });
  if (child.stdin === null || child.stdout === null || child.stderr === null) {
    reject(new Error("provider interface probe shim did not expose stdio"));
    return;
  }
  const childStdin = child.stdin;
  const childStdout = child.stdout;
  const childStderr = child.stderr;
  let stdout = "";
  let stderr = "";
  let settled = false;
  let firstFailure: unknown;
  let providerExit: ShimProviderExit | undefined;
  let shimFinalising = false;
  let shimFinalised = false;
  let shimExitObserved = false;
  let streamsComplete = false;
  let drainTimer: ReturnType<typeof setTimeout> | undefined;
  let custodyDeadlineTimer: ReturnType<typeof setTimeout> | undefined;
  let wallClockTimer: ReturnType<typeof setTimeout> | undefined;
  let terminationRequestSent = false;
  const append = (current: string, chunk: Buffer): string => {
    const next = current + chunk.toString();
    if (Buffer.byteLength(next) > MAX_OUTPUT) throw new Error("provider interface probe exceeded output limit");
    return next;
  };
  const rememberFailure = (error: unknown): void => {
    if (error !== undefined && error !== null && firstFailure === undefined) firstFailure = error;
  };
  const cleanup = (): void => {
    if (wallClockTimer !== undefined) clearTimeout(wallClockTimer);
    if (drainTimer !== undefined) clearTimeout(drainTimer);
    if (custodyDeadlineTimer !== undefined) clearTimeout(custodyDeadlineTimer);
    childStdout.removeListener("data", onStdout);
    childStderr.removeListener("data", onStderr);
    childStdin.removeListener("error", onStdinError);
    child.removeListener("message", onMessage);
    child.removeListener("error", onError);
    child.removeListener("exit", onExit);
    child.removeListener("close", onClose);
  };
  const destroyStreams = (): void => {
    childStdout.removeListener("data", onStdout);
    childStderr.removeListener("data", onStderr);
    childStdin.destroy();
    childStdout.destroy();
    childStderr.destroy();
    streamsComplete = true;
  };
  const rejectForCustodyDeadline = (): void => {
    if (settled) return;
    destroyStreams();
    try {
      if (child.connected) child.disconnect();
    } catch {
      // The shim may already have exited with its final custody result.
    }
    settled = true;
    cleanup();
    reject(firstFailure ?? new Error("provider interface probe custody deadline expired"));
  };
  const startCustodyDeadline = (): void => {
    if (settled || custodyDeadlineTimer !== undefined) return;
    custodyDeadlineTimer = setTimeout(() => {
      custodyDeadlineTimer = undefined;
      rejectForCustodyDeadline();
    }, CUSTODY_DEADLINE_MS);
  };
  const startDrain = (): void => {
    if (settled || streamsComplete || drainTimer !== undefined) return;
    // Escaped descendants are out of custody scope; retained local pipes are
    // destroyed after this bounded drain window.
    drainTimer = setTimeout(() => {
      drainTimer = undefined;
      if (streamsComplete) return;
      destroyStreams();
      trySettle();
    }, DRAIN_GRACE_MS);
  };
  const trySettle = (): void => {
    if (settled || providerExit === undefined || !shimFinalised || !shimExitObserved || !streamsComplete) return;
    settled = true;
    cleanup();
    if (firstFailure !== undefined) {
      reject(firstFailure);
      return;
    }
    const runnerInitiatedTermination = providerExit.terminationCauseAtExit === "first-line";
    resolve({
      stdout,
      stderr,
      exitCode: runnerInitiatedTermination ? 0 : providerExit.code,
      signal: runnerInitiatedTermination ? null : providerExit.signal,
    });
  };
  const requestTermination = (cause: ProbeTerminationCause, error?: unknown): void => {
    rememberFailure(error);
    if (settled || terminationRequestSent) return;
    terminationRequestSent = true;
    startCustodyDeadline();
    try {
      child.send({ type: "terminate", cause }, undefined, undefined, (sendError: Error | null) => {
        if (sendError !== null) rememberFailure(sendError);
      });
    } catch (error: unknown) {
      rememberFailure(error);
    }
  };
  const onStdout = (chunk: Buffer): void => {
    if (settled) return;
    if (firstFailure !== undefined) return;
    try {
      stdout = append(stdout, chunk);
      if (input.closeOnFirstLine === true && !terminationRequestSent && stdout.includes("\n")) {
        requestTermination("first-line");
      }
    } catch (error: unknown) {
      requestTermination("failure", error);
    }
  };
  const onStderr = (chunk: Buffer): void => {
    if (settled) return;
    if (firstFailure !== undefined) return;
    try { stderr = append(stderr, chunk); }
    catch (error: unknown) { requestTermination("failure", error); }
  };
  const onStdinError = (error: Error): void => requestTermination("failure", error);
  const onMessage = (message: ShimMessage): void => {
    if (message.type === "provider-exit") {
      if (providerExit === undefined) providerExit = message.exit;
      if (wallClockTimer !== undefined) clearTimeout(wallClockTimer);
      trySettle();
    } else if (message.type === "error" || message.type === "custody-error") {
      rememberFailure(shimError(message.error));
      startCustodyDeadline();
    } else if (message.type === "finalising") {
      shimFinalising = true;
      trySettle();
    } else if (message.type === "finalised") {
      shimFinalised = true;
      trySettle();
    }
  };
  const onError = (error: Error): void => {
    rememberFailure(error);
    startCustodyDeadline();
  };
  const onExit = (): void => {
    shimExitObserved = true;
    if (shimFinalising) shimFinalised = true;
    if (wallClockTimer !== undefined) clearTimeout(wallClockTimer);
    startCustodyDeadline();
    startDrain();
    trySettle();
  };
  const onClose = (): void => {
    streamsComplete = true;
    if (shimExitObserved && providerExit === undefined && firstFailure !== undefined) {
      rejectForCustodyDeadline();
      return;
    }
    trySettle();
  };
  childStdout.on("data", onStdout);
  childStderr.on("data", onStderr);
  childStdin.on("error", onStdinError);
  child.on("message", onMessage);
  child.once("error", onError);
  child.once("exit", onExit);
  child.once("close", onClose);
  wallClockTimer = setTimeout(() => {
    requestTermination("timeout", new Error("provider interface probe timed out"));
  }, input.timeoutMs);
  try {
    child.send({
      type: "start",
      input: {
        executable: input.executable,
        args: input.args,
        ...(input.cwd === undefined ? {} : { cwd: input.cwd }),
        timeoutMs: input.timeoutMs,
      },
    }, undefined, undefined, (sendError: Error | null) => {
      if (sendError !== null) {
        rememberFailure(sendError);
        startCustodyDeadline();
      }
    });
    childStdin.end(input.stdin);
  } catch (error: unknown) {
    requestTermination("failure", error);
  }
});

const REQUIRED_FLAGS: Record<string, string[]> = {
  "claude-agent-sdk": ["--print", "--output-format"],
  agy: ["--print", "--model", "--mode", "--log-file"],
  "cursor-agent": ["--print", "--output-format", "--model"],
};

function hasExactOption(helpText: string, option: string): boolean {
  const escaped = option.replace(/[.*+?^${}()|[\]\\]/gu, "\\$&");
  return new RegExp(`(?:^|[\\s,|])${escaped}(?=$|[\\s,|=])`, "mu").test(helpText);
}

/** Runs only bounded version/help or initialize operations; never a model turn. */
export async function probeProviderInterface(
  input: { adapterId: string; executable: string },
  runner: ProviderProbeRunner = runProbe,
): Promise<{ adapterId: string; conformant: true; probe: string; version: string }> {
  if (input.adapterId === "codex-app-server") {
    const request = `${JSON.stringify({
      id: 1,
      method: "initialize",
      params: { clientInfo: { name: "agent-fabric-probe", version: "1" }, capabilities: {} },
    })}\n`;
    const result = await observeProbe(input.adapterId, async () =>
      await runner({ executable: input.executable, args: ["app-server"], stdin: request, closeOnFirstLine: true, timeoutMs: PROBE_TIMEOUT_MS }));
    if (exitedAbnormally(result)) {
      throw probeFailure(input.adapterId, ADAPTER_INTERFACE_PROBE_INCOMPLETE, new Error("Codex initialize probe exited non-zero"));
    }
    const line = result.stdout.split(/\r?\n/u).find((item) => item.trim().length > 0);
    const response = parseProbeResponse(input.adapterId, line);
    const validEnvelope = typeof response === "object" && response !== null && Reflect.get(response, "id") === 1;
    const error = responseError(response);
    if (error !== undefined) {
      throw probeFailure(
        input.adapterId,
        validEnvelope && isAuthenticationError(error) ? ADAPTER_INTERFACE_PROBE_INCOMPLETE : "ADAPTER_INTERFACE_MISMATCH",
        error,
      );
    }
    const initialized = typeof response === "object" && response !== null ? Reflect.get(response, "result") : undefined;
    if (!validEnvelope || typeof initialized !== "object" || initialized === null) {
      throw probeFailure(input.adapterId, "ADAPTER_INTERFACE_MISMATCH", new Error("Codex initialize response is invalid"));
    }
    const userAgent = Reflect.get(initialized, "userAgent");
    return { adapterId: input.adapterId, conformant: true, probe: "app-server-initialize", version: typeof userAgent === "string" ? userAgent : "observed-via-initialize" };
  }
  if (input.adapterId === "kiro-acp" || input.adapterId === "opencode-acp") {
    const request = `${JSON.stringify({
      jsonrpc: "2.0",
      id: 1,
      method: "initialize",
      params: {
        protocolVersion: 1,
        clientCapabilities: { fs: { readTextFile: false, writeTextFile: false }, terminal: false },
        clientInfo: { name: "agent-fabric-probe", version: "1" },
      },
    })}\n`;
    const kiro = input.adapterId === "kiro-acp";
    let result: ProviderProbeResult;
    let help: ProviderProbeResult | undefined;
    if (kiro) {
      [result, help] = await Promise.all([
        observeProbe(input.adapterId, async () =>
          await runner({ executable: input.executable, args: ["acp", "--agent-engine", "v2"], stdin: request, closeOnFirstLine: true, timeoutMs: PROBE_TIMEOUT_MS })),
        observeProbe(input.adapterId, async () =>
          await runner({ executable: input.executable, args: ["acp", "--help"], timeoutMs: PROBE_TIMEOUT_MS })),
      ]);
    } else {
      const cwd = await observeProbe(input.adapterId, async () =>
        await mkdtemp(join(tmpdir(), "agent-fabric-opencode-probe-")));
      try {
        result = await observeProbe(input.adapterId, async () =>
          await runner({
            executable: input.executable,
            args: ["acp", "--pure", "--cwd", cwd],
            cwd,
            stdin: request,
            closeOnFirstLine: true,
            timeoutMs: PROBE_TIMEOUT_MS,
          }));
      } finally {
        await observeProbe(input.adapterId, async () =>
          await rm(cwd, { recursive: true, force: true }));
      }
    }
    if (exitedAbnormally(result) || (help !== undefined && exitedAbnormally(help))) {
      throw probeFailure(input.adapterId, ADAPTER_INTERFACE_PROBE_INCOMPLETE, new Error(`${input.adapterId} ACP probe exited non-zero`));
    }
    if (kiro && (help === undefined || !hasExactOption(`${help.stdout}\n${help.stderr}`, "--effort"))) {
      throw probeFailure(input.adapterId, ADAPTER_INTERFACE_PROBE_INCOMPLETE, new Error("required headless flags are unavailable"));
    }
    const line = result.stdout.split(/\r?\n/u).find((item) => item.trim().length > 0);
    const response = parseProbeResponse(input.adapterId, line);
    const validEnvelope = typeof response === "object" && response !== null &&
      Reflect.get(response, "jsonrpc") === "2.0" && Reflect.get(response, "id") === 1;
    const error = responseError(response);
    if (error !== undefined) {
      throw probeFailure(
        input.adapterId,
        validEnvelope && isAuthenticationError(error) ? ADAPTER_INTERFACE_PROBE_INCOMPLETE : "ADAPTER_INTERFACE_MISMATCH",
        error,
      );
    }
    const negotiated = typeof response === "object" && response !== null ? Reflect.get(response, "result") : undefined;
    if (!validEnvelope || typeof negotiated !== "object" || negotiated === null || Reflect.get(negotiated, "protocolVersion") !== 1) {
      throw probeFailure(input.adapterId, "ADAPTER_INTERFACE_MISMATCH", new Error(`${input.adapterId} ACP v1 initialize response is invalid`));
    }
    const agentInfo = Reflect.get(negotiated, "agentInfo");
    const version = typeof agentInfo === "object" && agentInfo !== null && typeof Reflect.get(agentInfo, "version") === "string"
      ? Reflect.get(agentInfo, "version") as string
      : "observed-via-initialize";
    return { adapterId: input.adapterId, conformant: true, probe: "acp-v1-initialize", version };
  }
  const flags = REQUIRED_FLAGS[input.adapterId];
  if (flags === undefined) {
    throw probeFailure(input.adapterId, ADAPTER_INTERFACE_PROBE_INCOMPLETE, new Error(`no interface probe is defined for ${input.adapterId}`));
  }
  const [help, version] = await Promise.all([
    observeProbe(input.adapterId, async () =>
      await runner({ executable: input.executable, args: ["--help"], timeoutMs: PROBE_TIMEOUT_MS })),
    observeProbe(input.adapterId, async () =>
      await runner({ executable: input.executable, args: ["--version"], timeoutMs: PROBE_TIMEOUT_MS })),
  ]);
  if (exitedAbnormally(help) || exitedAbnormally(version)) {
    throw probeFailure(input.adapterId, ADAPTER_INTERFACE_PROBE_INCOMPLETE, new Error("provider help/version probe exited non-zero"));
  }
  const helpText = `${help.stdout}\n${help.stderr}`;
  if (flags.some((flag) => !hasExactOption(helpText, flag))) {
    throw probeFailure(input.adapterId, ADAPTER_INTERFACE_PROBE_INCOMPLETE, new Error("required headless flags are unavailable"));
  }
  return {
    adapterId: input.adapterId,
    conformant: true,
    probe: "bounded-help-version",
    version: `${version.stdout}\n${version.stderr}`.trim(),
  };
}
