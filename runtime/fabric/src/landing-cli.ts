/**
 * Landing-lease verbs for repository scripts, so a landing tool can run a
 * command instead of holding an MCP connection. They share the store with
 * fabric_landing_lease.
 *
 * Exit codes: 0 ok (including an idempotent release that found nothing to
 * release), 1 refusal or stale lease, 2 usage, 3 held by another holder or a
 * push in progress. `--json` prints one object on stdout for every outcome
 * except usage and store errors, which go to stderr only.
 */
import { execFileSync } from "node:child_process";
import { CommitShaError, identify, resolveCommitSha, withoutGitRedirects, type Identity } from "./identity.js";
import { LandingHeldError, STALE_LANDING, type LandingLease, type Store } from "./store.js";

export class UsageError extends Error {}

export interface LandingOutcome {
  code: 0 | 1 | 3;
  json: boolean;
  result: Record<string, unknown>;
  line: string;
}

export const LANDING_VERBS = ["landing-acquire", "landing-verify", "landing-release", "landing-status"] as const;

const USAGES: Record<(typeof LANDING_VERBS)[number], { flags: string[]; positionals: number; usage: string }> = {
  "landing-acquire": { flags: ["expected-sha", "branch", "remote", "seconds", "label"], positionals: 1,
    usage: "fabric landing-acquire <session> (--expected-sha SHA | --branch B [--remote R]) [--seconds N] [--label L] [--json]" },
  "landing-verify": { flags: ["expected-sha", "branch", "remote", "label"], positionals: 2,
    usage: "fabric landing-verify <session> <generation> [--expected-sha SHA] [--branch B [--remote R]] [--label L] [--json]" },
  "landing-release": { flags: ["label"], positionals: 2,
    usage: "fabric landing-release <session> <generation> [--label L] [--json]" },
  "landing-status": { flags: [], positionals: 0, usage: "fabric landing-status [--json]" },
};

export const validBranch = (branch: string): boolean =>
  /^[A-Za-z0-9][A-Za-z0-9._/-]*$/u.test(branch) && !branch.includes("..") && !branch.endsWith("/");

export const validRemote = (remote: string): boolean => /^[A-Za-z0-9][A-Za-z0-9._-]*$/u.test(remote);

const iso = (ms: number): string => new Date(ms).toISOString();

function parse(verb: (typeof LANDING_VERBS)[number], args: string[]) {
  const { flags, positionals: count, usage } = USAGES[verb];
  const values = new Map<string, string>();
  const positionals: string[] = [];
  let json = false;
  for (let index = 0; index < args.length; index += 1) {
    const argument = args[index]!;
    if (argument === "--json") {
      if (json) throw new UsageError(`usage: ${usage}`);
      json = true;
    } else if (argument.startsWith("--")) {
      const name = argument.slice(2), value = args[++index];
      if (!flags.includes(name) || values.has(name) || value === undefined || value.startsWith("--"))
        throw new UsageError(`usage: ${usage}`);
      values.set(name, value);
    } else positionals.push(argument);
  }
  if (positionals.length !== count) throw new UsageError(`usage: ${usage}`);
  const [session, generationText] = positionals;
  if (session !== undefined && (!session.trim() || session.length > 128 || session.includes("/")))
    throw new UsageError("session must be 1 to 128 characters without /");
  if (generationText !== undefined && !/^[1-9]\d{0,14}$/u.test(generationText))
    throw new UsageError("generation must be a positive integer");
  const branch = values.get("branch"), remote = values.get("remote");
  if (branch !== undefined && !validBranch(branch)) throw new UsageError(`invalid branch name: ${branch}`);
  if (remote !== undefined && (branch === undefined || !validRemote(remote)))
    throw new UsageError(`--remote needs a valid remote name and --branch; usage: ${usage}`);
  return { values, json, session: session ?? "", generation: Number(generationText ?? 0) };
}

/** The remote branch tip from `git ls-remote`, never a local tracking ref. */
function remoteSha(who: Identity, remote: string, branch: string): string {
  const sha = execFileSync("git", ["ls-remote", "--heads", remote, `refs/heads/${branch}`],
    { cwd: who.cwd, env: withoutGitRedirects(process.env), encoding: "utf8", stdio: ["ignore", "pipe", "pipe"] })
    .trim().split(/\s+/u)[0] ?? "";
  if (!/^[0-9a-f]{40}$/u.test(sha)) throw new Error(`remote ${remote} has no branch ${branch}`);
  return sha;
}

function commitSha(who: Identity, input: string): string {
  try {
    return resolveCommitSha(who.cwd, input);
  } catch (error) {
    if (error instanceof CommitShaError) throw new UsageError(error.message);
    throw error;
  }
}

