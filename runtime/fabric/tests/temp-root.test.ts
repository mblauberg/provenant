import { chmodSync, mkdirSync, mkdtempSync, realpathSync, rmSync, symlinkSync } from "node:fs";
import { tmpdir } from "node:os";
import { fileURLToPath } from "node:url";
import { runJsonSubprocess } from "../src/json-subprocess.js";
import { dirname, join, sep } from "node:path";
import { afterEach, describe, expect, it } from "vitest";

import { acceptableFixtureTempRoot, assertFixtureRunRoot } from "../src/execution.js";
import { fixtureBase, scratchRoot } from "./helpers/temp-root.js";

const cleanup: string[] = [];
const savedTmpdir = process.env.TMPDIR;
const savedFallback = process.env.FABRIC_TEST_TMPDIR;
// A run-local root stands in for /tmp, which a sandbox may deny.
const runLocal = () => {
  const dir = realpathSync(mkdtempSync(join(tmpdir(), "fake-lane-")));
  cleanup.push(dir);
  return dir;
};
afterEach(() => {
  if (savedFallback === undefined) delete process.env.FABRIC_TEST_TMPDIR;
  else process.env.FABRIC_TEST_TMPDIR = savedFallback;
  if (savedTmpdir === undefined) delete process.env.TMPDIR;
  else process.env.TMPDIR = savedTmpdir;
  for (const path of cleanup.splice(0)) rmSync(path, { recursive: true, force: true });
});

describe("scratchRoot", () => {
  it("ignores a TMPDIR inside a .agent-run tree", () => {
    const base = realpathSync(mkdtempSync(join(tmpdir(), "fake-lane-")));
    cleanup.push(base);
    mkdirSync(join(base, ".git"));
    const agentRun = join(base, ".agent-run");
    const attemptTmp = join(agentRun, "runs/20261012-0126-batch-provenant/attempt-001/tmp");
    mkdirSync(attemptTmp, { recursive: true });
    const fallback = runLocal();
    process.env.TMPDIR = attemptTmp;
    process.env.FABRIC_TEST_TMPDIR = fallback;

    const root = scratchRoot("fabric-lean-");
    cleanup.push(root);

    expect(root.startsWith(agentRun + sep)).toBe(false);
    expect(root.split(sep)).not.toContain(".agent-run");
    expect(root.startsWith(fallback + sep)).toBe(true);
  });

  it("ignores a TMPDIR symlink that resolves inside a .agent-run tree", () => {
    const base = realpathSync(mkdtempSync(join(tmpdir(), "fake-lane-link-")));
    cleanup.push(base);
    mkdirSync(join(base, ".git"));
    const attemptTmp = join(base, ".agent-run/runs/attempt/tmp");
    mkdirSync(attemptTmp, { recursive: true });
    const link = join(base, "tmp-link");
    symlinkSync(attemptTmp, link);
    const fallback = runLocal();
    process.env.TMPDIR = link;
    process.env.FABRIC_TEST_TMPDIR = fallback;

    const root = scratchRoot("fabric-lean-");
    cleanup.push(root);

    expect(root.startsWith(realpathSync(attemptTmp) + sep)).toBe(false);
    expect(root.startsWith(fallback + sep)).toBe(true);
  });

  it("uses the OS temp dir when TMPDIR is an ordinary path", () => {
    const plain = realpathSync(mkdtempSync(join(tmpdir(), "plain-tmp-")));
    cleanup.push(plain);
    process.env.TMPDIR = plain;

    const root = scratchRoot("fabric-lean-");
    cleanup.push(root);

    expect(root.startsWith(plain + sep)).toBe(true);
  });

  it("takes the explicit run-local root when /tmp is denied", () => {
    const denied = runLocal();
    chmodSync(denied, 0o500);
    cleanup.push(denied);
    const checkout = runLocal();
    mkdirSync(join(checkout, ".git"));
    const lane = join(checkout, ".agent-run/attempt/tmp");
    mkdirSync(lane, { recursive: true });
    const good = runLocal();

    expect(fixtureBase(lane, [join(denied, "tmp"), join(runLocal(), "missing"), good])).toBe(good);
    process.env.TMPDIR = lane;
    process.env.FABRIC_TEST_TMPDIR = good;
    expect(scratchRoot("fabric-denied-").startsWith(good + sep)).toBe(true);
  });

  it("fails with a fix when no temp root is writable", () => {
    const checkout = runLocal();
    mkdirSync(join(checkout, ".git"));
    const lane = join(checkout, ".agent-run/attempt/tmp");
    mkdirSync(lane, { recursive: true });
    expect(() => fixtureBase(lane, [join(runLocal(), "missing")])).toThrow(/FABRIC_TEST_TMPDIR/u);
  });
});

it("accepts an isolated lane directory under .agent-run outside git, and runs a fixture there", async () => {
  const lane = join(runLocal(), ".agent-run/runs/20261022-run/attempt-001/tmp");
  mkdirSync(lane, { recursive: true });
  process.env.TMPDIR = lane;
  process.env.FABRIC_TEST_TMPDIR = lane;
  expect(fixtureBase(lane, [lane])).toBe(realpathSync(lane));

  const root = scratchRoot("fabric-sandbox-");
  cleanup.push(root);
  expect(root.startsWith(realpathSync(lane) + sep)).toBe(true);
  expect(() => assertFixtureRunRoot(join(root, "ws/.agent-run"), [{ model: "fixture" }], lane)).not.toThrow();
  // A run tree itself is still refused.
  expect(acceptableFixtureTempRoot(join(lane, "../../.."))).toBe(false);

  const fixture = join(dirname(fileURLToPath(import.meta.url)), "lifecycle-owner-fixture.mjs");
  const answer = await runJsonSubprocess<{ status: string }>(process.execPath, [fixture, "--preflight-json"], {
    cwd: root, input: { tasks: [] }, timeoutMs: 10_000, maxBytes: 65_536, unavailable: "fixture unavailable",
  });
  expect(answer.status).toBe("validated");
});

it("refuses a temp root inside a git work tree", () => {
  const repo = runLocal();
  mkdirSync(join(repo, ".git"));
  const inner = join(repo, "tmp");
  mkdirSync(inner);
  expect(() => fixtureBase(inner, [inner])).toThrow(/FABRIC_TEST_TMPDIR/u);
});
