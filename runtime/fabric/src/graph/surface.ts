/** `graph run | status | cancel | resume`, shared by the CLI and the `fabric_graph` tool. */
import { spawn } from "node:child_process";
import { mkdirSync, openSync, readFileSync } from "node:fs";
import { join, resolve } from "node:path";

import { catalogueSnapshot } from "../catalogue.js";
import { productRoot } from "../execution.js";
import { databasePath, runRoot, type Identity } from "../identity.js";
import { observedAlive } from "../run-registry.js";
import { Store } from "../store.js";
import { faultLines, newGraphId, validateGraph, type GraphDocument } from "./document.js";
import { fabricCommand } from "./gates.js";
import { attemptDirectory } from "./owner.js";
import { GraphStore, TERMINAL_GRAPH, type Attempt, type Graph } from "./store.js";

const MAX_LINES = 25;
type Result = Record<string, unknown>;

const ownerAlive = (graph: Graph) => graph.owner_pid !== null && observedAlive(graph.owner_pid, graph.owner_started);

/** Start the detached owner; it outlives this call and claims the graph itself. */
export function startGraphOwner(graph: Graph, env: NodeJS.ProcessEnv): void {
  const dir = join(runRoot(graph.chair.cwd, env), "graphs", graph.graph_id);
  mkdirSync(dir, { recursive: true });
  const log = openSync(join(dir, "owner.log"), "a", 0o600);
  const { command, args } = fabricCommand(["graph-owner", graph.graph_id, graph.project], env);
  const child = spawn(command, args, { cwd: graph.chair.cwd, env, detached: true, stdio: ["ignore", log, log] });
  child.unref();
}

function line(row: Attempt): string {
  const reason = typeof row.outcome?.reason === "string" ? ` · ${row.outcome.reason}` : "";
  return `  ${row.state.padEnd(10)} ${row.node_id}  ${row.adapter}/${row.model}${row.run_id ? `  ${row.run_id}` : ""}${reason}`;
}

/** The `fabric.graph.v1` view and its digest: one line per node that is running, blocked or stopped. */
export function graphView(store: GraphStore, graph: Graph, env: NodeJS.ProcessEnv): Result {
  const current = [...store.current(graph.project, graph.graph_id).values()];
  const alive = ownerAlive(graph);
  const counts = new Map<string, number>();
  for (const row of current) counts.set(row.state, (counts.get(row.state) ?? 0) + 1);
  const summary = [...counts].map(([state, count]) => `${count} ${state}`).join(", ");
  const shown = current.filter((row) => !["passed", "pending"].includes(row.state));
  // A just-started owner has not claimed the graph yet; that is not a death.
  const dead = graph.state === "running" && !alive && !(graph.owner_pid === null && Date.now() - graph.created_at < 30_000);
  const lines = [`graph ${graph.graph_id} ${graph.state} · ${current.length} nodes: ${summary}`,
    ...shown.slice(0, MAX_LINES - 3).map(line),
    ...(shown.length > MAX_LINES - 3 ? [`  ${shown.length - (MAX_LINES - 3)} more; use --json`] : []),
    ...(dead ? [`owner died: provenant graph resume ${graph.graph_id}`] : [])];
  return {
    schema: "fabric.graph.v1", status: "ok", graph_id: graph.graph_id, graph_state: graph.state, revision: graph.revision,
    owner_alive: alive, ...(dead ? { fix: `provenant graph resume ${graph.graph_id}` } : {}),
    nodes: current.map((row) => ({ node_id: row.node_id, state: row.state, attempt: row.attempt, task_id: row.task_id,
      run_id: row.run_id, input_attempts: row.input_attempts, receipt: join(attemptDirectory(graph.chair, row, env), "receipt.json"),
      ...(row.outcome?.reason ? { reason: row.outcome.reason } : {}) })),
    digest: lines.join("\n"),
  };
}

export function graphRun(input: { file?: string; document?: unknown }, chair: Identity, env: NodeJS.ProcessEnv = process.env): Result {
  let document: Record<string, unknown>;
  try {
    const raw = input.document ?? JSON.parse(readFileSync(resolve(chair.cwd, input.file ?? ""), "utf8"));
    if (typeof raw !== "object" || raw === null || Array.isArray(raw)) throw new Error("not an object");
    document = { ...raw as Record<string, unknown> };
  } catch (error) {
    return { status: "rejected", error: "graph_unreadable", fix: `Pass --file with one JSON graph document (${error instanceof Error ? error.message : String(error)}).` };
  }
  document.graph_id ??= newGraphId();
  document.revision ??= 1;
  const checked = validateGraph(document, chair, catalogueSnapshot(productRoot(env), env), env);
  if (!checked.plan) return { status: "rejected", error: "graph_invalid", fix: faultLines(checked.faults), faults: checked.faults };
  const store = new GraphStore(env);
  try {
    if (store.graph(chair.project, checked.plan.graph_id))
      return { status: "rejected", error: "graph_exists", fix: `Graph ${checked.plan.graph_id} exists; omit graph_id or pass a new one.` };
    const graph = store.create(chair, document as unknown as GraphDocument, checked.plan);
    const tasks = new Store(databasePath(env));
    try {
      for (const node of checked.plan.nodes)
        tasks.createTask(chair, `graph ${graph.graph_id} ${node.id}`, { taskId: `${graph.graph_id}.${node.id}`,
          dependsOn: node.depends_on.map((parent) => `${graph.graph_id}.${parent}`) });
    } finally { tasks.close(); }
    startGraphOwner(graph, env);
    return graphView(store, graph, env);
  } finally { store.close(); }
}

