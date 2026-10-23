"""Confined agy home staging and PowerShell shim helpers."""

from collections import namedtuple
import fcntl
import os
from pathlib import Path
import stat
import sys
import time
import uuid

try:
    from . import provider_exec
except ImportError:
    import provider_exec

# agy rewrites its bin/agentapi shim before running a command and puts bin/ on that command's
# PATH, so a later unconfined agy session or sidecar would run whatever a lane left there (#1055).
# A confined lane gets a HOME of links to the real entries, with its own antigravity-cli/bin; the
# sandbox still judges every access through a link at the real path, and the live bin/ stays
# read-only. agy signs in from the login Keychain, so its token files are neither linked nor
# copied back: a refresh renamed over a link would land in the overlay.
AGY_PRIVATE_BIN = Path(".gemini/antigravity-cli/bin")
AGY_UNLINKED = ("agentapi", "antigravity-oauth-token", "mcp_oauth_tokens.json", "oauth_creds.json")
# Granted top-level state agy rewrites by temp-and-rename, which replaces the overlay's link rather
# than the real file. SQLite resolves its link and keeps its journals beside the real database.
AGY_RENAMED_STATE = ("history.jsonl", "jetbox_summaries_proto.pb", "jetski_state.pbtxt",
                     "last_check.timestamp", "installation_id")
AGY_STATE_COPY_MAX = 64 * 1024 * 1024
AGY_DATABASE = "conversation_summaries.db"
AGY_COPY_BACK_LOCK = ".provenant-copy-back.lock"
AGY_COPY_BACK_WAIT = 10


# The staged overlay: its path for HOME, its name in the attempt tmp, and the identity of each
# real renamed-state file at staging, so copy-back never overwrites another session's update.
AgyHome = namedtuple("AgyHome", "path name before")


def _identity(directory_fd, name):
    try:
        found = os.stat(name, dir_fd=directory_fd, follow_symlinks=False)
    except FileNotFoundError:
        return None
    return found.st_dev, found.st_ino, found.st_size, found.st_mtime_ns


def _link_overlay(fd, source, source_fd, parts):
    """Link source's entries into the open directory fd, descending into a real copy of parts[0].

    source_fd is the real directory opened without following links, or None when it is missing.
    """
    part = parts[0] if parts else None
    for name in sorted(os.listdir(source_fd)) if source_fd is not None else ():
        if name != part and not name.startswith(AGY_UNLINKED):
            os.symlink(source / name, name, dir_fd=fd)
    if part is None:
        return
    os.mkdir(part, 0o700, dir_fd=fd)
    child = os.open(part, provider_exec.DIRECTORY_FLAGS, dir_fd=fd)
    child_source = None
    try:
        try:
            child_source = os.open(part, provider_exec.DIRECTORY_FLAGS, dir_fd=source_fd) if source_fd is not None else None
        except FileNotFoundError:
            pass
        _link_overlay(child, source / part, child_source, parts[1:])
    finally:
        for descriptor in (child, child_source):
            if descriptor is not None:
                os.close(descriptor)


def stage_agy_home(plan, tmp_fd, private_tmp):
    """The confined agy lane's HOME: a fresh overlay of links per attempt in the open attempt tmp,
    made after its state directories so each of them is a link too."""
    if plan["adapter"] != "agy" or plan["applied"].get("confinement") != "sandbox-exec":
        return None
    provider_exec._prepare_state_directories(plan)
    real_fd = provider_exec._open_canonical_dir(provider_exec._provider_state(plan)["resolved_home"] / "antigravity-cli")
    try:
        # SQLite resolves a link to its database; with no database to link, it would make one in
        # the overlay that cleanup then discards. An empty file is a valid empty database.
        try:
            os.close(os.open(AGY_DATABASE, os.O_WRONLY | os.O_CREAT | os.O_EXCL | os.O_NOFOLLOW, 0o600,
                             dir_fd=real_fd))
        except FileExistsError:
            pass
        before = {name: _identity(real_fd, name) for name in AGY_RENAMED_STATE}
    finally:
        os.close(real_fd)
    name = "agy-home-" + uuid.uuid4().hex[:12]
    os.mkdir(name, 0o700, dir_fd=tmp_fd)
    home = provider_exec._open_canonical_dir(Path.home().resolve())
    overlay = None
    try:
        overlay = os.open(name, provider_exec.DIRECTORY_FLAGS, dir_fd=tmp_fd)
        _link_overlay(overlay, Path.home(), home, AGY_PRIVATE_BIN.parts)
    except BaseException:
        provider_exec.attempt_storage.remove_at(tmp_fd, name)
        raise
    finally:
        for descriptor in (home, overlay):
            if descriptor is not None:
                os.close(descriptor)
    return AgyHome(Path(private_tmp) / name, name, before)


