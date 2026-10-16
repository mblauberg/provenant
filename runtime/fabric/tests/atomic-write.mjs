import { renameSync, writeFileSync } from "node:fs";

// Write-then-rename so a concurrent reader sees the old or new file, never a
// truncated one. The temp name ends in ".tmp" and so matches no "*.json" or
// "attempt-\d+" reader pattern.
export function writeFileAtomic(path, data) {
  const tmp = `${path}.${process.pid}.tmp`;
  writeFileSync(tmp, data);
  renameSync(tmp, path);
}
