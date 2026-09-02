import os
import re
import shlex
import shutil
import subprocess
import sys
from pathlib import Path

import pytest

from scripts import public_release_check as release_check
from scripts.public_release_check import publication_range_errors, scan_paths, tracked_files


def seed_required(root: Path) -> None:
    for relative in release_check.REQUIRED:
        path = root / relative
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_text("safe\n")


def copy_release_scripts(repository: Path) -> Path:
    source = Path(__file__).resolve().parents[1] / "scripts"
    target = repository / "scripts"
    target.mkdir()
    for name in ("public_release_check.py", "git_evidence.py"):
        shutil.copy2(source / name, target / name)
    return target / "public_release_check.py"


def git_at(root: Path, *args: str) -> str:
    result = subprocess.run(
        ["git", *args],
        cwd=root,
        text=True,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        check=False,
    )
    assert result.returncode == 0, result.stderr
    return result.stdout.strip()


def publication_repository(
    tmp_path: Path, object_format: str | None = None,
) -> tuple[Path, Path, str]:
    repository = tmp_path / "publication"
    repository.mkdir(parents=True)
    init_args = ["init", "-q"]
    if object_format is not None:
        init_args.append(f"--object-format={object_format}")
    git_at(repository, *init_args)
    git_at(repository, "config", "user.name", "Release Test")
    git_at(repository, "config", "user.email", "release@example.test")
    seed_required(repository)
    script = copy_release_scripts(repository)
    git_at(repository, "add", ".")
    git_at(repository, "commit", "-q", "-m", "base")
    return repository, script, git_at(repository, "rev-parse", "HEAD")


def run_publication_range(
    repository: Path, script: Path, base: str, head: str,
    environment: dict[str, str] | None = None,
) -> subprocess.CompletedProcess[str]:
    return subprocess.run(
        [sys.executable, str(script), "--publication-range", base, head],
        cwd=repository,
        env=None if environment is None else {**os.environ, **environment},
        text=True,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        check=False,
    )


def run_history(
    repository: Path, script: Path, environment: dict[str, str] | None = None,
) -> subprocess.CompletedProcess[str]:
    return subprocess.run(
        [sys.executable, str(script), "--history"],
        cwd=repository,
        env=None if environment is None else {**os.environ, **environment},
        text=True,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        check=False,
    )


def tainted_selected_and_clean_decoy(
    repository: Path, base: str,
) -> tuple[str, str]:
    git_at(repository, "switch", "-q", "-c", "selected")
    receipt = repository / ".agent-run" / "DEL-999" / "RUN.json"
    receipt.parent.mkdir(parents=True)
    receipt.write_text('{"contract":"delivery-run"}\n')
    (repository / "leak.txt").write_text(
        "/" + "Users/alice/private\n"
        "github" + "_pat_abcdefghijklmnopqrstuvwxyz123456\n"
    )
    git_at(repository, "add", ".agent-run/DEL-999/RUN.json", "leak.txt")
    git_at(repository, "commit", "-q", "-m", "tainted selected commit")
    tainted = git_at(repository, "rev-parse", "HEAD")

    git_at(repository, "switch", "-q", "-c", "clean-decoy", base)
    (repository / "decoy.txt").write_text("clean decoy\n")
    git_at(repository, "add", "decoy.txt")
    git_at(repository, "commit", "-q", "-m", "clean decoy commit")
    return tainted, git_at(repository, "rev-parse", "HEAD")


def hidden_linear_taint(repository: Path) -> tuple[str, str]:
    git_at(repository, "switch", "-q", "-c", "selected")
    receipt = repository / ".agent-run" / "DEL-999" / "RUN.json"
    receipt.parent.mkdir(parents=True)
    receipt.write_text('{"contract":"delivery-run"}\n')
    git_at(repository, "add", ".agent-run/DEL-999/RUN.json")
    git_at(repository, "commit", "-q", "-m", "tainted middle")
    middle = git_at(repository, "rev-parse", "HEAD")
    git_at(repository, "rm", "-q", ".agent-run/DEL-999/RUN.json")
    (repository / "head.txt").write_text("clean head\n")
    git_at(repository, "add", "head.txt")
    git_at(repository, "commit", "-q", "-m", "clean selected head")
    return middle, git_at(repository, "rev-parse", "HEAD")


