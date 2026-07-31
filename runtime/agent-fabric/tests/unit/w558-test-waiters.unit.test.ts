import { readdirSync, readFileSync } from "node:fs";
import { join, relative, resolve, sep } from "node:path";

import { describe, expect, it } from "vitest";

/**
 * One waiter, enforced.
 *
 * `tests/shared/deadline-wait.ts` owns waiting for this package. This test
 * refuses the two shapes that grew back four times before issue #558: a
 * hand-rolled deadline loop, and a bare sleep followed by an assertion.
 *
 * It is a scanner rather than an AST walk because TypeScript 7 ships no
 * JavaScript compiler API (`ts.createSourceFile` is undefined at 7.0.2). To stay
 * honest without a parser it masks comments, strings, template literals and
 * regex literals first, so the child-process scripts this suite embeds in
 * template literals — which legitimately contain their own timers and loops —
 * are not mistaken for test code.
 */

type WaitAllowance = Readonly<{
  id: string;
  site: string;
  reason: string;
}>;

type WaiterGolden = Readonly<{
  schema_version: 1;
  issue: string;
  wait_port: string;
  temporary_hand_rolled_waits: readonly WaitAllowance[];
}>;

type Offence = Readonly<{
  site: string;
  rule: "hand-rolled-deadline-loop" | "sleep-and-assert";
  detail: string;
}>;

const testRoot = resolve(import.meta.dirname, "..");
const goldenPath = resolve(testRoot, "fixtures/w558-test-waiters.json");
const waitPort = "shared/deadline-wait.ts";

const CLOCK_READS = ["Date.now(", "performance.now(", "process.hrtime", "hrtime.bigint("];

function testFiles(directory: string): string[] {
  return readdirSync(directory, { withFileTypes: true })
    .flatMap((entry) => {
      const path = join(directory, entry.name);
      if (entry.isDirectory()) return testFiles(path);
      return entry.isFile() && entry.name.endsWith(".ts") ? [path] : [];
    })
    .sort();
}

function isIdentifierPart(character: string | undefined): boolean {
  return character !== undefined && /[\p{L}\p{N}_$]/u.test(character);
}

/** True where a `/` opens a regex literal rather than dividing. */
function regexCanStartHere(masked: readonly string[], index: number): boolean {
  let cursor = index - 1;
  while (cursor >= 0 && /\s/u.test(masked[cursor] ?? "")) cursor -= 1;
  if (cursor < 0) return true;
  const previous = masked[cursor] ?? "";
  if ("(,=:[!&|?{;}+-*%<>~^".includes(previous)) return true;
  if (!isIdentifierPart(previous)) return false;
  let start = cursor;
  while (start >= 0 && isIdentifierPart(masked[start])) start -= 1;
  const word = masked.slice(start + 1, cursor + 1).join("");
  return ["return", "typeof", "instanceof", "in", "of", "new", "delete", "void", "throw", "case", "do", "else", "yield", "await"].includes(word);
}

/**
 * Replace every comment, string, template literal and regex literal with spaces,
 * preserving offsets and newlines so line numbers and brace matching stay true.
 */
function maskLiterals(source: string): string {
  const out = [...source];
  const length = source.length;
  let index = 0;
  const blank = (at: number): void => {
    if (source[at] !== "\n") out[at] = " ";
  };
  while (index < length) {
    const character = source[index];
    const next = source[index + 1];
    if (character === "/" && next === "/") {
      while (index < length && source[index] !== "\n") { blank(index); index += 1; }
      continue;
    }
    if (character === "/" && next === "*") {
      blank(index); blank(index + 1); index += 2;
      while (index < length && !(source[index] === "*" && source[index + 1] === "/")) { blank(index); index += 1; }
      if (index < length) { blank(index); blank(index + 1); index += 2; }
      continue;
    }
    if (character === '"' || character === "'") {
      const quote = character;
      blank(index); index += 1;
      while (index < length && source[index] !== quote) {
        if (source[index] === "\\") { blank(index); index += 1; }
        if (index < length) { blank(index); index += 1; }
      }
      if (index < length) { blank(index); index += 1; }
      continue;
    }
    if (character === "`") {
      // Mask the whole template, substitutions included: embedded child scripts
      // live here and must not be read as this package's test code.
      let depth = 0;
      blank(index); index += 1;
      while (index < length) {
        const current = source[index];
        if (current === "\\") { blank(index); blank(index + 1); index += 2; continue; }
        if (current === "`" && depth === 0) { blank(index); index += 1; break; }
        if (current === "$" && source[index + 1] === "{") { depth += 1; blank(index); blank(index + 1); index += 2; continue; }
        if (current === "}" && depth > 0) { depth -= 1; blank(index); index += 1; continue; }
        if (current === "{" && depth > 0) { depth += 1; blank(index); index += 1; continue; }
        blank(index); index += 1;
      }
      continue;
    }
    if (character === "/" && regexCanStartHere(out, index)) {
      let inClass = false;
      blank(index); index += 1;
      while (index < length) {
        const current = source[index];
        if (current === "\n") break;
        if (current === "\\") { blank(index); blank(index + 1); index += 2; continue; }
        if (current === "[") inClass = true;
        else if (current === "]") inClass = false;
        else if (current === "/" && !inClass) { blank(index); index += 1; break; }
        blank(index); index += 1;
      }
      continue;
    }
    index += 1;
  }
  return out.join("");
}

