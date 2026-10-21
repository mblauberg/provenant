import { mkdirSync, mkdtempSync, realpathSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { join, sep } from "node:path";
import { afterEach, describe, expect, it } from "vitest";

import { scratchRoot } from "./helpers/temp-root.js";

const cleanup: string[] = [];
const savedTmpdir = process.env.TMPDIR;
afterEach(() => {
  if (savedTmpdir === undefined) delete process.env.TMPDIR;
  else process.env.TMPDIR = savedTmpdir;
  for (const path of cleanup.splice(0)) rmSync(path, { recursive: true, force: true });
});

describe("scratchRoot", () => {
  it("ignores a TMPDIR inside a .agent-run tree", () => {
    const base = realpathSync(mkdtempSync(join("/tmp", "fake-lane-")));
    cleanup.push(base);
    const agentRun = join(base, ".agent-run");
    const attemptTmp = join(agentRun, "runs/20261012-0126-batch-provenant/attempt-001/tmp");
    mkdirSync(attemptTmp, { recursive: true });
    process.env.TMPDIR = attemptTmp;

    const root = scratchRoot("fabric-lean-");
    cleanup.push(root);

    expect(root.startsWith(agentRun + sep)).toBe(false);
    expect(root.split(sep)).not.toContain(".agent-run");
    expect(root.startsWith(realpathSync("/tmp") + sep)).toBe(true);
  });

  it("uses the OS temp dir when TMPDIR is an ordinary path", () => {
    const plain = realpathSync(mkdtempSync(join("/tmp", "plain-tmp-")));
    cleanup.push(plain);
    process.env.TMPDIR = plain;

    const root = scratchRoot("fabric-lean-");
    cleanup.push(root);

    expect(root.startsWith(plain + sep)).toBe(true);
  });
});
