import { describe, expect, it } from "vitest";

import {
  resolveAdapterExecutable,
  resolveExecutableOnPath,
} from "../../src/adapters/compatibility.ts";

describe("adapter executable resolution", () => {
  it("PATH resolution uses name when executable is not absolute", async () => {
    await expect(resolveExecutableOnPath("codex", {
      path: "/fake/bin",
      lookup: async (name) => name === "codex" ? "/fake/bin/codex" : undefined,
    })).resolves.toBe("/fake/bin/codex");

    await expect(resolveExecutableOnPath("/missing/codex", {
      path: "/fake/bin",
      lookup: async () => undefined,
    })).resolves.toBe("/missing/codex");
  });

  it("runtime override takes precedence over PATH resolution", async () => {
    await expect(resolveAdapterExecutable({
      executable: "codex",
      executableOverride: "/testing/codex",
      path: "/fake/bin",
      lookup: async () => "/fake/bin/codex",
    })).resolves.toBe("/testing/codex");
  });
});