function lineOf(source: string, index: number): number {
  let line = 1;
  for (let cursor = 0; cursor < index; cursor += 1) if (source[cursor] === "\n") line += 1;
  return line;
}

function matchDelimiter(masked: string, openIndex: number, open: string, close: string): number {
  let depth = 0;
  for (let cursor = openIndex; cursor < masked.length; cursor += 1) {
    if (masked[cursor] === open) depth += 1;
    else if (masked[cursor] === close) {
      depth -= 1;
      if (depth === 0) return cursor;
    }
  }
  return masked.length - 1;
}

function skipSpace(masked: string, from: number): number {
  let cursor = from;
  while (cursor < masked.length && /\s/u.test(masked[cursor] ?? "")) cursor += 1;
  return cursor;
}

type LoopSpan = Readonly<{ keyword: string; header: string; body: string; index: number }>;

function loopSpans(masked: string): LoopSpan[] {
  const spans: LoopSpan[] = [];
  const keywords = /\b(for|while|do)\b/gu;
  for (let match = keywords.exec(masked); match !== null; match = keywords.exec(masked)) {
    const keyword = match[1] ?? "";
    const start = match.index;
    if (isIdentifierPart(masked[start - 1])) continue;
    let header = "";
    let cursor = start + keyword.length;
    if (keyword === "for" || keyword === "while") {
      cursor = skipSpace(masked, cursor);
      if (masked[cursor] !== "(") continue;
      const headerEnd = matchDelimiter(masked, cursor, "(", ")");
      header = masked.slice(cursor, headerEnd + 1);
      cursor = skipSpace(masked, headerEnd + 1);
    } else {
      cursor = skipSpace(masked, cursor);
    }
    let body: string;
    if (masked[cursor] === "{") {
      body = masked.slice(cursor, matchDelimiter(masked, cursor, "{", "}") + 1);
    } else {
      const semicolon = masked.indexOf(";", cursor);
      body = masked.slice(cursor, semicolon === -1 ? masked.length : semicolon + 1);
    }
    spans.push({ keyword, header, body, index: start });
  }
  return spans;
}

/** A `for … of` / `for … in` walks a collection; it is not a deadline loop. */
function iteratesCollection(span: LoopSpan): boolean {
  return span.keyword === "for" && /\b(of|in)\b/u.test(span.header);
}

/**
 * True when the clock decides the loop rather than merely being sampled by it.
 * A benchmark timing each iteration reads a clock too; what marks a deadline
 * loop is a reading that gates the loop's exit.
 */
function clockDecidesTheLoop(span: LoopSpan): boolean {
  if (CLOCK_READS.some((call) => span.header.includes(call))) return true;
  return span.body.split("\n").some((line) =>
    CLOCK_READS.some((call) => line.includes(call))
    && (/\b(if|while|return|break|throw|continue)\b/u.test(line) || /[<>]=?/u.test(line)));
}

