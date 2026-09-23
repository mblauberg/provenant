import json
from pathlib import Path
import subprocess
import re

ROOT = Path(__file__).resolve().parents[1]
INIT = ROOT / "skills/orchestrate/scripts/run_dir_init.sh"


def test_default_run_name_and_owner_directory(tmp_path):
    result = subprocess.run(
        [str(INIT), "--kind", "dispatch", "--slug", "test lane", "--owner-logs"],
        cwd=tmp_path,
        capture_output=True,
        text=True,
    )
    assert result.returncode == 0, result.stderr
    path = Path(result.stdout.strip())
    assert path.parent == tmp_path / ".agent-run/runs"
    assert re.fullmatch(r"\d{8}-\d{4}-dispatch-test-lane-[a-f0-9]{6}", path.name)
    assert (path / "_owner").is_dir()
    assert (path / "RUN_RECEIPT.json").is_file()


def test_linked_worktree_shares_run_root(tmp_path):
    primary = tmp_path / "repo"
    primary.mkdir()
    subprocess.run(["git", "init", str(primary)], check=True, capture_output=True)
    subprocess.run(
        [
            "git",
            "-C",
            str(primary),
            "-c",
            "user.name=Fixture",
            "-c",
            "user.email=fixture@example.invalid",
            "commit",
            "--allow-empty",
            "-m",
            "initial",
        ],
        check=True,
        capture_output=True,
    )
    linked = tmp_path / "lane"
    subprocess.run(
        ["git", "-C", str(primary), "worktree", "add", "-b", "lane", str(linked)],
        check=True,
        capture_output=True,
    )
    result = subprocess.run([str(INIT)], cwd=linked, capture_output=True, text=True)
    assert result.returncode == 0, result.stderr
    assert Path(result.stdout.strip()).parent == primary / ".agent-run/runs"


def test_orphan_reaping_uses_shared_root_and_leaves_live_receipts(tmp_path):
    from skills.orchestrate.scripts import layout
    import os, time

    old = tmp_path / ".agent-run/runs/20260920-0000-dispatch-orphan-abcdef"
    old.mkdir(parents=True)
    live = tmp_path / ".agent-run/mcp-live"
    live.mkdir()
    fixture = json.loads((ROOT / "tests/fixtures/fabric-v1/attempt.json").read_text())
    fixture.update(state="running", status=None, pgid=None)
    attempt = old / "tasks/task-1/attempt-001/attempt.json"
    attempt.parent.mkdir(parents=True)
    attempt.write_text(json.dumps(fixture))
    for directory, pid in [(old, 99999999), (live, os.getpid())]:
        receipt = directory / "RUN_RECEIPT.json"
        receipt.write_text(json.dumps({"status": "active", "owner_pid": pid}))
        os.utime(receipt, (time.time() - 49 * 3600,) * 2)
    assert layout.reap_orphans(tmp_path) == [str(old)]
    assert json.loads(attempt.read_text())["status"] == "interrupted"
    assert json.loads((old / "RUN_RECEIPT.json").read_text())["status"] == "interrupted"
    assert json.loads((live / "RUN_RECEIPT.json").read_text())["status"] == "active"


def test_shared_layout_vectors_and_symlinked_cwd(tmp_path, monkeypatch):
    from skills.orchestrate.scripts import layout

    vectors = json.loads(
        (ROOT / "tests/fixtures/fabric-v1/layout-cases.json").read_text()
    )
    for case in vectors:
        monkeypatch.setattr(
            layout.subprocess,
            "run",
            lambda *args, case=case, **kwargs: subprocess.CompletedProcess(
                args,
                0 if case["git_common_dir"] else 1,
                stdout=case["git_common_dir"] or "",
            ),
        )
        assert str(layout.run_root(case["cwd"])) == case["run_root"]


def test_orphan_reaping_does_not_close_a_live_unindexed_provider(tmp_path):
    from skills.orchestrate.scripts import layout
    import os, time

    run = tmp_path / ".agent-run/runs/20260920-0000-dispatch-live-abcdef"
    run.mkdir(parents=True)
    receipt = run / "RUN_RECEIPT.json"
    receipt.write_text(json.dumps({"status": "active"}))
    os.utime(receipt, (time.time() - 49 * 3600,) * 2)
    row = json.loads((ROOT / "tests/fixtures/fabric-v1/attempt.json").read_text())
    row.update(state="running", status=None, pgid=os.getpid())
    attempt = run / "tasks/task-1/attempt-001/attempt.json"
    attempt.parent.mkdir(parents=True)
    attempt.write_text(json.dumps(row))
    assert layout.reap_orphans(tmp_path) == []
    assert json.loads(receipt.read_text())["status"] == "active"


def test_nested_non_git_cwd_accepts_explicit_workspace_run(tmp_path):
    from skills.orchestrate.scripts import layout
    (tmp_path / '.agent-run/runs').mkdir(parents=True)
    nested = tmp_path / 'src/nested'
    nested.mkdir(parents=True)
    assert layout.run_root(nested) == nested
    assert layout.contains_run(tmp_path / '.agent-run/runs/one', nested)
    assert not layout.contains_run(tmp_path.parent / 'other', nested)


def test_stale_batch_with_provider_record_still_checks_every_attempt(tmp_path):
    from skills.orchestrate.scripts import layout
    import os, time
    run = tmp_path / '.agent-run/runs/stale-batch'
    run.mkdir(parents=True)
    receipt = run / 'RUN_RECEIPT.json'
    receipt.write_text(json.dumps({'status':'active'}))
    os.utime(receipt, (time.time() - 49*3600,) * 2)
    (run / 'dispatch-provider.json').write_text(json.dumps({'provider_pgid':99999999}))
    row = json.loads((ROOT / 'tests/fixtures/fabric-v1/attempt.json').read_text())
    row.update(state='running', status=None, pgid=os.getpid())
    attempt = run / 'tasks/other/attempt-001/attempt.json'
    attempt.parent.mkdir(parents=True)
    attempt.write_text(json.dumps(row))
    assert layout.reap_orphans(tmp_path) == []


def test_stray_ancestor_run_directory_does_not_relocate_new_non_git_runs(tmp_path):
    from skills.orchestrate.scripts import layout
    (tmp_path / '.agent-run').mkdir()
    workspace = tmp_path / 'workspace'
    workspace.mkdir()
    assert layout.new_run_dir(workspace).parent == workspace / '.agent-run/runs'
