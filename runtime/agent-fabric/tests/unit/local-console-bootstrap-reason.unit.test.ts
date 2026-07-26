import { describe, expect, it } from "vitest";

import {
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
      "start-failed",
    ];
    const codes = new Set(
      reasons.map((reason) => new LocalOperatorConsoleUnavailableError(reason).code),
    );
    expect(codes.size).toBe(reasons.length);
    // The schema-cutover arm keeps its preserved-database operator message.
    expect(new LocalOperatorConsoleUnavailableError("schema-cutover-required").message)
      .toContain("CUTOVER REQUIRED");
  });
});
