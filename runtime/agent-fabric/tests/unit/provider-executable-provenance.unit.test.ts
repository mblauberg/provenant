import type { ChildProcess, ExecFileException, ExecFileOptionsWithStringEncoding } from "node:child_process";

import { describe, expect, it, vi } from "vitest";

type ExecFileCallback = (error: ExecFileException | null, stdout: string, stderr: string) => void;

const codesignFixture = vi.hoisted(() => ({
  invocations: [] as Array<{
    child: ChildProcess;
    closed: Promise<{ code: number | null; signal: NodeJS.Signals | null }>;
    timeout: number | undefined;
    killSignal: NodeJS.Signals | number | undefined;
  }>,
}));

vi.mock("node:child_process", async (importOriginal) => {
  const actual = await importOriginal<typeof import("node:child_process")>();
  const fixtureExecFile = ((
    file: string,
    _arguments: readonly string[],
    optionsOrCallback: ExecFileOptionsWithStringEncoding | ExecFileCallback,
    callback?: ExecFileCallback,
  ): ChildProcess => {
    if (file !== "/usr/bin/codesign") throw new Error(`unexpected executable: ${file}`);
    const options = typeof optionsOrCallback === "function"
      ? { encoding: "utf8" as const }
      : optionsOrCallback;
    const completion = typeof optionsOrCallback === "function" ? optionsOrCallback : callback;
    if (completion === undefined) throw new Error("codesign fixture requires a completion callback");
    const fixtureOptions = {
      ...options,
      ...(options.timeout === undefined ? {} : { timeout: 50 }),
    };
    const child = actual.execFile(process.execPath, [
      "-e",
      "process.on('SIGTERM', () => {}); setInterval(() => {}, 1_000);",
    ], fixtureOptions, completion);
    const closed = new Promise<{ code: number | null; signal: NodeJS.Signals | null }>((resolve) => {
      child.once("close", (code, signal) => resolve({ code, signal }));
    });
    codesignFixture.invocations.push({
      child,
      closed,
      timeout: options.timeout,
      killSignal: options.killSignal,
    });
    return child;
  }) as typeof actual.execFile;

  return { ...actual, execFile: fixtureExecFile };
});

import {
  verifyProviderExecutableIdentity,
  type ProviderIdentityPort,
} from "../../src/adapters/provider-identity.ts";

function port(overrides: Partial<ProviderIdentityPort> = {}): ProviderIdentityPort {
  return {
    inspectPath: vi.fn(async (path: string) => ({
      canonicalPath: path,
      regularFile: true,
      ownerUid: 501,
      mode: 0o755,
      sha256: "a".repeat(64),
    })),
    inspectDirectory: vi.fn(async (path: string) => ({
      canonicalPath: path,
      directory: true,
      ownerUid: 501,
      mode: 0o755,
    })),
    verifySignature: vi.fn(async () => undefined),
    signingIdentity: vi.fn(async () => ({ teamId: "Q6L2SF6YDW", identifier: "com.anthropic.claude-code" })),
    currentUid: vi.fn(() => 501),
    ...overrides,
  };
}

