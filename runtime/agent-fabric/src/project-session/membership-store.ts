import type Database from "better-sqlite3";

import { ProjectFabricCoreError, type CoreServiceOptions } from "./contracts.js";
import { membershipSourceDisposition } from "./membership-disposition.js";
import { integer, isRow, row, text } from "../persistence/row-codec.js";

export type AutomaticMembershipKind =
  | "task"
  | "lease"
  | "required-message"
  | "artifact-obligation"
  | "scoped-barrier";

export type AutomaticMembership = Readonly<{
  kind: AutomaticMembershipKind;
  memberId: string;
}>;

const FROZEN_SESSION_STATES = new Set([
  "quiescing",
  "awaiting_acceptance",
  "closed",
  "cancelled",
]);

export function touchProjectSessionMembershipRevision(
  database: Database.Database,
  projectSessionId: string,
  now: number,
  changes: number,
): void {
  if (changes === 0) return;
  const changed = database.prepare(`
    UPDATE project_sessions
       SET membership_revision=membership_revision+1,
           revision=revision+1,
           updated_at=?
     WHERE project_session_id=?
  `).run(now, projectSessionId);
  if (changed.changes !== 1) {
    throw new ProjectFabricCoreError("RECOVERY_REQUIRED", "membership owner session was not found");
  }
}

export function touchProjectSessionMembershipRevisionForRun(
  database: Database.Database,
  runId: string,
  now: number,
  changes: number,
): void {
  if (changes === 0) return;
  const identity = row(database.prepare(`
    SELECT project_session_id FROM runs WHERE run_id=?
  `).get(runId), "membership owner run");
  touchProjectSessionMembershipRevision(database, text(identity, "project_session_id"), now, changes);
}

export class ProjectSessionMembershipStore {
  readonly #database: Database.Database;
  readonly #clock: () => number;

  constructor(options: CoreServiceOptions) {
    this.#database = options.database;
    this.#clock = options.clock ?? Date.now;
  }

  bindRequired(runId: string, members: readonly AutomaticMembership[]): number {
    return this.#database.transaction(() => {
      if (members.length === 0) return 0;
      const identity = this.#runIdentity(runId);
      if (FROZEN_SESSION_STATES.has(identity.sessionState)) {
        throw new ProjectFabricCoreError(
          "LIFECYCLE_PRECONDITION_FAILED",
          "project-session membership is frozen",
        );
      }
      const unique = this.#uniqueMembers(members);
      const insert = this.#database.prepare(`
        INSERT INTO project_session_memberships(
          project_session_id, coordination_run_id, member_kind, member_id,
          required, state, revision, created_at, updated_at
        ) VALUES (?, ?, ?, ?, 1, 'active', 1, ?, ?)
        ON CONFLICT(project_session_id, coordination_run_id, member_kind, member_id, member_adapter_id)
        DO NOTHING
      `);
      const now = this.#clock();
      let changes = 0;
      for (const member of unique) {
        const existing = this.#database.prepare(`
          SELECT required, state FROM project_session_memberships
           WHERE project_session_id=? AND coordination_run_id=?
             AND member_kind=? AND member_id=?
        `).get(identity.projectSessionId, runId, member.kind, member.memberId);
        if (isRow(existing)) {
          if (integer(existing, "required") !== 1 || text(existing, "state") !== "active") {
            throw new ProjectFabricCoreError(
              "DEDUPE_CONFLICT",
              "automatic membership identity is already terminal or optional",
            );
          }
          continue;
        }
        changes += insert.run(
          identity.projectSessionId,
          runId,
          member.kind,
          member.memberId,
          now,
          now,
        ).changes;
      }
      this.#touchSession(identity.projectSessionId, changes, now);
      return changes;
    })();
  }

  reconcile(runId: string, members: readonly AutomaticMembership[]): number {
    return this.#database.transaction(() => {
      if (members.length === 0) return 0;
      const identity = this.#runIdentity(runId);
      const now = this.#clock();
      const update = this.#database.prepare(`
        UPDATE project_session_memberships
           SET state=?, abandoned_reason=?, revision=revision+1, updated_at=?
         WHERE project_session_id=? AND coordination_run_id=?
           AND member_kind=? AND member_id=? AND required=1 AND state='active'
      `);
      let changes = 0;
      for (const member of this.#uniqueMembers(members)) {
        const disposition = membershipSourceDisposition(
          this.#database,
          identity.projectSessionId,
          runId,
          member.kind,
          member.memberId,
        );
        if (disposition.state === "active") {
          throw new ProjectFabricCoreError(
            "LIFECYCLE_PRECONDITION_FAILED",
            `${member.kind} membership source is not terminal`,
          );
        }
        changes += update.run(
          disposition.state,
          disposition.state === "abandoned" ? disposition.reason : null,
          now,
          identity.projectSessionId,
          runId,
          member.kind,
          member.memberId,
        ).changes;
      }
      this.#touchSession(identity.projectSessionId, changes, now);
      return changes;
    })();
  }

  reconcileRequiredMessageIfSettled(runId: string, messageId: string): boolean {
    return this.#database.transaction(() => {
      const message = row(this.#database.prepare(`
        SELECT requires_ack FROM messages WHERE run_id=? AND message_id=?
      `).get(runId, messageId), "required message");
      if (integer(message, "requires_ack") !== 1) return false;
      const unresolved = row(this.#database.prepare(`
        SELECT COUNT(*) AS count FROM deliveries
         WHERE run_id=? AND message_id=?
           AND state NOT IN ('acknowledged','abandoned','expired')
      `).get(runId, messageId), "required message deliveries");
      if (integer(unresolved, "count") !== 0) return false;
      return this.reconcile(runId, [{ kind: "required-message", memberId: messageId }]) === 1;
    })();
  }

  #touchSession(projectSessionId: string, changes: number, now: number): void {
    touchProjectSessionMembershipRevision(this.#database, projectSessionId, now, changes);
  }

  #runIdentity(runId: string): { projectSessionId: string; sessionState: string } {
    const value = row(this.#database.prepare(`
      SELECT run.project_session_id, session.state
        FROM runs run JOIN project_sessions session
          ON session.project_session_id=run.project_session_id
       WHERE run.run_id=?
    `).get(runId), "membership coordination run");
    return {
      projectSessionId: text(value, "project_session_id"),
      sessionState: text(value, "state"),
    };
  }

  #uniqueMembers(members: readonly AutomaticMembership[]): AutomaticMembership[] {
    const byIdentity = new Map<string, AutomaticMembership>();
    for (const member of members) {
      if (member.memberId.trim().length === 0) {
        throw new ProjectFabricCoreError("PROTOCOL_INVALID", "membership source identity is required");
      }
      byIdentity.set(`${member.kind}\0${member.memberId}`, member);
    }
    return [...byIdentity.values()];
  }
}
