import { spawn } from "node:child_process";
import { createRequire } from "node:module";
import { resolve } from "node:path";
import { fileURLToPath } from "node:url";

const packageRoot = fileURLToPath(new URL("../../", import.meta.url));
const sourceCli = fileURLToPath(new URL("../../src/cli/main.ts", import.meta.url));
const tsxLoader = createRequire(import.meta.url).resolve("tsx");

export type CliResult = {
  exitCode: number | null;
  signal: NodeJS.Signals | null;
  stdout: string;
  stderr: string;
};

export async function runSourceCli(
  arguments_: string[],
  options: {
    absoluteLoader?: string;
    cwd?: string;
    environment?: Record<string, string | undefined>;
    detached?: boolean;
  } = {},
): Promise<CliResult> {
  const environment: NodeJS.ProcessEnv = { ...process.env };
  for (const [key, value] of Object.entries(options.environment ?? {})) {
    if (value === undefined) {
      delete environment[key];
    } else {
      environment[key] = value;
    }
  }

  const cwd = options.cwd ?? packageRoot;
  const loader = options.absoluteLoader ?? (resolve(cwd) === packageRoot ? "tsx" : tsxLoader);

  return await new Promise<CliResult>((resolve, reject) => {
    const child = spawn(process.execPath, ["--import", loader, sourceCli, ...arguments_], {
      cwd,
      env: environment,
      detached: options.detached ?? false,
      stdio: ["ignore", "pipe", "pipe"],
    });
    const stdout: Buffer[] = [];
    const stderr: Buffer[] = [];
    const timeout = setTimeout(() => {
      child.kill("SIGKILL");
    }, 10_000);

    child.stdout.on("data", (chunk: Buffer) => stdout.push(chunk));
    child.stderr.on("data", (chunk: Buffer) => stderr.push(chunk));
    child.once("error", (error) => {
      clearTimeout(timeout);
      reject(error);
    });
    child.once("close", (exitCode, signal) => {
      clearTimeout(timeout);
      resolve({
        exitCode,
        signal,
        stdout: Buffer.concat(stdout).toString("utf8"),
        stderr: Buffer.concat(stderr).toString("utf8"),
      });
    });
  });
}

function shellQuote(value: string): string {
  return `'${value.replaceAll("'", `'"'"'`)}'`;
}

export async function runSourceCliWithPty(
  arguments_: string[],
  options: {
    input: string;
    environment?: Record<string, string | undefined>;
  },
): Promise<CliResult> {
  const environment: NodeJS.ProcessEnv = { ...process.env };
  for (const [key, value] of Object.entries(options.environment ?? {})) {
    if (value === undefined) {
      delete environment[key];
    } else {
      environment[key] = value;
    }
  }
  const command = [process.execPath, "--import", "tsx", sourceCli, ...arguments_];
  const scriptArguments = process.platform === "linux"
    ? ["-q", "-e", "-c", command.map(shellQuote).join(" "), "/dev/null"]
    : ["-q", "-e", "/dev/null", ...command];

  return await new Promise<CliResult>((resolve, reject) => {
    const child = spawn("/bin/sh", [
      "-c",
      `input=$1
shift
(sleep 0.25; printf '%s' "$input"; sleep 0.25) | exec "$@"`,
      "agent-fabric-pty-driver",
      options.input,
      "script",
      ...scriptArguments,
    ], {
      cwd: packageRoot,
      env: environment,
      stdio: ["ignore", "pipe", "pipe"],
    });
    const stdout: Buffer[] = [];
    const stderr: Buffer[] = [];
    const timeout = setTimeout(() => {
      child.kill("SIGKILL");
    }, 10_000);

    child.stdout.on("data", (chunk: Buffer) => {
      stdout.push(chunk);
    });
    child.stderr.on("data", (chunk: Buffer) => stderr.push(chunk));
    child.once("error", (error) => {
      clearTimeout(timeout);
      reject(error);
    });
    child.once("close", (exitCode, signal) => {
      clearTimeout(timeout);
      resolve({
        exitCode,
        signal,
        stdout: Buffer.concat(stdout).toString("utf8"),
        stderr: Buffer.concat(stderr).toString("utf8"),
      });
    });
  });
}

export function parseCliJson(result: CliResult): unknown {
  if (result.exitCode !== 0) {
    throw new Error(`CLI exited ${String(result.exitCode)}: ${result.stderr.trim()}`);
  }
  const value: unknown = JSON.parse(result.stdout);
  return value;
}

export function parseCliPtyJson(result: CliResult): unknown {
  const transcript = result.stdout.replaceAll("\r", "");
  const start = transcript.indexOf("{");
  const end = transcript.lastIndexOf("}");
  if (start === -1 || end < start) {
    throw new Error(`PTY CLI emitted no JSON object: ${transcript.trim()}`);
  }
  const value: unknown = JSON.parse(transcript.slice(start, end + 1));
  return value;
}
