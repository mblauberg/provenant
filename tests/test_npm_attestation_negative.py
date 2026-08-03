from pathlib import Path
import os
import subprocess


ROOT = Path(__file__).resolve().parents[1]
TSX_LOADER_LIBRARY = ROOT / "scripts" / "lib" / "agent-fabric-tsx-loader.sh"


def run(command, *, cwd, timeout=180):
    return subprocess.run(
        command,
        cwd=cwd,
        env={**os.environ, "CI": "1"},
        text=True,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        timeout=timeout,
        check=False,
    )


def _resolve_tsx_loader(
    start: Path, workspace: Path | None = None
) -> subprocess.CompletedProcess[str]:
    call = f'resolve_tsx_loader "{start}"'
    if workspace is not None:
        call = f'resolve_tsx_loader "{start}" "{workspace}"'
    return subprocess.run(
        ["sh", "-c", f'. "{TSX_LOADER_LIBRARY}"; {call}'],
        text=True,
        capture_output=True,
        check=False,
    )


def _install_tsx(root: Path, *, name: str = "tsx") -> Path:
    package = root / "node_modules/tsx"
    (package / "dist").mkdir(parents=True, exist_ok=True)
    (package / "package.json").write_text(
        f'{{"name": "{name}", "version": "4.0.0"}}\n', encoding="utf-8"
    )
    loader = package / "dist/loader.mjs"
    loader.write_text("", encoding="utf-8")
    return loader


def _git_repository(root: Path) -> None:
    root.mkdir(parents=True, exist_ok=True)
    subprocess.run(
        ["git", "init", "--quiet"], cwd=root, check=True, capture_output=True
    )


def test_tsx_loader_resolves_from_the_owning_repository(tmp_path: Path) -> None:
    repository = tmp_path / "repo"
    _git_repository(repository)
    loader = _install_tsx(repository)
    worktree = repository / ".worktrees/impl-example"
    worktree.mkdir(parents=True)

    result = _resolve_tsx_loader(worktree)

    assert result.returncode == 0, result.stderr
    assert result.stdout.strip() == str(loader)


def test_tsx_loader_prefers_the_declaring_package(tmp_path: Path) -> None:
    repository = tmp_path / "repo"
    _git_repository(repository)
    hoisted = _install_tsx(repository)
    package_root = repository / "runtime/fabric"
    package_root.mkdir(parents=True)
    own = _install_tsx(package_root)

    result = _resolve_tsx_loader(package_root, repository)

    assert result.returncode == 0, result.stderr
    assert result.stdout.strip() == str(own)
    assert result.stdout.strip() != str(hoisted)


def test_tsx_loader_refuses_a_loader_above_the_owning_repository(
    tmp_path: Path,
) -> None:
    planted = _install_tsx(tmp_path)
    repository = tmp_path / "repo"
    _git_repository(repository)
    worktree = repository / ".worktrees/impl-example"
    worktree.mkdir(parents=True)

    result = _resolve_tsx_loader(worktree)

    assert result.returncode == 1
    assert result.stdout.strip() == ""
    assert str(planted) not in result.stdout


def test_tsx_loader_rejects_a_loader_that_is_not_the_tsx_package(
    tmp_path: Path,
) -> None:
    repository = tmp_path / "repo"
    _git_repository(repository)
    _install_tsx(repository, name="not-tsx")

    result = _resolve_tsx_loader(repository)

    assert result.returncode == 1
    assert result.stdout.strip() == ""


def test_tsx_loader_ignores_a_node_modules_holding_no_packages(tmp_path: Path) -> None:
    repository = tmp_path / "repo"
    _git_repository(repository)
    loader = _install_tsx(repository)
    worktree = repository / ".worktrees/impl-example"
    (worktree / "node_modules/.vite").mkdir(parents=True)

    result = _resolve_tsx_loader(worktree)

    assert result.returncode == 0, result.stderr
    assert result.stdout.strip() == str(loader)


def test_tsx_loader_resolves_through_a_symlinked_invocation(tmp_path: Path) -> None:
    repository = tmp_path / "repo"
    _git_repository(repository)
    loader = _install_tsx(repository)
    worktree = repository / ".worktrees/impl-example"
    worktree.mkdir(parents=True)
    alias = tmp_path / "alias"
    alias.symlink_to(worktree)

    result = _resolve_tsx_loader(alias)

    assert result.returncode == 0, result.stderr
    assert result.stdout.strip() == str(loader)