def _read_overlay_state(lane_fd, name):
    """A renamed state file's bytes, or (None, problem); never follows a link."""
    try:
        fd = os.open(name, os.O_RDONLY | os.O_NOFOLLOW | os.O_NONBLOCK, dir_fd=lane_fd)
    except FileNotFoundError:
        return None, None
    except OSError as exc:
        linked = stat.S_ISLNK(os.stat(name, dir_fd=lane_fd, follow_symlinks=False).st_mode)
        return None, None if linked else f"{name}: {exc}"
    try:
        found = os.fstat(fd)
        if not stat.S_ISREG(found.st_mode):
            return None, f"{name}: not a regular file"
        if found.st_nlink != 1:  # a hard link would copy back another file's bytes
            return None, f"{name}: not a single-link regular file"
        chunks, size = [], 0
        while size <= AGY_STATE_COPY_MAX and (chunk := os.read(fd, 1024 * 1024)):
            chunks.append(chunk)
            size += len(chunk)
        if max(size, found.st_size) > AGY_STATE_COPY_MAX:
            return None, f"{name}: larger than {AGY_STATE_COPY_MAX} bytes"
        if size != found.st_size:
            return None, f"{name}: changed while being read"
        return b"".join(chunks), None
    finally:
        os.close(fd)


def _install_state(directory, name, data):
    """Replace name in the open real directory by a staged file renamed over it."""
    staged = f".{name}.provenant-{uuid.uuid4().hex}"
    try:
        current = os.stat(name, dir_fd=directory, follow_symlinks=False)
        mode = stat.S_IMODE(current.st_mode) & 0o666 if stat.S_ISREG(current.st_mode) else 0o600
    except FileNotFoundError:
        mode = 0o600
    try:
        provider_exec._write_new(directory, staged, data, mode)
        os.replace(staged, name, src_dir_fd=directory, dst_dir_fd=directory)
    except OSError:
        try:
            os.unlink(staged, dir_fd=directory)
        except OSError:
            pass
        raise


def _append_history(directory, data, before):
    """Append only the lines the lane added to history.jsonl since staging; others may append too."""
    flags = os.O_RDWR | os.O_APPEND | os.O_NOFOLLOW | (os.O_CREAT if before is None else 0)
    try:
        fd = os.open("history.jsonl", flags, 0o600, dir_fd=directory)
    except FileNotFoundError:
        return "history.jsonl: removed since staging"
    try:
        found = os.fstat(fd)
        start = 0 if before is None else before[2]
        if not stat.S_ISREG(found.st_mode) or found.st_nlink != 1:
            return "history.jsonl: not a single-link regular file"
        if before is not None and ((found.st_dev, found.st_ino) != before[:2] or found.st_size < start):
            return "history.jsonl: replaced since staging"
        if start > len(data):  # never read a staged size the copy itself does not reach
            return "history.jsonl: shorter than at staging"
        for offset in range(0, start, 1024 * 1024):
            chunk = data[offset:min(start, offset + 1024 * 1024)]
            if os.pread(fd, len(chunk), offset) != chunk:
                return "history.jsonl: rewritten rather than appended"
        view = memoryview(data)[start:]
        while view:
            view = view[os.write(fd, view):]
        return None
    finally:
        os.close(fd)


