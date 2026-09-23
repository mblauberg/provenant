#!/usr/bin/env python3
"""Portable process facts for the supervisor and the sandbox-safe ps command."""

from __future__ import annotations

from collections import deque
import ctypes
import ctypes.util
from dataclasses import dataclass
from functools import lru_cache
import os
from pathlib import Path
import shlex
import subprocess
import stat
import sys
import time

@dataclass(frozen=True)
class _ProcessRow:
    pid: int
    ppid: int
    pgid: int
    started: str
    command: str
    zombie: bool = False
    tty: str = "??"
    status: int = 0

    @property
    def identity(self):
        return self.pid, self.started


class _DarwinBsdInfo(ctypes.Structure):
    _fields_ = [
        ("flags", ctypes.c_uint32), ("status", ctypes.c_uint32),
        ("xstatus", ctypes.c_uint32), ("pid", ctypes.c_uint32),
        ("ppid", ctypes.c_uint32), ("uid", ctypes.c_uint32),
        ("gid", ctypes.c_uint32), ("ruid", ctypes.c_uint32),
        ("rgid", ctypes.c_uint32), ("svuid", ctypes.c_uint32),
        ("svgid", ctypes.c_uint32), ("reserved", ctypes.c_uint32),
        ("comm", ctypes.c_char * 16), ("name", ctypes.c_char * 32),
        ("nfiles", ctypes.c_uint32), ("pgid", ctypes.c_uint32),
        ("pjobc", ctypes.c_uint32), ("tdev", ctypes.c_uint32),
        ("tpgid", ctypes.c_uint32), ("nice", ctypes.c_int32),
        ("start_sec", ctypes.c_uint64), ("start_usec", ctypes.c_uint64),
    ]


@lru_cache(maxsize=1)
def _darwin_libproc():
    library = ctypes.util.find_library("proc")
    if not library:
        return None
    try:
        libproc = ctypes.CDLL(library, use_errno=True)
    except OSError:
        return None
    libproc.proc_listallpids.argtypes = (ctypes.c_void_p, ctypes.c_int)
    libproc.proc_listallpids.restype = ctypes.c_int
    libproc.proc_pidinfo.argtypes = (
        ctypes.c_int, ctypes.c_int, ctypes.c_uint64, ctypes.c_void_p, ctypes.c_int,
    )
    libproc.proc_pidinfo.restype = ctypes.c_int
    return libproc


def _linux_process_row(pid, proc_root=Path("/proc")):
    stat = (proc_root / str(pid) / "stat").read_bytes().decode(errors="replace")
    end = stat.rfind(") ")
    if end < 0:
        return None
    fields = stat[end + 2:].split()
    if len(fields) < 20:
        return None
    return _ProcessRow(
        pid, int(fields[1]), int(fields[2]), fields[19],
        stat[stat.find("(") + 1:end], fields[0] == "Z",
    )


def _linux_tree_snapshot(root_pid, known_pids, proc_root=Path("/proc")):
    """Walk task children from the direct provider and previously seen children."""
    rows = {}
    queue = deque([root_pid, *known_pids])
    visited = set()
    while queue:
        pid = queue.popleft()
        if pid in visited:
            continue
        visited.add(pid)
        try:
            row = _linux_process_row(pid, proc_root)
        except OSError:
            continue
        if row is None:
            continue
        rows[pid] = row
        task = proc_root / str(pid) / "task"
        try:
            threads = list(task.iterdir())
        except OSError:
            return None  # Kernel lacks task/children; use the full census.
        for thread in threads:
            try:
                children = (thread / "children").read_text()
            except OSError:
                return None
            queue.extend(int(child) for child in children.split())
    return rows


@lru_cache(maxsize=256)
def _tty_name(device):
    if device in {0, 0xffffffff}:
        return "??"
    try:
        with os.scandir("/dev") as entries:
            for entry in entries:
                try:
                    info = entry.stat(follow_symlinks=True)
                except OSError:
                    continue
                if stat.S_ISCHR(info.st_mode) and info.st_rdev & 0xffffffff == device:
                    return entry.name
    except OSError:
        pass
    return str(device)


def _darwin_process_row(libproc, pid):
    info = _DarwinBsdInfo()
    if libproc.proc_pidinfo(pid, 3, 0, ctypes.byref(info), ctypes.sizeof(info)) != ctypes.sizeof(info):
        return None
    return _ProcessRow(
        pid, info.ppid, info.pgid,
        f"{info.start_sec}.{info.start_usec:06d}",
        info.name.split(b"\0", 1)[0].decode(errors="replace")
        or info.comm.split(b"\0", 1)[0].decode(errors="replace"),
        info.status == 5,
        _tty_name(info.tdev),
        info.status,
    )


def _probe_process_row(pid):
    """One pid's row, or None when it cannot be read."""
    try:
        if sys.platform == "darwin":
            libproc = _darwin_libproc()
            return _darwin_process_row(libproc, pid) if libproc is not None else None
        if sys.platform.startswith("linux"):
            return _linux_process_row(pid)
    except Exception:
        return None
    return None


