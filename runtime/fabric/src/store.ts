import Database from "better-sqlite3";
import { randomUUID } from "node:crypto";
import { copyFileSync, mkdirSync, mkdtempSync, readFileSync, rmSync, statSync } from "node:fs";
import { tmpdir } from "node:os";
import { dirname, resolve } from "node:path";
import { fileURLToPath } from "node:url";

import type { Identity } from "./identity.js";

const SCHEMA = resolve(dirname(fileURLToPath(import.meta.url)), "../schema.sql");
const REQUIRED_TABLES = [
  "agents", "messages", "deliveries", "delivery_claims", "teams", "team_members",
  "tasks", "task_dependencies", "activity",
];
const REQUIRED_CLAIM_COLUMNS = [
  "message_id", "project", "recipient_id", "claim_id", "claimed_at", "expires_at",
];
const BOOTSTRAP_WAIT = new Int32Array(new SharedArrayBuffer(4));

export const isSQLiteContention = (error: unknown): boolean => {
  const code = (error as { code?: unknown } | null)?.code;
  if (code === "SQLITE_BUSY" || code === "SQLITE_LOCKED") return true;
  const cause = (error as { cause?: unknown } | null)?.cause;
  return cause !== undefined && cause !== error && isSQLiteContention(cause);
};

export interface Message {
  messageId: string;
  from: string;
  body: string;
  kind: string;
  conversationId: string;
  replyTo: string | null;
  at: string;
  claimId: string | null;
  claimExpiresAt: string | null;
  taskId?: string;
  outputPath?: string;
}

export interface Acknowledgement {
  messageId: string;
  acknowledgedAt: string;
  alreadyAcknowledged: boolean;
}

export interface Task {
  taskId: string;
  objective: string;
  owner: string | null;
  state: string;
  dependsOn: string[];
}

export interface Activity {
  seq: number;
  at: string;
  agentId: string;
  kind: string;
  detail: string;
}

/**
 * Every operation is scoped to one project and one caller, both supplied by the
 * process rather than by the call, so no tool argument can widen its own reach.
 *
 * Concurrency is SQLite's problem, not ours: WAL lets any number of agent
 * processes read and write this file at once. That is the whole reason the
 * daemon could go.
 */
export class Store {
  readonly #db: Database.Database;

  constructor(path: string, busyTimeoutMs = 5000) {
    if (!Number.isSafeInteger(busyTimeoutMs) || busyTimeoutMs < 1 || busyTimeoutMs > 5000) {
      throw new Error("busy timeout must be between 1 and 5000 milliseconds");
    }
    mkdirSync(dirname(path), { recursive: true, mode: 0o700 });
    this.#db = new Database(path, { timeout: busyTimeoutMs });
    try {
      // Migration/schema PRAGMAs also contend on the first simultaneous open.
      // Install the wait policy before executing any of them, then retry only
      // the idempotent schema bootstrap on bounded SQLite contention.
      this.#db.pragma(`busy_timeout = ${busyTimeoutMs}`);
      const schema = readFileSync(SCHEMA, "utf8");
      const deadline = Date.now() + busyTimeoutMs;
      for (;;) {
        try {
          this.#db.exec(schema);
          this.#ensureMessageLinkColumns();
          break;
        } catch (error) {
          if (!isSQLiteContention(error) || Date.now() >= deadline) throw error;
          Atomics.wait(BOOTSTRAP_WAIT, 0, 0, 25);
        }
      }
    } catch (error) {
      this.#db.close();
      throw error;
    }
  }

  close(): void {
    this.#db.close();
  }

  /** Leave a lazily opened store on the normal operation timeout. */
  restoreDefaultBusyTimeout(): void {
    this.#db.pragma("busy_timeout = 5000");
  }

