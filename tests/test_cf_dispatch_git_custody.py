#!/usr/bin/env python3
"""Git repository-custody regressions for cf_dispatch.py."""

import shlex
import shutil
from pathlib import Path
import subprocess
from types import SimpleNamespace

import pytest

from tests.cf_dispatch_support import PRODUCT_ROOT, SCRIPT, fabric_free_env
from tests.support.fake_cli import fake_bin, write_executable
from tests.support.git import git_repo, linked_worktree


GIT_CONTEXT_VARIABLES = (
    "GIT_ALTERNATE_OBJECT_DIRECTORIES",
    "GIT_CEILING_DIRECTORIES",
    "GIT_COMMON_DIR",
    "GIT_CONFIG",
    "GIT_CONFIG_COUNT",
    "GIT_CONFIG_PARAMETERS",
    "GIT_DIR",
    "GIT_DISCOVERY_ACROSS_FILESYSTEM",
    "GIT_GRAFT_FILE",
    "GIT_IMPLICIT_WORK_TREE",
    "GIT_INDEX_FILE",
    "GIT_NAMESPACE",
    "GIT_NO_REPLACE_OBJECTS",
    "GIT_OBJECT_DIRECTORY",
    "GIT_PREFIX",
    "GIT_QUARANTINE_PATH",
    "GIT_REPLACE_REF_BASE",
    "GIT_SHALLOW_FILE",
    "GIT_WORK_TREE",
)


def make_fixture(tmp_path):
    repo = git_repo(tmp_path / "project", files={"tracked.txt": "base\n"}, message="base")

    source = linked_worktree(repo, "source-worktree", branch="fixture")
    copied = tmp_path / "copied-worktree"
    shutil.copytree(source, copied)
    index_path = Path(subprocess.check_output(
        ["git", "-C", str(source), "rev-parse", "--git-path", "index"],
        text=True,
    ).strip())

    def state():
        return {
            "config": (repo / ".git" / "config").read_bytes(),
            "index": index_path.read_bytes(),
            "refs": subprocess.check_output(
                ["git", "-C", str(repo), "show-ref"], text=True,
            ),
        }

    bin_dir = fake_bin(tmp_path)
    invoked = tmp_path / "provider.invoked"
    write_executable(
        bin_dir / "claude",
        f"""\
        #!/usr/bin/env bash
        touch {shlex.quote(str(invoked))}
        git config fixture.provider-ran true
        printf 'changed\\n' > tracked.txt
        git add tracked.txt
        git commit -qm provider
        printf 'OK\\n'
        """,
    )
    product_root = tmp_path / "route-product"
    (product_root / "scripts").mkdir(parents=True)
    write_executable(
        product_root / "scripts/model-route",
        """\
        #!/usr/bin/env bash
        printf '%s\\n' '{"status":"ok","alias":"senior","resolved_model":"sonnet","model_family":"anthropic","endpoint_provider":"anthropic","identity_source":"test","requested_effort":"","effort":"","effort_source":"route-default","effort_capability_source":"test"}'
        """,
    )
    env = fabric_free_env()
    env["AGENT_FABRIC_PRODUCT_ROOT"] = str(product_root)
    env["PATH"] = f"{bin_dir}:{PRODUCT_ROOT / 'scripts'}:{env['PATH']}"

    def dispatch(cwd, out_name):
        return subprocess.run(
            [
                str(SCRIPT), "--intent", "ordinary", "--tool", "claude",
                "--orchestrator-family", "openai", "--alias", "senior",
                "--role", "worker", "--out", str(tmp_path / out_name),
                "--prompt", "inspect",
            ],
            cwd=cwd,
            env=env,
            text=True,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
        )

    git_dir = Path(subprocess.check_output(
        ["git", "-C", str(source), "rev-parse", "--absolute-git-dir"],
        text=True,
    ).strip())
    return SimpleNamespace(
        repo=repo,
        source=source,
        copied=copied,
        git_dir=git_dir,
        bin_dir=bin_dir,
        invoked=invoked,
        env=env,
        state=state,
        before=state(),
        dispatch=dispatch,
    )


def assert_rejected(case, result, message):
    assert result.returncode == 2
    assert message in result.stderr
    assert not case.invoked.exists()
    assert case.state() == case.before


def mutate_metadata(case, mutation):
    if mutation == "symlink-dot-git":
        path = case.copied / ".git"
        original = path.read_bytes()
        path.unlink()
        path.symlink_to(case.source / ".git")
    else:
        names = {
            "trailing-back-pointer": "gitdir",
            "nul-back-pointer": "gitdir",
            "missing-back-pointer": "gitdir",
            "missing-head": "HEAD",
            "missing-common-dir": "commondir",
        }
        path = case.git_dir / names[mutation]
        original = path.read_bytes()
        if mutation == "trailing-back-pointer":
            path.write_bytes(original.rstrip(b"\n") + b"\ntrailing-junk\n")
        elif mutation == "nul-back-pointer":
            path.write_bytes(original.rstrip(b"\n") + b"\0")
        else:
            path.unlink()

    def restore():
        path.unlink(missing_ok=True)
        path.write_bytes(original)

    return restore


def test_copied_linked_worktree_is_rejected_before_provider_launch(tmp_path):
    case = make_fixture(tmp_path)
    result = case.dispatch(case.copied, "copied.txt")
    assert_rejected(case, result, "copied checkout")


@pytest.mark.parametrize(
    ("mutation", "message"),
    [
        ("symlink-dot-git", "symlinked .git"),
        ("trailing-back-pointer", "invalid linked-worktree back-pointer"),
        ("nul-back-pointer", "invalid linked-worktree back-pointer"),
        ("missing-back-pointer", "invalid linked-worktree back-pointer"),
        ("missing-head", "invalid Git metadata"),
        ("missing-common-dir", "invalid Git metadata"),
    ],
)
def test_malformed_linked_worktree_metadata_is_rejected(
    tmp_path, mutation, message,
):
    case = make_fixture(tmp_path)
    restore = mutate_metadata(case, mutation)
    try:
        result = case.dispatch(case.copied, f"{mutation}.txt")
    finally:
        restore()
    assert_rejected(case, result, message)


def test_provider_inherits_the_validated_repository_context(tmp_path):
    case = make_fixture(tmp_path)
    observed_root = tmp_path / "provider-root.txt"
    names = " ".join(GIT_CONTEXT_VARIABLES)
    write_executable(
        case.bin_dir / "claude",
        f"""\
        #!/usr/bin/env bash
        for name in {names}; do
          printenv "$name" >/dev/null 2>&1 && exit 88
        done
        git rev-parse --show-toplevel > {shlex.quote(str(observed_root))}
        touch {shlex.quote(str(case.invoked))}
        cat >/dev/null
        printf 'OK\\n'
        """,
    )
    case.env.update({
        name: str(case.repo / ".git")
        for name in GIT_CONTEXT_VARIABLES
    })

    result = case.dispatch(case.source, "valid.txt")

    assert result.returncode == 0, result.stderr
    assert case.invoked.exists()
    assert Path(observed_root.read_text(encoding="utf-8").strip()).resolve() == (
        case.source.resolve()
    )