def hidden_merge_taint(repository: Path, base: str) -> tuple[str, str, str]:
    git_at(repository, "switch", "-q", "-c", "tainted-side")
    receipt = repository / ".agent-run" / "DEL-999" / "RUN.json"
    receipt.parent.mkdir(parents=True)
    receipt.write_text('{"contract":"delivery-run"}\n')
    git_at(repository, "add", ".agent-run/DEL-999/RUN.json")
    git_at(repository, "commit", "-q", "-m", "tainted side commit")
    tainted = git_at(repository, "rev-parse", "HEAD")
    git_at(repository, "rm", "-q", ".agent-run/DEL-999/RUN.json")
    git_at(repository, "commit", "-q", "-m", "clean side tip")
    side_tip = git_at(repository, "rev-parse", "HEAD")

    git_at(repository, "switch", "-q", "-c", "selected", base)
    (repository / "main.txt").write_text("clean main\n")
    git_at(repository, "add", "main.txt")
    git_at(repository, "commit", "-q", "-m", "clean main commit")
    git_at(repository, "merge", "-q", "--no-ff", "tainted-side", "-m", "clean merge")
    return tainted, side_tip, git_at(repository, "rev-parse", "HEAD")


def loose_object_path(repository: Path, object_id: str) -> Path:
    git_directory = Path(git_at(repository, "rev-parse", "--git-dir"))
    if not git_directory.is_absolute():
        git_directory = repository / git_directory
    return git_directory / "objects" / object_id[:2] / object_id[2:]


def substitute_loose_object(repository: Path, target: str, source: str) -> None:
    target_path = loose_object_path(repository, target)
    source_path = loose_object_path(repository, source)
    assert target_path.is_file()
    assert source_path.is_file()
    target_path.chmod(0o600)
    shutil.copy2(source_path, target_path)


def write_raw_object(repository: Path, kind: str, body: bytes) -> str:
    result = subprocess.run(
        ["git", "hash-object", "-w", "-t", kind, "--stdin"],
        cwd=repository,
        input=body,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        check=False,
    )
    assert result.returncode == 0, result.stderr.decode(errors="replace")
    return result.stdout.decode("ascii").strip()


def chunk_crossing_payload(chunk_size: int, *patterns: bytes) -> bytes:
    payload = bytearray()
    for pattern in patterns:
        padding = (chunk_size - 2 - len(payload) % chunk_size) % chunk_size
        payload.extend(b"." * padding)
        payload.extend(pattern)
        payload.extend(b"\n")
    return bytes(payload)


@pytest.mark.parametrize("object_format", ["sha1", "sha256"])
def test_history_rejects_personal_annotated_tag_tagger_email(
    tmp_path, object_format,
):
    repository, _, base = publication_repository(tmp_path, object_format)
    tag = write_raw_object(
        repository,
        "tag",
        (
            f"object {base}\ntype commit\ntag personal-tagger\n".encode("ascii")
            + b"tagger Release Test <alice@gmail.com> 1700000000 +0000\n\n"
            + b"portable tag message\n"
        ),
    )
    git_at(repository, "update-ref", "refs/tags/personal-tagger", tag)

    errors = release_check.history_errors(repository)

    assert "reachable history exposes a personal email: alice@gmail.com" in errors


