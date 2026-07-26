import { describe, expect, it, vi } from "vitest";

const fakeProviderToken = `sk-${"A".repeat(24)}`;
const fakePrivateKey = ["-----BEGIN PRIVATE", " KEY-----\nnot-real\n-----END PRIVATE", " KEY-----"].join("");

import {
  FABRIC_OPERATIONS,
  OPERATION_CONTRACT_FIXTURES,
  type NegotiatedOperatorClient,
  type OperatorCapabilityCredential,
  type OperatorActionCommitRequest,
  type OperatorProjectionSnapshot,
  type Intake,
  type ProjectId,
  type ProjectSessionId,
  type Sha256Digest,
  type ScopedGate,
  type Timestamp,
} from "@local/agent-fabric-protocol";

import {
  createProductionConsoleWorkflowPlanner,
  type ConsoleTypedEntryPlanner,
} from "../src/workflow.js";
import { createEmptyViewPages, revisionFromProtocol } from "../src/model.js";
import type { FabricConsoleDataset } from "../src/protocol-adapter.js";

const credential = {
  capabilityId: "capability_workflow",
  token: "afop_secret_must_not_render",
} as OperatorCapabilityCredential;
const projectId = "project_workflow" as ProjectId;
const projectSessionId = "ps_workflow" as ProjectSessionId;
const digest = (`sha256:${"a".repeat(64)}`) as Sha256Digest;
const observedAt = "2026-07-12T00:00:00.000Z" as Timestamp;

function dataset(withSession = true): FabricConsoleDataset {
  const snapshot: OperatorProjectionSnapshot = {
    schemaVersion: 1,
    snapshotRevision: 11,
    readTransactionId: "read_workflow",
    project: {
      freshness: "live",
      source: "fabric",
      revision: 3,
      observedAt,
      value: { projectId, canonicalRoot: "/repo" },
    },
    session: withSession
      ? {
          freshness: "live",
          source: "fabric",
          revision: 8,
          observedAt,
          value: {
            projectSessionId,
            projectId,
            mode: "coordinated",
            state: "active",
            revision: 8,
            generation: 2,
            authorityRef: digest,
            budgetRef: "budget_workflow",
            launchPacketRef: { path: "launch/packet.json" as never, digest },
            membershipRevision: 1,
            origin: { kind: "operator-launch", operatorId: "operator_workflow" as never },
          },
        }
      : {
          freshness: "live",
          source: "fabric",
          revision: 3,
          observedAt,
          value: null,
        },
    runs: {
      freshness: "live",
      source: "fabric",
      revision: 4,
      observedAt,
      value: [],
    },
    attention: {
      freshness: "live",
      source: "fabric",
      revision: 1,
      observedAt,
      value: [],
    },
    capacity: {
      freshness: "live",
      source: "fabric",
      revision: 11,
      observedAt,
      value: {},
    },
    cursor: 0,
    stateDigest: digest,
  };
  return {
    connection: { state: "live", compatibility: { mode: "current" } },
    snapshot,
    snapshotRevision: revisionFromProtocol(11),
    cursor: 0,
    pages: createEmptyViewPages(),
    loadedAtMs: Date.parse(observedAt),
    canMutate: true,
  };
}

function launchRecoveryDataset(state: "launching" | "launch_ambiguous"): FabricConsoleDataset {
  const base = dataset();
  const snapshot = base.snapshot;
  const session = snapshot?.session;
  if (snapshot == null || session?.freshness !== "live" || session.value === null) {
    throw new Error("launch recovery session fixture unavailable");
  }
  return {
    ...base,
    snapshot: {
      ...snapshot,
      session: {
        ...session,
        value: { ...session.value, state },
      },
    },
    pages: {
      ...base.pages,
      project: {
        view: "project",
        rows: [{
          view: "project",
          stableId: projectId,
          revision: revisionFromProtocol(3),
          urgency: "normal",
          freshness: {
            state: "live",
            source: "fabric",
            revision: revisionFromProtocol(3),
            observedAt,
            ageMs: 0,
          },
          summary: {
            kind: "project",
            goal: "Recover launch custody",
            acceptedScopeRef: null,
            repositoryRevision: "abc123",
          },
          detailRef: { kind: "project", projectId, expectedRevision: 3 },
          actionAvailability: { state: "read-only", reason: "state-ineligible" },
        }],
        nextCursor: 1,
        hasMore: false,
        snapshotRevision: revisionFromProtocol(11),
        readTransactionId: "launch-recovery-project-page",
      },
    },
  };
}

function client(overrides: Partial<NegotiatedOperatorClient> = {}): NegotiatedOperatorClient {
  return {
    kind: "operator",
    features: [],
    operations: {},
    close: async () => {},
    ...overrides,
  };
}

function envelope(kind: string, request: unknown): string {
  return JSON.stringify({ kind, request });
}

function sessionBoundFixture(value: unknown): unknown {
  return JSON.parse(
    JSON.stringify(value)
      .replaceAll('"ps_01"', `"${projectSessionId}"`)
      .replaceAll('"project_01"', `"${projectId}"`),
  );
}

