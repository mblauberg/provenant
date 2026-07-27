import { mkdtemp, readFile, rm } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";

import Database from "better-sqlite3";
import { afterEach, describe, expect, it, vi } from "vitest";

const terminal = vi.hoisted(() => ({
  handle: 45_000,
  input: Buffer.alloc(0),
  offset: 0,
  prompt: "",
}));

vi.mock("node:fs", async (importOriginal) => {
  const actual = await importOriginal<typeof import("node:fs")>();
  return {
    ...actual,
    openSync: (...arguments_: unknown[]) => {
      if (arguments_[0] === "/dev/tty") return terminal.handle;
      return Reflect.apply(actual.openSync, actual, arguments_) as number;
    },
    readSync: (...arguments_: unknown[]) => {
      if (arguments_[0] !== terminal.handle) {
        return Reflect.apply(actual.readSync, actual, arguments_) as number;
      }
      if (terminal.offset >= terminal.input.length) return 0;
      const buffer = arguments_[1] as Buffer;
      const targetOffset = arguments_[2] as number;
      buffer[targetOffset] = terminal.input[terminal.offset] ?? 0;
      terminal.offset += 1;
      return 1;
    },
    writeSync: (...arguments_: unknown[]) => {
      if (arguments_[0] !== terminal.handle) {
        return Reflect.apply(actual.writeSync, actual, arguments_) as number;
      }
      const value = arguments_[1];
      const text = typeof value === "string"
        ? value
        : Buffer.from(value as Uint8Array).toString("utf8");
      terminal.prompt += text;
      return Buffer.byteLength(text);
    },
    closeSync: (...arguments_: unknown[]) => {
      if (arguments_[0] === terminal.handle) return;
      Reflect.apply(actual.closeSync, actual, arguments_);
    },
  };
});

import { runDatabaseArchiveAndFreshCli } from "../../src/cli/database-archive-and-fresh-cli.ts";

const cleanup: string[] = [];

afterEach(async () => {
  terminal.input = Buffer.alloc(0);
  terminal.offset = 0;
  terminal.prompt = "";
  await Promise.all(cleanup.splice(0).map((path) => rm(path, { recursive: true, force: true })));
});

async function fixture(): Promise<Readonly<{
  databasePath: string;
  archiveDirectory: string;
  sourceSetSha256: string;
}>> {
  const root = await mkdtemp(join(tmpdir(), "fabric-cutover-interactive-unit-"));
  cleanup.push(root);
  const databasePath = join(root, "fabric.sqlite3");
  const archiveDirectory = join(root, "archive");
  const database = new Database(databasePath);
  database.exec("CREATE TABLE legacy_sentinel(value TEXT NOT NULL)");
  database.close();
  const preview = runDatabaseArchiveAndFreshCli(
    ["--archive", archiveDirectory],
    databasePath,
  );
  if (preview.result.status !== "confirmation-required") {
    throw new TypeError("expected confirmation preview");
  }
  return {
    databasePath,
    archiveDirectory,
    sourceSetSha256: preview.result.confirmation.sourceSetSha256,
  };
}

describe("database cutover interactive flow", () => {
  it("re-inspects with a verified approval and records the interactive receipt", async () => {
    const value = await fixture();
    terminal.input = Buffer.from("ARCHIVE-AND-FRESH\n");

    const result = runDatabaseArchiveAndFreshCli(
      [
        "--archive",
        value.archiveDirectory,
        "--confirm-source-set",
        value.sourceSetSha256,
      ],
      value.databasePath,
    );

    expect(result).toMatchObject({ exitCode: 0, result: { status: "completed" } });
    expect(terminal.prompt).toBe(
      "Type ARCHIVE-AND-FRESH to confirm live coordination-state displacement: ",
    );
    const receiptText = await readFile(
      join(value.archiveDirectory, "source-set", "receipt.json"),
      "utf8",
    );
    expect(JSON.parse(receiptText)).toMatchObject({
      status: "completed",
      approval: {
        kind: "interactive-confirmation",
        status: "verified",
      },
    });
  });

  it("returns the documented mismatch without publishing or displacing", async () => {
    const value = await fixture();
    const before = await readFile(value.databasePath);
    terminal.input = Buffer.from("not-the-confirmation\n");

    const result = runDatabaseArchiveAndFreshCli(
      [
        "--archive",
        value.archiveDirectory,
        "--confirm-source-set",
        value.sourceSetSha256,
      ],
      value.databasePath,
    );

    expect(result).toMatchObject({
      exitCode: 1,
      result: {
        status: "approval-required",
        code: "CUTOVER_INTERACTIVE_CONFIRMATION_MISMATCH",
        message: "interactive confirmation did not match ARCHIVE-AND-FRESH",
        sourcePreserved: true,
      },
    });
    expect(await readFile(value.databasePath)).toEqual(before);
    await expect(readFile(
      join(value.archiveDirectory, "source-set", "receipt.json"),
    )).rejects.toMatchObject({ code: "ENOENT" });
  });
});