@dataclass(frozen=True)
class ProcessInfo:
    pid: int
    ppid: int
    pgid: int
    lstart: str
    elapsed: str
    cpu: str
    command: str
    comm: str
    stat: str
    rss: int
    tty: str = "??"
    uid: int = -1


@lru_cache(maxsize=1)
def _linux_boot_time():
    for line in Path("/proc/stat").read_text().splitlines():
        if line.startswith("btime "):
            return int(line.split()[1])
    raise ValueError("Linux boot time unavailable")


def _lstart(epoch):
    local = time.localtime(epoch)
    days = ("Mon", "Tue", "Wed", "Thu", "Fri", "Sat", "Sun")
    months = ("", "Jan", "Feb", "Mar", "Apr", "May", "Jun",
              "Jul", "Aug", "Sep", "Oct", "Nov", "Dec")
    return (f"{days[local.tm_wday]} {months[local.tm_mon]} "
            f"{local.tm_mday:2d} {local.tm_hour:02d}:{local.tm_min:02d}:"
            f"{local.tm_sec:02d} {local.tm_year}")


def _duration(seconds, *, elapsed=False):
    seconds = max(0, seconds)
    whole = int(seconds)
    days, rem = divmod(whole, 86400)
    hours, rem = divmod(rem, 3600)
    minutes, secs = divmod(rem, 60)
    if not elapsed:
        hundredths = min(99, int((seconds - whole) * 100 + 0.5))
        total_hours = days * 24 + hours
        if sys.platform == "darwin":  # macOS ps: minutes are unbounded, 69:45.86
            return f"{total_hours * 60 + minutes}:{secs:02d}.{hundredths:02d}"
        if total_hours:
            return f"{total_hours}:{minutes:02d}:{secs:02d}.{hundredths:02d}"
        return f"{minutes}:{secs:02d}.{hundredths:02d}"
    if days:
        return f"{days}-{hours:02d}:{minutes:02d}:{secs:02d}"
    if hours:
        return f"{hours:02d}:{minutes:02d}:{secs:02d}"
    return f"{minutes:02d}:{secs:02d}"


class _DarwinTaskInfo(ctypes.Structure):
    _fields_ = [
        ("virtual_size", ctypes.c_uint64), ("resident_size", ctypes.c_uint64),
        ("total_user", ctypes.c_uint64), ("total_system", ctypes.c_uint64),
        ("threads_user", ctypes.c_uint64), ("threads_system", ctypes.c_uint64),
        *[(name, ctypes.c_int32) for name in (
            "policy", "faults", "pageins", "cow_faults", "messages_sent",
            "messages_received", "syscalls_mach", "syscalls_unix", "csw",
            "threadnum", "numrunning", "priority",
        )],
    ]


class _MachTimebase(ctypes.Structure):
    _fields_ = [("numer", ctypes.c_uint32), ("denom", ctypes.c_uint32)]


@lru_cache(maxsize=1)
def _mach_tick_seconds():
    """Task CPU counters are Mach ticks: nanoseconds on Intel, 125/3 ns on Apple Silicon."""
    timebase = _MachTimebase()
    try:
        if ctypes.CDLL(None).mach_timebase_info(ctypes.byref(timebase)) == 0 and timebase.denom:
            return timebase.numer / timebase.denom / 1_000_000_000
    except (OSError, AttributeError):
        pass
    return 1 / 1_000_000_000


def _darwin_argv(pid):
    libc = ctypes.CDLL(None, use_errno=True)
    mib = (ctypes.c_int * 3)(1, 49, pid)  # CTL_KERN, KERN_PROCARGS2
    size = ctypes.c_size_t()
    if libc.sysctl(mib, 3, None, ctypes.byref(size), None, 0) != 0 or size.value > 1024 * 1024:
        return ""
    data = ctypes.create_string_buffer(size.value)
    if libc.sysctl(mib, 3, data, ctypes.byref(size), None, 0) != 0:
        return ""
    raw = data.raw[:size.value]
    if len(raw) < 4:
        return ""
    argc = ctypes.c_int.from_buffer_copy(raw).value
    if not 0 < argc < 65536:
        return ""
    cursor = raw.find(b"\0", 4) + 1
    if cursor == 0:
        return ""
    while cursor < len(raw) and raw[cursor] == 0:
        cursor += 1
    args = []
    for _ in range(argc):
        end = raw.find(b"\0", cursor)
        if end < 0:
            break
        args.append(os.fsdecode(raw[cursor:end]))
        cursor = end + 1
    return shlex.join(args)


