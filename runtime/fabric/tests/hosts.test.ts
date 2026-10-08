import { Client } from "@modelcontextprotocol/sdk/client/index.js";
import { StdioClientTransport } from "@modelcontextprotocol/sdk/client/stdio.js";
import { createRequire } from "node:module";
import { execFileSync } from "node:child_process";
import { mkdtempSync, mkdirSync, rmSync, writeFileSync, readFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join, resolve } from "node:path";
import { expect, it } from "vitest";

function tempRoot(prefix: string) {
  // Project fixtures own their Git roots, so scratch may live in a worktree.
  return resolve(mkdtempSync(join(tmpdir(), prefix)));
}

it.each(["codex", "agy", "claude"])("exposes host list and peer doctor through the %s MCP seat", async (seat) => {
  const root = tempRoot("fabric-hosts-mcp-");
  const workspace = join(root, "Repos/project"), instance = join(root, "instance");
  const peerHome = join(root, "peer"), ssh = join(root, "ssh");
  const python = execFileSync("python3", ["-c", "import sys;print(sys.executable)"], { encoding: "utf8" }).trim();
  mkdirSync(join(peerHome, ".agents/.agent-fabric"), { recursive: true });
  mkdirSync(join(peerHome, "Repos/project"), { recursive: true });
  execFileSync("git", ["-C", join(peerHome, "Repos/project"), "init", "-q"]);
  writeFileSync(join(peerHome, ".agents/.agent-fabric/hosts.json"), JSON.stringify({ schema_version: 1, local_host: "workshop" }));
  writeFileSync(ssh, `#!${python}\nimport os,sys\nos.environ["HOME"]=${JSON.stringify(peerHome)}\nos.environ["AGENT_FABRIC_INSTANCE_ROOT"]=${JSON.stringify(join(peerHome, ".agents"))}\nos.chdir(os.environ["HOME"])\nos.execv(${JSON.stringify(resolve(import.meta.dirname, "../../../scripts/provenant"))},["provenant","peer"])\n`, { mode: 0o755 });
  mkdirSync(workspace, { recursive: true });
  execFileSync("git", ["-C", workspace, "init", "-q"]);
  mkdirSync(join(instance, ".agent-fabric"), { recursive: true });
  writeFileSync(join(instance, ".agent-fabric/hosts.json"), JSON.stringify({ schema_version: 1, local_host: "laptop",
    peers: { workshop: { ssh_destination: "workshop" } } }));
  const client = new Client({ name: "hosts-contract", version: "1" });
  try {
    await client.connect(new StdioClientTransport({
      command: process.execPath,
      args: ["--import", createRequire(import.meta.url).resolve("tsx"), resolve(import.meta.dirname, "../src/server.ts")],
      cwd: workspace,
      env: { ...process.env as Record<string, string>, HOME: root,
        AGENT_FABRIC_INSTANCE_ROOT: instance, AGENT_FABRIC_PRODUCT_ROOT: resolve(import.meta.dirname, "../../.."),
        AGENT_FABRIC_STATE_DIRECTORY: join(root, "state"), AGENT_FABRIC_SEAT: seat,
        AGENT_FABRIC_SSH_PROGRAM: ssh, HARNESS_PYTHON: python },
      stderr: "pipe",
    }));
    expect((await client.listTools()).tools.map((tool) => tool.name)).toContain("fabric_hosts");
    const list = await client.callTool({ name: "fabric_hosts", arguments: { action: "list" } });
    expect(list.structuredContent).toMatchObject({ schema: "fabric.hosts.v1", hosts: [
      { host: "laptop", local: true }, { host: "workshop", local: false, ssh_destination: "workshop" },
    ] });
    const doctor = await client.callTool({ name: "fabric_hosts", arguments: { action: "doctor", hosts: ["laptop"] } });
    expect(doctor.structuredContent).toMatchObject({ schema: "fabric.hosts.doctor.v1", project_path: "Repos/project",
      hosts: [{ host: "laptop", reachability: "reachable", result: { project: { present: true } } }] });
    const peer = await client.callTool({ name: "fabric_hosts", arguments: { action: "doctor", hosts: ["workshop"] } });
    expect(peer.structuredContent).toMatchObject({ schema: "fabric.hosts.doctor.v1", project_path: "Repos/project",
      hosts: [{ host: "workshop", reachability: "reachable", ok: true,
        result: { host: "workshop", project: { path: "Repos/project", present: true } } }] });
    const run = join(peerHome, "Repos/project/.agent-run/runs/20261008-0000-dispatch-peer-abcdef");
    const attempt = join(run, "tasks/peer-task/attempt-001");
    mkdirSync(attempt, { recursive: true });
    const row = JSON.parse(readFileSync(resolve(import.meta.dirname, "fixtures/attempt.json"), "utf8"));
    Object.assign(row, { task_id: "peer-task", run_id: "mcp-abcdef", started_at: new Date().toISOString(), ended_at: new Date().toISOString(),
      provenance: { requested: { adapter: "fixture" }, resolved_model: "fixture" },
      paths: { result: "tasks/peer-task/attempt-001/result.md" } });
    writeFileSync(join(attempt, "attempt.json"), JSON.stringify(row));
    writeFileSync(join(attempt, "result.md"), "peer output " + "x".repeat(40000));
    const lanes = await client.callTool({ name: "fabric_runs", arguments: { detail: "full" } });
    expect(lanes.structuredContent, JSON.stringify(lanes.structuredContent)).toMatchObject({ schema: "fabric.runs.v2", runs: [{ host: "workshop", id: "peer-task@workshop", state: "terminal" }] });
    const status = await client.callTool({ name: "fabric_status", arguments: { ids: ["peer-task@workshop"], detail: "full" } });
    expect(status.structuredContent).toMatchObject({ runs: [{ host: "workshop", reachability: "reachable", state: "terminal" }] });
    const output = await client.callTool({ name: "fabric_output", arguments: { id: "peer-task@workshop", max_bytes: 40 } });
    expect(output.structuredContent).toMatchObject({ host: "workshop", next_offset: 40 });
    const cancel = await client.callTool({ name: "fabric_cancel", arguments: { id: "peer-task@workshop", operation_id: "seat-stop", detail: "full" } });
    expect(cancel.structuredContent).toMatchObject({ host: "workshop", operation_id: "seat-stop", runs: [{ state: "terminal" }] });
    const dispatch = await client.callTool({ name: "fabric_dispatch", arguments: { host: "workshop", prompt: "fixture", cwd: "/etc", operation_id: "seat-start", detail: "full" } });
    expect(dispatch.structuredContent).toMatchObject({ status: "rejected", error: "invalid_remote_path" });
    const unknown = await client.callTool({ name: "fabric_hosts", arguments: { action: "list", hosts: ["missing"] } });
    expect(unknown.structuredContent).toMatchObject({ ok: false, error: { code: "selector_not_found" } });
  } finally {
    await client.close();
    rmSync(root, { recursive: true, force: true });
  }
}, 30_000);

