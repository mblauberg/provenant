#!/usr/bin/env node
import { randomUUID } from "node:crypto";
import { createConnection } from "node:net";
import { dirname } from "node:path";
import type { HerdrSteerDispatchResult } from "@local/agent-fabric-protocol";

const DOCTOR_USAGE = `usage: agent-fabric doctor [--consume-provider-quota] [--project PATH] [--agents-home PATH] [--trusted-config PATH] [--compatibility PATH] [--compatibility-schema PATH] [--review-profile PATH] [--json]

Options:
  --consume-provider-quota  run live provider capability probes and refresh the private cache
  --help, -h                show this help`;

function option(arguments_: string[], name: string): string | undefined {
  const index = arguments_.indexOf(name);
  return index === -1 ? undefined : arguments_[index + 1];
}

async function servingSocketPath(runtimeDirectory: string, fallback: string): Promise<string> {
  try {
    const { privateDiscoveryPaths, readPrivateDiscovery, readPrivateDiscoveryOwner } =
      await import("../daemon/private-discovery.js");
    const discoveryPaths = privateDiscoveryPaths(runtimeDirectory);
    const owner = await readPrivateDiscoveryOwner(discoveryPaths);
    if (owner === undefined || owner.state !== "active") return fallback;
    const discovery = await readPrivateDiscovery(discoveryPaths, owner.socketPath);
    if (discovery.status !== "active") return fallback;
    process.kill(discovery.owner.pid, 0);
    return discovery.receipt.socketPath;
  } catch {
    return fallback;
  }
}

async function inspect(arguments_: string[]): Promise<void> {
  const [{ default: Database }, { resolveFabricPaths }] = await Promise.all([
    import("better-sqlite3"),
    import("./paths.js"),
  ]);
  const paths = resolveFabricPaths();
  const databasePath = option(arguments_, "--database") ?? paths.databasePath;
  const runtimeDirectory = option(arguments_, "--runtime-directory") ?? paths.runtimeDirectory;
  const socketPath = await servingSocketPath(runtimeDirectory, paths.socketPath);
  const database = new Database(databasePath, { readonly: true, fileMustExist: true });
  try {
    const rows = database.prepare("SELECT run_id, chair_agent_id FROM runs ORDER BY run_id").all();
    const runs: Array<{ runId: string; chairAgentId: string }> = [];
    for (const value of rows) {
      if (
        typeof value !== "object" ||
        value === null ||
        !("run_id" in value) ||
        typeof value.run_id !== "string" ||
        !("chair_agent_id" in value) ||
        typeof value.chair_agent_id !== "string"
      ) {
        throw new Error("database returned an invalid run row");
      }
      runs.push({ runId: value.run_id, chairAgentId: value.chair_agent_id });
    }
    const output = {
      schemaVersion: 1,
      databasePath,
      stateDirectory: paths.stateDirectory,
      runtimeDirectory: dirname(socketPath),
      socketPath,
      runs,
    };
    process.stdout.write(`${JSON.stringify(output, null, arguments_.includes("--json") ? 2 : 0)}\n`);
  } finally {
    database.close();
  }
}

async function verifyReceipt(arguments_: string[]): Promise<void> {
  const runReceiptPath = option(arguments_, "--run-receipt");
  if (runReceiptPath === undefined) {
    throw new Error("receipt verify requires --run-receipt <path>");
  }
  const { verifyFabricReceiptLink } = await import("../exports/receipt.js");
  const result = await verifyFabricReceiptLink({ runReceiptPath });
  process.stdout.write(`verified ${result.relativePath} sha256 ${result.sha256}\n`);
}

