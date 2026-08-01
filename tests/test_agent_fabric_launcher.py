from __future__ import annotations

import os
from pathlib import Path
import shutil
import subprocess
import time

import pytest


REPO_ROOT = Path(__file__).resolve().parents[1]
LAUNCHER = REPO_ROOT / "scripts" / "agent-fabric"
MCP_WRAPPER = REPO_ROOT / "scripts" / "agent-fabric-mcp"
WARM = REPO_ROOT / "scripts" / "agent-fabric-warm"
SCRIPT_LIBRARY_DIR = REPO_ROOT / "scripts" / "lib"
FRESHNESS_LIBRARY = SCRIPT_LIBRARY_DIR / "agent-fabric-workspace-freshness.sh"
BUILD_LOCK_LIBRARY = SCRIPT_LIBRARY_DIR / "agent-fabric-protocol-build-lock.sh"
TSX_LOADER_LIBRARY = SCRIPT_LIBRARY_DIR / "agent-fabric-tsx-loader.sh"
PREFLIGHT = REPO_ROOT / "scripts" / "agent-fabric-protocol-preflight"
PROTOCOL_BUILD = REPO_ROOT / "scripts" / "agent-fabric-protocol-build"
ATTESTATION_VERIFY = REPO_ROOT / "runtime/agent-fabric/scripts/verify-npm-ci-attestation.mjs"
ATTESTATION_WRITE = REPO_ROOT / "runtime/agent-fabric/scripts/write-npm-ci-attestation.mjs"
ATTESTATION_LIBRARY = REPO_ROOT / "runtime/agent-fabric/scripts/lib/npm-install-attestation.mjs"
PROTOCOL_BIN_PREFLIGHT = (
    REPO_ROOT
    / "runtime/agent-fabric-protocol/bin/protocol-build-preflight.js"
)
CONSUMER_BINS = {
    "agent-fabric-herdr": (
        REPO_ROOT
        / "runtime/agent-fabric-herdr/bin/agent-fabric-herdr.js"
    ),
    "agent-fabric-console": (
        REPO_ROOT
        / "runtime/agent-fabric-console/bin/agent-fabric-console.js"
    ),
}
ROOT_MANIFEST_STAMP = ".root-manifests.sha256"


def _write(path: Path, content: str = "fixture\n") -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(content, encoding="utf-8")


def _wait_for_path(path: Path, *, timeout: float = 10) -> None:
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        if path.exists():
            return
        time.sleep(0.01)
    raise AssertionError(f"timed out waiting for test handshake: {path}")


def _wait_for_glob(directory: Path, pattern: str, *, timeout: float = 10) -> Path:
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        match = next(directory.glob(pattern), None)
        if match is not None:
            return match
        time.sleep(0.01)
    raise AssertionError(f"timed out waiting for test handshake: {directory / pattern}")


def _copy_attestation_scripts(root: Path) -> None:
    attestation_scripts = root / "runtime/agent-fabric/scripts"
    attestation_scripts.mkdir(parents=True)
    (attestation_scripts / "lib").mkdir()
    shutil.copy2(ATTESTATION_VERIFY, attestation_scripts / ATTESTATION_VERIFY.name)
    shutil.copy2(ATTESTATION_WRITE, attestation_scripts / ATTESTATION_WRITE.name)
    shutil.copy2(ATTESTATION_LIBRARY, attestation_scripts / "lib/npm-install-attestation.mjs")


def _copy_launcher_scripts(root: Path) -> None:
    scripts = root / "scripts"
    scripts.mkdir(parents=True)
    for source in (LAUNCHER, MCP_WRAPPER, WARM, PREFLIGHT, PROTOCOL_BUILD):
        shutil.copy2(source, scripts / source.name)
    # Copy the whole library directory. Enumerating its members here meant that
    # adding a scripts/lib file broke this fixture rather than exercising the
    # launcher it is meant to test.
    shutil.copytree(SCRIPT_LIBRARY_DIR, scripts / "lib")
    _copy_attestation_scripts(root)


def _commit_fixture(root: Path) -> None:
    subprocess.run(["git", "init", "--quiet"], cwd=root, check=True)
    subprocess.run(
        ["git", "config", "user.name", "Agent Fabric Fixture"],
        cwd=root,
        check=True,
    )
    subprocess.run(
        ["git", "config", "user.email", "agent-fabric-fixture@example.invalid"],
        cwd=root,
        check=True,
    )
    subprocess.run(["git", "add", "."], cwd=root, check=True)
    subprocess.run(["git", "commit", "--quiet", "-m", "fixture"], cwd=root, check=True)


def _write_fixture_attestation(root: Path) -> None:
    subprocess.run(
        ["node", str(root / "runtime/agent-fabric/scripts/write-npm-ci-attestation.mjs"), str(root)],
        cwd=root,
        check=True,
        capture_output=True,
        text=True,
    )


def _fixture(
    tmp_path: Path,
    *,
    launcher_mode: str,
    protocol_dist: str,
) -> tuple[Path, Path, Path]:
    root = tmp_path / "agents"
    _copy_launcher_scripts(root)

    fabric_source = root / "runtime/agent-fabric/src/cli/main.ts"
    fabric_dist = root / "runtime/agent-fabric/dist/cli/main.js"
    protocol_source = root / "runtime/agent-fabric-protocol/src/index.ts"
    protocol_output = root / "runtime/agent-fabric-protocol/dist/index.js"
    _write(fabric_source)
    _write(protocol_source)
    # A real install carries the package manifest beside the loader, and
    # resolution checks it: the loader path is executed, so a bare file of the
    # right name is not evidence that it is really tsx.
    _write(root / "node_modules/tsx/dist/loader.mjs")
    _write(root / "node_modules/.keep")
    _write(root / "node_modules/tsx/package.json", '{"name":"tsx","version":"4.0.0"}\n')
    now = 1_700_000_000
    for manifest, content in {
        "package.json": '{"type":"module"}\n',
        "package-lock.json": '{"lockfileVersion":3,"packages":{}}\n',
        "tsconfig.json": '{"files":[]}\n',
    }.items():
        _write(root / manifest, content)
        os.utime(root / manifest, (now, now))
    os.utime(fabric_source, (now, now))
    os.utime(protocol_source, (now, now))
    if launcher_mode == "packaged":
        _write(fabric_dist)
        os.utime(fabric_dist, (now + 30, now + 30))
    if protocol_dist != "missing":
        _write(protocol_output)
        output_time = now + 20 if protocol_dist == "current" else now - 20
        os.utime(protocol_output, (output_time, output_time))

    _commit_fixture(root)
    _write_fixture_attestation(root)

    marker = tmp_path / "daemon-election-attempt"
    fake_node = tmp_path / "bin" / "node"
    # The stub exists to capture the final exec, not to stand in for node
    # everywhere. Helper invocations — the loader resolution probes, and the
    # module-type check above — must reach the real interpreter, or the helper
    # is being tested against a shell script rather than against node.
    real_node = shutil.which("node") or "/usr/bin/env node"
    _write(
        fake_node,
        "#!/bin/sh\n"
        '[ "${1:-}" != "--input-type=module" ] || exit 0\n'
        f'[ "${{1:-}}" != "-e" ] || exec "{real_node}" "$@"\n'
        f'case "${{1:-}}" in */verify-npm-ci-attestation.mjs|*/write-npm-ci-attestation.mjs) exec "{real_node}" "$@" ;; esac\n'
        "printf '%s\\n' \"$@\" > \"$LAUNCHER_TEST_MARKER\"\n"
        "if [ -n \"${LAUNCHER_TEST_ENV_MARKER:-}\" ]; then\n"
        "  printf '%s\\n%s\\n' \"${AGENT_FABRIC_PROTOCOL_BUILD_VERDICT:-}\" "
        "\"${AGENT_FABRIC_PROTOCOL_BUILD_REPAIR:-}\" > \"$LAUNCHER_TEST_ENV_MARKER\"\n"
        "fi\n",
    )
    fake_node.chmod(0o755)
    return root, marker, fake_node


def _run(root: Path, marker: Path, fake_node: Path) -> subprocess.CompletedProcess[str]:
    return subprocess.run(
        [str(root / "scripts/agent-fabric"), "doctor"],
        cwd=root,
        env={
            **os.environ,
            "AGENTS_HOME": str(root),
            "LAUNCHER_TEST_MARKER": str(marker),
            "LAUNCHER_TEST_ENV_MARKER": str(marker.with_name("launcher-environment")),
            "PATH": f"{fake_node.parent}:{os.environ['PATH']}",
        },
        text=True,
        capture_output=True,
        check=False,
    )