describe("typed Console workflow planner", () => {
  it("reviews then creates and attaches a project session through the typed client", async () => {
    const create = vi.fn(async (request) => ({
      ...request,
      state: "draft" as const,
      revision: 1,
      membershipRevision: 1,
      origin: { kind: "operator-launch" as const, operatorId: "operator_workflow" as never },
    }));
    const planner = createProductionConsoleWorkflowPlanner({
      client: client({ projectSessions: { create, get: vi.fn(), transition: vi.fn(), close: vi.fn(), bindMembership: vi.fn() } as never }),
      credential,
      operatorId: "operator_workflow" as never,
      clientId: "console_workflow" as never,
      projectId,
    });
    const raw = envelope("project-session-create", {
      projectSessionId,
      projectId,
      mode: "coordinated",
      generation: 1,
      authorityRef: digest,
      budgetRef: "budget_workflow",
      launchPacketRef: { path: "launch/packet.json", digest },
    });

    const review = await planner.prepare({ raw, dataset: dataset(false), eventId: "palette-create" });
    expect(create).not.toHaveBeenCalled();
    expect(review).toMatchObject({
      kind: "project-session-create",
      stage: "review",
      expectedRevision: "3",
      consequenceClass: "consequential",
      confirmationMode: "explicit",
    });
    expect(JSON.stringify(review)).not.toContain(credential.token);

    const armed = planner.arm(review, "workflow-arm");
    const committed = await planner.commit({ review: armed, eventId: "workflow-confirm" });
    expect(create).toHaveBeenCalledWith(expect.objectContaining({
      projectSessionId,
      command: expect.objectContaining({
        credential,
        expectedRevision: 3,
        provenance: expect.objectContaining({ inputEventId: "workflow-confirm" }),
      }),
    }));
    expect(committed.reconnectProjectSessionId).toBe(projectSessionId);
    expect(committed.review).toMatchObject({ stage: "committed", result: expect.stringContaining(projectSessionId) });
  });

  it("dispatches intake discussion/revision, gate decision and delivery acceptance only after confirmation", async () => {
    const createDraft = vi.fn(async () => OPERATION_CONTRACT_FIXTURES[FABRIC_OPERATIONS.intakeDraftCreate].result as never);
    const submit = vi.fn(async () => OPERATION_CONTRACT_FIXTURES[FABRIC_OPERATIONS.intakeSubmit].result as never);
    const revise = vi.fn(async () => OPERATION_CONTRACT_FIXTURES[FABRIC_OPERATIONS.intakeRevise].result as never);
    const resolve = vi.fn(async () => OPERATION_CONTRACT_FIXTURES[FABRIC_OPERATIONS.scopedGateResolve].result as never);
    const close = vi.fn(async () => OPERATION_CONTRACT_FIXTURES[FABRIC_OPERATIONS.projectSessionClose].result as never);
    const gateRead = vi.fn(async () => ({
      status: "current" as const,
      gate: sessionBoundFixture(OPERATION_CONTRACT_FIXTURES[FABRIC_OPERATIONS.scopedGateResolve].result) as never,
      readTransactionId: "gate-read",
      stateDigest: digest,
    }));
    const planner = createProductionConsoleWorkflowPlanner({
      client: client({
        intakes: { createDraft, read: vi.fn(), submit, revise },
        gates: { create: vi.fn(), resolve },
        projectSessions: { create: vi.fn(), get: vi.fn(), transition: vi.fn(), close, bindMembership: vi.fn() },
        console: {
          readOnly: false,
          launchAvailable: true,
          actions: { preview: vi.fn(), commit: vi.fn(), status: vi.fn(), reconcile: vi.fn() },
          gates: { read: gateRead },
          projection: { viewPage: vi.fn(), runPage: vi.fn(), readDetail: vi.fn() },
        },
      }),
      credential,
      operatorId: "operator_workflow" as never,
      clientId: "console_workflow" as never,
      projectId,
    });

    const fixtures = [
      ["intake-draft-create", OPERATION_CONTRACT_FIXTURES[FABRIC_OPERATIONS.intakeDraftCreate].input, createDraft],
      ["intake-submit", OPERATION_CONTRACT_FIXTURES[FABRIC_OPERATIONS.intakeSubmit].input, submit],
      ["intake-revise", OPERATION_CONTRACT_FIXTURES[FABRIC_OPERATIONS.intakeRevise].input, revise],
      ["scoped-gate-resolve", OPERATION_CONTRACT_FIXTURES[FABRIC_OPERATIONS.scopedGateResolve].input, resolve],
      ["project-session-close", OPERATION_CONTRACT_FIXTURES[FABRIC_OPERATIONS.projectSessionClose].input, close],
    ] as const;

    for (const [kind, fixture, dispatch] of fixtures) {
      const request = { ...(sessionBoundFixture(fixture) as Record<string, unknown>) };
      delete request.command;
      if (kind === "intake-revise") delete request.origin;
      if (kind === "scoped-gate-resolve") delete request.decisionEvidence;
      const review = await planner.prepare({
        raw: envelope(kind, request),
        dataset: dataset(),
        eventId: `palette-${kind}`,
      });
      expect(dispatch).not.toHaveBeenCalled();
      if (kind === "scoped-gate-resolve") {
        expect(review.summary).toContain("Proceed?");
        expect(review.details).toContainEqual({
          label: "Consequence",
          value: "Implementation continues.",
        });
        expect(review.evidence).toContain(`docs/spec.md@${digest}`);
      }
      const result = await planner.commit({ review: planner.arm(review, `arm-${kind}`), eventId: `confirm-${kind}` });
      expect(result.review.stage).toBe("committed");
      expect(dispatch).toHaveBeenCalledTimes(1);
    }
    expect(resolve).toHaveBeenCalledWith(expect.objectContaining({
      decisionEvidence: {
        kind: "typed-console",
        confirmationCommandId: expect.stringMatching(/^console_[a-f0-9]{48}$/u),
      },
    }));
  });

  it("uses the daemon's Preview to review and commit launch/watch/control/stop actions", async () => {
    const actionFixture = OPERATION_CONTRACT_FIXTURES[FABRIC_OPERATIONS.operatorActionPreview];
    const previewResult = {
      ...(actionFixture.result as Record<string, unknown>),
      confirmationMode: "echo",
    };
    const preview = vi.fn(async () => previewResult as never);
    const commit = vi.fn(async () => OPERATION_CONTRACT_FIXTURES[FABRIC_OPERATIONS.operatorActionCommit].result as never);
    const planner = createProductionConsoleWorkflowPlanner({
      client: client({
        console: {
          readOnly: false,
          launchAvailable: true,
          actions: { preview, commit, status: vi.fn(), reconcile: vi.fn() },
          gates: { read: vi.fn() },
          projection: { viewPage: vi.fn(), runPage: vi.fn(), readDetail: vi.fn() },
        },
      }),
      credential,
      operatorId: "operator_workflow" as never,
      clientId: "console_workflow" as never,
      projectId,
    });
    const fixtureInput = actionFixture.input as Record<string, unknown>;
    const raw = envelope("operator-action", { intent: fixtureInput.intent });

    const review = await planner.prepare({ raw, dataset: dataset(), eventId: "palette-action" });
    expect(preview).toHaveBeenCalledTimes(1);
    expect(commit).not.toHaveBeenCalled();
    expect(review).toMatchObject({ stage: "review", source: "daemon-preview" });

    const armed = planner.arm(review, "action-arm");
    await expect(planner.commit({
      review: { ...armed, confirmationMode: "explicit" },
      eventId: "forged-action-confirm",
    })).rejects.toThrow("stale or not distinct");
    await planner.commit({
      review: armed,
      eventId: "action-confirm",
      echoText: armed.previewDigest,
    });
    expect(commit).toHaveBeenCalledWith(expect.objectContaining({
      previewId: expect.any(String),
      confirmation: expect.objectContaining({ kind: "echo" }),
    }));
  });

  it("builds an evidence acceptance workflow from a guided intake ID without raw JSON", async () => {
    const intake: Intake = {
      intakeId: "intake_guided_accept" as never,
      projectId,
      projectSessionId,
      coordinationRunId: "run_guided_accept" as never,
      revision: 4,
      state: "awaiting-human",
      dedupeKey: "guided-accept",
      summary: "Review the accepted scope",
      artifactRefs: [],
      gateIds: [],
    };
    const read = vi.fn(async () => intake);
    const revise = vi.fn(async (request) => ({ ...intake, ...request, revision: 5 } as never));
    const planner = createProductionConsoleWorkflowPlanner({
      client: client({
        intakes: { createDraft: vi.fn(), read, submit: vi.fn(), revise },
      }),
      credential,
      operatorId: "operator_workflow" as never,
      clientId: "console_workflow" as never,
      projectId,
    });
    const selected = dataset();
    const artifactRef = { path: "docs/spec.md" as never, digest };
    const binding = {
      view: "evidence" as const,
      itemId: "evidence_guided_accept",
      itemRevision: revisionFromProtocol(7),
      projectionRevision: revisionFromProtocol(11),
    };
    const guidedDataset: FabricConsoleDataset = {
      ...selected,
      inspection: {
        kind: "artifact",
        state: "current",
        binding,
        readTransactionId: "guided-artifact-read",
        result: {
          artifactRef,
          evidenceRevision: 7,
          evidenceKind: "artifact",
          sourceKind: "project-file",
          publisherKind: "agent",
          publisherRef: "chair-guided",
          projectSessionId,
          coordinationRunId: intake.coordinationRunId,
          taskId: null,
          createdAt: observedAt,
          mediaType: "text/markdown",
          content: "approved scope",
          totalBytes: 14,
          totalLines: 1,
          renderedTotalBytes: 14,
          renderedTotalLines: 1,
          renderedArtifactDigest: digest,
          transformation: "none",
          terminalNeutralised: true,
          capabilityValuesRedacted: true,
          credentialValuesRedacted: true,
          pages: [{ pageIndex: 0, lineFragment: "whole", pageContentDigest: digest, bytes: 14 }],
          coverage: { complete: true, verified: true, pageCount: 1 },
          reviewDisposition: "eligible",
        },
      },
    };

    const review = await planner.prepareGuided({
      action: "accept",
      binding,
      raw: "intake=intake_guided_accept",
      dataset: guidedDataset,
      eventId: "guided-accept-open",
    });

    expect(read).toHaveBeenCalledWith({ credential, intakeId: "intake_guided_accept" });
    expect(revise).not.toHaveBeenCalled();
    expect(review).toMatchObject({
      kind: "intake-revise",
      stage: "review",
      details: expect.arrayContaining([
        { label: "state", value: '"accepted"' },
        { label: "acceptedScopeRef", value: expect.stringContaining("docs/spec.md") },
      ]),
    });

    await planner.commit({
      review: planner.arm(review, "guided-accept-arm"),
      eventId: "guided-accept-confirm",
    });
    expect(revise).toHaveBeenCalledWith(expect.objectContaining({
      intakeId: "intake_guided_accept",
      expectedRevision: 4,
      state: "accepted",
      artifactRefs: [artifactRef],
      acceptedScopeRef: artifactRef,
    }));
  });

  it("reviews an editable closed launch packet from exact accepted evidence before preparing the session", async () => {
    const acceptedScopeRef = { path: "docs/accepted-scope.md" as never, digest };
    const intake: Intake = {
      intakeId: "intake_implement" as never,
      projectId,
      projectSessionId,
      coordinationRunId: "run_implement" as never,
      revision: 5,
      state: "accepted",
      dedupeKey: "implement",
      summary: "Implement accepted scope",
      artifactRefs: [acceptedScopeRef],
      gateIds: [],
      acceptedScopeRef,
    };
    let loseFirstResponse = true;
    const prepareImplementation = vi.fn(async (request) => {
      if (loseFirstResponse) {
        loseFirstResponse = false;
        throw new Error("response lost after durable preparation");
      }
      return {
        projectSession: {
          ...request,
          mode: "coordinated",
          state: "awaiting_launch",
          revision: 9,
          generation: 2,
          authorityRef: digest,
          budgetRef: "budget_workflow",
          launchPacketRef: request.launchPacketRef,
          membershipRevision: 1,
          origin: { kind: "operator-launch", operatorId: "operator_workflow" },
        },
        launchPacketRef: request.launchPacketRef,
        resourcePlanRef: request.resourcePlanRef,
        acceptedScopeRef,
      };
    });
    const planner = createProductionConsoleWorkflowPlanner({
      client: client({
        intakes: { createDraft: vi.fn(), read: vi.fn(async () => intake), submit: vi.fn(), revise: vi.fn() },
        projectSessions: { prepareImplementation } as never,
      }),
      credential,
      operatorId: "operator_workflow" as never,
      clientId: "console_workflow" as never,
      projectId,
      now: () => Date.parse("2026-07-17T01:00:00.000Z"),
    });
    const selected = dataset();
    const session = selected.snapshot?.session;
    if (session?.freshness !== "live" || session.value === null) throw new Error("session fixture unavailable");
    const binding = {
      view: "evidence" as const,
      itemId: "evidence_implement",
      itemRevision: revisionFromProtocol(7),
      projectionRevision: revisionFromProtocol(11),
    };
    const guidedDataset: FabricConsoleDataset = {
      ...selected,
      snapshot: {
        ...selected.snapshot!,
        session: { ...session, value: { ...session.value, state: "draft" } },
      },
      inspection: {
        kind: "artifact",
        state: "current",
        binding,
        readTransactionId: "implement-artifact-read",
        result: {
          artifactRef: acceptedScopeRef,
          evidenceRevision: 7,
          evidenceKind: "artifact",
          sourceKind: "project-file",
          publisherKind: "agent",
          publisherRef: "chair-implement",
          projectSessionId,
          coordinationRunId: intake.coordinationRunId,
          taskId: null,
          createdAt: observedAt,
          mediaType: "text/markdown",
          content: "accepted scope",
          totalBytes: 14,
          totalLines: 1,
          renderedTotalBytes: 14,
          renderedTotalLines: 1,
          renderedArtifactDigest: digest,
          transformation: "none",
          terminalNeutralised: true,
          capabilityValuesRedacted: true,
          credentialValuesRedacted: true,
          pages: [{ pageIndex: 0, lineFragment: "whole", pageContentDigest: digest, bytes: 14 }],
          coverage: { complete: true, verified: true, pageCount: 1 },
          reviewDisposition: "eligible",
        },
      },
    };
    const resourcePlan = {
      schemaVersion: 1,
      projectId,
      projectSessionId,
      runId: "run_implement",
      budgetRef: "budget_workflow",
      scopes: {
        project: { scopeId: "scope_project", limits: { provider_calls: 2, concurrent_turns: 2 } },
        projectSession: { scopeId: "scope_session", limits: { provider_calls: 2, concurrent_turns: 2 } },
        coordinationRun: { scopeId: "scope_run", limits: { provider_calls: 1, concurrent_turns: 1 } },
      },
      launchReservation: { amounts: { provider_calls: 1, concurrent_turns: 1 } },
    };
    const packet = {
      schemaVersion: 1,
      projectId,
      projectSessionId,
      runId: "run_implement",
      chairAgentId: "chair_implement",
      projectRunDirectory: ".agent-run/run_implement",
      topologyMode: "coordinated",
      budgetRef: "budget_workflow",
      resourcePlanRef: { path: ".agent-run/run_implement/launch-resources.json", digest },
      chairAuthority: {
        schemaVersion: 2,
        approval: { approvedBy: "human-maintainer", evidenceId: "accepted-scope", evidenceDigest: digest },
        workspaceRoots: ["."],
        sourcePaths: ["runtime/agent-fabric-console"],
        artifactPaths: [".agent-run/run_implement"],
        actions: [FABRIC_OPERATIONS.getTask],
        deniedPaths: [],
        deniedActions: [],
        prohibitedActions: ["deployment"],
        disclosure: { level: "scoped", scopes: ["local", "approved-provider"] },
        secrets: { access: "none" },
        deployment: { allowed: false },
        irreversibleActions: { allowed: false },
        network: { toolEgress: "none" },
        expiresAt: "2026-07-18T01:00:00.000Z",
        budget: { provider_calls: 1, concurrent_turns: 1 },
      },
      provider: {
        adapterId: "claude-agent-sdk",
        actionId: "provider_implement",
        contractDigest: digest,
        inputSchemaId: "provider-launch.v1",
        input: {
          model: "claude-opus",
          prompt: "Reopen docs/accepted-scope.md and implement it.",
        },
      },
    };
    const raw = [
      "intake=intake_implement",
      "launch-packet-path=.agent-run/run_implement/launch-packet.json",
      `packet=${JSON.stringify(packet)}`,
      `resource-plan=${JSON.stringify(resourcePlan)}`,
    ].join("\n");

    const review = await planner.prepareGuided({
      action: "implement",
      binding,
      raw,
      dataset: guidedDataset,
      eventId: "implement-open",
    });

    expect(prepareImplementation).not.toHaveBeenCalled();
    expect(review).toMatchObject({
      kind: "project-session-launch-packet-prepare",
      stage: "review",
      confirmationMode: "explicit",
      details: expect.arrayContaining([
        { label: "Provider route", value: expect.stringContaining("claude-agent-sdk") },
        { label: "Provider input", value: expect.stringContaining("Reopen docs/accepted-scope.md") },
        { label: "Worktree/write scopes", value: expect.stringContaining("runtime/agent-fabric-console") },
        { label: "Budget", value: expect.stringContaining("concurrent_turns") },
      ]),
    });
    const unsafePacket = {
      ...packet,
      provider: {
        ...packet.provider,
        input: {
          apiKey: "not-real-api-key",
          password: "not-real-password",
          prompt: `Use ${fakeProviderToken} then https://operator:not-real@example.invalid/task`,
          keyBlock: fakePrivateKey,
        },
      },
    };
    const unsafeReview = await planner.prepareGuided({
      action: "implement",
      binding,
      raw: [
        "intake=intake_implement",
        "launch-packet-path=.agent-run/run_implement/launch-packet.json",
        `packet=${JSON.stringify(unsafePacket)}`,
        `resource-plan=${JSON.stringify(resourcePlan)}`,
      ].join("\n"),
      dataset: guidedDataset,
      eventId: "implement-redacted-preview",
    });
    const unsafeRendered = JSON.stringify(unsafeReview.details);
    for (const secret of [
      "not-real-api-key",
      "not-real-password",
      fakeProviderToken,
      "operator:not-real",
      fakePrivateKey.split("\n", 1)[0],
    ]) expect(unsafeRendered).not.toContain(secret);
    expect(unsafeRendered).toContain("[REDACTED credential]");
    await expect(planner.prepareGuided({
      action: "implement",
      binding,
      raw: raw.replace("2026-07-18T01:00:00.000Z", "2026-07-17T00:59:59.000Z"),
      dataset: guidedDataset,
      eventId: "implement-expired",
    })).rejects.toThrow("authority has expired");
    expect(prepareImplementation).not.toHaveBeenCalled();

    const editedRaw = raw
      .replace("and implement it.", "and implement it, then write the handoff.")
      .replace("claude-opus", "claude-sonnet");
    const editedReview = await planner.prepareGuided({
      action: "implement",
      binding,
      raw: editedRaw,
      dataset: guidedDataset,
      eventId: "implement-edit",
    });
    expect(editedReview.previewDigest).not.toBe(review.previewDigest);
    expect(editedReview.details).toContainEqual({
      label: "Launch packet",
      value: expect.stringContaining("sha256:"),
    });
    expect(editedReview.details).toContainEqual({
      label: "Provider input",
      value: expect.stringMatching(/claude-sonnet.*write the handoff/),
    });

    const committed = await planner.commit({
      review: planner.arm(editedReview, "implement-arm"),
      eventId: "implement-confirm",
    });
    expect(committed.review.stage).toBe("committed");
    expect(prepareImplementation).toHaveBeenCalledTimes(2);
    expect(prepareImplementation).toHaveBeenCalledWith(expect.objectContaining({
      acceptedScopeRef,
      launchPacket: expect.objectContaining({
        provider: expect.objectContaining({
          input: expect.objectContaining({
            model: "claude-sonnet",
            prompt: expect.stringContaining("write the handoff"),
          }),
        }),
      }),
    }));
    const committedCommandId = prepareImplementation.mock.calls[1]?.[0].command.commandId;

    const restartedPrepare = vi.fn(async (request) => ({
      projectSession: {
        ...request,
        mode: "coordinated" as const,
        state: "awaiting_launch" as const,
        revision: 9,
        generation: 2,
        authorityRef: digest,
        budgetRef: "budget_workflow",
        launchPacketRef: request.launchPacketRef,
        membershipRevision: 1,
        origin: { kind: "operator-launch" as const, operatorId: "operator_workflow" },
      },
      launchPacketRef: request.launchPacketRef,
      resourcePlanRef: request.resourcePlanRef,
      acceptedScopeRef,
    }));
    const restarted = createProductionConsoleWorkflowPlanner({
      client: client({
        intakes: { createDraft: vi.fn(), read: vi.fn(async () => intake), submit: vi.fn(), revise: vi.fn() },
        projectSessions: { prepareImplementation: restartedPrepare } as never,
      }),
      credential,
      operatorId: "operator_workflow" as never,
      clientId: "console_workflow_restarted" as never,
      projectId,
      now: () => Date.parse("2026-07-17T01:00:00.000Z"),
    });
    const committedRequest = prepareImplementation.mock.calls[1]?.[0];
    if (committedRequest === undefined) throw new Error("implementation request unavailable");
    const restartedDataset: FabricConsoleDataset = {
      ...guidedDataset,
      snapshot: {
        ...guidedDataset.snapshot!,
        session: {
          ...session,
          value: {
            ...session.value,
            state: "awaiting_launch",
            revision: 9,
            launchPacketRef: committedRequest.launchPacketRef,
          },
        },
      },
    };
    const recoveryReview = await restarted.prepareGuided({
      action: "implement",
      binding,
      raw: editedRaw,
      dataset: restartedDataset,
      eventId: "implement-reconnect-open",
    });
    const recovered = await restarted.commit({
      review: restarted.arm(recoveryReview, "implement-reconnect-arm"),
      eventId: "implement-reconnect-confirm",
    });
    expect(recovered.review.stage).toBe("committed");
    expect(restartedPrepare.mock.calls[0]?.[0].command.commandId).toBe(committedCommandId);
    expect(restartedPrepare.mock.calls[0]?.[0].command.expectedRevision).toBe(8);

    let ambiguousAttempts = 0;
    const ambiguousPrepare = vi.fn(async (request) => {
      ambiguousAttempts += 1;
      if (ambiguousAttempts <= 2) throw new Error("transport unavailable");
      return {
        projectSession: {
          ...request,
          mode: "coordinated" as const,
          state: "awaiting_launch" as const,
          revision: 9,
          generation: 2,
          authorityRef: digest,
          budgetRef: "budget_workflow",
          launchPacketRef: request.launchPacketRef,
          membershipRevision: 1,
          origin: { kind: "operator-launch" as const, operatorId: "operator_workflow" },
        },
        launchPacketRef: request.launchPacketRef,
        resourcePlanRef: request.resourcePlanRef,
        acceptedScopeRef,
      };
    });
    const ambiguousPlanner = createProductionConsoleWorkflowPlanner({
      client: client({
        intakes: { createDraft: vi.fn(), read: vi.fn(async () => intake), submit: vi.fn(), revise: vi.fn() },
        projectSessions: { prepareImplementation: ambiguousPrepare } as never,
      }),
      credential,
      operatorId: "operator_workflow" as never,
      clientId: "console_workflow_ambiguous" as never,
      projectId,
      now: () => Date.parse("2026-07-17T01:00:00.000Z"),
    });
    const ambiguousReview = await ambiguousPlanner.prepareGuided({
      action: "implement",
      binding,
      raw: editedRaw,
      dataset: guidedDataset,
      eventId: "implement-ambiguous-open",
    });
    const ambiguous = await ambiguousPlanner.commit({
      review: ambiguousPlanner.arm(ambiguousReview, "implement-ambiguous-arm"),
      eventId: "implement-ambiguous-confirm",
    });
    expect(ambiguous.review.stage).toBe("ambiguous");
    const observed = await ambiguousPlanner.observe({
      review: ambiguous.review,
      eventId: "implement-ambiguous-observe",
    });
    expect(observed.stage).toBe("committed");
    expect(new Set(ambiguousPrepare.mock.calls.map(([request]) => request.command.commandId))).toEqual(
      new Set([committedCommandId]),
    );
  });

  it("fails a guided decision before mutation when the supplied intake is cross-session", async () => {
    const wrongSessionIntake: Intake = {
      intakeId: "intake_wrong_session" as never,
      projectId,
      projectSessionId: "ps_other" as never,
      coordinationRunId: "run_other" as never,
      revision: 2,
      state: "awaiting-human" as const,
      dedupeKey: "wrong-session",
      summary: "Wrong session",
      artifactRefs: [],
      gateIds: [],
    };
    const read = vi.fn(async () => wrongSessionIntake);
    const revise = vi.fn();
    const planner = createProductionConsoleWorkflowPlanner({
      client: client({
        intakes: { createDraft: vi.fn(), read, submit: vi.fn(), revise },
      }),
      credential,
      operatorId: "operator_workflow" as never,
      clientId: "console_workflow" as never,
      projectId,
    });

    await expect(planner.prepareGuided({
      action: "defer",
      binding: {
        view: "evidence",
        itemId: "evidence_wrong_session",
        itemRevision: revisionFromProtocol(1),
        projectionRevision: revisionFromProtocol(11),
      },
      raw: "intake=intake_wrong_session",
      dataset: dataset(),
      eventId: "guided-wrong-session",
    })).rejects.toThrow("another project session");
    expect(revise).not.toHaveBeenCalled();
  });

  it("routes typed launch, Git and promotion forms only through the registered entry planner", async () => {
    const intent = {
      kind: "promotion" as const,
      projectSessionId,
      coordinationRunId: "run_promotion" as never,
      gateId: "gate_promotion" as never,
      expectedGateRevision: 3,
      expectedGateStatus: "approved" as const,
      releaseBinding: {
        acceptedDeliveryReceiptRef: { path: "receipts/accepted.json" as never, digest },
        artifactDigest: digest,
        promotionAction: "publish-package",
        target: "registry:staging",
      },
    };
    const buildIntent = vi.fn(async () => ({ intent, expectedRevision: 3 }));
    const typedEntryPlanner: ConsoleTypedEntryPlanner = {
      capabilities: {
        launch: { state: "available" },
        git: { state: "unavailable", reason: "git-contract-not-negotiated" },
        promotion: { state: "available" },
      },
      buildIntent,
    };
    const preview = vi.fn(async (request) => ({
      previewId: "preview_guided_promotion",
      previewRevision: 1,
      previewDigest: digest,
      intent: request.intent,
      intentDigest: digest,
      beforeStateDigest: digest,
      consequenceClass: "promotion" as const,
      evidenceRefs: [],
      gateIds: [intent.gateId],
      confirmationMode: "explicit" as const,
      expiresAt: "2099-01-01T00:00:00.000Z" as Timestamp,
    }));
    const planner = createProductionConsoleWorkflowPlanner({
      client: client({
        console: {
          readOnly: false,
          launchAvailable: true,
          actions: { preview, commit: vi.fn(), status: vi.fn(), reconcile: vi.fn() },
          gates: { read: vi.fn() },
          projection: { viewPage: vi.fn(), runPage: vi.fn(), readDetail: vi.fn() },
        },
      }),
      credential,
      operatorId: "operator_workflow" as never,
      clientId: "console_workflow" as never,
      projectId,
      typedEntryPlanner,
    });
    const binding = {
      view: "project" as const,
      itemId: projectId,
      itemRevision: revisionFromProtocol(3),
      projectionRevision: revisionFromProtocol(11),
    };

    const review = await planner.prepareGuided({
      action: "promotion",
      binding,
      raw: "gate=gate_promotion\ntarget=registry:staging",
      dataset: dataset(),
      eventId: "guided-promotion",
    });

    expect(planner.capabilities).toMatchObject({
      launch: { state: "available" },
      git: { state: "unavailable", reason: "git-contract-not-negotiated" },
      promotion: { state: "available" },
    });
    expect(buildIntent).toHaveBeenCalledWith(expect.objectContaining({
      kind: "promotion",
      fields: { gate: "gate_promotion", target: "registry:staging" },
      binding,
    }));
    expect(preview).toHaveBeenCalledWith(expect.objectContaining({ intent }));
    expect(review).toMatchObject({ source: "daemon-preview", consequenceClass: "promotion" });

    await expect(planner.prepareGuided({
      action: "git",
      binding,
      raw: "operation=stage",
      dataset: dataset(),
      eventId: "guided-git-unavailable",
    })).rejects.toThrow("git-contract-not-negotiated");
    expect(buildIntent).toHaveBeenCalledTimes(1);
  });

  it("commits a dedicated Launch preview without calling the forbidden generic preview API", async () => {
    const intent = {
      kind: "project-session-launch" as const,
      projectId,
      projectSessionId,
      expectedProjectRevision: 3,
      expectedSessionRevision: 8,
      expectedSessionGeneration: 2,
      trustRecordDigest: digest,
      launchPacketRef: { path: "launch/packet.json" as never, digest },
      authorityRef: digest,
      budgetRef: "budget_workflow",
      resourcePlanRef: { path: "launch/resource-plan.json" as never, digest },
      providerAdapterId: "claude-agent-sdk",
      providerActionId: "provider_launch_workflow" as never,
      providerContractDigest: digest,
      resourceStateDigest: digest,
    };
    const daemonPreview = {
      previewId: "preview_guided_launch",
      previewRevision: 1,
      previewDigest: digest,
      intent,
      intentDigest: digest,
      beforeStateDigest: digest,
      consequenceClass: "consequential" as const,
      evidenceRefs: [],
      gateIds: [],
      confirmationMode: "explicit" as const,
      expiresAt: "2099-01-01T00:00:00.000Z" as Timestamp,
    };
    const genericPreview = vi.fn(async () => {
      throw new Error("project-session-launch previews are server-authored only");
    });
    const preparedJournal = {
      schemaVersion: 1 as const,
      projectSessionId,
      coordinationRunId: "run_launch_workflow" as never,
      actionRef: { adapterId: "claude-agent-sdk", actionId: intent.providerActionId },
      providerContractDigest: digest,
      custodyAttemptGeneration: 1,
      journalRevision: 1,
      journalState: "prepared" as const,
      outcomeKind: null,
      outcomeDigest: null,
    };
    const receipt = {
      commandId: "command_launch_commit",
      previewId: daemonPreview.previewId,
      previewRevision: 1,
      intentDigest: digest,
      beforeStateDigest: digest,
      afterStateDigest: digest,
      launchProviderActionJournalRef: preparedJournal,
      evidenceRefs: [],
      committedAt: observedAt,
    };
    const commit = vi.fn(async (request: OperatorActionCommitRequest) => ({
      ...receipt,
      commandId: request.command.commandId,
    }));
    const status = vi.fn(async (request: { commandId: string }) => ({
      status: "committed" as const,
      commandId: request.commandId,
      receipt: { ...receipt, commandId: request.commandId },
      launchProviderActionJournalRef: {
        ...preparedJournal,
        journalRevision: 2,
        journalState: "terminal" as const,
        outcomeKind: "terminal-success" as const,
        outcomeDigest: digest,
      },
      seatProvisioning: {
        schemaVersion: 1 as const,
        projectSessionId,
        sessionRevision: 9,
        sessionGeneration: 2,
        coordinationRunId: preparedJournal.coordinationRunId,
        runRevision: 2,
        chairAgentId: "chair_launch_workflow" as never,
        chairGeneration: 1,
        chairLeaseId: "chair:run_launch_workflow:1" as never,
      },
    }));
    const planner = createProductionConsoleWorkflowPlanner({
      client: client({
        console: {
          readOnly: false,
          launchAvailable: true,
          actions: { preview: genericPreview, commit, status, reconcile: vi.fn() },
          gates: { read: vi.fn() },
          projection: { viewPage: vi.fn(), runPage: vi.fn(), readDetail: vi.fn() },
        },
      }),
      credential,
      operatorId: "operator_workflow" as never,
      clientId: "console_workflow" as never,
      projectId,
      typedEntryPlanner: {
        capabilities: {
          launch: { state: "available" },
          git: { state: "unavailable", reason: "git-unavailable" },
          promotion: { state: "unavailable", reason: "promotion-unavailable" },
        },
        buildIntent: vi.fn(async () => ({ intent, expectedRevision: 8, daemonPreview })),
      },
    });
    const binding = {
      view: "project" as const,
      itemId: projectId,
      itemRevision: revisionFromProtocol(3),
      projectionRevision: revisionFromProtocol(11),
    };

    const review = await planner.prepareGuided({
      action: "launch",
      binding,
      raw: "",
      dataset: dataset(),
      eventId: "guided-launch",
    });
    expect(genericPreview).not.toHaveBeenCalled();

    const result = await planner.commit({
      review: planner.arm(review, "guided-launch-arm"),
      eventId: "guided-launch-confirm",
    });
    expect(commit).toHaveBeenCalledWith(expect.objectContaining({
      previewId: daemonPreview.previewId,
      expectedPreviewRevision: daemonPreview.previewRevision,
      confirmation: expect.objectContaining({ kind: "explicit" }),
    }));
    expect(status).toHaveBeenCalledWith(expect.objectContaining({
      commandId: expect.stringMatching(/^console_/u),
    }));
    expect(result.review.stage).toBe("committed");
  });

  it("keeps pending Launch custody observable and treats terminal no-effect as failure", async () => {
    const intent = {
      kind: "project-session-launch" as const,
      projectId,
      projectSessionId,
      expectedProjectRevision: 3,
      expectedSessionRevision: 8,
      expectedSessionGeneration: 2,
      trustRecordDigest: digest,
      launchPacketRef: { path: "launch/packet.json" as never, digest },
      authorityRef: digest,
      budgetRef: "budget_workflow",
      resourcePlanRef: { path: "launch/resource-plan.json" as never, digest },
      providerAdapterId: "claude-agent-sdk",
      providerActionId: "provider_launch_settlement" as never,
      providerContractDigest: digest,
      resourceStateDigest: digest,
    };
    const preparedJournal = {
      schemaVersion: 1 as const,
      projectSessionId,
      coordinationRunId: "run_launch_settlement" as never,
      actionRef: { adapterId: "claude-agent-sdk", actionId: intent.providerActionId },
      providerContractDigest: digest,
      custodyAttemptGeneration: 1,
      journalRevision: 1,
      journalState: "prepared" as const,
      outcomeKind: null,
      outcomeDigest: null,
    };
    const daemonPreview = {
      previewId: "preview_launch_settlement",
      previewRevision: 1,
      previewDigest: digest,
      intent,
      intentDigest: digest,
      beforeStateDigest: digest,
      consequenceClass: "consequential" as const,
      evidenceRefs: [],
      gateIds: [],
      confirmationMode: "explicit" as const,
      expiresAt: "2099-01-01T00:00:00.000Z" as Timestamp,
    };
    const receipt = {
      commandId: "command_launch_settlement",
      previewId: daemonPreview.previewId,
      previewRevision: 1,
      intentDigest: digest,
      beforeStateDigest: digest,
      afterStateDigest: digest,
      launchProviderActionJournalRef: preparedJournal,
      evidenceRefs: [],
      committedAt: observedAt,
    };
    const pendingStatus = {
      status: "pending" as const,
      commandId: receipt.commandId,
      intentDigest: digest,
      phase: "observing" as const,
      attemptGeneration: 1,
      launchProviderActionJournalRef: { ...preparedJournal, journalState: "accepted" as const },
    };
    const noEffectStatus = {
      status: "committed" as const,
      commandId: receipt.commandId,
      receipt,
      launchProviderActionJournalRef: {
        ...preparedJournal,
        journalRevision: 2,
        journalState: "terminal" as const,
        outcomeKind: "terminal-no-effect" as const,
        outcomeDigest: digest,
      },
    };
    const status = vi.fn()
      .mockImplementationOnce(async (request: { commandId: string }) => ({
        ...pendingStatus,
        commandId: request.commandId,
      }))
      .mockImplementationOnce(async (request: { commandId: string }) => ({
        ...noEffectStatus,
        commandId: request.commandId,
        receipt: { ...receipt, commandId: request.commandId },
      }));
    const reconcile = vi.fn();
    const commit = vi.fn(async (_request: OperatorActionCommitRequest) => receipt);
    const planner = createProductionConsoleWorkflowPlanner({
      client: client({
        console: {
          readOnly: false,
          launchAvailable: true,
          actions: {
            preview: vi.fn(),
            commit,
            status,
            reconcile,
          },
          gates: { read: vi.fn() },
          projection: { viewPage: vi.fn(), runPage: vi.fn(), readDetail: vi.fn() },
        },
      }),
      credential,
      operatorId: "operator_workflow" as never,
      clientId: "console_workflow" as never,
      projectId,
      typedEntryPlanner: {
        capabilities: {
          launch: { state: "available" },
          git: { state: "unavailable", reason: "git-unavailable" },
          promotion: { state: "unavailable", reason: "promotion-unavailable" },
        },
        buildIntent: vi.fn(async () => ({ intent, expectedRevision: 8, daemonPreview })),
      },
    });
    const review = await planner.prepareGuided({
      action: "launch",
      binding: {
        view: "project",
        itemId: projectId,
        itemRevision: revisionFromProtocol(3),
        projectionRevision: revisionFromProtocol(11),
      },
      raw: "",
      dataset: dataset(),
      eventId: "launch-settlement-review",
    });
    const pending = await planner.commit({
      review: planner.arm(review, "launch-settlement-arm"),
      eventId: "launch-settlement-commit",
    });
    expect(pending.review).toMatchObject({ stage: "pending", failure: null });
    const committedCommandId = commit.mock.calls[0]?.[0].command.commandId;
    expect(committedCommandId).toMatch(/^console_launch_/u);
    expect(status.mock.calls[0]?.[0].commandId).toBe(committedCommandId);

    const reopenedStatus = vi.fn(async (request: { commandId: string }) => ({
      ...pendingStatus,
      commandId: request.commandId,
    }));
    const reopenedBuildIntent = vi.fn(async () => {
      throw new Error("restart recovery must not prepare a second launch");
    });
    const reopenedPlanner = createProductionConsoleWorkflowPlanner({
      client: client({
        console: {
          readOnly: false,
          launchAvailable: true,
          actions: {
            preview: vi.fn(),
            commit: vi.fn(),
            status: reopenedStatus,
            reconcile: vi.fn(),
          },
          gates: { read: vi.fn() },
          projection: { viewPage: vi.fn(), runPage: vi.fn(), readDetail: vi.fn() },
        },
      }),
      credential,
      operatorId: "operator_workflow" as never,
      clientId: "console_after_restart" as never,
      projectId,
      typedEntryPlanner: {
        capabilities: {
          launch: { state: "available" },
          git: { state: "unavailable", reason: "git-unavailable" },
          promotion: { state: "unavailable", reason: "promotion-unavailable" },
        },
        buildIntent: reopenedBuildIntent,
      },
    });
    const reopened = await reopenedPlanner.prepareGuided({
      action: "launch",
      binding: {
        view: "project",
        itemId: projectId,
        itemRevision: revisionFromProtocol(3),
        projectionRevision: revisionFromProtocol(11),
      },
      raw: "",
      dataset: launchRecoveryDataset("launching"),
      eventId: "launch-after-process-restart",
    });
    expect(reopened).toMatchObject({ stage: "pending", failure: null });
    expect(reopenedStatus).toHaveBeenCalledWith(expect.objectContaining({
      commandId: committedCommandId,
    }));
    expect(reopenedBuildIntent).not.toHaveBeenCalled();

    const settled = await planner.observe({
      review: pending.review,
      eventId: "launch-settlement-observe",
    });
    expect(status).toHaveBeenLastCalledWith(expect.objectContaining({
      commandId: expect.stringMatching(/^console_/u),
    }));
    expect(reconcile).not.toHaveBeenCalled();
    expect(settled).toMatchObject({
      stage: "conflict",
      failure: "LAUNCH_TERMINAL_NO_EFFECT",
    });

    commit.mockRejectedValueOnce(new Error("commit response lost"));
    status.mockImplementationOnce(async (request: { commandId: string }) => ({
      ...noEffectStatus,
      commandId: request.commandId,
      receipt: { ...receipt, commandId: request.commandId },
    }));
    const lostResponseReview = await planner.prepareGuided({
      action: "launch",
      binding: {
        view: "project",
        itemId: projectId,
        itemRevision: revisionFromProtocol(3),
        projectionRevision: revisionFromProtocol(11),
      },
      raw: "",
      dataset: dataset(),
      eventId: "launch-lost-response-review",
    });
    const recoveredLostResponse = await planner.commit({
      review: planner.arm(lostResponseReview, "launch-lost-response-arm"),
      eventId: "launch-lost-response-commit",
    });
    expect(recoveredLostResponse.review).toMatchObject({
      stage: "conflict",
      failure: "LAUNCH_TERMINAL_NO_EFFECT",
    });

    status
      .mockRejectedValueOnce(new Error("first status unavailable"))
      .mockRejectedValueOnce(new Error("recovery status unavailable"))
      .mockImplementationOnce(async (request: { commandId: string }) => ({
        ...noEffectStatus,
        commandId: request.commandId,
        receipt: { ...receipt, commandId: request.commandId },
      }));
    const unavailableReview = await planner.prepareGuided({
      action: "launch",
      binding: {
        view: "project",
        itemId: projectId,
        itemRevision: revisionFromProtocol(3),
        projectionRevision: revisionFromProtocol(11),
      },
      raw: "",
      dataset: dataset(),
      eventId: "launch-status-unavailable-review",
    });
    const unavailable = await planner.commit({
      review: planner.arm(unavailableReview, "launch-status-unavailable-arm"),
      eventId: "launch-status-unavailable-commit",
    });
    expect(unavailable.review).toMatchObject({
      stage: "pending",
      failure: "LAUNCH_STATUS_UNAVAILABLE",
    });
    const recoveredUnavailable = await planner.observe({
      review: unavailable.review,
      eventId: "launch-status-recovered-observe",
    });
    expect(recoveredUnavailable).toMatchObject({
      stage: "conflict",
      failure: "LAUNCH_TERMINAL_NO_EFFECT",
    });
    expect(reconcile).not.toHaveBeenCalled();
  });

  it("previews and commits a revision-bound Attention request-changes decision", async () => {
    const gate = sessionBoundFixture(
      OPERATION_CONTRACT_FIXTURES[FABRIC_OPERATIONS.scopedGateResolve].result,
    ) as ScopedGate;
    const readGate = vi.fn(async () => ({
      status: "current" as const,
      gate,
      readTransactionId: "guided-gate-read",
      stateDigest: digest,
    }));
    const resolve = vi.fn(async () => gate);
    const planner = createProductionConsoleWorkflowPlanner({
      client: client({
        gates: { create: vi.fn(), resolve },
        console: {
          readOnly: false,
          launchAvailable: false,
          actions: { preview: vi.fn(), commit: vi.fn(), status: vi.fn(), reconcile: vi.fn() },
          gates: { read: readGate },
          projection: { viewPage: vi.fn(), runPage: vi.fn(), readDetail: vi.fn() },
        },
      }),
      credential,
      operatorId: "operator_workflow" as never,
      clientId: "console_workflow" as never,
      projectId,
    });
    const binding = {
      view: "attention" as const,
      itemId: "attention_gate",
      itemRevision: revisionFromProtocol(1),
      projectionRevision: revisionFromProtocol(11),
    };
    const selected = dataset();
    const attentionDataset: FabricConsoleDataset = {
      ...selected,
      pages: {
        ...selected.pages,
        attention: {
          ...selected.pages.attention,
          rows: [{
            view: "attention",
            stableId: binding.itemId,
            revision: binding.itemRevision,
            urgency: "safety-integrity",
            freshness: {
              state: "live",
              source: "fabric",
              revision: binding.itemRevision,
              observedAt,
              ageMs: 0,
            },
            summary: {
              kind: "attention",
              label: "Decision",
              priority: "safety-integrity",
              title: gate.question,
              gateBinding: {
                gateId: gate.gateId,
                gateRevision: gate.revision,
                coordinationRunId: gate.coordinationRunId,
              },
              nativeNotification: {
                kind: "feature-unavailable",
                status: "unavailable",
                reason: "feature-not-negotiated",
              },
            } as never,
            detailRef: {
              kind: "run",
              projectSessionId,
              coordinationRunId: gate.coordinationRunId,
              expectedRevision: 1,
            },
            actionAvailability: { state: "read-only", reason: "state-ineligible" },
          }],
          snapshotRevision: revisionFromProtocol(11),
          readTransactionId: "guided-attention-page",
        },
      },
    };

    const review = await planner.prepareGuided({
      action: "request-changes",
      binding,
      raw: "",
      dataset: attentionDataset,
      eventId: "guided-gate-request-changes",
    });

    expect(readGate).toHaveBeenCalledWith(expect.objectContaining({
      gateId: gate.gateId,
      expectedRevision: gate.revision,
    }));
    expect(resolve).not.toHaveBeenCalled();
    expect(review).toMatchObject({
      kind: "scoped-gate-resolve",
      stage: "review",
      expectedRevision: String(gate.revision),
    });

    await planner.commit({
      review: planner.arm(review, "guided-gate-arm"),
      eventId: "guided-gate-confirm",
    });
    expect(resolve).toHaveBeenCalledWith(expect.objectContaining({
      gateId: gate.gateId,
      status: "rejected",
      command: expect.objectContaining({ expectedRevision: gate.revision }),
    }));

    readGate.mockResolvedValueOnce({
      status: "changed",
      expectedRevision: gate.revision,
      gate: { ...gate, revision: gate.revision + 1 } as never,
      readTransactionId: "guided-gate-changed",
      stateDigest: digest,
    } as never);
    await expect(planner.prepareGuided({
      action: "accept",
      binding,
      raw: "",
      dataset: attentionDataset,
      eventId: "guided-gate-stale",
    })).rejects.toThrow("gate binding is stale");

    readGate.mockResolvedValueOnce({
      status: "current",
      gate: { ...gate, coordinationRunId: "run_other" } as never,
      readTransactionId: "guided-gate-wrong-run",
      stateDigest: digest,
    });
    await expect(planner.prepareGuided({
      action: "defer",
      binding,
      raw: "",
      dataset: attentionDataset,
      eventId: "guided-gate-wrong-run",
    })).rejects.toThrow("gate binding is stale");
    expect(resolve).toHaveBeenCalledTimes(1);
  });

  it.each(["discuss", "request-changes"] as const)(
    "previews and commits guided %s with an atomic successor chair request",
    async (action) => {
      const intake = {
        intakeId: "intake_discussion" as never,
        projectId,
        projectSessionId,
        coordinationRunId: "run_discussion" as never,
        revision: 4,
        state: "awaiting-human" as const,
        dedupeKey: "discussion",
        summary: "Review the current plan",
        artifactRefs: [{ path: "docs/spec.md" as never, digest }],
        gateIds: ["gate_discussion" as never],
        chairRequestSeed: {
          conversationId: "conversation_discussion",
          targetAgentId: "chair_discussion",
          targetProviderSessionRef: "provider_discussion",
          baseRevision: "base_revision",
        },
      } as unknown as Intake;
      const read = vi.fn(async () => intake);
      const revise = vi.fn(async (request) => ({ ...intake, ...request, revision: 5 } as never));
      const planner = createProductionConsoleWorkflowPlanner({
        client: client({
          intakes: { createDraft: vi.fn(), read, submit: vi.fn(), revise },
        }),
        credential,
        operatorId: "operator_workflow" as never,
        clientId: "console_workflow" as never,
        projectId,
      });
      const selectedDigest = (`sha256:${"b".repeat(64)}`) as Sha256Digest;
      const selectedArtifactRef = {
        path: "reports/review.md" as never,
        digest: selectedDigest,
      };
      const binding = {
        view: "evidence" as const,
        itemId: "evidence_discussion_blocked",
        itemRevision: revisionFromProtocol(1),
        projectionRevision: revisionFromProtocol(11),
      };
      const selectedDataset: FabricConsoleDataset = {
        ...dataset(),
        inspection: {
          kind: "artifact",
          state: "current",
          binding,
          readTransactionId: "discussion-artifact-read",
          result: {
            artifactRef: selectedArtifactRef,
            evidenceRevision: 1,
            evidenceKind: "artifact",
            sourceKind: "project-file",
            publisherKind: "agent",
            publisherRef: "chair-discussion",
            projectSessionId,
            coordinationRunId: "run_discussion" as never,
            taskId: null,
            createdAt: observedAt,
            mediaType: "text/markdown",
            content: "review notes",
            totalBytes: 12,
            totalLines: 1,
            renderedTotalBytes: 12,
            renderedTotalLines: 1,
            renderedArtifactDigest: selectedDigest,
            transformation: "none",
            terminalNeutralised: true,
            capabilityValuesRedacted: true,
            credentialValuesRedacted: true,
            pages: [{
              pageIndex: 0,
              lineFragment: "whole",
              pageContentDigest: selectedDigest,
              bytes: 12,
            }],
            coverage: { complete: true, verified: true, pageCount: 1 },
            reviewDisposition: "eligible",
          },
        },
      };

      const review = await planner.prepareGuided({
        action,
        binding,
        raw: `intake=intake_discussion\nsummary=${action === "discuss" ? "Discuss the plan" : "Revise the plan"}`,
        dataset: selectedDataset,
        eventId: `guided-${action}-open`,
      });

      expect(read).toHaveBeenCalledWith({ credential, intakeId: "intake_discussion" });
      expect(revise).not.toHaveBeenCalled();
      expect(review).toMatchObject({
        kind: "intake-revise",
        stage: "review",
        expectedRevision: "4",
      });
      await planner.commit({
        review: planner.arm(review, `guided-${action}-arm`),
        eventId: `guided-${action}-confirm`,
      });
      expect(revise).toHaveBeenCalledWith(expect.objectContaining({
        intakeId: "intake_discussion",
        expectedRevision: 4,
        state: action === "discuss" ? "discussing" : "awaiting-chair",
        artifactRefs: [intake.artifactRefs[0], selectedArtifactRef],
        chairRequest: expect.objectContaining({
          projectSessionId,
          coordinationRunId: "run_discussion",
          task: expect.objectContaining({
            taskRevision: 1,
            expectedArtifactPaths: ["docs/spec.md", "reports/review.md"],
          }),
          request: expect.objectContaining({
            requestRevision: 1,
            conversationId: "conversation_discussion",
            targetAgentId: "chair_discussion",
            targetProviderSessionRef: "provider_discussion",
            intakeBinding: {
              intakeId: "intake_discussion",
              intakeRevision: 5,
              gateIds: ["gate_discussion"],
              artifactDigests: [digest, selectedDigest],
            },
          }),
        }),
      }));
    },
  );

  it("returns a stable safe code for malformed guided fields", async () => {
    const planner = createProductionConsoleWorkflowPlanner({
      client: client({
        intakes: {
          createDraft: vi.fn(),
          read: vi.fn(),
          submit: vi.fn(),
          revise: vi.fn(),
        },
      }),
      credential,
      operatorId: "operator_workflow" as never,
      clientId: "console_workflow" as never,
      projectId,
    });

    await expect(planner.prepareGuided({
      action: "defer",
      binding: {
        view: "evidence",
        itemId: "evidence_malformed_form",
        itemRevision: revisionFromProtocol(1),
        projectionRevision: revisionFromProtocol(11),
      },
      raw: "this is not a named field",
      dataset: dataset(),
      eventId: "guided-malformed-form",
    })).rejects.toMatchObject({
      code: "CONSOLE_GUIDED_KEY_VALUE_REQUIRED",
    });
  });

  it("requires the exact terminal-neutralised artifact confirmation before guided acceptance", async () => {
    const intake: Intake = {
      intakeId: "intake_terminal_neutralised" as never,
      projectId,
      projectSessionId,
      coordinationRunId: "run_terminal_neutralised" as never,
      revision: 2,
      state: "awaiting-human",
      dedupeKey: "terminal-neutralised",
      summary: "Review neutralised source",
      artifactRefs: [],
      gateIds: [],
    };
    const planner = createProductionConsoleWorkflowPlanner({
      client: client({
        intakes: {
          createDraft: vi.fn(),
          read: vi.fn(async () => intake),
          submit: vi.fn(),
          revise: vi.fn(),
        },
      }),
      credential,
      operatorId: "operator_workflow" as never,
      clientId: "console_workflow" as never,
      projectId,
    });
    const binding = {
      view: "evidence" as const,
      itemId: "evidence_terminal_neutralised",
      itemRevision: revisionFromProtocol(9),
      projectionRevision: revisionFromProtocol(11),
    };
    const renderedDigest = (`sha256:${"b".repeat(64)}`) as Sha256Digest;
    const selected: FabricConsoleDataset = {
      ...dataset(),
      inspection: {
        kind: "artifact",
        state: "current",
        binding,
        readTransactionId: "terminal-neutralised-read",
        result: {
          artifactRef: { path: "docs/neutralised.md" as never, digest },
          evidenceRevision: 9,
          evidenceKind: "artifact",
          sourceKind: "project-file",
          publisherKind: "agent",
          publisherRef: "chair",
          projectSessionId,
          coordinationRunId: intake.coordinationRunId,
          taskId: null,
          createdAt: observedAt,
          mediaType: "text/markdown",
          content: "neutralised",
          totalBytes: 12,
          totalLines: 1,
          renderedTotalBytes: 11,
          renderedTotalLines: 1,
          renderedArtifactDigest: renderedDigest,
          transformation: "terminal-neutralised",
          terminalNeutralised: true,
          capabilityValuesRedacted: true,
          credentialValuesRedacted: true,
          pages: [{ pageIndex: 0, lineFragment: "whole", pageContentDigest: renderedDigest, bytes: 11 }],
          coverage: { complete: true, verified: true, pageCount: 1 },
          reviewDisposition: "confirm-terminal-neutralised",
        },
      },
    };
    const input = {
      action: "accept" as const,
      binding,
      raw: "intake=intake_terminal_neutralised",
      dataset: selected,
      eventId: "terminal-neutralised-accept",
    };

    await expect(planner.prepareGuided(input)).rejects.toThrow(
      "exact terminal-neutralised confirmation",
    );
    await expect(planner.prepareGuided({
      ...input,
      artifactConfirmation: {
        evidenceId: binding.itemId,
        evidenceRevision: 9,
        sourceDigest: digest,
        renderedDigest,
        transformation: "terminal-neutralised",
        pageCount: 1,
      },
    })).resolves.toMatchObject({ kind: "intake-revise", stage: "review" });
  });

  it("rejects unsupported or changed payloads before dispatch and never treats arbitrary methods as workflow", async () => {
    const planner = createProductionConsoleWorkflowPlanner({
      client: client(),
      credential,
      operatorId: "operator_workflow" as never,
      clientId: "console_workflow" as never,
      projectId,
    });
    await expect(planner.prepare({
      raw: JSON.stringify({ kind: "arbitrary-rpc", request: { method: "fabric.v1.any" } }),
      dataset: dataset(),
      eventId: "palette-unsafe",
    })).rejects.toThrow("unsupported Console workflow");
    await expect(planner.prepare({
      raw: JSON.stringify({ kind: "project-session-close", request: {}, extra: true }),
      dataset: dataset(),
      eventId: "palette-extra",
    })).rejects.toThrow("exactly kind and request");
    await expect(planner.prepare({
      raw: JSON.stringify({
        kind: "project-session-close",
        request: {
          projectSessionId,
          to: "closed",
          terminalPath: { kind: "cancelled", reason: "fixture" },
          unexpected: "must fail before Review",
        },
      }),
      dataset: dataset(),
      eventId: "palette-invalid-request",
    })).rejects.toThrow();
  });
});
