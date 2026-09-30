import { spawnSync } from "node:child_process";
import { mkdirSync, mkdtempSync, realpathSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { join, resolve } from "node:path";
import { createRequire } from "node:module";
import { afterAll, expect, it } from "vitest";

const root = realpathSync(mkdtempSync(join(tmpdir(), "fabric-lanes-cli-")));
afterAll(() => rmSync(root, { recursive: true, force: true }));
const cli = resolve(import.meta.dirname, "../src/cli.ts");
const loader = createRequire(import.meta.url).resolve("tsx");

function lanes(cwd: string, ...args: string[]) {
  const result = spawnSync(process.execPath, ["--import", loader, cli, "lanes", ...args], {
    cwd, encoding: "utf8", timeout: 60_000,
    env: { ...process.env, AGENT_FABRIC_STATE_DIRECTORY: join(root, "state"), AGENT_FABRIC_LABEL: "lanes-cli" },
  });
  return { code: result.status, out: result.stdout, err: result.stderr };
}

it("says which project an empty lane listing resolved to, and accepts --project", () => {
  const repo = join(root, "repo");
  const nested = join(repo, "sub");
  mkdirSync(nested, { recursive: true });
  spawnSync("git", ["init", "-q", repo]);
  const bare = lanes(repo);
  expect(bare.code).toBe(0);
  expect(bare.out).toBe(`no lanes for project ${repo}\n`);
  const fromNested = lanes(nested);
  expect(fromNested.out).toContain(`no lanes for project ${repo}`);
  expect(fromNested.out).toContain(`cwd ${nested} resolves to it`);
  const elsewhere = lanes(root, "--project", repo);
  expect(elsewhere.code).toBe(0);
  expect(elsewhere.out).toBe(`no lanes for project ${repo}\n`);
});

it("rejects --all and --timeout without --wait, and a bad timeout", () => {
  expect(lanes(root, "--all").code).toBe(2);
  expect(lanes(root, "--timeout", "5").code).toBe(2);
  expect(lanes(root, "--wait", "--timeout", "abc").code).toBe(2);
  expect(lanes(root, "--wait", "--timeout", "-1").code).toBe(2);
  expect(lanes(root, "--wait", "--timeout", "0").code).toBe(0);
});
