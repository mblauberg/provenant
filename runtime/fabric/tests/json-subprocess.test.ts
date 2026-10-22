import { describe, expect, it } from "vitest";
import { runJsonSubprocess, SubprocessError } from "../src/json-subprocess.js";

const node = process.execPath;
const base = { timeoutMs: 5_000, maxBytes: 64 * 1024, unavailable: "unavailable" };
const run = (script: string, extra: Record<string, unknown> = {}) =>
  runJsonSubprocess(node, ["-e", script], { input: { ok: true }, ...base, ...extra } as never);
const kindOf = async (promise: Promise<unknown>) => {
  const error = await promise.then(() => undefined, (caught: unknown) => caught);
  expect(error).toBeInstanceOf(SubprocessError);
  return (error as SubprocessError).kind;
};

describe("runJsonSubprocess", () => {
  it("round-trips JSON", async () => {
    const script = "let s='';process.stdin.on('data',d=>s+=d).on('end',()=>console.log(JSON.stringify({got:JSON.parse(s)})))";
    await expect(run(script)).resolves.toEqual({ got: { ok: true } });
  });

  it("survives a child that exits at once while a 4 MiB request is written", async () => {
    const input = { blob: "x".repeat(4 * 1024 * 1024) };
    await expect(kindOf(run("process.exit(3)", { input }))).resolves.toBe("exit");
    // A child that exits cleanly without reading stdin is read for its answer.
    await expect(run("console.log('{\"early\":true}')", { input })).resolves.toEqual({ early: true });
  });

  it("rejects malformed output", async () => {
    await expect(kindOf(run("console.log('not json')"))).resolves.toBe("malformed");
    await expect(kindOf(run(""))).resolves.toBe("malformed");
  });

  it("times out and kills a hung child", async () => {
    const started = Date.now();
    await expect(kindOf(run("setInterval(()=>{},1000)", { timeoutMs: 200 }))).resolves.toBe("timeout");
    expect(Date.now() - started).toBeLessThan(4_000);
  });

  it("stops a child that prints past the cap", async () => {
    const script = "const w=()=>{process.stdout.write('x'.repeat(65536))&&w()};w();setInterval(()=>{},1000)";
    await expect(kindOf(run(script, { maxBytes: 1024 }))).resolves.toBe("overflow");
  });

  it("cancels on abort, before and during the run", async () => {
    const controller = new AbortController();
    const pending = run("setInterval(()=>{},1000)", { signal: controller.signal });
    setTimeout(() => controller.abort(new Error("stop")), 100);
    await expect(pending).rejects.toThrow("stop");
    await expect(run("", { signal: AbortSignal.abort() })).rejects.toMatchObject({ name: "AbortError" });
  });

  it("reports a missing executable", async () => {
    const error = await runJsonSubprocess("/nonexistent/fabric-binary", [], { input: {}, ...base }).catch((e: unknown) => e);
    expect(error).toMatchObject({ kind: "spawn" });
  });
});
