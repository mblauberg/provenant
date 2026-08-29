import { createHash } from 'node:crypto';
import fs from 'node:fs';
import path from 'node:path';


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
  const metadata = fs.lstatSync(lexical, { bigint: true });
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
  let parentMetadata;
  let metadata;
  try {
    parentMetadata = fs.lstatSync(path.dirname(lexical), { bigint: true });
    parentReal = fs.realpathSync.native(path.dirname(lexical));
    targetReal = fs.realpathSync.native(lexical);
    metadata = fs.lstatSync(lexical, { bigint: true });
  } catch (cause) {
    throw sourceError('source_path_invalid', 'Source path does not name an existing file', cause);
  }
  if (!parentMetadata.isDirectory() || parentMetadata.isSymbolicLink()) {
    throw sourceError('source_path_symlink', 'Source parent must be a real directory');
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
  if (metadata.nlink !== 1n) {
    throw sourceError('source_path_hard_link', 'Source file has multiple hard links');
  }
  return { rootReal, path: lexical, parentReal, parentMetadata, metadata };
}


function requireNoFollowFlag() {
  const noFollow = fs.constants.O_NOFOLLOW;
  if (!Number.isInteger(noFollow) || noFollow === 0) {
    throw sourceError(
      'source_no_follow_unavailable',
      'This platform cannot open source files without following symbolic links',
    );
  }
  return noFollow;
}


function digestBytes(bytes) {
  return createHash('sha256').update(bytes).digest('hex');
}


function sameStableMetadata(left, right) {
  return left.dev === right.dev
    && left.ino === right.ino
    && left.mode === right.mode
    && left.nlink === right.nlink
    && left.size === right.size
    && left.mtimeNs === right.mtimeNs
    && left.ctimeNs === right.ctimeNs
    && left.uid === right.uid
    && left.gid === right.gid
    && left.isFile()
    && right.isFile();
}


function sameObjectIdentity(metadata, snapshot) {
  return metadata.dev === snapshot.dev
    && metadata.ino === snapshot.ino
    && metadata.mode === snapshot.rawMode
    && metadata.nlink === 1n
    && metadata.uid === snapshot.uid
    && metadata.gid === snapshot.gid
    && metadata.isFile();
}


function readStableDescriptor(descriptor) {
  const before = fs.fstatSync(descriptor, { bigint: true });
  if (!before.isFile() || before.nlink !== 1n || before.size > BigInt(Number.MAX_SAFE_INTEGER)) {
    throw sourceError('source_path_changed', 'Source descriptor no longer names one regular file');
  }
  const bytes = Buffer.alloc(Number(before.size));
  let offset = 0;
  while (offset < bytes.length) {
    const read = fs.readSync(descriptor, bytes, offset, bytes.length - offset, offset);
    if (read === 0) {
      throw sourceError('source_path_changed', 'Source file changed while it was read');
    }
    offset += read;
  }
  const after = fs.fstatSync(descriptor, { bigint: true });
  if (!sameStableMetadata(before, after) || after.size !== BigInt(bytes.length)) {
    throw sourceError('source_path_changed', 'Source file changed while it was read');
  }
  return { before, after, bytes, digest: digestBytes(bytes) };
}


function openNoFollow(sourcePath, accessFlag, failureCode, failureMessage) {
  try {
    return fs.openSync(sourcePath, accessFlag | requireNoFollowFlag());
  } catch (cause) {
    if (cause?.code === 'ELOOP') {
      throw sourceError('source_path_symlink', 'Source path must not name a symlink', cause);
    }
    throw sourceError(failureCode, failureMessage, cause);
  }
}


function validateParentBinding(snapshot) {
  const current = inspectContainedSource(snapshot.root, snapshot.path);
  if (
    current.rootReal !== snapshot.root
    || current.parentReal !== snapshot.parent
    || current.parentMetadata.dev !== snapshot.parentDev
    || current.parentMetadata.ino !== snapshot.parentIno
    || !sameObjectIdentity(current.metadata, snapshot)
  ) {
    throw sourceError('source_path_changed', 'Source path or parent changed after validation');
  }
}


export function readContainedSource(rootDir, candidatePath, options = {}) {
  const inspected = inspectContainedSource(rootDir, candidatePath, options);
  const descriptor = openNoFollow(
    inspected.path,
    fs.constants.O_RDONLY,
    'source_path_invalid',
    'Could not open source file for reading',
  );
  try {
    const read = readStableDescriptor(descriptor);
    if (!sameStableMetadata(inspected.metadata, read.before)) {
      throw sourceError('source_path_changed', 'Source file changed before it was read');
    }
    validateParentBinding({
      root: inspected.rootReal,
      path: inspected.path,
      parent: inspected.parentReal,
      parentDev: inspected.parentMetadata.dev,
      parentIno: inspected.parentMetadata.ino,
      rawMode: read.after.mode,
      dev: read.after.dev,
      ino: read.after.ino,
      uid: read.after.uid,
      gid: read.after.gid,
    });
    return {
      root: inspected.rootReal,
      path: inspected.path,
      parent: inspected.parentReal,
      parentDev: inspected.parentMetadata.dev,
      parentIno: inspected.parentMetadata.ino,
      relative: path.relative(inspected.rootReal, inspected.path).split(path.sep).join('/'),
      bytes: read.bytes,
      digest: read.digest,
      mode: Number(read.after.mode & 0o7777n),
      rawMode: read.after.mode,
      dev: read.after.dev,
      ino: read.after.ino,
      nlink: read.after.nlink,
      size: read.after.size,
      mtimeNs: read.after.mtimeNs,
      ctimeNs: read.after.ctimeNs,
      uid: read.after.uid,
      gid: read.after.gid,
    };
  } finally {
    fs.closeSync(descriptor);
  }
}


