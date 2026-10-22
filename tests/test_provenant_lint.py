from pathlib import Path
import json
import os
import shutil
import subprocess
import sys

import pytest

ROOT = Path(__file__).resolve().parents[1]
SCRIPT = ROOT / "scripts" / "provenant_lint.py"
FIXTURE = ROOT / "tests" / "fixtures" / "lint-corpus.txt"
SEALLE = Path("/Users/user/Repos/seal/sealle-app")

sys.path.insert(0, str(ROOT / "scripts"))
import provenant_lint as lint  # noqa: E402

DEFAULTS = {"forbid_attribution": True, "forbid_closing_keywords": True, "areas": []}

# The JS source, verbatim from the Sealle tools this lint was ported from.
JS = r"""
const SESSION_PHRASES =
  /\b(?:WP\d+|(?:wave|phase|batch|lane)(?:\d+[a-z0-9-]*|\s+\d+[a-z0-9-]*|-[a-z]*\d[a-z0-9-]*)|round(?:\d+[a-z0-9-]*|-[a-z]*\d[a-z0-9-]*)|\d{8}-\d{4}-[a-z0-9-]+-[a-f0-9]{6}|[a-z]+-\d{4}-\d{2}-\d{2}-[a-z0-9-]+|(?:fabric|run)[-:/][a-f0-9]{8,}(?:-[a-f0-9]+)*)\b/i;
const CLOSING =
  /\b(?:closes?|closed|fix(?:es|ed)?|resolves?|resolved):?\s+#([1-9]\d*)\b/gi;
const lines = require("fs").readFileSync(0, "utf8").split("\n");
for (const line of lines)
  console.log(JSON.stringify([SESSION_PHRASES.test(line), [...line.matchAll(CLOSING)].map((m) => Number(m[1]))]));
"""


def run(*args, cwd, **kw):
    return subprocess.run([sys.executable, str(SCRIPT), *args], cwd=cwd, capture_output=True, text=True, **kw)


def message(tmp_path, text, name="MSG"):
    path = tmp_path / name
    path.write_bytes(text if isinstance(text, bytes) else text.encode())
    return path


def repo(tmp_path, branch="feat/repo-lint-rules"):
    subprocess.run(["git", "init", "-q", "-b", branch, str(tmp_path)], check=True)
    return tmp_path


@pytest.mark.parametrize("branch", [
    "feat/repo-lint-rules", "fix/tools-lint-hook-crash", "proto/repo-spike-idea",
    "land/20261022-weekly-train", "dependabot/npm_and_yarn/foo-1.2.3",
])
def test_valid_branches(branch):
    assert lint.branch_problems(branch, []) == []


@pytest.mark.parametrize("branch", [
    "feature/repo-lint-rules", "feat/lint", "feat/repo-lint", "feat/repo-a-b-c-d-e-f", "Feat/repo-lint-rules",
    "feat/repo-wave-3-cleanup", "fix/repo-claude-hook-fix", "feat/repo-" + "x-" * 30 + "end",
    "feat/repo-lint-rules-42x", "wip",
])
def test_invalid_branches(branch):
    assert lint.branch_problems(branch, [])


def test_areas_come_from_config():
    assert lint.branch_problems("feat/api-lint-rules", ["api"]) == []
    assert lint.branch_problems("feat/web-lint-rules", ["api"])


def test_branch_cli_current_branch(tmp_path):
    repo(tmp_path, "feat/repo-lint-rules")
    assert run("branch", cwd=tmp_path).returncode == 0
    subprocess.run(["git", "-C", str(tmp_path), "checkout", "-q", "-b", "oops"], check=True)
    result = run("branch", cwd=tmp_path)
    assert result.returncode == 1 and "oops" in result.stderr
    assert run("branch", "fix/repo-ok-name", cwd=tmp_path).returncode == 0


def test_main_and_detached_head_skip(tmp_path):
    repo(tmp_path, "main")
    subprocess.run(["git", "-C", str(tmp_path), "-c", "user.name=t", "-c", "user.email=t@t", "commit", "-q", "--allow-empty", "-m", "x"], check=True)
    main = run("branch", cwd=tmp_path)
    assert main.returncode == 0 and "skipping" in main.stdout
    subprocess.run(["git", "-C", str(tmp_path), "checkout", "-q", "--detach"], check=True)
    detached = run("branch", cwd=tmp_path)
    assert detached.returncode == 0 and "detached" in detached.stdout


