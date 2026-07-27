import { describe, expect, it } from "vitest";

import {
  FabricError,
  LocalOperatorConsoleUnavailableError,
  daemonStartUnavailableReason,
  type LocalOperatorConsoleUnavailableReason,
} from "../../src/index.js";

/**
 * The console bootstrap once collapsed every daemon/transport failure into a
 * generic `start-failed`. These cases prove that each stable, non-secret
 * bootstrap `code`/error name maps to the exact truthful reason the
 * lifecycle-and-failure contract requires, while anything unrecognised stays
 * the honest `start-failed` fallback rather than a fabricated stage.
 */
describe("daemonStartUnavailableReason", () => {
  const cases: ReadonlyArray<
    readonly [unknown, LocalOperatorConsoleUnavailableReason]
  > = [
    [{ code: "SCHEMA_CUTOVER_REQUIRED" }, "schema-cutover-required"],
    [{ code: "BOOTSTRAP_SOCKET_MISMATCH" }, "socket-unavailable"],
    [{ code: "BOOTSTRAP_INCOMPATIBLE_INCUMBENT" }, "daemon-incompatible"],
    [{ code: "PROTOCOL_INCOMPATIBLE" }, "daemon-incompatible"],
    [{ code: "BOOTSTRAP_HANDSHAKE_INVALID" }, "bootstrap-receipt-invalid"],
    [{ code: "BOOTSTRAP_ACTION_MISMATCH" }, "bootstrap-receipt-invalid"],
    [{ code: "BOOTSTRAP_RECEIPT_INVALID" }, "bootstrap-receipt-invalid"],
    [{ code: "BOOTSTRAP_LEASE_EXPIRED" }, "daemon-unreachable"],
    [new FabricError(
      "ARTIFACT_DIGEST_INVALID",
      "the pinned semantic snapshot differs",
      { field: "config/review-profiles/certifying-review-four-slot-v1.json" },
    ), "review-profile-catalogue-invalid"],
    [new FabricError(
      "NOT_FOUND",
      "the required schema source is unavailable",
      { field: "runtime/agent-fabric/schemas/review-profile.v1.schema.json" },
    ), "review-profile-catalogue-invalid"],
    [new FabricError(
      "NOT_FOUND",
      "review profile catalogue lookalike",
      { field: "config/not-a-review-profile-member.json" },
    ), "start-failed"],
    [{
      code: "NOT_FOUND",
      field: "runtime/agent-fabric/schemas/review-profile.v1.schema.json",
    }, "start-failed"],
    [{ name: "BootstrapElectionError" }, "daemon-election-conflict"],
    [{ name: "BootstrapSpawnPhaseError", phase: "spawn" }, "daemon-spawn-failed"],
    [new Error("opaque runtime failure"), "start-failed"],
    ["not-an-object", "start-failed"],
    [null, "start-failed"],
    [undefined, "start-failed"],
  ];

  it.each(cases)("maps %o to its truthful reason", (error, expected) => {
    expect(daemonStartUnavailableReason(error)).toBe(expected);
  });

  it("keeps catalogue discrimination stable when a typed error message is reworded", () => {
    const error = new FabricError(
      "ARTIFACT_DIGEST_INVALID",
      "wording deliberately unrelated to the catalogue",
      { field: "config/review-profiles/certifying-review-four-slot-v1.json" },
    );

    expect(daemonStartUnavailableReason(error)).toBe("review-profile-catalogue-invalid");
  });

  it("assigns each reason a distinct safe code", () => {
    const reasons: readonly LocalOperatorConsoleUnavailableReason[] = [
      "configuration-missing",
      "schema-cutover-required",
      "authority-unavailable",
      "daemon-unreachable",
      "daemon-incompatible",
      "socket-unavailable",
      "daemon-election-conflict",
      "daemon-spawn-failed",
      "bootstrap-receipt-invalid",
      "review-profile-catalogue-invalid",
      "start-failed",
    ];
    const codes = new Set(
      reasons.map((reason) => new LocalOperatorConsoleUnavailableError(reason).code),
    );
    expect(codes.size).toBe(reasons.length);
    // The schema-cutover arm keeps its preserved-database operator message.
    expect(new LocalOperatorConsoleUnavailableError("schema-cutover-required").message)
      .toContain("CUTOVER REQUIRED");
    expect(new LocalOperatorConsoleUnavailableError("review-profile-catalogue-invalid").message)
      .toContain("npm run profile:catalogue:pin");
  });
});
