// Modified from Impeccable for this harness; see the repository THIRD_PARTY_NOTICES.md.
import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import { randomUUID } from 'node:crypto';

export const IMPECCABLE_DIR = '.impeccable';
export const LIVE_DIR = 'live';

export function getImpeccableDir(cwd = process.cwd()) {
  return path.join(cwd, IMPECCABLE_DIR);
}

export function getDesignSidecarPath(cwd = process.cwd()) {
  return path.join(getImpeccableDir(cwd), 'design.json');
}

export function getDesignSidecarCandidates(cwd = process.cwd(), contextDir = cwd) {
  const candidates = [
    getDesignSidecarPath(cwd),
    path.join(cwd, 'DESIGN.json'),
  ];
  const contextLegacy = path.join(contextDir, 'DESIGN.json');
  if (!candidates.includes(contextLegacy)) candidates.push(contextLegacy);
  return candidates;
}

export function resolveDesignSidecarPath(cwd = process.cwd(), contextDir = cwd) {
  return firstExisting(getDesignSidecarCandidates(cwd, contextDir));
}

export function getLiveDir(cwd = process.cwd()) {
  return path.join(getImpeccableDir(cwd), LIVE_DIR);
}

export function ensureCanonicalLiveStateRoot(cwd = process.cwd()) {
  const root = path.resolve(cwd);
  const impeccableDir = getImpeccableDir(root);
  const liveDir = getLiveDir(root);
  try {
    for (const directory of [impeccableDir, liveDir]) {
      if (!fs.existsSync(directory)) fs.mkdirSync(directory);
      const metadata = fs.lstatSync(directory);
      if (metadata.isSymbolicLink() || !metadata.isDirectory()) {
        throw new Error('state path is not a real directory');
      }
      if (fs.realpathSync.native(directory) !== path.resolve(directory)) {
        throw new Error('state path is non-canonical');
      }
    }
  } catch (cause) {
    const error = new Error(
      'Live state root must be a canonical non-symlinked directory',
      { cause },
    );
    error.code = 'live_state_root_invalid';
    throw error;
  }
  return liveDir;
}

export function getLiveConfigPath(cwd = process.cwd()) {
  return path.join(getLiveDir(cwd), 'config.json');
}

export function getLegacyLiveConfigPath(scriptsDir) {
  return path.join(scriptsDir, 'config.json');
}

export function resolveLiveConfigPath({ cwd = process.cwd(), scriptsDir, env = process.env } = {}) {
  if (env.IMPECCABLE_LIVE_CONFIG && env.IMPECCABLE_LIVE_CONFIG.trim()) {
    const configured = env.IMPECCABLE_LIVE_CONFIG.trim();
    return path.isAbsolute(configured) ? configured : path.resolve(cwd, configured);
  }
  const primary = getLiveConfigPath(cwd);
  if (fs.existsSync(primary)) return primary;
  if (scriptsDir) {
    const legacy = getLegacyLiveConfigPath(scriptsDir);
    if (fs.existsSync(legacy)) return legacy;
  }
  return primary;
}

export function getLiveServerPath(cwd = process.cwd()) {
  return path.join(getLiveDir(cwd), 'server.json');
}

export function getLegacyLiveServerPath(cwd = process.cwd()) {
  return path.join(cwd, '.impeccable-live.json');
}

export function readLiveServerInfo(cwd = process.cwd()) {
  for (const filePath of [getLiveServerPath(cwd), getLegacyLiveServerPath(cwd)]) {
    try {
      return { info: JSON.parse(fs.readFileSync(filePath, 'utf-8')), path: filePath };
    } catch {
      /* try next */
    }
  }
  return null;
}

export function writeLiveServerInfo(cwd = process.cwd(), info) {
  const filePath = getLiveServerPath(cwd);
  const directory = path.dirname(filePath);
  const payload = JSON.stringify(info);
  const temporary = path.join(
    directory,
    `.${path.basename(filePath)}.${process.pid}.${randomUUID()}.tmp`,
  );
  fs.mkdirSync(directory, { recursive: true });
  try {
    fs.writeFileSync(temporary, payload, { encoding: 'utf8', flag: 'wx', mode: 0o600 });
    fs.chmodSync(temporary, 0o600);
    fs.renameSync(temporary, filePath);
  } finally {
    try { fs.unlinkSync(temporary); } catch {}
  }
  return filePath;
}