describe("provider executable identity", () => {
  it("terminates a codesign child at its deadline and reports an incomplete probe", async () => {
    const result = await Promise.race([
      verifyProviderExecutableIdentity({
        adapterId: "claude-agent-sdk",
        executable: process.execPath,
      }).then(
        () => ({ outcome: "resolved" as const }),
        (error: unknown) => ({ outcome: "rejected" as const, error }),
      ),
      new Promise<{ outcome: "fixture-timeout" }>((resolve) => {
        setTimeout(() => resolve({ outcome: "fixture-timeout" }), 500);
      }),
    ]);
    const invocation = codesignFixture.invocations[0];

    try {
      expect(result).toMatchObject({
        outcome: "rejected",
        error: { code: "ADAPTER_INTERFACE_PROBE_INCOMPLETE" },
      });
      expect(invocation).toBeDefined();
      expect(invocation?.timeout).toBe(15_000);
      expect(invocation?.killSignal).toBe("SIGKILL");
      await expect(invocation?.closed).resolves.toEqual({ code: null, signal: "SIGKILL" });
      expect(invocation?.child.exitCode).toBeNull();
      expect(invocation?.child.signalCode).toBe("SIGKILL");
    } finally {
      if (invocation?.child.exitCode === null && invocation.child.signalCode === null) {
        invocation.child.kill("SIGKILL");
        await invocation.closed;
      }
    }
  });

  it("accepts changed bytes when vendor identity and safe path still conform", async () => {
    const first = await verifyProviderExecutableIdentity({
      adapterId: "claude-agent-sdk",
      executable: "/opt/homebrew/bin/claude",
    }, port());
    const second = await verifyProviderExecutableIdentity({
      adapterId: "claude-agent-sdk",
      executable: "/opt/homebrew/bin/claude",
    }, port({
      inspectPath: vi.fn(async (path: string) => ({
        canonicalPath: path,
        regularFile: true,
        ownerUid: 501,
        mode: 0o755,
        sha256: "b".repeat(64),
      })),
    }));

    expect(first.sha256).not.toBe(second.sha256);
    expect(second.assurance).toBe("full-vendor-identity");
  });

  it("fails closed on the wrong vendor identity", async () => {
    await expect(verifyProviderExecutableIdentity({
      adapterId: "claude-agent-sdk",
      executable: "/opt/homebrew/bin/claude",
    }, port({ signingIdentity: vi.fn(async () => ({ teamId: "ATTACKER", identifier: "claude" })) })))
      .rejects.toMatchObject({ code: "ADAPTER_IDENTITY_MISMATCH" });
  });

  it("fails closed before identity extraction when strict signature verification fails", async () => {
    const signingIdentity = vi.fn(async () => ({ teamId: "94KV3E626L", identifier: "kiro-cli" }));
    await expect(verifyProviderExecutableIdentity({
      adapterId: "kiro-acp",
      executable: "/fixture/example/.local/bin/kiro-cli",
    }, port({
      verifySignature: vi.fn(async () => { throw new Error("invalid signature"); }),
      signingIdentity,
    }))).rejects.toMatchObject({ code: "ADAPTER_IDENTITY_MISMATCH" });
    expect(signingIdentity).not.toHaveBeenCalled();
  });

  it("fails closed on an unsafe Agy executable", async () => {
    await expect(verifyProviderExecutableIdentity({
      adapterId: "agy",
      executable: "/fixture/example/.local/bin/agy",
    }, port({
      inspectPath: vi.fn(async (path: string) => ({
        canonicalPath: path,
        regularFile: true,
        ownerUid: 501,
        mode: 0o777,
        sha256: "c".repeat(64),
      })),
      signingIdentity: vi.fn(async () => ({ teamId: "EQHXZ8M8AV", identifier: "cli" })),
    }))).rejects.toMatchObject({ code: "ADAPTER_PATH_UNSAFE" });
  });

  it("admits the stable Kiro shim by Amazon signing identity", async () => {
    const verifySignature = vi.fn(async () => undefined);
    const signingIdentity = vi.fn(async () => ({ teamId: "94KV3E626L", identifier: "kiro-cli" }));
    await expect(verifyProviderExecutableIdentity({
      adapterId: "kiro-acp",
      executable: "/fixture/example/.local/bin/kiro-cli",
    }, port({ verifySignature, signingIdentity })))
      .resolves.toMatchObject({ assurance: "full-vendor-identity" });
    expect(verifySignature).toHaveBeenCalledWith("/fixture/example/.local/bin/kiro-cli");
    expect(verifySignature.mock.invocationCallOrder[0]).toBeLessThan(signingIdentity.mock.invocationCallOrder[0] ?? 0);
  });

  it("labels Cursor partial identity and checks its signed helper and Node", async () => {
    const signingIdentity = vi.fn(async (path: string) => path.endsWith("spawn-helper")
      ? { teamId: "DCNK4UB866", identifier: "com.todesktop.230313mzl4w4u92.spawn-helper" }
      : { teamId: "HX7739G8FX", identifier: "node" });
    const result = await verifyProviderExecutableIdentity({
      adapterId: "cursor-agent",
      executable: "/fixture/example/.local/share/cursor-agent/versions/current/cursor-agent",
      cursorInstallRoot: "/fixture/example/.local/share/cursor-agent",
    }, port({ signingIdentity }));

    expect(result.assurance).toBe("partial-signed-helpers");
    expect(signingIdentity).toHaveBeenCalledTimes(2);
  });

  it("admits an updated OpenCode executable only inside its safe owner-controlled install root", async () => {
    const result = await verifyProviderExecutableIdentity({
      adapterId: "opencode-acp",
      executable: "/opt/homebrew/Cellar/opencode/1.17.18/bin/opencode",
      providerInstallRoot: "/opt/homebrew/Cellar/opencode",
    }, port({
      inspectDirectory: vi.fn(async (path: string) => ({
        canonicalPath: path,
        directory: true,
        ownerUid: 501,
        mode: 0o755,
      })),
    }));

    expect(result).toMatchObject({
      assurance: "owner-controlled-install-root",
      canonicalPath: "/opt/homebrew/Cellar/opencode/1.17.18/bin/opencode",
      sha256: "a".repeat(64),
      signing: [],
    });
  });

  it("rejects OpenCode outside its canonical install root", async () => {
    await expect(verifyProviderExecutableIdentity({
      adapterId: "opencode-acp",
      executable: "/tmp/opencode",
      providerInstallRoot: "/opt/homebrew/Cellar/opencode",
    }, port())).rejects.toMatchObject({ code: "ADAPTER_PATH_UNSAFE" });
  });
});
