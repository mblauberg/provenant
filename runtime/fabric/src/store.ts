import Database from "better-sqlite3";
import { randomUUID } from "node:crypto";
import { mkdirSync, readFileSync } from "node:fs";
import { dirname, resolve } from "node:path";
import { fileURLToPath } from "node:url";

import type { Identity } from "./identity.js";

const SCHEMA = resolve(dirname(fileURLToPath(import.meta.url)), "../schema.sql");

export interface Message {
  messageId: string;
  from: string;
  body: string;
  kind: string;
  conversationId: string;
  replyTo: string | null;
  at: string;
}

export interface Task {
  taskId: string;
  objective: string;
  owner: string | null;
  state: string;
  dependsOn: string[];
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

  constructor(path: string) {
    mkdirSync(dirname(path), { recursive: true });
    this.#db = new Database(path);
    this.#db.exec(readFileSync(SCHEMA, "utf8"));
    // Wait rather than fail when another agent holds the write lock.
    this.#db.pragma("busy_timeout = 5000");
  }

  close(): void {
    this.#db.close();
  }

  /** Register the caller if this is the first time it has been seen. */
  announce(who: Identity): void {
    const now = Date.now();
    this.#db
      .prepare(
        `INSERT INTO agents(project, agent_id, provider, first_seen, last_seen)
         VALUES (?, ?, ?, ?, ?)
         ON CONFLICT(project, agent_id) DO UPDATE SET last_seen = excluded.last_seen`,
      )
      .run(who.project, who.agentId, who.provider, now, now);
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
    options: { kind?: string; replyTo?: string } = {},
  ): { messageId: string; recipients: string[] } {
    const recipients = this.#resolveRecipients(who, to);
    if (recipients.length === 0) {
      throw new Error(
        `no recipients for "${to}" in ${who.project}. ` +
          `Known agents: ${this.agents(who.project).map((a) => a.agentId).join(", ") || "none yet"}`,
      );
    }
    const messageId = randomUUID();
    const parentConversation = options.replyTo === undefined
      ? undefined
      : this.#conversationOf(options.replyTo);
    const conversationId = parentConversation ?? messageId;
    // A stale or externally supplied reply id starts a new conversation. Do
    // not write a dangling foreign-key value into the message row.
    const replyTo = parentConversation === undefined ? null : options.replyTo ?? null;
    const now = Date.now();

    this.#db.transaction(() => {
      this.#db
        .prepare(
          `INSERT INTO messages(message_id, project, sender_id, body, kind, conversation_id, reply_to, created_at)
           VALUES (?, ?, ?, ?, ?, ?, ?, ?)`,
        )
        .run(
          messageId,
          who.project,
          who.agentId,
          body,
          options.kind ?? "note",
          conversationId,
          replyTo,
          now,
        );
      const delivery = this.#db.prepare(
        `INSERT INTO deliveries(message_id, project, recipient_id, read_at) VALUES (?, ?, ?, NULL)`,
      );
      for (const recipient of recipients) delivery.run(messageId, who.project, recipient);
      this.#log(who, "send", `to ${to}: ${body.slice(0, 120)}`);
    })();

    return { messageId, recipients };
  }

  /** Unread messages for the caller. Reading marks them read unless peeking. */
  inbox(who: Identity, options: { limit?: number; peek?: boolean } = {}): Message[] {
    const limit = options.limit ?? 20;
    const rows = this.#db
      .prepare(
        `SELECT m.message_id, m.sender_id, m.body, m.kind, m.conversation_id, m.reply_to, m.created_at
         FROM deliveries d JOIN messages m ON m.message_id = d.message_id
         WHERE d.project = ? AND d.recipient_id = ? AND d.read_at IS NULL
         ORDER BY m.created_at LIMIT ?`,
      )
      .all(who.project, who.agentId, limit) as Array<{
        message_id: string;
        sender_id: string;
        body: string;
        kind: string;
        conversation_id: string;
        reply_to: string | null;
        created_at: number;
      }>;

    if (!(options.peek ?? false) && rows.length > 0) {
      const mark = this.#db.prepare(
        `UPDATE deliveries SET read_at = ? WHERE message_id = ? AND recipient_id = ?`,
      );
      const now = Date.now();
      this.#db.transaction(() => {
        for (const row of rows) mark.run(now, row.message_id, who.agentId);
      })();
    }

    return rows.map((row) => ({
      messageId: row.message_id,
      from: row.sender_id,
      body: row.body,
      kind: row.kind,
      conversationId: row.conversation_id,
      replyTo: row.reply_to,
      at: new Date(row.created_at).toISOString(),
    }));
  }

  createTeam(who: Identity, teamId: string, members: string[]): { teamId: string; members: string[] } {
    const now = Date.now();
    this.#db.transaction(() => {
      this.#db
        .prepare(`INSERT OR IGNORE INTO teams(project, team_id, created_at) VALUES (?, ?, ?)`)
        .run(who.project, teamId, now);
      const add = this.#db.prepare(
        `INSERT OR IGNORE INTO team_members(project, team_id, agent_id) VALUES (?, ?, ?)`,
      );
      for (const member of members) add.run(who.project, teamId, member);
      this.#log(who, "team", `${teamId}: ${members.join(", ")}`);
    })();
    return { teamId, members };
  }

  createTask(
    who: Identity,
    objective: string,
    options: { taskId?: string; owner?: string; dependsOn?: string[] } = {},
  ): Task {
    const taskId = options.taskId ?? randomUUID().slice(0, 8);
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
    const changed = this.#db
      .prepare(`UPDATE tasks SET state = ?, updated_at = ? WHERE project = ? AND task_id = ?`)
      .run(state, Date.now(), who.project, taskId);
    if (changed.changes === 0) throw new Error(`no task ${taskId} in ${who.project}`);
    this.#log(who, "task", `${taskId} ${state}${note === undefined ? "" : `: ${note}`}`);
    const task = this.tasks(who.project).find((candidate) => candidate.taskId === taskId);
    if (task === undefined) throw new Error(`task ${taskId} vanished during update`);
    return task;
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

  activity(project: string, limit = 50): Array<{ at: string; agentId: string; kind: string; detail: string }> {
    return this.#db
      .prepare(
        `SELECT at, agent_id, kind, detail FROM activity
         WHERE project = ? ORDER BY seq DESC LIMIT ?`,
      )
      .all(project, limit)
      .map((row) => {
        const record = row as { at: number; agent_id: string; kind: string; detail: string };
        return {
          at: new Date(record.at).toISOString(),
          agentId: record.agent_id,
          kind: record.kind,
          detail: record.detail,
        };
      });
  }

  #log(who: Identity, kind: string, detail: string): void {
    this.#db
      .prepare(`INSERT INTO activity(project, agent_id, at, kind, detail) VALUES (?, ?, ?, ?, ?)`)
      .run(who.project, who.agentId, Date.now(), kind, detail);
  }

  #conversationOf(messageId: string): string | undefined {
    const row = this.#db
      .prepare(`SELECT conversation_id FROM messages WHERE message_id = ?`)
      .get(messageId) as { conversation_id: string } | undefined;
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
