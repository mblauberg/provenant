import importlib.util
from pathlib import Path
import subprocess


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
