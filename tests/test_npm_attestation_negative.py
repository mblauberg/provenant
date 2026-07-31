from pathlib import Path
import os
import shutil
import subprocess


ROOT = Path(__file__).resolve().parents[1]


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


def test_tampered_tsx_loader_is_rejected_at_adapter_admission(tmp_path):
    checkout = tmp_path / "tampered-loader-checkout"
    shutil.copytree(
        ROOT,
        checkout,
        symlinks=True,
        ignore=shutil.ignore_patterns(
            ".git",
            ".agent-run",
            ".npm-ci-attestation",
            ".pytest_cache",
            ".review-snapshots",
            ".venv",
            "node_modules",
        ),
    )
    for command in (
        ["git", "init", "-q"],
        ["git", "add", "."],
        [
            "git",
            "-c",
            "user.name=Attestation Test",
            "-c",
            "user.email=attestation-test@example.invalid",
            "commit",
            "-qm",
            "attestation fixture",
        ],
    ):
        result = run(command, cwd=checkout)
        assert result.returncode == 0, result.stderr

    installed = run([str(checkout / "scripts" / "install-agent-fabric-dependencies")], cwd=checkout)
    assert installed.returncode == 0, installed.stderr
    attestation = checkout / "runtime" / "agent-fabric" / ".npm-ci-attestation"
    assert attestation.is_file()

    loader = checkout / "node_modules" / "tsx" / "dist" / "loader.mjs"
    original_size = loader.stat().st_size
    with loader.open("ab") as stream:
        stream.write(b"@")
    assert loader.stat().st_size == original_size + 1
    # Protocol autobuild is itself an npm execution path reached before the
    # TypeScript launcher. Its build entrypoint must reject the drift first.
    protocol_build = run([str(checkout / "scripts" / "agent-fabric-protocol-build")], cwd=checkout)
    assert protocol_build.returncode == 1
    assert "NPM_INSTALL_ATTESTATION_MISMATCH" in protocol_build.stderr
    assert "SyntaxError" not in protocol_build.stderr

    resolved = run(
        [
            str(checkout / "scripts" / "agent-fabric"),
            "adapter",
            "executable",
            "--adapter",
            "codex-app-server",
            "--product-root",
            str(checkout),
            "--instance-root",
            str(checkout),
        ],
        cwd=checkout,
    )

    assert resolved.returncode == 1
    assert resolved.stdout == ""
    assert "NPM_INSTALL_ATTESTATION_MISMATCH" in resolved.stderr
    assert "node_modules was modified after npm ci" in resolved.stderr
    assert str(checkout / "scripts" / "install-agent-fabric-dependencies") in resolved.stderr
    assert "SyntaxError" not in resolved.stderr


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
