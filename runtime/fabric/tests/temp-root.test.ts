import { chmodSync, mkdirSync, mkdtempSync, realpathSync, rmSync, symlinkSync } from "node:fs";
import { tmpdir } from "node:os";
import { join, sep } from "node:path";
import { afterEach, describe, expect, it } from "vitest";

import { assertFixtureRunRoot } from "../src/execution.js";
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
    const lane = join(runLocal(), ".agent-run/attempt/tmp");
    mkdirSync(lane, { recursive: true });
    const good = runLocal();

    expect(fixtureBase(lane, [join(denied, "tmp"), join(runLocal(), "missing"), good])).toBe(good);
    process.env.TMPDIR = lane;
    process.env.FABRIC_TEST_TMPDIR = good;
    expect(scratchRoot("fabric-denied-").startsWith(good + sep)).toBe(true);
  });

  it("fails with a fix when no temp root is writable", () => {
    const lane = join(runLocal(), ".agent-run/attempt/tmp");
    mkdirSync(lane, { recursive: true });
    expect(() => fixtureBase(lane, [join(runLocal(), "missing")])).toThrow(/FABRIC_TEST_TMPDIR/u);
  });
});

it("never picks a FABRIC_TEST_TMPDIR beneath .agent-run, which assertFixtureRunRoot would refuse", () => {
  const lane = join(runLocal(), ".agent-run/attempt/tmp");
  mkdirSync(lane, { recursive: true });
  process.env.TMPDIR = lane;
  process.env.FABRIC_TEST_TMPDIR = lane;
  expect(() => fixtureBase(lane, [lane])).toThrow(/FABRIC_TEST_TMPDIR/u);
  expect(() => assertFixtureRunRoot(join(lane, "ws/.agent-run"), [{ model: "fixture" }], lane)).toThrow(/fixture run refused/u);
});
