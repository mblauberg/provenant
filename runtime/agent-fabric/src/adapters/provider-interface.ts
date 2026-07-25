import { spawn } from "node:child_process";
import { mkdtemp, rm } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";

import { FabricError } from "../errors.js";

type ProbeResult = { stdout: string; stderr: string; exitCode: number };
export type ProviderProbeRunner = (input: {
  executable: string;
  args: string[];
  cwd?: string;
  stdin?: string;
  closeOnFirstLine?: boolean;
  timeoutMs: number;
}) => Promise<ProbeResult>;

const MAX_OUTPUT = 1024 * 1024;
const PROBE_TIMEOUT_MS = 15_000;
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

const runProbe: ProviderProbeRunner = async (input) => await new Promise((resolve, reject) => {
  const child = spawn(input.executable, input.args, {
    ...(input.cwd === undefined ? {} : { cwd: input.cwd }),
    env: {
      PATH: process.env.PATH ?? "/usr/bin:/bin",
      ...(process.env.HOME === undefined ? {} : { HOME: process.env.HOME }),
      ...(process.env.CODEX_HOME === undefined ? {} : { CODEX_HOME: process.env.CODEX_HOME }),
    },
    shell: false,
    stdio: ["pipe", "pipe", "pipe"],
  });
  let stdout = "";
  let stderr = "";
  let settled = false;
  const finish = (error?: Error, result?: ProbeResult): void => {
    if (settled) return;
    settled = true;
    clearTimeout(timer);
    if (error !== undefined) reject(error);
    else if (result !== undefined) resolve(result);
  };
  const append = (current: string, chunk: Buffer): string => {
    const next = current + chunk.toString();
    if (Buffer.byteLength(next) > MAX_OUTPUT) throw new Error("provider interface probe exceeded output limit");
    return next;
  };
  child.stdout.on("data", (chunk: Buffer) => {
    try {
      stdout = append(stdout, chunk);
      if (input.closeOnFirstLine === true && stdout.includes("\n")) {
        child.kill("SIGTERM");
        finish(undefined, { stdout, stderr, exitCode: 0 });
      }
    } catch (error: unknown) { child.kill("SIGKILL"); finish(error as Error); }
  });
  child.stderr.on("data", (chunk: Buffer) => {
    try { stderr = append(stderr, chunk); } catch (error: unknown) { child.kill("SIGKILL"); finish(error as Error); }
  });
  child.once("error", (error) => finish(error));
  child.once("close", (code) => finish(undefined, { stdout, stderr, exitCode: code ?? -1 }));
  const timer = setTimeout(() => {
    child.kill("SIGKILL");
    finish(new Error("provider interface probe timed out"));
  }, input.timeoutMs);
  if (input.closeOnFirstLine === true) child.stdin.write(input.stdin);
  else child.stdin.end(input.stdin);
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
    if (result.exitCode !== 0) {
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
    let result: ProbeResult;
    let help: ProbeResult | undefined;
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
    if (result.exitCode !== 0 || (help !== undefined && help.exitCode !== 0)) {
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
  if (help.exitCode !== 0 || version.exitCode !== 0) {
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
