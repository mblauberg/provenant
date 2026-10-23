import subprocess
import sys

from tests.support.modules import load_script

movecheck = load_script("scripts/movecheck.py")

OLD = 'LIMIT = 4\n\n\ndef keep(x):\n    return x + LIMIT\n\n\ndef stay():\n    return 1\n'


def test_reformatted_move_is_identical():
    report = movecheck.compare([OLD], '# moved\nLIMIT = 4\n\ndef keep(x):\n    return (x + LIMIT)  # same\n')
    assert report["identical"] == ["keep"]
    assert report["changed"] == [] and report["new"] == []


def test_changed_body_and_constant_are_reported():
    report = movecheck.compare([OLD], 'LIMIT = 5\n\ndef keep(x):\n    return x - LIMIT\n\ndef extra():\n    pass\n')
    assert report["changed"] == ["keep", "LIMIT"]
    assert report["new"] == ["extra"]


def test_name_still_defined_in_old_file_is_reported():
    report = movecheck.compare([OLD], 'def keep(x):\n    return x + LIMIT\n', {"old.py": OLD})
    assert report["duplicated"] == [("keep", "old.py")]


def test_cli_fails_only_on_changed_definitions(tmp_path):
    def git(*args):
        subprocess.run(["git", *args], cwd=tmp_path, check=True, capture_output=True)

    git("init", "-q")
    (tmp_path / "old.py").write_text(OLD, encoding="utf-8")
    git("add", "old.py")
    git("-c", "user.name=t", "-c", "user.email=t@example.invalid", "commit", "-qm", "base")
    (tmp_path / "old.py").write_text('def stay():\n    return 1\n', encoding="utf-8")
    new = tmp_path / "new.py"
    script = movecheck.__file__

    def run():
        return subprocess.run([sys.executable, script, "HEAD", "new.py", "old.py"], cwd=tmp_path, capture_output=True, text=True)

    new.write_text('LIMIT = 4\n\ndef keep(x):\n    return x + LIMIT\n', encoding="utf-8")
    result = run()
    assert result.returncode == 0, result.stderr
    assert "1 identical, 0 changed, 0 new" in result.stdout

    new.write_text('def keep(x):\n    return x * 2\n', encoding="utf-8")
    result = run()
    assert result.returncode == 1
    assert "CHANGED keep" in result.stdout


def test_name_defined_in_several_old_files_matches_any_of_them():
    other = 'LIMIT = 5\n'
    assert movecheck.compare([OLD, other], 'LIMIT = 4\n')["changed"] == []
    assert movecheck.compare([OLD, other], 'LIMIT = 5\n')["changed"] == []