def test_public_scan_rejects_private_paths_secrets_and_unlicensed_skill(tmp_path):
    seed_required(tmp_path)
    private = tmp_path / "notes.md"
    private.write_text("/" + "Users/alice/secret/file\n")
    token = tmp_path / "token.txt"
    token.write_text("github" + "_pat_abcdefghijklmnopqrstuvwxyz123456\n")
    errors = scan_paths(
        [
            *release_check.REQUIRED,
            "notes.md",
            "token.txt",
            "skills/academic-writing/SKILL.md",
            "skills/clean-writing/SKILL.md",
            "skills/humanise-text/SKILL.md",
            "skills/tanstack-query-best-practices/SKILL.md",
            "skills/vercel-react-best-practices/SKILL.md",
            "skills/playwright/SKILL.md",
            "skills/react-performance/SKILL.md",
            "skills/tanstack-query/SKILL.md",
            "skills/typescript-clean-code/SKILL.md",
            "skills/uml-diagrams/SKILL.md",
            "skills/web-stack-conventions/SKILL.md",
        ],
        tmp_path,
    )
    assert any("personal absolute home path" in error for error in errors)
    assert any("possible GitHub token" in error for error in errors)
    assert sum("forbidden tracked path" in error for error in errors) == 11


def test_public_scan_accepts_portable_text_tree(tmp_path):
    seed_required(tmp_path)
    (tmp_path / "safe.md").write_text("Use ${PROJECT_ROOT:-$PWD}.\n")
    assert scan_paths([*release_check.REQUIRED, "safe.md"], tmp_path) == []


def test_public_scan_rejects_untracked_required_legal_file(tmp_path):
    seed_required(tmp_path)
    tracked = release_check.REQUIRED - {"NOTICE"}

    assert scan_paths(list(tracked), tmp_path) == [
        "required public file is not tracked: NOTICE"
    ]


def test_public_scan_rejects_delivery_receipt_when_tracked(tmp_path):
    seed_required(tmp_path)
    receipt = tmp_path / ".agent-run" / "DEL-999" / "RUN.json"
    receipt.parent.mkdir(parents=True)
    receipt.write_text(
        '{"schema_version":1,"contract":"delivery-run","run_id":"DEL-999"}\n'
    )

    assert scan_paths([
        *release_check.REQUIRED,
        ".agent-run/DEL-999/RUN.json",
    ], tmp_path) == [
        "forbidden tracked path: .agent-run/DEL-999/RUN.json"
    ]


def test_repository_tracks_no_private_agent_runs():
    assert [path for path in tracked_files() if path.startswith(".agent-run/")] == []


def test_publication_range_ignores_tainted_sibling_but_scans_selected_commits(tmp_path):
    repository = tmp_path / "publication"
    repository.mkdir()
    git_at(repository, "init", "-q")
    git_at(repository, "config", "user.name", "Release Test")
    git_at(repository, "config", "user.email", "release@example.test")
    seed_required(repository)
    script = copy_release_scripts(repository)
    git_at(repository, "add", ".")
    git_at(repository, "commit", "-q", "-m", "base")
    base = git_at(repository, "rev-parse", "HEAD")

    (repository / "safe.md").write_text("portable\n")
    git_at(repository, "add", "safe.md")
    git_at(repository, "commit", "-q", "-m", "clean publication change")
    clean_head = git_at(repository, "rev-parse", "HEAD")

    git_at(repository, "switch", "-q", "-c", "tainted", base)
    git_at(repository, "config", "user.email", "alice@gmail.com")
    (repository / "leak.txt").write_text(
        "/" + "Users/alice/private\n"
        "github" + "_pat_abcdefghijklmnopqrstuvwxyz123456\n"
    )
    git_at(repository, "add", "leak.txt")
    git_at(repository, "commit", "-q", "-m", "tainted sibling")
    tainted_head = git_at(repository, "rev-parse", "HEAD")
    git_at(repository, "switch", "-q", "--detach", clean_head)

    clean = subprocess.run(
        [sys.executable, str(script), "--publication-range", base, clean_head],
        cwd=repository,
        text=True,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        check=False,
    )
    assert clean.returncode == 0, clean.stderr
    assert "publication range" in clean.stdout

    tainted = subprocess.run(
        [sys.executable, str(script), "--publication-range", base, tainted_head],
        cwd=repository,
        text=True,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        check=False,
    )
    assert tainted.returncode == 1
    assert "publication range contains a personal absolute home path" in tainted.stderr
    assert "publication range contains a possible GitHub token" in tainted.stderr
    assert "publication range exposes a personal email: alice@gmail.com" in tainted.stderr

    assert publication_range_errors(clean_head, clean_head, repository) == [
        "publication range must contain at least one commit"
    ]
    assert publication_range_errors(clean_head, tainted_head, repository) == [
        "publication range base is not an ancestor of head"
    ]
    invalid = publication_range_errors("--all", clean_head, repository)
    assert len(invalid) == 1
    assert invalid[0].startswith("cannot resolve publication endpoint '--all':")

    all_refs = subprocess.run(
        [sys.executable, str(script), "--history"],
        cwd=repository,
        text=True,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        check=False,
    )
    assert all_refs.returncode == 1
    assert "reachable history contains a personal absolute home path" in all_refs.stderr
    assert "reachable history exposes a personal email: alice@gmail.com" in all_refs.stderr


