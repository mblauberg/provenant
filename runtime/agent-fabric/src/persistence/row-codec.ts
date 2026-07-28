import { createHash } from "node:crypto";

export type Row = Record<string, unknown>;

export function isRow(value: unknown): value is Row {
  return typeof value === "object" && value !== null && !Array.isArray(value);
}

export function row(value: unknown, label: string): Row {
  if (!isRow(value)) throw new Error(`${label} was not found`);
  return value;
}

export function text(value: Row, field: string): string {
  const item = value[field];
  if (typeof item !== "string") throw new Error(`${field} is not text`);
  return item;
}

export function nullableText(value: Row, field: string): string | null {
  const item = value[field];
  if (typeof item !== "string" && item !== null) throw new Error(`${field} is not nullable text`);
  return item;
}

export function integer(value: Row, field: string): number {
  const item = value[field];
  if (typeof item !== "number" || !Number.isSafeInteger(item)) throw new Error(`${field} is not an integer`);
  return item;
}

export function canonicalJson(value: unknown): string {
  if (value === null || typeof value === "boolean" || typeof value === "number" || typeof value === "string") {
    return JSON.stringify(value);
  }
  if (Array.isArray(value)) return `[${value.map(canonicalJson).join(",")}]`;
  if (isRow(value)) {
    return `{${Object.keys(value).sort().map((key) => `${JSON.stringify(key)}:${canonicalJson(value[key])}`).join(",")}}`;
  }
  throw new TypeError("value is not JSON-compatible");
}

export function sha256(value: string): string {
  return createHash("sha256").update(value).digest("hex");
}

export function digest(value: unknown): `sha256:${string}` {
  return `sha256:${sha256(canonicalJson(value))}`;
}

export function stringDigest(value: string): `sha256:${string}` {
  return `sha256:${sha256(value)}`;
}

export function timestampToMillis(value: string): number {
  const parsed = Date.parse(value);
  if (!Number.isFinite(parsed)) throw new TypeError("timestamp is invalid");
  return parsed;
}
