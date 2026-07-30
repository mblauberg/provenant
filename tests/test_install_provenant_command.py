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


def test_publish_restores_a_foreign_link_raced_into_atomic_exchange(
    tmp_path,
    monkeypatch,
):
    helper = load_helper()
    destination = tmp_path / "bin/provenant"
    destination.parent.mkdir()
    legacy_target = tmp_path / "instance/scripts/provenant"
    destination.symlink_to(legacy_target)
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
        helper.publish(SOURCE, destination, legacy_target, "legacy-link")
    except helper.Collision as exc:
        assert "changed during atomic publication" in str(exc)
    else:
        raise AssertionError("foreign race was not rejected")

    assert destination.is_symlink()
    assert destination.readlink() == foreign_target
