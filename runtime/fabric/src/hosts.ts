/** Thin facade over the single Python host/client owner. */
import { spawn, type ChildProcess } from "node:child_process";
import { resolve } from "node:path";
import { StringDecoder } from "node:string_decoder";
import { existsSync } from "node:fs";
import { homedir } from "node:os";

export async function federatedLane(action: string, cwd: string, input: Record<string, unknown>, signal?: AbortSignal): Promise<Record<string, any> | undefined> {
  if (process.env.PROVENANT_HOST_LOCAL_ONLY === "1") return undefined;
  const config = resolve(process.env.AGENT_FABRIC_INSTANCE_ROOT || resolve(homedir(), ".agents"), ".agent-fabric/hosts.json");
  if (!existsSync(config) && input.host === undefined &&
      ![input.id, input.resume, input.handoff, input.session, ...(Array.isArray(input.ids) ? input.ids : [])].some((value) => typeof value === "string" && value.includes("@"))) return undefined;
  const result = await hostOwner(["lane"], cwd, signal, { action, cwd, input });
  if (result.local) {
    if (result.input) {
      for (const key of Object.keys(input)) delete input[key];
      Object.assign(input, result.input);
    } else {
      delete input.host;
      delete input.operation_id;
    }
    if (result.fallback) input.placement_fallback = result.fallback;
    return undefined;
  }
  return result;
}

const active = new Set<ChildProcess>();
function terminate(child: ChildProcess) {
  if (child.pid !== undefined) {
    try { process.kill(-child.pid, "SIGTERM"); } catch { child.kill("SIGTERM"); }
  }
}
export function releaseHostChecks() {
  for (const child of active) terminate(child);
}

export async function fabricHosts(action: "list" | "doctor", hosts: string[], cwd: string, signal?: AbortSignal) {
  return hostOwner(["hosts", action, "--json", "--", ...hosts], cwd, signal);
}

async function hostOwner(args: string[], cwd: string, signal?: AbortSignal, request?: unknown) {
  const root = process.env.AGENT_FABRIC_PRODUCT_ROOT || resolve(import.meta.dirname, "../../..");
  return await new Promise<Record<string, unknown>>((done) => {
    const child = spawn(resolve(root, "scripts/fabric-hosts"), args,
      { cwd, env: process.env, detached: true, stdio: [request === undefined ? "ignore" : "pipe", "pipe", "pipe"] });
    if (request !== undefined) {
      child.stdin?.on("error", () => { /* Close/error returns the owner error below. */ });
      child.stdin?.end(JSON.stringify(request));
    }
    active.add(child);
    let output = "", size = 0, error: string | undefined;
    const decoder = new StringDecoder("utf8");
    let stderrTail = Buffer.alloc(0);
    const stop = (code: string) => {
      if (error === undefined) error = code;
      terminate(child);
    };
    const abort = () => stop("hosts_cancelled");
    signal?.addEventListener("abort", abort, { once: true });
    if (signal?.aborted) abort();
    child.stdout!.on("data", (chunk: Buffer) => {
      size += chunk.length;
      if (size > 4 * 1024 * 1024) stop("hosts_bad_response");
      else output += decoder.write(chunk);
    });
    child.stderr!.on("data", (chunk: Buffer) => {
      stderrTail = Buffer.concat([stderrTail, chunk]);
      if (stderrTail.length > 4096) stderrTail = stderrTail.subarray(-4096);
    });
    child.on("error", () => { error = "hosts_owner_unavailable"; });
    child.on("close", () => {
      active.delete(child);
      signal?.removeEventListener("abort", abort);
      if (error === undefined) {
        output += decoder.end();
        try { done(JSON.parse(output) as Record<string, unknown>); return; }
        catch { error = "hosts_bad_response"; }
      }
      const detail = stderrTail.toString("utf8").trim();
      done({ ok: false, error: { code: error, message: detail || "Host owner did not return a JSON result" } });
    });
  });
}
