from pathlib import Path
import os
import re
import shutil
import subprocess

import pytest
import yaml


ROOT = Path(__file__).resolve().parents[1]
GIT_REDIRECT_VARIABLES = (
    "GIT_DIR", "GIT_WORK_TREE", "GIT_COMMON_DIR", "GIT_INDEX_FILE",
    "GIT_OBJECT_DIRECTORY", "GIT_ALTERNATE_OBJECT_DIRECTORIES",
    "GIT_CEILING_DIRECTORIES", "GIT_DISCOVERY_ACROSS_FILESYSTEM",
)


def clean_git_test_environment() -> dict[str, str]:
    environment = os.environ.copy()
    for name in GIT_REDIRECT_VARIABLES:
        environment.pop(name, None)
    return environment


def frontmatter_name(path: Path) -> str:
    text = path.read_text()
    match = re.search(r"^name:\s*([^\n]+)$", text, re.MULTILINE)
    assert match, f"missing name in {path}"
    return match.group(1).strip()


def test_lifecycle_skills_are_portable_and_named_for_their_directory():
    for name in ("implement", "code-review"):
        skill = ROOT / "skills" / name / "SKILL.md"
        assert skill.is_file(), f"missing portable {name} skill"
        assert frontmatter_name(skill) == name


def test_root_harness_checker_is_available():
    checker = ROOT / "scripts" / "check-harness"
    assert checker.is_file()
    assert checker.stat().st_mode & 0o111


def test_root_harness_checker_reports_identity_before_loading_test_helpers(tmp_path):
    scripts_root = tmp_path / "scripts"
    marker = tmp_path / "shadow-git-ran"
    shadow_dir = tmp_path / "shadow-bin"
    shadow_dir.mkdir()
    shadow_git = shadow_dir / "git"
    shadow_git.write_text(f"#!/bin/sh\ntouch '{marker}'\nprintf '%040d\\n' 0\n")
    shadow_git.chmod(0o755)
    helper = scripts_root / "lib/harness-python.sh"
    helper.parent.mkdir(parents=True)
    helper.write_text(
        "printf 'git_dir=%s ceiling=%s git_bin=%s\\n' "
        '"${GIT_DIR-<unset>}" "${GIT_CEILING_DIRECTORIES-<unset>}" '
        '"${PROVENANT_GIT_BIN-<unset>}"\n'
        "exit 19\n"
    )
    clean_git_environment = clean_git_test_environment()
    expected_head = subprocess.check_output(
        ["git", "-C", str(ROOT), "rev-parse", "--verify", "HEAD"],
        env=clean_git_environment,
        text=True,
    ).strip()
    environment = os.environ.copy()
    environment.update({
        "AGENT_FABRIC_PRODUCT_ROOT": str(ROOT),
        "PROVENANT_SCRIPTS_ROOT": str(scripts_root),
        "PROVENANT_SKILLS_ROOT": str(tmp_path / "skills"),
        "PROVENANT_TESTS_ROOT": str(tmp_path / "tests"),
        "GIT_DIR": str(tmp_path / "redirected.git"),
        "GIT_CEILING_DIRECTORIES": str(ROOT),
        "HARNESS_PYTHON": str(tmp_path / "selected-python"),
        "PATH": f"{shadow_dir}:{environment['PATH']}",
    })

    refused = subprocess.run(
        ["bash", str(ROOT / "scripts/check-harness")],
        cwd=ROOT,
        env=environment,
        text=True,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        check=False,
    )
    assert refused.returncode == 3
    assert "component-root overrides require PROVENANT_CHECK_TEST_OVERRIDES=1" in refused.stderr
    environment["PROVENANT_CHECK_TEST_OVERRIDES"] = "1"

    result = subprocess.run(
        ["bash", str(ROOT / "scripts/check-harness")],
        cwd=ROOT,
        env=environment,
        text=True,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        check=False,
    )

    assert result.returncode == 19
    output = result.stdout.splitlines()
    assert output[:2] == [
        f"check-harness: product_root={ROOT}", f"check-harness: git_head={expected_head}",
    ]
    assert output[2].startswith("check-harness: test_overrides=scripts_root=")
    assert output[3] == f"check-harness: harness_python={tmp_path / 'selected-python'}"
    assert output[4].startswith("git_dir=<unset> ceiling=<unset> git_bin=/")
    assert not marker.exists()


