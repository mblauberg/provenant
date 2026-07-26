import { createRequire } from "node:module";

import { Ajv2020 } from "ajv/dist/2020.js";
import type { FormatsPlugin } from "ajv-formats";
import { describe, expect, it } from "vitest";

import {
  FABRIC_OPERATIONS,
  LAUNCH_CONTRACT_FIXTURES,
  LAUNCH_CONTRACT_SCHEMAS,
  addProtocolSchemaKeywords,
  createOperatorClient,
  negotiateProtocol,
  parseIdentifier,
} from "../src/index.js";

const addFormats = createRequire(import.meta.url)("ajv-formats") as FormatsPlugin;

describe("launch contract schema and fixtures", () => {
  it.each([
    ["projectSessionLaunchIntent", "projectSessionLaunchIntent"],
    ["launchPacketV1", "launchPacketV1"],
    ["launchResourcePlanV1", "launchResourcePlanV1"],
    ["projectSessionLaunchCurrentState", "projectSessionLaunchCurrentState"],
    ["launchAdapterOutcomeV1", "terminalSuccessOutcome"],
    ["launchAdapterOutcomeV1", "terminalNoEffectOutcome"],
    ["launchAdapterOutcomeV1", "ambiguousOutcome"],
    ["providerActionRefV1", "providerActionRefV1"],
    ["launchProviderActionJournalRefV1", "launchProviderActionJournalRefV1"],
  ] as const)("validates %s against fixture %s", (schemaName, fixtureName) => {
    const ajv = new Ajv2020({ strict: false, allErrors: true });
    addFormats(ajv);
    addProtocolSchemaKeywords(ajv);
    const validate = ajv.compile(LAUNCH_CONTRACT_SCHEMAS[schemaName]);
    expect(validate(LAUNCH_CONTRACT_FIXTURES[fixtureName]), ajv.errorsText(validate.errors)).toBe(true);
  });

  it("rejects unknown launch fields and inconsistent provider-action state", () => {
    const ajv = new Ajv2020({ strict: false, allErrors: true });
    addFormats(ajv);
    addProtocolSchemaKeywords(ajv);

    const validatePacket = ajv.compile(LAUNCH_CONTRACT_SCHEMAS.launchPacketV1);
    expect(validatePacket({ ...LAUNCH_CONTRACT_FIXTURES.launchPacketV1, executable: "/tmp/provider" })).toBe(false);

    const validateProviderAction = ajv.compile(LAUNCH_CONTRACT_SCHEMAS.providerActionRefV1);
    expect(validateProviderAction({
      ...LAUNCH_CONTRACT_FIXTURES.providerActionRefV1,
      journalState: "accepted",
    })).toBe(false);
    const validateJournal = ajv.compile(LAUNCH_CONTRACT_SCHEMAS.launchProviderActionJournalRefV1);
    expect(validateJournal({
      ...LAUNCH_CONTRACT_FIXTURES.launchProviderActionJournalRefV1,
      journalState: "accepted",
    })).toBe(false);
  });

  it("keeps schema parity for exact-root chair authority paths", () => {
    const ajv = new Ajv2020({ strict: false, allErrors: true });
    addFormats(ajv);
    addProtocolSchemaKeywords(ajv);
    const validatePacket = ajv.compile(LAUNCH_CONTRACT_SCHEMAS.launchPacketV1);
    const exactRootAuthorityPacket = {
      ...LAUNCH_CONTRACT_FIXTURES.launchPacketV1,
      chairAuthority: {
        ...LAUNCH_CONTRACT_FIXTURES.launchPacketV1.chairAuthority,
        workspaceRoots: ["."],
        sourcePaths: ["."],
        artifactPaths: ["."],
        deniedPaths: ["."],
      },
    };
    expect(validatePacket(exactRootAuthorityPacket), ajv.errorsText(validatePacket.errors)).toBe(true);
    expect(validatePacket({ ...exactRootAuthorityPacket, projectRunDirectory: "." })).toBe(false);
    expect(validatePacket({
      ...exactRootAuthorityPacket,
      resourcePlanRef: { ...exactRootAuthorityPacket.resourcePlanRef, path: "." },
    })).toBe(false);
  });
});

const readOperations = [
  FABRIC_OPERATIONS.scopedGateRead,
  FABRIC_OPERATIONS.projectionViewPage,
  FABRIC_OPERATIONS.projectionRunPage,
  FABRIC_OPERATIONS.projectionDetailRead,
] as const;
const actionOperations = [
  FABRIC_OPERATIONS.operatorActionPreview,
  FABRIC_OPERATIONS.operatorActionCommit,
  FABRIC_OPERATIONS.operatorActionStatus,
  FABRIC_OPERATIONS.operatorActionReconcile,
] as const;

function operatorClient(features: readonly (
  "scoped-gate-read.v1" |
  "operator-projection.v2" |
  "run-scoped-projection.v1" |
  "agent-topology-projection.v1" |
  "work-facts-projection.v1" |
  "operator-actions.v1" |
  "launch-custody.v1"
)[]) {
  return createOperatorClient({
    features,
    principal: {
      kind: "operator",
      operatorId: parseIdentifier<"OperatorId">("operator_01", "principal.operatorId"),
      projectId: parseIdentifier<"ProjectId">("project_01", "principal.projectId"),
      projectAuthorityGeneration: 1,
      principalGeneration: 1,
    },
    allowedOperations: new Set([...readOperations, ...actionOperations]),
    call: () => Promise.reject(new Error("not called")),
    close: () => Promise.resolve(),
  });
}

describe("launch custody feature availability", () => {
  it("requires launch-custody.v1 in addition to generic operator actions", () => {
    expect(operatorClient([
      "scoped-gate-read.v1",
      "operator-projection.v2",
      "run-scoped-projection.v1",
      "agent-topology-projection.v1",
      "work-facts-projection.v1",
      "operator-actions.v1",
    ]).console).toMatchObject({ readOnly: false, launchAvailable: false });
    expect(operatorClient([
      "scoped-gate-read.v1",
      "operator-projection.v2",
      "run-scoped-projection.v1",
      "agent-topology-projection.v1",
      "work-facts-projection.v1",
      "operator-actions.v1",
      "launch-custody.v1",
    ]).console).toMatchObject({ readOnly: false, launchAvailable: true });
  });

  it("negotiates the additive feature without inventing operator-actions.v2", () => {
    expect(negotiateProtocol({
      protocolVersion: 1,
      requiredFeatures: ["launch-custody.v1"],
      optionalFeatures: [],
    }, {
      protocolVersion: 1,
      features: ["operator-actions.v1"],
    })).toStrictEqual({
      ok: false,
      reason: "required-features-unavailable",
      missingFeatures: ["launch-custody.v1"],
    });
  });
});