function leaseView(lease: LandingLease) {
  return { holder: lease.holder, generation: lease.generation, expected_sha: lease.expectedSha,
    expires_at: iso(lease.expiresAtMs), ...(lease.pushingUntilMs === undefined ? {} : { pushing_until: iso(lease.pushingUntilMs) }) };
}

export function isLandingVerb(command: string): command is (typeof LANDING_VERBS)[number] {
  return (LANDING_VERBS as readonly string[]).includes(command);
}

/** Parse first, so a usage error never opens the store. */
export function landingCommand(verb: (typeof LANDING_VERBS)[number], args: string[], env: NodeJS.ProcessEnv,
  openStore: () => Store, cwd?: string): LandingOutcome {
  const { values, json, session, generation } = parse(verb, args);
  const label = values.get("label");
  const who = identify(label === undefined ? env : { ...env, AGENT_FABRIC_LABEL: label }, cwd);
  const remote = values.get("remote") ?? "origin", branch = values.get("branch");
  const done = (code: LandingOutcome["code"], result: Record<string, unknown>, line: string): LandingOutcome =>
    ({ code, json, result, line });

  if (verb === "landing-acquire") {
    const given = values.get("expected-sha");
    if ((given === undefined) === (branch === undefined))
      throw new UsageError(`pass exactly one of --expected-sha or --branch; usage: ${USAGES[verb].usage}`);
    const secondsText = values.get("seconds") ?? "300";
    const seconds = Number(secondsText);
    if (!/^\d+$/u.test(secondsText) || seconds < 1 || seconds > 3600)
      throw new UsageError("--seconds must be an integer from 1 to 3600");
    const sha = given === undefined ? remoteSha(who, remote, branch!) : commitSha(who, given);
    const store = openStore();
    try {
      const lease = store.acquireLanding(who, session, sha, seconds);
      const takeover = lease.takenOverFrom === undefined ? {} : { taken_over_from: lease.takenOverFrom };
      return done(0, { acquired: true, ...leaseView(lease), label: who.agentId, session, ...takeover },
        `acquired ${lease.holder} generation ${lease.generation} expected ${lease.expectedSha} until ${iso(lease.expiresAtMs)}` +
        (lease.takenOverFrom === undefined ? "" : ` (took over from ${lease.takenOverFrom})`));
    } catch (error) {
      if (!(error instanceof LandingHeldError)) throw error;
      return done(3, { acquired: false, reason: "held", holder: error.holder, until: error.until }, error.message);
    } finally {
      store.close();
    }
  }

  if (verb === "landing-verify") {
    const given = values.get("expected-sha");
    const sha = given === undefined ? undefined : commitSha(who, given);
    const store = openStore();
    let lease: LandingLease | undefined;
    try {
      const live = store.landingLease(who.project);
      if (live) lease = store.verifyLanding(who, session, generation, live.expectedSha);
    } catch (error) {
      if (!(error instanceof Error && error.message === STALE_LANDING)) throw error;
    } finally {
      store.close();
    }
    if (lease === undefined)
      return done(1, { valid: false, reason: "stale" }, `invalid: stale (${who.agentId}/${session} generation ${generation})`);
    if (sha !== undefined && sha !== lease.expectedSha)
      return done(1, { valid: false, reason: "sha_mismatch", expected_sha: lease.expectedSha, sha },
        `invalid: sha_mismatch (lease expects ${lease.expectedSha}, given ${sha})`);
    const current = branch === undefined ? undefined : remoteSha(who, remote, branch);
    if (current !== undefined && current !== lease.expectedSha)
      return done(1, { valid: false, reason: "sha_moved", expected_sha: lease.expectedSha, remote_sha: current },
        `invalid: sha_moved (${remote}/${branch} is ${current}, lease expects ${lease.expectedSha})`);
    return done(0, { valid: true, ...leaseView(lease) },
      `valid ${lease.holder} generation ${lease.generation} expected ${lease.expectedSha} until ${iso(lease.expiresAtMs)}`);
  }

  const store = openStore();
  try {
    if (verb === "landing-release") {
      const outcome = store.tryReleaseLanding(who, session, generation);
      if (outcome.released) return done(0, { released: true, generation }, `released generation ${generation}`);
      return done(outcome.reason === "pushing" ? 3 : 0, { released: false, reason: outcome.reason }, `not released: ${outcome.reason}`);
    }
    const lease = store.landingLease(who.project);
    if (lease === null) return done(0, { lease: null }, "no landing lease");
    const view = leaseView(lease);
    return done(0, { lease: view }, `${view.holder} generation ${view.generation} expected ${view.expected_sha} ` +
      `until ${view.expires_at}${view.pushing_until === undefined ? "" : ` pushing until ${view.pushing_until}`}`);
  } finally {
    store.close();
  }
}