def test_non_git_product_does_not_borrow_identity_or_python_from_an_ancestor_repo(tmp_path):
    outer = tmp_path / "outer"
    outer.mkdir()
    clean_git_environment = clean_git_test_environment()
    subprocess.run(
        ["git", "init", "-q"], cwd=outer, env=clean_git_environment, check=True
    )
    product = outer / "product"
    product.mkdir()
    scripts_root = tmp_path / "scripts"
    helper = scripts_root / "lib/harness-python.sh"
    helper.parent.mkdir(parents=True)
    shutil.copy2(ROOT / "scripts/lib/harness-python.sh", helper)
    marker = tmp_path / "borrowed-python-ran"
    borrowed = outer / ".venv/bin/python"
    borrowed.parent.mkdir(parents=True)
    borrowed.write_text(f"#!/bin/sh\ntouch '{marker}'\nexit 0\n")
    borrowed.chmod(0o755)
    environment = clean_git_environment.copy()
    environment.update({
        "AGENT_FABRIC_PRODUCT_ROOT": str(product),
        "PROVENANT_SCRIPTS_ROOT": str(scripts_root),
        "PROVENANT_SKILLS_ROOT": str(tmp_path / "skills"),
        "PROVENANT_TESTS_ROOT": str(tmp_path / "tests"),
    })

    result = subprocess.run(
        ["bash", str(ROOT / "scripts/check-harness")],
        cwd=product,
        env=environment,
        text=True,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        check=False,
    )

    assert result.returncode == 3
    assert result.stdout.splitlines()[:2] == [
        f"check-harness: product_root={product}", "check-harness: git_head=unavailable",
    ]
    assert (
        f"check-harness: cannot verify checkout ownership or a full Git HEAD for {product}"
        in result.stderr
    )
    assert not marker.exists()


@pytest.mark.parametrize("metadata_kind", ["symlink", "gitfile"])
def test_checker_refuses_git_metadata_owned_by_another_repository(tmp_path, metadata_kind):
    outer = tmp_path / "outer"
    outer.mkdir()
    environment = clean_git_test_environment()
    subprocess.run(["git", "init", "-q"], cwd=outer, env=environment, check=True)
    subprocess.run(
        ["git", "config", "user.email", "test@example.com"],
        cwd=outer, env=environment, check=True,
    )
    subprocess.run(
        ["git", "config", "user.name", "Test"],
        cwd=outer, env=environment, check=True,
    )
    (outer / "tracked").write_text("fixture\n")
    subprocess.run(["git", "add", "tracked"], cwd=outer, env=environment, check=True)
    subprocess.run(["git", "commit", "-qm", "fixture"], cwd=outer, env=environment, check=True)
    product = outer / "product"
    product.mkdir()
    dot_git = product / ".git"
    if metadata_kind == "symlink":
        dot_git.symlink_to(outer / ".git", target_is_directory=True)
    else:
        dot_git.write_text(f"gitdir: {outer / '.git'}\n")
    marker = tmp_path / "borrowed-python-ran"
    borrowed = outer / ".venv/bin/python"
    borrowed.parent.mkdir(parents=True)
    borrowed.write_text(f"#!/bin/sh\ntouch '{marker}'\nexit 0\n")
    borrowed.chmod(0o755)
    environment.update({
        "AGENT_FABRIC_PRODUCT_ROOT": str(product),
        "PROVENANT_SCRIPTS_ROOT": str(tmp_path / "scripts"),
        "PROVENANT_SKILLS_ROOT": str(tmp_path / "skills"),
        "PROVENANT_TESTS_ROOT": str(tmp_path / "tests"),
    })

    result = subprocess.run(
        ["bash", str(ROOT / "scripts/check-harness")],
        cwd=product,
        env=environment,
        text=True,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        check=False,
    )

    assert result.returncode == 3
    assert result.stdout.splitlines()[:2] == [
        f"check-harness: product_root={product}", "check-harness: git_head=unavailable",
    ]
    assert "cannot verify checkout ownership" in result.stderr
    assert not marker.exists()