def process(pid):
    """Return readable process facts, or None when identity cannot be inspected."""
    try:
        row = _probe_process_row(pid)
        if row is None:
            if sys.platform == "darwin" and _darwin_libproc() is not None:
                return None
            if sys.platform.startswith("linux") and Path("/proc").is_dir():
                return None
            return _ps_process(pid)
        if sys.platform == "darwin":
            epoch = float(row.started)
            task = _DarwinTaskInfo()
            libproc = _darwin_libproc()
            read = libproc.proc_pidinfo(pid, 4, 0, ctypes.byref(task), ctypes.sizeof(task))
            complete = read == ctypes.sizeof(task)
            cpu = (task.total_user + task.total_system) * _mach_tick_seconds() if complete else 0
            rss = task.resident_size // 1024 if read == ctypes.sizeof(task) else 0
            command = _darwin_argv(pid) or row.command
            # The BSD status says SRUN for sleeping processes too; running threads decide R or S.
            stat = {4: "T", 5: "Z"}.get(row.status) or ("R" if complete and task.numrunning > 0 else "S")
            tty = row.tty
            info = _DarwinBsdInfo()
            read = libproc.proc_pidinfo(pid, 3, 0, ctypes.byref(info), ctypes.sizeof(info))
            uid = info.uid if read == ctypes.sizeof(info) else -1
        elif sys.platform.startswith("linux"):
            raw = Path(f"/proc/{pid}/stat").read_text(errors="replace")
            fields = raw[raw.rfind(") ") + 2:].split()
            ticks = os.sysconf("SC_CLK_TCK")
            epoch = _linux_boot_time() + int(row.started) / ticks
            cpu = (int(fields[11]) + int(fields[12])) / ticks
            rss = int(fields[21]) * os.sysconf("SC_PAGE_SIZE") // 1024
            try:
                argv = Path(f"/proc/{pid}/cmdline").read_bytes().strip(b"\0")
                # A kernel thread has an empty cmdline; shlex.join([""]) would be the truthy "''".
                command = shlex.join(os.fsdecode(arg) for arg in argv.split(b"\0")) if argv else row.command
            except OSError:
                command = row.command
            stat = fields[0]
            tty = "?" if fields[4] == "0" else fields[4]
            uid = Path(f"/proc/{pid}").stat().st_uid
        else:
            return _ps_process(pid)
        return ProcessInfo(pid, row.ppid, row.pgid, _lstart(epoch),
                           _duration(time.time() - epoch, elapsed=True), _duration(cpu),
                           command, row.command, stat, rss, tty, uid)
    except (OSError, ValueError, IndexError, TypeError, AttributeError):
        return None


def processes():
    if sys.platform == "darwin":
        libproc = _darwin_libproc()
        if libproc is None:
            return _ps_processes()
        count = libproc.proc_listallpids(None, 0)
        if count <= 0:
            return _ps_processes()
        pids = (ctypes.c_int * (max(count, 0) + 128))()
        count = libproc.proc_listallpids(pids, ctypes.sizeof(pids))
        if count <= 0:
            return _ps_processes()
        candidates = pids[:max(count, 0)]
    elif sys.platform.startswith("linux"):
        try:
            candidates = (int(entry.name) for entry in os.scandir("/proc") if entry.name.isdecimal())
        except OSError:
            return _ps_processes()
    else:
        return _ps_processes()
    return [info for pid in candidates if pid > 0 and (info := process(pid)) is not None]


def start_time(pid):
    info = process(pid)
    if info is not None:
        return info.lstart
    return _ps_start_time(pid)


def _ps_start_time(pid, *, canonical=True):
    try:
        result = subprocess.run(
            ["/bin/ps", "-o", "lstart=", "-p", str(pid)], capture_output=True,
            text=True, timeout=2, check=False,
            env={**os.environ, **({"LC_ALL": "C", "LANG": "C"} if canonical else {})},
        )
        return result.stdout.strip() if result.returncode == 0 else None
    except (OSError, subprocess.SubprocessError):
        return None


def _ps_process(pid):
    try:
        result = subprocess.run(
            ["/bin/ps", "-o", "pid=,ppid=,pgid=,etime=,time=,stat=,rss=,command=", "-p", str(pid)],
            capture_output=True, text=True, timeout=2, check=False,
            env={**os.environ, "LC_ALL": "C", "LANG": "C"},
        )
        if result.returncode or not result.stdout.strip():
            return None
        values = result.stdout.strip().split(None, 7)
        if len(values) != 8:
            return None
        _, ppid, pgid, elapsed, cpu, stat, rss, command = values
        lstart = _ps_start_time(pid)
        if not lstart:
            return None
        return ProcessInfo(pid, int(ppid), int(pgid), lstart, elapsed, cpu,
                           command, Path(command.split()[0]).name, stat, int(rss))
    except (OSError, subprocess.SubprocessError, ValueError, IndexError):
        return None


def _ps_processes():
    try:
        result = subprocess.run(["/bin/ps", "-axo", "pid="], capture_output=True,
                                text=True, timeout=5, check=False)
        if result.returncode:
            raise OSError(result.stderr.strip() or "ps census failed")
        pids = [int(value) for value in result.stdout.split() if value.isdecimal()]
        if not pids:
            raise OSError("ps census returned no PIDs")
        rows = [row for pid in pids if (row := _ps_process(pid)) is not None]
        if not rows:
            raise OSError("ps census could not inspect any PID")
        return rows
    except subprocess.SubprocessError as exc:
        raise OSError(f"ps census failed: {exc}") from exc
