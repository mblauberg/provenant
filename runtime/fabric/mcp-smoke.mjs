// Fixture-only end-to-end contract, including a real linked Git worktree.
import { spawnSync } from "node:child_process";
import { createRequire } from "node:module";
import { dirname, join } from "node:path";
const require = createRequire(import.meta.url);
const vitest = join(dirname(require.resolve("vitest/package.json")), "vitest.mjs");
const result = spawnSync(process.execPath, [vitest, "run", "tests/surface.test.ts", "--reporter=verbose"], {
  cwd: import.meta.dirname,
  env: process.env,
  stdio: "inherit",
});
process.exitCode = result.status ?? 1;