def _real_source_fixture(tmp_path: Path, protocol_dist: str) -> Path:
    root = tmp_path / "real-agents"
    _copy_launcher_scripts(root)
    _write(
        root / "runtime/agent-fabric/src/cli/main.ts",
        'import { fixtureValue } from "@local/agent-fabric-protocol";\n'
        "process.stdout.write(`${fixtureValue}\\n`);\n",
    )
    _write(root / "runtime/agent-fabric/package.json", '{"type":"module"}\n')
    protocol_root = root / "runtime/agent-fabric-protocol"
    _write(
        protocol_root / "package.json",
        '{"name":"@local/agent-fabric-protocol","type":"module","exports":'
        '{".":{"source":"./src/index.ts","types":"./dist/index.d.ts",'
        '"import":"./dist/index.js"}}}\n',
    )
    _write(protocol_root / "src/index.ts", 'export const fixtureValue = "source";\n')
    _write(root / "package.json", '{"type":"module"}\n')
    _write(root / "package-lock.json", '{"lockfileVersion":3,"packages":{}}\n')
    now = 1_700_000_000
    os.utime(root / "package.json", (now, now))
    os.utime(root / "package-lock.json", (now, now))
    os.utime(protocol_root / "package.json", (now, now))
    os.utime(protocol_root / "src/index.ts", (now, now))
    if protocol_dist != "missing":
        _write(protocol_root / "dist/index.js", 'export const fixtureValue = "dist";\n')
        output_time = now + 20 if protocol_dist == "current" else now - 20
        os.utime(protocol_root / "dist/index.js", (output_time, output_time))

    tsx_package_json = subprocess.run(
        ["node", "-p", "require.resolve('tsx/package.json')"],
        cwd=REPO_ROOT,
        text=True,
        capture_output=True,
        check=True,
    ).stdout.strip()
    node_modules = root / "node_modules"
    (node_modules / "@local").mkdir(parents=True)
    shutil.copytree(Path(tsx_package_json).parent, node_modules / "tsx", symlinks=True)
    shutil.copytree(REPO_ROOT / "node_modules/esbuild", node_modules / "esbuild", symlinks=True)
    esbuild_root = REPO_ROOT / "node_modules/@esbuild"
    esbuild_packages = sorted(
        (
            package
            for package in esbuild_root.iterdir()
            if package.is_dir()
        )
        if esbuild_root.is_dir()
        else ()
    )
    if not esbuild_packages:
        pytest.skip("no platform-specific esbuild package is installed under node_modules/@esbuild")
    (node_modules / "@esbuild").mkdir()
    for package in esbuild_packages:
        shutil.copytree(
            package,
            node_modules / "@esbuild" / package.name,
            symlinks=True,
        )
    (node_modules / "@local/agent-fabric-protocol").symlink_to(
        protocol_root,
        target_is_directory=True,
    )
    _commit_fixture(root)
    _write_fixture_attestation(root)
    return root


def _copy_protocol_consumer(root: Path, package: str) -> tuple[Path, Path]:
    source = CONSUMER_BINS[package]
    executable = root / "runtime" / package / "bin" / source.name
    executable.parent.mkdir(parents=True)
    shutil.copy2(source, executable)
    executable.chmod(0o755)
    if PROTOCOL_BIN_PREFLIGHT.exists():
        destination = (
            root
            / "runtime/agent-fabric-protocol/bin"
            / PROTOCOL_BIN_PREFLIGHT.name
        )
        destination.parent.mkdir(parents=True)
        shutil.copy2(PROTOCOL_BIN_PREFLIGHT, destination)
    marker = root / f"{package}-started"
    _write(
        root / "runtime" / package / "dist/bin.js",
        'await import("node:fs/promises").then(({ writeFile }) => '
        'writeFile(process.env.CONSUMER_TEST_MARKER, "started\\n"));\n',
    )
    return executable, marker


def _stale_refusal(agents_home: Path) -> str:
    return (
        "AGENT_FABRIC_PROTOCOL_BUILD_STALE: local "
        "@local/agent-fabric-protocol dist is missing, unloadable, or stale "
        "against its build inputs\n"
        f'repair: AGENTS_HOME="{agents_home}" '
        f'"{agents_home / "scripts/agent-fabric-protocol-build"}"\n'
    )


def _mark_install_root(root: Path) -> None:
    if not (root / ".git").exists():
        _commit_fixture(root)


def _use_derived_agents_home(env: dict[str, str]) -> None:
    for name in (
        "AGENTS_HOME",
        "AGENT_FABRIC_PROTOCOL_NO_AUTOBUILD",
        "AGENT_FABRIC_PROTOCOL_PREFLIGHT_MODE",
        "AGENT_FABRIC_PROTOCOL_AUTOBUILD",
        "AGENT_FABRIC_PROTOCOL_AUTOBUILD_WAIT_ONLY",
    ):
        env.pop(name, None)


@pytest.mark.parametrize("protocol_dist", ["missing", "current", "stale"])
def test_source_launcher_requires_current_protocol_dist_before_start(
    tmp_path: Path,
    protocol_dist: str,
) -> None:
    root, marker, fake_node = _fixture(
        tmp_path,
        launcher_mode="source",
        protocol_dist=protocol_dist,
    )

    result = _run(root, marker, fake_node)

    if protocol_dist in {"current", "stale"}:
        assert result.returncode == 0, result.stderr
        assert marker.read_text(encoding="utf-8").splitlines() == [
            "--import",
            str(root / "node_modules/tsx/dist/loader.mjs"),
            str(root / "runtime/agent-fabric/src/cli/main.ts"),
            "doctor",
        ]
        environment = marker.with_name("launcher-environment").read_text(
            encoding="utf-8",
        ).splitlines()
        if protocol_dist == "stale":
            assert environment == [
                "stale",
                f'AGENTS_HOME="{root}" "{root / "scripts/agent-fabric-protocol-build"}"',
            ]
        else:
            assert environment == ["", ""]
    else:
        assert result.returncode == 78
        assert "AGENT_FABRIC_PROTOCOL_BUILD_STALE" in result.stderr
        assert not marker.exists(), "stale protocol dist must fail before daemon election"


@pytest.mark.parametrize("protocol_dist", ["missing", "current", "stale"])
def test_source_launcher_imports_current_protocol_dist_with_real_node(
    tmp_path: Path,
    protocol_dist: str,
) -> None:
    root = _real_source_fixture(tmp_path, protocol_dist)

    result = subprocess.run(
        [str(root / "scripts/agent-fabric")],
        cwd=root,
        env={**os.environ, "AGENTS_HOME": str(root)},
        text=True,
        capture_output=True,
        check=False,
    )

    if protocol_dist == "current":
        assert result.returncode == 0, result.stderr
        assert result.stdout == "dist\n"
    else:
        assert result.returncode == 78
        assert "AGENT_FABRIC_PROTOCOL_BUILD_STALE" in result.stderr


def test_packaged_launcher_accepts_current_protocol_dist(tmp_path: Path) -> None:
    root, marker, fake_node = _fixture(
        tmp_path,
        launcher_mode="packaged",
        protocol_dist="current",
    )

    result = _run(root, marker, fake_node)

    assert result.returncode == 0, result.stderr
    assert marker.read_text(encoding="utf-8").splitlines() == [
        str(root / "runtime/agent-fabric/dist/cli/main.js"),
        "doctor",
    ]


def test_packaged_launcher_rejects_tampered_node_modules_before_dist(
    tmp_path: Path,
) -> None:
    root, marker, fake_node = _fixture(
        tmp_path,
        launcher_mode="packaged",
        protocol_dist="current",
    )
    with (root / "node_modules/tsx/dist/loader.mjs").open("a", encoding="utf-8") as stream:
        stream.write("tampered\n")

    result = _run(root, marker, fake_node)

    assert result.returncode == 1
    assert result.stdout == ""
    assert "NPM_INSTALL_ATTESTATION_MISMATCH" in result.stderr
    assert "node_modules was modified after npm ci" in result.stderr
    assert not marker.exists()


@pytest.mark.parametrize("package", sorted(CONSUMER_BINS))
@pytest.mark.parametrize("protocol_dist", ["missing", "stale"])
def test_node_bin_consumers_preflight_protocol_before_import(
    tmp_path: Path,
    package: str,
    protocol_dist: str,
) -> None:
    root, _launcher_marker, _fake_node = _fixture(
        tmp_path,
        launcher_mode="packaged",
        protocol_dist=protocol_dist,
    )
    executable, marker = _copy_protocol_consumer(root, package)

    result = subprocess.run(
        [str(executable)],
        cwd=root,
        env={
            **os.environ,
            "AGENTS_HOME": str(root),
            "CONSUMER_TEST_MARKER": str(marker),
        },
        text=True,
        capture_output=True,
        check=False,
    )

    assert result.returncode == 78
    assert "AGENT_FABRIC_PROTOCOL_BUILD_STALE" in result.stderr
    assert (
        f'repair: AGENTS_HOME="{root}" '
        f'"{root / "scripts/agent-fabric-protocol-build"}"'
    ) in result.stderr
    assert not marker.exists(), "consumer dist must not import before preflight"


@pytest.mark.parametrize("protocol_dist", ["missing"])
def test_packaged_launcher_reports_stale_protocol_before_election(
    tmp_path: Path,
    protocol_dist: str,
) -> None:
    root, marker, fake_node = _fixture(
        tmp_path,
        launcher_mode="packaged",
        protocol_dist=protocol_dist,
    )

    result = _run(root, marker, fake_node)

    assert result.returncode == 78
    assert "AGENT_FABRIC_PROTOCOL_BUILD_STALE" in result.stderr
    assert (
        f'repair: AGENTS_HOME="{root}" '
        f'"{root / "scripts/agent-fabric-protocol-build"}"'
    ) in result.stderr
    assert not marker.exists(), "stale protocol dist must fail before daemon election"


def test_packaged_doctor_runs_with_stale_protocol_verdict_and_exact_repair(
    tmp_path: Path,
) -> None:
    root, marker, fake_node = _fixture(
        tmp_path,
        launcher_mode="packaged",
        protocol_dist="stale",
    )

    result = _run(root, marker, fake_node)

    assert result.returncode == 0, result.stderr
    assert marker.read_text(encoding="utf-8").splitlines() == [
        str(root / "runtime/agent-fabric/dist/cli/main.js"),
        "doctor",
    ]
    assert marker.with_name("launcher-environment").read_text(
        encoding="utf-8",
    ).splitlines() == [
        "stale",
        f'AGENTS_HOME="{root}" "{root / "scripts/agent-fabric-protocol-build"}"',
    ]


def test_doctor_reports_stale_protocol_when_repair_script_is_missing(
    tmp_path: Path,
) -> None:
    root, marker, fake_node = _fixture(
        tmp_path,
        launcher_mode="packaged",
        protocol_dist="stale",
    )
    (root / "scripts/agent-fabric-protocol-build").unlink()

    result = _run(root, marker, fake_node)

    assert result.returncode == 0, result.stderr
    assert marker.read_text(encoding="utf-8").splitlines() == [
        str(root / "runtime/agent-fabric/dist/cli/main.js"),
        "doctor",
    ]
    assert marker.with_name("launcher-environment").read_text(
        encoding="utf-8",
    ).splitlines() == [
        "stale",
        f'AGENTS_HOME="{root}" "{root / "scripts/agent-fabric-protocol-build"}"',
    ]


