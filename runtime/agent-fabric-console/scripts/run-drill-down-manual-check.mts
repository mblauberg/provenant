import { readFile } from "node:fs/promises";
import { createInterface } from "node:readline/promises";

import {
  parseRunDrillDownManifest,
  projectionForRunDrillDownFixture,
  scoreRunDrillDownAnswers,
  type RunDrillDownAnswers,
} from "../src/run-drill-down-evaluation.js";
import { runProjectionDetailLines } from "../src/renderer-detail.js";

const manifest = parseRunDrillDownManifest(JSON.parse(await readFile(
  new URL("../evals/run-drill-down-fixtures.v1.json", import.meta.url),
  "utf8",
)));
const requestedId = process.argv[2] ?? manifest.fixtures[0]?.id;
const fixture = manifest.fixtures.find(({ id }) => id === requestedId);
if (fixture === undefined) throw new TypeError(`unknown fixture: ${String(requestedId)}`);

const started = performance.now();
process.stdout.write(`${runProjectionDetailLines(
  projectionForRunDrillDownFixture(fixture),
).join("\n")}\n\n`);
process.stdout.write(
  "Identify lead, currentMilestone, currentTask, remainingWork, nextDependency, blockingIssue and evidencePath.\n",
);
const input = createInterface({ input: process.stdin, output: process.stdout });
const serialized = await input.question("Answers as one JSON object: ");
input.close();
const durationMs = Math.round(performance.now() - started);
const answers = JSON.parse(serialized) as Partial<RunDrillDownAnswers>;
const fieldSuccessRate = scoreRunDrillDownAnswers(fixture, answers);
const passed =
  durationMs <= manifest.maximumIdentificationMs &&
  fieldSuccessRate >= manifest.minimumFieldSuccessRate;
process.stdout.write(JSON.stringify({
  fixtureId: fixture.id,
  durationMs,
  fieldSuccessRate,
  maximumIdentificationMs: manifest.maximumIdentificationMs,
  minimumFieldSuccessRate: manifest.minimumFieldSuccessRate,
  passed,
}) + "\n");
if (!passed) process.exitCode = 1;
