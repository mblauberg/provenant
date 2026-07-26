import { fileURLToPath } from "node:url";

import { describe, expect, it } from "vitest";
import { resolveConfig } from "vite";

const packageRoot = fileURLToPath(new URL("../../", import.meta.url));

describe("protocol package resolution", () => {
  it("matches packaged production while schema artifacts remain unconditional", async () => {
    const config = await resolveConfig({
      configFile: `${packageRoot}/vitest.config.ts`,
      configLoader: "runner",
    }, "serve");
    const resolve = config.createResolver();

    const protocol = await resolve("@local/agent-fabric-protocol", packageRoot);
    const schema = await resolve(
      "@local/agent-fabric-protocol/schemas/protocol.schema.json",
      packageRoot,
    );

    expect(protocol).toMatch(/\/runtime\/agent-fabric-protocol\/dist\/index\.js$/u);
    expect(schema).toMatch(
      /\/runtime\/agent-fabric-protocol\/schemas\/protocol\.schema\.json$/u,
    );
  });
});
