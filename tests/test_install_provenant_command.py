import importlib.util
import errno
import os
from pathlib import Path
import subprocess

import pytest


ROOT = Path(__file__).resolve().parents[1]
HELPER = ROOT / "scripts" / "install-provenant-command.py"
SOURCE = ROOT / "scripts" / "provenant.template"


def load_helper():
    spec = importlib.util.spec_from_file_location("install_provenant_command", HELPER)
    assert spec is not None
    assert spec.loader is not None
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


def invoke(*arguments: str) -> subprocess.CompletedProcess[str]:
    return subprocess.run(
        [str(HELPER), *arguments],
        cwd=ROOT,
        text=True,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        check=False,
    )


def test_publish_rejects_a_foreign_link_raced_into_atomic_exchange(
    tmp_path,
    monkeypatch,
):
    helper = load_helper()
    destination = tmp_path / "bin/provenant"
    destination.parent.mkdir()
    destination.write_bytes(SOURCE.read_bytes() + b"\n" + helper.MANAGED_MARKER)
    foreign_target = tmp_path / "foreign/provenant"
    exchange = helper._exchange
    raced = False

    def race_then_exchange(first, second):
        nonlocal raced
        if not raced:
            second.unlink()
            second.symlink_to(foreign_target)
            raced = True
        exchange(first, second)

    monkeypatch.setattr(helper, "_exchange", race_then_exchange)

    try:
        helper.publish(SOURCE, destination, "managed-file")
    except helper.Collision:
        pass
    else:
        raise AssertionError("foreign race was not rejected")

    assert raced
    assert destination.is_symlink()
    assert destination.readlink() == foreign_target


def test_publish_preserves_a_displaced_file_when_rollback_also_races(
    tmp_path,
    monkeypatch,
):
    helper = load_helper()
    destination = tmp_path / "bin/provenant"
    destination.parent.mkdir()
    destination.write_bytes(SOURCE.read_bytes() + b"\n" + helper.MANAGED_MARKER)
    first_foreign = b"first foreign file\n"
    second_foreign = b"second foreign file\n"
    exchange = helper._exchange
    raced = False

    def race_exchange(first, second):
        nonlocal raced
        if not raced:
            second.unlink()
            second.write_bytes(first_foreign)
            exchange(first, second)
            second.unlink()
            second.write_bytes(second_foreign)
            raced = True
            return
        exchange(first, second)

    monkeypatch.setattr(helper, "_exchange", race_exchange)

    try:
        helper.publish(SOURCE, destination, "managed-file")
    except helper.Collision as exc:
        assert "displaced path preserved=" in str(exc)
    else:
        raise AssertionError("two-step foreign race was not rejected")

    assert destination.read_bytes() == second_foreign
    recovery_paths = list(destination.parent.glob(".provenant.*"))
    assert any(path.read_bytes() == first_foreign for path in recovery_paths)


def test_publish_preserves_a_displaced_file_when_rollback_raises(
    tmp_path,
    monkeypatch,
):
    helper = load_helper()
    destination = tmp_path / "bin/provenant"
    destination.parent.mkdir()
    destination.write_bytes(SOURCE.read_bytes() + b"\n" + helper.MANAGED_MARKER)
    foreign = b"foreign file requiring recovery\n"
    exchange = helper._exchange
    raced = False

    def race_exchange(first, second):
        nonlocal raced
        if not raced:
            second.unlink()
            second.write_bytes(foreign)
            raced = True
        exchange(first, second)

    def fail_rollback(*_args):
        raise OSError("injected rollback failure")

    monkeypatch.setattr(helper, "_exchange", race_exchange)
    monkeypatch.setattr(helper, "_restore_after_mismatch", fail_rollback)

    try:
        helper.publish(SOURCE, destination, "managed-file")
    except OSError:
        pass
    else:
        raise AssertionError("rollback failure was not propagated")

    recovery_paths = list(destination.parent.glob(".provenant.*"))
    assert any(path.read_bytes() == foreign for path in recovery_paths)


