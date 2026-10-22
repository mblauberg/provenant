import { resolve } from "node:path";
import { describe, expect, it } from "vitest";

import { dispatchConfiguredBatch, dispatchConfiguredProvider } from "../src/execution.js";

const root = resolve(import.meta.dirname, "../../..");
const identity = { project: root, cwd: root, agentId: "nested-probe", provider: "codex" };
const environment = {
  AGENT_FABRIC_PRODUCT_ROOT: root,
  PROVENANT_HOST_LOCAL_ONLY: "1",
  CODEX_SANDBOX: "seatbelt",
  PROVENANT_PARENT_ATTEMPT: JSON.stringify({ run_id: "parent", task_id: "writer", attempt: 1, run_dir: root }),
};

describe("nested dispatch from a sandboxed Codex lane", () => {
  it("rejects a single dispatch before placement or route validation", async () => {
    const result = await dispatchConfiguredProvider(
      { model: "claude/sonnet", prompt: "reply OK", prompt_file: "unused", host: "unconfigured" },
      identity, new AbortController().signal, environment,
    );
    expect(result).toMatchObject({ status: "rejected", error: "nested_dispatch_unavailable" });
    expect(result.fix).toContain("dispatch reviewers from the chair");
  });

  it("rejects every batch task before placement", async () => {
    const result = await dispatchConfiguredBatch(
      { tasks: [
        { id: "review", model: "claude/sonnet", prompt: "reply OK", prompt_file: "unused" },
        { id: "same-family", model: "codex/fixture", prompt: "reply OK", prompt_file: "unused" },
      ], host: "unconfigured" },
      identity, new AbortController().signal, environment,
    );
    expect(result).toMatchObject({ status: "rejected", error: "nested_dispatch_unavailable",
      tasks: [
        { task_id: "review", status: "rejected", state: "terminal", error: "nested_dispatch_unavailable" },
        { task_id: "same-family", status: "rejected", state: "terminal", error: "nested_dispatch_unavailable" },
      ] });
  });

  it.each(["CODEX_SANDBOX", "PROVENANT_PARENT_ATTEMPT"])("keeps ordinary validation without %s", async (key) => {
    const env: NodeJS.ProcessEnv = { ...environment };
    delete env[key];
    const result = await dispatchConfiguredBatch({ tasks: [] }, identity, new AbortController().signal, env);
    expect(result).toMatchObject({ status: "rejected", error: "invalid_input" });
  });
});