def test_tsx_loader_ignores_git_environment_that_would_widen_the_boundary(
    tmp_path: Path,
) -> None:
    outer = tmp_path / "outer"
    _git_repository(outer)
    planted = _install_tsx(outer)
    repository = outer / "repo"
    _git_repository(repository)
    worktree = repository / ".worktrees/impl-example"
    worktree.mkdir(parents=True)

    result = subprocess.run(
        [
            "sh",
            "-c",
            f'. "{TSX_LOADER_LIBRARY}"; resolve_tsx_loader "{worktree}"',
        ],
        text=True,
        capture_output=True,
        check=False,
        env={**os.environ, "GIT_DIR": str(outer / ".git")},
    )

    assert result.returncode == 1
    assert str(planted) not in result.stdout


def test_tsx_loader_rejects_a_loader_symlinked_outside_the_boundary(
    tmp_path: Path,
) -> None:
    repository = tmp_path / "repo"
    _git_repository(repository)
    loader = _install_tsx(repository)
    outside = tmp_path / "outside/payload.mjs"
    outside.parent.mkdir(parents=True)
    outside.write_text("", encoding="utf-8")
    loader.unlink()
    loader.symlink_to(outside)

    result = _resolve_tsx_loader(repository)

    assert result.returncode == 1
    assert result.stdout.strip() == ""


def test_tsx_loader_rejects_a_manifest_that_only_mentions_tsx(tmp_path: Path) -> None:
    repository = tmp_path / "repo"
    _git_repository(repository)
    package = repository / "node_modules/tsx"
    (package / "dist").mkdir(parents=True)
    (package / "dist/loader.mjs").write_text("", encoding="utf-8")
    (package / "package.json").write_text(
        '{"name": "impostor", "description": "shim for \\"name\\": \\"tsx\\" loaders",'
        ' "dependencies": {"tsx": "4.0.0"}}\n',
        encoding="utf-8",
    )

    result = _resolve_tsx_loader(repository)

    assert result.returncode == 1
    assert result.stdout.strip() == ""


def test_tsx_loader_refuses_when_the_package_sits_outside_the_boundary(
    tmp_path: Path,
) -> None:
    planted = _install_tsx(tmp_path)
    workspace = tmp_path / "repo"
    _git_repository(workspace)
    stray = tmp_path / "elsewhere/pkg"
    stray.mkdir(parents=True)

    result = _resolve_tsx_loader(stray, workspace)

    assert result.returncode == 1
    assert str(planted) not in result.stdout


def test_tsx_loader_fails_when_nothing_inside_the_boundary_provides_it(
    tmp_path: Path,
) -> None:
    result = _resolve_tsx_loader(tmp_path)

    assert result.returncode == 1
    assert result.stdout.strip() == ""


def test_package_local_shadow_loader_is_never_selected(tmp_path):
    library = ROOT / "scripts" / "lib" / "agent-fabric-tsx-loader.sh"
    product_root = tmp_path / "product"
    root_loader = product_root / "node_modules" / "tsx" / "dist" / "loader.mjs"
    shadow_loader = product_root / "runtime" / "agent-fabric" / "node_modules" / "tsx" / "dist" / "loader.mjs"
    for loader, marker in ((root_loader, "root"), (shadow_loader, "shadow")):
        loader.parent.mkdir(parents=True)
        loader.write_text(f"// {marker}\n")
        (loader.parents[1] / "package.json").write_text('{"name":"tsx"}\n')

    result = run(
        [
            "sh",
            "-c",
            '. "$1"; resolve_attested_tsx_loader "$2"',
            "resolve-attested-loader",
            str(library),
            str(product_root),
        ],
        cwd=product_root,
    )

    assert result.returncode == 0, result.stderr
    assert Path(result.stdout.strip()) == root_loader


def test_loader_symlink_into_excluded_bin_tree_is_rejected(tmp_path):
    library = ROOT / "scripts" / "lib" / "agent-fabric-tsx-loader.sh"
    product_root = tmp_path / "product"
    excluded_package = product_root / "node_modules" / ".bin" / "tsx"
    loader = excluded_package / "dist" / "loader.mjs"
    loader.parent.mkdir(parents=True)
    loader.write_text("// excluded\n")
    (excluded_package / "package.json").write_text('{"name":"tsx"}\n')
    (product_root / "node_modules" / "tsx").symlink_to(Path(".bin") / "tsx")

    result = run(
        [
            "sh",
            "-c",
            '. "$1"; resolve_attested_tsx_loader "$2"',
            "resolve-attested-loader",
            str(library),
            str(product_root),
        ],
        cwd=product_root,
    )

    assert result.returncode == 1
    assert result.stdout == ""
