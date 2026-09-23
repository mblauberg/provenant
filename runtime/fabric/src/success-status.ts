/** Drop the alias when pre-upgrade succeeded receipts have expired. */
export function canonicalSuccessStatus(status: unknown): unknown {
  return status === "succeeded" ? "ok" : status;
}

export function isSuccessStatus(status: unknown): boolean {
  return canonicalSuccessStatus(status) === "ok";
}
