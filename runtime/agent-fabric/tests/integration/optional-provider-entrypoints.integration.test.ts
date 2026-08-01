import { access, chmod, mkdtemp, readFile, rm, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { fileURLToPath } from "node:url";

import { afterEach, describe, expect, it } from "vitest";

import { AdapterProcessTransport } from "../../src/adapters/process.ts";
import { AdapterSupervisor } from "../../src/adapters/supervisor.ts";

const temporaryDirectories: string[] = [];

afterEach(async () => {
  await Promise.all(temporaryDirectories.splice(0).map((path) => rm(path, { recursive: true, force: true })));
});

async function providerFixture(
  directory: string,
  cursorResult: Record<string, unknown> = {
    type: "result",
    subtype: "success",
    is_error: false,
    session_id: "provider-session-1",
    result: "done",
  },
): Promise<string> {
  const path = join(directory, "provider-fixture.mjs");
  await writeFile(
    path,
    `#!/usr/bin/env node
import { writeFileSync } from "node:fs";
const cursor = process.argv.includes("--output-format");
const logIndex = process.argv.indexOf("--log-file");
if (logIndex !== -1) writeFileSync(process.argv[logIndex + 1], "Created conversation 3cbfa155-fc5f-4c6e-aa99-3a44d48262b4\\n");
const result = cursor
  ? ${JSON.stringify(cursorResult)}
  : "done";
process.stdout.write((typeof result === "string" ? result : JSON.stringify(result)) + "\\n");
`,
  );
  await chmod(path, 0o700);
  return path;
}

describe("optional provider executable wrappers", () => {
  it("lets an inner optional-provider timeout persist ambiguity before the outer response envelope expires", async () => {
    const directory = await mkdtemp(join(tmpdir(), "fabric-agy-timeout-envelope-"));
    temporaryDirectories.push(directory);
    const invocationMarker = join(directory, "provider-invocations");
    const providerExecutable = join(directory, "hanging-provider.mjs");
    await writeFile(
      providerExecutable,
      `#!/usr/bin/env node\nimport { appendFileSync } from "node:fs";\nappendFileSync(${JSON.stringify(invocationMarker)}, "invoked\\n");\nsetInterval(() => undefined, 1000);\n`,
    );
    await chmod(providerExecutable, 0o700);
    const wrapperPath = fileURLToPath(new URL("../support/provider-wrapper-entrypoint.ts", import.meta.url));
    const supervisor = new AdapterSupervisor({
      agy: {
        command: [
          process.execPath,
          "--import",
          "tsx",
          wrapperPath,
          "--journal",
          join(directory, "adapter.sqlite3"),
          "--provider-executable",
          providerExecutable,
          "--cwd",
          directory,
        ],
        environment: {
          AGENT_FABRIC_TEST_ADAPTER: "agy",
          AGENT_FABRIC_TEST_PROVIDER_TURN_TIMEOUT_MS: "500",
        },
      },
    }, { controlTimeoutMs: 2_000, providerTurnTimeoutMs: 500 });
    const request = {
      actionId: "agy:timeout:1",
      payload: {
        model: "gemini-fixture",
        modelFamily: "google",
        prompt: "bounded fixture task",
      },
    };

    try {
      await expect(supervisor.request("agy", "spawn", request)).rejects.toMatchObject({
        name: "PROVIDER_TIMEOUT",
      });
      await expect(supervisor.request("agy", "lookup_action", { actionId: request.actionId }))
        .resolves.toMatchObject({ status: "ambiguous", executionCount: 1, effectCount: 0 });
      await expect(supervisor.request("agy", "spawn", request)).rejects.toMatchObject({
        name: "ACTION_RECONCILIATION_REQUIRED",
      });
      expect(await readFile(invocationMarker, "utf8")).toBe("invoked\n");
    } finally {
      await supervisor.close();
    }
  });

  it("reports Kiro ACP capabilities without eagerly spawning the provider", async () => {
    const directory = await mkdtemp(join(tmpdir(), "fabric-kiro-entrypoint-"));
    temporaryDirectories.push(directory);
    const markerPath = join(directory, "provider-spawned");
    const providerExecutable = join(directory, "kiro-provider-fixture.mjs");
    await writeFile(
      providerExecutable,
      `#!/usr/bin/env node\nrequire("node:fs").writeFileSync(${JSON.stringify(markerPath)}, "spawned");\n`,
    );
    await chmod(providerExecutable, 0o700);
    const wrapperPath = fileURLToPath(new URL("../../src/adapters/providers/optional/kiro-acp.ts", import.meta.url));
    const transport = new AdapterProcessTransport({
      command: [
        process.execPath,
        "--import",
        "tsx",
        wrapperPath,
        "--journal",
        join(directory, "adapter.sqlite3"),
        "--provider-executable",
        providerExecutable,
      ],
      environment: {},
      responseTimeoutMs: 2_000,
    });
    try {
      await expect(transport.request("capabilities", {})).resolves.toMatchObject({
        adapterId: "kiro-acp",
        operations: ["spawn", "send_turn", "release"],
        persistentSession: false,
        recoveryOperations: ["lookup_action"],
      });
      await expect(access(markerPath)).rejects.toMatchObject({ code: "ENOENT" });
    } finally {
      await transport.close();
    }
  });

  it("reports OpenCode ACP capabilities without eagerly spawning the provider", async () => {
    const directory = await mkdtemp(join(tmpdir(), "fabric-opencode-entrypoint-"));
    temporaryDirectories.push(directory);
    const markerPath = join(directory, "provider-spawned");
    const providerExecutable = join(directory, "opencode-provider-fixture.mjs");
    await writeFile(providerExecutable, `#!/usr/bin/env node\nrequire("node:fs").writeFileSync(${JSON.stringify(markerPath)}, "spawned");\n`);
    await chmod(providerExecutable, 0o700);
    const wrapperPath = fileURLToPath(new URL("../../src/adapters/providers/optional/opencode-acp.ts", import.meta.url));
    const transport = new AdapterProcessTransport({
      command: [
        process.execPath,
        "--import",
        "tsx",
        wrapperPath,
        "--journal",
        join(directory, "adapter.sqlite3"),
        "--provider-executable",
        providerExecutable,
      ],
      environment: {},
      responseTimeoutMs: 2_000,
    });
    try {
      await expect(transport.request("capabilities", {})).resolves.toMatchObject({
        adapterId: "opencode-acp",
        operations: ["spawn", "send_turn", "release"],
        allowedModelFamilies: ["generic-open"],
        persistentSession: false,
        recoveryOperations: ["lookup_action"],
      });
      await expect(access(markerPath)).rejects.toMatchObject({ code: "ENOENT" });
    } finally {
      await transport.close();
    }
  });

  it("serves Pi through one persistent bounded JSONL RPC process without inheriting the full environment", async () => {
    const directory = await mkdtemp(join(tmpdir(), "fabric-pi-entrypoint-"));
    temporaryDirectories.push(directory);
    const wrapperPath = fileURLToPath(new URL("../../src/adapters/providers/optional/pi-rpc.ts", import.meta.url));
    const providerPath = fileURLToPath(new URL("../support/pi-jsonl-rpc-fake.ts", import.meta.url));
    const tsxLoaderPath = fileURLToPath(import.meta.resolve("tsx"));
    process.env.AGENT_FABRIC_TEST_SECRET = "must-not-cross-provider-boundary";
    const transport = new AdapterProcessTransport({
      command: [
        process.execPath,
        "--import",
        "tsx",
        wrapperPath,
        "--journal",
        join(directory, "adapter.sqlite3"),
        "--provider-executable",
        process.execPath,
        "--provider-argument",
        "--import",
        "--provider-argument",
        tsxLoaderPath,
        "--provider-argument",
        providerPath,
        "--cwd",
        directory,
        "--turn-timeout-ms",
        "30",
        "--allowed-provider",
        "fixture-provider",
      ],
      environment: {},
      responseTimeoutMs: 2_000,
    });
    try {
      await expect(transport.request("capabilities", {})).resolves.toMatchObject({
        protocolVersion: 1,
        adapterId: "pi-rpc",
        actionJournal: true,
      });
      await expect(
        transport.request("spawn", {
          actionId: "pi-rpc:spawn:1",
          payload: {
            provider: "fixture-provider",
            model: "fixture-open-model",
            modelFamily: "open-weight",
            cwd: directory,
          },
        }),
      ).resolves.toMatchObject({
        resumeReference: "/sessions/pi-session-1.jsonl",
        sessionId: "pi-session-1",
      });
      await expect(
        transport.request("dispatch", {
          actionId: "pi-rpc:turn:1",
          operation: "send_turn",
          payload: {
            prompt: "bounded task",
            model: "fixture-open-model",
            modelFamily: "open-weight",
            cwd: directory,
            resumeReference: "/sessions/pi-session-1.jsonl",
          },
        }),
      ).resolves.toMatchObject({ status: "terminal", executionCount: 1, effectCount: 1 });
      await expect(
        transport.request("dispatch", {
          actionId: "pi-rpc:abort:1",
          operation: "interrupt",
          payload: { resumeReference: "/sessions/pi-session-1.jsonl" },
        }),
      ).resolves.toMatchObject({ status: "terminal" });
      await expect(
        transport.request("dispatch", {
          actionId: "pi-rpc:turn-timeout:1",
          operation: "send_turn",
          payload: {
            prompt: "hang",
            model: "fixture-open-model",
            modelFamily: "open-weight",
            cwd: directory,
            resumeReference: "/sessions/pi-session-1.jsonl",
          },
        }),
      ).rejects.toMatchObject({ name: "PROVIDER_RESPONSE_TIMEOUT" });
    } finally {
      delete process.env.AGENT_FABRIC_TEST_SECRET;
      await transport.close();
    }
  });

  it.each([
    {
      adapterId: "agy",
      source: "../support/provider-wrapper-entrypoint.ts",
      model: "gemini-fixture",
      modelFamily: "google",
      expectedReference: "3cbfa155-fc5f-4c6e-aa99-3a44d48262b4",
    },
    {
      adapterId: "cursor-agent",
      source: "../support/provider-wrapper-entrypoint.ts",
      model: "composer-fixture",
      modelFamily: "cursor-composer",
      expectedReference: "provider-session-1",
    },
  ])("serves the fabric NDJSON protocol for $adapterId around its bounded CLI boundary", async (fixture) => {
    const directory = await mkdtemp(join(tmpdir(), `fabric-${fixture.adapterId}-entrypoint-`));
    temporaryDirectories.push(directory);
    const providerExecutable = await providerFixture(directory);
    const wrapperPath = fileURLToPath(new URL(fixture.source, import.meta.url));
    const transport = new AdapterProcessTransport({
      command: [
        process.execPath,
        "--import",
        "tsx",
        wrapperPath,
        "--journal",
        join(directory, "adapter.sqlite3"),
        "--provider-executable",
        providerExecutable,
        ...(fixture.adapterId === "cursor-agent" ? ["--provider-install-root", directory] : []),
        "--cwd",
        directory,
      ],
      environment: { AGENT_FABRIC_TEST_ADAPTER: fixture.adapterId },
      responseTimeoutMs: 2_000,
    });
    try {
      await expect(transport.request("capabilities", {})).resolves.toMatchObject({
        protocolVersion: 1,
        adapterId: fixture.adapterId,
        actionJournal: true,
      });
      await expect(
        transport.request("spawn", {
          actionId: `${fixture.adapterId}:wrong-family`,
          payload: {
            model: fixture.model,
            modelFamily: "open-weight",
            prompt: "must not reach provider",
          },
        }),
      ).rejects.toMatchObject({ name: "ADAPTER_FAMILY_FORBIDDEN" });
      await expect(
        transport.request("spawn", {
          actionId: `${fixture.adapterId}:spawn:1`,
          payload: {
            model: fixture.model,
            modelFamily: fixture.modelFamily,
            prompt: "bounded fixture task",
          },
        }),
      ).resolves.toMatchObject({ resumeReference: fixture.expectedReference, result: "done" });
      await expect(
        transport.request("lookup_action", { actionId: `${fixture.adapterId}:spawn:1` }),
      ).resolves.toMatchObject({ status: "terminal", executionCount: 1, effectCount: 1 });
    } finally {
      await transport.close();
    }
  });

  it("preserves bounded Cursor record diagnostics and ambiguous effect accounting across the adapter process", async () => {
    const directory = await mkdtemp(join(tmpdir(), "fabric-cursor-unsupported-record-"));
    temporaryDirectories.push(directory);
    const providerExecutable = await providerFixture(directory, {
      type: "private_provider_content",
      private_provider_content: "must not cross the adapter boundary",
    });
    const wrapperPath = fileURLToPath(new URL("../support/provider-wrapper-entrypoint.ts", import.meta.url));
    const transport = new AdapterProcessTransport({
      command: [
        process.execPath,
        "--import",
        "tsx",
        wrapperPath,
        "--journal",
        join(directory, "adapter.sqlite3"),
        "--provider-executable",
        providerExecutable,
        "--provider-install-root",
        directory,
        "--cwd",
        directory,
      ],
      environment: { AGENT_FABRIC_TEST_ADAPTER: "cursor-agent" },
      responseTimeoutMs: 2_000,
    });
    const actionId = "cursor-agent:unsupported-record:1";
    try {
      await expect(transport.request("spawn", {
        actionId,
        payload: {
          model: "composer-fixture",
          modelFamily: "cursor-composer",
          prompt: "bounded fixture task",
        },
      })).rejects.toMatchObject({
        name: "PROVIDER_RESPONSE_INVALID",
        message: "Cursor stream contained an unsupported record type",
        details: {
          recordIndex: 0,
          recordTypeSha256: "03ac7d3ac98ea049784810426c00f5a0221fe198644336030896a0310099ee47",
          recordTypeLength: 24,
          recordFields: ["type"],
        },
      });
      await expect(transport.request("lookup_action", { actionId })).resolves.toMatchObject({
        status: "ambiguous",
        executionCount: 1,
        effectCount: 0,
      });
    } finally {
      await transport.close();
    }
  });
});
