import {
  buildAgyInvocation,
  buildCursorInvocation,
  buildKiroAcpInvocation,
  type ProviderInvocation,
} from "../adapters/providers/optional/invocations.js";
import { resolveAdapterExecutableCli } from "./adapter-executable.js";

const COMMON_OPTIONS = new Set([
  "--adapter",
  "--agents-home",
  "--product-root",
  "--instance-root",
  "--config",
  "--compatibility",
  "--compatibility-schema",
]);
const MAX_NODE_TIMER_MS = 2_147_483_647;
type AdapterInvocationDependencies = NonNullable<Parameters<typeof resolveAdapterExecutableCli>[1]>;

function parseOptionPairs(arguments_: string[]): Map<string, string> {
  const parsed = new Map<string, string>();
  for (let index = 0; index < arguments_.length; index += 2) {
    const name = arguments_[index] ?? "<missing>";
    const value = arguments_[index + 1];
    if (value === undefined || value.length === 0 || value.startsWith("-")) {
      throw new Error(`adapter invocation requires a value for ${name}`);
    }
    if (parsed.has(name)) {
      throw new Error(`adapter invocation received duplicate option: ${name}`);
    }
    parsed.set(name, value);
  }
  return parsed;
}

function required(parsed: ReadonlyMap<string, string>, provider: string, option: string): string {
  const value = parsed.get(option);
  if (value === undefined || value.trim().length === 0) {
    throw new Error(`adapter invocation for ${provider} requires ${option}`);
  }
  return value;
}

function optionalTimeout(parsed: ReadonlyMap<string, string>, provider: string): number | undefined {
  const value = parsed.get("--timeout-ms");
  if (value === undefined) return undefined;
  const timeoutMs = Number(value);
  if (!Number.isSafeInteger(timeoutMs) || timeoutMs <= 0) {
    throw new Error(`adapter invocation for ${provider} received invalid timeout-ms value: ${value}`);
  }
  if (timeoutMs > MAX_NODE_TIMER_MS) {
    throw new Error(
      `adapter invocation for ${provider} received invalid timeout-ms value: ${value}; maximum is ${String(MAX_NODE_TIMER_MS)}ms`,
    );
  }
  return timeoutMs;
}

function assertAllowedOptions(
  parsed: ReadonlyMap<string, string>,
  provider: string,
  providerOptions: ReadonlySet<string>,
): void {
  for (const option of parsed.keys()) {
    if (!COMMON_OPTIONS.has(option) && !providerOptions.has(option)) {
      throw new Error(`adapter invocation for ${provider} received unknown option: ${option}`);
    }
  }
}

function executableArguments(parsed: ReadonlyMap<string, string>, adapterId: string): string[] {
  const arguments_ = ["--adapter", adapterId];
  for (const option of COMMON_OPTIONS) {
    if (option === "--adapter") continue;
    const value = parsed.get(option);
    if (value !== undefined) arguments_.push(option, value);
  }
  return arguments_;
}

export async function resolveAdapterInvocationCli(
  arguments_: string[],
  dependencies: AdapterInvocationDependencies = {},
): Promise<ProviderInvocation> {
  const parsed = parseOptionPairs(arguments_);
  const provider = parsed.get("--adapter");
  if (provider === undefined) {
    throw new Error("adapter invocation requires --adapter");
  }
  if (provider === "copilot") {
    throw new Error("copilot is not supported; it has no adapter");
  }
  if (provider === "opencode-acp") {
    throw new Error("provider opencode-acp is registered but has no invocation payload");
  }
  if (provider === "pi") {
    throw new Error(
      "provider pi is registered but has no provider conformance policy and is disabled in the adapter compatibility config",
    );
  }
  if (provider !== "agy" && provider !== "cursor" && provider !== "kiro") {
    throw new Error(`unknown provider: ${provider}`);
  }

  if (provider === "kiro") {
    assertAllowedOptions(
      parsed,
      provider,
      new Set(["--model", "--agent-engine", "--cwd", "--timeout-ms"]),
    );
    const agentEngine = required(parsed, provider, "--agent-engine");
    if (agentEngine !== "v2") {
      throw new Error(
        `adapter invocation for ${provider} received invalid agent-engine value: ${agentEngine}`,
      );
    }
    const model = required(parsed, provider, "--model");
    const cwd = parsed.get("--cwd");
    const timeoutMs = optionalTimeout(parsed, provider);
    const executable = await resolveAdapterExecutableCli(
      executableArguments(parsed, "kiro-acp"),
      dependencies,
    );
    return buildKiroAcpInvocation({
      executable,
      agentEngine,
      model,
      ...(cwd === undefined ? {} : { cwd }),
      ...(timeoutMs === undefined ? {} : { timeoutMs }),
    });
  }

  if (provider === "cursor") {
    assertAllowedOptions(
      parsed,
      provider,
      new Set(["--mode", "--model", "--prompt", "--resume-reference", "--cwd", "--timeout-ms"]),
    );
    const mode = required(parsed, provider, "--mode");
    if (mode !== "plan" && mode !== "ask") {
      throw new Error(`adapter invocation for ${provider} received invalid mode value: ${mode}`);
    }
    const model = required(parsed, provider, "--model");
    const prompt = required(parsed, provider, "--prompt");
    const resumeReference = parsed.get("--resume-reference");
    const cwd = parsed.get("--cwd");
    const timeoutMs = optionalTimeout(parsed, provider);
    const executable = await resolveAdapterExecutableCli(
      executableArguments(parsed, "cursor-agent"),
      dependencies,
    );
    return buildCursorInvocation({
      executable,
      mode,
      model,
      prompt,
      ...(resumeReference === undefined ? {} : { resumeReference }),
      ...(cwd === undefined ? {} : { cwd }),
      ...(timeoutMs === undefined ? {} : { timeoutMs }),
    });
  }

  assertAllowedOptions(
    parsed,
    provider,
    new Set(["--mode", "--model", "--prompt", "--resume-reference", "--log-file", "--cwd", "--timeout-ms"]),
  );
  const mode = required(parsed, provider, "--mode");
  if (mode !== "plan" && mode !== "accept-edits") {
    throw new Error(`adapter invocation for ${provider} received invalid mode value: ${mode}`);
  }
  const model = required(parsed, provider, "--model");
  const prompt = required(parsed, provider, "--prompt");
  const resumeReference = parsed.get("--resume-reference");
  const logFile = parsed.get("--log-file");
  const cwd = parsed.get("--cwd");
  const timeoutMs = optionalTimeout(parsed, provider);
  const executable = await resolveAdapterExecutableCli(executableArguments(parsed, "agy"), dependencies);
  return buildAgyInvocation({
    executable,
    mode,
    model,
    prompt,
    ...(resumeReference === undefined ? {} : { resumeReference }),
    ...(logFile === undefined ? {} : { logFile }),
    ...(cwd === undefined ? {} : { cwd }),
    ...(timeoutMs === undefined ? {} : { timeoutMs }),
  });
}
