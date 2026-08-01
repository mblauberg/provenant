import { execFile } from "node:child_process";
import { realpath } from "node:fs/promises";
import { fileURLToPath } from "node:url";
import { dirname, join } from "node:path";
import { promisify } from "node:util";

const execFileAsync = promisify(execFile);
const deliveryReceiptProducer = fileURLToPath(
  new URL("../../../../skills/deliver/scripts/delivery_receipt.py", import.meta.url),
);

export async function writeDeliveryRunFixture(input: {
  runDirectory: string;
  runId: string;
  artifactPath: string;
  artifactSha256: string;
  profile?: "analysis" | "agent-product";
  accepted?: boolean;
}): Promise<string> {
  const profile = input.profile ?? "analysis";
  const runDirectory = await realpath(input.runDirectory);
  await execFileAsync("python3", [
    deliveryReceiptProducer,
    "reference",
    "--run-dir", runDirectory,
    "--run-id", input.runId,
    "--profile", profile,
    "--artifact-path", input.artifactPath,
    "--artifact-sha256", input.artifactSha256,
    ...(input.accepted === true ? ["--accepted"] : []),
  ], { cwd: dirname(dirname(runDirectory)) });
  return join(runDirectory, "RUN.json");
}
