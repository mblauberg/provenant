import Database from "better-sqlite3";
import { afterEach, describe, expect, it } from "vitest";

import {
  assertProviderActionOwner,
  classifyProviderActionOwner,
  classifyProviderActionOwnerForStartup,
  PROVIDER_ACTION_OWNERS,
  ProviderActionOwnerError,
  type ProviderActionCustodyOwner,
  type ProviderActionOwnerRef,
  type ProviderActionOwner,
} from "../../src/application/provider-action-owner.ts";
import { canonicalJson, sha256 } from "../../src/persistence/row-codec.ts";

const databases: Database.Database[] = [];

function openClassifierDatabase(): Database.Database {
  const database = new Database(":memory:");
  databases.push(database);
  database.exec(`
    CREATE TABLE provider_actions(
      run_id TEXT NOT NULL, adapter_id TEXT NOT NULL, action_id TEXT NOT NULL,
      operation TEXT NOT NULL, payload_hash TEXT NOT NULL, payload_json TEXT NOT NULL,
      result_json TEXT,
      target_agent_id TEXT, provider_session_generation INTEGER,
      turn_lease_generation INTEGER,
      finding_capacity_reservation_digest TEXT,
      PRIMARY KEY(adapter_id, action_id)
    );
    CREATE TABLE project_session_launch_custody(
      coordination_run_id TEXT NOT NULL, provider_adapter_id TEXT NOT NULL,
      provider_action_id TEXT NOT NULL
    );
    CREATE TABLE provider_agent_custody(
      run_id TEXT NOT NULL, adapter_id TEXT NOT NULL, action_id TEXT NOT NULL,
      operation TEXT, actor_agent_id TEXT, target_agent_id TEXT,
      bridge_contract_digest TEXT, capability_hash TEXT,
      principal_generation INTEGER, requested_provider_session_ref TEXT
    );
    CREATE TABLE lifecycle_rotation_custodies(
      run_id TEXT NOT NULL, provider_action_adapter_id TEXT NOT NULL,
      provider_action_id TEXT NOT NULL, agent_id TEXT,
      replacement_contract_digest TEXT, staged_capability_hash TEXT,
      target_principal_generation INTEGER
    );
    CREATE TABLE chair_bridge_recovery_custody(
      loss_id TEXT NOT NULL, path TEXT NOT NULL,
      provider_adapter_id TEXT NOT NULL, provider_action_id TEXT
    );
    CREATE TABLE chair_bridge_losses(
      loss_id TEXT NOT NULL, coordination_run_id TEXT NOT NULL
    );
    CREATE TABLE chair_live_handoff_custody(
      coordination_run_id TEXT NOT NULL, provider_adapter_id TEXT NOT NULL,
      promotion_action_id TEXT NOT NULL
    );
    CREATE TABLE operator_control_provider_action_bindings(
      custody_id TEXT NOT NULL, run_id TEXT NOT NULL, adapter_id TEXT NOT NULL,
      action_id TEXT NOT NULL, source_adapter_id TEXT NOT NULL,
      source_action_id TEXT NOT NULL, source_payload_hash TEXT NOT NULL,
      operation TEXT NOT NULL, target_agent_id TEXT NOT NULL,
      provider_session_ref TEXT NOT NULL,
      provider_session_generation INTEGER NOT NULL,
      turn_lease_generation INTEGER NOT NULL, turn_id TEXT NOT NULL
    );
    CREATE TABLE runs(run_id TEXT PRIMARY KEY, project_session_id TEXT NOT NULL);
    CREATE TABLE operator_effect_custody(
      custody_id TEXT NOT NULL, operator_id TEXT NOT NULL, project_id TEXT NOT NULL,
      project_session_id TEXT NOT NULL, operation TEXT NOT NULL,
      intent_digest TEXT NOT NULL, intent_json TEXT NOT NULL
    );
    CREATE TABLE agents(
      run_id TEXT NOT NULL, agent_id TEXT NOT NULL, provider_session_ref TEXT
    );
    CREATE TABLE provider_session_turn_leases(
      run_id TEXT NOT NULL, agent_id TEXT NOT NULL, adapter_id TEXT NOT NULL,
      action_id TEXT NOT NULL, provider_session_generation INTEGER NOT NULL,
      turn_lease_generation INTEGER NOT NULL
    );
    CREATE TABLE review_finding_capacity_reservations(
      run_id TEXT NOT NULL, adapter_id TEXT NOT NULL, action_id TEXT NOT NULL,
      target_generation INTEGER NOT NULL, slot TEXT NOT NULL,
      reservation_digest TEXT NOT NULL
    );
    CREATE TABLE provider_action_routes(
      run_id TEXT NOT NULL, adapter_id TEXT NOT NULL, action_id TEXT NOT NULL,
      certifying_review INTEGER NOT NULL, target_generation INTEGER, slot TEXT
    );
  `);
  return database;
}

