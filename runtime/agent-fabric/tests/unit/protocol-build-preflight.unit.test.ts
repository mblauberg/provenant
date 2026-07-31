import { mkdtemp, rm } from "node:fs/promises";

import { afterEach, describe, expect, it } from "vitest";

import { preflightProtocolBuild } from "../../src/daemon/protocol-build-preflight.js";

const roots: string[] = [];

afterEach(async () => {
  await Promise.allSettled(
    roots.splice(0).map(async (root) => rm(root, { force: true, recursive: true })),
  );
});

describe("protocol build preflight", () => {
  it("judges the code-adjacent product tree, not a different product root", async () => {
    // AGENTS_HOME names product code, but this process imports the dist beside
    // this module. That code-adjacent product root is the only tree whose
    // freshness is evidence about the loaded protocol package.
    const home = await mkdtemp("/tmp/afb-preflight-");
    roots.push(home);
    const previous = process.env.AGENTS_HOME;
    process.env.AGENTS_HOME = home;
    try {
      await expect(preflightProtocolBuild()).resolves.toBeUndefined();
    } finally {
      if (previous === undefined) delete process.env.AGENTS_HOME;
      else process.env.AGENTS_HOME = previous;
    }
  });
});
