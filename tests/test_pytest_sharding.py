import zlib
from pathlib import Path
from types import SimpleNamespace

from tests.support.git import git
from tests.support.modules import load_script

ROOT = Path(__file__).resolve().parents[1]
conftest = load_script(ROOT / "tests" / "conftest.py", "provenant_root_tests_conftest")


def test_collection_hook_pins_fabric_hosts_and_partitions_other_files(monkeypatch) -> None:
    monkeypatch.setenv(conftest.LIVE_PROVIDER_ENV, "1")
    paths = [
        conftest.ROOT / "tests/test_fabric_hosts.py",
        conftest.ROOT / "tests/test_alpha.py",
        conftest.ROOT / "tests/test_beta.py",
    ]
    memberships: dict[Path, list[int]] = {path: [] for path in paths}

    for index in range(1, 4):
        selected = [SimpleNamespace(path=path) for path in paths]
        deselected = []
        config = SimpleNamespace(
            hook=SimpleNamespace(pytest_deselected=lambda items: deselected.extend(items))
        )
        monkeypatch.setenv("PROVENANT_TEST_SHARD", f"{index}/3")
        conftest.pytest_collection_modifyitems(config, selected)
        for item in selected:
            memberships[Path(item.path)].append(index)
        assert {Path(item.path) for item in selected + deselected} == set(paths)
        assert len(selected) + len(deselected) == len(paths)

    assert memberships[paths[0]] == [1]
    for path in paths[1:]:
        relative = path.relative_to(conftest.ROOT).as_posix()
        expected = zlib.crc32(relative.encode("utf-8")) % 2 + 2
        assert memberships[path] == [expected]


def test_temp_paths_stay_under_tmpdir_inside_a_checkout(tmp_path, monkeypatch) -> None:
    import os
    import subprocess
    import tempfile

    checkout = tmp_path / "checkout"
    checkout.mkdir()
    subprocess.run(["git", "init", "-q", str(checkout)], check=True)
    permitted = checkout / ".agent-run" / "tmp"
    permitted.mkdir(parents=True)
    monkeypatch.setenv("TMPDIR", str(permitted))
    monkeypatch.setattr(tempfile, "tempdir", None)

    conftest._keep_tmpdir_outside_git()

    selected = Path(os.environ["TMPDIR"])
    assert selected.is_relative_to(permitted)
    assert selected.is_dir()


def test_detached_checkout_temp_sources_stay_outside_git(tmp_path, monkeypatch) -> None:
    import os
    import subprocess
    import tempfile

    primary = tmp_path / "primary"
    primary.mkdir()
    subprocess.run(["git", "init", "-q", str(primary)], check=True)
    (primary / ".gitignore").write_text(".agent-run/\n.worktrees/\n")
    subprocess.run(["git", "-C", str(primary), "add", ".gitignore"], check=True)
    subprocess.run(["git", "-C", str(primary), "-c", "user.name=Fixture",
                    "-c", "user.email=fixture@example.invalid", "commit", "-qm", "base"], check=True)
    checkout = primary / ".worktrees" / "read-review"
    subprocess.run(["git", "-C", str(primary), "worktree", "add", "--detach", str(checkout)],
                   check=True, capture_output=True)
    permitted = checkout / ".agent-run" / "tmp"
    permitted.mkdir(parents=True)
    monkeypatch.setenv("TMPDIR", str(permitted))
    monkeypatch.setattr(tempfile, "tempdir", None)
    conftest._keep_tmpdir_outside_git()
    fixture = Path(os.environ["TMPDIR"]) / "frontend"
    fixture.mkdir()
    assert subprocess.run(["git", "rev-parse", "--git-dir"], cwd=fixture,
                          capture_output=True).returncode != 0
    # A fixture can still establish its own Git repository below the boundary.
    subprocess.run(["git", "init", "-q"], cwd=fixture, check=True)
    assert git(fixture, "rev-parse", "--show-toplevel", env=os.environ) == str(fixture)
