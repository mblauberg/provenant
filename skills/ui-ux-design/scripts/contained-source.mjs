import fs from 'node:fs';
import path from 'node:path';
import { randomUUID } from 'node:crypto';


function sourceError(code, message, cause = undefined) {
  const error = new Error(message, cause ? { cause } : undefined);
  error.code = code;
  return error;
}


function isWithin(rootPath, candidatePath) {
  const relative = path.relative(rootPath, candidatePath);
  return relative === ''
    || (!path.isAbsolute(relative) && relative !== '..' && !relative.startsWith(`..${path.sep}`));
}


function projectRoot(rootDir) {
  const lexical = path.resolve(rootDir);
  const metadata = fs.lstatSync(lexical);
  if (metadata.isSymbolicLink() || !metadata.isDirectory()) {
    throw sourceError('source_root_invalid', 'Project root must be a real directory');
  }
  return fs.realpathSync.native(lexical);
}


function validateRelativeInput(candidatePath) {
  if (typeof candidatePath !== 'string' || !candidatePath || candidatePath.includes('\0')) {
    throw sourceError('source_path_invalid', 'Source path must be a non-empty project-relative path');
  }
  const portable = candidatePath.replaceAll('\\', '/');
  if (path.isAbsolute(candidatePath) || path.posix.isAbsolute(portable) || path.win32.isAbsolute(candidatePath)) {
    throw sourceError('source_path_outside_project', 'Source paths must be project-relative');
  }
  if (portable.split('/').includes('..')) {
    throw sourceError('source_path_outside_project', 'Source path escapes the project root');
  }
}


function inspectContainedSource(rootDir, candidatePath, { relativeOnly = false } = {}) {
  if (relativeOnly) validateRelativeInput(candidatePath);
  const rootReal = projectRoot(rootDir);
  const lexical = path.resolve(rootReal, candidatePath);
  if (!isWithin(rootReal, lexical)) {
    throw sourceError('source_path_outside_project', 'Source path escapes the project root');
  }

  let parentReal;
  let targetReal;
  let metadata;
  try {
    parentReal = fs.realpathSync.native(path.dirname(lexical));
    targetReal = fs.realpathSync.native(lexical);
    metadata = fs.lstatSync(lexical);
  } catch (cause) {
    throw sourceError('source_path_invalid', 'Source path does not name an existing file', cause);
  }
  if (!isWithin(rootReal, parentReal) || !isWithin(rootReal, targetReal)) {
    throw sourceError('source_path_outside_project', 'Source path resolves outside the project root');
  }
  if (targetReal !== lexical || metadata.isSymbolicLink()) {
    throw sourceError('source_path_symlink', 'Source path must not contain or name a symlink');
  }
  if (!metadata.isFile()) {
    throw sourceError('source_path_invalid', 'Source path must name a regular file');
  }
  if (metadata.nlink !== 1) {
    throw sourceError('source_path_hard_link', 'Source file has multiple hard links');
  }
  return { rootReal, path: lexical, parentReal, metadata };
}


function sameIdentity(metadata, snapshot) {
  return metadata.dev === snapshot.dev
    && metadata.ino === snapshot.ino
    && metadata.mode === snapshot.rawMode
    && metadata.nlink === 1
    && metadata.isFile();
}


function revalidateSnapshot(snapshot) {
  const current = inspectContainedSource(snapshot.root, snapshot.path);
  if (current.rootReal !== snapshot.root || current.parentReal !== snapshot.parent) {
    throw sourceError('source_path_changed', 'Source parent changed after validation');
  }
  if (!sameIdentity(current.metadata, snapshot)) {
    throw sourceError('source_path_changed', 'Source file changed after validation');
  }
  return current;
}


export function readContainedSource(rootDir, candidatePath, options = {}) {
  const inspected = inspectContainedSource(rootDir, candidatePath, options);
  const bytes = fs.readFileSync(inspected.path);
  const afterRead = fs.lstatSync(inspected.path);
  const snapshot = {
    root: inspected.rootReal,
    path: inspected.path,
    parent: inspected.parentReal,
    relative: path.relative(inspected.rootReal, inspected.path).split(path.sep).join('/'),
    bytes,
    mode: afterRead.mode & 0o7777,
    rawMode: afterRead.mode,
    dev: afterRead.dev,
    ino: afterRead.ino,
  };
  if (!sameIdentity(afterRead, snapshot)) {
    throw sourceError('source_path_changed', 'Source file changed while it was read');
  }
  return snapshot;
}


