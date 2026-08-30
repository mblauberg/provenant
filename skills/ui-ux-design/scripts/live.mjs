// Modified for Provenant.
/**
 * CLI entry point: prepare everything needed to enter the live variant poll loop.
 *
 * Does (all in one command):
 *   1. Check .impeccable/live/config.json (returns config_missing if first-ever run)
 *   2. Start the live server in the background (or reuse a running one)
 *   3. Inject the browser script tag into the project's entry file
 *   4. Summarize PRODUCT.md / DESIGN.md as bounded project metadata
 *   5. Print a single JSON blob with everything the agent needs
 *
 * After this, the agent's only remaining steps are:
 *   - Open the project's live dev/preview URL in the browser (optional, if browser automation exists)—not `serverPort`; that port is the Impeccable helper for /live.js and /poll
 *   - Enter the poll loop: `node live-poll.mjs`
 *
 * Usage:
 *   node live.mjs # Prepare within the active implementation lifecycle
 *   node live.mjs --help
 */

import { execFileSync } from 'node:child_process';
import fs from 'node:fs';
import path from 'node:path';
import { fileURLToPath } from 'node:url';
import { loadContext, summarizeContext } from './load-context.mjs';
import { resolveFiles } from './live-inject.mjs';
import { ensureCanonicalLiveStateRoot, readLiveServerInfo } from './impeccable-paths.mjs';

const __dirname = path.dirname(fileURLToPath(import.meta.url));

async function liveCli() {
  const args = process.argv.slice(2);

  if (args.includes('--help') || args.includes('-h')) {
    console.log(`Usage: node live.mjs

Prepare everything for live variant mode within the active implementation lifecycle:
  - Checks .impeccable/live/config.json (required, created once per project)
  - Starts (or reuses) the live server in the background
  - Injects the browser script tag
  - Summarizes PRODUCT.md / DESIGN.md as bounded project metadata

On success, prints a JSON blob with non-secret server and bounded project metadata.
The bearer token remains only in private .impeccable/live/server.json state.

On config_missing, prints:
  { ok: false, error: "config_missing", path }

The agent should then:
  1. If config_missing, create the config within the authorised project paths and re-run
  2. Optionally open the project's dev/preview URL in the browser (see references/live.md—not serverPort)
  3. Enter the poll loop: node live-poll.mjs`);
    process.exit(0);
  }

  try {
    ensureCanonicalLiveStateRoot(process.cwd());
  } catch (error) {
    console.log(JSON.stringify({
      ok: false,
      error: error.code || 'live_state_root_invalid',
      message: error.message,
    }));
    process.exit(1);
  }

  // 1. Check config (fail fast if missing — no point starting anything else)
  const checkOut = runScript('live-inject.mjs', ['--check']);
  const checkResult = safeParse(checkOut);
  if (!checkResult || !checkResult.ok) {
    console.log(JSON.stringify(checkResult || { ok: false, error: 'check_failed', raw: checkOut }));
    process.exit(0);
  }

  // 2. Start server (or reuse existing)
  const serverInfo = await ensureServerRunning();
  if (!serverInfo || serverInfo.failure) {
    const failure = serverInfo?.failure;
    const reason = failure?.status === 'timeout'
      ? 'server_start_timeout'
      : failure?.status === 'refused'
        ? 'server_start_refused'
        : null;
    console.log(JSON.stringify({
      ok: false,
      error: 'server_start_failed',
      ...(failure ? { diagnostic: failure } : {}),
      ...(reason ? { reason } : {}),
    }));
    process.exit(1);
  }

  // 3. Inject the script tag at the current port
  const injectExecution = runScriptResult('live-inject.mjs', [
    '--port', String(serverInfo.port),
    '--token', String(serverInfo.token || ''),
  ]);
  const injectResult = safeParse(injectExecution.stdout) || safeParse(injectExecution.stderr);
  if (!injectResult || !injectResult.ok) {
    const serverCleanup = serverInfo.startedByThisInvocation
      ? await stopOwnedServer(serverInfo)
      : 'not-owned';
    console.log(JSON.stringify({
      ok: false,
      error: 'inject_failed',
      detail: injectResult || 'Live injection failed without a structured result',
      serverPort: serverInfo.port,
      serverCleanup,
    }));
    process.exit(1);
  }

  // 4. Load bounded PRODUCT.md + DESIGN.md metadata without mutating legacy files.
  const ctx = summarizeContext(loadContext(process.cwd()));

  // 5. Compute drift-heal: compare resolved inject targets against the
  //    project's HTML files. Orphans are HTML files not covered by config.
  //    Warning only — the agent decides whether to act.
  const resolvedFiles = resolveFiles(process.cwd(), checkResult.config);
  const drift = scanForDrift(process.cwd(), resolvedFiles, checkResult.config);

  // 6. Emit everything the agent needs
  console.log(JSON.stringify({
    ok: true,
    serverPort: serverInfo.port,
    pageFiles: resolvedFiles,
    configDrift: drift,
    ...ctx,
  }, null, 2));
}