def finish_agy_home(plan, tmp_fd, home):
    """Copy renamed state back to the real home, then remove the overlay. Returns warnings.

    Every step starts from the attempt tmp held open since before staging or from a directory
    opened from / without following a link, so a lane that swapped tmp, the overlay or an
    ancestor cannot redirect a read, write or delete. A real file another session changed since
    staging is kept, with a warning.
    """
    problems = []
    fds = []
    try:
        fds.append(os.open(home.name, provider_exec.DIRECTORY_FLAGS, dir_fd=tmp_fd))
        for part in AGY_PRIVATE_BIN.parts[:-1]:
            fds.append(os.open(part, provider_exec.DIRECTORY_FLAGS, dir_fd=fds[-1]))
        fds.append(provider_exec._open_canonical_dir(provider_exec._provider_state(plan)["resolved_home"] / "antigravity-cli"))
        lane_fd, real_fd = fds[-2], fds[-1]
        # Concurrent finishers take turns, so each compares and installs against the last install.
        # A confined lane may read, and so lock, the file too: wait a bounded time, never forever.
        fds.append(os.open(AGY_COPY_BACK_LOCK, os.O_RDWR | os.O_CREAT | os.O_NOFOLLOW, 0o600, dir_fd=real_fd))
        deadline = time.monotonic() + AGY_COPY_BACK_WAIT
        while True:
            try:
                fcntl.flock(fds[-1], fcntl.LOCK_EX | fcntl.LOCK_NB)
                break
            except BlockingIOError:
                if time.monotonic() > deadline:
                    raise OSError(f"{AGY_COPY_BACK_LOCK} stayed locked for {AGY_COPY_BACK_WAIT}s") from None
                time.sleep(0.05)
        for name in AGY_RENAMED_STATE:
            try:
                data, problem = _read_overlay_state(lane_fd, name)
                if data is None:
                    pass
                elif name == "history.jsonl":
                    problem = _append_history(real_fd, data, home.before[name])
                elif _identity(real_fd, name) != home.before[name]:
                    problem = f"{name}: changed by another session since staging; kept it"
                else:
                    _install_state(real_fd, name, data)
            except OSError as exc:
                problem = f"{name}: {exc}"
            if problem:
                problems.append("agy state not copied back: " + problem)
    except OSError as exc:
        problems.append(f"agy state not copied back: {exc}")
    finally:
        for fd in fds:
            os.close(fd)
    left = provider_exec.attempt_storage.remove_at(tmp_fd, home.name)
    if left:
        problems.append("agy home overlay not removed: " + ", ".join(left))
    return problems


def needs_ps_shim(plan):
    """macOS refuses to exec the setuid /bin/ps inside any sandbox, native or sandbox-exec."""
    return sys.platform == "darwin" and (
        (plan["adapter"] == "codex" and plan["applied"]["sandbox"] != "full")
        or plan["applied"].get("confinement") == "sandbox-exec")


def stage_ps_shim(tmp_fd, private_tmp):
    """Copy the ps shim into the attempt's temp, which every sandbox lets the lane read.

    The product checkout may sit below a denied home or outside a read-only lane's cwd. A retry
    reuses the attempt tmp, so whatever the earlier lane left at the shim is removed, never
    written through.
    """
    source = Path(__file__).resolve().parent
    left = provider_exec.attempt_storage.remove_at(tmp_fd, "provenant-shim")
    if left:
        raise OSError(f"ps shim left by an earlier try was not removed: {', '.join(left)}")
    os.mkdir("provenant-shim", 0o700, dir_fd=tmp_fd)
    stage = os.open("provenant-shim", provider_exec.DIRECTORY_FLAGS, dir_fd=tmp_fd)
    try:
        provider_exec._write_new(stage, "process_info.py", (source / "process_info.py").read_bytes(), 0o644)
        os.mkdir("bin", 0o700, dir_fd=stage)
        shim_bin = os.open("bin", provider_exec.DIRECTORY_FLAGS, dir_fd=stage)
        try:
            provider_exec._write_new(shim_bin, "ps", (source / "bin" / "ps").read_bytes(), 0o755)
        finally:
            os.close(shim_bin)
    finally:
        os.close(stage)
    return Path(private_tmp) / "provenant-shim" / "bin"