def test_publication_gates_reject_a_clean_author_with_a_personal_committer(tmp_path):
    repository, script, base = publication_repository(tmp_path)
    (repository / "safe.md").write_text("portable\n")
    git_at(repository, "add", "safe.md")
    git_at(
        repository,
        "-c", "user.email=alice@gmail.com",
        "commit", "-q", "-m", "replayed publication commit",
        "--author=Contained Candidate <candidate@example.invalid>",
    )
    head = git_at(repository, "rev-parse", "HEAD")
    assert git_at(repository, "show", "-s", "--format=%ae %ce", head) == (
        "candidate@example.invalid alice@gmail.com"
    )

    selected = run_publication_range(repository, script, base, head)

    assert selected.returncode == 1, selected.stdout
    assert (
        "publication range exposes a personal email: alice@gmail.com"
        in selected.stderr
    )

    history = run_history(repository, script)

    assert history.returncode == 1, history.stdout
    assert (
        "reachable history exposes a personal email: alice@gmail.com"
        in history.stderr
    )


def test_publication_range_ignores_tainted_checked_out_sibling(tmp_path):
    repository, script, base = publication_repository(tmp_path)
    git_at(repository, "switch", "-q", "-c", "selected")
    (repository / "safe.md").write_text("portable selected change\n")
    git_at(repository, "add", "safe.md")
    git_at(repository, "commit", "-q", "-m", "clean selected change")
    selected_head = git_at(repository, "rev-parse", "HEAD")

    git_at(repository, "switch", "-q", "-c", "tainted-checkout", base)
    receipt = repository / ".agent-run" / "DEL-999" / "RUN.json"
    receipt.parent.mkdir(parents=True)
    receipt.write_text('{"contract":"delivery-run"}\n')
    (repository / "leak.txt").write_text(
        "/" + "Users/alice/private\n"
        "github" + "_pat_abcdefghijklmnopqrstuvwxyz123456\n"
    )
    (repository / "LICENSE").unlink()
    git_at(repository, "add", "-A")
    git_at(repository, "commit", "-q", "-m", "tainted checked-out sibling")

    result = run_publication_range(repository, script, base, selected_head)

    assert result.returncode == 0, result.stderr
    assert result.stdout == "PASS: publication range clean\n"


def test_raw_range_and_history_ignore_default_replacement_objects(tmp_path):
    repository, script, base = publication_repository(tmp_path)
    tainted, clean = tainted_selected_and_clean_decoy(repository, base)
    git_at(repository, "replace", tainted, clean)

    selected = run_publication_range(repository, script, base, tainted)
    history = run_history(repository, script)

    assert selected.returncode == 1
    assert (
        "publication HEAD forbidden tracked path: .agent-run/DEL-999/RUN.json"
    ) in selected.stderr
    assert history.returncode == 1
    assert (
        "reachable history contains a forbidden tracked path: "
        ".agent-run/DEL-999/RUN.json"
    ) in history.stderr