def test_doctor_hard_blocks_when_protocol_dist_is_unloadable(tmp_path: Path) -> None:
    root, marker, _fake_node = _fixture(
        tmp_path,
        launcher_mode="packaged",
        protocol_dist="current",
    )
    _write(
        root / "runtime/agent-fabric-protocol/dist/index.js",
        'export * from "./missing.js";\n',
    )

    result = subprocess.run(
        [str(root / "scripts/agent-fabric"), "doctor"],
        cwd=root,
        env={**os.environ, "AGENTS_HOME": str(root)},
        text=True,
        capture_output=True,
        check=False,
    )

    assert result.returncode == 78
    assert "AGENT_FABRIC_PROTOCOL_BUILD_STALE" in result.stderr
    assert "repair:" in result.stderr
    assert not marker.exists(), "an unloadable protocol dist must block doctor"


def test_non_doctor_entrypoints_do_not_pay_for_the_loadability_probe(
    tmp_path: Path,
) -> None:
    """The probe is scoped to `doctor`, the caller that cannot run without it.

    Charging every entrypoint a Node start-up on the path that runs before each
    `agent-fabric` invocation buys nothing their own import does not establish a
    moment later, so a current-but-unloadable dist keeps its pre-#439 behaviour
    here: the preflight passes and the command's own import reports the fault.
    """
    root, marker, _fake_node = _fixture(
        tmp_path,
        launcher_mode="packaged",
        protocol_dist="current",
    )
    _write(
        root / "runtime/agent-fabric-protocol/dist/index.js",
        'export * from "./missing.js";\n',
    )

    ordinary = subprocess.run(
        [str(root / "scripts/agent-fabric-protocol-preflight")],
        cwd=root,
        env={**os.environ, "AGENTS_HOME": str(root)},
        text=True,
        capture_output=True,
        check=False,
    )
    doctor = subprocess.run(
        [str(root / "scripts/agent-fabric-protocol-preflight")],
        cwd=root,
        env={
            **os.environ,
            "AGENTS_HOME": str(root),
            "AGENT_FABRIC_PROTOCOL_PREFLIGHT_MODE": "doctor",
        },
        text=True,
        capture_output=True,
        check=False,
    )

    assert ordinary.returncode == 0, ordinary.stderr
    assert "AGENT_FABRIC_PROTOCOL_BUILD_STALE" not in ordinary.stderr
    assert doctor.returncode == 78
    assert "AGENT_FABRIC_PROTOCOL_BUILD_STALE" in doctor.stderr
    assert not marker.exists()


def test_non_doctor_subcommand_still_hard_blocks_on_stale_protocol(
    tmp_path: Path,
) -> None:
    root, marker, fake_node = _fixture(
        tmp_path,
        launcher_mode="packaged",
        protocol_dist="stale",
    )

    result = subprocess.run(
        [str(root / "scripts/agent-fabric"), "status"],
        cwd=root,
        env={
            **os.environ,
            "AGENTS_HOME": str(root),
            "LAUNCHER_TEST_MARKER": str(marker),
            "PATH": f"{fake_node.parent}:{os.environ['PATH']}",
        },
        text=True,
        capture_output=True,
        check=False,
    )

    assert result.returncode == 78
    assert "AGENT_FABRIC_PROTOCOL_BUILD_STALE" in result.stderr
    assert not marker.exists(), "non-doctor commands must stop before daemon election"


def test_unrelated_project_autobuilds_stale_install_root_once(
    tmp_path: Path,
) -> None:
    root, marker, fake_node = _fixture(
        tmp_path,
        launcher_mode="packaged",
        protocol_dist="stale",
    )
    _mark_install_root(root)
    unrelated = tmp_path / "unrelated-project"
    unrelated.mkdir()
    npm_marker, env = _repair_fixture(
        tmp_path,
        root,
        'mkdir -p "$dist"\n'
        "printf 'export const fixtureValue = \"dist\";\\n' > \"$dist/index.js\"\n",
    )
    _use_derived_agents_home(env)
    env.update(
        {
            "LAUNCHER_TEST_MARKER": str(marker),
            "PATH": f"{fake_node.parent}:{env['PATH']}",
        },
    )

    result = subprocess.run(
        [str(root / "scripts/agent-fabric"), "status"],
        cwd=unrelated,
        env=env,
        text=True,
        capture_output=True,
        check=False,
    )

    assert result.returncode == 0, result.stderr
    assert result.stdout == ""
    assert result.stderr == (
        f'agent-fabric protocol autobuild: stale dist at "{root}"; rebuilding\n'
    )
    assert npm_marker.read_text(encoding="utf-8").splitlines() == [
        "run build --workspace=@local/agent-fabric-protocol",
    ]
    assert marker.exists(), "the command must continue after the self-repair"


@pytest.mark.parametrize("disabled_value", ["1", ""], ids=["one", "empty"])
def test_no_autobuild_preserves_the_exact_stale_refusal(
    tmp_path: Path,
    disabled_value: str,
) -> None:
    root, marker, fake_node = _fixture(
        tmp_path,
        launcher_mode="packaged",
        protocol_dist="stale",
    )
    _mark_install_root(root)
    npm_marker, env = _repair_fixture(
        tmp_path,
        root,
        'mkdir -p "$dist"\n'
        "printf 'export const fixtureValue = \"dist\";\\n' > \"$dist/index.js\"\n",
    )
    _use_derived_agents_home(env)
    env.update(
        {
            "AGENT_FABRIC_PROTOCOL_NO_AUTOBUILD": disabled_value,
            "LAUNCHER_TEST_MARKER": str(marker),
            "PATH": f"{fake_node.parent}:{env['PATH']}",
        },
    )

    result = subprocess.run(
        [str(root / "scripts/agent-fabric"), "status"],
        cwd=tmp_path,
        env=env,
        text=True,
        capture_output=True,
        check=False,
    )

    assert result.returncode == 78
    assert result.stdout == ""
    assert result.stderr == _stale_refusal(root)
    assert not npm_marker.exists(), "NO_AUTOBUILD must prevent the build"
    assert not marker.exists(), "the refused command must not start"


def test_missing_dist_does_not_start_a_new_autobuild(
    tmp_path: Path,
) -> None:
    root, marker, fake_node = _fixture(
        tmp_path,
        launcher_mode="packaged",
        protocol_dist="missing",
    )
    _mark_install_root(root)
    npm_marker, env = _repair_fixture(
        tmp_path,
        root,
        'mkdir -p "$dist"\n'
        "printf 'export const fixtureValue = \"dist\";\\n' > \"$dist/index.js\"\n",
    )
    _use_derived_agents_home(env)
    env.update(
        {
            "LAUNCHER_TEST_MARKER": str(marker),
            "PATH": f"{fake_node.parent}:{env['PATH']}",
        },
    )

    result = subprocess.run(
        [str(root / "scripts/agent-fabric"), "status"],
        cwd=tmp_path,
        env=env,
        text=True,
        capture_output=True,
        check=False,
    )

    assert result.returncode == 78
    assert result.stdout == ""
    assert result.stderr == _stale_refusal(root)
    assert not npm_marker.exists()
    assert not marker.exists()


def test_missing_build_lock_library_does_not_autobuild(
    tmp_path: Path,
) -> None:
    root, marker, fake_node = _fixture(
        tmp_path,
        launcher_mode="packaged",
        protocol_dist="stale",
    )
    _mark_install_root(root)
    (root / "scripts/lib/agent-fabric-protocol-build-lock.sh").unlink()
    npm_marker, env = _repair_fixture(
        tmp_path,
        root,
        'mkdir -p "$dist"\n'
        "printf 'export const fixtureValue = \"dist\";\\n' > \"$dist/index.js\"\n",
    )
    _use_derived_agents_home(env)
    env.update(
        {
            "LAUNCHER_TEST_MARKER": str(marker),
            "PATH": f"{fake_node.parent}:{env['PATH']}",
        },
    )

    result = subprocess.run(
        [str(root / "scripts/agent-fabric"), "status"],
        cwd=tmp_path,
        env=env,
        text=True,
        capture_output=True,
        check=False,
    )

    assert result.returncode == 78
    assert result.stdout == ""
    assert result.stderr == _stale_refusal(root)
    assert not npm_marker.exists()
    assert not marker.exists()


def test_linked_worktree_stale_dist_keeps_the_exact_refusal(
    tmp_path: Path,
) -> None:
    root, marker, fake_node = _fixture(
        tmp_path,
        launcher_mode="packaged",
        protocol_dist="stale",
    )
    shutil.rmtree(root / ".git")
    _write(root / ".git", "gitdir: /external/common/worktrees/fixture\n")
    npm_marker, env = _repair_fixture(
        tmp_path,
        root,
        'mkdir -p "$dist"\n'
        "printf 'export const fixtureValue = \"dist\";\\n' > \"$dist/index.js\"\n",
    )
    _use_derived_agents_home(env)
    env.update(
        {
            "LAUNCHER_TEST_MARKER": str(marker),
            "PATH": f"{fake_node.parent}:{env['PATH']}",
        },
    )

    result = subprocess.run(
        [str(root / "scripts/agent-fabric"), "status"],
        cwd=tmp_path,
        env=env,
        text=True,
        capture_output=True,
        check=False,
    )

    assert result.returncode == 78
    assert result.stdout == ""
    assert result.stderr == _stale_refusal(root)
    assert not npm_marker.exists(), "a linked worktree must never autobuild"
    assert not marker.exists()


