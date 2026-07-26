import type { FabricConsoleDataset } from "./protocol-adapter.js";

export function capacityLabel(dataset: FabricConsoleDataset): string {
  const fact = dataset.snapshot?.capacity;
  const value = fact !== undefined &&
      (fact.freshness === "live" ||
        fact.freshness === "snapshot" ||
        fact.freshness === "stale")
    ? fact.value
    : null;
  if (value === null) return "unknown";
  return Object.entries(value)
    .map(([name, capacity]) => {
      if (
        typeof capacity === "object" &&
        capacity !== null &&
        !Array.isArray(capacity)
      ) {
        const used = Reflect.get(capacity, "used");
        const reserved = Reflect.get(capacity, "reserved");
        const limit = Reflect.get(capacity, "limit");
        if (
          typeof used === "number" &&
          typeof reserved === "number" &&
          typeof limit === "number"
        ) {
          return `${name}:${String(used)}+${String(reserved)}/${String(limit)}`;
        }
      }
      return `${name}:declared`;
    })
    .join(" ") || "declared";
}
