import { readFile, rm } from "node:fs/promises";
import { createServer } from "node:net";
import { join } from "node:path";

import { PROTOCOL_FEATURES, PROTOCOL_LIMITS } from "@local/agent-fabric-protocol";
import Database from "better-sqlite3";
import { afterEach, describe, expect, it } from "vitest";

import { servePublicProtocolConnection } from "../../../src/daemon/public-protocol.ts";
import type { Fabric, FabricClient } from "../../../src/index.ts";
import { eventually } from "../../shared/deadline-wait.ts";
import { TestLifecycleReceiptAuthority } from "../../support/lifecycle-receipt-authority-fake.ts";
import {
  createLifecycleFixture,
  reopenLifecycleFabric,
  writeLifecycleCheckpoint,
  type LifecycleFixture,
} from "../../support/lifecycle-testkit.ts";

const cleanup: Array<() => Promise<void>> = [];

async function restartWithReceiptAuthority(fixture: LifecycleFixture): Promise<{
  fabric: Fabric;
  chair: FabricClient;
  authority: TestLifecycleReceiptAuthority;
}> {
  await fixture.fabric.close();
  const authority = new TestLifecycleReceiptAuthority();
  const socketPath = join(fixture.directory, "lifecycle-recovery.sock");
  const fabric = await reopenLifecycleFabric(fixture, {
    fabricSocketPath: socketPath,
    lifecycleReceiptAuthority: authority,
  });
  const server = createServer((socket) => {
    servePublicProtocolConnection(socket, {
      daemonVersion: "lifecycle-checkpoint-recovery",
      daemonInstanceGeneration: 2,
      offeredFeatures: PROTOCOL_FEATURES,
      limits: PROTOCOL_LIMITS,
      verifyCredential: (credential) => fabric.verifyProtocolCredential(credential),
      dispatch: async (protocolContext, operation, input) =>
        await fabric.dispatchPublicProtocol(protocolContext, operation, input),
    });
  });
  await new Promise<void>((resolve, reject) => {
    server.once("error", reject);
    server.listen(socketPath, () => {
      server.off("error", reject);
      resolve();
    });
  });
  fixture.fabric = fabric;
  cleanup.push(async () => await new Promise<void>((resolve) => server.close(() => resolve())));
  return { fabric, chair: fabric.connect(fixture.capabilities.chair), authority };
}

function rotationHead(fixture: LifecycleFixture, commandId: string): Record<string, unknown> {
  const database = new Database(fixture.databasePath, { readonly: true });
  try {
    return database.prepare(`
      SELECT head.state,head.disposition_code,head.terminal,head.current_revision,
             action.status AS action_status,action.execution_count,action.effect_count
        FROM lifecycle_rotation_custody_heads head
        JOIN lifecycle_rotation_custodies custody
          ON custody.run_id=head.run_id AND custody.agent_id=head.agent_id
         AND custody.custody_id=head.custody_id
        JOIN provider_actions action
          ON action.run_id=custody.run_id
         AND action.adapter_id=custody.provider_action_adapter_id
         AND action.action_id=custody.provider_action_id
       WHERE custody.run_id=? AND custody.agent_id='leader' AND custody.command_id=?
    `).get(fixture.runId, commandId) as Record<string, unknown>;
  } finally {
    database.close();
  }
}

afterEach(async () => {
  await Promise.all(cleanup.splice(0).map((close) => close()));
});

