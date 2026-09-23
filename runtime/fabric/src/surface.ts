/** Small presentation boundary. Python owns execution digests. */
import { existsSync, readdirSync, statSync, readFileSync, mkdirSync, writeFileSync } from "node:fs";
import { join, resolve, dirname } from "node:path";
import { homedir } from "node:os";
import type { CatalogueSnapshot } from "./catalogue.js";
import type { Message } from "./store.js";
import { databasePath } from "./identity.js";

export function digest(row: Record<string, any>): string {
  if (typeof row.digest === "string") return row.digest;
  if (Array.isArray(row.digest)) return row.digest.join("\n");
  if (row.error || row.status === "rejected")
    return `rejected ${row.error ?? "request"} · fix: ${row.fix ?? "Inspect the run receipt."}`;
  if (Array.isArray(row.runs)) {
    const lines = row.runs.map(digest).join("\n") || "no runs";
    const first = row.runs[0];
    if (first?.batch_id && row.runs.every((run: Record<string, any>) => run.run_id === first.run_id)) {
      const counts = row.runs.reduce((counts: Record<string, number>, run: Record<string, any>) => {
        const state = String(run.status ?? run.state);
        counts[state] = (counts[state] ?? 0) + 1;
        return counts;
      }, {});
      return `batch ${first.run_id} ${row.runs.length} tasks: ${Object.entries(counts)
        .map(([state, count]) => `${count} ${state}`)
        .join(" ")}\n${lines}`;
    }
    return lines;
  }
  if (row.status) {
    const id = row.run_id ?? row.id ?? row.task_id ?? "?";
    const route = row.provenance?.line;
    const adapter = row.adapter ?? row.route?.adapter;
    const model = row.model ?? row.route?.resolved_model;
    const effort = row.provenance?.effort_applied ?? row.route?.effort;
    const routeText = adapter && model ? ` ${adapter}/${model}${effort ? `@${effort}` : ""}` : "";
    return `${row.status} ${id}${routeText}${row.result_path ? ` · result ${row.result_path}` : row.state === "running" ? ` · fabric_status{ids:["${id}"],wait_seconds:55}` : ""}${route ? `\n  ${route}` : ""}`;
  }
  return JSON.stringify(row);
}
export function reply(value: unknown) {
  const payload = Array.isArray(value) ? { items: value } : (value as Record<string, any>);
  return { content: [{ type: "text" as const, text: digest(payload) }], structuredContent: payload };
}
const boot = Date.now();
export function serverBuild(root = resolve(import.meta.dirname, ".."), loadedAt = boot) {
  const version = JSON.parse(readFileSync(join(root, "package.json"), "utf8")).version;
  const source = join(root, "src");
  const build_stale = readdirSync(source).some((name) => statSync(join(source, name)).mtimeMs > loadedAt);
  return {
    server_version: version,
    build_stale,
    ...(build_stale ? { fix: "Restart the Fabric MCP server to load changed sources." } : {}),
  };
}
export function mailboxView(row: Message, peek: boolean) {
  if (peek) return { id: row.messageId, from: row.from, kind: row.kind, preview: row.body.slice(0, 80) };
  const data = Buffer.from(row.body);
  if (data.length <= 4096) return row;
  const root = join(dirname(databasePath()), "message-bodies");
  mkdirSync(root, { recursive: true, mode: 0o700 });
  const body_path = join(root, `${row.messageId}.txt`);
  writeFileSync(body_path, row.body, { mode: 0o600 });
  return {
    ...row,
    body: data
      .subarray(0, 4096)
      .toString("utf8")
      .replace(/\uFFFD$/u, ""),
    body_path,
  };
}
export function adapterView(snapshot: CatalogueSnapshot, detail?: string) {
  let cooldowns: Record<string, any> = {};
  try {
    const value = JSON.parse(readFileSync(join(homedir(), ".local/state/agent-harness/fabric/cooldowns.json"), "utf8"));
    cooldowns = value.cooldowns ?? value.entries ?? value;
  } catch {
    /* Optional cache. */
  }
  const adapters = snapshot.adapters.map((adapter) => {
    const binary: Record<string, string> = { cursor: "cursor-agent", kiro: "kiro-cli", agy: "agy" };
    const installed = (process.env.PATH ?? "")
      .split(":")
      .some((dir) => existsSync(join(dir, binary[adapter.name] ?? adapter.name)));
    const cooling = Object.values(cooldowns).filter(
      (c) => c?.adapter === adapter.name && Date.parse(c.cooling_until) > Date.now(),
    );
    return `${adapter.name} ${installed ? "ok auth?" : "cli missing"} ${adapter.models.join("|")} default=${adapter.aliases.workhorse?.[0] ?? "-"} ro=${adapter.read_only_guarantee ?? "unknown"} write=${adapter.write_modes?.length ? "yes" : "no"}${
      cooling.length
        ? ` cooling until ${cooling
            .map((c) => c.cooling_until)
            .sort()
            .at(-1)}`
        : ""
    }`;
  });
  const drift = (snapshot as unknown as { drift?: string[] }).drift ?? [];
  return {
    digest: [...adapters, ...drift.map((note) => `note: ${note}`)].join("\n"),
    ...(detail === "full" ? snapshot : {}),
  };
}