function encodeReplacement(content) {
  try {
    return Buffer.isBuffer(content) ? Buffer.from(content) : Buffer.from(String(content), 'utf8');
  } catch (cause) {
    throw sourceError('source_replace_failed', 'Could not encode source replacement', cause);
  }
}


function snapshotStillMatches(metadata, snapshot) {
  return sameObjectIdentity(metadata, snapshot)
    && metadata.size === snapshot.size
    && metadata.mtimeNs === snapshot.mtimeNs
    && metadata.ctimeNs === snapshot.ctimeNs;
}


function openReplacement(snapshot, bytes) {
  if ((snapshot.rawMode & 0o222n) === 0n) {
    throw sourceError('source_not_writable', 'Source file has no write permission bits');
  }
  const descriptor = openNoFollow(
    snapshot.path,
    fs.constants.O_RDWR,
    'source_replace_failed',
    'Could not open source file for replacement',
  );
  try {
    const read = readStableDescriptor(descriptor);
    if (!snapshotStillMatches(read.before, snapshot)) {
      throw sourceError('source_path_changed', 'Source file changed after validation');
    }
    if (read.digest !== snapshot.digest || !read.bytes.equals(snapshot.bytes)) {
      throw sourceError('source_path_changed', 'Source contents changed after validation');
    }
    validateParentBinding(snapshot);
    return { snapshot, descriptor, original: read.bytes, replacement: bytes };
  } catch (error) {
    fs.closeSync(descriptor);
    throw error;
  }
}


function writeDescriptor(entry, bytes) {
  fs.ftruncateSync(entry.descriptor, 0);
  let offset = 0;
  while (offset < bytes.length) {
    const written = fs.writeSync(entry.descriptor, bytes, offset, bytes.length - offset, offset);
    if (written === 0) throw sourceError('source_replace_failed', 'Source write made no progress');
    offset += written;
  }
  fs.fsyncSync(entry.descriptor);
  const verified = readStableDescriptor(entry.descriptor);
  if (
    !sameObjectIdentity(verified.after, entry.snapshot)
    || verified.digest !== digestBytes(bytes)
    || !verified.bytes.equals(bytes)
  ) {
    throw sourceError('source_replace_failed', 'Source replacement verification failed');
  }
}


export function replaceContainedSources(replacements, options = {}) {
  if (!Array.isArray(replacements)) {
    throw sourceError('source_replace_invalid', 'Replacements must be an array');
  }
  const paths = replacements.map(({ snapshot }) => snapshot?.path);
  if (paths.some((value) => typeof value !== 'string') || new Set(paths).size !== paths.length) {
    throw sourceError('source_replace_invalid', 'Each replacement must target one unique source file');
  }

  const encoded = replacements.map(({ snapshot, content }) => ({
    snapshot,
    bytes: encodeReplacement(content),
  }));
  const opened = [];
  try {
    for (const { snapshot, bytes } of encoded) {
      opened.push(openReplacement(snapshot, bytes));
    }
  } catch (error) {
    for (const entry of opened) fs.closeSync(entry.descriptor);
    throw error;
  }

  const applied = [];
  try {
    options.afterOpen?.({ paths: opened.map((entry) => entry.snapshot.path) });
    for (let index = 0; index < opened.length; index += 1) {
      const entry = opened[index];
      options.beforeReplace?.({ index, path: entry.snapshot.path });
      applied.push(entry);
      writeDescriptor(entry, entry.replacement);
      options.afterWrite?.({ index, path: entry.snapshot.path });
      validateParentBinding(entry.snapshot);
    }
  } catch (cause) {
    const rollbackErrors = [];
    for (const entry of applied.reverse()) {
      try {
        writeDescriptor(entry, entry.original);
      } catch (rollbackError) {
        rollbackErrors.push({ path: entry.snapshot.path, error: rollbackError.message });
      }
    }
    if (rollbackErrors.length > 0) {
      const error = sourceError(
        'source_rollback_failed',
        'Source replacement failed and rollback was incomplete',
        cause,
      );
      error.rollbackErrors = rollbackErrors;
      throw error;
    }
    throw sourceError('source_replace_failed', 'Source replacement failed; changes were rolled back', cause);
  } finally {
    for (const entry of opened) fs.closeSync(entry.descriptor);
  }
}


export function replaceContainedSource(snapshot, content) {
  replaceContainedSources([{ snapshot, content }]);
}


export function resolveContainedSourcePath(rootDir, candidatePath, options = {}) {
  return inspectContainedSource(rootDir, candidatePath, options).path;
}