def test_raw_range_and_history_ignore_alternate_replacement_namespace(tmp_path):
    repository, script, base = publication_repository(tmp_path)
    tainted, clean = tainted_selected_and_clean_decoy(repository, base)
    git_at(repository, "update-ref", f"refs/custom-replace/{tainted}", clean)
    environment = {"GIT_REPLACE_REF_BASE": "refs/custom-replace/"}

    selected = run_publication_range(repository, script, base, tainted, environment)
    history = run_history(repository, script, environment)

    assert selected.returncode == 1
    assert (
        "publication HEAD forbidden tracked path: .agent-run/DEL-999/RUN.json"
    ) in selected.stderr
    assert history.returncode == 1
    assert (
        "reachable history contains a forbidden tracked path: "
        ".agent-run/DEL-999/RUN.json"
    ) in history.stderr


def test_history_validates_raw_head_required_files_over_worktree_decoy(tmp_path):
    repository, script, _ = publication_repository(tmp_path)
    git_at(repository, "rm", "-q", "LICENSE")
    git_at(repository, "commit", "-q", "-m", "remove required licence")
    (repository / "LICENSE").write_text("untracked decoy licence\n")

    history = run_history(repository, script)

    assert history.returncode == 1
    assert "publication HEAD missing required public file: LICENSE" in history.stderr


def test_history_validates_raw_head_symlink_and_size_over_index_decoy(tmp_path):
    repository, script, _ = publication_repository(tmp_path)
    (repository / "portable-link").symlink_to("README.md")
    (repository / "oversized.bin").write_bytes(b"x" * (5 * 1024 * 1024 + 1))
    git_at(repository, "add", "portable-link", "oversized.bin")
    git_at(repository, "commit", "-q", "-m", "non-portable raw head")
    git_at(repository, "rm", "-q", "portable-link", "oversized.bin")

    history = run_history(repository, script)

    assert history.returncode == 1
    assert "publication HEAD tracked symlink is not portable: portable-link" in history.stderr
    assert "publication HEAD tracked file exceeds 5 MiB: oversized.bin" in history.stderr


def test_evidence_git_environment_removes_inherited_redirections():
    inherited = {
        "PATH": "/trusted/bin",
        "HOME": "/trusted/home",
        "TMPDIR": "/trusted/tmp",
        "GIT_DIR": "/redirected/repo",
        "GIT_COMMON_DIR": "/redirected/common",
        "GIT_INDEX_FILE": "/redirected/index",
        "GIT_REPLACE_REF_BASE": "refs/custom-replace/",
        "GIT_OBJECT_DIRECTORY": "/redirected/objects",
        "GIT_ALTERNATE_OBJECT_DIRECTORIES": "/redirected/alternate",
        "GIT_SHALLOW_FILE": "/redirected/shallow",
        "GIT_CONFIG_COUNT": "1",
        "GIT_CONFIG_KEY_0": "core.useReplaceRefs",
        "GIT_CONFIG_VALUE_0": "true",
    }

    environment = release_check.sanitized_git_environment(inherited)

    for name in (
        "GIT_DIR", "GIT_COMMON_DIR", "GIT_INDEX_FILE", "GIT_REPLACE_REF_BASE",
        "GIT_OBJECT_DIRECTORY", "GIT_ALTERNATE_OBJECT_DIRECTORIES",
        "GIT_SHALLOW_FILE", "GIT_CONFIG_COUNT", "GIT_CONFIG_KEY_0",
        "GIT_CONFIG_VALUE_0",
    ):
        assert name not in environment
    assert environment["GIT_NO_REPLACE_OBJECTS"] == "1"
    assert environment["GIT_CONFIG_NOSYSTEM"] == "1"
    assert environment["GIT_CONFIG_GLOBAL"] == os.devnull
    assert environment["GIT_CONFIG_SYSTEM"] == os.devnull
    assert environment["GIT_GRAFT_FILE"] == os.devnull
    assert environment["GIT_NO_LAZY_FETCH"] == "1"
    assert environment["GIT_OPTIONAL_LOCKS"] == "0"
    assert environment["GIT_TERMINAL_PROMPT"] == "0"


