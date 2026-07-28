import { authorityEnvelopeV2Contained, parseAuthorityEnvelopeV2 } from "@local/agent-fabric-protocol";
import { describe, expect, it } from "vitest";

import { observerAuthority, peerSeatAuthority } from "../../src/cli/observer-provision.ts";
import { FABRIC_OPERATIONS } from "../../src/domain/operations.ts";
import { ROOT_AUTHORITY } from "../support/stage1-fixture.ts";

describe("observer authority", () => {
  it("produces an exact zero-budget observer contained by its chair", () => {
    const parent = parseAuthorityEnvelopeV2({
      ...ROOT_AUTHORITY,
      disclosure: { level: "allowed" },
    });

    const observer = observerAuthority(parent);

    expect(observer).toEqual({
      schemaVersion: 2,
      approval: parent.approval,
      workspaceRoots: parent.workspaceRoots,
      sourcePaths: [],
      artifactPaths: [],
      actions: [FABRIC_OPERATIONS.observeEvents],
      deniedPaths: parent.deniedPaths,
      deniedActions: parent.deniedActions,
      prohibitedActions: parent.prohibitedActions,
      disclosure: { level: "scoped", scopes: ["local"] },
      secrets: { access: "none" },
      deployment: { allowed: false },
      irreversibleActions: { allowed: false },
      network: { toolEgress: "none" },
      expiresAt: parent.expiresAt,
      budget: {},
    });
    expect(parseAuthorityEnvelopeV2(observer)).toEqual(observer);
    expect(authorityEnvelopeV2Contained(observer, parent)).toBe(true);
  });

  it.each([
    [{ level: "forbidden" }, { level: "forbidden" }],
    [{ level: "scoped", scopes: ["approved-provider"] }, { level: "forbidden" }],
    [{ level: "scoped", scopes: ["local", "approved-provider"] }, { level: "scoped", scopes: ["local"] }],
  ] as const)("does not widen parent disclosure %#", (disclosure, expected) => {
    const parent = parseAuthorityEnvelopeV2({ ...ROOT_AUTHORITY, disclosure });
    const observer = observerAuthority(parent);
    expect(observer.disclosure).toEqual(expected);
    expect(authorityEnvelopeV2Contained(observer, parent)).toBe(true);
  });
});

describe("peer seat authority", () => {
  it("intersects the messaging and read subset with the parent without widening", () => {
    const parent = parseAuthorityEnvelopeV2({
      ...ROOT_AUTHORITY,
      actions: [
        FABRIC_OPERATIONS.sendMessage,
        FABRIC_OPERATIONS.getMailboxState,
        FABRIC_OPERATIONS.delegateAuthority,
      ],
      disclosure: { level: "allowed" },
    });

    const peer = peerSeatAuthority(parent);

    expect(peer).toMatchObject({
      actions: [
        FABRIC_OPERATIONS.sendMessage,
        FABRIC_OPERATIONS.getMailboxState,
      ],
      sourcePaths: [],
      artifactPaths: [],
      disclosure: { level: "forbidden" },
      secrets: { access: "none" },
      deployment: { allowed: false },
      irreversibleActions: { allowed: false },
      network: { toolEgress: "none" },
      budget: {},
    });
    expect(authorityEnvelopeV2Contained(peer, parent)).toBe(true);
  });

  it("never adds a messaging or read action absent from the parent", () => {
    const parent = parseAuthorityEnvelopeV2({
      ...ROOT_AUTHORITY,
      actions: [FABRIC_OPERATIONS.delegateAuthority],
    });

    expect(peerSeatAuthority(parent).actions).toEqual([]);
    expect(authorityEnvelopeV2Contained(peerSeatAuthority(parent), parent)).toBe(true);
  });
});