def direct_checker_probe(product: Path, tmp_path: Path) -> tuple[subprocess.CompletedProcess[str], Path]:
    scripts_root = tmp_path / "probe-scripts"
    helper = scripts_root / "lib/harness-python.sh"
    helper.parent.mkdir(parents=True)
    marker = tmp_path / "helper-loaded"
    helper.write_text(f"touch '{marker}'\nexit 19\n")
    environment = clean_git_test_environment()
    environment.update({
        "AGENT_FABRIC_PRODUCT_ROOT": str(product),
        "PROVENANT_SCRIPTS_ROOT": str(scripts_root),
        "PROVENANT_SKILLS_ROOT": str(tmp_path / "probe-skills"),
        "PROVENANT_TESTS_ROOT": str(tmp_path / "probe-tests"),
        "PROVENANT_CHECK_TEST_OVERRIDES": "1",
    })
    return subprocess.run(
        ["bash", str(ROOT / "scripts/check-harness")],
        cwd=product,
        env=environment,
        text=True,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        check=False,
    ), marker


def initialise_git_fixture(path: Path) -> str:
    path.mkdir()
    environment = clean_git_test_environment()
    subprocess.run(["git", "init", "-q"], cwd=path, env=environment, check=True)
    subprocess.run(
        ["git", "config", "user.email", "test@example.com"],
        cwd=path, env=environment, check=True,
    )
    subprocess.run(
        ["git", "config", "user.name", "Test"], cwd=path, env=environment, check=True
    )
    (path / "tracked").write_text(f"{path.name}\n")
    subprocess.run(["git", "add", "tracked"], cwd=path, env=environment, check=True)
    subprocess.run(["git", "commit", "-qm", path.name], cwd=path, env=environment, check=True)
    return subprocess.check_output(
        ["git", "rev-parse", "HEAD"], cwd=path, env=environment, text=True
    ).strip()


def test_checker_refuses_a_foreign_common_git_directory(tmp_path):
    product = tmp_path / "product"
    foreign = tmp_path / "foreign"
    product_head = initialise_git_fixture(product)
    foreign_head = initialise_git_fixture(foreign)
    assert product_head != foreign_head
    (product / ".git/commondir").write_text(f"{foreign / '.git'}\n")

    result, marker = direct_checker_probe(product, tmp_path)

    assert result.returncode == 3
    assert result.stdout.splitlines()[:2] == [
        f"check-harness: product_root={product}", "check-harness: git_head=unavailable",
    ]
    assert foreign_head not in result.stdout
    assert not marker.exists()


def test_checker_refuses_a_linked_admin_with_a_foreign_common_directory(tmp_path):
    primary = tmp_path / "primary"
    initialise_git_fixture(primary)
    linked = tmp_path / "linked"
    subprocess.run(
        ["git", "worktree", "add", "-q", "-b", "linked-fixture", str(linked)],
        cwd=primary,
        env=clean_git_test_environment(),
        check=True,
    )
    admin = Path(
        subprocess.check_output(
            ["git", "rev-parse", "--absolute-git-dir"],
            cwd=linked,
            env=clean_git_test_environment(),
            text=True,
        ).strip()
    )
    foreign = tmp_path / "foreign"
    foreign_head = initialise_git_fixture(foreign)
    subprocess.run(
        ["git", "branch", "linked-fixture"],
        cwd=foreign,
        env=clean_git_test_environment(),
        check=True,
    )
    (admin / "commondir").write_text(f"{foreign / '.git'}\n")
    assert subprocess.check_output(
        ["git", "rev-parse", "HEAD"],
        cwd=linked,
        env=clean_git_test_environment(),
        text=True,
    ).strip() == foreign_head

    result, marker = direct_checker_probe(linked, tmp_path)

    assert result.returncode == 3
    assert result.stdout.splitlines()[:2] == [
        f"check-harness: product_root={linked}", "check-harness: git_head=unavailable",
    ]
    assert foreign_head not in result.stdout
    assert not marker.exists()


def test_checker_accepts_a_registered_relative_back_pointer(tmp_path):
    primary = tmp_path / "primary"
    initialise_git_fixture(primary)
    linked = tmp_path / "linked"
    subprocess.run(
        ["git", "worktree", "add", "-q", "-b", "relative-fixture", str(linked)],
        cwd=primary,
        env=clean_git_test_environment(),
        check=True,
    )
    admin = Path(
        subprocess.check_output(
            ["git", "rev-parse", "--absolute-git-dir"],
            cwd=linked,
            env=clean_git_test_environment(),
            text=True,
        ).strip()
    )
    (admin / "gitdir").write_text(f"{os.path.relpath(linked / '.git', admin)}\n")

    result, marker = direct_checker_probe(linked, tmp_path)

    assert result.returncode == 19, result.stderr
    assert result.stdout.splitlines()[0] == f"check-harness: product_root={linked}"
    assert marker.exists()