it("preserves Unicode in host output split across UTF-8 byte boundaries", async () => {
  const { fabricHosts } = await import("../src/hosts.js");
  const root = tempRoot("fabric-hosts-unicode-");
  const python = execFileSync("python3", ["-c", "import sys;print(sys.executable)"], { encoding: "utf8" }).trim();
  mkdirSync(join(root, "scripts"));
  writeFileSync(join(root, "scripts/fabric-hosts"), `#!${python}\nimport os,time\nos.write(1,b'{"path":"caf\\xc3')\ntime.sleep(.1)\nos.write(1,b'\\xa9"}')\n`, { mode: 0o755 });
  const previous = process.env.AGENT_FABRIC_PRODUCT_ROOT;
  process.env.AGENT_FABRIC_PRODUCT_ROOT = root;
  try { expect(await fabricHosts("list", [], root)).toEqual({ path: "café" }); }
  finally {
    if (previous === undefined) delete process.env.AGENT_FABRIC_PRODUCT_ROOT;
    else process.env.AGENT_FABRIC_PRODUCT_ROOT = previous;
    rmSync(root, { recursive: true, force: true });
  }
});

it("cancellation reaps the SSH child before the facade returns", async () => {
  const { fabricHosts } = await import("../src/hosts.js");
  const { readFileSync, existsSync } = await import("node:fs");
  const { setTimeout: delay } = await import("node:timers/promises");
  const root = tempRoot("fabric-hosts-cancel-");
  const instance = join(root, "instance"), pidFile = join(root, "ssh.pid"), shim = join(root, "ssh");
  execFileSync("git", ["-C", root, "init", "-q"]);
  mkdirSync(join(instance, ".agent-fabric"), { recursive: true });
  writeFileSync(join(instance, ".agent-fabric/hosts.json"), JSON.stringify({ schema_version: 1, local_host: "laptop",
    peers: { workshop: { ssh_destination: "workshop", response_deadline: 30 } } }));
  const python = execFileSync("python3", ["-c", "import sys;print(sys.executable)"], { encoding: "utf8" }).trim();
  writeFileSync(shim, `#!${python}\nimport os,time\nopen(${JSON.stringify(pidFile)},'w').write(str(os.getpid()))\ntime.sleep(30)\n`, { mode: 0o755 });
  const saved = { ...process.env };
  let pid: number | undefined;
  try {
    process.env.HOME = root;
    process.env.AGENT_FABRIC_INSTANCE_ROOT = instance;
    process.env.AGENT_FABRIC_SSH_PROGRAM = shim;
    process.env.HARNESS_PYTHON = python;
    const controller = new AbortController();
    const pending = fabricHosts("doctor", ["workshop"], root, controller.signal);
    for (let count = 0; count < 100 && !existsSync(pidFile); count++) await delay(20);
    expect(existsSync(pidFile)).toBe(true);
    pid = Number(readFileSync(pidFile, "utf8"));
    controller.abort();
    expect(await pending).toMatchObject({ ok: false, error: { code: "hosts_cancelled" } });
    expect(() => process.kill(pid!, 0)).toThrow();
  } finally {
    if (pid !== undefined) { try { process.kill(-pid, "SIGKILL"); } catch { /* already reaped */ } }
    for (const key of Object.keys(process.env)) if (!(key in saved)) delete process.env[key];
    Object.assign(process.env, saved);
    rmSync(root, { recursive: true, force: true });
  }
}, 10_000);

it("includes the launcher's bounded stderr tail in host facade errors", async () => {
  const { fabricHosts } = await import("../src/hosts.js");
  const root = tempRoot("fabric-hosts-launch-error-");
  mkdirSync(join(root, "scripts"));
  writeFileSync(join(root, "scripts", "fabric-hosts"), "#!/bin/sh\nprintf '%05000d' 0 >&2\nexit 1\n", { mode: 0o755 });
  const oldRoot = process.env.AGENT_FABRIC_PRODUCT_ROOT;
  process.env.AGENT_FABRIC_PRODUCT_ROOT = root;
  try {
    const result = await fabricHosts("list", [], root);
    expect(result).toMatchObject({ ok: false, error: { code: "hosts_bad_response" } });
    expect((result.error as { message: string }).message).toBe("0".repeat(4096));
  } finally {
    if (oldRoot === undefined) delete process.env.AGENT_FABRIC_PRODUCT_ROOT;
    else process.env.AGENT_FABRIC_PRODUCT_ROOT = oldRoot;
    rmSync(root, { recursive: true, force: true });
  }
}, 10_000);