def test_default_branch_config(tmp_path):
    repo(tmp_path, "trunk")
    assert run("branch", cwd=tmp_path).returncode == 1
    (tmp_path / ".agents").mkdir()
    (tmp_path / ".agents/fabric-policy.json").write_text(json.dumps({"tools": {"default_branch": "trunk"}}))
    assert run("branch", cwd=tmp_path).returncode == 0


@pytest.mark.parametrize("text", [
    "feat: add the thing\n\nBody text.\n",
    "Merge branch 'main' into feat/x\n",
    "Merge pull request #1086 from mblauberg/docs/memory-fold\n\nWave 12 closes #4\n",
    'Revert "Wave 12 cleanup"\n\nThis reverts commit abc.\nCloses #4\n',
    "fix: thing\n\nRefs #12\n# Please enter the commit message\n# wave 12\n",
])
def test_clean_messages_pass(tmp_path, text):
    assert run("commit-msg", str(message(tmp_path, text)), cwd=tmp_path).returncode == 0


@pytest.mark.parametrize("text, needle", [
    ("fix: thing\n\nCo-Authored-By: A <a@b.c>\n", "attribution"),
    ("fix: thing\n\nGenerated with [Claude Code](x)\n", "attribution"),
    ("fix: wave 12 cleanup\n", "session marker"),
    ("fix: thing\n\nCloses #12\n", "closing keyword"),
    ("fix: thing\r\n\r\nfixes #3\r\n", "closing keyword"),
])
def test_bad_messages_fail(tmp_path, text, needle):
    result = run("commit-msg", str(message(tmp_path, text)), cwd=tmp_path)
    assert result.returncode == 1 and needle in result.stderr


@pytest.mark.parametrize("text", [b"", b"\n\n", b"# only\n# comments\n", b"   \n# c\n"])
def test_empty_or_comment_only_fails_clearly(tmp_path, text):
    result = run("commit-msg", str(message(tmp_path, text)), cwd=tmp_path)
    assert result.returncode == 1 and "empty commit message" in result.stderr


def test_scissors_tail_and_html_comments_are_ignored(tmp_path):
    text = "fix: thing\n<!-- wave 12 -->\n# ------------------------ >8 ------------------------\ndiff wave 12\n"
    assert run("commit-msg", str(message(tmp_path, text)), cwd=tmp_path).returncode == 0


def test_bad_utf8_does_not_crash(tmp_path):
    result = run("commit-msg", str(message(tmp_path, b"fix: caf\xe9 \xff\n")), cwd=tmp_path)
    assert result.returncode == 0


def test_missing_message_file_is_usage_error(tmp_path):
    assert run("commit-msg", str(tmp_path / "nope"), cwd=tmp_path).returncode == 2
    assert run(cwd=tmp_path).returncode == 2
    assert run("pr", cwd=tmp_path).returncode == 2


def test_custom_comment_char_and_verbatim(tmp_path):
    repo(tmp_path)
    subprocess.run(["git", "-C", str(tmp_path), "config", "core.commentChar", ";"], check=True)
    path = message(tmp_path, "fix: thing\n; wave 12\n")
    assert run("commit-msg", str(path), cwd=tmp_path).returncode == 0
    subprocess.run(["git", "-C", str(tmp_path), "config", "commit.cleanup", "verbatim"], check=True)
    path = message(tmp_path, "fix: thing\n; wave 12\n")
    assert run("commit-msg", str(path), cwd=tmp_path).returncode == 1


def test_config_switches_rules_off(tmp_path):
    repo(tmp_path)
    (tmp_path / ".agents").mkdir()
    (tmp_path / ".agents/fabric-policy.json").write_text(json.dumps(
        {"tools": {"lint": {"forbid_attribution": False, "forbid_closing_keywords": False, "unknown": 1}, "other": {}}}
    ))
    path = message(tmp_path, "fix: thing\n\nCo-Authored-By: A <a@b.c>\nCloses #3\n")
    assert run("commit-msg", str(path), cwd=tmp_path).returncode == 0


