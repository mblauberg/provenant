import {
  MCP_BOOTSTRAP_CREDENTIALS_FEATURE,
  MCP_BOOTSTRAP_RESULT_SHAPE_FEATURE,
} from "@local/agent-fabric-protocol";

import { isRecord } from "../domain/record.js";
import { FABRIC_PROTOCOL_LIMITS, type FabricProtocolLimits } from "./bounded-ndjson.js";

export const FABRIC_PROTOCOL_VERSION = 1 as const;
export const FABRIC_DAEMON_VERSION = "0.1.0";

export type DaemonInitializeParams = {
  protocolVersion: number;
  client: { name: string; version: string };
  capabilities: string[];
};

export type DaemonInitializeResult = {
  protocolVersion: typeof FABRIC_PROTOCOL_VERSION;
  daemonVersion: string;
  capabilities: string[];
  limits: FabricProtocolLimits;
  activeAdapters: string[];
};

export function daemonInitializeResult(activeAdapters: string[]): DaemonInitializeResult {
  return {
    protocolVersion: FABRIC_PROTOCOL_VERSION,
    daemonVersion: FABRIC_DAEMON_VERSION,
    capabilities: ["rpc", MCP_BOOTSTRAP_CREDENTIALS_FEATURE, MCP_BOOTSTRAP_RESULT_SHAPE_FEATURE],
    limits: FABRIC_PROTOCOL_LIMITS,
    activeAdapters: [...new Set(activeAdapters)].sort(),
  };
}

export type DaemonRequest = {
  id: string;
  capability: string;
  method: string;
  params: Record<string, unknown>;
};

export type DaemonResponse =
  | { id: string; result: unknown }
  | { id: string; error: { name: string; code: string; message: string } };

export function isDaemonRequest(value: unknown): value is DaemonRequest {
  return (
    isRecord(value) &&
    typeof value.id === "string" &&
    typeof value.capability === "string" &&
    typeof value.method === "string" &&
    isRecord(value.params)
  );
}

export function isDaemonResponse(value: unknown): value is DaemonResponse {
  if (!isRecord(value) || typeof value.id !== "string") {
    return false;
  }
  const hasResult = Object.hasOwn(value, "result");
  const hasError = Object.hasOwn(value, "error");
  return hasResult !== hasError && (
    hasResult || (
      isRecord(value.error) &&
      typeof value.error.name === "string" &&
      typeof value.error.code === "string" &&
      typeof value.error.message === "string"
    )
  );
}

export function daemonInitializeParams(value: Record<string, unknown>): DaemonInitializeParams {
  if (
    Object.keys(value).some((key) => !["protocolVersion", "client", "capabilities"].includes(key)) ||
    typeof value.protocolVersion !== "number" ||
    !Number.isSafeInteger(value.protocolVersion) ||
    !isRecord(value.client) ||
    typeof value.client.name !== "string" ||
    typeof value.client.version !== "string" ||
    Object.keys(value.client).some((key) => !["name", "version"].includes(key)) ||
    !Array.isArray(value.capabilities) ||
    !value.capabilities.every((capability) => typeof capability === "string")
  ) {
    throw new TypeError("daemon initialize parameters are invalid");
  }
  return {
    protocolVersion: value.protocolVersion,
    client: { name: value.client.name, version: value.client.version },
    capabilities: value.capabilities,
  };
}

export function isDaemonInitializeResult(value: unknown): value is DaemonInitializeResult {
  const limits = isRecord(value) ? value.limits : undefined;
  if (
    !isRecord(value) ||
    Object.keys(value).some((key) => !["protocolVersion", "daemonVersion", "capabilities", "limits", "activeAdapters"].includes(key)) ||
    value.protocolVersion !== FABRIC_PROTOCOL_VERSION ||
    typeof value.daemonVersion !== "string" ||
    !Array.isArray(value.capabilities) ||
    !value.capabilities.every((capability) => typeof capability === "string") ||
    !value.capabilities.includes("rpc") ||
    !Array.isArray(value.activeAdapters) ||
    !value.activeAdapters.every((adapter) => typeof adapter === "string") ||
    new Set(value.activeAdapters).size !== value.activeAdapters.length ||
    !isRecord(limits)
  ) return false;
  if (Object.keys(limits).some((key) => !Object.hasOwn(FABRIC_PROTOCOL_LIMITS, key))) return false;
  return Object.entries(FABRIC_PROTOCOL_LIMITS).every(([key, maximum]) => {
    const effective = limits[key];
    return typeof effective === "number" && Number.isSafeInteger(effective) && effective > 0 && effective <= maximum;
  });
}
