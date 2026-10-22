/**
 * Graph scheduler state in the Fabric database. Every attempt transition is
 * one transaction guarded by its current state and writes one activity line,
 * so `fabric_events` and the console see the graph move.
 */
import Database from "better-sqlite3";
import { randomUUID } from "node:crypto";
import { readFileSync, rmSync, writeFileSync } from "node:fs";
import { join } from "node:path";

import { databasePath, type Identity } from "../identity.js";
import { Store } from "../store.js";
import { attemptTaskId, type GraphDocument, type GraphPlan } from "./document.js";

export const RESERVATION_FILE = "provenant-graph-reservation.json";
export const TERMINAL_GRAPH = ["done", "stopped", "cancelled"];
/** Attempt states that occupy graph width: a lane, gates or a fence may still be live. */
const ACTIVE = ["running", "gating", "cancelling"];

export type AttemptState =
  | "pending" | "running" | "gating" | "passed" | "failed" | "blocked"
  | "cancelling" | "cancelled" | "superseded" | "stopped";

export interface GateProc {
  index: number;
  token: string;
  argv_hash: string;
  pid?: number;
  pgid?: number;
  started_at?: string | null;
  exit?: { exit_code: number | null; signal: string | null };
}

export interface AttemptKey { project: string; graph_id: string; node_id: string; revision: number; attempt: number; restart: number }

export interface Attempt extends AttemptKey {
  state: AttemptState;
  task_id: string;
  run_id: string | null;
  run_dir: string | null;
  input_attempts: Record<string, number>;
  adapter: string;
  model: string;
  account: string;
  holds_slot: boolean;
  kept: boolean;
  gate_procs: GateProc[];
  outcome: Record<string, unknown> | null;
  updated_at: number;
}

export interface Graph {
  project: string;
  graph_id: string;
  revision: number;
  state: "running" | "done" | "stopped" | "cancelled";
  owner_pid: number | null;
  owner_started: string | null;
  chair: Identity;
  cancel_requested: boolean;
  created_at: number;
  updated_at: number;
}

export interface Reservation { path: string; gitDir: string; token: string; owner: Record<string, unknown> }

export type ReserveResult = "reserved" | "changed" | "width" | "pool" | "worktree";

type Row = Record<string, unknown>;

const key = (row: AttemptKey) => [row.project, row.graph_id, row.node_id, row.revision, row.attempt, row.restart];
const WHERE_KEY = "project = ? AND graph_id = ? AND node_id = ? AND revision = ? AND attempt = ? AND restart = ?";

function attempt(row: Row): Attempt {
  return {
    project: row.project as string, graph_id: row.graph_id as string, node_id: row.node_id as string,
    revision: row.revision as number, attempt: row.attempt as number, restart: row.restart as number,
    state: row.state as AttemptState, task_id: row.task_id as string,
    run_id: (row.run_id as string | null) ?? null, run_dir: (row.run_dir as string | null) ?? null,
    input_attempts: JSON.parse(row.input_attempts as string) as Record<string, number>,
    adapter: row.adapter as string, model: row.model as string, account: row.account as string,
    holds_slot: row.holds_slot === 1, kept: row.kept === 1,
    gate_procs: JSON.parse(row.gate_procs as string) as GateProc[],
    outcome: row.outcome === null ? null : JSON.parse(row.outcome as string) as Record<string, unknown>,
    updated_at: row.updated_at as number,
  };
}

function graph(row: Row): Graph {
  return {
    project: row.project as string, graph_id: row.graph_id as string, revision: row.revision as number,
    state: row.state as Graph["state"], owner_pid: (row.owner_pid as number | null) ?? null,
    owner_started: (row.owner_started as string | null) ?? null,
    chair: JSON.parse(row.chair_identity as string) as Identity, cancel_requested: row.cancel_requested === 1,
    created_at: row.created_at as number, updated_at: row.updated_at as number,
  };
}

export class GraphStore {
  readonly #db: Database.Database;

  constructor(env: NodeJS.ProcessEnv = process.env) {
    const path = databasePath(env);
    new Store(path).close(); // bootstraps the schema, graph tables included
    this.#db = new Database(path, { timeout: 5000 });
    this.#db.pragma("busy_timeout = 5000");
  }

  close(): void { this.#db.close(); }

  #log(project: string, agentId: string, detail: string): void {
    this.#db.prepare(`INSERT INTO activity(project, agent_id, at, kind, detail) VALUES (?, ?, ?, 'graph', ?)`)
      .run(project, agentId, Date.now(), detail);
  }