  /** Register the caller if this is the first time it has been seen. */
  announce(who: Identity): void {
    if (who.agentId === "all") throw new Error('recipient id "all" is reserved for broadcast');
    const now = Date.now();
    this.#db.transaction(() => {
      const existing = this.#db
        .prepare(`SELECT provider FROM agents WHERE project = ? AND agent_id = ?`)
        .get(who.project, who.agentId) as { provider: string } | undefined;
      if (existing === undefined) {
        const reservedByTeam = this.#db
          .prepare(`SELECT 1 FROM teams WHERE project = ? AND team_id = ?`)
          .get(who.project, who.agentId);
        if (reservedByTeam !== undefined) {
          throw new Error(
            `agent label ${who.agentId} collides with an existing team id in ${who.project}`,
          );
        }
      }
      const announced = this.#db
        .prepare(
          `INSERT INTO agents(project, agent_id, provider, first_seen, last_seen)
           VALUES (?, ?, ?, ?, ?)
           ON CONFLICT(project, agent_id) DO UPDATE SET last_seen = excluded.last_seen
           WHERE agents.provider = excluded.provider`,
        )
        .run(who.project, who.agentId, who.provider, now, now);
      if (announced.changes === 0) {
        throw new Error(
          `agent label ${who.agentId} in ${who.project} already belongs to client seat ` +
            `${existing?.provider ?? "unknown"}; refusing seat ${who.provider}`,
        );
      }
    }).immediate();
  }

  agents(project: string): Array<{ agentId: string; provider: string; lastSeen: string }> {
    return this.#db
      .prepare(
        `SELECT agent_id, provider, last_seen FROM agents
         WHERE project = ? ORDER BY last_seen DESC`,
      )
      .all(project)
      .map((row) => {
        const record = row as { agent_id: string; provider: string; last_seen: number };
        return {
          agentId: record.agent_id,
          provider: record.provider,
          lastSeen: new Date(record.last_seen).toISOString(),
        };
      });
  }

  /**
   * Send to one agent, a team, or everyone else in the project.
   *
   * Recipients are resolved at send time and written as delivery rows, so an
   * agent that joins later does not retroactively receive old broadcasts.
   */
  send(
    who: Identity,
    to: string,
    body: string,
    options: { kind?: string; replyTo?: string; taskId?: string; outputPath?: string } = {},
  ): { messageId: string; recipients: string[] } {
    if (options.taskId !== undefined && options.taskId.length === 0) {
      throw new Error("task id must not be empty");
    }
    if (options.outputPath !== undefined && options.outputPath.length === 0) {
      throw new Error("output path must not be empty");
    }
    const messageId = randomUUID();
    const now = Date.now();

    return this.#db.transaction(() => {
      const recipients = this.#resolveRecipients(who, to);
      if (recipients.length === 0) {
        throw new Error(
          `no recipients for "${to}" in ${who.project}. ` +
            `Use an agent label, team id, or "all". Known agents: ` +
            `${this.agents(who.project).map((agent) => agent.agentId).join(", ") || "none yet"}`,
        );
      }
      const parentConversation = options.replyTo === undefined
        ? undefined
        : this.#conversationOf(who.project, options.replyTo);
      if (options.replyTo !== undefined && parentConversation === undefined) {
        throw new Error(`reply parent ${options.replyTo} does not exist in project ${who.project}`);
      }
      if (options.taskId !== undefined) {
        const task = this.#db
          .prepare(`SELECT 1 FROM tasks WHERE project = ? AND task_id = ?`)
          .get(who.project, options.taskId);
        if (task === undefined) {
          throw new Error(`task ${options.taskId} does not exist in project ${who.project}`);
        }
      }
      const conversationId = parentConversation ?? messageId;
      const replyTo = options.replyTo ?? null;
      this.#db
        .prepare(
          `INSERT INTO messages(
             message_id, project, sender_id, body, kind, conversation_id, reply_to,
             task_id, output_path, created_at
           ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?)`,
        )
        .run(
          messageId,
          who.project,
          who.agentId,
          body,
          options.kind ?? "note",
          conversationId,
          replyTo,
          options.taskId ?? null,
          options.outputPath ?? null,
          now,
        );
      const delivery = this.#db.prepare(
        `INSERT INTO deliveries(message_id, project, recipient_id, read_at) VALUES (?, ?, ?, NULL)`,
      );
      for (const recipient of recipients) delivery.run(messageId, who.project, recipient);
      this.#log(who, "send", `to ${to}: ${body.slice(0, 120)}`);
      return { messageId, recipients };
    }).immediate();
  }

  /** Claim unacknowledged messages for the caller. Peeking never creates a claim. */
  inbox(
    who: Identity,
    options: {
      limit?: number;
      peek?: boolean;
      claimTtlMs?: number;
      busyTimeoutMs?: number;
      taskId?: string;
    } = {},
  ): Message[] {
    const limit = options.limit ?? 20;
    if (!Number.isSafeInteger(limit) || limit <= 0) throw new Error("inbox limit must be positive");
    if (options.taskId !== undefined && options.taskId.length === 0) {
      throw new Error("task id must not be empty");
    }
    const claimTtlMs = options.claimTtlMs ?? 300_000;
    if (!Number.isSafeInteger(claimTtlMs) || claimTtlMs <= 0 || claimTtlMs > 3_600_000) {
      throw new Error("claim TTL must be between 1 and 3600000 milliseconds");
    }

    if (options.peek ?? false) {
      const observedAt = Date.now();
      return this.#deliveryRows(who, limit, observedAt, true, options.taskId).map((row) =>
        this.#message(row, null, row.claim_expires_at === null || row.claim_expires_at <= observedAt
          ? null
          : row.claim_expires_at));
    }

    const busyTimeoutMs = options.busyTimeoutMs ?? 5000;
    if (!Number.isSafeInteger(busyTimeoutMs) || busyTimeoutMs < 1 || busyTimeoutMs > 5000) {
      throw new Error("busy timeout must be between 1 and 5000 milliseconds");
    }
    this.#db.pragma(`busy_timeout = ${busyTimeoutMs}`);
    try {
      return this.#db.transaction(() => {
        // BEGIN IMMEDIATE has acquired the writer lock before this callback runs.
        // Starting the lease here prevents lock wait time consuming the claim TTL.
        const now = Date.now();
        const rows = this.#deliveryRows(who, limit, now, false, options.taskId);
        const claim = this.#db.prepare(
          `INSERT INTO delivery_claims(
             message_id, project, recipient_id, claim_id, claimed_at, expires_at
           ) VALUES (?, ?, ?, ?, ?, ?)
           ON CONFLICT(message_id, recipient_id) DO UPDATE SET
             project = excluded.project,
             claim_id = excluded.claim_id,
             claimed_at = excluded.claimed_at,
             expires_at = excluded.expires_at
           WHERE delivery_claims.expires_at <= ?`,
        );
        return rows.flatMap((row) => {
          const claimId = randomUUID();
          const expiresAt = now + claimTtlMs;
          const changed = claim.run(
            row.message_id,
            who.project,
            who.agentId,
            claimId,
            now,
            expiresAt,
            now,
          );
          return changed.changes === 1 ? [this.#message(row, claimId, expiresAt)] : [];
        });
      }).immediate();
    } finally {
      if (busyTimeoutMs !== 5000) this.#db.pragma("busy_timeout = 5000");
    }
  }

  /** Acknowledge one claimed delivery. Retries with the same token are idempotent. */
  acknowledge(who: Identity, messageId: string, claimId: string): Acknowledgement {
    return this.#db.transaction(() => {
      const row = this.#db
        .prepare(
          `SELECT d.read_at, c.claim_id, c.expires_at
           FROM deliveries d
           LEFT JOIN delivery_claims c
             ON c.message_id = d.message_id AND c.recipient_id = d.recipient_id
           WHERE d.project = ? AND d.recipient_id = ? AND d.message_id = ?`,
        )
        .get(who.project, who.agentId, messageId) as {
          read_at: number | null;
          claim_id: string | null;
          expires_at: number | null;
        } | undefined;
      if (row === undefined) {
        throw new Error(`no delivery ${messageId} for ${who.agentId} in ${who.project}`);
      }
      if (row.claim_id !== claimId) throw new Error(`claim ${claimId} does not own delivery ${messageId}`);
      if (row.read_at !== null) {
        return {
          messageId,
          acknowledgedAt: new Date(row.read_at).toISOString(),
          alreadyAcknowledged: true,
        };
      }
      const now = Date.now();
      if (row.expires_at === null || row.expires_at <= now) {
        throw new Error(`claim ${claimId} for delivery ${messageId} has expired`);
      }
      this.#db
        .prepare(
          `UPDATE deliveries SET read_at = ?
           WHERE project = ? AND recipient_id = ? AND message_id = ? AND read_at IS NULL`,
        )
        .run(now, who.project, who.agentId, messageId);
      this.#log(who, "ack", messageId);
      return {
        messageId,
        acknowledgedAt: new Date(now).toISOString(),
        alreadyAcknowledged: false,
      };
    }).immediate();
  }

  createTeam(who: Identity, teamId: string, members: string[]): { teamId: string; members: string[] } {
    if (teamId === "all" || members.includes("all")) {
      throw new Error('recipient id "all" is reserved for broadcast');
    }
    const now = Date.now();
    const effectiveMembers = [...new Set(members)];
    this.#db.transaction(() => {
      const existingTeam = this.#db
        .prepare(`SELECT 1 FROM teams WHERE project = ? AND team_id = ?`)
        .get(who.project, teamId);
      if (existingTeam === undefined) {
        const reservedByAgent = this.#db
          .prepare(`SELECT 1 FROM agents WHERE project = ? AND agent_id = ?`)
          .get(who.project, teamId);
        if (reservedByAgent !== undefined) {
          throw new Error(`team id ${teamId} collides with an agent label in ${who.project}`);
        }
      }
      this.#db
        .prepare(`INSERT OR IGNORE INTO teams(project, team_id, created_at) VALUES (?, ?, ?)`)
        .run(who.project, teamId, now);
      this.#db
        .prepare(`DELETE FROM team_members WHERE project = ? AND team_id = ?`)
        .run(who.project, teamId);
      const add = this.#db.prepare(
        `INSERT INTO team_members(project, team_id, agent_id) VALUES (?, ?, ?)`,
      );
      for (const member of effectiveMembers) add.run(who.project, teamId, member);
      this.#log(who, "team", `${teamId}: ${effectiveMembers.join(", ")}`);
    }).immediate();
    return { teamId, members: effectiveMembers };
  }

  createTask(
    who: Identity,
    objective: string,
    options: { taskId?: string; owner?: string; dependsOn?: string[] } = {},
  ): Task {
    const taskId = options.taskId ?? randomUUID().slice(0, 8);
    if (taskId.length === 0) throw new Error("task id must not be empty");
    const now = Date.now();
    this.#db.transaction(() => {
      this.#db
        .prepare(
          `INSERT INTO tasks(project, task_id, objective, owner_id, state, created_at, updated_at)
           VALUES (?, ?, ?, ?, 'open', ?, ?)`,
        )
        .run(who.project, taskId, objective, options.owner ?? null, now, now);
      const dependency = this.#db.prepare(
        `INSERT OR IGNORE INTO task_dependencies(project, task_id, depends_on) VALUES (?, ?, ?)`,
      );
      for (const parent of options.dependsOn ?? []) dependency.run(who.project, taskId, parent);
      this.#log(who, "task", `${taskId} open: ${objective.slice(0, 120)}`);
    })();
    return {
      taskId,
      objective,
      owner: options.owner ?? null,
      state: "open",
      dependsOn: options.dependsOn ?? [],
    };
  }

  updateTask(who: Identity, taskId: string, state: string, note?: string): Task {
    if (state === "claimed") throw new Error("state claimed is reserved for atomic task claim");
    return this.#db.transaction(() => {
      const changed = this.#db
        .prepare(`UPDATE tasks SET state = ?, updated_at = ? WHERE project = ? AND task_id = ?`)
        .run(state, Date.now(), who.project, taskId);
      if (changed.changes === 0) throw new Error(`no task ${taskId} in ${who.project}`);
      this.#log(who, "task", `${taskId} ${state}${note === undefined ? "" : `: ${note}`}`);
      return this.#task(who.project, taskId);
    })();
  }

  /** Claim one open, unowned task without changing its cooperative state. */
  claimTask(who: Identity, taskId: string): Task {
    return this.#db.transaction(() => {
      const claimed = this.#db
        .prepare(
          `UPDATE tasks SET owner_id = ?, updated_at = ?
           WHERE project = ? AND task_id = ? AND state = 'open' AND owner_id IS NULL`,
        )
        .run(who.agentId, Date.now(), who.project, taskId);
      if (claimed.changes !== 1) {
        const current = this.#task(who.project, taskId);
        if (current.state === "open" && current.owner === who.agentId) return current;
        if (current.state === "open" && current.owner !== null) {
          throw new Error(`task ${taskId} is already assigned to ${current.owner}`);
        }
        throw new Error(`task ${taskId} is not open and unowned in ${who.project}`);
      }
      this.#log(who, "task", `${taskId} claimed by ${who.agentId}`);
      return this.#task(who.project, taskId);
    }).immediate();
  }

  tasks(project: string, state?: string): Task[] {
    const rows = this.#db
      .prepare(
        `SELECT task_id, objective, owner_id, state FROM tasks
         WHERE project = ? AND (? IS NULL OR state = ?) ORDER BY created_at`,
      )
      .all(project, state ?? null, state ?? null) as Array<{
        task_id: string;
        objective: string;
        owner_id: string | null;
        state: string;
      }>;
    const dependency = this.#db.prepare(
      `SELECT depends_on FROM task_dependencies WHERE project = ? AND task_id = ?`,
    );
    return rows.map((row) => ({
      taskId: row.task_id,
      objective: row.objective,
      owner: row.owner_id,
      state: row.state,
      dependsOn: dependency
        .all(project, row.task_id)
        .map((entry) => (entry as { depends_on: string }).depends_on),
    }));
  }

  note(who: Identity, detail: string): void {
    this.#log(who, "note", detail);
  }

  activity(project: string, limit = 50): Activity[] {
    if (!Number.isSafeInteger(limit) || limit <= 0) throw new Error("activity limit must be positive");
    return this.#db
      .prepare(
        `SELECT seq, at, agent_id, kind, detail FROM activity
         WHERE project = ? ORDER BY seq DESC LIMIT ?`,
      )
      .all(project, limit)
      .map((row) => this.#activity(row as ActivityRow));
  }

  /** Activity committed after a cursor, oldest first, for lossless tailing. */
  activityAfter(project: string, afterSeq: number, limit = 200): Activity[] {
    if (!Number.isSafeInteger(afterSeq) || afterSeq < 0) {
      throw new Error("activity cursor must be a non-negative integer");
    }
    if (!Number.isSafeInteger(limit) || limit <= 0) throw new Error("activity limit must be positive");
    return this.#db
      .prepare(
        `SELECT seq, at, agent_id, kind, detail FROM activity
         WHERE project = ? AND seq > ? ORDER BY seq LIMIT ?`,
      )
      .all(project, afterSeq, limit)
      .map((row) => this.#activity(row as ActivityRow));
  }

  #log(who: Identity, kind: string, detail: string): void {
    this.#db
      .prepare(`INSERT INTO activity(project, agent_id, at, kind, detail) VALUES (?, ?, ?, ?, ?)`)
      .run(who.project, who.agentId, Date.now(), kind, detail);
  }

  #task(project: string, taskId: string): Task {
    const task = this.tasks(project).find((candidate) => candidate.taskId === taskId);
    if (task === undefined) throw new Error(`task ${taskId} vanished during update`);
    return task;
  }

  #deliveryRows(
    who: Identity,
    limit: number,
    now: number,
    includeClaimed: boolean,
    taskId?: string,
  ): DeliveryRow[] {
    return this.#db
      .prepare(
        `SELECT m.message_id, m.sender_id, m.body, m.kind, m.conversation_id,
                m.reply_to, m.task_id, m.output_path, m.created_at,
                c.expires_at AS claim_expires_at
         FROM deliveries d
         JOIN messages m ON m.message_id = d.message_id
         LEFT JOIN delivery_claims c
           ON c.message_id = d.message_id AND c.recipient_id = d.recipient_id
         WHERE d.project = ? AND d.recipient_id = ? AND d.read_at IS NULL
           AND (? = 1 OR c.expires_at IS NULL OR c.expires_at <= ?)
           AND (? IS NULL OR m.task_id = ?)
         ORDER BY m.created_at, m.message_id LIMIT ?`,
      )
      .all(
        who.project,
        who.agentId,
        includeClaimed ? 1 : 0,
        now,
        taskId ?? null,
        taskId ?? null,
        limit,
      ) as DeliveryRow[];
  }

  #message(row: DeliveryRow, claimId: string | null, expiresAt: number | null): Message {
    const message: Message = {
      messageId: row.message_id,
      from: row.sender_id,
      body: row.body,
      kind: row.kind,
      conversationId: row.conversation_id,
      replyTo: row.reply_to,
      at: new Date(row.created_at).toISOString(),
      claimId,
      claimExpiresAt: expiresAt === null ? null : new Date(expiresAt).toISOString(),
    };
    if (row.task_id !== null) message.taskId = row.task_id;
    if (row.output_path !== null) message.outputPath = row.output_path;
    return message;
  }

  #ensureMessageLinkColumns(): void {
    const columns = new Set(
      (this.#db.pragma("table_info(messages)") as Array<{ name: string }>).map((column) => column.name),
    );
    // Additive nullable fields keep old databases and rows intact. A duplicate
    // column is harmless if two first openers race this migration.
    for (const [name, definition] of [["task_id", "TEXT"], ["output_path", "TEXT"]] as const) {
      if (columns.has(name)) continue;
      try {
        this.#db.exec(`ALTER TABLE messages ADD COLUMN ${name} ${definition}`);
      } catch (error) {
        if (!(error instanceof Error) || !/duplicate column name/i.test(error.message)) throw error;
      }
    }
  }

  #activity(row: ActivityRow): Activity {
    return {
      seq: row.seq,
      at: new Date(row.at).toISOString(),
      agentId: row.agent_id,
      kind: row.kind,
      detail: row.detail,
    };
  }

  #conversationOf(project: string, messageId: string): string | undefined {
    const row = this.#db
      .prepare(`SELECT conversation_id FROM messages WHERE project = ? AND message_id = ?`)
      .get(project, messageId) as { conversation_id: string } | undefined;
    return row?.conversation_id;
  }

  #resolveRecipients(who: Identity, to: string): string[] {
    if (to === "all") {
      return this.agents(who.project)
        .map((agent) => agent.agentId)
        .filter((agentId) => agentId !== who.agentId);
    }
    const team = this.#db
      .prepare(`SELECT agent_id FROM team_members WHERE project = ? AND team_id = ?`)
      .all(who.project, to)
      .map((row) => (row as { agent_id: string }).agent_id);
    if (team.length > 0) return team.filter((agentId) => agentId !== who.agentId);
    const knownAgent = this.#db
      .prepare(`SELECT 1 FROM agents WHERE project = ? AND agent_id = ?`)
      .get(who.project, to);
    return knownAgent === undefined ? [] : [to];
  }
}