@pytest.mark.parametrize("config_source", ["direct", "included", "worktree"])
def test_history_disables_repository_fsmonitor_hooks(tmp_path, config_source):
    repository, script, _ = publication_repository(tmp_path)
    marker = tmp_path / f"{config_source}-fsmonitor-ran"
    hook = tmp_path / f"{config_source}-fsmonitor.sh"
    hook.write_text(
        "#!/bin/sh\n"
        f": > {shlex.quote(str(marker))}\n"
    )
    hook.chmod(0o700)
    if config_source == "direct":
        git_at(repository, "config", "core.fsmonitor", str(hook))
    elif config_source == "included":
        included = tmp_path / "included-config"
        included.write_text(f"[core]\n\tfsmonitor = {hook}\n")
        git_at(repository, "config", "include.path", str(included))
    else:
        git_at(repository, "config", "extensions.worktreeConfig", "true")
        git_at(repository, "config", "--worktree", "core.fsmonitor", str(hook))
    git_at(repository, "status", "--porcelain")
    assert marker.is_file()
    marker.unlink()

    history = run_history(repository, script)

    assert history.returncode == 0, history.stderr
    assert not marker.exists()


def test_range_passes_from_clean_linked_worktree(tmp_path):
    repository, _, base = publication_repository(tmp_path / "source")
    (repository / "safe.md").write_text("clean linked worktree change\n")
    git_at(repository, "add", "safe.md")
    git_at(repository, "commit", "-q", "-m", "clean linked worktree change")
    head = git_at(repository, "rev-parse", "HEAD")
    linked = tmp_path / "linked"
    git_at(repository, "worktree", "add", "-q", "--detach", str(linked), head)

    selected = run_publication_range(
        linked, linked / "scripts" / "public_release_check.py", base, head,
    )

    assert selected.returncode == 0, selected.stderr
    assert selected.stdout == "PASS: publication range clean\n"


def test_range_passes_with_repository_native_object_alternate(tmp_path):
    source, _, base = publication_repository(tmp_path / "source")
    (source / "safe.md").write_text("clean alternate-backed change\n")
    git_at(source, "add", "safe.md")
    git_at(source, "commit", "-q", "-m", "clean alternate-backed change")
    head = git_at(source, "rev-parse", "HEAD")
    alternate = tmp_path / "alternate"
    subprocess.run(
        ["git", "clone", "-q", "--shared", str(source), str(alternate)],
        check=True,
    )
    assert (alternate / ".git" / "objects" / "info" / "alternates").read_text().strip()

    selected = run_publication_range(
        alternate, alternate / "scripts" / "public_release_check.py", base, head,
    )

    assert selected.returncode == 0, selected.stderr
    assert selected.stdout == "PASS: publication range clean\n"


def test_range_and_history_pass_with_packed_object_storage(tmp_path):
    repository, script, base = publication_repository(tmp_path)
    (repository / "safe.md").write_text("packed object evidence\n")
    git_at(repository, "add", "safe.md")
    git_at(repository, "commit", "-q", "-m", "packed object evidence")
    head = git_at(repository, "rev-parse", "HEAD")
    git_at(repository, "gc", "--prune=now")
    assert not loose_object_path(repository, head).exists()
    assert list((repository / ".git" / "objects" / "pack").glob("*.pack"))
    assert list((repository / ".git" / "objects" / "pack").glob("*.idx"))

    selected = run_publication_range(repository, script, base, head)
    history = run_history(repository, script)

    assert selected.returncode == 0, selected.stderr
    assert history.returncode == 0, history.stderr


