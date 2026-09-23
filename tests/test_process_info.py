"""Process inspection works without executing the setuid system ps."""

import os
from pathlib import Path
import shlex
import subprocess
import sys

import pytest

SCRIPTS = Path(__file__).resolve().parents[1] / "skills/orchestrate/scripts"
sys.path.insert(0, str(SCRIPTS))


def test_start_time_matches_system_ps():
    import process_info

    observed = process_info.process(os.getpid())
    assert observed is not None
    try:
        system = subprocess.run(
            ["/bin/ps", "-o", "lstart=", "-p", str(os.getpid())],
            capture_output=True, text=True, check=True,
            env={**os.environ, "LC_ALL": "C", "LANG": "C"},
        )
    except (OSError, subprocess.CalledProcessError):
        pytest.skip("setuid /bin/ps cannot execute in this sandbox")
    assert observed.lstart == system.stdout.strip()


def test_shim_formats_requested_fields_and_default_output():
    shim = SCRIPTS / "bin/ps"
    pid = str(os.getpid())
    result = subprocess.run([str(shim), "-o", "pid=,ppid=,pgid=,lstart=,etime=,time=,command=", "-p", pid],
                            capture_output=True, text=True, check=True)
    assert result.stdout.strip().startswith(pid)
    assert "python" in result.stdout.lower()
    assert len(result.stdout.strip().split()) >= 11
    default = subprocess.run([str(shim), "-p", pid], capture_output=True, text=True, check=True)
    assert default.stdout.splitlines()[0].split() == ["PID", "TTY", "TIME", "CMD"]
    unsupported = subprocess.run([str(shim), "-Z"], capture_output=True, text=True)
    assert unsupported.returncode != 0
    assert "supported" in unsupported.stderr.lower()


def test_shim_accepts_compact_all_process_options():
    shim = SCRIPTS / "bin/ps"
    result = subprocess.run([str(shim), "-axo", "pid=,ppid=,etime=,time=,command="],
                            capture_output=True, text=True, check=True)
    assert any(line.split()[0] == str(os.getpid()) for line in result.stdout.splitlines())


def test_shim_O_adds_fields_to_default_output():
    shim = SCRIPTS / "bin/ps"
    result = subprocess.run([str(shim), "-O", "ppid", "-p", str(os.getpid())],
                            capture_output=True, text=True, check=True)
    assert result.stdout.splitlines()[0].split() == ["PID", "TTY", "TIME", "PPID", "CMD"]
    cells = result.stdout.splitlines()[1].split(None, 4)
    assert cells[3] == str(os.getppid())
    assert "pytest" in cells[4]


def test_shim_cpu_time_retains_hundredths():
    shim = SCRIPTS / "bin/ps"
    result = subprocess.run([str(shim), "-o", "time=", "-p", str(os.getpid())],
                            capture_output=True, text=True, check=True)
    assert "." in result.stdout.strip()


def test_unavailable_census_is_not_an_empty_process_list(monkeypatch):
    import process_info

    class UnavailableLibproc:
        def proc_listallpids(self, *_args):
            return 0

    monkeypatch.setattr(process_info.sys, "platform", "darwin")
    monkeypatch.setattr(process_info, "_darwin_libproc", lambda: UnavailableLibproc())
    monkeypatch.setattr(process_info.subprocess, "run",
                        lambda *_args, **_kwargs: (_ for _ in ()).throw(PermissionError("seatbelt")))
    with pytest.raises(OSError, match="seatbelt"):
        process_info.processes()


@pytest.mark.skipif(sys.platform != "darwin", reason="macOS tty device naming")
def test_tty_device_is_rendered_by_name():
    import process_info

    master, slave = os.openpty()
    try:
        assert process_info._tty_name(os.fstat(slave).st_rdev) == Path(os.ttyname(slave)).name
    finally:
        os.close(master)
        os.close(slave)


def test_command_preserves_arguments_with_spaces():
    import process_info

    child = subprocess.Popen([sys.executable, "-c", "import time; time.sleep(5)", "two words"])
    try:
        observed = process_info.process(child.pid)
        assert observed is not None
        assert shlex.split(observed.command)[-1] == "two words"
    finally:
        child.terminate()
        child.wait(timeout=5)


@pytest.mark.parametrize(("argv", "header"), [
    (["aux"], ["USER", "PID", "RSS", "TT", "STAT", "TIME", "COMMAND"]),
    (["-ef"], ["UID", "PID", "PPID", "TTY", "TIME", "CMD"]),
    (["ax"], ["PID", "TTY", "TIME", "CMD"]),
])
def test_shim_accepts_the_forms_agents_type(argv, header):
    shim = SCRIPTS / "bin/ps"
    result = subprocess.run([str(shim), *argv], capture_output=True, text=True, check=True)
    lines = result.stdout.splitlines()
    assert lines[0].split() == header
    pid_column = header.index("PID")
    pids = [int(line.split()[pid_column]) for line in lines[1:]]
    assert os.getpid() in pids
    assert pids == sorted(pids)


def test_shim_ends_quietly_when_its_reader_closes():
    shim = SCRIPTS / "bin/ps"
    reader = subprocess.Popen([str(shim), "ax"], stdout=subprocess.PIPE, stderr=subprocess.PIPE)
    reader.stdout.readline()
    reader.stdout.close()  # as head -1 does
    _, stderr = reader.communicate(timeout=30)
    assert stderr == b""


def test_cpu_time_matches_what_the_process_used():
    """macOS task counters are Mach ticks (125/3 ns on Apple Silicon), not nanoseconds."""
    import process_info
    import resource
    import time

    started = time.process_time()
    while time.process_time() - started < 1.0:
        pass
    usage = resource.getrusage(resource.RUSAGE_SELF)
    minutes, seconds = process_info.process(os.getpid()).cpu.split(":")
    reported = int(minutes) * 60 + float(seconds)
    assert abs(reported - (usage.ru_utime + usage.ru_stime)) < 0.5


def test_a_sleeping_process_is_not_reported_running():
    import process_info
    import time

    sleeper = subprocess.Popen([sys.executable, "-c", "import time; time.sleep(30)"])
    try:
        time.sleep(0.5)
        assert process_info.process(sleeper.pid).stat == "S"
    finally:
        sleeper.kill()
        sleeper.wait()


def test_shim_accepts_bsd_axo():
    shim = SCRIPTS / "bin/ps"
    result = subprocess.run([str(shim), "axo", "pid,comm"], capture_output=True, text=True, check=True)
    assert result.stdout.splitlines()[0].split() == ["PID", "COMM"]


@pytest.mark.skipif(os.geteuid() == 0, reason="root can read every process")
def test_shim_shows_an_unreadable_live_process_rather_than_dropping_it():
    import process_info

    if process_info.process(1) is not None:
        pytest.skip("pid 1 is readable on this host")
    shim = SCRIPTS / "bin/ps"
    result = subprocess.run([str(shim), "-o", "pid=,stat=", "-p", f"{os.getpid()},1"],
                            capture_output=True, text=True, check=True)
    assert [line.split()[0] for line in result.stdout.splitlines()] == ["1", str(os.getpid())]
    assert "cannot be read" in result.stderr
