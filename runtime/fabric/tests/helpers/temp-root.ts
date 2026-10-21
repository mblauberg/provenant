import { mkdtempSync, realpathSync } from "node:fs";
import { tmpdir } from "node:os";
import { join, resolve, sep } from "node:path";

/**
 * A fresh temp root for a fixture workspace. A Provenant lane sets TMPDIR inside
 * its attempt directory, which sits under a real .agent-run tree; a fixture
 * rooted there leaks into that run listing (issue #1002). In that case use the
 * fixed system temp path instead.
 */
export function scratchRoot(prefix: string): string {
  const preferred = realpathSync.native(resolve(tmpdir()));
  const base = preferred.split(sep).includes(".agent-run") ? "/tmp" : preferred;
  return realpathSync.native(mkdtempSync(join(base, prefix)));
}