async function herdrSteer(arguments_: string[]): Promise<void> {
  const [
    { FABRIC_OPERATIONS, NdjsonRpcTransport, ProtocolTransportError },
    { herdrUnavailableReason, parseHerdrSteerArguments },
    { resolveFabricPaths },
    { resolveMcpCapability },
  ] = await Promise.all([
    import("@local/agent-fabric-protocol"),
    import("./herdr-steer.js"),
    import("./paths.js"),
    import("../mcp/credentials.js"),
  ]);
  const request = await parseHerdrSteerArguments(arguments_);
  const paths = resolveFabricPaths();
  const socketPath = process.env.AGENT_FABRIC_SOCKET_PATH ??
    await servingSocketPath(paths.runtimeDirectory, paths.socketPath);
  const capability = await resolveMcpCapability(
    process.env,
    process.cwd(),
    (message) => process.stderr.write(`warning: ${message}\n`),
  );
  delete process.env.AGENT_FABRIC_CAPABILITY;
  let transport: Awaited<ReturnType<typeof NdjsonRpcTransport.connect>> | undefined;
  let result: HerdrSteerDispatchResult | {
    status: "unavailable";
    integration: "agent-fabric";
    reason: string;
  };
  try {
    transport = await NdjsonRpcTransport.connect(createConnection(socketPath), {
      protocolVersion: 1,
      client: { name: "agent-fabric-herdr-steer", version: "1.0.0" },
      authentication: {
        scheme: "capability",
        credential: capability,
        clientNonce: `herdr_steer_${randomUUID()}`,
      },
      expectedPrincipalKind: "agent",
      requiredFeatures: ["herdr-control.v1"],
      optionalFeatures: [],
    });
    result = await transport.call(FABRIC_OPERATIONS.herdrSteerDispatch, request);
  } catch (error: unknown) {
    if (!(error instanceof ProtocolTransportError) && !isConnectionFailure(error)) throw error;
    result = { status: "unavailable", integration: "agent-fabric", reason: herdrUnavailableReason(error) };
  } finally {
    await transport?.close();
  }
  process.stdout.write(`${JSON.stringify(result)}\n`);
  if (result.status !== "terminal") process.exitCode = 1;
}

async function checkHerdrSteer(): Promise<void> {
  const [
    { FABRIC_OPERATIONS, NdjsonRpcTransport },
    { checkHerdrSteer: runCheck },
    { resolveFabricPaths },
    { resolveMcpCapability },
  ] = await Promise.all([
    import("@local/agent-fabric-protocol"),
    import("./herdr-steer.js"),
    import("./paths.js"),
    import("../mcp/credentials.js"),
  ]);
  const result = await runCheck(process.env, {
    resolveCapability: async () => await resolveMcpCapability(process.env, process.cwd()),
    checkIntegration: async (capability) => {
      const paths = resolveFabricPaths();
      const socketPath = process.env.AGENT_FABRIC_SOCKET_PATH ??
        await servingSocketPath(paths.runtimeDirectory, paths.socketPath);
      const transport = await NdjsonRpcTransport.connect(createConnection(socketPath), {
        protocolVersion: 1,
        client: { name: "agent-fabric-herdr-check", version: "1.0.0" },
        authentication: {
          scheme: "capability",
          credential: capability,
          clientNonce: `herdr_check_${randomUUID()}`,
        },
        expectedPrincipalKind: "agent",
        requiredFeatures: ["herdr-control.v1"],
        optionalFeatures: [],
      });
      try {
        if (!transport.allowedOperations.has(FABRIC_OPERATIONS.herdrSteerDispatch)) {
          throw new Error("fabric.v1.herdr-steer.dispatch is not granted");
        }
      } finally {
        await transport.close();
      }
    },
  });
  process.stdout.write(`${JSON.stringify(result)}\n`);
  if (result.status !== "ready") process.exitCode = 1;
}

function isConnectionFailure(error: unknown): boolean {
  return error instanceof Error && "code" in error &&
    typeof error.code === "string" && ["ECONNREFUSED", "ENOENT", "EPIPE", "ECONNRESET"].includes(error.code);
}

