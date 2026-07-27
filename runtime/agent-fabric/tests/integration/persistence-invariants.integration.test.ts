import Database from "better-sqlite3";

import { afterEach, describe, expect, it } from "vitest";

import { applyMigrations } from "../../src/core/migrations.ts";
import {
  admitProviderActionFixture,
  insertProviderActionPreflightParent,
} from "../support/provider-action-fixture.ts";

const openDatabases: Database.Database[] = [];

function applyInvariantMigrations(database: Database.Database): void {
  applyMigrations(database);
}

function seedInvariantRuns(database: Database.Database): void {
  database.exec(`
    INSERT INTO projects(project_id,canonical_root,revision,authority_generation,created_at,updated_at)
    VALUES ('project-a','/tmp/a',1,1,1,1), ('project-b','/tmp/b',1,1,1,1);
    INSERT INTO project_sessions(
      project_session_id,project_id,mode,state,revision,generation,authority_ref,budget_ref,
      launch_packet_path,launch_packet_digest,membership_revision,origin_kind,origin_operator_id,
      created_at,updated_at
    ) VALUES
      ('session-a','project-a','coordinated','active',1,1,'authority-a','budget-a','launch.json',
       'sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa',1,'operator-launch','operator-a',1,1),
      ('session-b','project-b','coordinated','active',1,1,'authority-b','budget-b','launch.json',
       'sha256:bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb',1,'operator-launch','operator-b',1,1);
    INSERT INTO runs(
      run_id,chair_agent_id,workspace_root,project_run_directory,created_at,project_session_id,
      lifecycle_state,revision,chair_generation,chair_lease_id,authority_ref,budget_ref,
      dependency_revision,topology_slot
    ) VALUES
      ('run-a','chair-a','/tmp/a',NULL,1,'session-a','active',1,1,'chair:run-a:1','authority-a','budget-a',1,1),
      ('run-b','chair-b','/tmp/b',NULL,1,'session-b','active',1,1,'chair:run-b:1','authority-b','budget-b',1,1);
  `);
}

afterEach(() => {
  for (const database of openDatabases.splice(0)) database.close();
});

