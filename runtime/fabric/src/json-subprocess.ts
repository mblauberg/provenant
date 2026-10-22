/** Run a helper script that reads one JSON request on stdin and prints one JSON answer. */
import { spawn } from "node:child_process";

export type SubprocessFailure = "exit" | "timeout" | "overflow" | "malformed" | "spawn";

export class SubprocessError extends Error {
  constructor(
    readonly kind: SubprocessFailure,
    message: string,
    readonly stderr = "",
  ) {
    super(message);
  }
}

export interface JsonSubprocessOptions {
  cwd?: string;
  env?: NodeJS.ProcessEnv;
  /** Written to stdin, then stdin is closed. */
  input: unknown;
  signal?: AbortSignal;
  timeoutMs: number;
  /** Cap for stdout and for kept stderr, each in bytes. */
  maxBytes: number;
  /** Message for a failed, timed-out, oversized or malformed run. */
  unavailable: string;
}

/**
 * Spawn `command`, send `input` as JSON, and parse stdout as JSON. The child is
 * killed on timeout, cancellation or output past `maxBytes`. A child that exits
 * without reading its stdin (EPIPE) is an ordinary failure, never an uncaught
 * stream error in the host.
 */
export function runJsonSubprocess<T = unknown>(
  command: string,
  args: string[],
  options: JsonSubprocessOptions,
): Promise<T> {
  return new Promise<T>((resolveResult, rejectResult) => {
    const { signal } = options;
    if (signal?.aborted) return rejectResult(abortReason(signal));
    const child = spawn(command, args, { cwd: options.cwd, env: options.env, stdio: ["pipe", "pipe", "pipe"] });
    const stdout: Buffer[] = [];
    const stderr: Buffer[] = [];
    let stdoutBytes = 0;
    let stderrBytes = 0;
    let failure: SubprocessError | undefined;
    let aborted = false;
    let settled = false;

    const fail = (error: SubprocessError) => {
      failure ??= error;
      child.kill("SIGKILL");
    };
    const onAbort = () => {
      aborted = true;
      child.kill("SIGKILL");
    };
    const timer = setTimeout(() => fail(new SubprocessError("timeout", options.unavailable)), options.timeoutMs);
    signal?.addEventListener("abort", onAbort, { once: true });
    const finish = (settle: () => void) => {
      if (settled) return;
      settled = true;
      clearTimeout(timer);
      signal?.removeEventListener("abort", onAbort);
      settle();
    };

    child.stdout.on("data", (chunk: Buffer) => {
      stdoutBytes += chunk.length;
      if (stdoutBytes > options.maxBytes) return fail(new SubprocessError("overflow", options.unavailable));
      stdout.push(chunk);
    });
    child.stderr.on("data", (chunk: Buffer) => {
      if (stderrBytes >= options.maxBytes) return;
      stderr.push(chunk.subarray(0, options.maxBytes - stderrBytes));
      stderrBytes += stderr[stderr.length - 1]!.length;
    });
    // EPIPE or ECONNRESET: the child closed stdin early. Its exit status decides the outcome.
    child.stdin.on("error", () => undefined);
    child.once("error", (error) => {
      finish(() => rejectResult(new SubprocessError("spawn", error.message)));
    });
    child.once("close", (code) => finish(() => {
      const detail = Buffer.concat(stderr).toString("utf8");
      if (aborted && signal) return rejectResult(abortReason(signal));
      if (failure) return rejectResult(failure);
      if (code !== 0) return rejectResult(new SubprocessError("exit", options.unavailable, detail));
      try {
        resolveResult(JSON.parse(Buffer.concat(stdout).toString("utf8")) as T);
      } catch {
        rejectResult(new SubprocessError("malformed", options.unavailable, detail));
      }
    }));

    try {
      child.stdin.end(JSON.stringify(options.input));
    } catch (error) {
      fail(new SubprocessError("spawn", error instanceof Error ? error.message : String(error)));
    }
  });
}

function abortReason(signal: AbortSignal): unknown {
  return signal.reason ?? new DOMException("This operation was aborted", "AbortError");
}