export function removeLiveServerInfo(cwd = process.cwd()) {
  for (const filePath of [getLiveServerPath(cwd), getLegacyLiveServerPath(cwd)]) {
    try { fs.unlinkSync(filePath); } catch {}
  }
}

export function writeLiveAgentServerInfo(directory, info) {
  const metadata = fs.lstatSync(directory);
  if (metadata.isSymbolicLink() || !metadata.isDirectory()) {
    throw new Error('Agent state directory must be a real directory');
  }
  const filePath = path.join(directory, 'agent.json');
  fs.writeFileSync(filePath, JSON.stringify(info), {
    encoding: 'utf8',
    flag: 'wx',
    mode: 0o600,
  });
  fs.chmodSync(filePath, 0o600);
  return filePath;
}

export function readLiveAgentServerInfo(serverInfo) {
  const filePath = serverInfo?.agentStatePath;
  if (!path.isAbsolute(filePath || '') || path.basename(filePath) !== 'agent.json') {
    throw new Error('Live agent state path is invalid');
  }
  const parent = path.dirname(filePath);
  const parentPath = path.resolve(parent);
  const tempPath = path.resolve(os.tmpdir());
  const lexicalRelative = path.relative(tempPath, parentPath);
  if (!path.basename(parentPath).startsWith('impeccable-live-')
    || lexicalRelative === '..'
    || lexicalRelative.startsWith(`..${path.sep}`)) {
    throw new Error('Live agent state directory is unsafe');
  }
  let parentMetadata;
  let parentReal;
  try {
    parentMetadata = fs.lstatSync(parent);
    parentReal = fs.realpathSync.native(parent);
  } catch (cause) {
    if (cause?.code !== 'ENOENT') throw cause;
    const error = new Error('Live agent state is missing', { cause });
    error.code = 'live_agent_state_missing';
    throw error;
  }
  const tempReal = fs.realpathSync.native(os.tmpdir());
  const relative = path.relative(tempReal, parentReal);
  if (parentMetadata.isSymbolicLink()
    || !parentMetadata.isDirectory()
    || !path.basename(parentReal).startsWith('impeccable-live-')
    || relative === '..'
    || relative.startsWith(`..${path.sep}`)
    || (typeof process.getuid === 'function' && parentMetadata.uid !== process.getuid())
    || (parentMetadata.mode & 0o077) !== 0) {
    throw new Error('Live agent state directory is unsafe');
  }
  if (!Number.isInteger(fs.constants.O_NOFOLLOW)) {
    throw new Error('Live agent state requires O_NOFOLLOW support');
  }
  let descriptor;
  try {
    descriptor = fs.openSync(filePath, fs.constants.O_RDONLY | fs.constants.O_NOFOLLOW);
  } catch (cause) {
    if (cause?.code !== 'ENOENT') throw cause;
    const error = new Error('Live agent state is missing', { cause });
    error.code = 'live_agent_state_missing';
    throw error;
  }
  try {
    const metadata = fs.fstatSync(descriptor);
    if (!metadata.isFile()
      || metadata.nlink !== 1
      || (typeof process.getuid === 'function' && metadata.uid !== process.getuid())
      || (metadata.mode & 0o077) !== 0) {
      throw new Error('Live agent state file is unsafe');
    }
    const info = JSON.parse(fs.readFileSync(descriptor, 'utf8'));
    if (info?.pid !== serverInfo.pid
      || info?.port !== serverInfo.port
      || typeof info?.agentToken !== 'string'
      || !info.agentToken) {
      const error = new Error('Live agent state does not match the server');
      error.code = 'live_agent_state_stale';
      throw error;
    }
    return info;
  } finally {
    fs.closeSync(descriptor);
  }
}

export function getLiveSessionsDir(cwd = process.cwd()) {
  return path.join(getLiveDir(cwd), 'sessions');
}

export function getLegacyLiveSessionsDir(cwd = process.cwd()) {
  return path.join(cwd, '.impeccable-live', 'sessions');
}

export function getLiveAnnotationsDir(cwd = process.cwd()) {
  return path.join(getLiveDir(cwd), 'annotations');
}

export function getLegacyLiveAnnotationsDir(cwd = process.cwd()) {
  return path.join(cwd, '.impeccable-live', 'annotations');
}

function firstExisting(paths) {
  return paths.find((filePath) => fs.existsSync(filePath)) || null;
}