def test_publication_range_rejects_receipt_added_then_deleted_on_selected_branch(tmp_path):
    repository, script, base = publication_repository(tmp_path)
    git_at(repository, "switch", "-q", "-c", "selected")
    receipt = repository / ".agent-run" / "DEL-999" / "RUN.json"
    receipt.parent.mkdir(parents=True)
    receipt.write_text(
        '{"schema_version":1,"contract":"delivery-run","run_id":"DEL-999"}\n'
    )
    git_at(repository, "add", ".agent-run/DEL-999/RUN.json")
    git_at(repository, "commit", "-q", "-m", "add private receipt")
    git_at(repository, "rm", "-q", ".agent-run/DEL-999/RUN.json")
    git_at(repository, "commit", "-q", "-m", "delete private receipt")
    selected_head = git_at(repository, "rev-parse", "HEAD")
    git_at(repository, "switch", "-q", "-c", "clean-sibling", base)

    result = run_publication_range(repository, script, base, selected_head)

    assert result.returncode == 1
    assert (
        "publication range contains a forbidden tracked path: "
        ".agent-run/DEL-999/RUN.json"
    ) in result.stderr


def test_publication_range_validates_selected_head_tree_not_checked_out_sibling(tmp_path):
    repository, script, base = publication_repository(tmp_path)
    git_at(repository, "switch", "-q", "-c", "selected")
    git_at(repository, "rm", "-q", "LICENSE")
    git_at(repository, "commit", "-q", "-m", "remove required licence")
    selected_head = git_at(repository, "rev-parse", "HEAD")
    git_at(repository, "switch", "-q", "-c", "clean-sibling", base)

    result = run_publication_range(repository, script, base, selected_head)

    assert result.returncode == 1
    assert "publication HEAD missing required public file: LICENSE" in result.stderr


def test_publication_range_rejects_selected_head_symlink_and_oversized_file(tmp_path):
    repository, script, base = publication_repository(tmp_path)
    git_at(repository, "switch", "-q", "-c", "selected")
    (repository / "portable-link").symlink_to("README.md")
    (repository / "oversized.bin").write_bytes(b"x" * (5 * 1024 * 1024 + 1))
    git_at(repository, "add", "portable-link", "oversized.bin")
    git_at(repository, "commit", "-q", "-m", "add non-portable tree entries")
    selected_head = git_at(repository, "rev-parse", "HEAD")
    git_at(repository, "switch", "-q", "-c", "clean-sibling", base)

    result = run_publication_range(repository, script, base, selected_head)

    assert result.returncode == 1
    assert "publication HEAD tracked symlink is not portable: portable-link" in result.stderr
    assert "publication HEAD tracked file exceeds 5 MiB: oversized.bin" in result.stderr


def test_publication_range_scans_full_selected_commit_messages(tmp_path):
    repository, script, base = publication_repository(tmp_path)
    git_at(repository, "switch", "-q", "-c", "selected")
    body = (
        "metadata must not expose /" + "Users/alice/private\n"
        "or github" + "_pat_abcdefghijklmnopqrstuvwxyz123456"
    )
    git_at(
        repository, "commit", "-q", "--allow-empty", "-m", "metadata leak",
        "-m", body,
    )
    selected_head = git_at(repository, "rev-parse", "HEAD")
    git_at(repository, "switch", "-q", "-c", "clean-sibling", base)

    result = run_publication_range(repository, script, base, selected_head)

    assert result.returncode == 1
    assert (
        "publication range commit message contains a personal absolute home path"
    ) in result.stderr
    assert (
        "publication range commit message contains a possible GitHub token"
    ) in result.stderr


def test_public_tree_retains_ui_ux_pro_max_attribution():
    # Epic #124 Workstream E: third-party licence texts are centralised in the
    # top-level LICENSES/ directory (not beside each skill), and the prose
    # provenance index lives in THIRD_PARTY_NOTICES.md.
    root = Path(__file__).resolve().parents[1]
    licence = root / "LICENSES/ui-ux-pro-max-MIT.txt"
    repository_notice = (root / "THIRD_PARTY_NOTICES.md").read_text()
    assert licence.is_file()
    assert "Copyright (c) 2024 Next Level Builder" in licence.read_text()
    assert "UI UX Pro Max v2.0.0" in repository_notice
    assert "ui-ux-pro-max-MIT.txt" in repository_notice