interface DeliveryRow {
  message_id: string;
  sender_id: string;
  body: string;
  kind: string;
  conversation_id: string;
  reply_to: string | null;
  task_id: string | null;
  output_path: string | null;
  created_at: number;
  claim_expires_at: number | null;
}

interface ActivityRow {
  seq: number;
  at: number;
  agent_id: string;
  kind: string;
  detail: string;
}

export interface DatabaseDiagnostic {
  status: "absent" | "ok" | "error";
  exists: boolean;
  readOnly: true;
  database: string;
  project: string;
  counts?: Record<string, number>;
  checks?: Array<{ name: string; ok: boolean; detail: string }>;
  error?: string;
}

// Deliberately no ctime. A concurrent writer's read-write open of the database
// advances the inode's ctime on Darwin without changing a byte, so including it
// here would make the snapshot loop report an unchanged database as changed and
// fail a read-only diagnostic. Identity is still pinned by dev, ino, size and
// mtime, and any real write moves size or mtime. Same reasoning as #742.
const fileVersion = (path: string): string | null => {
  try {
    const value = statSync(path, { bigint: true });
    return [value.dev, value.ino, value.size, value.mtimeNs].join(":");
  } catch (error) {
    if ((error as NodeJS.ErrnoException).code === "ENOENT") return null;
    throw error;
  }
};

