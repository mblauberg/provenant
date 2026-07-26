import { readFile } from "node:fs/promises";

import { describe, expect, it } from "vitest";

import {
  RUN_DRILL_DOWN_ANSWER_FIELDS,
  evaluateRunDrillDownProxy,
  parseRunDrillDownManifest,
  projectionForRunDrillDownFixture,
  scoreRunDrillDownAnswers,
} from "../src/run-drill-down-evaluation.js";
import { runProjectionDetailLines } from "../src/renderer-detail.js";

async function manifest() {
  return parseRunDrillDownManifest(JSON.parse(await readFile(
    new URL("../evals/run-drill-down-fixtures.v1.json", import.meta.url),
    "utf8",
  )));
}

describe("run drill-down timed fixture", () => {
  it("covers exact coordination, delivery-workstream and independent-session targets", async () => {
    const value = await manifest();

    expect(value).toMatchObject({
      repetitions: 3,
      maximumIdentificationMs: 20_000,
      minimumFieldSuccessRate: 0.9,
    });
    expect(value.fixtures.map(({ id }) => id)).toStrictEqual([
      "coordination-active",
      "delivery-workstream",
      "independent-session",
    ]);
    expect(value.fixtures[1]?.target).toMatchObject({
      kind: "delivery-workstream",
      coordinationRunId: "run-coordinated",
      deliveryRunId: "delivery-console",
      workstreamId: "workstream-console",
    });
  });

  it("passes all three proxy repetitions without making a human timing claim", async () => {
    const value = await manifest();
    const report = evaluateRunDrillDownProxy(value);

    expect(report).toMatchObject({
      observations: 9,
      fieldSuccessRate: 1,
      proxyPassed: true,
      humanTimingClaim: false,
    });
    expect(report.maximumDurationMs).toBeLessThanOrEqual(20_000);
  });

  it("renders every required answer and scores an exact manual response", async () => {
    const fixture = (await manifest()).fixtures[0]!;
    const lines = runProjectionDetailLines(
      projectionForRunDrillDownFixture(fixture),
    );

    for (const field of RUN_DRILL_DOWN_ANSWER_FIELDS) {
      expect(lines.some((line) => line.includes(fixture.expectedAnswers[field]))).toBe(true);
    }
    expect(scoreRunDrillDownAnswers(fixture, fixture.expectedAnswers)).toBe(1);
  });

  it("separates an observed-empty section from an unobserved one", async () => {
    // The server returns every negotiated section, so zero rows means zero were
    // observed. Rendering that as "Unobserved" asserts the section was not
    // observed at all, on the same screen that reports server work-state counts
    // for it -- the three-state collapse this projection exists to prevent.
    const fixture = (await manifest()).fixtures[0]!;
    const lines = runProjectionDetailLines({
      ...projectionForRunDrillDownFixture(fixture),
      work: [],
      agents: [],
      evidencePaths: [],
      evidencePathObservation: "Observed",
      evidencePathsUnobserved: 0,
    });

    expect(lines).toContain("Task ledger: None observed");
    expect(lines).toContain("Topology: None observed");
    expect(lines).toContain("Evidence path: None observed");
    // Other fields may legitimately be Unobserved; these three sections were
    // observed and empty, so none of them may claim otherwise.
    expect(lines.filter((line) =>
      /^(?:Task ledger|Topology|Evidence path): /u.test(line) && line.includes("Unobserved")
    )).toStrictEqual([]);
  });

  it("marks evidence facts it could not observe instead of shortening the list", async () => {
    // A dropped unavailable row would otherwise leave a complete-looking list
    // marked Observed, with the missing facts invisible.
    const fixture = (await manifest()).fixtures[0]!;
    const base = projectionForRunDrillDownFixture(fixture);
    const lines = runProjectionDetailLines({
      ...base,
      evidencePathObservation: "Observed",
      evidencePathsUnobserved: 2,
    });

    expect(lines).toContain("Evidence path: Unobserved x2");
    expect(lines.some((line) => line.startsWith("Evidence path: ") &&
      !line.includes("Unobserved"))).toBe(true);
  });

  it("scores answers only against their labelled field", async () => {
    const value = await manifest();
    const changed = {
      ...value,
      fixtures: value.fixtures.map((fixture, index) => index === 0
        ? {
            ...fixture,
            expectedAnswers: {
              ...fixture.expectedAnswers,
              remainingWork: fixture.expectedAnswers.currentMilestone,
            },
          }
        : fixture),
    };

    expect(evaluateRunDrillDownProxy(changed).fieldSuccessRate).toBeLessThan(1);
  });
});