function insertAction(
  database: Database.Database,
  input: Readonly<{
    runId?: string;
    adapterId?: string;
    actionId?: string;
    operation?: string;
    payload?: unknown;
    reservationDigest?: string | null;
  }> = {},
): void {
  database.prepare(`INSERT OR IGNORE INTO runs VALUES (?,?)`).run(
    input.runId ?? "run-1",
    `session-${input.runId ?? "run-1"}`,
  );
  database.prepare(`
    INSERT INTO provider_actions(
      run_id,adapter_id,action_id,operation,payload_hash,payload_json,result_json,target_agent_id,
      provider_session_generation,turn_lease_generation,
      finding_capacity_reservation_digest
    ) VALUES (?,?,?,?,?,?,?,?,?,?,?)
  `).run(
    input.runId ?? "run-1",
    input.adapterId ?? "adapter-1",
    input.actionId ?? "action-1",
    input.operation ?? "send_turn",
    input.actionId === "source-1" ? "source-hash" : "action-hash",
    JSON.stringify(input.payload ?? {}),
    input.actionId === "source-1" ? JSON.stringify({ turnId: "turn-1" }) : null,
    null,
    null,
    null,
    input.reservationDigest ?? null,
  );
}

const ref = { runId: "run-1", adapterId: "adapter-1", actionId: "action-1" } as const;

function bindOperatorControl(
  database: Database.Database,
  actionRef: ProviderActionOwnerRef = ref,
): void {
  database.prepare(`UPDATE provider_actions SET payload_json=? WHERE adapter_id=? AND action_id=?`)
    .run(JSON.stringify({ operatorCustodyId: "custody-1" }), actionRef.adapterId, actionRef.actionId);
  database.prepare(`INSERT INTO operator_effect_custody VALUES (?,?,?,?,?,?,?)`)
    .run("custody-1", "operator-1", `project-${actionRef.runId}`, `session-${actionRef.runId}`, "steer", "intent-1", '{"kind":"control","action":"steer"}');
  database.prepare(`INSERT INTO agents VALUES (?,?,?)`)
    .run(actionRef.runId, "agent-1", "resume-1");
  insertAction(database, { runId: actionRef.runId, adapterId: actionRef.adapterId, actionId: "source-1" });
  database.prepare(`INSERT INTO provider_session_turn_leases VALUES (?,?,?,?,?,?)`)
    .run(actionRef.runId, "agent-1", actionRef.adapterId, "source-1", 2, 3);
  database.prepare(`
    UPDATE provider_actions
       SET operation='steer',target_agent_id='agent-1',
           provider_session_generation=2,turn_lease_generation=3
     WHERE adapter_id=? AND action_id=?
  `).run(actionRef.adapterId, actionRef.actionId);
  database.prepare(`
    INSERT INTO operator_control_provider_action_bindings
    VALUES (?,?,?,?,?,?,?,?,?,?,?,?,?)
  `).run(
    "custody-1", actionRef.runId, actionRef.adapterId, actionRef.actionId, actionRef.adapterId,
    "source-1", "source-hash", "steer", "agent-1", "resume-1", 2, 3, "turn-1",
  );
}

afterEach(() => {
  for (const database of databases.splice(0)) database.close();
});