def history_fixture(root: Path) -> None:
    """Seed a repository whose secret survives only in unreachable history."""
    git_at(root, "init", "-q", "-b", "main")
    git_at(root, "config", "user.name", "Fixture Author")
    git_at(root, "config", "user.email", "fixture@example.invalid")
    (root / "README.md").write_text("clean start\n")
    git_at(root, "add", "-A")
    git_at(root, "commit", "-q", "-m", "first commit")
    (root / "leak.txt").write_text(
        "token = gh" + "p_" + "A" * 36 + "\n"
        "path = /" + "Users/someone/secret/file\n"
    )
    git_at(root, "add", "-A")
    git_at(root, "commit", "-q", "-m", "add credentials by mistake")
    (root / ".DS_Store").write_text("junk\n")
    git_at(root, "add", "-f", ".DS_Store")
    git_at(root, "commit", "-q", "-m", "add a forbidden path")
    (root / "leak.txt").unlink()
    (root / ".DS_Store").unlink()
    git_at(root, "rm", "-q", "--cached", "leak.txt", ".DS_Store")
    git_at(root, "commit", "-q", "-m", "remove the leaked credentials")
    (root / "notes.md").write_text("nothing here\n")
    git_at(root, "add", "-A")
    git_at(root, "commit", "-q", "-m", "cleanup; old key was AKIA" + "ABCDEFGHIJKLMNOP")


# Captured by running the deleted raw-git object parser over `history_fixture`
# before it was removed. It is the oracle for the `git grep` replacement: the
# history findings and the range findings must both still be exactly these.
HISTORY_ORACLE = (
    "reachable history contains a forbidden tracked path: .DS_Store",
    "reachable history contains a personal absolute home path",
    "reachable history contains a possible GitHub token",
    "reachable history commit message contains a possible AWS access key",
)
RANGE_ORACLE = (
    "publication range contains a forbidden tracked path: .DS_Store",
    "publication range contains a personal absolute home path",
    "publication range contains a possible GitHub token",
    "publication range commit message contains a possible AWS access key",
)


def test_history_scan_reproduces_the_removed_parser_hits(tmp_path):
    repository = tmp_path / "fixture"
    repository.mkdir()
    history_fixture(repository)

    errors = release_check.history_errors(repository)
    assert tuple(
        error for error in errors
        if error.startswith("reachable history")
    ) == HISTORY_ORACLE

    root_commit = git_at(repository, "rev-list", "--max-parents=0", "HEAD")
    range_errors = publication_range_errors(root_commit, "HEAD", repository)
    assert tuple(
        error for error in range_errors
        if error.startswith("publication range")
    ) == RANGE_ORACLE


def test_history_scans_annotated_tag_messages(tmp_path):
    repository = tmp_path / "fixture"
    repository.mkdir()
    history_fixture(repository)
    git_at(
        repository, "tag", "-a", "leaky", "-m",
        "shipped with sk-ant-" + "abcdefghijklmnopqrstuvwxyz012345",
    )

    errors = release_check.history_errors(repository)
    assert (
        "reachable history annotated tag message contains a possible Anthropic key"
        in errors
    )


def test_git_grep_prefilter_is_a_superset_of_every_registry_pattern():
    """The prefilter may over-match, but it must never lose a registry hit."""
    samples = {
        "personal absolute home path": b"see /" + b"Users/someone/notes",
        "possible private key": b"-----BEGIN " + b"OPENSSH PRIVATE KEY-----",
        "possible GitHub token": b"gh" + b"p_" + b"B" * 36,
        "possible OpenAI key": b"sk" + b"-" + b"C" * 24,
        "possible Anthropic key": b"sk" + b"-ant-" + b"D" * 24,
        "possible AWS access key": b"AKIA" + b"E" * 16,
    }
    assert set(samples) == set(release_check.FINDING_PATTERNS)
    for label, sample in samples.items():
        assert release_check.classify(sample) >= {label}, label
        widened = re.compile(
            release_check.grep_pattern(release_check.FINDING_PATTERNS[label]).encode()
        )
        assert widened.search(sample), label
