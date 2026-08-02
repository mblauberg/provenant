import importlib.util
import errno
import os
from pathlib import Path
import subprocess
import sys


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
        [sys.executable, str(HELPER), *arguments],
        cwd=ROOT,
        text=True,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        check=False,
    )


def test_publish_rejects_a_destination_changed_after_legacy_preflight(tmp_path):
    destination = tmp_path / "bin/provenant"
    destination.parent.mkdir()
    legacy_target = tmp_path / "instance/scripts/provenant"
    destination.symlink_to(legacy_target)

    classified = invoke(
        "classify",
        "--source",
        str(SOURCE),
        "--destination",
        str(destination),
        "--legacy-target",
        str(legacy_target),
    )
    assert classified.returncode == 0, classified.stderr
    assert classified.stdout.strip() == "legacy-link"

    destination.unlink()
    foreign_target = tmp_path / "foreign/provenant"
    destination.symlink_to(foreign_target)

    published = invoke(
        "publish",
        "--source",
        str(SOURCE),
        "--destination",
        str(destination),
        "--legacy-target",
        str(legacy_target),
        "--expected",
        "legacy-link",
    )

    assert published.returncode == 3
    assert "collision" in published.stderr
    assert destination.is_symlink()
    assert destination.readlink() == foreign_target


def test_publish_restores_a_same_target_link_raced_into_atomic_exchange(
    tmp_path,
    monkeypatch,
):
    helper = load_helper()
    destination = tmp_path / "bin/provenant"
    destination.parent.mkdir()
    legacy_target = tmp_path / "instance/scripts/provenant"
    destination.symlink_to(legacy_target)
    exchange = helper._exchange
    raced = False

    def race_then_exchange(first, second):
        nonlocal raced
        if not raced:
            second.unlink()
            second.symlink_to(legacy_target)
            raced = True
        exchange(first, second)

    monkeypatch.setattr(helper, "_exchange", race_then_exchange)

    try:
        helper.publish(SOURCE, destination, legacy_target, "legacy-link")
    except helper.Collision as exc:
        assert "changed during atomic publication" in str(exc)
    else:
        raise AssertionError("foreign race was not rejected")

    assert raced
    assert destination.is_symlink()
    assert destination.readlink() == legacy_target


def test_publish_preserves_a_displaced_file_when_rollback_also_races(
    tmp_path,
    monkeypatch,
):
    helper = load_helper()
    destination = tmp_path / "bin/provenant"
    destination.parent.mkdir()
    legacy_target = tmp_path / "instance/scripts/provenant"
    destination.symlink_to(legacy_target)
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
        helper.publish(SOURCE, destination, legacy_target, "legacy-link")
    except helper.Collision as exc:
        assert "displaced path preserved=" in str(exc)
    else:
        raise AssertionError("two-step foreign race was not rejected")

    assert destination.read_bytes() == second_foreign
    recovery_paths = list(destination.parent.glob(".provenant.*"))
    assert len(recovery_paths) == 1
    assert recovery_paths[0].read_bytes() == first_foreign


def test_publish_preserves_a_displaced_file_when_rollback_raises(
    tmp_path,
    monkeypatch,
):
    helper = load_helper()
    destination = tmp_path / "bin/provenant"
    destination.parent.mkdir()
    legacy_target = tmp_path / "instance/scripts/provenant"
    destination.symlink_to(legacy_target)
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
        helper.publish(SOURCE, destination, legacy_target, "legacy-link")
    except OSError as exc:
        assert "injected rollback failure" in str(exc)
    else:
        raise AssertionError("rollback failure was not propagated")

    recovery_paths = list(destination.parent.glob(".provenant.*"))
    assert len(recovery_paths) == 1
    assert recovery_paths[0].read_bytes() == foreign


def test_snapshot_maps_a_racing_type_swap_to_a_collision(tmp_path, monkeypatch):
    helper = load_helper()
    destination = tmp_path / "bin/provenant"
    destination.parent.mkdir()
    legacy_target = tmp_path / "instance/scripts/provenant"
    destination.symlink_to(legacy_target)
    real_readlink = os.readlink

    def swapped_before_read(path, *args, **kwargs):
        if Path(path) == destination:
            raise OSError(errno.EINVAL, "Invalid argument", str(path))
        return real_readlink(path, *args, **kwargs)

    monkeypatch.setattr(helper.os, "readlink", swapped_before_read)

    try:
        helper._raw_snapshot(destination)
    except helper.Collision as exc:
        assert "changed during classification" in str(exc)
    else:
        raise AssertionError("racing type swap was not mapped to a collision")


def test_snapshot_propagates_a_non_race_read_failure(tmp_path, monkeypatch):
    helper = load_helper()
    destination = tmp_path / "bin/provenant"
    destination.parent.mkdir()
    legacy_target = tmp_path / "instance/scripts/provenant"
    destination.symlink_to(legacy_target)
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


def test_snapshot_maps_a_racing_delete_to_a_collision(tmp_path, monkeypatch):
    helper = load_helper()
    destination = tmp_path / "bin/provenant"
    destination.parent.mkdir()
    legacy_target = tmp_path / "instance/scripts/provenant"
    destination.symlink_to(legacy_target)
    real_readlink = os.readlink

    def deleted_before_read(path, *args, **kwargs):
        if Path(path) == destination:
            raise FileNotFoundError(
                errno.ENOENT, "No such file or directory", str(path)
            )
        return real_readlink(path, *args, **kwargs)

    monkeypatch.setattr(helper.os, "readlink", deleted_before_read)

    try:
        helper._raw_snapshot(destination)
    except helper.Collision as exc:
        assert "changed during classification" in str(exc)
    else:
        raise AssertionError("racing delete was not mapped to a collision")