/**
 * Drift-heal scan. Walks the project for HTML files under common
 * page-source directories (public/, src/, app/, pages/) and reports any
 * that aren't covered by the resolved inject targets. This is purely
 * advisory — the agent can ignore it, or suggest the user add the
 * orphans to config.files.
 *
 * Skipped if config.files already contains at least one glob pattern
 * covering everything in practice (signaled by the orphan count being 0).
 */
function scanForDrift(rootDir, resolvedFiles, config) {
  const SCAN_ROOTS = ['public', 'src', 'app', 'pages'];
  const IGNORE_DIRS = new Set([
    'node_modules', '.git', '.next', '.nuxt', '.svelte-kit', '.astro',
    '.turbo', '.vercel', '.cache', 'coverage', 'dist', 'build',
  ]);

  const resolvedSet = new Set(resolvedFiles.map((f) => f.split(path.sep).join('/')));

  // Files matching the user's `exclude` globs are intentional omissions,
  // not drift. Compile them to regexes so the orphan list stays signal.
  const userExcludeRegexes = (Array.isArray(config.exclude) ? config.exclude : [])
    .map((p) => globToRegex(p));
  const isUserExcluded = (rel) => userExcludeRegexes.some((re) => re.test(rel));

  const orphans = [];

  const walk = (dir, relBase) => {
    let entries;
    try { entries = fs.readdirSync(dir, { withFileTypes: true }); }
    catch { return; }
    for (const e of entries) {
      const rel = relBase ? `${relBase}/${e.name}` : e.name;
      if (e.isDirectory()) {
        if (IGNORE_DIRS.has(e.name) || e.name.startsWith('.')) continue;
        walk(path.join(dir, e.name), rel);
      } else if (e.isFile() && e.name.endsWith('.html')) {
        if (resolvedSet.has(rel)) continue;
        if (isUserExcluded(rel)) continue;
        orphans.push(rel);
      }
    }
  };

  for (const root of SCAN_ROOTS) {
    const abs = path.join(rootDir, root);
    if (fs.existsSync(abs) && fs.statSync(abs).isDirectory()) {
      walk(abs, root);
    }
  }

  if (orphans.length === 0) return null;
  const capped = orphans.slice(0, 20);
  return {
    orphans: capped,
    orphanCount: orphans.length,
    hint: `${orphans.length} HTML file(s) exist but aren't in config.files. Consider adding them, or use a glob pattern like "public/**/*.html".`,
  };
}

/**
 * Same glob-to-regex mapping used by live-inject.mjs. Kept inline here
 * to avoid a circular import (live-inject.mjs already imports nothing
 * from live.mjs). The two must stay in sync.
 */