def test_inherited_agents_home_stale_dist_keeps_the_exact_refusal(
    tmp_path: Path,
) -> None:
    script_root, marker, fake_node = _fixture(
        tmp_path,
        launcher_mode="packaged",
        protocol_dist="current",
    )
    diagnosed = tmp_path / "other-tree"
    protocol_root = diagnosed / "runtime/agent-fabric-protocol"
    _write(protocol_root / "src/index.ts")
    _write(protocol_root / "dist/index.js", "export {};\n")
    now = 1_700_000_000
    os.utime(protocol_root / "dist/index.js", (now - 20, now - 20))
    os.utime(protocol_root / "src/index.ts", (now, now))
    for manifest, content in {
        "package.json": '{"type":"module"}\n',
        "package-lock.json": '{"lockfileVersion":3,"packages":{}}\n',
        "tsconfig.json": '{"files":[]}\n',
    }.items():
        _write(diagnosed / manifest, content)
        os.utime(diagnosed / manifest, (now, now))
    _mark_install_root(diagnosed)
    _write(diagnosed / "node_modules/.keep")
    (diagnosed / "scripts").mkdir()
    shutil.copy2(PROTOCOL_BUILD, diagnosed / "scripts/agent-fabric-protocol-build")
    _copy_attestation_scripts(diagnosed)
    _write_fixture_attestation(diagnosed)
    npm_marker, env = _repair_fixture(
        tmp_path,
        diagnosed,
        'mkdir -p "$dist"\n'
        "printf 'export const fixtureValue = \"dist\";\\n' > \"$dist/index.js\"\n",
    )
    env.update(
        {
            "LAUNCHER_TEST_MARKER": str(marker),
            "PATH": f"{fake_node.parent}:{env['PATH']}",
        },
    )

    result = subprocess.run(
        [str(script_root / "scripts/agent-fabric"), "status"],
        cwd=tmp_path,
        env=env,
        text=True,
        capture_output=True,
        check=False,
    )

    assert result.returncode == 78
    assert result.stdout == ""
    assert result.stderr == _stale_refusal(diagnosed)
    assert not npm_marker.exists(), "an inherited AGENTS_HOME must never autobuild"
    assert not marker.exists()


def test_doctor_reports_stale_owned_root_without_autobuilding(
    tmp_path: Path,
) -> None:
    root, _marker, _fake_node = _fixture(
        tmp_path,
        launcher_mode="packaged",
        protocol_dist="stale",
    )
    _mark_install_root(root)
    protocol_dist = root / "runtime/agent-fabric-protocol/dist/index.js"
    _write(protocol_dist, "export {};\n")
    os.utime(protocol_dist, (1_699_999_980, 1_699_999_980))
    npm_marker, env = _repair_fixture(
        tmp_path,
        root,
        'mkdir -p "$dist"\n'
        "printf 'export const fixtureValue = \"dist\";\\n' > \"$dist/index.js\"\n",
    )
    _use_derived_agents_home(env)
    env["AGENT_FABRIC_PROTOCOL_PREFLIGHT_MODE"] = "doctor"

    result = subprocess.run(
        [str(root / "scripts/agent-fabric-protocol-preflight")],
        cwd=tmp_path,
        env=env,
        text=True,
        capture_output=True,
        check=False,
    )

    assert result.returncode == 0, result.stderr
    assert result.stderr == ""
    assert result.stdout == (
        f'AGENTS_HOME="{root}" '
        f'"{root / "scripts/agent-fabric-protocol-build"}"\n'
    )
    assert not npm_marker.exists(), "doctor reports; it does not repair"


def test_doctor_unloadable_owned_root_remains_a_hard_refusal(
    tmp_path: Path,
) -> None:
    root, _marker, _fake_node = _fixture(
        tmp_path,
        launcher_mode="packaged",
        protocol_dist="current",
    )
    _mark_install_root(root)
    _write(
        root / "runtime/agent-fabric-protocol/dist/index.js",
        'export * from "./missing.js";\n',
    )
    npm_marker, env = _repair_fixture(
        tmp_path,
        root,
        'mkdir -p "$dist"\n'
        "printf 'export const fixtureValue = \"dist\";\\n' > \"$dist/index.js\"\n",
    )
    _use_derived_agents_home(env)
    env["AGENT_FABRIC_PROTOCOL_PREFLIGHT_MODE"] = "doctor"

    result = subprocess.run(
        [str(root / "scripts/agent-fabric-protocol-preflight")],
        cwd=tmp_path,
        env=env,
        text=True,
        capture_output=True,
        check=False,
    )

    assert result.returncode == 78
    assert result.stdout == ""
    assert result.stderr == _stale_refusal(root)
    assert not npm_marker.exists()


def test_mcp_wrapper_autobuilds_when_agents_home_was_not_inherited(
    tmp_path: Path,
) -> None:
    root, marker, fake_node = _fixture(
        tmp_path,
        launcher_mode="packaged",
        protocol_dist="stale",
    )
    _mark_install_root(root)
    _write(root / "runtime/agent-fabric/dist/mcp/main.js")
    _write(root / "runtime/agent-fabric/src/mcp/main.ts")
    npm_marker, env = _repair_fixture(
        tmp_path,
        root,
        'mkdir -p "$dist"\n'
        "printf 'export const fixtureValue = \"dist\";\\n' > \"$dist/index.js\"\n",
    )
    _use_derived_agents_home(env)
    env.update(
        {
            "LAUNCHER_TEST_MARKER": str(marker),
            "PATH": f"{fake_node.parent}:{env['PATH']}",
        },
    )

    result = subprocess.run(
        [str(root / "scripts/agent-fabric-mcp")],
        cwd=tmp_path,
        env=env,
        text=True,
        capture_output=True,
        check=False,
    )

    assert result.returncode == 0, result.stderr
    assert result.stdout == ""
    assert result.stderr == (
        f'agent-fabric protocol autobuild: stale dist at "{root}"; rebuilding\n'
    )
    assert npm_marker.read_text(encoding="utf-8").splitlines() == [
        "run build --workspace=@local/agent-fabric-protocol",
    ]
    assert marker.exists()


def test_node_bin_consumer_autobuilds_when_agents_home_was_not_inherited(
    tmp_path: Path,
) -> None:
    root, _launcher_marker, _fake_node = _fixture(
        tmp_path,
        launcher_mode="packaged",
        protocol_dist="stale",
    )
    _mark_install_root(root)
    executable, marker = _copy_protocol_consumer(root, "agent-fabric-herdr")
    npm_marker, env = _repair_fixture(
        tmp_path,
        root,
        'mkdir -p "$dist"\n'
        "printf 'export const fixtureValue = \"dist\";\\n' > \"$dist/index.js\"\n",
    )
    _use_derived_agents_home(env)
    env["CONSUMER_TEST_MARKER"] = str(marker)

    result = subprocess.run(
        [str(executable)],
        cwd=tmp_path,
        env=env,
        text=True,
        capture_output=True,
        check=False,
    )

    assert result.returncode == 0, result.stderr
    assert result.stdout == ""
    assert result.stderr == (
        f'agent-fabric protocol autobuild: stale dist at "{root}"; rebuilding\n'
    )
    assert npm_marker.read_text(encoding="utf-8").splitlines() == [
        "run build --workspace=@local/agent-fabric-protocol",
    ]
    assert marker.exists()


@pytest.mark.parametrize("protocol_dist", ["missing", "stale"])
def test_mcp_wrapper_reports_stale_protocol_before_proxying(
    tmp_path: Path,
    protocol_dist: str,
) -> None:
    """The MCP wrapper is where a stale protocol dist actually bites.

    It surfaces there as a SyntaxError about a missing export, which the
    wrapper's Python caller reports as `JSONDecodeError: Expecting value`
    naming nothing about builds. The wrapper must fail with the typed,
    repairable error instead of proxying.
    """
    root, marker, fake_node = _fixture(
        tmp_path,
        launcher_mode="packaged",
        protocol_dist=protocol_dist,
    )
    _write(root / "runtime/agent-fabric/dist/mcp/main.js")
    _write(root / "runtime/agent-fabric/src/mcp/main.ts")

    result = subprocess.run(
        [str(root / "scripts/agent-fabric-mcp")],
        cwd=root,
        env={
            **os.environ,
            "AGENTS_HOME": str(root),
            "LAUNCHER_TEST_MARKER": str(marker),
            "PATH": f"{fake_node.parent}:{os.environ['PATH']}",
        },
        text=True,
        capture_output=True,
        check=False,
    )

    assert result.returncode == 78
    assert "AGENT_FABRIC_PROTOCOL_BUILD_STALE" in result.stderr
    assert (
        f'repair: AGENTS_HOME="{root}" '
        f'"{root / "scripts/agent-fabric-protocol-build"}"'
    ) in result.stderr
    assert not marker.exists(), "stale protocol dist must fail before the proxy starts"


def _repair_fixture(tmp_path: Path, root: Path, emit: str) -> tuple[Path, dict[str, str]]:
    """Run the repair against a fake npm whose emit the test chooses.

    Real `node` must stay on PATH and the fixture's fake one must not: the
    build script verifies the dist by importing it, which is the whole point of
    the emit variants below.
    """
    npm_marker = tmp_path / "npm-invocations"
    npm_bin = tmp_path / "npm-bin"
    npm_bin.mkdir()
    fake_npm = npm_bin / "npm"
    _write(
        fake_npm,
        '#!/bin/sh\nprintf \'%s\\n\' "$*" > "$NPM_TEST_MARKER"\n'
        'dist="$AGENTS_HOME/runtime/agent-fabric-protocol/dist"\n'
        f"{emit}\n",
    )
    fake_npm.chmod(0o755)
    return npm_marker, {
        **os.environ,
        "AGENTS_HOME": str(root),
        "NPM_TEST_MARKER": str(npm_marker),
        "PATH": f"{npm_bin}:{os.environ['PATH']}",
    }


