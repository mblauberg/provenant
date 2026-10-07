/** Thin facade over the single Python host/client owner. */
import { spawn, type ChildProcess } from "node:child_process";
import { resolve } from "node:path";

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
  const root = process.env.AGENT_FABRIC_PRODUCT_ROOT || resolve(import.meta.dirname, "../../..");
  return await new Promise<Record<string, unknown>>((done) => {
    const child = spawn(resolve(root, "scripts/fabric-hosts"), ["hosts", action, "--json", "--", ...hosts],
      { cwd, env: process.env, detached: true, stdio: ["ignore", "pipe", "pipe"] });
    active.add(child);
    let output = "", size = 0, error: string | undefined;
    let stderrTail = Buffer.alloc(0);
    const stop = (code: string) => {
      error = code;
      terminate(child);
    };
    const abort = () => stop("hosts_cancelled");
    signal?.addEventListener("abort", abort, { once: true });
    if (signal?.aborted) abort();
    child.stdout.on("data", (chunk: Buffer) => {
      size += chunk.length;
      if (size > 4 * 1024 * 1024) stop("hosts_bad_response");
      else output += chunk.toString("utf8");
    });
    child.stderr.on("data", (chunk: Buffer) => {
      stderrTail = Buffer.concat([stderrTail, chunk]);
      if (stderrTail.length > 4096) stderrTail = stderrTail.subarray(-4096);
    });
    child.on("error", () => { error = "hosts_owner_unavailable"; });
    child.on("close", () => {
      active.delete(child);
      signal?.removeEventListener("abort", abort);
      if (error === undefined) {
        try { done(JSON.parse(output) as Record<string, unknown>); return; }
        catch { error = "hosts_bad_response"; }
      }
      const detail = stderrTail.toString("utf8").trim();
      done({ ok: false, error: { code: error, message: detail || "Host owner did not return a JSON result" } });
    });
  });
}
