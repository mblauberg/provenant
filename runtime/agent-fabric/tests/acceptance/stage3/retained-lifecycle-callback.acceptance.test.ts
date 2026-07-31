import Database from "better-sqlite3";
import { realpathSync } from "node:fs";
import { join } from "node:path";
import { describe, expect, it } from "vitest";

import { eventually } from "../../shared/deadline-wait.ts";
import { createRetainedLifecycleCallbackFixture } from "../../support/lifecycle-testkit.ts";

describe("Stage 3 retained provider lifecycle callback", () => {
  it("returns one durable accepted-suspended receipt from the actual retained send_turn principal", async () => {
    const fixture = await createRetainedLifecycleCallbackFixture();
    try {
      const action = await fixture.dispatchLifecycleCallback();
      expect(action).toMatchObject({
        actionId: "retained-lifecycle:send-turn",
        status: "terminal",
        effectCount: 1,
        result: {
          completed: true,
          lifecycleAcceptance: {
            schemaVersion: 1,
            kind: "accepted-suspended",
            coordinationRunId: fixture.runId,
            action: "rotate",
            agentId: fixture.childAgentId,
            taskId: fixture.task.taskId,
            lifecycle: "suspended",
            sourceProviderGeneration: 1,
            sourcePrincipalGeneration: 1,
            sourceBridgeGeneration: 1,
            targetProviderGeneration: 2,
            targetPrincipalGeneration: 2,
            targetBridgeGeneration: 2,
            custodyRef: {
              runId: fixture.runId,
              agentId: fixture.childAgentId,
              custodyRevision: 1,
            },
          },
        },
      });
      const database = new Database(fixture.databasePath, { readonly: true });
      try {
        expect(database.prepare(`
          SELECT agent.lifecycle,head.disposition_code,head.terminal
            FROM agents agent
            JOIN lifecycle_rotation_custody_heads head
              ON head.run_id=agent.run_id AND head.agent_id=agent.agent_id
           WHERE agent.run_id=? AND agent.agent_id=?
        `).get(fixture.runId, fixture.childAgentId)).toEqual({
          lifecycle: "suspended",
          disposition_code: "none",
          terminal: 0,
        });
        const command = database.prepare(`
          SELECT result_json FROM commands
           WHERE run_id=? AND actor_agent_id=? AND command_id='retained-lifecycle:rotate'
        `).get(fixture.runId, fixture.childAgentId) as { result_json: string };
        expect(JSON.parse(command.result_json)).toEqual(
          (action.result as { lifecycleAcceptance: unknown }).lifecycleAcceptance,
        );
        const replacement = database.prepare(`
          SELECT payload_json FROM provider_actions
           WHERE run_id=? AND adapter_id='fake-lifecycle'
             AND action_id='retained-lifecycle:rotate:spawn'
        `).get(fixture.runId) as { payload_json: string };
        expect(JSON.parse(replacement.payload_json)).toMatchObject({
          cwd: join(realpathSync(fixture.directory), "src", "retained-child"),
          model: "claude-opus-current",
          modelFamily: "anthropic",
          effort: "high",
        });
      } finally {
        database.close();
      }
    } finally {
      await fixture.close();
    }
  });

  it("stays suspended at committing when no external lifecycle receipt authority is configured", async () => {
    const fixture = await createRetainedLifecycleCallbackFixture();
    try {
      await fixture.dispatchLifecycleCallback();
      await eventually(() => {
        const database = new Database(fixture.databasePath, { readonly: true });
        try {
          expect(database.prepare(`
            SELECT agent.lifecycle,head.current_revision,head.state,
                   head.disposition_code,head.terminal,bridge.action_id,
                   bridge.provider_session_generation,bridge.bridge_generation
              FROM agents agent
              JOIN lifecycle_rotation_custody_heads head
                ON head.run_id=agent.run_id AND head.agent_id=agent.agent_id
              JOIN agent_bridge_state bridge
                ON bridge.run_id=agent.run_id AND bridge.agent_id=agent.agent_id
             WHERE agent.run_id=? AND agent.agent_id=?
          `).get(fixture.runId, fixture.childAgentId)).toEqual({
            lifecycle: "suspended",
            current_revision: 5,
            state: "committing",
            disposition_code: "none",
            terminal: 0,
            action_id: "retained-lifecycle:attach",
            provider_session_generation: 1,
            bridge_generation: 1,
          });
          expect(database.prepare(`
            SELECT status,execution_count,effect_count FROM provider_actions
             WHERE adapter_id='fake-lifecycle' AND action_id='retained-lifecycle:rotate:spawn'
          `).get()).toEqual({ status: "terminal", execution_count: 1, effect_count: 1 });
          expect(database.prepare("SELECT COUNT(*) AS count FROM lifecycle_authority_receipts").get())
            .toEqual({ count: 0 });
          expect(database.prepare("SELECT COUNT(*) AS count FROM lifecycle_transition_applies").get())
            .toEqual({ count: 0 });
          const failure = database.prepare(`
            SELECT payload_json FROM events
             WHERE run_id=? AND type='lifecycle-continuation-failed'
             ORDER BY created_at DESC LIMIT 1
          `).get(fixture.runId) as { payload_json: string } | undefined;
          expect(failure).toBeDefined();
          expect(JSON.parse(failure?.payload_json ?? "{}")).toMatchObject({
            message: "lifecycle terminal apply requires an external receipt authority",
          });
        } finally {
          database.close();
        }
      }, 5_000, "retained lifecycle callback durable state");
    } finally {
      await fixture.close();
    }
  });
});
