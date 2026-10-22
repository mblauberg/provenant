/**
 * The graph owner: one detached process per graph, `fabric graph-owner ID
 * PROJECT`. It holds no authority of its own; every node is an ordinary
 * dispatch made with the creating seat's identity. It calls no model and is
 * never polled: the chair hears from it through Fabric messages.
 */
import { execFileSync, spawn } from "node:child_process";
import { existsSync, mkdirSync, readFileSync, rmSync, statSync, writeFileSync } from "node:fs";
import { isAbsolute, join, relative, resolve } from "node:path";

import { catalogueSnapshot } from "../catalogue.js";
import { cancelConfiguredRun, dispatchConfiguredProvider, hostOwnersInThemselves, productRoot } from "../execution.js";
import { databasePath, runRoot, withoutGitRedirects, type Identity } from "../identity.js";
import {
  listRecordedRuns, observedAlive, processMatches, processStartedAt, readOwnerRecord, runProcessAlive,
  signalRecordedGroup, statusRows, type RecordedGroup,
} from "../run-registry.js";
import { Store } from "../store.js";
import { validateGraph, faultLines, type GraphPlan, type PlannedNode } from "./document.js";
import { argvHash, launchGate, readGateExit } from "./gates.js";
import { GraphStore, newToken, type Attempt, type AttemptState, type GateProc, type Graph } from "./store.js";

export interface GraphDefaults {
  pool_caps: Record<string, number>;
  tick_seconds: number;
  probe_seconds: number;
  gate_grace_seconds: number;
}

const BUILT_IN: GraphDefaults = {
  pool_caps: { opencode: 2, cursor: 1, agy: 2, codex: 3, claude: 3, default: 1 },
  tick_seconds: 2, probe_seconds: 30, gate_grace_seconds: 3,
};
const TERMINAL: AttemptState[] = ["passed", "failed", "blocked", "cancelled", "stopped", "superseded"];
const TIER_ORDER = { junior: 0, senior: 1, principal: 2 };
const WRITER_LOCK = "provenant-dispatch-writer.lock";

export function graphDefaults(env: NodeJS.ProcessEnv): GraphDefaults {
  try {
    const value = JSON.parse(readFileSync(join(productRoot(env), "config", "graph-defaults.json"), "utf8")) as Partial<GraphDefaults>;
    return { ...BUILT_IN, ...value, pool_caps: { ...BUILT_IN.pool_caps, ...(value.pool_caps ?? {}) } };
  } catch { return BUILT_IN; }
}

/** The directory holding one attempt's brief, gates and receipt. */
export function attemptDirectory(chair: Identity, row: Attempt, env: NodeJS.ProcessEnv): string {
  return join(runRoot(chair.cwd, env), "graphs", row.graph_id, `r${row.revision}`, row.node_id, `a${row.attempt}`);
}

const sleep = (ms: number) => new Promise((resolveSleep) => setTimeout(resolveSleep, ms));
const git = (cwd: string, args: string[]) => {
  try {
    return execFileSync("git", ["-C", cwd, ...args], { encoding: "utf8", env: withoutGitRedirects(process.env), stdio: ["ignore", "pipe", "ignore"] }).trim();
  } catch { return undefined; }
};
const inside = (root: string, path: string) => { const rel = relative(root, path); return rel === "" || (!rel.startsWith("..") && !isAbsolute(rel)); };

function resultPath(row: Record<string, any>): string | undefined {
  if (typeof row.result_path === "string") return row.result_path;
  const result = row.paths?.result;
  return typeof result === "string" && typeof row.run_dir === "string" ? resolve(row.run_dir, result) : undefined;
}