function stageReplacement(snapshot, content) {
  revalidateSnapshot(snapshot);
  const temporary = path.join(
    snapshot.parent,
    `.${path.basename(snapshot.path)}.${process.pid}.${randomUUID()}.tmp`,
  );
  let bytes;
  try {
    bytes = Buffer.isBuffer(content) ? content : Buffer.from(String(content), 'utf8');
  } catch (cause) {
    throw sourceError('source_replace_failed', 'Could not encode source replacement', cause);
  }
  let descriptor;
  let failure;
  try {
    descriptor = fs.openSync(temporary, 'wx', snapshot.mode);
    fs.writeFileSync(descriptor, bytes);
    fs.fchmodSync(descriptor, snapshot.mode);
    fs.fsyncSync(descriptor);
  } catch (cause) {
    failure = cause;
  } finally {
    if (descriptor !== undefined) fs.closeSync(descriptor);
  }
  if (failure) {
    cleanupTemporary(temporary);
    throw sourceError('source_replace_failed', 'Could not stage source replacement', failure);
  }
  return temporary;
}


function syncDirectory(directory) {
  let descriptor;
  try {
    descriptor = fs.openSync(directory, 'r');
    fs.fsyncSync(descriptor);
  } catch {
    // Directory fsync is not supported by every platform/filesystem. The file
    // replacement remains atomic; this best-effort step improves crash durability.
  } finally {
    if (descriptor !== undefined) fs.closeSync(descriptor);
  }
}


function replaceOne(snapshot, temporary) {
  revalidateSnapshot(snapshot);
  fs.renameSync(temporary, snapshot.path);
  syncDirectory(snapshot.parent);
  return readContainedSource(snapshot.root, snapshot.path);
}


function cleanupTemporary(temporary) {
  try { fs.unlinkSync(temporary); } catch {}
}


export function replaceContainedSources(replacements, options = {}) {
  if (!Array.isArray(replacements)) {
    throw sourceError('source_replace_invalid', 'Replacements must be an array');
  }
  const paths = replacements.map(({ snapshot }) => snapshot?.path);
  if (paths.some((value) => typeof value !== 'string') || new Set(paths).size !== paths.length) {
    throw sourceError('source_replace_invalid', 'Each replacement must target one unique source file');
  }

  for (const { snapshot } of replacements) revalidateSnapshot(snapshot);
  const staged = [];
  try {
    for (const { snapshot, content } of replacements) {
      staged.push({ snapshot, temporary: stageReplacement(snapshot, content) });
    }
  } catch (error) {
    for (const { temporary } of staged) cleanupTemporary(temporary);
    throw error;
  }
  const applied = [];

  try {
    for (let index = 0; index < staged.length; index += 1) {
      const entry = staged[index];
      options.beforeReplace?.({ index, path: entry.snapshot.path });
      const replacementSnapshot = replaceOne(entry.snapshot, entry.temporary);
      applied.push({ original: entry.snapshot, current: replacementSnapshot });
    }
  } catch (cause) {
    const rollbackErrors = [];
    for (const entry of applied.reverse()) {
      let temporary;
      try {
        temporary = stageReplacement(entry.current, entry.original.bytes);
        replaceOne(entry.current, temporary);
      } catch (rollbackError) {
        rollbackErrors.push({ path: entry.original.path, error: rollbackError.message });
      } finally {
        if (temporary) cleanupTemporary(temporary);
      }
    }
    if (rollbackErrors.length > 0) {
      const error = sourceError('source_rollback_failed', 'Source replacement failed and rollback was incomplete', cause);
      error.rollbackErrors = rollbackErrors;
      throw error;
    }
    throw sourceError('source_replace_failed', 'Source replacement failed; prior changes were rolled back', cause);
  } finally {
    for (const { temporary } of staged) cleanupTemporary(temporary);
  }
}


export function replaceContainedSource(snapshot, content) {
  replaceContainedSources([{ snapshot, content }]);
}


export function resolveContainedSourcePath(rootDir, candidatePath, options = {}) {
  return inspectContainedSource(rootDir, candidatePath, options).path;
}
