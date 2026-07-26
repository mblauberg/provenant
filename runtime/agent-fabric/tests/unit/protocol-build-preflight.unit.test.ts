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
  it("judges the tree it was loaded from, not an ambient AGENTS_HOME", async () => {
    // AGENTS_HOME selects a config/state home, and callers legitimately point it
    // at a synthetic one that holds no code at all — the MCP lifecycle tests do
    // exactly that. The dist this process will import is the one beside this
    // module, so that is the only tree whose freshness is evidence about it.
    // Inheriting AGENTS_HOME instead reports every such caller as stale.
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
