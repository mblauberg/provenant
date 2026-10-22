/**
 * Trusted fast gates. A gate is an allowlisted argv run in the node's working
 * tree with a minimal environment. That is not confinement: the command has
 * the user's filesystem and network reach.
 *
 * Launch handshake: the owner commits the intent, starts `fabric gate-exec
 * TOKEN` as a new process-group leader reading a pipe only the owner can
 * write (Node opens pipes close-on-exec), commits the leader's identity, then
 * writes one `go` line. A wrapper that reads end-of-file first runs nothing,
 * so no gate runs unrecorded. The wrapper runs the argv as a child in its own
 * group, reaps it, and writes the exit status atomically.
 */
import { spawn, type ChildProcess } from "node:child_process";
import { createHash } from "node:crypto";
import { closeSync, openSync, readFileSync, renameSync, writeFileSync } from "node:fs";
import { createRequire } from "node:module";
import { dirname, resolve } from "node:path";
import { fileURLToPath } from "node:url";

const CLI = resolve(dirname(fileURLToPath(import.meta.url)), "..", "cli.ts");
const GATE_ENV = ["PATH", "HOME", "USER", "LOGNAME", "LANG", "LC_ALL", "TMPDIR", "SHELL", "TERM"];

/** How to run this Fabric CLI as a child process. */
export function fabricCommand(args: string[], env: NodeJS.ProcessEnv = process.env): { command: string; args: string[] } {
  const loader = env.AGENT_FABRIC_TSX_LOADER || createRequire(import.meta.url).resolve("tsx");
  return { command: process.execPath, args: ["--import", loader, CLI, ...args] };
}

export interface GateOrder { token: string; argv: string[]; cwd: string; out: string; err: string; exit: string }
export interface GateExit { token: string; exit_code: number | null; signal: string | null; finished_at: string }

export const argvHash = (argv: string[]): string => createHash("sha256").update(JSON.stringify(argv)).digest("hex");

/** A started wrapper, blocked until `go`. */
export interface GateLaunch { pid: number; child: ChildProcess; exited: Promise<void>; go(order: GateOrder): void }

export function launchGate(token: string, cwd: string, env: NodeJS.ProcessEnv = process.env): GateLaunch {
  const { command, args } = fabricCommand(["gate-exec", token], env);
  const child = spawn(command, args, { cwd, env, detached: true, stdio: ["pipe", "ignore", "ignore"] });
  if (child.pid === undefined) throw new Error("gate wrapper did not start");
  const exited = new Promise<void>((resolveExit) => { child.once("exit", () => resolveExit()); child.once("error", () => resolveExit()); });
  child.stdin!.on("error", () => { /* a wrapper that died early is seen through its exit */ });
  return {
    pid: child.pid, child, exited,
    go(order) { child.stdin!.end(`${JSON.stringify(order)}\n`); },
  };
}

export function readGateExit(path: string, token: string): GateExit | undefined {
  try {
    const value = JSON.parse(readFileSync(path, "utf8")) as GateExit;
    return value.token === token && typeof value.finished_at === "string" ? value : undefined;
  } catch { return undefined; }
}

/** `fabric gate-exec TOKEN`: wait for the owner's go line, then run and reap the gate. */
export async function runGateExec(token: string): Promise<number> {
  const chunks: Buffer[] = [];
  for await (const chunk of process.stdin) chunks.push(chunk as Buffer);
  const text = Buffer.concat(chunks).toString("utf8");
  const end = text.indexOf("\n");
  if (end < 0) return 3; // the owner never said go: run nothing
  let order: GateOrder;
  try { order = JSON.parse(text.slice(0, end)) as GateOrder; } catch { return 3; }
  if (order.token !== token || !Array.isArray(order.argv) || order.argv.length === 0 ||
      !order.argv.every((item) => typeof item === "string")) return 3;
  const env = Object.fromEntries(GATE_ENV.filter((name) => process.env[name] !== undefined).map((name) => [name, process.env[name]]));
  const out = openSync(order.out, "w", 0o600), err = openSync(order.err, "w", 0o600);
  const result = await new Promise<{ code: number | null; signal: string | null }>((resolveRun) => {
    const child = spawn(order.argv[0]!, order.argv.slice(1), { cwd: order.cwd, env, stdio: ["ignore", out, err] });
    child.once("error", () => resolveRun({ code: 127, signal: null }));
    child.once("exit", (code, signal) => resolveRun({ code, signal }));
  });
  closeSync(out); closeSync(err);
  const temporary = `${order.exit}.${process.pid}.tmp`;
  writeFileSync(temporary, `${JSON.stringify({ token, exit_code: result.code, signal: result.signal, finished_at: new Date().toISOString() })}\n`,
    { mode: 0o600 });
  renameSync(temporary, order.exit);
  return 0;
}
