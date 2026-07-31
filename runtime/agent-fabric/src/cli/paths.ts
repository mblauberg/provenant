import { chmodSync, mkdirSync } from "node:fs";
import { homedir } from "node:os";
import { join, resolve } from "node:path";

export type FabricPaths = {
  stateDirectory: string;
  runtimeDirectory: string;
  databasePath: string;
  socketPath: string;
};

function environmentPath(name: string): string | undefined {
  const value = process.env[name];
  return value === undefined || value.length === 0 ? undefined : resolve(value);
}

function privateDirectory(path: string): string {
  mkdirSync(path, { recursive: true, mode: 0o700 });
  chmodSync(path, 0o700);
  return path;
}

export function resolveFabricPaths(options: { createDirectories?: boolean } = {}): FabricPaths {
  const resolveDirectory = options.createDirectories === true ? privateDirectory : (path: string): string => path;
  const stateDirectory = resolveDirectory(
    environmentPath("AGENT_FABRIC_STATE_DIRECTORY") ??
      join(environmentPath("XDG_STATE_HOME") ?? join(environmentPath("HOME") ?? homedir(), ".local", "state"), "agent-harness", "fabric"),
  );
  const runtimeDirectory = resolveDirectory(
    environmentPath("AGENT_FABRIC_RUNTIME_DIRECTORY") ??
      join(stateDirectory, "runtime"),
  );
  return {
    stateDirectory,
    runtimeDirectory,
    databasePath: environmentPath("AGENT_FABRIC_DATABASE_PATH") ?? join(stateDirectory, "fabric-v1.sqlite3"),
    socketPath: join(runtimeDirectory, "fabric-v1.sock"),
  };
}

export function ensureFabricPaths(paths: FabricPaths): FabricPaths {
  privateDirectory(paths.stateDirectory);
  privateDirectory(paths.runtimeDirectory);
  return paths;
}