@pytest.mark.parametrize("content", ["{not json", "[]", '{"tools": 3}', '{"tools": {"lint": {"areas": "x", "forbid_attribution": "no"}}}', ""])
def test_malformed_policy_falls_back_to_defaults(tmp_path, content):
    repo(tmp_path)
    (tmp_path / ".agents").mkdir()
    (tmp_path / ".agents/fabric-policy.json").write_text(content)
    path = message(tmp_path, "fix: thing\n\nCo-Authored-By: A <a@b.c>\n")
    result = run("commit-msg", str(path), cwd=tmp_path)
    assert result.returncode == 1 and "attribution" in result.stderr
    assert run("branch", "feat/repo-lint-rules", cwd=tmp_path).returncode == 0


def test_tools_policy_defaults_and_unknown_keys(tmp_path):
    tools, warnings = lint.fabric_policy.tools_policy(tmp_path)
    assert tools == {"default_branch": "main", "lint": DEFAULTS} and warnings == []
    (tmp_path / ".agents").mkdir()
    (tmp_path / ".agents/fabric-policy.json").write_text(json.dumps({"tools": {"extra": 1, "lint": {"areas": ["api"], "x": 2}}}))
    tools, warnings = lint.fabric_policy.tools_policy(tmp_path)
    assert tools["extra"] == 1 and tools["lint"]["areas"] == ["api"] and tools["lint"]["x"] == 2 and warnings == []


def test_works_as_commit_msg_hook(tmp_path):
    repo(tmp_path)
    hook = tmp_path / ".git/hooks/commit-msg"
    hook.write_text(f'#!/bin/sh\nexec "{sys.executable}" "{SCRIPT}" commit-msg "$1"\n')
    hook.chmod(0o755)
    env = {**os.environ, "GIT_AUTHOR_NAME": "t", "GIT_AUTHOR_EMAIL": "t@t", "GIT_COMMITTER_NAME": "t", "GIT_COMMITTER_EMAIL": "t@t"}
    bad = subprocess.run(["git", "commit", "-q", "--allow-empty", "-m", "wave 12 stuff"], cwd=tmp_path, env=env, capture_output=True, text=True)
    assert bad.returncode != 0 and "session marker" in bad.stderr
    good = subprocess.run(["git", "commit", "-q", "--allow-empty", "-m", "feat: stuff"], cwd=tmp_path, env=env, capture_output=True, text=True)
    assert good.returncode == 0


# JS to Python regex equivalence.

def js_results(lines):
    out = subprocess.run(["node", "-e", JS], input="\n".join(lines), capture_output=True, text=True, check=True)
    return [json.loads(row) for row in out.stdout.splitlines()]


def python_results(lines):
    return [[bool(lint.SESSION_PHRASES.search(line)), [int(m.group(1)) for m in lint.CLOSING_REFERENCE.finditer(line)]] for line in lines]


def assert_equivalent(lines):
    expected, actual = js_results(lines), python_results(lines)
    mismatches = [(line, e, a) for line, e, a in zip(lines, expected, actual) if e != a]
    assert not mismatches, mismatches[:5]


needs_node = pytest.mark.skipif(shutil.which("node") is None, reason="node is not installed")


@needs_node
def test_synthetic_corpus_matches_js_semantics():
    lines = FIXTURE.read_text().split("\n")
    lines += ["wave 12", "phase　 5", "ＷＡＶＥ 12", "round٤", "batch١", "İ wave 2", "run-ABCDEF0123", "fix #5"]
    assert_equivalent(lines)


@needs_node
@pytest.mark.skipif(not (SEALLE / ".git").exists(), reason="Sealle checkout is not present")
def test_sealle_history_matches_js_semantics():
    log = subprocess.run(["git", "-C", str(SEALLE), "log", "-n", "2000", "--format=%B"], capture_output=True, text=True, check=True)
    lines = log.stdout.replace("\r\n", "\n").split("\n")
    assert len(lines) > 1000
    assert_equivalent(lines)