describe("additive persistence invariants", () => {
  it("keeps provider-action preflight identity immutable and closes its state graph", () => {
    const database = new Database(":memory:");
    openDatabases.push(database);
    applyInvariantMigrations(database);
    seedInvariantRuns(database);
    insertProviderActionPreflightParent(database, {
      runId: "run-a", adapterId: "fake", actionId: "action-a",
    });

    expect(() => database.prepare(`
      UPDATE provider_action_pair_preflights SET input_digest='crossed'
       WHERE adapter_id='fake' AND action_id='action-a'
    `).run()).toThrow(/INVARIANT_provider_action_preflight_transition/u);
    database.prepare(`
      UPDATE provider_action_pair_preflights SET state='admitted',updated_at=2
       WHERE adapter_id='fake' AND action_id='action-a'
    `).run();
    expect(() => database.prepare(`
      UPDATE provider_action_pair_preflights SET state='released',failure_json='{"name":"Error","message":"late"}',updated_at=3
       WHERE adapter_id='fake' AND action_id='action-a'
    `).run()).toThrow(/INVARIANT_provider_action_preflight_transition/u);
    expect(() => database.prepare(`
      DELETE FROM provider_action_pair_preflights
       WHERE adapter_id='fake' AND action_id='action-a'
    `).run()).toThrow(/INVARIANT_provider_action_preflight_immutable/u);
  });

  it("rejects invalid critical values and cross-run references", () => {
    const database = new Database(":memory:");
    openDatabases.push(database);
    applyInvariantMigrations(database);
    seedInvariantRuns(database);
    database.exec(`
      INSERT INTO authorities VALUES ('authority-a','run-a',NULL,'{}','a',1);
      INSERT INTO authorities VALUES ('authority-b','run-b',NULL,'{}','b',1);
      INSERT INTO agents VALUES ('run-a','chair-a',NULL,'authority-a',NULL,'ready',NULL);
      INSERT INTO agents VALUES ('run-b','chair-b',NULL,'authority-b',NULL,'ready',NULL);
    `);

    expect(() => database.prepare("UPDATE agents SET lifecycle='invented' WHERE run_id='run-a'").run()).toThrow(/INVARIANT_agents_lifecycle/u);
    expect(() => database.prepare("INSERT INTO tasks VALUES ('run-a','task-x','authority-b','x','base','ready',NULL,0,0,'chair-a')").run()).toThrow(/INVARIANT_tasks_authority_same_run/u);
    expect(() => database.prepare("INSERT INTO messages VALUES ('message-x','run-a','chair-a','dedupe','hash','{}','event','body',2,'conversation',NULL,NULL,0,NULL,1)").run()).toThrow(/INVARIANT_messages_requires_ack/u);
  });

  it("couples provider action budget vectors, task liveness and authority ledger transitions", () => {
    const database = new Database(":memory:");
    openDatabases.push(database);
    applyInvariantMigrations(database);
    seedInvariantRuns(database);
    database.exec(`
      INSERT INTO authorities VALUES ('authority-a','run-a',NULL,'{}','a',1);
      INSERT INTO agents VALUES ('run-a','chair-a',NULL,'authority-a',NULL,'ready',NULL);
      INSERT INTO authority_budget(authority_id,unit_key,granted)
      VALUES ('authority-a','turns',4),('authority-a','provider_calls',2),('authority-a','descendants',1);
      INSERT INTO tasks VALUES ('run-a','task-a','authority-a','review','base','active','chair-a',1,1,'chair-a');
      INSERT INTO authorities VALUES ('authority-unsafe','run-a','authority-a','{}','unsafe',1);
      INSERT INTO authority_budget(authority_id,unit_key,granted)
      VALUES ('authority-unsafe','turns',9007199254740992);
    `);
    insertProviderActionPreflightParent(database, {
      runId: "run-a", adapterId: "fake", actionId: "action-unsafe-turns",
    });
    expect(() => database.prepare(`
      INSERT INTO provider_actions(
        run_id,action_id,adapter_id,operation,target_agent_id,provider_session_generation,
        turn_lease_generation,identity_hash,payload_hash,payload_json,status,history_json,
        execution_count,effect_count,idempotency_proven,result_json,updated_at,journal_revision,
        task_id,budget_authority_id,budget_reservation_json,budget_settlement_json,budget_state,budget_started_at
      ) VALUES (
        'run-a','action-unsafe-turns','fake','spawn',NULL,NULL,NULL,'identity-unsafe','payload-unsafe','{"maxTurns":9007199254740992}',
        'dispatched','["prepared","dispatched"]',1,0,0,NULL,1,1,
        'task-a','authority-unsafe','{"turns":9007199254740992}',NULL,'reserved',1
      )
    `).run()).toThrow(/INVARIANT_provider_actions_budget_reservation/u);
    insertProviderActionPreflightParent(database, {
      runId: "run-a", adapterId: "fake", actionId: "action-invalid-unit",
    });
    expect(() => database.prepare(`
      INSERT INTO provider_actions(
        run_id,action_id,adapter_id,operation,target_agent_id,provider_session_generation,
        turn_lease_generation,identity_hash,payload_hash,payload_json,status,history_json,
        execution_count,effect_count,idempotency_proven,result_json,updated_at,journal_revision,
        task_id,budget_authority_id,budget_reservation_json,budget_settlement_json,budget_state,budget_started_at
      ) VALUES (
        'run-a','action-invalid-unit','fake','spawn',NULL,NULL,NULL,'identity-invalid','payload-invalid','{}',
        'dispatched','["prepared","dispatched"]',1,0,0,NULL,1,1,
        'task-a','authority-a','{"descendants":1}',NULL,'reserved',1
      )
    `).run()).toThrow(/INVARIANT_provider_actions_budget_reservation/u);
    insertProviderActionPreflightParent(database, {
      runId: "run-a", adapterId: "fake", actionId: "action-missing-turns",
    });
    expect(() => database.prepare(`
      INSERT INTO provider_actions(
        run_id,action_id,adapter_id,operation,target_agent_id,provider_session_generation,
        turn_lease_generation,identity_hash,payload_hash,payload_json,status,history_json,
        execution_count,effect_count,idempotency_proven,result_json,updated_at,journal_revision,
        task_id,budget_authority_id,budget_reservation_json,budget_settlement_json,budget_state,budget_started_at
      ) VALUES (
        'run-a','action-missing-turns','fake','spawn',NULL,NULL,NULL,'identity-no-turns','payload-no-turns','{"maxTurns":1}',
        'dispatched','["prepared","dispatched"]',1,0,0,NULL,1,1,
        'task-a','authority-a','{"provider_calls":1}',NULL,'reserved',1
      )
    `).run()).toThrow(/INVARIANT_provider_actions_budget_reservation/u);
    admitProviderActionFixture(database, {
      runId: "run-a",
      actionId: "action-budget",
      adapterId: "fake",
      operation: "spawn",
      identityHash: "identity",
      payloadHash: "payload",
      payloadJson: '{"maxTurns":2}',
      status: "dispatched",
      historyJson: '["prepared","dispatched"]',
      executionCount: 1,
      taskId: "task-a",
      budgetAuthorityId: "authority-a",
      budgetReservationJson: '{"provider_calls":1,"turns":2}',
      budgetState: "reserved",
      budgetStartedAt: 1,
      updatedAt: 1,
    });

    expect(database.prepare(`
      SELECT unit_key,reserved,consumed,provider_reserved,provider_consumed FROM authority_budget
       WHERE authority_id='authority-a' AND unit_key IN ('provider_calls','turns') ORDER BY unit_key
    `).all()).toEqual([
      { unit_key: "provider_calls", reserved: 1, consumed: 0, provider_reserved: 1, provider_consumed: 0 },
      { unit_key: "turns", reserved: 2, consumed: 0, provider_reserved: 2, provider_consumed: 0 },
    ]);
    expect(() => database.prepare(`
      UPDATE authority_budget SET provider_reserved=0
       WHERE authority_id='authority-a' AND unit_key='turns'
    `).run()).toThrow(/INVARIANT_authority_budget_provider_ledger/u);
    expect(() => database.prepare(`
      UPDATE provider_actions SET budget_reservation_json='{"turns":1}'
       WHERE action_id='action-budget'
    `).run()).toThrow(/INVARIANT_provider_actions_budget_binding_immutable/u);
    expect(() => database.prepare(`
      UPDATE provider_actions
         SET status='terminal',budget_state='settled',budget_settlement_json='{"turns":1}'
       WHERE action_id='action-budget'
    `).run()).toThrow(/INVARIANT_provider_actions_budget_settlement/u);
    expect(() => database.prepare("UPDATE tasks SET state='complete' WHERE task_id='task-a'").run())
      .toThrow(/INVARIANT_task_provider_action_unresolved/u);
    expect(() => database.prepare(`
      UPDATE provider_actions SET status='terminal' WHERE action_id='action-budget'
    `).run()).toThrow(/CHECK constraint failed/u);
    expect(() => database.prepare(`
      UPDATE provider_actions SET status='quarantined' WHERE action_id='action-budget'
    `).run()).toThrow(/CHECK constraint failed/u);

    database.prepare(`
      UPDATE provider_actions
         SET status='terminal',effect_count=1,budget_state='settled',
             budget_settlement_json='{"provider_calls":1,"turns":1}'
       WHERE action_id='action-budget'
    `).run();
    expect(database.prepare(`
      SELECT unit_key,reserved,consumed,provider_reserved,provider_consumed,usage_unknown FROM authority_budget
       WHERE authority_id='authority-a' AND unit_key IN ('provider_calls','turns') ORDER BY unit_key
    `).all()).toEqual([
      { unit_key: "provider_calls", reserved: 0, consumed: 1, provider_reserved: 0, provider_consumed: 1, usage_unknown: 0 },
      { unit_key: "turns", reserved: 0, consumed: 1, provider_reserved: 0, provider_consumed: 1, usage_unknown: 0 },
    ]);
    expect(() => database.prepare(`
      UPDATE authority_budget SET consumed=0,provider_consumed=0
       WHERE authority_id='authority-a' AND unit_key='turns'
    `).run()).toThrow(/INVARIANT_authority_budget_provider_ledger/u);
    expect(() => database.prepare("UPDATE tasks SET state='complete' WHERE task_id='task-a'").run())
      .not.toThrow();

    database.prepare("UPDATE tasks SET state='active' WHERE task_id='task-a'").run();
    admitProviderActionFixture(database, {
      runId: "run-a",
      actionId: "action-unknown",
      adapterId: "fake",
      operation: "spawn",
      identityHash: "identity-unknown",
      payloadHash: "payload-unknown",
      payloadJson: '{"maxTurns":1}',
      status: "dispatched",
      historyJson: '["prepared","dispatched"]',
      executionCount: 1,
      taskId: "task-a",
      budgetAuthorityId: "authority-a",
      budgetReservationJson: '{"turns":1}',
      budgetState: "reserved",
      budgetStartedAt: 2,
      updatedAt: 2,
    });
    database.prepare(`
      UPDATE provider_actions
         SET status='terminal',effect_count=1,budget_state='usage-unknown',
             budget_settlement_json='{"turns":"unknown"}'
       WHERE action_id='action-unknown'
    `).run();
    expect(() => database.prepare(`
      UPDATE authority_budget SET usage_unknown=0
       WHERE authority_id='authority-a' AND unit_key='turns'
    `).run()).toThrow(/INVARIANT_authority_budget_provider_ledger/u);
    expect(() => database.prepare("UPDATE tasks SET state='complete' WHERE task_id='task-a'").run())
      .not.toThrow();
  });

  it("rejects a fifth run-wide team leader even when the existing leaders are nested", () => {
    const database = new Database(":memory:");
    openDatabases.push(database);
    applyInvariantMigrations(database);
    seedInvariantRuns(database);
    const insert = database.prepare(`
      INSERT INTO teams(
        run_id,team_id,parent_team_id,depth,leader_agent_id,original_leader_agent_id,
        successor_agent_id,root_task_id,authority_id,budget_id,state,generation,
        handoff_evidence,created_at
      ) VALUES ('run-a',?,?,?,?,?,NULL,?,?,?,'active',1,NULL,1)
    `);
    insert.run("parent", null, 1, "leader-parent", "leader-parent", "task-parent", "authority-parent", "budget-parent");
    for (const suffix of ["a", "b", "c"]) {
      insert.run(`child-${suffix}`, "parent", 2, `leader-${suffix}`, `leader-${suffix}`, `task-${suffix}`, `authority-${suffix}`, `budget-${suffix}`);
    }

    expect(() => insert.run(
      "child-d",
      "parent",
      2,
      "leader-d",
      "leader-d",
      "task-d",
      "authority-d",
      "budget-d",
    )).toThrow(/INVARIANT_teams_run_leader_cap/u);
    expect(database.prepare("SELECT COUNT(*) AS count FROM teams WHERE run_id='run-a'").get()).toEqual({ count: 4 });
  });

  it("fires every additive insert and update invariant trigger", () => {
    const cases = [
      ["INVARIANT_agents_lifecycle", "INSERT INTO agents VALUES ('run-a','bad',NULL,'authority-a',NULL,'bad',NULL)", "UPDATE agents SET lifecycle='bad' WHERE agent_id='worker-a'"],
      ["INVARIANT_agents_authority_same_run", "INSERT INTO agents VALUES ('run-a','bad',NULL,'authority-b',NULL,'ready',NULL)", "UPDATE agents SET authority_id='authority-b' WHERE agent_id='worker-a'"],
      ["INVARIANT_agents_parent_same_run", "INSERT INTO agents VALUES ('run-a','bad','chair-b','authority-a',NULL,'ready',NULL)", "UPDATE agents SET parent_agent_id='chair-b' WHERE agent_id='worker-a'"],
      ["INVARIANT_authorities_parent_same_run", "INSERT INTO authorities VALUES ('authority-x','run-a','authority-b','{}','x',1)", "UPDATE authorities SET parent_authority_id='authority-b' WHERE authority_id='authority-child'"],
      ["INVARIANT_tasks_values", "INSERT INTO tasks VALUES ('run-a','task-x','authority-a','x','b','bad',NULL,0,0,'chair-a')", "UPDATE tasks SET state='bad' WHERE task_id='task-a'"],
      ["INVARIANT_tasks_authority_same_run", "INSERT INTO tasks VALUES ('run-a','task-x','authority-b','x','b','ready',NULL,0,0,'chair-a')", "UPDATE tasks SET authority_id='authority-b' WHERE task_id='task-a'"],
      ["INVARIANT_tasks_owner_same_run", "INSERT INTO tasks VALUES ('run-a','task-x','authority-a','x','b','ready','chair-b',0,0,'chair-a')", "UPDATE tasks SET owner_agent_id='chair-b' WHERE task_id='task-a'"],
      ["INVARIANT_tasks_creator_same_run", "INSERT INTO tasks VALUES ('run-a','task-x','authority-a','x','b','ready',NULL,0,0,'chair-b')", "UPDATE tasks SET created_by='chair-b' WHERE task_id='task-a'"],
      ["INVARIANT_messages_requires_ack", "INSERT INTO messages VALUES ('message-x','run-a','chair-a','dx','h','{}','event','b',2,'c',NULL,NULL,0,NULL,1)", "UPDATE messages SET requires_ack=2 WHERE message_id='message-a'"],
      ["INVARIANT_messages_sender_same_run", "INSERT INTO messages VALUES ('message-x','run-a','chair-b','dx','h','{}','event','b',1,'c',NULL,NULL,0,NULL,1)", "UPDATE messages SET sender_id='chair-b' WHERE message_id='message-a'"],
      ["INVARIANT_messages_reply_same_run", "INSERT INTO messages VALUES ('message-x','run-a','chair-a','dx','h','{}','event','b',1,'c','message-b',NULL,0,NULL,1)", "UPDATE messages SET reply_to_message_id='message-b' WHERE message_id='message-a'"],
      ["INVARIANT_deliveries_values", "INSERT INTO deliveries VALUES ('delivery-x','message-a','run-a','worker-a',0,'ready',0,NULL,NULL,NULL,NULL)", "UPDATE deliveries SET mailbox_sequence=0 WHERE delivery_id='delivery-a'"],
      ["INVARIANT_deliveries_message_same_run", "INSERT INTO deliveries VALUES ('delivery-x','message-b','run-a','worker-a',2,'ready',0,NULL,NULL,NULL,NULL)", "UPDATE deliveries SET message_id='message-b' WHERE delivery_id='delivery-a'"],
      ["INVARIANT_deliveries_recipient_same_run", "INSERT INTO deliveries VALUES ('delivery-x','message-a','run-a','chair-b',2,'ready',0,NULL,NULL,NULL,NULL)", "UPDATE deliveries SET recipient_id='chair-b' WHERE delivery_id='delivery-a'"],
      ["INVARIANT_leases_values", "INSERT INTO leases VALUES ('lease-x','run-a','other','worker-a',1,'active',9,1)", "UPDATE leases SET status='bad' WHERE lease_id='lease-a'"],
      ["INVARIANT_leases_holder_same_run", "INSERT INTO leases VALUES ('lease-x','run-a','write','chair-b',1,'active',9,1)", "UPDATE leases SET holder_agent_id='chair-b' WHERE lease_id='lease-a'"],
      ["INVARIANT_provider_actions_values", "INSERT INTO provider_actions(run_id,action_id,adapter_id,operation,target_agent_id,provider_session_generation,turn_lease_generation,identity_hash,payload_hash,payload_json,status,history_json,execution_count,effect_count,idempotency_proven,result_json,updated_at,journal_revision) VALUES ('run-a','action-x','a','turn','worker-a',1,1,'i','p','{}','bad','[]',0,0,0,NULL,1,1)", "UPDATE provider_actions SET status='bad' WHERE action_id='action-a'"],
      ["INVARIANT_provider_actions_target_same_run", "INSERT INTO provider_actions(run_id,action_id,adapter_id,operation,target_agent_id,provider_session_generation,turn_lease_generation,identity_hash,payload_hash,payload_json,status,history_json,execution_count,effect_count,idempotency_proven,result_json,updated_at,journal_revision) VALUES ('run-a','action-x','a','turn','chair-b',1,1,'i','p','{}','terminal','[]',0,0,1,NULL,1,1)", "UPDATE provider_actions SET target_agent_id='chair-b' WHERE action_id='action-a'"],
      ["INVARIANT_authority_budget_boolean", "INSERT INTO authority_budget VALUES ('authority-a','other',1,0,0,0,0,2)", "UPDATE authority_budget SET usage_unknown=2 WHERE authority_id='authority-a'"],
      ["INVARIANT_capabilities_generation", "INSERT INTO capabilities VALUES ('token-x','run-a','worker-a',0,9,NULL)", "UPDATE capabilities SET principal_generation=0 WHERE token_hash='token-a'"],
      ["INVARIANT_provider_state_generation", "INSERT INTO provider_state VALUES ('run-a','chair-a',0,NULL,NULL)", "UPDATE provider_state SET provider_session_generation=0 WHERE agent_id='worker-a'"],
      ["INVARIANT_events_actor_same_run", "INSERT INTO events VALUES ('event-x','run-a','x','chair-b','{}',2)", "UPDATE events SET actor_agent_id='chair-b' WHERE event_id='event-a'"],
      ["INVARIANT_barriers_state", "INSERT INTO barriers VALUES ('run-a','stage','x','open',NULL,NULL)", "UPDATE barriers SET state='open' WHERE run_id='run-a'"],
      ["INVARIANT_teams_values", "INSERT INTO teams VALUES ('run-a','team-x',NULL,0,'chair-a','chair-a',NULL,'task-a','authority-a','budget-a','active',1,NULL,1)", "UPDATE teams SET depth=0 WHERE team_id='team-a'"],
      ["INVARIANT_budgets_state", "INSERT INTO budgets VALUES ('run-a','budget-x',NULL,'team-a','chair-a','bad','{}',1)", "UPDATE budgets SET state='bad' WHERE budget_id='budget-a'"],
      ["INVARIANT_budget_dimensions_values", "INSERT INTO budget_dimensions VALUES ('run-a','budget-a','other',1,2,0,0,0)", "UPDATE budget_dimensions SET reserved=20 WHERE budget_id='budget-a'"],
      ["INVARIANT_objective_check_status", "INSERT INTO task_objective_checks VALUES ('run-a','task-a','check-x','bad',NULL)", "UPDATE task_objective_checks SET status='bad' WHERE check_id='check-a'"],
    ] as const;

    for (const [code, invalidInsert, invalidUpdate] of cases) {
      const database = new Database(":memory:");
      try {
        applyInvariantMigrations(database);
        seedInvariantRuns(database);
        insertProviderActionPreflightParent(database, {
          runId: "run-a", adapterId: "a", actionId: "action-x",
        });
        database.exec(`
          INSERT INTO authorities VALUES ('authority-a','run-a',NULL,'{}','a',1);
          INSERT INTO authorities VALUES ('authority-b','run-b',NULL,'{}','b',1);
          INSERT INTO authorities VALUES ('authority-child','run-a','authority-a','{}','c',1);
          INSERT INTO agents VALUES ('run-a','chair-a',NULL,'authority-a',NULL,'ready',NULL);
          INSERT INTO agents VALUES ('run-a','worker-a','chair-a','authority-a',NULL,'ready','worker');
          INSERT INTO agents VALUES ('run-b','chair-b',NULL,'authority-b',NULL,'ready',NULL);
          INSERT INTO authority_budget VALUES ('authority-a','turns',10,0,0,0,0,0);
          INSERT INTO tasks VALUES ('run-a','task-a','authority-a','x','b','ready','worker-a',0,0,'chair-a');
          INSERT INTO messages VALUES ('message-a','run-a','chair-a','da','h','{}','event','b',1,'c',NULL,NULL,0,NULL,1);
          INSERT INTO messages VALUES ('message-b','run-b','chair-b','db','h','{}','event','b',1,'c',NULL,NULL,0,NULL,1);
          INSERT INTO deliveries VALUES ('delivery-a','message-a','run-a','worker-a',1,'ready',0,NULL,NULL,NULL,NULL);
          INSERT INTO leases VALUES ('lease-a','run-a','write','worker-a',1,'active',9,1);
          INSERT INTO capabilities VALUES ('token-a','run-a','worker-a',1,9,NULL);
          INSERT INTO provider_state VALUES ('run-a','worker-a',1,NULL,NULL);
          INSERT INTO events VALUES ('event-a','run-a','x','worker-a','{}',1);
          INSERT INTO barriers VALUES ('run-a','run','','closed',1,'hash');
          INSERT INTO teams VALUES ('run-a','team-a',NULL,1,'chair-a','chair-a',NULL,'task-a','authority-a','budget-a','active',1,NULL,1);
          INSERT INTO budgets VALUES ('run-a','budget-a',NULL,'team-a','chair-a','active','{}',1);
          INSERT INTO budget_dimensions VALUES ('run-a','budget-a','turns',10,0,0,0,0);
          INSERT INTO task_objective_checks VALUES ('run-a','task-a','check-a','pending',NULL);
        `);
        admitProviderActionFixture(database, {
          runId: "run-a",
          actionId: "action-a",
          adapterId: "a",
          operation: "turn",
          targetAgentId: "worker-a",
          providerSessionGeneration: 1,
          turnLeaseGeneration: 1,
          identityHash: "i",
          payloadHash: "p",
          payloadJson: "{}",
          status: "terminal",
          historyJson: "[]",
          executionCount: 0,
          idempotencyProven: true,
          updatedAt: 1,
        });
        expect(() => database.exec(invalidInsert), `${code} insert`).toThrow(new RegExp(code, "u"));
        expect(() => database.exec(invalidUpdate), `${code} update`).toThrow(new RegExp(code, "u"));
      } finally { database.close(); }
    }
  });

  it("selects the focused indexes for hot-path predicates", () => {
    const database = new Database(":memory:");
    openDatabases.push(database);
    applyInvariantMigrations(database);
    const plan = (sql: string): string => database.prepare(`EXPLAIN QUERY PLAN ${sql}`).all().map((row) => String((row as { detail: string }).detail)).join("\n");

    expect(plan("SELECT * FROM deliveries WHERE run_id='r' AND recipient_id='a' AND state='ready' ORDER BY mailbox_sequence")).toContain("deliveries_ready_mailbox");
    expect(plan("SELECT * FROM tasks WHERE run_id='r' AND state='active'")).toContain("tasks_by_state");
    expect(plan("SELECT * FROM tasks WHERE run_id='r' AND owner_agent_id='a' AND state='active'")).toContain("tasks_by_owner");
    expect(plan("SELECT * FROM leases WHERE status='active' AND expires_at<=1")).toContain("leases_by_expiry");
    expect(plan("SELECT * FROM provider_actions WHERE run_id='r' AND status IN ('prepared','dispatched','ambiguous') ORDER BY updated_at")).toContain("provider_actions_unresolved");
    expect(plan("SELECT e.* FROM observer_event_sequence s JOIN events e ON e.event_id=s.event_id WHERE e.run_id='r' AND s.sequence>1 ORDER BY s.sequence")).toMatch(/observer_event_sequence|events_by_run_cursor/u);
  });
});