function globToRegex(pattern) {
  let re = '';
  let i = 0;
  while (i < pattern.length) {
    const c = pattern[i];
    if (c === '*') {
      if (pattern[i + 1] === '*') {
        if (pattern[i + 2] === '/') { re += '(?:.*/)?'; i += 3; }
        else { re += '.*'; i += 2; }
      } else {
        re += '[^/]*';
        i += 1;
      }
    } else if (c === '?') {
      re += '[^/]';
      i += 1;
    } else if (/[.+^${}()|[\]\\]/.test(c)) {
      re += '\\' + c;
      i += 1;
    } else {
      re += c;
      i += 1;
    }
  }
  return new RegExp('^' + re + '$');
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

function runScript(name, args) {
  return runScriptResult(name, args).stdout;
}

function runScriptResult(name, args) {
  const scriptPath = path.join(__dirname, name);
  try {
    return {
      stdout: execFileSync(process.execPath, [scriptPath, ...args], {
        encoding: 'utf-8',
        cwd: process.cwd(),
        timeout: 15_000,
      }),
      stderr: '',
    };
  } catch (err) {
    return {
      // execFileSync throws on non-zero exit; return stdout and stderr so the
      // live-server launcher can retain its structured startup diagnosis.
      stdout: err.stdout || err.message || '',
      stderr: err.stderr || '',
    };
  }
}

function safeParse(out) {
  try { return JSON.parse(String(out).trim()); } catch { return null; }
}

/**
 * Return { pid, port, token } for the running live server, starting one if needed.
 */
async function probeServerInfo(info, timeoutMs = 1_000) {
  if (!info
      || !Number.isInteger(info.pid)
      || !Number.isInteger(info.port)
      || info.port < 1
      || info.port > 65535
      || typeof info.token !== 'string'
      || !info.token) return false;
  try {
    const response = await fetch(
      `http://127.0.0.1:${info.port}/status?token=${encodeURIComponent(info.token)}`,
      { signal: AbortSignal.timeout(timeoutMs) },
    );
    if (!response.ok) return false;
    const status = await response.json();
    return status?.status === 'ok'
      && status.pid === info.pid
      && status.port === info.port;
  } catch {
    return false;
  }
}

async function ensureServerRunning() {
  // Try to reuse an existing server
  try {
    const existing = readLiveServerInfo(process.cwd())?.info;
    if (existing && existing.pid) {
      try {
        process.kill(existing.pid, 0); // throws if dead
        if (await probeServerInfo(existing)) {
          return { ...existing, startedByThisInvocation: false };
        }
        return {
          failure: {
            status: 'refused',
            classification: 'unrelated_pid_record',
            pid: existing.pid,
            port: existing.port,
          },
        };
      } catch { /* stale PID file — the server script will clean it up */ }
    }
  } catch { /* no PID file */ }

  // Start a new server
  const result = runScriptResult('live-server.mjs', ['--background']);
  const publicInfo = safeParse(result.stdout);
  if (publicInfo?.pid && publicInfo?.port) {
    const privateInfo = readLiveServerInfo(process.cwd())?.info;
    if (privateInfo?.pid === publicInfo.pid
        && privateInfo?.port === publicInfo.port
        && await probeServerInfo(privateInfo)) {
      return { ...privateInfo, startedByThisInvocation: true };
    }
  }
  const failure = safeParse(result.stderr);
  if (failure) return { failure };
  return null;
}


async function stopOwnedServer(serverInfo) {
  const current = readLiveServerInfo(process.cwd())?.info;
  if (!current || current.pid !== serverInfo.pid || current.token !== serverInfo.token) {
    return 'ownership-lost';
  }
  try {
    const response = await fetch(
      `http://127.0.0.1:${current.port}/stop?token=${encodeURIComponent(current.token)}`,
    );
    if (!response.ok) return 'failed';
  } catch {
    return 'failed';
  }

  const deadline = Date.now() + 2_000;
  while (Date.now() < deadline) {
    const stillCurrent = readLiveServerInfo(process.cwd())?.info;
    if (!stillCurrent || stillCurrent.pid !== serverInfo.pid || stillCurrent.token !== serverInfo.token) {
      return 'stopped';
    }
    await new Promise((resolve) => setTimeout(resolve, 25));
  }
  return 'failed';
}

// ---------------------------------------------------------------------------
// Auto-execute
// ---------------------------------------------------------------------------

const _running = process.argv[1];
if (_running?.endsWith('live.mjs') || _running?.endsWith('live.mjs/')) {
  liveCli();
}