async function main(arguments_: string[]): Promise<void> {
  if (arguments_[0] === "status") {
    const [{ fabricStatus }, { resolveFabricPaths }] = await Promise.all([
      import("./status.js"),
      import("./paths.js"),
    ]);
    process.stdout.write(`${JSON.stringify(await fabricStatus(arguments_.slice(1), resolveFabricPaths()), null, 2)}\n`);
    return;
  }
  if (arguments_[0] === "doctor") {
    if (arguments_.some((argument) => ["--help", "-h"].includes(argument))) {
      process.stdout.write(`${DOCTOR_USAGE}\n`);
      return;
    }
    const [{ fabricDoctor }, { resolveFabricPaths }] = await Promise.all([
      import("./status.js"),
      import("./paths.js"),
    ]);
    const output = await fabricDoctor(arguments_.slice(1), resolveFabricPaths({ createDirectories: false }));
    process.stdout.write(`${JSON.stringify(output, null, 2)}\n`);
    if (output.healthy !== true) process.exitCode = 1;
    return;
  }
  if (arguments_[0] === "inspect") {
    await inspect(arguments_.slice(1));
    return;
  }
  if (arguments_[0] === "adapter" && arguments_[1] === "executable") {
    const { resolveAdapterExecutableCli } = await import("./adapter-executable.js");
    process.stdout.write(`${await resolveAdapterExecutableCli(arguments_.slice(2))}\n`);
    return;
  }
  if (arguments_[0] === "receipt" && arguments_[1] === "verify") {
    await verifyReceipt(arguments_.slice(2));
    return;
  }
  if (arguments_[0] === "daemon" && arguments_[1] === "run") {
    const { runForegroundDaemon } = await import("./daemon-run.js");
    await runForegroundDaemon(arguments_.slice(2));
    return;
  }
  if (arguments_[0] === "observe") {
    const { runEventObserver } = await import("./event-observer.js");
    await runEventObserver(arguments_.slice(1));
    return;
  }
  if (arguments_[0] === "herdr" && arguments_[1] === "steer") {
    if (arguments_.length === 3 && arguments_[2] === "--check") {
      await checkHerdrSteer();
      return;
    }
    if (arguments_.length === 3 && ["--help", "-h"].includes(arguments_[2] ?? "")) {
      const { HERDR_STEER_USAGE } = await import("./herdr-steer.js");
      process.stdout.write(`${HERDR_STEER_USAGE}\n`);
      return;
    }
    await herdrSteer(arguments_.slice(2));
    return;
  }
  if (arguments_[0] === "mcp" && arguments_[1] === "provision") {
    const [{ provisionMcpSeats }, { resolveFabricPaths }] = await Promise.all([
      import("./mcp-provision.js"),
      import("./paths.js"),
    ]);
    const output = await provisionMcpSeats(arguments_.slice(2), resolveFabricPaths());
    process.stdout.write(`${JSON.stringify(output, null, 2)}\n`);
    return;
  }
  if (
    (arguments_[0] === "bootstrap" && arguments_.length === 3 && arguments_[1] === "--seat") ||
    (arguments_[0] === "mcp" && arguments_[1] === "bootstrap" && arguments_.length === 4 && arguments_[2] === "--seat")
  ) {
    const seat = arguments_[0] === "bootstrap" ? arguments_[2] : arguments_[3];
    if (seat === undefined) throw new Error("bootstrap requires --seat claude|codex");
    const [{ bootstrapMcpSeat, bootstrapMcpSeatIdentity }, { resolveFabricPaths }] = await Promise.all([
      import("./mcp-bootstrap.js"),
      import("./paths.js"),
    ]);
    const output = await bootstrapMcpSeat({
      environment: { ...process.env, AGENT_FABRIC_SEAT: seat },
      cwd: process.cwd(),
      paths: resolveFabricPaths(),
    });
    const { credential: _credential, credentials, ...safeOutput } = output;
    const publicOutput = {
      ...safeOutput,
      ...bootstrapMcpSeatIdentity(output, seat),
      credentials: credentials.map(({ capability: _capability, ...metadata }) => metadata),
    };
    process.stdout.write(`${JSON.stringify(publicOutput, null, 2)}\n`);
    // The seat is installed and durable either way; an unhealthy smoke is
    // reported in the receipt and must still fail the process boundary.
    if (!output.receipt.healthy) process.exitCode = 1;
    return;
  }
  if (
    (arguments_[0] === "bootstrap" && arguments_.includes("--inspect")) ||
    (arguments_[0] === "mcp" && arguments_[1] === "bootstrap" && arguments_.includes("--inspect"))
  ) {
    const bootstrapArguments = arguments_[0] === "bootstrap" ? arguments_.slice(1) : arguments_.slice(2);
    const seatIndex = bootstrapArguments.indexOf("--seat");
    const seat = seatIndex === -1 ? process.env.AGENT_FABRIC_SEAT : bootstrapArguments[seatIndex + 1];
    const expected = seatIndex === -1 ? ["--inspect"] : ["--inspect", "--seat", seat];
    if (seat === undefined || bootstrapArguments.length !== expected.length || !bootstrapArguments.includes("--inspect")) {
      throw new Error("bootstrap --inspect accepts only --seat claude|codex");
    }
    const [{ inspectBootstrapMcpSeat }, { resolveFabricPaths }] = await Promise.all([
      import("./mcp-bootstrap.js"),
      import("./paths.js"),
    ]);
    const output = await inspectBootstrapMcpSeat({
      environment: { ...process.env, AGENT_FABRIC_SEAT: seat },
      cwd: process.cwd(),
      paths: resolveFabricPaths({ createDirectories: false }),
    });
    process.stdout.write(`${JSON.stringify(output, null, 2)}\n`);
    return;
  }
  if (arguments_[0] === "mcp" && arguments_[1] === "seat-path") {
    const [{ mcpSeatPath }, { resolveFabricPaths }] = await Promise.all([
      import("./mcp-provision.js"),
      import("./paths.js"),
    ]);
    const output = await mcpSeatPath(arguments_.slice(2), resolveFabricPaths());
    process.stdout.write(`${JSON.stringify(output, null, 2)}\n`);
    return;
  }
  if (arguments_[0] === "mcp" && arguments_[1] === "observer-provision") {
    const projectIndex = arguments_.indexOf("--project");
    const project = projectIndex === -1 ? undefined : arguments_[projectIndex + 1];
    if (project === undefined || arguments_.length !== 4) throw new Error("mcp observer-provision requires --project <path>");
    const [{ provisionObserverCredential }, { resolveFabricPaths }] = await Promise.all([
      import("./observer-provision.js"),
      import("./paths.js"),
    ]);
    const output = await provisionObserverCredential({ project, paths: resolveFabricPaths() });
    process.stdout.write(`${JSON.stringify(output, null, 2)}\n`);
    return;
  }
  if (arguments_[0] === "workspace") {
    const [{ runWorkspaceTrust }, { resolveFabricPaths }] = await Promise.all([
      import("./workspace-trust.js"),
      import("./paths.js"),
    ]);
    const output = await runWorkspaceTrust(arguments_.slice(1), resolveFabricPaths());
    process.stdout.write(`${JSON.stringify(output, null, 2)}\n`);
    return;
  }
  if (arguments_[0] === "retention") {
    const [{ runRetentionCli }, { resolveFabricPaths }] = await Promise.all([
      import("./retention.js"),
      import("./paths.js"),
    ]);
    const output = await runRetentionCli(arguments_.slice(1), resolveFabricPaths().databasePath);
    process.stdout.write(`${JSON.stringify(output, null, 2)}\n`);
    return;
  }
  if (arguments_[0] === "database" && arguments_[1] === "archive-and-fresh") {
    const [{ runDatabaseArchiveAndFreshCli }, { resolveFabricPaths }] = await Promise.all([
      import("./database-archive-and-fresh.js"),
      import("./paths.js"),
    ]);
    const output = await runDatabaseArchiveAndFreshCli(
      arguments_.slice(2),
      resolveFabricPaths({ createDirectories: false }).databasePath,
    );
    process.stdout.write(`${JSON.stringify(output.result, null, 2)}\n`);
    if (output.exitCode !== 0) process.exitCode = output.exitCode;
    return;
  }
  throw new Error(
    "usage: agent-fabric status [--project PATH] [--agents-home PATH] [--trusted-config PATH] [--compatibility PATH] [--compatibility-schema PATH] | doctor [--consume-provider-quota] [--project PATH] [--agents-home PATH] [--trusted-config PATH] [--compatibility PATH] [--compatibility-schema PATH] | bootstrap --seat claude|codex [--inspect] | inspect [--database PATH] [--runtime-directory PATH] [--json] | adapter executable --adapter ID [--agents-home PATH] [--config PATH] [--compatibility PATH] [--compatibility-schema PATH] | workspace trust|inspect|status|list|revoke [PATH] | retention status|preview [--database PATH] | retention archive --run-id ID --output ABSOLUTE_DIRECTORY [--database PATH] | database archive-and-fresh [--database PATH] --archive ABSOLUTE_NEW_DIRECTORY [--confirm-source-set sha256:DIGEST] | receipt verify --run-receipt PATH | daemon run (...) | observe --socket PATH --capability-file PATH --run-id ID --cursor PATH [--once] [--interval-ms N] | herdr steer (...) | mcp provision --project PATH --project-session-id ID --session-revision N --session-generation N --run-id ID --run-revision N --chair-seat SEAT --chair-agent-id ID --chair-generation N --chair-lease-id ID --seat-bindings SEAT=AGENT@GENERATION,... --expires-at ISO_TIMESTAMP | mcp seat-path --project PATH --seat SEAT",
  );
}

try {
  await main(process.argv.slice(2));
} catch (error: unknown) {
  // A schema cutover gate is a user decision, not a failure report: emit the
  // machine-readable gate on stdout so the agent can relay counts and
  // consequences without displacing anything.
  const { McpBootstrapSchemaCutoverGateError } = await import("./mcp-bootstrap.js");
  if (error instanceof McpBootstrapSchemaCutoverGateError) {
    process.stdout.write(`${JSON.stringify(error.gate, null, 2)}\n`);
  }
  process.stderr.write(`${error instanceof Error ? error.message : String(error)}\n`);
  process.exitCode = 1;
}
