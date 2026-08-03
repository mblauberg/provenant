import { mkdtemp, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";

import { describe, expect, it } from "vitest";

import { loadFabricConfig } from "../../../src/config/index.ts";

describe("NFR-009 configured execution-profile selection", () => {
  it("refuses a project layer that selects an execution profile", async () => {
    const directory = await mkdtemp(join(tmpdir(), "fabric-profile-config-"));
    const globalPath = join(directory, "global.yaml");
    const projectPath = join(directory, "project.yaml");
    await writeFile(globalPath, [
      "schemaVersion: 1",
      "allowedProfiles: [paired-observed]",
      `workspaceRoots: [${JSON.stringify(directory)}]`,
      "",
    ].join("\n"));
    await writeFile(projectPath, "schemaVersion: 1\nnamedExecutionProfile: paired-observed\n", { mode: 0o600 });
    await expect(loadFabricConfig({ globalPath, projectPath })).rejects.toMatchObject({
      code: "CONFIG_UNTRUSTED_FIELD",
      field: "namedExecutionProfile",
    });
  });
});