  /** Store revision 1 and one pending first attempt per node. */
  create(chair: Identity, document: GraphDocument, plan: GraphPlan): Graph {
    const now = Date.now();
    this.#db.transaction(() => {
      this.#db.prepare(`INSERT INTO graphs(project, graph_id, revision, state, chair_identity, created_at, updated_at)
        VALUES (?, ?, 1, 'running', ?, ?, ?)`).run(chair.project, plan.graph_id, JSON.stringify(chair), now, now);
      this.#db.prepare(`INSERT INTO graph_revisions(project, graph_id, revision, document_json, origin, created_at)
        VALUES (?, ?, 1, ?, ?, ?)`).run(chair.project, plan.graph_id, JSON.stringify(document),
        document.origin === undefined ? null : JSON.stringify(document.origin), now);
      const insert = this.#db.prepare(`INSERT INTO graph_attempts(project, graph_id, node_id, revision, attempt, restart,
        state, task_id, adapter, model, account, updated_at) VALUES (?, ?, ?, 1, 1, 0, 'pending', ?, ?, ?, ?, ?)`);
      for (const node of plan.nodes)
        insert.run(chair.project, plan.graph_id, node.id, attemptTaskId({ project: chair.project, graph_id: plan.graph_id,
          node_id: node.id, revision: 1, attempt: 1, restart: 0 }), node.pool.adapter, node.pool.model, node.pool.account, now);
      this.#log(chair.project, chair.agentId, `${plan.graph_id} created: ${plan.nodes.length} nodes`);
    }).immediate();
    return this.graph(chair.project, plan.graph_id)!;
  }

  graph(project: string, graphId: string): Graph | undefined {
    const row = this.#db.prepare(`SELECT * FROM graphs WHERE project = ? AND graph_id = ?`).get(project, graphId) as Row | undefined;
    return row && graph(row);
  }

  graphs(project: string, limit = 20): Graph[] {
    return (this.#db.prepare(`SELECT * FROM graphs WHERE project = ? ORDER BY created_at DESC LIMIT ?`)
      .all(project, limit) as Row[]).map(graph);
  }

  document(project: string, graphId: string, revision: number): GraphDocument {
    const row = this.#db.prepare(`SELECT document_json FROM graph_revisions WHERE project = ? AND graph_id = ? AND revision = ?`)
      .get(project, graphId, revision) as { document_json: string } | undefined;
    if (!row) throw new Error(`graph ${graphId} has no revision ${revision}`);
    return JSON.parse(row.document_json) as GraphDocument;
  }

  /** Every attempt of a graph, oldest first per node. */
  attempts(project: string, graphId: string): Attempt[] {
    return (this.#db.prepare(`SELECT * FROM graph_attempts WHERE project = ? AND graph_id = ?
      ORDER BY node_id, revision, attempt, restart`).all(project, graphId) as Row[]).map(attempt);
  }

  /** Each node's current attempt: its latest one that has not been superseded. */
  current(project: string, graphId: string): Map<string, Attempt> {
    const current = new Map<string, Attempt>();
    for (const row of this.attempts(project, graphId)) if (row.state !== "superseded") current.set(row.node_id, row);
    return current;
  }

  /** Claim the graph for this owner unless another live owner holds it. */
  claimOwner(project: string, graphId: string, pid: number, started: string | null, alive: (pid: number, started: string | null) => boolean): boolean {
    return this.#db.transaction(() => {
      const row = this.graph(project, graphId);
      if (!row) return false;
      if (row.owner_pid !== null && !(row.owner_pid === pid && row.owner_started === started) && alive(row.owner_pid, row.owner_started))
        return false;
      this.#db.prepare(`UPDATE graphs SET owner_pid = ?, owner_started = ?, updated_at = ? WHERE project = ? AND graph_id = ?`)
        .run(pid, started, Date.now(), project, graphId);
      return true;
    }).immediate();
  }

  /** Whether this process still owns the graph. */
  owns(project: string, graphId: string, pid: number, started: string | null): boolean {
    const row = this.graph(project, graphId);
    return row?.owner_pid === pid && row.owner_started === started;
  }

  setGraph(project: string, graphId: string, patch: { state?: Graph["state"]; cancel_requested?: boolean }, agentId: string): void {
    this.#db.transaction(() => {
      if (patch.state !== undefined)
        this.#db.prepare(`UPDATE graphs SET state = ?, updated_at = ? WHERE project = ? AND graph_id = ?`)
          .run(patch.state, Date.now(), project, graphId);
      if (patch.cancel_requested !== undefined)
        this.#db.prepare(`UPDATE graphs SET cancel_requested = ?, updated_at = ? WHERE project = ? AND graph_id = ?`)
          .run(patch.cancel_requested ? 1 : 0, Date.now(), project, graphId);
      this.#log(project, agentId, `${graphId} ${patch.state ?? (patch.cancel_requested ? "cancel requested" : "updated")}`);
    }).immediate();
  }

  /**
   * Move an attempt from one of `from` to `to`, applying `patch`. Returns false
   * when the attempt was no longer in an expected state, so a stale observer
   * never overwrites a newer decision.
   */
  transition(row: AttemptKey, from: AttemptState[], to: AttemptState, patch: Partial<Pick<Attempt,
    "run_id" | "run_dir" | "holds_slot" | "kept" | "gate_procs" | "outcome" | "input_attempts">> = {}, agentId = "graph"): boolean {
    return this.#db.transaction(() => {
      const columns = ["state = ?", "updated_at = ?"], values: unknown[] = [to, Date.now()];
      for (const [name, value] of Object.entries(patch)) {
        columns.push(`${name} = ?`);
        values.push(typeof value === "boolean" ? (value ? 1 : 0) : value !== null && typeof value === "object" ? JSON.stringify(value) : value);
      }
      const changed = this.#db.prepare(`UPDATE graph_attempts SET ${columns.join(", ")} WHERE ${WHERE_KEY}
        AND state IN (${from.map(() => "?").join(", ")})`).run(...values, ...key(row), ...from);
      if (changed.changes !== 1) return false;
      if (to !== from[0] || from.length > 1) this.#log(row.project, agentId, `${row.graph_id} ${row.node_id} a${row.attempt}: ${to}`);
      return true;
    }).immediate();
  }

  /** Every run id a graph attempt has launched, across projects: these lanes are not hand-made. */
  runIds(): string[] {
    return (this.#db.prepare(`SELECT run_id FROM graph_attempts WHERE run_id IS NOT NULL`).all() as Array<{ run_id: string }>).map((row) => row.run_id);
  }

  /** Record the lane a launch created, before its owner starts; refused unless this attempt is running unlaunched. */
  recordLaunch(row: AttemptKey, runId: string, runDir: string): void {
    const changed = this.#db.prepare(`UPDATE graph_attempts SET run_id = ?, run_dir = ?, kept = 1, updated_at = ?
      WHERE ${WHERE_KEY} AND state = 'running' AND run_id IS NULL`).run(runId, runDir, Date.now(), ...key(row));
    if (changed.changes !== 1) throw new Error(`graph attempt ${row.graph_id} ${row.node_id} a${row.attempt} is no longer launchable`);
  }

  setGates(row: AttemptKey, gates: GateProc[]): void {
    this.#db.prepare(`UPDATE graph_attempts SET gate_procs = ?, updated_at = ? WHERE ${WHERE_KEY}`)
      .run(JSON.stringify(gates), Date.now(), ...key(row));
  }

  releaseSlot(row: AttemptKey): void {
    this.#db.prepare(`UPDATE graph_attempts SET holds_slot = 0, updated_at = ? WHERE ${WHERE_KEY}`).run(Date.now(), ...key(row));
  }

  /** A fresh attempt row after the previous one is superseded (a launch that never started its owner). */
  addAttempt(row: AttemptKey, pool: { adapter: string; model: string; account: string }): AttemptKey {
    const next = { ...row, attempt: row.attempt + 1 };
    this.#db.prepare(`INSERT INTO graph_attempts(project, graph_id, node_id, revision, attempt, restart, state, task_id,
      adapter, model, account, updated_at) VALUES (?, ?, ?, ?, ?, ?, 'pending', ?, ?, ?, ?, ?)`)
      .run(...key(next), attemptTaskId(next), pool.adapter, pool.model, pool.account, Date.now());
    return next;
  }

  /** Slot holders whose graph has no live owner: the caller frees those whose lane has ended. */
  orphanSlots(alive: (pid: number, started: string | null) => boolean): Attempt[] {
    const rows = (this.#db.prepare(`SELECT a.*, g.owner_pid AS g_pid, g.owner_started AS g_started FROM graph_attempts a
      JOIN graphs g ON g.project = a.project AND g.graph_id = a.graph_id WHERE a.holds_slot = 1`).all() as Row[]);
    return rows.filter((row) => row.g_pid === null || !alive(row.g_pid as number, (row.g_started as string | null) ?? null)).map(attempt);
  }

  /**
   * In one immediate transaction: check the attempt is still pending in a
   * running graph, the graph's width and the pool's cap, and for a writer take
   * the worktree reservation; then mark it running and holding a slot.
   */
  reserve(row: AttemptKey, limits: { maxWidth: number; cap: number; handOccupancy: number; writer?: Reservation }, agentId: string): ReserveResult {
    return this.#db.transaction((): ReserveResult => {
      const current = this.#db.prepare(`SELECT a.state, a.adapter, a.model, a.account, g.state AS graph_state, g.cancel_requested
        FROM graph_attempts a JOIN graphs g ON g.project = a.project AND g.graph_id = a.graph_id WHERE ${WHERE_KEY.replace(/(\w+) = \?/gu, "a.$1 = ?")}`)
        .get(...key(row)) as Row | undefined;
      if (!current || current.state !== "pending" || current.graph_state !== "running" || current.cancel_requested === 1) return "changed";
      const width = this.#db.prepare(`SELECT COUNT(*) AS n FROM graph_attempts WHERE project = ? AND graph_id = ?
        AND state IN (${ACTIVE.map(() => "?").join(", ")})`).get(row.project, row.graph_id, ...ACTIVE) as { n: number };
      if (width.n >= limits.maxWidth) return "width";
      const pool = this.#db.prepare(`SELECT COUNT(*) AS n FROM graph_attempts WHERE adapter = ? AND model = ? AND account = ? AND holds_slot = 1`)
        .get(current.adapter, current.model, current.account) as { n: number };
      if (pool.n + limits.handOccupancy >= limits.cap) return "pool";
      if (limits.writer && !this.#reserveWorktree(row, limits.writer)) return "worktree";
      this.#db.prepare(`UPDATE graph_attempts SET state = 'running', holds_slot = 1, updated_at = ? WHERE ${WHERE_KEY}`)
        .run(Date.now(), ...key(row));
      this.#log(row.project, agentId, `${row.graph_id} ${row.node_id} a${row.attempt}: running`);
      return "reserved";
    }).immediate();
  }

  #reserveWorktree(row: AttemptKey, writer: Reservation): boolean {
    const existing = this.#db.prepare(`SELECT r.*, g.state AS graph_state FROM graph_worktree_reservations r
      LEFT JOIN graphs g ON g.project = r.project AND g.graph_id = r.graph_id WHERE r.realpath = ?`).get(writer.path) as Row | undefined;
    // Writers of one graph sharing a worktree are totally ordered, so the graph holds it across them.
    const ours = existing && existing.graph_id === row.graph_id && existing.project === row.project;
    if (existing && !ours && existing.graph_state !== null && !TERMINAL_GRAPH.includes(existing.graph_state as string)) return false;
    const file = join(writer.gitDir, RESERVATION_FILE);
    let onDisk: Row | undefined;
    try { onDisk = JSON.parse(readFileSync(file, "utf8")) as Row; } catch { onDisk = undefined; }
    if (onDisk && onDisk.token !== existing?.token) {
      // A file with no live row is stale; one whose row is live but another graph's is a conflict.
      const live = this.#db.prepare(`SELECT 1 FROM graph_worktree_reservations r JOIN graphs g ON g.project = r.project AND g.graph_id = r.graph_id
        WHERE r.token = ? AND g.state = 'running'`).get(onDisk.token);
      if (live) return false;
    }
    const token = ours ? existing.token as string : writer.token;
    this.#db.prepare(`INSERT OR REPLACE INTO graph_worktree_reservations(realpath, project, graph_id, node_id, revision, attempt, token, created_at)
      VALUES (?, ?, ?, ?, ?, ?, ?, ?)`).run(writer.path, row.project, row.graph_id, row.node_id, row.revision, row.attempt, token, Date.now());
    rmSync(file, { force: true });
    writeFileSync(file, `${JSON.stringify({ schema: "provenant.graph-reservation.v1", graph_id: row.graph_id, node_id: row.node_id,
      revision: row.revision, attempt: row.attempt, token, owner: writer.owner })}\n`, { flag: "wx", mode: 0o600 });
    return true;
  }

  /** The reservation token a writer attempt holds, if any. */
  reservation(path: string): { token: string; graph_id: string; node_id: string; project: string } | undefined {
    return this.#db.prepare(`SELECT token, graph_id, node_id, project FROM graph_worktree_reservations WHERE realpath = ?`).get(path) as
      { token: string; graph_id: string; node_id: string; project: string } | undefined;
  }

  /** Drop a reservation this graph holds: the file first, then the row, so a refusal never outlives neither. */
  releaseReservation(path: string, gitDir: string, graphId: string): void {
    this.#db.transaction(() => {
      const row = this.reservation(path);
      if (row?.graph_id !== graphId) return;
      const file = join(gitDir, RESERVATION_FILE);
      try {
        const onDisk = JSON.parse(readFileSync(file, "utf8")) as Row;
        if (onDisk.token === row.token) rmSync(file, { force: true });
      } catch { /* already gone */ }
      this.#db.prepare(`DELETE FROM graph_worktree_reservations WHERE realpath = ? AND graph_id = ?`).run(path, graphId);
    }).immediate();
  }

  notice(project: string, agentId: string, detail: string): void { this.#log(project, agentId, detail); }
}

export const newToken = (): string => randomUUID();