const SLEEP_STATEMENT = /^await\s+(?:new\s+Promise\b[\s\S]*?|(?:delay|sleep|setTimeout|pause)\s*\()/u;
const ASSERTION_STATEMENT = /^(?:await\s+)?expect(?:\.\w+)?\s*\(/u;

function scan(path: string): Offence[] {
  const source = readFileSync(path, "utf8");
  const masked = maskLiterals(source);
  const site = relative(testRoot, path).split(sep).join("/");
  const offences: Offence[] = [];

  for (const span of loopSpans(masked)) {
    // A collection walk terminates on its collection, so neither a timer nor a
    // clock reading inside it can be a deadline.
    if (iteratesCollection(span)) continue;
    const line = lineOf(masked, span.index);
    if (span.body.includes("setTimeout(")) {
      offences.push({
        site,
        rule: "hand-rolled-deadline-loop",
        detail: `${site}:${String(line)} sleeps inside a ${span.keyword} loop`,
      });
      continue;
    }
    if (clockDecidesTheLoop(span)) {
      const read = CLOCK_READS.find((call) => span.header.includes(call) || span.body.includes(call)) ?? "a clock";
      offences.push({
        site,
        rule: "hand-rolled-deadline-loop",
        detail: `${site}:${String(line)} reads ${read.replace("(", "")} inside a ${span.keyword} loop`,
      });
    }
  }

  const lines = masked.split("\n");
  for (let cursor = 0; cursor < lines.length; cursor += 1) {
    const statement = (lines[cursor] ?? "").trim();
    if (!SLEEP_STATEMENT.test(statement)) continue;
    if (statement.includes("new Promise") && !statement.includes("setTimeout")) continue;
    let lookahead = cursor + 1;
    while (lookahead < lines.length && (lines[lookahead] ?? "").trim().length === 0) lookahead += 1;
    if (!ASSERTION_STATEMENT.test((lines[lookahead] ?? "").trim())) continue;
    offences.push({
      site,
      rule: "sleep-and-assert",
      detail: `${site}:${String(cursor + 1)} sleeps then asserts`,
    });
  }

  return offences;
}

function readGolden(): WaiterGolden {
  return JSON.parse(readFileSync(goldenPath, "utf8")) as WaiterGolden;
}

function scanTestTree(): Offence[] {
  return testFiles(testRoot)
    .flatMap(scan)
    .filter((offence) => offence.site !== waitPort)
    .sort((left, right) => left.detail.localeCompare(right.detail));
}

describe("#558 one waiter for the fabric test suite", () => {
  it("refuses a hand-rolled deadline loop or a bare sleep-and-assert", () => {
    const golden = readGolden();
    const allowed = new Set(golden.temporary_hand_rolled_waits.map((allowance) => allowance.site));
    const offences = scanTestTree().filter((offence) => !allowed.has(offence.detail));
    expect(
      offences.map((offence) => `${offence.rule}: ${offence.detail}`),
      `wait via tests/${waitPort} (waitUntil, eventually, waitForFile, waitForProcessExit) instead of a hand-rolled loop, and use settle(ms, reason) for a deliberate quiescence window`,
    ).toEqual([]);
  });

  it("keeps every allowance named, justified and still true", () => {
    const golden = readGolden();
    expect(golden.schema_version).toBe(1);
    expect(golden.wait_port).toBe(waitPort);
    const detected = new Set(scanTestTree().map((offence) => offence.detail));
    for (const allowance of golden.temporary_hand_rolled_waits) {
      expect(allowance.id).toMatch(/^TEMP-[A-Z0-9-]+$/u);
      expect(allowance.reason.trim().length).toBeGreaterThan(0);
      expect(allowance.reason).not.toMatch(/[\r\n]/u);
      expect(detected, `stale allowance ${allowance.id}: ${allowance.site} no longer offends`)
        .toContain(allowance.site);
    }
    const ids = golden.temporary_hand_rolled_waits.map((allowance) => allowance.id);
    expect(new Set(ids).size).toBe(ids.length);
  });

  it("detects both refused shapes in a fixture that contains them", () => {
    // Guards the guard: a scanner that silently stops matching is worse than none.
    const probe = resolve(testRoot, "fixtures/w558-hand-rolled-wait-probe.ts.txt");
    const offences = scan(probe);
    expect(offences.map((offence) => offence.rule).sort()).toEqual([
      "hand-rolled-deadline-loop",
      "hand-rolled-deadline-loop",
      "sleep-and-assert",
    ]);
  });

  it("does not mistake a template-literal child script or a collection walk for a waiter", () => {
    const probe = resolve(testRoot, "fixtures/w558-permitted-wait-probe.ts.txt");
    expect(scan(probe)).toEqual([]);
  });

  it("names the wait port as the single owner", () => {
    const port = readFileSync(resolve(testRoot, waitPort), "utf8");
    for (const owned of ["export async function waitUntil", "export async function eventually", "export const monotonicWaitClock"]) {
      expect(port).toContain(owned);
    }
    // The port is the only place allowed to schedule a timer for waiting.
    expect(port.match(/setTimeout\(/gu) ?? []).toHaveLength(1);
  });
});