def _concurrent_repair_fixture(
    tmp_path: Path,
    root: Path,
) -> tuple[Path, Path, dict[str, str]]:
    """Fake a non-atomic tsc emit and record any overlapping observer.

    The first npm process deliberately leaves protocol dist half-written while
    it gives an unlocked contender time to enter. A contender probes those
    bytes and records the raw SyntaxError condition that the repair lock exists
    to prevent.
    """
    npm_marker = tmp_path / "npm-invocations"
    overlap_marker = tmp_path / "half-written-observed"
    active = tmp_path / "npm-active"
    contender_observed = tmp_path / "npm-contender-observed"
    npm_bin = tmp_path / "concurrent-npm-bin"
    npm_bin.mkdir()
    fake_npm = npm_bin / "npm"
    _write(
        fake_npm,
        "#!/bin/sh\n"
        "set -eu\n"
        "printf '%s\\n' \"$*\" >> \"$NPM_TEST_MARKER\"\n"
        'dist="$AGENTS_HOME/runtime/agent-fabric-protocol/dist"\n'
        'if mkdir "$RACE_TEST_ACTIVE" 2>/dev/null; then\n'
        "  trap 'rmdir \"$RACE_TEST_ACTIVE\" 2>/dev/null || :' EXIT HUP INT TERM\n"
        '  mkdir -p "$dist"\n'
        "  printf 'export const fixtureValue = \"' > \"$dist/index.js\"\n"
        "  attempts=0\n"
        '  while [ ! -f "$RACE_TEST_CONTENDER_OBSERVED" ] && [ "$attempts" -lt 100 ]; do\n'
        "    sleep 0.02\n"
        "    attempts=$((attempts + 1))\n"
        "  done\n"
        "  printf 'dist\";\\n' >> \"$dist/index.js\"\n"
        "else\n"
        "  if ! node --input-type=module -e 'await import(process.argv[1]);' "
        '"$dist/index.js" >/dev/null 2>&1; then\n'
        '    : > "$RACE_TEST_OVERLAP_MARKER"\n'
        "  fi\n"
        '  : > "$RACE_TEST_CONTENDER_OBSERVED"\n'
        '  mkdir -p "$dist"\n'
        "  printf 'export const fixtureValue = \"dist\";\\n' > \"$dist/index.js\"\n"
        "fi\n"
        "for output in \\\n"
        '  "$AGENTS_HOME/runtime/agent-fabric/dist/cli/main.js" \\\n'
        '  "$AGENTS_HOME/runtime/agent-fabric/dist/mcp/main.js" \\\n'
        '  "$AGENTS_HOME/runtime/agent-fabric-herdr/dist/bin.js" \\\n'
        '  "$AGENTS_HOME/runtime/agent-fabric-console/dist/bin.js"\n'
        "do\n"
        '  mkdir -p "${output%/*}"\n'
        "  printf 'export {};\\n' > \"$output\"\n"
        "done\n",
    )
    fake_npm.chmod(0o755)
    return npm_marker, overlap_marker, {
        **os.environ,
        "AGENTS_HOME": str(root),
        "NPM_TEST_MARKER": str(npm_marker),
        "RACE_TEST_ACTIVE": str(active),
        "RACE_TEST_CONTENDER_OBSERVED": str(contender_observed),
        "RACE_TEST_OVERLAP_MARKER": str(overlap_marker),
        "PATH": f"{npm_bin}:{os.environ['PATH']}",
    }


def _concurrent_autobuild_fixture(
    tmp_path: Path,
    root: Path,
    *,
    emit: str = (
        'mkdir -p "$dist"\n'
        "printf 'export const fixtureValue = \"dist\";\\n' > \"$dist/index.js\""
    ),
) -> tuple[Path, Path, dict[str, str]]:
    npm_marker = tmp_path / "autobuild-npm-invocations"
    release = tmp_path / "release-autobuild"
    npm_bin = tmp_path / "autobuild-npm-bin"
    npm_bin.mkdir()
    fake_npm = npm_bin / "npm"
    _write(
        fake_npm,
        "#!/bin/sh\n"
        "set -eu\n"
        "printf '%s\\n' \"$*\" >> \"$NPM_TEST_MARKER\"\n"
        'dist="$AGENTS_HOME/runtime/agent-fabric-protocol/dist"\n'
        'mkdir -p "$dist"\n'
        "printf 'export {};\\n' > \"$dist/index.js\"\n"
        "attempts=0\n"
        'while [ ! -f "$AUTOBUILD_TEST_RELEASE" ] && [ "$attempts" -lt 500 ]; do\n'
        "  sleep 0.01\n"
        "  attempts=$((attempts + 1))\n"
        "done\n"
        f"{emit}\n",
    )
    fake_npm.chmod(0o755)
    env = dict(os.environ)
    _use_derived_agents_home(env)
    env.update(
        {
            "NPM_TEST_MARKER": str(npm_marker),
            "AUTOBUILD_TEST_RELEASE": str(release),
            "PATH": f"{npm_bin}:{os.environ['PATH']}",
        },
    )
    return npm_marker, release, env


def _make_all_non_protocol_outputs_current(root: Path) -> None:
    now = 1_700_000_100
    for workspace, outputs in {
        "agent-fabric": ["dist/cli/main.js", "dist/mcp/main.js"],
        "agent-fabric-herdr": ["dist/bin.js"],
        "agent-fabric-console": ["dist/bin.js"],
    }.items():
        source = root / "runtime" / workspace / "src/index.ts"
        _write(source)
        os.utime(source, (now, now))
        for relative in outputs:
            output = root / "runtime" / workspace / relative
            _write(output, "export {};\n")
            os.utime(output, (now + 20, now + 20))


def test_protocol_build_and_warm_serialize_non_atomic_emit_and_loser_rechecks(
    tmp_path: Path,
) -> None:
    root, _marker, _fake_node = _fixture(
        tmp_path,
        launcher_mode="packaged",
        protocol_dist="stale",
    )
    _write(root / "node_modules/.keep")
    _make_all_non_protocol_outputs_current(root)
    npm_marker, overlap_marker, env = _concurrent_repair_fixture(tmp_path, root)

    build = subprocess.Popen(
        [str(root / "scripts/agent-fabric-protocol-build")],
        cwd=root,
        env=env,
        text=True,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
    )
    warm = subprocess.Popen(
        [str(root / "scripts/agent-fabric-warm")],
        cwd=root,
        env=env,
        text=True,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
    )
    build_stdout, build_stderr = build.communicate(timeout=15)
    warm_stdout, warm_stderr = warm.communicate(timeout=15)

    assert not overlap_marker.exists(), (
        "the waiting repair must never enter npm while dist/index.js is half-written"
    )
    assert build.returncode == 0, f"{build_stdout}\n{build_stderr}"
    assert warm.returncode == 0, f"{warm_stdout}\n{warm_stderr}"
    assert len(npm_marker.read_text(encoding="utf-8").splitlines()) == 1, (
        "the loser must re-check the completed stamped dist instead of rebuilding"
    )
    loaded = subprocess.run(
        [
            "node",
            "--input-type=module",
            "-e",
            "await import(process.argv[1]);",
            str(root / "runtime/agent-fabric-protocol/dist/index.js"),
        ],
        text=True,
        capture_output=True,
        check=False,
    )
    assert loaded.returncode == 0, loaded.stderr


def test_racing_entrypoints_share_one_autobuild_and_both_succeed(
    tmp_path: Path,
) -> None:
    root, marker, fake_node = _fixture(
        tmp_path,
        launcher_mode="packaged",
        protocol_dist="stale",
    )
    _mark_install_root(root)
    npm_marker, release, env = _concurrent_autobuild_fixture(tmp_path, root)
    env.update(
        {
            "LAUNCHER_TEST_MARKER": str(marker),
            "PATH": f"{fake_node.parent}:{env['PATH']}",
        },
    )

    first = subprocess.Popen(
        [str(root / "scripts/agent-fabric"), "status"],
        cwd=tmp_path,
        env=env,
        text=True,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
    )
    second: subprocess.Popen[str] | None = None
    try:
        _wait_for_path(npm_marker)
        second = subprocess.Popen(
            [str(root / "scripts/agent-fabric"), "status"],
            cwd=tmp_path,
            env=env,
            text=True,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
        )
        time.sleep(0.1)
        assert first.poll() is None
        assert second.poll() is None, "the contender must wait while the build lock is held"
        _write(release)
        first_stdout, first_stderr = first.communicate(timeout=15)
        second_stdout, second_stderr = second.communicate(timeout=15)
    finally:
        release.touch()
        for process in (first, second):
            if process is not None and process.poll() is None:
                process.terminate()
                process.communicate(timeout=5)

    assert first.returncode == 0, first_stderr
    assert second is not None
    assert second.returncode == 0, second_stderr
    assert first_stdout == second_stdout == ""
    assert npm_marker.read_text(encoding="utf-8").splitlines() == [
        "run build --workspace=@local/agent-fabric-protocol",
    ]
    assert (first_stderr + second_stderr).splitlines() == [
        f'agent-fabric protocol autobuild: stale dist at "{root}"; rebuilding',
    ]


