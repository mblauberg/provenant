import { access } from "node:fs/promises";

import { describe, expect, it, vi } from "vitest";

import { probeProviderInterface } from "../../src/adapters/provider-interface.ts";

describe("provider non-answer interface conformance", () => {
  it.each([
    ["claude-agent-sdk", "--print --output-format stream-json"],
    ["agy", "--print --model --mode --log-file"],
    ["cursor-agent", "--print --output-format --model"],
  ] as const)("accepts the required %s headless flags", async (adapterId, stdout) => {
    const run = vi.fn(async () => ({ stdout, stderr: "", exitCode: 0 }));
    await expect(probeProviderInterface({ adapterId, executable: "/provider" }, run))
      .resolves.toMatchObject({ adapterId, conformant: true });
  });

  it("accepts complete option tokens followed by separate or equals values", async () => {
    const run = vi.fn(async () => ({
      stdout: "--print=true\n--model=<MODEL>\n--mode MODE\n--log-file=PATH",
      stderr: "",
      exitCode: 0,
    }));
    await expect(probeProviderInterface({ adapterId: "agy", executable: "/agy" }, run))
      .resolves.toMatchObject({ adapterId: "agy", conformant: true });
  });

  it("rejects prefixed and suffixed lookalike option names", async () => {
    const run = vi.fn(async () => ({
      stdout: "--sprint prefix--model --mode-extra --log-file-suffix",
      stderr: "",
      exitCode: 0,
    }));
    await expect(probeProviderInterface({ adapterId: "agy", executable: "/agy" }, run))
      .rejects.toMatchObject({ code: "ADAPTER_INTERFACE_PROBE_INCOMPLETE" });
  });

  it("proves the Codex app-server initialize handshake", async () => {
    const run = vi.fn(async () => ({ stdout: '{"id":1,"result":{"userAgent":"probe"}}\n', stderr: "", exitCode: 0 }));
    await expect(probeProviderInterface({ adapterId: "codex-app-server", executable: "/codex" }, run))
      .resolves.toMatchObject({ adapterId: "codex-app-server", conformant: true, probe: "app-server-initialize" });
  });

  it("classifies a Codex authentication response as an incomplete probe", async () => {
    const run = vi.fn(async () => ({
      stdout: '{"id":1,"error":{"message":"authentication required"}}\n',
      stderr: "",
      exitCode: 0,
    }));
    await expect(probeProviderInterface({ adapterId: "codex-app-server", executable: "/codex" }, run))
      .rejects.toMatchObject({
        code: "ADAPTER_INTERFACE_PROBE_INCOMPLETE",
        cause: { message: "authentication required" },
      });
  });

  it("classifies a Codex protocol error response as an interface mismatch", async () => {
    const run = vi.fn(async () => ({
      stdout: '{"id":1,"error":{"code":-32601,"message":"method not found"}}\n',
      stderr: "",
      exitCode: 0,
    }));
    await expect(probeProviderInterface({ adapterId: "codex-app-server", executable: "/codex" }, run))
      .rejects.toMatchObject({
        code: "ADAPTER_INTERFACE_MISMATCH",
        cause: { code: -32601, message: "method not found" },
      });
  });

  it("classifies an auth-shaped response with the wrong request id as an interface mismatch", async () => {
    const run = vi.fn(async () => ({
      stdout: '{"id":2,"error":{"message":"authentication required"}}\n',
      stderr: "",
      exitCode: 0,
    }));
    await expect(probeProviderInterface({ adapterId: "codex-app-server", executable: "/codex" }, run))
      .rejects.toMatchObject({
        code: "ADAPTER_INTERFACE_MISMATCH",
        cause: { message: "authentication required" },
      });
  });

  it("classifies a malformed Codex initialize result as an interface mismatch", async () => {
    const run = vi.fn(async () => ({ stdout: '{"id":1,"result":null}\n', stderr: "", exitCode: 0 }));
    await expect(probeProviderInterface({ adapterId: "codex-app-server", executable: "/codex" }, run))
      .rejects.toMatchObject({
        code: "ADAPTER_INTERFACE_MISMATCH",
        cause: expect.objectContaining({ message: "Codex initialize response is invalid" }),
      });
  });

  it("classifies a runner failure as an incomplete probe and preserves its cause", async () => {
    const cause = Object.assign(new Error("spawn failed"), { code: "ENOENT" });
    const run = vi.fn(async () => await Promise.reject(cause));
    await expect(probeProviderInterface({ adapterId: "codex-app-server", executable: "/missing" }, run))
      .rejects.toMatchObject({ code: "ADAPTER_INTERFACE_PROBE_INCOMPLETE", cause });
  });

  it("proves the Kiro ACP v1 initialize handshake", async () => {
    const run = vi.fn(async (input: { args: string[] }) => input.args.includes("--help")
      ? { stdout: "--model <MODEL> --effort <EFFORT> --agent-engine <ENGINE>", stderr: "", exitCode: 0 }
      : { stdout: '{"jsonrpc":"2.0","id":1,"result":{"protocolVersion":1}}\n', stderr: "", exitCode: 0 });
    await expect(probeProviderInterface({ adapterId: "kiro-acp", executable: "/kiro" }, run))
      .resolves.toMatchObject({ adapterId: "kiro-acp", conformant: true, probe: "acp-v1-initialize" });
  });

  it("proves the OpenCode ACP v1 initialize handshake without a model turn", async () => {
    let disposableCwd: string | undefined;
    const run = vi.fn(async (input: { cwd?: string }) => {
      disposableCwd = input.cwd;
      return ({
      stdout: '{"jsonrpc":"2.0","id":1,"result":{"protocolVersion":1,"agentInfo":{"name":"OpenCode","version":"1.17.18"}}}\n',
      stderr: "",
      exitCode: 0,
      });
    });
    await expect(probeProviderInterface({ adapterId: "opencode-acp", executable: "/opencode" }, run))
      .resolves.toMatchObject({ adapterId: "opencode-acp", conformant: true, probe: "acp-v1-initialize", version: "1.17.18" });
    expect(disposableCwd).toEqual(expect.any(String));
    expect(run).toHaveBeenCalledWith(expect.objectContaining({ args: ["acp", "--pure", "--cwd", disposableCwd], cwd: disposableCwd }));
    await expect(access(disposableCwd!)).rejects.toMatchObject({ code: "ENOENT" });
  });

  it("rejects Kiro ACP when the effort interface disappears", async () => {
    const run = vi.fn(async (input: { args: string[] }) => input.args.includes("--help")
      ? { stdout: "--model <MODEL> --agent-engine <ENGINE>", stderr: "", exitCode: 0 }
      : { stdout: '{"jsonrpc":"2.0","id":1,"result":{"protocolVersion":1}}\n', stderr: "", exitCode: 0 });
    await expect(probeProviderInterface({ adapterId: "kiro-acp", executable: "/kiro" }, run))
      .rejects.toMatchObject({ code: "ADAPTER_INTERFACE_PROBE_INCOMPLETE" });
  });

  it("fails closed when a required interface disappears", async () => {
    const run = vi.fn(async () => ({ stdout: "--print --model", stderr: "", exitCode: 0 }));
    await expect(probeProviderInterface({ adapterId: "agy", executable: "/agy" }, run))
      .rejects.toMatchObject({ code: "ADAPTER_INTERFACE_PROBE_INCOMPLETE" });
  });
});