const copyConsistentSnapshot = (path: string, snapshotPath: string): void => {
  const walPath = `${path}-wal`;
  const snapshotWalPath = `${snapshotPath}-wal`;
  for (let attempt = 0; attempt < 3; attempt += 1) {
    const beforeMain = fileVersion(path);
    const beforeWal = fileVersion(walPath);
    try {
      copyFileSync(path, snapshotPath);
      if (beforeWal === null) rmSync(snapshotWalPath, { force: true });
      else copyFileSync(walPath, snapshotWalPath);
    } catch (error) {
      rmSync(snapshotPath, { force: true });
      rmSync(snapshotWalPath, { force: true });
      if (attempt === 2) throw error;
      continue;
    }
    if (
      beforeMain === fileVersion(path) &&
      beforeWal === fileVersion(walPath)
    ) return;
    rmSync(snapshotPath, { force: true });
    rmSync(snapshotWalPath, { force: true });
  }
  throw new Error("database changed while creating a diagnostic snapshot");
};

/** Inspect existing state without creating directories, schema, rows or activity. */
export function inspectDatabase(
  path: string,
  project: string,
  mode: "status" | "doctor",
): DatabaseDiagnostic {
  try {
    statSync(path);
  } catch (error) {
    if ((error as NodeJS.ErrnoException).code === "ENOENT") {
      return { status: "absent", exists: false, readOnly: true, database: path, project };
    }
    return {
      status: "error",
      exists: true,
      readOnly: true,
      database: path,
      project,
      error: error instanceof Error ? error.message : String(error),
    };
  }

  let db: Database.Database | undefined;
  let snapshotDirectory: string | undefined;
  try {
    // Inspect a version-checked private main/WAL pair so a concurrent
    // checkpoint cannot silently pair files from different database states.
    // SQLite rebuilds only the private shared-memory index.
    snapshotDirectory = mkdtempSync(resolve(tmpdir(), "fabric-inspect-"));
    const snapshotPath = resolve(snapshotDirectory, "fabric.sqlite3");
    copyConsistentSnapshot(path, snapshotPath);
    db = new Database(snapshotPath, { fileMustExist: true });
    db.pragma("query_only = ON");
    const tables = new Set(
      (db.prepare(`SELECT name FROM sqlite_master WHERE type = 'table'`).all() as Array<{
        name: string;
      }>).map((row) => row.name),
    );
    const counts: Record<string, number> = {};
    const count = (table: string, extra = ""): number => {
      if (!tables.has(table)) return 0;
      const row = db!.prepare(`SELECT count(*) AS count FROM ${table} WHERE project = ? ${extra}`)
        .get(project) as { count: number };
      return Number(row.count);
    };
    counts.agents = count("agents");
    counts.messages = count("messages");
    counts.pendingDeliveries = count("deliveries", "AND read_at IS NULL");
    counts.tasks = count("tasks");
    counts.activity = count("activity");

    if (mode === "status") {
      return { status: "ok", exists: true, readOnly: true, database: path, project, counts };
    }

    const missing = REQUIRED_TABLES.filter((table) => !tables.has(table));
    const claimColumnRows = tables.has("delivery_claims")
      ? db.pragma("table_info(delivery_claims)") as Array<{ name: string }>
      : [];
    const claimColumns = tables.has("delivery_claims")
      ? new Set(claimColumnRows.map((column) => column.name))
      : new Set<string>();
    const missingClaimColumns = REQUIRED_CLAIM_COLUMNS.filter((column) => !claimColumns.has(column));
    const integrityRows = db.pragma("quick_check") as Array<{ quick_check: string }>;
    const integrity = integrityRows.every((row) => row.quick_check === "ok");
    const foreignKeyViolations = db.pragma("foreign_key_check") as Array<{
      table: string;
      rowid: number;
      parent: string;
      fkid: number;
    }>;
    const namespaceCollisions = tables.has("agents") && tables.has("teams")
      ? (db.prepare(
        `SELECT a.agent_id
         FROM agents a JOIN teams t
           ON t.project = a.project AND t.team_id = a.agent_id
         WHERE a.project = ? ORDER BY a.agent_id`,
      ).all(project) as Array<{ agent_id: string }>).map((row) => row.agent_id)
      : [];
    const checks = [
      {
        name: "schema",
        ok: missing.length === 0,
        detail: missing.length === 0 ? `${REQUIRED_TABLES.length} required tables present` :
          `missing tables: ${missing.join(", ")}`,
      },
      {
        name: "integrity",
        ok: integrity,
        detail: integrityRows.map((row) => row.quick_check).join("; "),
      },
      {
        name: "delivery-claim-schema",
        ok: missingClaimColumns.length === 0,
        detail: missingClaimColumns.length === 0 ? "delivery claim columns present" :
          `missing columns: ${missingClaimColumns.join(", ")}`,
      },
      {
        name: "foreign-keys",
        ok: foreignKeyViolations.length === 0,
        detail: foreignKeyViolations.length === 0 ? "no foreign-key violations" :
          foreignKeyViolations.map((violation) =>
            `${violation.table} row ${violation.rowid} -> ${violation.parent}`).join("; "),
      },
      {
        name: "recipient-namespace",
        ok: namespaceCollisions.length === 0,
        detail: namespaceCollisions.length === 0 ? "agent labels and team ids are distinct" :
          `agent/team ids overlap (team-first routing): ${namespaceCollisions.join(", ")}`,
      },
    ];
    return {
      status: checks.every((check) => check.ok) ? "ok" : "error",
      exists: true,
      readOnly: true,
      database: path,
      project,
      counts,
      checks,
    };
  } catch (error) {
    return {
      status: "error",
      exists: true,
      readOnly: true,
      database: path,
      project,
      error: error instanceof Error ? error.message : String(error),
    };
  } finally {
    db?.close();
    if (snapshotDirectory !== undefined) rmSync(snapshotDirectory, { recursive: true, force: true });
  }
}