def test_no_autobuild_refuses_during_an_in_progress_partial_emit(
    tmp_path: Path,
) -> None:
    root, marker, fake_node = _fixture(
        tmp_path,
        launcher_mode="packaged",
        protocol_dist="stale",
    )
    _mark_install_root(root)
    npm_marker, release, env = _concurrent_autobuild_fixture(tmp_path, root)
    env.update(
        {
            "LAUNCHER_TEST_MARKER": str(marker),
            "PATH": f"{fake_node.parent}:{env['PATH']}",
        },
    )
    strict_env = dict(env)
    strict_env["AGENT_FABRIC_PROTOCOL_NO_AUTOBUILD"] = "1"

    builder = subprocess.Popen(
        [str(root / "scripts/agent-fabric"), "status"],
        cwd=tmp_path,
        env=env,
        text=True,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
    )
    try:
        _wait_for_path(npm_marker)
        _wait_for_path(root / "runtime/agent-fabric-protocol/dist/index.js")
        strict = subprocess.run(
            [str(root / "scripts/agent-fabric"), "status"],
            cwd=tmp_path,
            env=strict_env,
            text=True,
            capture_output=True,
            check=False,
            timeout=5,
        )
        assert builder.poll() is None
        _write(release)
        builder_stdout, builder_stderr = builder.communicate(timeout=15)
    finally:
        release.touch()
        if builder.poll() is None:
            builder.terminate()
            builder.communicate(timeout=5)

    assert strict.returncode == 78
    assert strict.stdout == ""
    assert strict.stderr == _stale_refusal(root)
    assert builder.returncode == 0, builder_stderr
    assert builder_stdout == ""
    assert npm_marker.read_text(encoding="utf-8").splitlines() == [
        "run build --workspace=@local/agent-fabric-protocol",
    ]


def test_failed_autobuild_refuses_and_leaves_no_partial_dist(
    tmp_path: Path,
) -> None:
    root, marker, fake_node = _fixture(
        tmp_path,
        launcher_mode="packaged",
        protocol_dist="stale",
    )
    _mark_install_root(root)
    npm_marker, env = _repair_fixture(
        tmp_path,
        root,
        'mkdir -p "$dist"\n'
        "printf 'export const partial = ' > \"$dist/index.js\"\n"
        "exit 1",
    )
    _use_derived_agents_home(env)
    env.update(
        {
            "LAUNCHER_TEST_MARKER": str(marker),
            "PATH": f"{fake_node.parent}:{env['PATH']}",
        },
    )

    result = subprocess.run(
        [str(root / "scripts/agent-fabric"), "status"],
        cwd=tmp_path,
        env=env,
        text=True,
        capture_output=True,
        check=False,
    )

    assert result.returncode == 78
    assert result.stdout == ""
    assert result.stderr.endswith(_stale_refusal(root))
    assert (
        f'agent-fabric protocol autobuild: stale dist at "{root}"; rebuilding\n'
        in result.stderr
    )
    assert (
        "agent-fabric protocol build failed; dist removed so the workspace "
        "stays honestly stale\n"
        in result.stderr
    )
    assert npm_marker.read_text(encoding="utf-8").splitlines() == [
        "run build --workspace=@local/agent-fabric-protocol",
    ]
    assert not (root / "runtime/agent-fabric-protocol/dist").exists()
    assert not marker.exists()


def test_interrupted_autobuild_removes_partial_dist_before_refusing(
    tmp_path: Path,
) -> None:
    root, marker, fake_node = _fixture(
        tmp_path,
        launcher_mode="packaged",
        protocol_dist="stale",
    )
    _mark_install_root(root)
    npm_marker, env = _repair_fixture(
        tmp_path,
        root,
        'mkdir -p "$dist"\n'
        "printf 'export const partial = ' > \"$dist/index.js\"\n"
        'kill -TERM "$PPID"\n'
        "sleep 0.1\n"
        "exit 1",
    )
    _use_derived_agents_home(env)
    env.update(
        {
            "LAUNCHER_TEST_MARKER": str(marker),
            "PATH": f"{fake_node.parent}:{env['PATH']}",
        },
    )

    interrupted = subprocess.run(
        [str(root / "scripts/agent-fabric"), "status"],
        cwd=tmp_path,
        env=env,
        text=True,
        capture_output=True,
        check=False,
        timeout=10,
    )
    retried = subprocess.run(
        [str(root / "scripts/agent-fabric-protocol-preflight")],
        cwd=tmp_path,
        env=env,
        text=True,
        capture_output=True,
        check=False,
        timeout=5,
    )

    assert interrupted.returncode == 78
    assert interrupted.stdout == ""
    assert interrupted.stderr.endswith(_stale_refusal(root))
    assert retried.returncode == 78
    assert retried.stdout == ""
    assert retried.stderr == _stale_refusal(root)
    assert npm_marker.read_text(encoding="utf-8").splitlines() == [
        "run build --workspace=@local/agent-fabric-protocol",
    ]
    assert not (root / "runtime/agent-fabric-protocol/dist").exists()
    assert not (
        root
        / "runtime/agent-fabric-protocol"
        / ".dist.agent-fabric-protocol-build.lock"
    ).exists()
    assert not marker.exists()


def test_waiter_does_not_retry_a_failed_autobuild(
    tmp_path: Path,
) -> None:
    root, marker, fake_node = _fixture(
        tmp_path,
        launcher_mode="packaged",
        protocol_dist="stale",
    )
    _mark_install_root(root)
    npm_marker, release, env = _concurrent_autobuild_fixture(
        tmp_path,
        root,
        emit=(
            'mkdir -p "$dist"\n'
            "printf 'export const partial = ' > \"$dist/index.js\"\n"
            "exit 1"
        ),
    )
    env.update(
        {
            "LAUNCHER_TEST_MARKER": str(marker),
            "PATH": f"{fake_node.parent}:{env['PATH']}",
        },
    )

    first = subprocess.Popen(
        [str(root / "scripts/agent-fabric"), "status"],
        cwd=tmp_path,
        env=env,
        text=True,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
    )
    second: subprocess.Popen[str] | None = None
    try:
        _wait_for_path(npm_marker)
        second = subprocess.Popen(
            [str(root / "scripts/agent-fabric"), "status"],
            cwd=tmp_path,
            env=env,
            text=True,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
        )
        time.sleep(0.1)
        assert second.poll() is None
        _write(release)
        first_stdout, first_stderr = first.communicate(timeout=15)
        second_stdout, second_stderr = second.communicate(timeout=15)
    finally:
        release.touch()
        for process in (first, second):
            if process is not None and process.poll() is None:
                process.terminate()
                process.communicate(timeout=5)

    assert first.returncode == 78
    assert second is not None
    assert second.returncode == 78
    assert first_stdout == second_stdout == ""
    assert first_stderr.endswith(_stale_refusal(root))
    assert second_stderr == _stale_refusal(root)
    assert npm_marker.read_text(encoding="utf-8").splitlines() == [
        "run build --workspace=@local/agent-fabric-protocol",
    ]
    assert not (root / "runtime/agent-fabric-protocol/dist").exists()
    assert not marker.exists()


def test_waiter_before_owner_publication_does_not_retry_failed_build(
    tmp_path: Path,
) -> None:
    root, marker, fake_node = _fixture(
        tmp_path,
        launcher_mode="packaged",
        protocol_dist="stale",
    )
    _mark_install_root(root)
    npm_marker, first_env = _repair_fixture(
        tmp_path,
        root,
        'mkdir -p "$dist"\n'
        "printf 'export const partial = ' > \"$dist/index.js\"\n"
        "exit 1",
    )
    _use_derived_agents_home(first_env)
    hooks = tmp_path / "owner-publication-hooks"
    hooks.mkdir()
    first_env.update(
        {
            "AGENT_FABRIC_PROTOCOL_BUILD_LOCK_TEST_HOOK_DIRECTORY": str(hooks),
            "LAUNCHER_TEST_MARKER": str(marker),
            "PATH": f"{fake_node.parent}:{first_env['PATH']}",
        },
    )
    second_env = dict(first_env)
    second_env.pop("AGENT_FABRIC_PROTOCOL_BUILD_LOCK_TEST_HOOK_DIRECTORY")

    first = subprocess.Popen(
        [str(root / "scripts/agent-fabric"), "status"],
        cwd=tmp_path,
        env=first_env,
        text=True,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
    )
    second: subprocess.Popen[str] | None = None
    try:
        ready = _wait_for_glob(hooks, "*.owner-publish.ready")
        lock = (
            root
            / "runtime/agent-fabric-protocol"
            / ".dist.agent-fabric-protocol-build.lock"
        )
        assert lock.is_dir()
        assert not (lock / "owner").exists()
        second = subprocess.Popen(
            [str(root / "scripts/agent-fabric"), "status"],
            cwd=tmp_path,
            env=second_env,
            text=True,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
        )
        time.sleep(0.1)
        assert second.poll() is None
        _write(ready.with_name(ready.name.replace(".ready", ".continue")))
        first_stdout, first_stderr = first.communicate(timeout=15)
        second_stdout, second_stderr = second.communicate(timeout=15)
    finally:
        for process in (first, second):
            if process is not None and process.poll() is None:
                process.terminate()
                process.communicate(timeout=5)

    assert first.returncode == 78
    assert second is not None
    assert second.returncode == 78
    assert first_stdout == second_stdout == ""
    assert first_stderr.endswith(_stale_refusal(root))
    assert second_stderr == _stale_refusal(root)
    assert npm_marker.read_text(encoding="utf-8").splitlines() == [
        "run build --workspace=@local/agent-fabric-protocol",
    ]
    assert not (root / "runtime/agent-fabric-protocol/dist").exists()
    assert not marker.exists()


def test_protocol_build_recovers_a_crashed_holders_stale_lock(
    tmp_path: Path,
) -> None:
    root, _marker, _fake_node = _fixture(
        tmp_path,
        launcher_mode="packaged",
        protocol_dist="stale",
    )
    _write(root / "node_modules/.keep")
    npm_marker, env = _repair_fixture(
        tmp_path,
        root,
        'mkdir -p "$dist"\n'
        "printf 'export const fixtureValue = \"dist\";\\n' > \"$dist/index.js\"\n",
    )
    lock = (
        root
        / "runtime/agent-fabric-protocol"
        / ".dist.agent-fabric-protocol-build.lock"
    )
    _write(lock / "owner", "99999999\n")
    env["AGENT_FABRIC_PROTOCOL_BUILD_LOCK_TIMEOUT_SECONDS"] = "1"

    repaired = subprocess.run(
        [str(root / "scripts/agent-fabric-protocol-build")],
        cwd=root,
        env=env,
        text=True,
        capture_output=True,
        check=False,
        timeout=10,
    )

    assert repaired.returncode == 0, repaired.stderr
    assert npm_marker.read_text(encoding="utf-8").splitlines() == [
        "run build --workspace=@local/agent-fabric-protocol",
    ]
    assert not lock.exists(), "the recovered lock must be released after repair"