describe("FR-013 Stage 3 checkpointed lifecycle requests", () => {
  it("rejects a self-consistent checkpoint that omits current children and open work", async () => {
    const fixture = await createLifecycleFixture();
    cleanup.push(async () => {
      await fixture.fabric.close();
      await rm(fixture.directory, { recursive: true, force: true });
    });
    const checkpoint = await writeLifecycleCheckpoint(fixture, {
      agentId: "leader",
      inFlightChildren: [],
      openWork: [],
      nextAction: "continue without the omitted work",
    });

    await expect(fixture.leader.requestLifecycle({
      action: "completion-ready",
      agentId: "leader",
      taskId: fixture.leaderTask.taskId,
      taskRevision: fixture.leaderTask.revision,
      checkpoint,
      commandId: "lifecycle:completion-ready:false-current-state",
    })).rejects.toMatchObject({ code: "CHECKPOINT_INCOMPLETE" });
  });

  it("rejects a self-consistent checkpoint for a different provider session", async () => {
    const fixture = await createLifecycleFixture();
    cleanup.push(async () => {
      await fixture.fabric.close();
      await rm(fixture.directory, { recursive: true, force: true });
    });
    const checkpoint = await writeLifecycleCheckpoint(fixture, {
      agentId: "leader",
      inFlightChildren: ["child"],
      openWork: ["leader-task"],
      nextAction: "continue in the wrong provider session",
      providerResumeReference: "different-provider-session",
    });

    await expect(fixture.leader.requestLifecycle({
      action: "completion-ready",
      agentId: "leader",
      taskId: fixture.leaderTask.taskId,
      taskRevision: fixture.leaderTask.revision,
      checkpoint,
      commandId: "lifecycle:completion-ready:false-provider-state",
    })).rejects.toMatchObject({ code: "CHECKPOINT_INCOMPLETE" });
  });

  it("rejects rotation when the durable checkpoint omits resume-critical fields", async () => {
    const fixture = await createLifecycleFixture();
    cleanup.push(async () => {
      await fixture.fabric.close();
      await rm(fixture.directory, { recursive: true, force: true });
    });
    const complete = await writeLifecycleCheckpoint(fixture, {
      agentId: "leader",
      inFlightChildren: ["child"],
      openWork: ["leader-task"],
      nextAction: "reconcile child before rotation",
    });
    const incomplete: unknown = {
      relativePath: complete.relativePath,
      sha256: complete.sha256,
      mailboxWatermark: complete.mailboxWatermark,
      acknowledgedAboveWatermark: complete.acknowledgedAboveWatermark,
      inFlightChildren: complete.inFlightChildren,
    };

    const requestLifecycle: unknown = Reflect.get(fixture.leader, "requestLifecycle");
    if (typeof requestLifecycle !== "function") throw new Error("requestLifecycle is unavailable");

    await expect(
      Reflect.apply(requestLifecycle, fixture.leader, [{
        action: "rotate",
        agentId: "leader",
        taskId: fixture.leaderTask.taskId,
        taskRevision: fixture.leaderTask.revision,
        checkpoint: incomplete,
        commandId: "lifecycle:rotate:incomplete",
      }]),
    ).rejects.toMatchObject({ code: "CHECKPOINT_INCOMPLETE" });
  });

  it("accepts a complete checkpoint as the only portable lifecycle handoff", async () => {
    const fixture = await createLifecycleFixture();
    cleanup.push(async () => {
      await fixture.fabric.close();
      await rm(fixture.directory, { recursive: true, force: true });
    });
    const checkpoint = await writeLifecycleCheckpoint(fixture, {
      agentId: "leader",
      inFlightChildren: ["child"],
      openWork: ["leader-task"],
      nextAction: "resume the task graph from the recorded revision",
    });

    const result = await fixture.leader.requestLifecycle({
      action: "completion-ready",
      agentId: "leader",
      taskId: fixture.leaderTask.taskId,
      taskRevision: fixture.leaderTask.revision,
      checkpoint,
      commandId: "lifecycle:completion-ready:complete",
    });

    expect(result).toMatchObject({ agentId: "leader", lifecycle: "idle" });
  });

  it("passes the verified checkpoint as a bounded provider handoff before rotating", async () => {
    const fixture = await createLifecycleFixture({ retainedAgents: true });
    cleanup.push(async () => {
      await fixture.fabric.close();
      await rm(fixture.directory, { recursive: true, force: true });
    });
    const checkpoint = await writeLifecycleCheckpoint(fixture, {
      agentId: "leader",
      inFlightChildren: ["child"],
      openWork: ["leader-task"],
      nextAction: "resume the exact task and mailbox revisions",
    });

    const accepted = await fixture.leader.requestLifecycle({
      action: "rotate",
      agentId: "leader",
      taskId: fixture.leaderTask.taskId,
      taskRevision: fixture.leaderTask.revision,
      checkpoint,
      commandId: "lifecycle:rotate:provider-handoff",
    });
    expect(accepted).toMatchObject({
      kind: "accepted-suspended",
      lifecycle: "suspended",
      sourceProviderGeneration: 1,
      targetProviderGeneration: 2,
    });

    const database = new Database(fixture.databasePath, { readonly: true });
    const row = database.prepare(`
      SELECT payload_json FROM provider_actions
       WHERE run_id=? AND adapter_id='fake-lifecycle' AND action_id=?
    `).get(fixture.runId, "lifecycle:rotate:provider-handoff:spawn") as { payload_json: string };
    database.close();
    const payload: unknown = JSON.parse(row.payload_json);
    expect(payload).toMatchObject({
      priorResumeReference: fixture.providerSessionMarker,
      generation: 2,
      prompt: expect.stringContaining(checkpoint.sha256),
    });
    expect(JSON.stringify(payload)).toContain("resume the exact task and mailbox revisions");
    expect(Buffer.byteLength((payload as { prompt: string }).prompt, "utf8")).toBeLessThanOrEqual(65_536);
    await eventually(() => {
      expect(rotationHead(fixture, "lifecycle:rotate:provider-handoff")).toMatchObject({
        state: "committing",
        disposition_code: "none",
        terminal: 0,
        action_status: "terminal",
        execution_count: 1,
        effect_count: 1,
      });
    });
    const authorityProof = new Database(fixture.databasePath, { readonly: true });
    expect(authorityProof.prepare("SELECT COUNT(*) AS count FROM lifecycle_authority_receipts").get())
      .toEqual({ count: 0 });
    expect(authorityProof.prepare("SELECT COUNT(*) AS count FROM lifecycle_transition_applies").get())
      .toEqual({ count: 0 });
    authorityProof.close();
  });

  it("reconciles a lost provider result with the same action and never repeats the effect", async () => {
    const fixture = await createLifecycleFixture({ retainedAgents: true, spawnResultLost: true });
    cleanup.push(async () => {
      await fixture.fabric.close();
      await rm(fixture.directory, { recursive: true, force: true });
    });
    const checkpoint = await writeLifecycleCheckpoint(fixture, {
      agentId: "leader",
      inFlightChildren: ["child"],
      openWork: ["leader-task"],
      nextAction: "reconcile the exact lifecycle action",
    });
    const request = {
      action: "rotate" as const,
      agentId: "leader",
      taskId: fixture.leaderTask.taskId,
      taskRevision: fixture.leaderTask.revision,
      checkpoint,
      commandId: "lifecycle:rotate:lost-result",
    };

    await expect(fixture.leader.requestLifecycle(request)).resolves.toMatchObject({
      kind: "accepted-suspended",
      lifecycle: "suspended",
    });
    await eventually(async () => {
      const providerJournal = JSON.parse(await readFile(fixture.providerJournalPath, "utf8")) as {
        actions: Record<string, { status: string; executionCount: number; effectCount: number }>;
      };
      expect(providerJournal.actions["lifecycle:rotate:lost-result:spawn"]).toMatchObject({
        status: "terminal",
        executionCount: 1,
        effectCount: 1,
      });
      expect(rotationHead(fixture, request.commandId)).toMatchObject({
        state: "dispatched",
        action_status: "dispatched",
      });
    });
    const restarted = await restartWithReceiptAuthority(fixture);
    await expect(restarted.fabric.recoverStartupState()).resolves.toMatchObject({ actionsReconciled: 0 });
    await expect(restarted.chair.getAgentLifecycle({ agentId: "leader" })).resolves.toMatchObject({
      lifecycle: "ready",
      providerSessionGeneration: 2,
      currentSource: { state: "finalized", disposition: "adopted" },
    });
    const providerJournal = JSON.parse(await readFile(fixture.providerJournalPath, "utf8")) as {
      actions: Record<string, { executionCount: number; effectCount: number }>;
    };
    expect(providerJournal.actions["lifecycle:rotate:lost-result:spawn"]).toMatchObject({
      executionCount: 1,
      effectCount: 1,
    });
    expect(rotationHead(fixture, request.commandId)).toMatchObject({
      state: "finalized",
      disposition_code: "adopted",
      terminal: 1,
      action_status: "terminal",
      execution_count: 1,
      effect_count: 1,
    });
  });

  it("replays the same lifecycle action after a crash at the durable prepare boundary", async () => {
    const fixture = await createLifecycleFixture({
      retainedAgents: true,
      fault: (label) => {
        if (label === "lifecycle-rotation:prepared") throw new Error("simulated lifecycle process crash");
      },
    });
    cleanup.push(async () => {
      await fixture.fabric.close();
      await rm(fixture.directory, { recursive: true, force: true });
    });
    const checkpoint = await writeLifecycleCheckpoint(fixture, {
      agentId: "leader",
      inFlightChildren: ["child"],
      openWork: ["leader-task"],
      nextAction: "resume from the durable lifecycle prepare",
    });
    const request = {
      action: "rotate" as const,
      agentId: "leader",
      taskId: fixture.leaderTask.taskId,
      taskRevision: fixture.leaderTask.revision,
      checkpoint,
      commandId: "lifecycle:rotate:prepare-crash",
    };

    const pendingAcceptance = fixture.leader.requestLifecycle(request).catch((error: unknown) => error);
    await eventually(() => {
      expect(rotationHead(fixture, request.commandId)).toMatchObject({
        state: "awaiting-boundary",
        action_status: "prepared",
        execution_count: 0,
        effect_count: 0,
      });
    });
    await fixture.fabric.close();
    await Promise.allSettled([pendingAcceptance]);

    const restarted = await restartWithReceiptAuthority(fixture);
    await expect(restarted.fabric.recoverStartupState()).resolves.toMatchObject({ actionsReconciled: 0 });
    await expect(restarted.chair.getAgentLifecycle({ agentId: "leader" })).resolves.toMatchObject({
      lifecycle: "ready",
      providerSessionGeneration: 1,
      currentSource: { state: "finalized", disposition: "no-effect" },
    });
    expect(rotationHead(fixture, request.commandId)).toMatchObject({
      state: "finalized",
      disposition_code: "no-effect",
      terminal: 1,
      action_status: "terminal",
      execution_count: 0,
      effect_count: 0,
    });
  });

  it("keeps an ambiguous lifecycle effect unreconciled without dispatching it again", async () => {
    const fixture = await createLifecycleFixture({ retainedAgents: true, spawnUnresolved: true });
    cleanup.push(async () => {
      await fixture.fabric.close();
      await rm(fixture.directory, { recursive: true, force: true });
    });
    const checkpoint = await writeLifecycleCheckpoint(fixture, {
      agentId: "leader",
      inFlightChildren: ["child"],
      openWork: ["leader-task"],
      nextAction: "wait for exact provider reconciliation",
    });
    const request = {
      action: "rotate" as const,
      agentId: "leader",
      taskId: fixture.leaderTask.taskId,
      taskRevision: fixture.leaderTask.revision,
      checkpoint,
      commandId: "lifecycle:rotate:unresolved",
    };

    await expect(fixture.leader.requestLifecycle(request)).resolves.toMatchObject({
      kind: "accepted-suspended",
      lifecycle: "suspended",
    });
    await eventually(async () => {
      const providerJournal = JSON.parse(await readFile(fixture.providerJournalPath, "utf8")) as {
        actions: Record<string, { status: string; executionCount: number; effectCount: number }>;
      };
      expect(providerJournal.actions["lifecycle:rotate:unresolved:spawn"]).toMatchObject({
        status: "ambiguous",
        executionCount: 1,
        effectCount: 1,
      });
      expect(rotationHead(fixture, request.commandId)).toMatchObject({
        state: "dispatched",
        action_status: "dispatched",
        execution_count: 1,
        effect_count: 0,
      });
    });
    const restarted = await restartWithReceiptAuthority(fixture);
    await expect(restarted.fabric.recoverStartupState()).resolves.toMatchObject({ actionsReconciled: 0 });
    await expect(restarted.chair.getAgentLifecycle({ agentId: "leader" })).resolves.toMatchObject({
      lifecycle: "suspended",
      providerSessionGeneration: 1,
      currentSource: { state: "finalized", disposition: "quarantined" },
    });
    const providerJournal = JSON.parse(await readFile(fixture.providerJournalPath, "utf8")) as {
      actions: Record<string, { status: string; executionCount: number; effectCount: number }>;
    };
    expect(providerJournal.actions["lifecycle:rotate:unresolved:spawn"]).toEqual(expect.objectContaining({
      status: "ambiguous",
      executionCount: 1,
      effectCount: 1,
    }));
    expect(rotationHead(fixture, request.commandId)).toMatchObject({
      state: "finalized",
      disposition_code: "quarantined",
      terminal: 1,
      action_status: "quarantined",
      execution_count: 1,
      effect_count: 0,
    });
  });

  it("coalesces concurrent replay of one lifecycle command onto one provider effect", async () => {
    const fixture = await createLifecycleFixture({ retainedAgents: true, spawnDelayMs: 100 });
    cleanup.push(async () => {
      await fixture.fabric.close();
      await rm(fixture.directory, { recursive: true, force: true });
    });
    const checkpoint = await writeLifecycleCheckpoint(fixture, {
      agentId: "leader",
      inFlightChildren: ["child"],
      openWork: ["leader-task"],
      nextAction: "resume one coalesced lifecycle command",
    });
    const request = {
      action: "rotate" as const,
      agentId: "leader",
      taskId: fixture.leaderTask.taskId,
      taskRevision: fixture.leaderTask.revision,
      checkpoint,
      commandId: "lifecycle:rotate:concurrent-replay",
    };

    const [first, second] = await Promise.all([
      fixture.leader.requestLifecycle(request),
      fixture.leader.requestLifecycle(request),
    ]);
    expect(second).toEqual(first);
    expect(first).toMatchObject({
      kind: "accepted-suspended",
      lifecycle: "suspended",
      targetProviderGeneration: 2,
    });
    await eventually(async () => {
      const providerJournal = JSON.parse(await readFile(fixture.providerJournalPath, "utf8")) as {
        actions: Record<string, { executionCount: number; effectCount: number }>;
      };
      expect(providerJournal.actions["lifecycle:rotate:concurrent-replay:spawn"]).toMatchObject({
        executionCount: 1,
        effectCount: 1,
      });
      expect(rotationHead(fixture, request.commandId)).toMatchObject({
        state: "committing",
        terminal: 0,
        action_status: "terminal",
        execution_count: 1,
        effect_count: 1,
      });
    });
  });

  it("never redispatches after adapter lookup loses a dispatched lifecycle action", async () => {
    const fixture = await createLifecycleFixture({ retainedAgents: true, spawnLookupMissing: true });
    cleanup.push(async () => {
      await fixture.fabric.close();
      await rm(fixture.directory, { recursive: true, force: true });
    });
    const checkpoint = await writeLifecycleCheckpoint(fixture, {
      agentId: "leader",
      inFlightChildren: ["child"],
      openWork: ["leader-task"],
      nextAction: "quarantine missing provider evidence",
    });
    const request = {
      action: "rotate" as const,
      agentId: "leader",
      taskId: fixture.leaderTask.taskId,
      taskRevision: fixture.leaderTask.revision,
      checkpoint,
      commandId: "lifecycle:rotate:lookup-missing",
    };

    await expect(fixture.leader.requestLifecycle(request)).resolves.toMatchObject({
      kind: "accepted-suspended",
      lifecycle: "suspended",
    });
    await eventually(async () => {
      const providerJournal = JSON.parse(await readFile(fixture.providerJournalPath, "utf8")) as {
        sessions: Record<string, { spawnRequests?: number }>;
      };
      expect(Object.values(providerJournal.sessions)).toHaveLength(1);
      expect(Object.values(providerJournal.sessions)[0]).toMatchObject({ spawnRequests: 1 });
      expect(rotationHead(fixture, request.commandId)).toMatchObject({
        state: "dispatched",
        action_status: "dispatched",
        execution_count: 1,
        effect_count: 0,
      });
    });
    const restarted = await restartWithReceiptAuthority(fixture);
    await expect(restarted.fabric.recoverStartupState()).resolves.toMatchObject({ actionsReconciled: 0 });
    await expect(restarted.chair.getAgentLifecycle({ agentId: "leader" })).resolves.toMatchObject({
      lifecycle: "suspended",
      currentSource: { state: "finalized", disposition: "quarantined" },
    });
    const providerJournal = JSON.parse(await readFile(fixture.providerJournalPath, "utf8")) as {
      sessions: Record<string, { spawnRequests?: number }>;
    };
    expect(Object.values(providerJournal.sessions)).toHaveLength(1);
    expect(Object.values(providerJournal.sessions)[0]).toMatchObject({ spawnRequests: 1 });
    expect(rotationHead(fixture, request.commandId)).toMatchObject({
      state: "finalized",
      disposition_code: "quarantined",
      terminal: 1,
      action_status: "quarantined",
      execution_count: 1,
      effect_count: 0,
    });
  });

  it("supersedes a prepared rotation when its checkpoint path is rebound to different bytes", async () => {
    const fixture = await createLifecycleFixture({
      retainedAgents: true,
      fault: (label) => {
        if (label === "lifecycle-rotation:prepared") throw new Error("simulated lifecycle process crash");
      },
    });
    cleanup.push(async () => {
      await fixture.fabric.close();
      await rm(fixture.directory, { recursive: true, force: true });
    });
    const checkpoint = await writeLifecycleCheckpoint(fixture, {
      agentId: "leader",
      inFlightChildren: ["child"],
      openWork: ["leader-task"],
      nextAction: "do not adopt a checkpoint path rebound after acceptance",
    });
    const request = {
      action: "rotate" as const,
      agentId: "leader",
      taskId: fixture.leaderTask.taskId,
      taskRevision: fixture.leaderTask.revision,
      checkpoint,
      commandId: "lifecycle:rotate:checkpoint-rebound",
    };
    await fixture.leader.requestLifecycle(request).catch(() => undefined);
    expect(rotationHead(fixture, request.commandId)).toMatchObject({
      state: "awaiting-boundary",
      action_status: "prepared",
      execution_count: 0,
    });
    const crossed = new Database(fixture.databasePath);
    crossed.prepare(`
      UPDATE lifecycle_checkpoints SET sha256=?
       WHERE run_id=? AND agent_id='leader' AND sha256=?
    `).run("f".repeat(64), fixture.runId, checkpoint.sha256);
    crossed.close();

    const restarted = await restartWithReceiptAuthority(fixture);
    await expect(restarted.fabric.recoverStartupState()).resolves.toMatchObject({ actionsReconciled: 0 });
    await expect(restarted.chair.getAgentLifecycle({ agentId: "leader" })).resolves.toMatchObject({
      lifecycle: "ready",
      providerSessionGeneration: 1,
      currentSource: { state: "finalized", disposition: "superseded" },
    });
    expect(rotationHead(fixture, request.commandId)).toMatchObject({
      state: "finalized",
      disposition_code: "superseded",
      terminal: 1,
      action_status: "terminal",
      execution_count: 0,
      effect_count: 0,
    });
  });

  it("fences source bridge drift while a terminal replacement awaits adopted apply", async () => {
    const fixture = await createLifecycleFixture({ retainedAgents: true });
    cleanup.push(async () => {
      await fixture.fabric.close();
      await rm(fixture.directory, { recursive: true, force: true });
    });
    const checkpoint = await writeLifecycleCheckpoint(fixture, {
      agentId: "leader",
      inFlightChildren: ["child"],
      openWork: ["leader-task"],
      nextAction: "retain the exact source until adopted apply",
    });
    const request = {
      action: "rotate" as const,
      agentId: "leader",
      taskId: fixture.leaderTask.taskId,
      taskRevision: fixture.leaderTask.revision,
      checkpoint,
      commandId: "lifecycle:rotate:source-cas-lost",
    };
    await expect(fixture.leader.requestLifecycle(request)).resolves.toMatchObject({ kind: "accepted-suspended" });
    await eventually(() => expect(rotationHead(fixture, request.commandId)).toMatchObject({
      state: "committing",
      action_status: "terminal",
      execution_count: 1,
      effect_count: 1,
    }));
    const drifted = new Database(fixture.databasePath);
    expect(() => drifted.prepare(`
      UPDATE agent_bridge_state SET revision=revision+1
       WHERE run_id=? AND agent_id='leader' AND bridge_state='active'
    `).run(fixture.runId)).toThrow("INVARIANT_agent_bridge_lifecycle_rotation_target");
    drifted.close();

    const restarted = await restartWithReceiptAuthority(fixture);
    await expect(restarted.fabric.recoverStartupState()).resolves.toMatchObject({ actionsReconciled: 0 });
    await expect(restarted.chair.getAgentLifecycle({ agentId: "leader" })).resolves.toMatchObject({
      lifecycle: "ready",
      providerSessionGeneration: 2,
      currentSource: { state: "finalized", disposition: "adopted" },
    });
    expect(rotationHead(fixture, request.commandId)).toMatchObject({
      state: "finalized",
      disposition_code: "adopted",
      terminal: 1,
      action_status: "terminal",
      execution_count: 1,
      effect_count: 1,
    });
  });

  it("continues recovering later custodies after one custody apply fails", async () => {
    const fixture = await createLifecycleFixture({
      retainedAgents: true,
      fault: (label) => {
        if (label === "lifecycle-rotation:prepared") throw new Error("simulated lifecycle process crash");
      },
    });
    cleanup.push(async () => {
      await fixture.fabric.close();
      await rm(fixture.directory, { recursive: true, force: true });
    });
    const leaderCheckpoint = await writeLifecycleCheckpoint(fixture, {
      agentId: "leader",
      inFlightChildren: ["child"],
      openWork: ["leader-task"],
      nextAction: "leave this failed custody isolated",
    });
    const childCheckpoint = await writeLifecycleCheckpoint(fixture, {
      agentId: "child",
      inFlightChildren: [],
      openWork: ["child-task"],
      nextAction: "recover after the earlier custody failure",
    });
    await fixture.leader.requestLifecycle({
      action: "rotate",
      agentId: "leader",
      taskId: fixture.leaderTask.taskId,
      taskRevision: fixture.leaderTask.revision,
      checkpoint: leaderCheckpoint,
      commandId: "lifecycle:rotate:isolation-leader",
    }).catch(() => undefined);
    await fixture.child.requestLifecycle({
      action: "rotate",
      agentId: "child",
      taskId: fixture.childTask.taskId,
      taskRevision: fixture.childTask.revision,
      checkpoint: childCheckpoint,
      commandId: "lifecycle:rotate:isolation-child",
    }).catch(() => undefined);
    const database = new Database(fixture.databasePath);
    database.prepare(`
      UPDATE delivery_freezes SET reason='foreign-freeze-owner'
       WHERE run_id=? AND agent_id='leader'
    `).run(fixture.runId);
    database.close();

    const restarted = await restartWithReceiptAuthority(fixture);
    await expect(restarted.fabric.recoverStartupState()).resolves.toMatchObject({ actionsReconciled: 0 });
    expect(rotationHead(fixture, "lifecycle:rotate:isolation-leader")).toMatchObject({
      state: "awaiting-boundary",
      terminal: 0,
      action_status: "prepared",
    });
    const childHead = new Database(fixture.databasePath, { readonly: true });
    expect(childHead.prepare(`
      SELECT head.state,head.disposition_code,head.terminal,action.status AS action_status,
             action.execution_count,action.effect_count
        FROM lifecycle_rotation_custody_heads head
        JOIN lifecycle_rotation_custodies custody
          ON custody.run_id=head.run_id AND custody.agent_id=head.agent_id
         AND custody.custody_id=head.custody_id
        JOIN provider_actions action
          ON action.adapter_id=custody.provider_action_adapter_id
         AND action.action_id=custody.provider_action_id
       WHERE custody.run_id=? AND custody.agent_id='child'
         AND custody.command_id='lifecycle:rotate:isolation-child'
    `).get(fixture.runId)).toMatchObject({
      state: "finalized",
      disposition_code: "no-effect",
      terminal: 1,
      action_status: "terminal",
      execution_count: 0,
      effect_count: 0,
    });
    childHead.close();
  });

  it("does not dispatch a prepared replay after its lifecycle freeze changes owner", async () => {
    const fixture = await createLifecycleFixture({
      retainedAgents: true,
      fault: (label) => {
        if (label === "lifecycle-rotation:prepared") throw new Error("simulated lifecycle process crash");
      },
    });
    cleanup.push(async () => {
      await fixture.fabric.close();
      await rm(fixture.directory, { recursive: true, force: true });
    });
    const checkpoint = await writeLifecycleCheckpoint(fixture, {
      agentId: "leader",
      inFlightChildren: ["child"],
      openWork: ["leader-task"],
      nextAction: "preserve the new freeze owner",
    });
    const request = {
      action: "rotate" as const,
      agentId: "leader",
      taskId: fixture.leaderTask.taskId,
      taskRevision: fixture.leaderTask.revision,
      checkpoint,
      commandId: "lifecycle:rotate:prepared-foreign-freeze",
    };
    const pendingAcceptance = fixture.leader.requestLifecycle(request).catch((error: unknown) => error);
    await eventually(() => {
      expect(rotationHead(fixture, request.commandId)).toMatchObject({
        state: "awaiting-boundary",
        action_status: "prepared",
        execution_count: 0,
        effect_count: 0,
      });
    });
    const database = new Database(fixture.databasePath);
    database.prepare(`
      UPDATE delivery_freezes SET reason='interactive-tui-lost'
       WHERE run_id=? AND agent_id='leader'
    `).run(fixture.runId);
    database.close();
    await fixture.fabric.close();
    await Promise.allSettled([pendingAcceptance]);

    const restarted = await restartWithReceiptAuthority(fixture);
    await expect(restarted.fabric.recoverStartupState()).resolves.toMatchObject({ actionsReconciled: 0 });
    const proof = new Database(fixture.databasePath, { readonly: true });
    expect(proof.prepare(`
      SELECT reason FROM delivery_freezes WHERE run_id=? AND agent_id='leader'
    `).get(fixture.runId)).toEqual({ reason: "interactive-tui-lost" });
    expect(proof.prepare(`
      SELECT status,execution_count,effect_count FROM provider_actions
       WHERE run_id=? AND adapter_id='fake-lifecycle' AND action_id=?
    `).get(fixture.runId, "lifecycle:rotate:prepared-foreign-freeze:spawn")).toEqual({
      status: "prepared",
      execution_count: 0,
      effect_count: 0,
    });
    expect(proof.prepare(`
      SELECT COUNT(*) AS count FROM events
       WHERE run_id=? AND type='lifecycle-recovery-custody-failed'
    `).get(fixture.runId)).toEqual({ count: 1 });
    proof.close();
  });
});
