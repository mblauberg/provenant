import { execFile } from "node:child_process";
import { chmod, mkdir, mkdtemp, writeFile } from "node:fs/promises";
import { join } from "node:path";
import { promisify } from "node:util";

const execFileAsync = promisify(execFile);

export async function shellCommandArguments(command: string, temporaryRoot: string): Promise<string[]> {
  const home = await mkdtemp(join(temporaryRoot, "command-home-"));
  const script = join(home, ".agents", "scripts", "agent-fabric");
  await mkdir(join(home, ".agents", "scripts"), { recursive: true });
  await writeFile(script, [
    "#!/usr/bin/env node",
    "process.stdout.write(JSON.stringify(process.argv.slice(2)));",
    "",
  ].join("\n"));
  await chmod(script, 0o700);
  const quotedExecutable = /'(?:[^']|'"'"')*\/scripts\/agent-fabric'/u;
  const executable = command.match(quotedExecutable)?.[0];
  const rewritten = executable === undefined
    ? command
    : command.replace(executable, JSON.stringify(script));
  const { stdout } = await execFileAsync("/bin/sh", ["-c", rewritten], {
    env: { ...process.env, HOME: home },
  });
  const parsed: unknown = JSON.parse(stdout);
  if (!Array.isArray(parsed) || parsed.some((value) => typeof value !== "string")) {
    throw new Error("shell command did not emit string arguments");
  }
  return parsed as string[];
}