def test_dead_owner_reclaim_cannot_destroy_a_new_live_lock(
    tmp_path: Path,
) -> None:
    root, _marker, _fake_node = _fixture(
        tmp_path,
        launcher_mode="packaged",
        protocol_dist="stale",
    )
    _write(root / "node_modules/.keep")
    _npm_marker, env = _repair_fixture(
        tmp_path,
        root,
        'mkdir -p "$dist"\n'
        "printf 'export const fixtureValue = \"dist\";\\n' > \"$dist/index.js\"\n",
    )
    lock = (
        root
        / "runtime/agent-fabric-protocol"
        / ".dist.agent-fabric-protocol-build.lock"
    )
    _write(lock / "owner", "99999999\n")
    hooks = tmp_path / "reclaim-hooks"
    hooks.mkdir()
    env["AGENT_FABRIC_PROTOCOL_BUILD_LOCK_TEST_HOOK_DIRECTORY"] = str(hooks)
    env["AGENT_FABRIC_PROTOCOL_BUILD_LOCK_TIMEOUT_SECONDS"] = "20"

    waiters = [
        subprocess.Popen(
            [str(root / "scripts/agent-fabric-protocol-build")],
            cwd=root,
            env=env,
            text=True,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
        )
        for _ in range(2)
    ]
    first, second = waiters
    try:
        for waiter in waiters:
            _wait_for_path(hooks / f"{waiter.pid}.dead-owner-observed.ready")

        _write(hooks / f"{first.pid}.dead-owner-observed.continue")
        _wait_for_path(hooks / f"{first.pid}.dead-owner-remove.ready")
        _write(hooks / f"{second.pid}.dead-owner-observed.continue")

        second_remove = hooks / f"{second.pid}.dead-owner-remove.ready"
        second_lost = hooks / f"{second.pid}.dead-owner-reclaim-lost.ready"
        deadline = time.monotonic() + 10
        while time.monotonic() < deadline:
            if second_remove.exists() or second_lost.exists():
                break
            time.sleep(0.01)
        else:
            raise AssertionError("second waiter neither reached removal nor lost reclaim")

        _write(hooks / f"{first.pid}.dead-owner-remove.continue")
        _wait_for_path(hooks / f"{first.pid}.dead-owner-removed.ready")
        _write(lock / "owner", f"{os.getpid()}\n")

        if second_remove.exists():
            _write(hooks / f"{second.pid}.dead-owner-remove.continue")
            _wait_for_path(hooks / f"{second.pid}.dead-owner-removed.ready")

        assert lock.is_dir(), "a stale waiter destroyed the replacement live lock"
        assert (lock / "owner").read_text(encoding="utf-8") == f"{os.getpid()}\n"
    finally:
        for waiter in waiters:
            waiter.terminate()
        for waiter in waiters:
            waiter.communicate(timeout=5)


def test_protocol_build_lock_wait_is_bounded_and_typed(
    tmp_path: Path,
) -> None:
    root, _marker, _fake_node = _fixture(
        tmp_path,
        launcher_mode="packaged",
        protocol_dist="stale",
    )
    _write(root / "node_modules/.keep")
    npm_marker, env = _repair_fixture(
        tmp_path,
        root,
        'mkdir -p "$dist"\n'
        "printf 'export const fixtureValue = \"dist\";\\n' > \"$dist/index.js\"\n",
    )
    holder = subprocess.Popen(["sleep", "10"])
    lock = (
        root
        / "runtime/agent-fabric-protocol"
        / ".dist.agent-fabric-protocol-build.lock"
    )
    _write(lock / "owner", f"{holder.pid}\n")
    env["AGENT_FABRIC_PROTOCOL_BUILD_LOCK_TIMEOUT_SECONDS"] = "1"
    try:
        result = subprocess.run(
            [str(root / "scripts/agent-fabric-protocol-build")],
            cwd=root,
            env=env,
            text=True,
            capture_output=True,
            check=False,
            timeout=5,
        )
    finally:
        holder.terminate()
        holder.wait(timeout=5)

    assert result.returncode == 78
    assert "AGENT_FABRIC_PROTOCOL_BUILD_LOCK_TIMEOUT" in result.stderr
    assert (
        f'repair: AGENTS_HOME="{root}" '
        f'"{root / "scripts/agent-fabric-protocol-build"}"'
    ) in result.stderr
    assert not npm_marker.exists(), "a timed-out waiter must not enter the build"


def test_reported_protocol_repair_clears_staleness_without_root_build(
    tmp_path: Path,
) -> None:
    root, marker, fake_node = _fixture(
        tmp_path,
        launcher_mode="packaged",
        protocol_dist="stale",
    )
    _write(root / "node_modules/.keep")
    npm_marker, env = _repair_fixture(
        tmp_path,
        root,
        'mkdir -p "$dist"\n'
        "printf 'export const fixtureValue = \"dist\";\\n' > \"$dist/index.js\"\n",
    )

    repaired = subprocess.run(
        [str(root / "scripts/agent-fabric-protocol-build")],
        cwd=tmp_path,
        env=env,
        text=True,
        capture_output=True,
        check=False,
    )
    launched = _run(root, marker, fake_node)

    assert repaired.returncode == 0, repaired.stderr
    assert npm_marker.read_text(encoding="utf-8").splitlines() == [
        "run build --workspace=@local/agent-fabric-protocol",
    ]
    assert launched.returncode == 0, launched.stderr
    assert marker.exists()


def _run_preflight(root: Path) -> subprocess.CompletedProcess[str]:
    return subprocess.run(
        [str(root / "scripts/agent-fabric-protocol-preflight")],
        cwd=root,
        env={**os.environ, "AGENTS_HOME": str(root)},
        text=True,
        capture_output=True,
        check=False,
    )


def _successful_protocol_build(tmp_path: Path, root: Path) -> subprocess.CompletedProcess[str]:
    _write(root / "node_modules/.keep")
    _npm_marker, env = _repair_fixture(
        tmp_path,
        root,
        'mkdir -p "$dist"\n'
        "printf 'export const fixtureValue = \"dist\";\\n' > \"$dist/index.js\"\n",
    )
    return subprocess.run(
        [str(root / "scripts/agent-fabric-protocol-build")],
        cwd=tmp_path,
        env=env,
        text=True,
        capture_output=True,
        check=False,
    )


def test_unchanged_root_manifest_touch_does_not_block_entrypoints(
    tmp_path: Path,
) -> None:
    root, marker, fake_node = _fixture(
        tmp_path,
        launcher_mode="packaged",
        protocol_dist="current",
    )
    _write(root / "runtime/agent-fabric/dist/mcp/main.js")
    _write(root / "runtime/agent-fabric/src/mcp/main.ts")
    built = _successful_protocol_build(tmp_path, root)
    before_touch = _run_preflight(root)

    (root / "package-lock.json").touch()
    protocol_dist = root / "runtime/agent-fabric-protocol/dist/index.js"
    touched_time = protocol_dist.stat().st_mtime + 10
    os.utime(root / "package-lock.json", (touched_time, touched_time))

    after_touch = _run_preflight(root)
    launcher = subprocess.run(
        [str(root / "scripts/agent-fabric"), "status"],
        cwd=root,
        env={
            **os.environ,
            "AGENTS_HOME": str(root),
            "LAUNCHER_TEST_MARKER": str(marker),
            "PATH": f"{fake_node.parent}:{os.environ['PATH']}",
        },
        text=True,
        capture_output=True,
        check=False,
    )
    marker.unlink(missing_ok=True)
    mcp = subprocess.run(
        [str(root / "scripts/agent-fabric-mcp")],
        cwd=root,
        env={
            **os.environ,
            "AGENTS_HOME": str(root),
            "LAUNCHER_TEST_MARKER": str(marker),
            "PATH": f"{fake_node.parent}:{os.environ['PATH']}",
        },
        text=True,
        capture_output=True,
        check=False,
    )

    assert built.returncode == 0, built.stderr
    assert before_touch.returncode == 0, before_touch.stderr
    assert after_touch.returncode == 0, (
        "touching unchanged package-lock.json must not stale a byte-current protocol dist: "
        f"{after_touch.stderr}"
    )
    assert launcher.returncode == 0, launcher.stderr
    assert mcp.returncode == 0, mcp.stderr


def test_root_manifest_content_change_stales_even_when_its_mtime_is_older(
    tmp_path: Path,
) -> None:
    root, _marker, _fake_node = _fixture(
        tmp_path,
        launcher_mode="packaged",
        protocol_dist="current",
    )
    built = _successful_protocol_build(tmp_path, root)
    protocol_dist = root / "runtime/agent-fabric-protocol/dist/index.js"
    lockfile = root / "package-lock.json"
    _write(lockfile, "genuine dependency change\n")
    dist_time = protocol_dist.stat().st_mtime
    os.utime(lockfile, (dist_time - 10, dist_time - 10))

    result = _run_preflight(root)

    assert built.returncode == 0, built.stderr
    assert result.returncode == 78, (
        "changed root-manifest bytes must stale the protocol dist independently of mtime"
    )
    assert "AGENT_FABRIC_PROTOCOL_BUILD_STALE" in result.stderr