@pytest.mark.parametrize(
    ("exc_type", "code", "message", "label"),
    (
        pytest.param(
            OSError,
            errno.EINVAL,
            "Invalid argument",
            "racing type swap",
            id="test_snapshot_maps_a_racing_type_swap_to_a_collision",
        ),
        pytest.param(
            FileNotFoundError,
            errno.ENOENT,
            "No such file or directory",
            "racing delete",
            id="test_snapshot_maps_a_racing_delete_to_a_collision",
        ),
    ),
)
def test_snapshot_maps_a_racing_change_to_a_collision(
    tmp_path, monkeypatch, exc_type, code, message, label
):
    helper = load_helper()
    destination = tmp_path / "bin/provenant"
    destination.parent.mkdir()
    foreign_target = tmp_path / "foreign/provenant"
    destination.symlink_to(foreign_target)
    real_readlink = os.readlink

    def raced_before_read(path, *args, **kwargs):
        if Path(path) == destination:
            raise exc_type(code, message, str(path))
        return real_readlink(path, *args, **kwargs)

    monkeypatch.setattr(helper.os, "readlink", raced_before_read)

    try:
        helper._raw_snapshot(destination)
    except helper.Collision:
        pass
    else:
        raise AssertionError(f"{label} was not mapped to a collision")


def test_snapshot_propagates_a_non_race_read_failure(tmp_path, monkeypatch):
    helper = load_helper()
    destination = tmp_path / "bin/provenant"
    destination.parent.mkdir()
    foreign_target = tmp_path / "foreign/provenant"
    destination.symlink_to(foreign_target)
    real_readlink = os.readlink

    def denied_before_read(path, *args, **kwargs):
        if Path(path) == destination:
            raise PermissionError(errno.EACCES, "Permission denied", str(path))
        return real_readlink(path, *args, **kwargs)

    monkeypatch.setattr(helper.os, "readlink", denied_before_read)

    try:
        helper._raw_snapshot(destination)
    except PermissionError:
        pass
    except helper.Collision:
        raise AssertionError("non-race read failure was masked as a collision")
    else:
        raise AssertionError("non-race read failure was swallowed")


# --- A compiled front door goes through the same managed-file rule (ADR 0027) ---


def compiled_front_door(path: Path, digest: str = "a" * 64) -> Path:
    """Bytes that are not text: NULs, 0xff, a long line, then the identity as its own lines."""
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_bytes(
        b"\x7fELF\x00\x01\xff\xfe" * 64
        + b"\n"
        + load_helper().MANAGED_MARKER
        + b"\nprovenant-front-door source-sha256="
        + digest.encode()
        + b"\n\x00\x00trailing"
    )
    path.chmod(0o755)
    return path


def test_binary_source_replaces_the_managed_template_and_back(tmp_path):
    binary = compiled_front_door(tmp_path / "build/provenant")
    destination = tmp_path / "bin/provenant"
    destination.parent.mkdir()
    destination.write_bytes(SOURCE.read_bytes())

    assert invoke("classify", "--source", str(binary), "--destination", str(destination)).stdout == "managed-file\n"
    published = invoke(
        "publish", "--source", str(binary), "--destination", str(destination), "--expected", "managed-file"
    )
    assert published.stdout == "updated\n", published.stderr
    assert destination.read_bytes() == binary.read_bytes()
    assert os.access(destination, os.X_OK) and (destination.stat().st_mode & 0o777) == 0o755

    # Rollback: the template is the source now, and the installed binary is managed.
    assert invoke("classify", "--source", str(SOURCE), "--destination", str(destination)).stdout == "managed-file\n"
    rolled = invoke(
        "publish", "--source", str(SOURCE), "--destination", str(destination), "--expected", "managed-file"
    )
    assert rolled.stdout == "updated\n", rolled.stderr
    assert destination.read_bytes() == SOURCE.read_bytes()


def test_identical_binary_is_existing_and_a_rebuilt_one_is_managed(tmp_path):
    binary = compiled_front_door(tmp_path / "build/provenant")
    destination = tmp_path / "bin/provenant"
    destination.parent.mkdir()
    destination.write_bytes(binary.read_bytes())
    rebuilt = compiled_front_door(tmp_path / "rebuilt/provenant", digest="b" * 64)

    assert invoke("classify", "--source", str(binary), "--destination", str(destination)).stdout == "existing\n"
    assert invoke("classify", "--source", str(rebuilt), "--destination", str(destination)).stdout == "managed-file\n"


def test_unmarked_binary_is_a_foreign_collision(tmp_path):
    binary = compiled_front_door(tmp_path / "build/provenant")
    destination = tmp_path / "bin/provenant"
    destination.parent.mkdir()
    destination.write_bytes(b"\x7fELF\x00 someone else's binary, no marker\n")

    result = invoke("classify", "--source", str(binary), "--destination", str(destination))

    assert result.returncode == 3
    assert "provenant command collision=" in result.stderr