describe("canonical provider-action owner classification", () => {
  it("keeps the closed result set exact", () => {
    expect(PROVIDER_ACTION_OWNERS).toEqual([
      "generic", "launch", "provider_agent", "lifecycle", "herdr",
      "chair_recovery", "chair_live_handoff", "operator_control",
      "certifying_review", "integrity_failed",
    ]);
  });

  it("maps classifier exceptions to the startup fail-closed result", () => {
    const database = openClassifierDatabase();
    insertAction(database);
    database.close();
    expect(classifyProviderActionOwnerForStartup(database, ref)).toStrictEqual({
      owner: "integrity_failed",
      reason: "owner_classification_failed",
    });
  });
  const specialised: ReadonlyArray<Readonly<{
    owner: Exclude<ProviderActionCustodyOwner, "generic">;
    bind: (database: Database.Database, actionRef: ProviderActionOwnerRef) => void;
  }>> = [
    {
      owner: "launch",
      bind: (database, actionRef) => database.prepare(`
        INSERT INTO project_session_launch_custody VALUES (?,?,?)
      `).run(actionRef.runId, actionRef.adapterId, actionRef.actionId),
    },
    {
      owner: "provider_agent",
      bind: (database, actionRef) => database.prepare(`
        INSERT INTO provider_agent_custody(run_id,adapter_id,action_id) VALUES (?,?,?)
      `).run(actionRef.runId, actionRef.adapterId, actionRef.actionId),
    },
    {
      owner: "lifecycle",
      bind: (database, actionRef) => database.prepare(`
        INSERT INTO lifecycle_rotation_custodies(
          run_id,provider_action_adapter_id,provider_action_id
        ) VALUES (?,?,?)
      `).run(actionRef.runId, actionRef.adapterId, actionRef.actionId),
    },
    {
      owner: "herdr",
      bind: () => undefined,
    },
    {
      owner: "chair_recovery",
      bind: (database, actionRef) => {
        database.prepare(`INSERT INTO chair_bridge_losses VALUES (?,?)`).run("loss-1", actionRef.runId);
        database.prepare(`INSERT INTO chair_bridge_recovery_custody VALUES (?,?,?,?)`)
          .run("loss-1", "rebind", actionRef.adapterId, actionRef.actionId);
      },
    },
    {
      owner: "chair_live_handoff",
      bind: (database, actionRef) => database.prepare(`
        INSERT INTO chair_live_handoff_custody VALUES (?,?,?)
      `).run(actionRef.runId, actionRef.adapterId, actionRef.actionId),
    },
    {
      owner: "operator_control",
      bind: bindOperatorControl,
    },
    {
      owner: "certifying_review",
      bind: (database, actionRef) => {
        const reservationDigest = `sha256:${"e".repeat(64)}`;
        database.prepare(`UPDATE provider_actions SET finding_capacity_reservation_digest=? WHERE adapter_id=? AND action_id=?`)
          .run(reservationDigest, actionRef.adapterId, actionRef.actionId);
        database.prepare(`INSERT INTO review_finding_capacity_reservations VALUES (?,?,?,?,?,?)`)
          .run(actionRef.runId, actionRef.adapterId, actionRef.actionId, 7, "native", reservationDigest);
        database.prepare(`INSERT INTO provider_action_routes VALUES (?,?,?,?,?,?)`)
          .run(actionRef.runId, actionRef.adapterId, actionRef.actionId, 1, 7, "native");
      },
    },
  ];

  function canonicalRef(owner: ProviderActionOwner): ProviderActionOwnerRef {
    return owner === "herdr" ? { ...ref, adapterId: "herdr-control-v1" } : ref;
  }

  it.each(specialised)("classifies $owner only from its persisted identity", ({ owner, bind }) => {
    const database = openClassifierDatabase();
    const actionRef = canonicalRef(owner);
    insertAction(database, owner === "herdr"
      ? { adapterId: "herdr-control-v1", operation: "herdr:agent.wake" }
      : {});
    bind(database, actionRef);
    expect(classifyProviderActionOwner(database, actionRef)).toBe(owner);
  });

  it.each(specialised)("rejects $owner at the live generic owner assertion", ({ owner, bind }) => {
    const database = openClassifierDatabase();
    const actionRef = canonicalRef(owner);
    insertAction(database, owner === "herdr"
      ? { adapterId: "herdr-control-v1", operation: "herdr:agent.wake" }
      : {});
    bind(database, actionRef);
    expect(() => assertProviderActionOwner(database, actionRef, "generic")).toThrowError(
      expect.objectContaining({ expectedOwner: "generic", actualOwner: owner }),
    );
  });

  const independentAmbiguousPairs = specialised.flatMap((left, leftIndex) =>
    specialised.slice(leftIndex + 1)
      .filter((right) => new Set([left.owner, right.owner]).size === 2)
      .filter((right) => !(
        (left.owner === "provider_agent" && right.owner === "lifecycle") ||
        (left.owner === "lifecycle" && right.owner === "provider_agent")
      ))
      .map((right) => ({
        pair: `${left.owner} + ${right.owner}`,
        left,
        right,
      })),
  );

  it.each(independentAmbiguousPairs)(
    "fails closed when the independent $pair owner families both claim one canonical record",
    ({ left, right }) => {
      const database = openClassifierDatabase();
      const actionRef = canonicalRef(left.owner === "herdr" || right.owner === "herdr" ? "herdr" : "generic");
      insertAction(database, {
        runId: actionRef.runId,
        adapterId: actionRef.adapterId,
        actionId: actionRef.actionId,
        operation: actionRef.adapterId === "herdr-control-v1" ? "herdr:agent.wake" : "send_turn",
      });
      left.bind(database, actionRef);
      right.bind(database, actionRef);
      expect(classifyProviderActionOwner(database, actionRef)).toBe("integrity_failed");
    },
  );

  const identityMutations = [
    {
      field: "runId",
      mutate: (actionRef: ProviderActionOwnerRef): ProviderActionOwnerRef => ({
        ...actionRef,
        runId: `${actionRef.runId}-mutated`,
      }),
    },
    {
      field: "adapterId",
      mutate: (actionRef: ProviderActionOwnerRef): ProviderActionOwnerRef => ({
        ...actionRef,
        adapterId: `${actionRef.adapterId}-mutated`,
      }),
    },
    {
      field: "actionId",
      mutate: (actionRef: ProviderActionOwnerRef): ProviderActionOwnerRef => ({
        ...actionRef,
        actionId: `${actionRef.actionId}-mutated`,
      }),
    },
  ] as const;
  const canonicalFamilies = [
    {
      owner: "generic" as const,
      bind: (_database: Database.Database, _actionRef: ProviderActionOwnerRef): void => undefined,
    },
    ...specialised,
  ];
  const singleFieldMutations = canonicalFamilies.flatMap((family) =>
    identityMutations.map((mutation) => ({ ...family, ...mutation })),
  );

  it.each(singleFieldMutations)(
    "fails closed for $owner when only required identity join field $field changes",
    ({ owner, bind, mutate }) => {
      const database = openClassifierDatabase();
      const actionRef = canonicalRef(owner);
      insertAction(database, {
        runId: actionRef.runId,
        adapterId: actionRef.adapterId,
        actionId: actionRef.actionId,
        operation: owner === "herdr" ? "herdr:agent.wake" : "send_turn",
      });
      bind(database, actionRef);
      expect(classifyProviderActionOwner(database, actionRef)).toBe(owner);
      expect(classifyProviderActionOwner(database, mutate(actionRef))).toBe("integrity_failed");
    },
  );

  it.each([
    {
      owner: "lifecycle",
      arrange: (database: Database.Database) => {
        insertAction(database);
        database.prepare(`
          INSERT INTO lifecycle_rotation_custodies(
            run_id,provider_action_adapter_id,provider_action_id,agent_id,
            replacement_contract_digest,staged_capability_hash,target_principal_generation
          ) VALUES (?,?,?,?,?,?,?)
        `).run(ref.runId, ref.adapterId, ref.actionId, "agent-1", "contract-1", "cap-1", 2);
        database.prepare(`
          INSERT INTO provider_agent_custody(
            run_id,adapter_id,action_id,operation,actor_agent_id,target_agent_id,
            bridge_contract_digest,capability_hash,principal_generation,requested_provider_session_ref
          ) VALUES (?,?,?,?,?,?,?,?,?,NULL)
        `).run(ref.runId, ref.adapterId, ref.actionId, "spawn", "agent-1", "agent-1", "contract-1", "cap-1", 2);
      },
      mutate: (database: Database.Database) => database.prepare(`
        UPDATE provider_agent_custody SET capability_hash='persisted-corruption'
         WHERE run_id=? AND adapter_id=? AND action_id=?
      `).run(ref.runId, ref.adapterId, ref.actionId),
    },
    {
      owner: "operator_control",
      arrange: (database: Database.Database) => {
        insertAction(database);
        bindOperatorControl(database);
      },
      mutate: (database: Database.Database) => database.prepare(`
        UPDATE operator_control_provider_action_bindings SET source_payload_hash='persisted-corruption'
         WHERE run_id=? AND adapter_id=? AND action_id=?
      `).run(ref.runId, ref.adapterId, ref.actionId),
    },
    {
      owner: "certifying_review",
      arrange: (database: Database.Database) => {
        const reservationDigest = `sha256:${"9".repeat(64)}`;
        insertAction(database, { reservationDigest });
        database.prepare(`INSERT INTO review_finding_capacity_reservations VALUES (?,?,?,?,?,?)`)
          .run(ref.runId, ref.adapterId, ref.actionId, 7, "native", reservationDigest);
        database.prepare(`INSERT INTO provider_action_routes VALUES (?,?,?,?,?,?)`)
          .run(ref.runId, ref.adapterId, ref.actionId, 1, 7, "native");
      },
      mutate: (database: Database.Database) => database.prepare(`
        UPDATE provider_action_routes SET slot='persisted-corruption'
         WHERE run_id=? AND adapter_id=? AND action_id=?
      `).run(ref.runId, ref.adapterId, ref.actionId),
    },
  ] as const)("reaches the persisted $owner joins when persisted owner fields mutate", ({ owner, arrange, mutate }) => {
    const database = openClassifierDatabase();
    arrange(database);
    expect(classifyProviderActionOwner(database, ref)).toBe(owner);
    mutate(database);
    expect(classifyProviderActionOwner(database, ref)).toBe("integrity_failed");
  });

  it("classifies an unbound ordinary provider action as generic", () => {
    const database = openClassifierDatabase();
    insertAction(database);
    expect(classifyProviderActionOwner(database, ref)).toBe("generic");
  });

  it("derives certifying-review ownership from the action reservation join and only asserts the route flag", () => {
    const database = openClassifierDatabase();
    const reservationDigest = `sha256:${"a".repeat(64)}`;
    insertAction(database, {
      reservationDigest,
      payload: { certifyingReview: false },
    });
    database.prepare(`INSERT INTO review_finding_capacity_reservations VALUES (?,?,?,?,?,?)`)
      .run(ref.runId, ref.adapterId, ref.actionId, 7, "native", reservationDigest);
    database.prepare(`INSERT INTO provider_action_routes VALUES (?,?,?,?,?,?)`)
      .run(ref.runId, ref.adapterId, ref.actionId, 1, 7, "native");
    expect(classifyProviderActionOwner(database, ref)).toBe("certifying_review");
    expect(() => assertProviderActionOwner(database, ref, "generic")).toThrowError(
      expect.objectContaining({ expectedOwner: "generic", actualOwner: "certifying_review" }),
    );
  });

  it.each([
    ["missing action", (_database: Database.Database): void => undefined],
    ["wrong run", (database: Database.Database) => insertAction(database, { runId: "run-other" })],
    ["multiple independent owners", (database: Database.Database) => {
      insertAction(database);
      database.prepare(`INSERT INTO provider_agent_custody(run_id,adapter_id,action_id) VALUES (?,?,?)`).run(ref.runId, ref.adapterId, ref.actionId);
      database.prepare(`INSERT INTO project_session_launch_custody VALUES (?,?,?)`).run(ref.runId, ref.adapterId, ref.actionId);
    }],
    ["certifying binding missing", (database: Database.Database) => {
      insertAction(database, { reservationDigest: `sha256:${"b".repeat(64)}` });
    }],
    ["certifying route missing", (database: Database.Database) => {
      const reservationDigest = `sha256:${"c".repeat(64)}`;
      insertAction(database, { reservationDigest });
      database.prepare(`INSERT INTO review_finding_capacity_reservations VALUES (?,?,?,?,?,?)`)
        .run(ref.runId, ref.adapterId, ref.actionId, 4, "other-primary", reservationDigest);
    }],
    ["certifying route conflicts", (database: Database.Database) => {
      const reservationDigest = `sha256:${"d".repeat(64)}`;
      insertAction(database, { reservationDigest });
      database.prepare(`INSERT INTO review_finding_capacity_reservations VALUES (?,?,?,?,?,?)`)
        .run(ref.runId, ref.adapterId, ref.actionId, 4, "other-primary", reservationDigest);
      database.prepare(`INSERT INTO provider_action_routes VALUES (?,?,?,?,?,?)`)
        .run(ref.runId, ref.adapterId, ref.actionId, 0, null, null);
    }],
    ["route flag cannot establish ownership", (database: Database.Database) => {
      insertAction(database, { payload: { certifyingReview: true } });
      database.prepare(`INSERT INTO provider_action_routes VALUES (?,?,?,?,?,?)`)
        .run(ref.runId, ref.adapterId, ref.actionId, 1, 4, "native");
    }],
  ] as const)("returns integrity_failed for %s", (_label, arrange) => {
    const database = openClassifierDatabase();
    arrange(database);
    expect(classifyProviderActionOwner(database, ref)).toBe("integrity_failed");
  });

  it("keeps payload-only operator correlation generic and never treats it as ownership", () => {
    const database = openClassifierDatabase();
    insertAction(database, { payload: { operatorCustodyId: "correlation-only" } });
    expect(classifyProviderActionOwner(database, ref)).toBe("generic");
  });

  it("derives a missing operator binding from persisted custody and action identities", () => {
    const database = openClassifierDatabase();
    const candidate = {
      schemaVersion: 1,
      operatorId: "operator-1",
      projectId: "project-1",
      projectSessionId: "session-run-1",
      intentDigest: "intent-1",
      adapterId: ref.adapterId,
      runId: ref.runId,
      agentId: "agent-1",
      providerSessionGeneration: 2,
      sourceActionId: "source-1",
      turnLeaseGeneration: 3,
      turnId: "turn-1",
      operation: "steer",
    } as const;
    const actionId = `operator-${sha256(canonicalJson(candidate)).slice(0, 48)}`;
    insertAction(database, { actionId, operation: "steer" });
    database.prepare(`INSERT INTO operator_effect_custody VALUES (?,?,?,?,?,?,?)`)
      .run("custody-1", "operator-1", "project-1", "session-run-1", "steer", "intent-1", '{"kind":"control","action":"steer"}');
    database.prepare(`INSERT INTO agents VALUES (?,?,?)`).run(ref.runId, "agent-1", "resume-1");
    insertAction(database, { actionId: "source-1" });
    database.prepare(`INSERT INTO provider_session_turn_leases VALUES (?,?,?,?,?,?)`)
      .run(ref.runId, "agent-1", ref.adapterId, "source-1", 2, 3);
    database.prepare(`
      UPDATE provider_actions SET target_agent_id='agent-1',provider_session_generation=2,
             turn_lease_generation=3 WHERE adapter_id=? AND action_id=?
    `).run(ref.adapterId, actionId);
    expect(classifyProviderActionOwner(database, { ...ref, actionId })).toBe("integrity_failed");
  });

  it("classifies lifecycle above its subordinate provider-agent bridge binding", () => {
    const database = openClassifierDatabase();
    insertAction(database);
    database.prepare(`
      INSERT INTO lifecycle_rotation_custodies(
        run_id,provider_action_adapter_id,provider_action_id,agent_id,
        replacement_contract_digest,staged_capability_hash,target_principal_generation
      ) VALUES (?,?,?,?,?,?,?)
    `).run(ref.runId, ref.adapterId, ref.actionId, "agent-1", "contract-1", "cap-1", 2);
    database.prepare(`
      INSERT INTO provider_agent_custody(
        run_id,adapter_id,action_id,operation,actor_agent_id,target_agent_id,
        bridge_contract_digest,capability_hash,principal_generation,requested_provider_session_ref
      ) VALUES (?,?,?,?,?,?,?,?,?,NULL)
    `).run(ref.runId, ref.adapterId, ref.actionId, "spawn", "agent-1", "agent-1", "contract-1", "cap-1", 2);
    expect(classifyProviderActionOwner(database, ref)).toBe("lifecycle");
  });

  it("rejects a conflicting provider-agent row under lifecycle custody", () => {
    const database = openClassifierDatabase();
    insertAction(database);
    database.prepare(`
      INSERT INTO lifecycle_rotation_custodies(
        run_id,provider_action_adapter_id,provider_action_id,agent_id,
        replacement_contract_digest,staged_capability_hash,target_principal_generation
      ) VALUES (?,?,?,?,?,?,?)
    `).run(ref.runId, ref.adapterId, ref.actionId, "agent-1", "contract-1", "cap-1", 2);
    database.prepare(`
      INSERT INTO provider_agent_custody(
        run_id,adapter_id,action_id,operation,actor_agent_id,target_agent_id,
        bridge_contract_digest,capability_hash,principal_generation,requested_provider_session_ref
      ) VALUES (?,?,?,?,?,?,?,?,?,NULL)
    `).run(ref.runId, ref.adapterId, ref.actionId, "attach", "other-agent", "other-agent", "contract-2", "cap-2", 9);
    expect(classifyProviderActionOwner(database, ref)).toBe("integrity_failed");
  });

  it("fails a missing expected operator binding with one stable typed error", () => {
    const database = openClassifierDatabase();
    insertAction(database);
    expect(() => assertProviderActionOwner(database, ref, "operator_control")).toThrowError(
      expect.objectContaining({
        name: "ProviderActionOwnerError",
        code: "CAPABILITY_FORBIDDEN",
        expectedOwner: "operator_control",
        actualOwner: "generic",
      }),
    );
    try {
      assertProviderActionOwner(database, ref, "operator_control");
    } catch (error: unknown) {
      expect(error).toBeInstanceOf(ProviderActionOwnerError);
      expect((error as ProviderActionOwnerError).details).toMatchObject({
        runId: ref.runId,
        adapterId: ref.adapterId,
        actionId: ref.actionId,
      });
    }
  });

  it("fails conflicting and multiple operator bindings as integrity_failed", () => {
    const database = openClassifierDatabase();
    insertAction(database);
    database.prepare(`UPDATE provider_actions SET operation='steer' WHERE adapter_id=? AND action_id=?`)
      .run(ref.adapterId, ref.actionId);
    database.prepare(`INSERT INTO operator_control_provider_action_bindings VALUES (?,?,?,?,?,?,?,?,?,?,?,?,?)`)
      .run(
        "missing-custody", ref.runId, ref.adapterId, ref.actionId, ref.adapterId,
        "missing-source", "wrong", "steer", "agent-1", "resume-1", 2, 3, "turn-1",
      );
    expect(classifyProviderActionOwner(database, ref)).toBe("integrity_failed");
    database.prepare(`INSERT INTO operator_control_provider_action_bindings VALUES (?,?,?,?,?,?,?,?,?,?,?,?,?)`)
      .run(
        "other-custody", ref.runId, ref.adapterId, ref.actionId, ref.adapterId,
        "other-source", "wrong", "steer", "agent-1", "resume-1", 2, 3, "turn-1",
      );
    expect(classifyProviderActionOwner(database, ref)).toBe("integrity_failed");
  });

  it("fails an operator binding after its bound provider-session reference changes", () => {
    const database = openClassifierDatabase();
    insertAction(database);
    bindOperatorControl(database);
    expect(classifyProviderActionOwner(database, ref)).toBe("operator_control");
    database.prepare(`UPDATE agents SET provider_session_ref='resume-2' WHERE run_id=? AND agent_id=?`)
      .run(ref.runId, "agent-1");
    expect(classifyProviderActionOwner(database, ref)).toBe("integrity_failed");
  });

  it("rejects an operator binding to non-control custody", () => {
    const database = openClassifierDatabase();
    insertAction(database);
    bindOperatorControl(database);
    database.prepare(`
      UPDATE operator_effect_custody
         SET operation='git',intent_json='{"kind":"git"}'
       WHERE custody_id='custody-1'
    `).run();
    expect(classifyProviderActionOwner(database, ref)).toBe("integrity_failed");
  });
});