def test_missing_root_manifest_stamp_falls_back_to_mtime_rule(
    tmp_path: Path,
) -> None:
    root, _marker, _fake_node = _fixture(
        tmp_path,
        launcher_mode="packaged",
        protocol_dist="current",
    )
    built = _successful_protocol_build(tmp_path, root)
    stamp = root / "runtime/agent-fabric-protocol/dist" / ROOT_MANIFEST_STAMP
    assert built.returncode == 0, built.stderr
    assert stamp.exists(), "a successful protocol build must record its root-manifest digest"
    stamp.unlink()
    (root / "package-lock.json").touch()
    protocol_dist = root / "runtime/agent-fabric-protocol/dist/index.js"
    touched_time = protocol_dist.stat().st_mtime + 10
    os.utime(root / "package-lock.json", (touched_time, touched_time))

    result = _run_preflight(root)

    assert result.returncode == 78, (
        "a missing digest stamp must fail safe to the root-manifest mtime rule"
    )
    assert "AGENT_FABRIC_PROTOCOL_BUILD_STALE" in result.stderr


@pytest.mark.parametrize(
    "emit",
    [
        "exit 1",
        'mkdir -p "$dist"\n'
        "printf 'export * from \"./absent.js\";\\n' > \"$dist/index.js\"\n",
    ],
    ids=["build-fails", "build-emits-unloadable-dist"],
)
def test_failed_protocol_repair_leaves_the_workspace_honestly_stale(
    tmp_path: Path,
    emit: str,
) -> None:
    """A repair that did not repair must not clear the staleness signal.

    `dist/index.js` is a barrel, so its presence and mtime say nothing about
    whether the package loads: an incremental compile leaves a deleted sibling
    missing and still exits 0, and a type error still emits while the build's
    `&&` skips the schema generator. Stamping either would turn a detectable
    stale build into an undetectable one, so both must leave the tree absent.
    """
    root, marker, fake_node = _fixture(
        tmp_path,
        launcher_mode="packaged",
        protocol_dist="stale",
    )
    _write(root / "node_modules/.keep")
    _npm_marker, env = _repair_fixture(tmp_path, root, emit)

    repaired = subprocess.run(
        [str(root / "scripts/agent-fabric-protocol-build")],
        cwd=tmp_path,
        env=env,
        text=True,
        capture_output=True,
        check=False,
    )
    launched = _run(root, marker, fake_node)

    assert repaired.returncode == 1, repaired.stdout
    assert not (root / "runtime/agent-fabric-protocol/dist").exists()
    assert launched.returncode == 78
    assert "AGENT_FABRIC_PROTOCOL_BUILD_STALE" in launched.stderr
    assert not marker.exists()


def test_protocol_preflight_repair_names_the_diagnosed_tree(tmp_path: Path) -> None:
    """A repair uses the product tree whose protocol build was diagnosed.

    AGENTS_HOME names the product code tree, so the emitted command must use the
    same tree for both the environment and build script. A preflight can be
    invoked from a different checkout, which is reachable whenever worktrees
    are in play, but a split instance is selected by
    AGENT_FABRIC_INSTANCE_ROOT, not AGENTS_HOME; this preflight has no
    instance-root input.
    """
    root, _marker, _fake_node = _fixture(
        tmp_path,
        launcher_mode="packaged",
        protocol_dist="missing",
    )
    diagnosed = tmp_path / "diagnosed-product"
    _copy_launcher_scripts(diagnosed)

    result = subprocess.run(
        [str(root / "scripts/agent-fabric-protocol-preflight")],
        env={**os.environ, "AGENTS_HOME": str(diagnosed)},
        text=True,
        capture_output=True,
        check=False,
    )

    assert result.returncode == 78
    assert (
        f'repair: AGENTS_HOME="{diagnosed}" '
        f'"{diagnosed / "scripts/agent-fabric-protocol-build"}"'
    ) in result.stderr


def test_protocol_preflight_reports_a_partial_install_as_typed_and_repairable(
    tmp_path: Path,
) -> None:
    """The preflight now fronts every entrypoint.

    Without scripts/lib it would take them all out under `set -eu` with a bare
    "No such file or directory" and exit 1 — neither typed nor repairable.
    """
    root, _marker, _fake_node = _fixture(
        tmp_path,
        launcher_mode="packaged",
        protocol_dist="current",
    )
    (root / "scripts/lib/agent-fabric-workspace-freshness.sh").unlink()

    result = subprocess.run(
        [str(root / "scripts/agent-fabric-protocol-preflight")],
        env={**os.environ, "AGENTS_HOME": str(root)},
        text=True,
        capture_output=True,
        check=False,
    )

    assert result.returncode == 78
    assert "AGENT_FABRIC_PREFLIGHT_INCOMPLETE" in result.stderr
    assert "scripts/lib" in result.stderr


def test_protocol_preflight_does_not_emit_a_missing_repair_script(
    tmp_path: Path,
) -> None:
    root, _marker, _fake_node = _fixture(
        tmp_path,
        launcher_mode="packaged",
        protocol_dist="missing",
    )
    (root / "scripts/agent-fabric-protocol-build").unlink()

    result = subprocess.run(
        [str(root / "scripts/agent-fabric-protocol-preflight")],
        env={**os.environ, "AGENTS_HOME": str(root)},
        text=True,
        capture_output=True,
        check=False,
    )

    assert result.returncode == 78
    assert "AGENT_FABRIC_PROTOCOL_BUILD_INCOMPLETE" in result.stderr
    assert "agent-fabric-protocol-build" in result.stderr
    assert "repair: AGENTS_HOME=" not in result.stderr
    assert "scripts/lib" in result.stderr


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
    """Create a node_modules/tsx that satisfies the package identity check.

    The loader path is handed straight to `node --import`, so a bare file of
    the right name is not enough to prove it is really tsx.
    """
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
    """A linked git worktree has no node_modules of its own.

    npm hoists workspace dependencies to the repository root, so resolution
    must look above the worktree. Assuming node_modules sits directly beneath
    AGENTS_HOME broke every worktree whose TypeScript was newer than its build,
    because only then does the launcher take the source-tree fallback.
    """
    repository = tmp_path / "repo"
    _git_repository(repository)
    loader = _install_tsx(repository)
    worktree = repository / ".worktrees/impl-example"
    worktree.mkdir(parents=True)

    result = _resolve_tsx_loader(worktree)

    assert result.returncode == 0, result.stderr
    assert result.stdout.strip() == str(loader)


def test_tsx_loader_prefers_the_declaring_package(tmp_path: Path) -> None:
    """Resolution starts at the package that declares the dependency.

    The previous implementation began at AGENTS_HOME and so never looked in
    runtime/agent-fabric/node_modules, skipping the package's own tsx in
    favour of whatever an ancestor happened to hold.
    """
    repository = tmp_path / "repo"
    _git_repository(repository)
    hoisted = _install_tsx(repository)
    package_root = repository / "runtime/agent-fabric"
    package_root.mkdir(parents=True)
    own = _install_tsx(package_root)

    result = _resolve_tsx_loader(package_root, repository)

    assert result.returncode == 0, result.stderr
    assert result.stdout.strip() == str(own)
    assert result.stdout.strip() != str(hoisted)


def test_tsx_loader_refuses_a_loader_above_the_owning_repository(
    tmp_path: Path,
) -> None:
    """The resolved path is executed, so the walk must be bounded.

    An unbounded walk towards / will run a loader.mjs planted in $HOME or any
    intermediate directory. That contradicts the workspace trust doctrine,
    which refuses filesystem-root and home-wide authority.
    """
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
    """Any file named loader.mjs used to satisfy the probe."""
    repository = tmp_path / "repo"
    _git_repository(repository)
    _install_tsx(repository, name="not-tsx")

    result = _resolve_tsx_loader(repository)

    assert result.returncode == 1
    assert result.stdout.strip() == ""


def test_tsx_loader_ignores_a_node_modules_holding_no_packages(tmp_path: Path) -> None:
    """Vitest leaves a .vite cache in the worktree it runs from.

    That directory satisfies a `[ -d node_modules ]` test while containing no
    packages, so the probe must test for the loader file itself.
    """
    repository = tmp_path / "repo"
    _git_repository(repository)
    loader = _install_tsx(repository)
    worktree = repository / ".worktrees/impl-example"
    (worktree / "node_modules/.vite").mkdir(parents=True)

    result = _resolve_tsx_loader(worktree)

    assert result.returncode == 0, result.stderr
    assert result.stdout.strip() == str(loader)


def test_tsx_loader_resolves_through_a_symlinked_invocation(tmp_path: Path) -> None:
    """Invoked through a symlinked alias, a lexical walk climbs the link path.

    ~/.codex/skills/... is such an alias. Walking the lexical ancestry misses
    the real tree entirely, so resolution must canonicalise first.
    """
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
    """GIT_DIR and friends override what `git rev-parse` reports.

    An inherited environment naming an ancestor repository would otherwise
    move the boundary out to that ancestor, which is exactly the widening the
    boundary exists to prevent. The boundary must come from the workspace on
    disk, never from the caller's environment.
    """
    outer = tmp_path / "outer"
    _git_repository(outer)
    planted = _install_tsx(outer)
    repository = outer / "repo"
    _git_repository(repository)
    worktree = repository / ".worktrees/impl-example"
    worktree.mkdir(parents=True)

    result = subprocess.run(
        ["sh", "-c", f'. "{TSX_LOADER_LIBRARY}"; resolve_tsx_loader "{worktree}"'],
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
    """The resolved file is executed, so its own basename must be canonical.

    A loader.mjs that is itself a symlink runs its target. Checking only the
    link path, or canonicalising the directory and re-appending the name,
    proves nothing about what `node --import` will actually load.
    """
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
    """Identity is a JSON field, not a substring.

    A manifest may carry `"name": "tsx"` inside a description, a keyword list
    or a dependency entry without being tsx at all.
    """
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
    """The walk never meets a boundary that is not its ancestor.

    With PACKAGE_ROOT outside WORKSPACE, a loop that only stops *at* the
    boundary climbs past it to /, accepting anything it passed on the way. Each
    candidate must therefore be checked against the boundary, not just the
    stopping point.
    """
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