export async function graphStatus(id: string | undefined, chair: Identity, env: NodeJS.ProcessEnv = process.env,
  options: { waitSeconds?: number; resume?: boolean; signal?: AbortSignal } = {}): Promise<Result> {
  const store = new GraphStore(env);
  try {
    if (id === undefined) {
      const graphs = store.graphs(chair.project);
      return { schema: "fabric.graphs.v1", status: "ok", graphs: graphs.map((graph) => ({ graph_id: graph.graph_id, graph_state: graph.state, owner_alive: ownerAlive(graph) })),
        digest: graphs.length ? graphs.map((graph) => `${graph.state.padEnd(9)} ${graph.graph_id}${graph.state === "running" && !ownerAlive(graph) ? "  owner died: provenant graph resume " + graph.graph_id : ""}`).join("\n") : "no graphs" };
    }
    let graph = store.graph(chair.project, id);
    if (!graph) return { status: "rejected", error: "graph_unknown", fix: `No graph ${id} in ${chair.project}; run provenant graph status to list graphs.` };
    if (options.resume && graph.state === "running" && !ownerAlive(graph)) startGraphOwner(graph, env);
    const deadline = Date.now() + (options.waitSeconds ?? 0) * 1000;
    while (!TERMINAL_GRAPH.includes(graph.state) && Date.now() < deadline && !options.signal?.aborted) {
      await new Promise((resolveWait) => setTimeout(resolveWait, 1000));
      graph = store.graph(chair.project, id)!;
    }
    return { ...graphView(store, graph, env), ...(options.waitSeconds && !TERMINAL_GRAPH.includes(graph.state) ? { timed_out: true } : {}) };
  } finally { store.close(); }
}

export function graphCancel(id: string, node: string | undefined, chair: Identity, env: NodeJS.ProcessEnv = process.env): Result {
  const store = new GraphStore(env);
  try {
    const graph = store.graph(chair.project, id);
    if (!graph) return { status: "rejected", error: "graph_unknown", fix: `No graph ${id} in ${chair.project}.` };
    if (TERMINAL_GRAPH.includes(graph.state)) return graphView(store, graph, env);
    if (node === undefined) store.setGraph(chair.project, id, { cancel_requested: true }, chair.agentId);
    else {
      const row = store.current(chair.project, id).get(node);
      if (!row) return { status: "rejected", error: "node_unknown", fix: `Graph ${id} has no node ${node}.` };
      if (row.state === "pending") store.transition(row, ["pending"], "cancelled", {}, chair.agentId);
      else if (["running", "gating"].includes(row.state)) store.transition(row, [row.state], "cancelling", {}, chair.agentId);
    }
    if (!ownerAlive(graph)) startGraphOwner(graph, env);
    return graphView(store, store.graph(chair.project, id)!, env);
  } finally { store.close(); }
}

export function graphResume(id: string, chair: Identity, env: NodeJS.ProcessEnv = process.env): Result {
  const store = new GraphStore(env);
  try {
    const graph = store.graph(chair.project, id);
    if (!graph) return { status: "rejected", error: "graph_unknown", fix: `No graph ${id} in ${chair.project}.` };
    if (graph.state === "running" && !ownerAlive(graph)) startGraphOwner(graph, env);
    return graphView(store, graph, env);
  } finally { store.close(); }
}

/** One stderr line for `lanes --wait` when a running graph in this project has lost its owner. */
export function deadGraphHint(chair: Identity, env: NodeJS.ProcessEnv = process.env): string | undefined {
  try {
    const store = new GraphStore(env);
    try {
      const dead = store.graphs(chair.project, 50).filter((graph) => graph.state === "running" && !ownerAlive(graph));
      return dead.length ? dead.map((graph) => `graph ${graph.graph_id} owner died: provenant graph resume ${graph.graph_id}`).join("\n") : undefined;
    } finally { store.close(); }
  } catch { return undefined; }
}