/** Hold dispatch's own writer lock on a worktree while gates run, so a concurrent writer is still refused. */
async function holdWriterLock(gitDir: string, env: NodeJS.ProcessEnv): Promise<() => void> {
  const python = env.HARNESS_PYTHON && isAbsolute(env.HARNESS_PYTHON) ? env.HARNESS_PYTHON : "python3";
  const script = "import fcntl,sys,time\nf=open(sys.argv[1],'a+')\nfor _ in range(300):\n  try:\n    fcntl.flock(f.fileno(),fcntl.LOCK_EX|fcntl.LOCK_NB);break\n  except OSError: time.sleep(0.1)\nelse: sys.exit(3)\nprint('locked',flush=True)\nsys.stdin.read()\n";
  const child = spawn(python, ["-c", script, join(gitDir, WRITER_LOCK)], { stdio: ["pipe", "pipe", "ignore"] });
  await new Promise<void>((resolveLock, reject) => {
    child.stdout!.once("data", () => resolveLock());
    child.once("exit", (code) => reject(new Error(`worktree writer lock unavailable (${code})`)));
  });
  return () => { child.stdin!.end(); };
}

class Owner {
  readonly #store: GraphStore;
  readonly #env: NodeJS.ProcessEnv;
  readonly #defaults: GraphDefaults;
  readonly #inflight = new Map<string, Promise<void>>();
  readonly #aborts = new Map<string, AbortController>();
  readonly #signatures = new Map<string, string>();
  #wake: () => void = () => {};
  #lastProbe = 0;
  #graph!: Graph;
  #plan!: GraphPlan;
  #nodes = new Map<string, PlannedNode>();
  #chair!: Identity;
  #started: string | null = null;

  constructor(readonly project: string, readonly graphId: string, env: NodeJS.ProcessEnv) {
    this.#env = { ...env, PROVENANT_HOST_LOCAL_ONLY: "1" };
    this.#store = new GraphStore(env);
    this.#defaults = graphDefaults(env);
  }