@pytest.mark.skipif(not Path("/private/var").is_dir(), reason="macOS /var alias only")
def test_checker_canonicalises_the_macos_var_alias(tmp_path):
    product = tmp_path / "product"
    initialise_git_fixture(product)
    canonical = product.resolve()
    if not str(canonical).startswith("/private/var/"):
        pytest.skip("temporary directory is not beneath /private/var")
    alias = Path(str(canonical).removeprefix("/private"))

    result, marker = direct_checker_probe(alias, tmp_path)

    assert result.returncode == 19, result.stderr
    assert result.stdout.splitlines()[0] == f"check-harness: product_root={canonical}"
    assert marker.exists()


def test_dispatchers_use_the_stable_product_command_and_local_skill_helpers():
    dispatcher = (ROOT / "skills" / "orchestrate" / "scripts" / "cf_dispatch.sh").read_text()
    assert 'resolve_routing' in dispatcher  # tries provenant, falls back to model_route.py
    assert '"$SCRIPT_DIR/capabilities.py" codex' in dispatcher
    assert '-c service_tier="default"' in dispatcher
    assert "AGENTS_ROOT" not in dispatcher
    assert "HARNESS_ROOT" not in dispatcher


def test_default_agent_run_directory_is_ignored_in_the_harness_repo():
    assert ".agent-run/" in (ROOT / ".gitignore").read_text().splitlines()


@pytest.mark.skipif(shutil.which("mmdc") is None, reason="optional local Mermaid CLI is absent")
def test_readme_mermaid_parses_with_available_local_renderer(tmp_path):
    readme = (ROOT / "README.md").read_text()
    diagrams = re.findall(r"```mermaid\n(.*?)\n```", readme, re.DOTALL)
    for index, diagram in enumerate(diagrams):
        source = tmp_path / f"diagram-{index}.mmd"
        output = tmp_path / f"diagram-{index}.svg"
        source.write_text(diagram)
        subprocess.run(
            ["mmdc", "-i", str(source), "-o", str(output)],
            cwd=tmp_path,
            check=True,
            capture_output=True,
            text=True,
        )
        assert output.is_file()


def test_openai_skill_sidecar_descriptions_fit_provider_contract():
    for path in (ROOT / "skills").glob("*/agents/openai.yaml"):
        value = yaml.safe_load(path.read_text())
        description = value["interface"]["short_description"]
        assert 25 <= len(description) <= 64, path


@pytest.mark.parametrize("broken_suffix", ("js", "mjs", "cjs"))
def test_skill_javascript_gate_checks_all_module_suffixes_and_prunes_dependencies(
    tmp_path: Path,
    broken_suffix: str,
):
    checker = ROOT / "scripts" / "check-skill-javascript"
    assert checker.stat().st_mode & 0o111
    skills = tmp_path / "skills"
    skills.mkdir()
    (skills / "valid.js").write_text("const valid = true;\n")
    (skills / "valid.mjs").write_text(
        "export const valid = await Promise.resolve(true);\n"
    )
    (skills / "valid.cjs").write_text("module.exports = { valid: true };\n")
    vendored = skills / "node_modules"
    vendored.mkdir()
    (vendored / "ignored.mjs").write_text("const = broken;\n")

    passing = subprocess.run(
        [str(checker), str(skills)],
        check=False,
        capture_output=True,
        text=True,
    )
    assert passing.returncode == 0, passing.stderr
    assert "PASS: checked 3 skill JavaScript files" in passing.stdout

    broken = skills / f"broken.{broken_suffix}"
    broken.write_text("const = broken;\n")
    failing = subprocess.run(
        [str(checker), str(skills)],
        check=False,
        capture_output=True,
        text=True,
    )
    assert failing.returncode != 0
    assert str(broken) in failing.stderr


def test_skill_javascript_gate_rejects_an_empty_tree(tmp_path: Path):
    checker = ROOT / "scripts" / "check-skill-javascript"
    result = subprocess.run(
        [str(checker), str(tmp_path)],
        check=False,
        capture_output=True,
        text=True,
    )
    assert result.returncode != 0
    assert "no skill JavaScript files found" in result.stderr
