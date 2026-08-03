import { describe, expect, it } from "vitest";

import { daemonConfiguration } from "../../src/cli/daemon-run.ts";

describe("foreground daemon configuration", () => {
  it("passes an explicit project root instead of accepting a project config path", () => {
    const configuration = daemonConfiguration([
      "--trusted-config", "/product/config/agent-fabric.yaml",
      "--compatibility", "/product/config/adapter-compatibility.yaml",
      "--compatibility-schema", "/product/runtime/agent-fabric/schemas/adapter-compatibility.schema.json",
      "--agents-home", "/product",
      "--project", "/repo",
    ]);

    expect(configuration).toMatchObject({
      globalConfigPath: "/product/config/agent-fabric.yaml",
      projectRoot: "/repo",
      compatibilityPath: "/product/config/adapter-compatibility.yaml",
    });
    expect(configuration).not.toHaveProperty("projectConfigPath");
  });
});