  async run(): Promise<number> {
    try {
      const graph = this.#store.graph(this.project, this.graphId);
      if (!graph || graph.state !== "running") return 0;
      this.#started = processStartedAt(process.pid);
      if (!this.#store.claimOwner(this.project, this.graphId, process.pid, this.#started, processMatches)) return 0;
      hostOwnersInThemselves();
      const mailbox = new Store(databasePath(this.#env));
      try { this.#chair = { ...graph.chair, registeredProjects: mailbox.projects() }; } finally { mailbox.close(); }
      const document = this.#store.document(this.project, this.graphId, graph.revision);
      const checked = validateGraph(document, this.#chair, catalogueSnapshot(productRoot(this.#env), this.#env));
      if (!checked.plan) {
        this.#notify(`graph ${this.graphId} stopped: its document no longer validates · ${faultLines(checked.faults).split("\n")[0]}`);
        this.#store.setGraph(this.project, this.graphId, { state: "stopped" }, this.#chair.agentId);
        return 1;
      }
      this.#plan = checked.plan;
      this.#nodes = new Map(checked.plan.nodes.map((node) => [node.id, node]));
      this.#graph = graph;
      this.#recover();
      for (;;) {
        if (!this.#store.owns(this.project, this.graphId, process.pid, this.#started)) return 0;
        this.#graph = this.#store.graph(this.project, this.graphId)!;
        if (this.#graph.cancel_requested) this.#cancelAll();
        await this.#step();
        if (await this.#finishIfDone()) return 0;
        await Promise.race([sleep(this.#defaults.tick_seconds * 1000), new Promise<void>((wake) => { this.#wake = wake; })]);
      }
    } finally {
      for (const abort of this.#aborts.values()) abort.abort();
      await Promise.allSettled(this.#inflight.values());
      this.#store.close();
    }
  }

  #current(): Map<string, Attempt> { return this.#store.current(this.project, this.graphId); }

  #notify(body: string): void {
    const mailbox = new Store(databasePath(this.#env));
    try { mailbox.send(this.#chair, this.#chair.agentId, body, { kind: "graph" }); }
    catch { this.#store.notice(this.project, this.#chair.agentId, body); }
    finally { mailbox.close(); }
  }

  #task(node: string, state: string): void {
    const tasks = new Store(databasePath(this.#env));
    try { tasks.updateTask(this.#chair, `${this.graphId}.${node}`, state); } catch { /* the task row is a view */ } finally { tasks.close(); }
  }

  /** Reconcile what a previous owner left: nothing is redispatched under an identity that has a lane. */
  #recover(): void {
    for (const row of this.#current().values()) {
      if (row.state === "running" && row.run_id === null) {
        this.#store.transition(row, ["running"], "pending", { holds_slot: false }, this.#chair.agentId);
      } else if (row.state === "running" && row.run_dir !== null && readOwnerRecord(row.run_dir) === undefined &&
          !existsSync(join(row.run_dir, "tasks", row.task_id, "attempt-001", "attempt.json")) &&
          !runProcessAlive(row.run_dir, row.task_id, 1) && Date.now() - row.updated_at > 10_000) {
        // The crash fell between recording the launch and starting its owner: that lane never ran.
        if (this.#store.transition(row, ["running"], "superseded", { holds_slot: false }, this.#chair.agentId)) {
          const node = this.#nodes.get(row.node_id)!;
          this.#store.addAttempt(row, node.pool);
        }
      }
    }
  }

  /** Mark every unfinished attempt for cancellation; the step loop fences each. */
  #cancelAll(): void {
    for (const row of this.#current().values()) {
      if (row.state === "pending") this.#store.transition(row, ["pending"], "cancelled", {}, this.#chair.agentId);
      else if (["running", "gating"].includes(row.state))
        this.#store.transition(row, [row.state], "cancelling", {}, this.#chair.agentId);
    }
  }

  async #step(): Promise<void> {
    const current = this.#current();
    const probe = Date.now() - this.#lastProbe >= this.#defaults.probe_seconds * 1000;
    if (probe) this.#lastProbe = Date.now();
    const observe: Attempt[] = [];
    for (const row of current.values()) {
      if (this.#inflight.has(row.node_id)) {
        if (row.state === "cancelling") this.#aborts.get(row.node_id)?.abort();
        continue;
      }
      if (row.state === "running" && row.run_id !== null) {
        const signature = this.#signature(row);
        if (probe || signature !== this.#signatures.get(row.node_id)) observe.push(row);
        this.#signatures.set(row.node_id, signature);
      } else if (row.state === "running") {
        this.#store.transition(row, ["running"], "pending", { holds_slot: false }, this.#chair.agentId);
      } else if (row.state === "gating") {
        this.#spawnTask(row.node_id, (abort) => this.#gates(row, abort));
      } else if (row.state === "cancelling") {
        this.#spawnTask(row.node_id, () => this.#fence(row));
      }
    }
    if (observe.length) await this.#observe(observe);
    await this.#releaseFinishedSlots();
    this.#block();
    await this.#launchReady();
  }

  #signature(row: Attempt): string {
    try { const stat = statSync(join(row.run_dir!, "dispatch-status.json")); return `${stat.mtimeMs}:${stat.size}`; }
    catch { return "absent"; }
  }

  #spawnTask(node: string, work: (abort: AbortSignal) => Promise<void>): void {
    const abort = new AbortController();
    this.#aborts.set(node, abort);
    const task = work(abort.signal).catch((error: unknown) => {
      this.#store.notice(this.project, this.#chair.agentId, `${this.graphId} ${node}: owner error ${error instanceof Error ? error.message : String(error)}`);
    }).finally(() => { this.#inflight.delete(node); this.#aborts.delete(node); this.#wake(); });
    this.#inflight.set(node, task);
  }

  /** Read running lanes through the registry and settle the ones that ended. */
  async #observe(rows: Attempt[]): Promise<void> {
    const status = await statusRows(this.#chair.cwd, rows.map((row) => row.run_id!), 0, "all", undefined, "brief", false, null);
    for (const row of rows) {
      const lane = (status.runs ?? []).find((candidate: Record<string, any>) => candidate.run_id === row.run_id &&
        (candidate.task_id === row.task_id || candidate.task_id === undefined));
      if (lane?.state === "input_required") {
        // Slice 1 has no resume path inside a graph: the lane stays for the chair to answer by hand.
        this.#settle(row, "failed", { ...(row.outcome ?? {}), lane_status: "input_required", reason: "lane needs input; answer it with fabric resume" });
        continue;
      }
      if (!lane || lane.state !== "terminal") continue;
      const node = this.#nodes.get(row.node_id)!;
      const outcome = {
        lane_status: lane.status ?? null, result_path: resultPath({ ...lane, run_dir: lane.run_dir ?? row.run_dir }) ?? null,
        route: lane.provenance?.line ?? lane.route ?? null, context: lane.context ?? null, usage_source: lane.usage_source ?? null,
        ...(node.worktree ? { head_sha: git(node.worktree.path, ["rev-parse", "HEAD"]) ?? null } : {}),
      };
      const merged = { ...(row.outcome ?? {}), ...outcome };
      if (lane.status === "ok" && node.gates.length) {
        if (this.#store.transition(row, ["running"], "gating", { outcome: merged }, this.#chair.agentId)) this.#wake();
      } else if (lane.status === "ok") {
        this.#settle(row, "passed", merged);
      } else {
        this.#settle(row, "failed", { ...merged, reason: `lane ${lane.status ?? "ended"}` });
      }
    }
  }

  /** Terminal state, receipt and task row, then a notice for a failure. */
  #settle(row: Attempt, state: "passed" | "failed" | "cancelled", outcome: Record<string, unknown>): void {
    const from: AttemptState[] = state === "cancelled" ? ["cancelling"] : ["running", "gating"];
    const final: Record<string, unknown> = { ...outcome, state, stale: false };
    if (!this.#store.transition(row, from, state, { outcome: final }, this.#chair.agentId)) return;
    this.#receipt({ ...row, state, outcome: final });
    this.#task(row.node_id, state === "passed" ? "done" : state);
    if (state === "failed")
      this.#notify(`graph ${this.graphId} node ${row.node_id} failed: ${String(final.reason ?? final.lane_status)}; its descendants are blocked, independent nodes continue`);
  }

  #receipt(row: Attempt): void {
    const node = this.#nodes.get(row.node_id)!;
    const dir = attemptDirectory(this.#chair, row, this.#env);
    mkdirSync(dir, { recursive: true });
    const outcome = row.outcome ?? {};
    const receipt = {
      schema: "provenant.graph-node-receipt.v1",
      identity: { project: row.project, graph_id: row.graph_id, node_id: row.node_id, revision: row.revision,
        attempt: row.attempt, restart: row.restart, input_attempts: row.input_attempts },
      spec_hash: node.spec_hash,
      plan: { tier: node.tier, mode: node.authority.mode, confidential: node.authority.confidential,
        ...(node.worktree ? { worktree: node.worktree.path } : {}), ...(node.cwd ? { cwd: node.cwd } : {}) },
      route: { requested: node.route, pool: node.pool, line: outcome.route ?? null },
      dispatch: { run_id: row.run_id, task_id: row.task_id, run_dir: row.run_dir },
      outcome: {
        state: row.state, lane_status: outcome.lane_status ?? null, stale: false,
        base_sha: outcome.base_sha ?? null, head_sha: outcome.head_sha ?? null, result_path: outcome.result_path ?? null,
        gates: row.gate_procs.map((gate) => ({ index: gate.index, argv: node.gates[gate.index], exit_code: gate.exit?.exit_code ?? null,
          signal: gate.exit?.signal ?? null, stdout: join(dir, "gates", `${gate.index}.out`), stderr: join(dir, "gates", `${gate.index}.err`) })),
        ...(outcome.reason ? { reason: outcome.reason } : {}),
      },
      tokens: { context: outcome.context ?? null, usage_source: outcome.usage_source ?? null },
      written_at: new Date().toISOString(),
    };
    writeFileSync(join(dir, "receipt.json.tmp"), `${JSON.stringify(receipt, null, 2)}\n`, { mode: 0o600 });
    execFileSync("mv", ["-f", join(dir, "receipt.json.tmp"), join(dir, "receipt.json")]);
  }

  /** Free a provider slot once its lane process has gone, and a writer's worktree once nothing of it can run. */
  async #releaseFinishedSlots(): Promise<void> {
    for (const row of this.#current().values()) {
      if (row.holds_slot && row.state !== "running" && row.state !== "cancelling" &&
          (row.run_dir === null || !runProcessAlive(row.run_dir, row.task_id, 1)))
        this.#store.releaseSlot(row);
      const node = this.#nodes.get(row.node_id)!;
      if (node.worktree && TERMINAL.includes(row.state) && !this.#inflight.has(row.node_id) && !row.holds_slot &&
          (await this.#groupsExited(row))) {
        const later = [...this.#nodes.values()].some((other) => other.worktree?.path === node.worktree!.path && other.id !== node.id &&
          !TERMINAL.includes(this.#current().get(other.id)?.state ?? "pending"));
        if (!later) this.#store.releaseReservation(node.worktree.path, node.worktree.gitDir, this.graphId);
      }
    }
    for (const orphan of this.#store.orphanSlots(processMatches))
      if (orphan.run_dir === null || !runProcessAlive(orphan.run_dir, orphan.task_id, 1)) this.#store.releaseSlot(orphan);
  }

  async #groupsExited(row: Attempt): Promise<boolean> {
    for (const gate of row.gate_procs)
      if (gate.pid !== undefined && (await signalRecordedGroup(this.#group(gate), this.#defaults.gate_grace_seconds * 1000)) !== "exited") return false;
    return true;
  }

  #group(gate: GateProc): RecordedGroup { return { pid: gate.pid!, pgid: gate.pgid ?? gate.pid!, started_at: gate.started_at ?? null }; }

  /** A pending node whose parent can no longer pass is blocked; the block reaches every descendant in turn. */
  #block(): void {
    let changed = true;
    while (changed) {
      changed = false;
      const current = this.#current();
      for (const row of current.values()) {
        if (row.state !== "pending") continue;
        const node = this.#nodes.get(row.node_id)!;
        if (node.depends_on.some((parent) => ["failed", "blocked", "cancelled", "stopped"].includes(current.get(parent)?.state ?? ""))) {
          if (this.#store.transition(row, ["pending"], "blocked", {}, this.#chair.agentId)) { this.#task(row.node_id, "blocked"); changed = true; }
        }
      }
    }
  }

  #handOccupancy(): Map<string, number> {
    const occupancy = new Map<string, number>();
    try {
      const known = new Set(this.#store.runIds());
      for (const run of listRecordedRuns(this.#chair.cwd)) {
        if (known.has(run.run_id) || !observedAlive(run.owner_pid, run.owner_started_at)) continue;
        const status = JSON.parse(readFileSync(join(run.run_dir, "dispatch-status.json"), "utf8")) as { routes?: Array<Record<string, unknown>> };
        for (const route of status.routes ?? []) {
          const adapter = String(route.adapter ?? ""), model = String(route.resolved_model ?? route.model ?? route.alias ?? "default");
          occupancy.set(`${adapter}\u0000${model}`, (occupancy.get(`${adapter}\u0000${model}`) ?? 0) + 1);
        }
      }
    } catch { /* best effort against hand dispatch */ }
    return occupancy;
  }

  async #launchReady(): Promise<void> {
    if (this.#graph.cancel_requested) return;
    const current = this.#current();
    const ready = [...current.values()].filter((row) => row.state === "pending" && !this.#inflight.has(row.node_id) &&
      this.#nodes.get(row.node_id)!.depends_on.every((parent) => current.get(parent)?.state === "passed"));
    if (!ready.length) return;
    const order = (row: Attempt) => this.#nodes.get(row.node_id)!;
    ready.sort((a, b) => (this.#plan.height[b.node_id]! - this.#plan.height[a.node_id]!) ||
      (TIER_ORDER[order(a).tier] - TIER_ORDER[order(b).tier]) || a.node_id.localeCompare(b.node_id));
    // Routes are explicit: a cooling route is dispatch's to warn about, never a reason to hold a node silently.
    const hand = this.#handOccupancy();
    for (const row of ready) {
      const node = order(row);
      const cap = this.#defaults.pool_caps[node.pool.adapter] ?? this.#defaults.pool_caps.default ?? 1;
      const reserved = this.#store.reserve(row, {
        maxWidth: this.#plan.max_width, cap, handOccupancy: hand.get(`${node.pool.adapter}\u0000${node.pool.model}`) ?? 0,
        ...(node.worktree ? { writer: { path: node.worktree.path, gitDir: node.worktree.gitDir, token: newToken(),
          owner: { pid: process.pid, started_at: this.#started, graph_id: this.graphId } } } : {}),
      }, this.#chair.agentId);
      if (reserved === "width") break;
      if (reserved !== "reserved") continue;
      this.#spawnTask(row.node_id, () => this.#launch(row, node, current));
    }
  }

  async #launch(row: Attempt, node: PlannedNode, current: Map<string, Attempt>): Promise<void> {
    const dir = attemptDirectory(this.#chair, row, this.#env);
    mkdirSync(dir, { recursive: true });
    const workCwd = node.worktree?.path ?? node.cwd ?? this.#chair.cwd;
    const inputs = node.inputs.map((input) => ({ from: input.from, attempt: current.get(input.from)!.attempt,
      path: String(current.get(input.from)!.outcome?.result_path ?? "") }));
    const prompt = node.brief.prompt ?? readFileSync(node.brief.prompt_file!, "utf8");
    const brief = inputs.length
      ? `${prompt.trimEnd()}\n\n## Inputs\n\nRead these files; they are the results of earlier graph nodes.\n\n${inputs.map((input) => `- ${input.from}: ${input.path}`).join("\n")}\n`
      : prompt;
    writeFileSync(join(dir, "brief.md"), brief, { mode: 0o600 });
    const readRoots = [...new Set([...(node.authority.read_roots ?? []),
      ...inputs.map((input) => resolve(input.path, "..")).filter((path) => !inside(workCwd, path))])];
    const inputAttempts = Object.fromEntries(node.depends_on.map((parent) => [parent, current.get(parent)!.attempt]));
    const base = node.worktree ? git(node.worktree.path, ["rev-parse", "HEAD"]) ?? null : undefined;
    this.#store.transition(row, ["running"], "running", { input_attempts: inputAttempts,
      outcome: base === undefined ? {} : { base_sha: base } }, this.#chair.agentId);
    const reservation = node.worktree ? this.#store.reservation(node.worktree.path)?.token : undefined;
    const env = reservation ? { ...this.#env, PROVENANT_GRAPH_RESERVATION: reservation } : this.#env;
    let launched = false;
    const result = await dispatchConfiguredProvider({
      prompt_file: join(dir, "brief.md"),
      adapter: node.route.adapter, ...(node.route.model ? { model: node.route.model } : {}),
      ...(node.route.alias ? { alias: node.route.alias } : {}), ...(node.route.effort ? { effort: node.route.effort } : {}),
      mode: node.authority.mode,
      ...(node.worktree ? { worktree: node.worktree.path } : node.cwd ? { cwd: node.cwd } : {}),
      ...(node.authority.network !== undefined ? { network: node.authority.network } : {}),
      ...(node.authority.confidential ? { confidential: true } : {}),
      ...(node.authority.allow_secrets ? { allow_secrets: true } : {}),
      ...(node.authority.capabilities?.length ? { capabilities: node.authority.capabilities as Array<"postgres" | "browser"> } : {}),
      ...(readRoots.length ? { read_roots: readRoots } : {}),
      ...(node.timeout_seconds ? { timeout_seconds: node.timeout_seconds } : {}),
      task_id: row.task_id, wait_seconds: 0,
    }, this.#chair, new AbortController().signal, env, {
      onLaunch: ({ runId, runDir }) => {
        if (runDir === undefined) throw new Error("dispatch did not report its run directory");
        // The pin first: a throw here or in the record removes the run directory and stops the launch.
        writeFileSync(join(runDir, "KEEP"), `graph ${this.graphId} ${row.node_id} a${row.attempt}\n`, { flag: "wx" });
        this.#store.recordLaunch(row, runId, runDir);
        launched = true;
      },
    });
    if (!launched || result.status === "rejected") {
      const reason = `dispatch ${String(result.error ?? result.status)}${result.fix ? ` · fix: ${String(result.fix)}` : ""}`;
      this.#store.releaseSlot(row);
      if (!this.#store.transition(row, ["running"], "failed", { outcome: { reason, state: "failed", stale: false } }, this.#chair.agentId)) {
        this.#store.transition(row, ["cancelling"], "cancelled", {}, this.#chair.agentId);
        return;
      }
      this.#receipt({ ...this.#current().get(row.node_id)!, state: "failed" });
      this.#task(row.node_id, "failed");
      this.#notify(`graph ${this.graphId} node ${row.node_id} failed: ${reason}; its descendants are blocked, independent nodes continue`);
    }
  }

  /** Run a node's gates in order; any gate without a recorded zero exit fails the node. */
  async #gates(row: Attempt, abort: AbortSignal): Promise<void> {
    const node = this.#nodes.get(row.node_id)!;
    const dir = join(attemptDirectory(this.#chair, row, this.#env), "gates");
    mkdirSync(dir, { recursive: true });
    // A previous owner's groups first: terminated and verified before anything reruns.
    for (const gate of row.gate_procs)
      if (gate.pid !== undefined && gate.exit === undefined &&
          (await signalRecordedGroup(this.#group(gate), this.#defaults.gate_grace_seconds * 1000)) !== "exited") return;
    while (row.holds_slot && row.run_dir !== null && runProcessAlive(row.run_dir, row.task_id, 1)) {
      if (abort.aborted) return;
      await sleep(250);
    }
    const procs: GateProc[] = row.gate_procs.filter((gate) => gate.exit !== undefined && gate.exit.exit_code === 0);
    const cwd = node.worktree?.path ?? node.cwd ?? this.#chair.cwd;
    const release = node.worktree ? await holdWriterLock(node.worktree.gitDir, this.#env) : () => {};
    let failure: string | undefined;
    try {
      for (let index = procs.length; index < node.gates.length; index++) {
        if (abort.aborted) return;
        const argv = node.gates[index]!;
        const token = newToken();
        procs[index] = { index, token, argv_hash: argvHash(argv) };
        this.#store.setGates(row, procs);
        const launch = launchGate(token, cwd, this.#env);
        procs[index] = { ...procs[index]!, pid: launch.pid, pgid: launch.pid, started_at: processStartedAt(launch.pid) };
        this.#store.setGates(row, procs);
        const exitPath = join(dir, `${index}.exit.json`);
        launch.go({ token, argv, cwd, out: join(dir, `${index}.out`), err: join(dir, `${index}.err`), exit: exitPath });
        const timedOut = await Promise.race([
          launch.exited.then(() => false),
          sleep(node.gate_timeout_seconds * 1000).then(() => true),
          new Promise<boolean>((resolveAbort) => abort.addEventListener("abort", () => resolveAbort(false), { once: true })),
        ]);
        // Clear the group whatever happened: a gate's background children never outlive it.
        const fenced = await signalRecordedGroup(this.#group(procs[index]!), this.#defaults.gate_grace_seconds * 1000);
        if (abort.aborted) return;
        const exit = readGateExit(exitPath);
        procs[index] = { ...procs[index]!, ...(exit ? { exit: { exit_code: exit.exit_code, signal: exit.signal } } : {}) };
        this.#store.setGates(row, procs);
        if (fenced === "fenced") { failure = `gate ${index} group could not be verified stopped`; break; }
        if (timedOut) { failure = `gate ${index} timed out after ${node.gate_timeout_seconds}s`; break; }
        if (!exit || exit.exit_code !== 0) { failure = `gate ${index} ${exit ? `exited ${exit.exit_code ?? exit.signal}` : "recorded no exit"}`; break; }
      }
    } finally { release(); }
    const latest = { ...this.#current().get(row.node_id)!, gate_procs: procs };
    const outcome = { ...(latest.outcome ?? {}), ...(node.worktree ? { head_sha: git(node.worktree.path, ["rev-parse", "HEAD"]) ?? null } : {}) };
    if (failure) this.#settle(latest, "failed", { ...outcome, reason: failure });
    else this.#settle(latest, "passed", outcome);
  }

  /** Stop a cancelling attempt's lane and gate groups, verify both, then mark it cancelled. */
  async #fence(row: Attempt): Promise<void> {
    if (row.run_id !== null && row.run_dir !== null && runProcessAlive(row.run_dir, row.task_id, 1))
      await cancelConfiguredRun(row.run_id, this.#chair, `graph ${this.graphId} cancelled`, this.#env).catch(() => undefined);
    if (!(await this.#groupsExited(row))) return;
    const deadline = Date.now() + 60_000;
    while (row.run_dir !== null && runProcessAlive(row.run_dir, row.task_id, 1)) {
      if (Date.now() > deadline) return; // still fenced: the next tick tries again
      await sleep(250);
    }
    this.#store.releaseSlot(row);
    this.#settle({ ...row, holds_slot: false }, "cancelled", { ...(row.outcome ?? {}), reason: "cancelled" });
  }

  /** End the graph once every node is terminal and nothing it started can still run. */
  async #finishIfDone(): Promise<boolean> {
    if (this.#inflight.size) return false;
    const current = this.#current();
    if ([...current.values()].some((row) => !TERMINAL.includes(row.state) || row.holds_slot)) return false;
    for (const node of this.#nodes.values())
      if (node.worktree && this.#store.reservation(node.worktree.path)?.graph_id === this.graphId) return false;
    const rows = [...current.values()];
    const state = this.#graph.cancel_requested ? "cancelled" : rows.every((row) => row.state === "passed") ? "done" : "stopped";
    for (const row of this.#store.attempts(this.project, this.graphId))
      if (row.kept && row.run_dir) rmSync(join(row.run_dir, "KEEP"), { force: true });
    this.#store.setGraph(this.project, this.graphId, { state }, this.#chair.agentId);
    const count = (name: AttemptState) => rows.filter((row) => row.state === name).length;
    const failed = rows.filter((row) => row.state === "failed").map((row) => row.node_id);
    this.#notify(state === "done" ? `graph ${this.graphId} done: ${rows.length} nodes passed`
      : state === "cancelled" ? `graph ${this.graphId} cancelled`
      : `graph ${this.graphId} stopped: ${failed.length ? `failed ${failed.join(", ")}` : "no node failed"}; ${count("passed")} passed, ${count("blocked")} blocked`);
    return true;
  }
}

/** `fabric graph-owner ID PROJECT`. */
export async function runGraphOwner(graphId: string, project: string, env: NodeJS.ProcessEnv = process.env): Promise<number> {
  if (!graphId || !project) return 2;
  return new Owner(project, graphId, env).run();
}
