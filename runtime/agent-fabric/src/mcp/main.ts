import { StdioServerTransport } from "@modelcontextprotocol/sdk/server/stdio.js";

import {
  createFabricMcpServer,
  createUnprovisionedMcpServer,
  type FabricMcpServerHandle,
} from "./server.js";
import { McpSeatNotProvisionedError, resolveMcpCapability, resolveRenewableMcpCapability } from "./credentials.js";
import { resolveFabricPaths } from "../cli/paths.js";
import { bootstrapMcpSeat } from "../cli/mcp-bootstrap.js";

// Stdio MCP proxy entry point (spec section 14): one proxy process per client,
// each connecting to the shared daemon socket with its own capability. The
// proxy holds no fabric state and enforces no policy; the daemon derives the
// principal from the capability, never from MCP arguments.

const paths = resolveFabricPaths();
const socketPath = process.env.AGENT_FABRIC_SOCKET_PATH ?? paths.socketPath;
const effectivePaths = { ...paths, socketPath };
const renewCurrentSeat = async (): Promise<void> => {
  await bootstrapMcpSeat({
    environment: process.env,
    cwd: process.cwd(),
    paths: effectivePaths,
  });
};
const resolveCurrentCapability = async (): Promise<string> => process.env.AGENT_FABRIC_SEAT === undefined
  ? await resolveMcpCapability(process.env, process.cwd())
  : await resolveRenewableMcpCapability(
    process.env,
    process.cwd(),
    renewCurrentSeat,
    (message) => process.stderr.write(`warning: ${message}\n`),
  );

let handle: FabricMcpServerHandle;
try {
  const capability = await resolveCurrentCapability();
  handle = await createFabricMcpServer({
    socketPath,
    capability,
    ...(process.env.AGENT_FABRIC_SEAT === undefined
      ? {}
      : { refreshCapability: resolveCurrentCapability }),
    ...(process.env.AGENT_FABRIC_CLIENT_LABEL === undefined
      ? {}
      : { clientLabel: process.env.AGENT_FABRIC_CLIENT_LABEL }),
  });
} catch (error: unknown) {
  if (!(error instanceof McpSeatNotProvisionedError)) {
    process.stderr.write(`${error instanceof Error ? error.message : String(error)}\n`);
    process.exit(2);
  }
  process.stderr.write(`warning: ${error.message}; exact-root bootstrap is available\n`);
  handle = createUnprovisionedMcpServer({
    bootstrap: async () => {
      const bootstrapped = await bootstrapMcpSeat({
        environment: process.env,
        cwd: process.cwd(),
        paths: effectivePaths,
      });
      return {
        socketPath,
        capability: bootstrapped.credential,
        refreshCapability: resolveCurrentCapability,
        lifecycleReceipt: bootstrapped.receipt,
        ...(process.env.AGENT_FABRIC_CLIENT_LABEL === undefined
          ? {}
          : { clientLabel: process.env.AGENT_FABRIC_CLIENT_LABEL }),
      };
    },
  });
}
delete process.env.AGENT_FABRIC_CAPABILITY;

const transport = new StdioServerTransport();
await handle.server.connect(transport);

let shutdownPromise: Promise<void> | undefined;
const shutdown = (): void => {
  shutdownPromise ??= handle.close().finally(() => process.exit(0));
};

process.on("SIGTERM", shutdown);
process.on("SIGINT", shutdown);
process.stdin.on("end", shutdown);
process.stdin.on("close", shutdown);
