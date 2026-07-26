from __future__ import annotations

import os
from pathlib import Path
import shutil
import subprocess

import pytest


REPO_ROOT = Path(__file__).resolve().parents[1]
LAUNCHER = REPO_ROOT / "scripts" / "agent-fabric"
FRESHNESS_LIBRARY = REPO_ROOT / "scripts" / "lib" / "agent-fabric-workspace-freshness.sh"
PREFLIGHT = REPO_ROOT / "scripts" / "agent-fabric-protocol-preflight"
PROTOCOL_BUILD = REPO_ROOT / "scripts" / "agent-fabric-protocol-build"


def _write(path: Path, content: str = "fixture\n") -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(content, encoding="utf-8")


def _copy_launcher_scripts(root: Path) -> None:
    scripts = root / "scripts"
    scripts.mkdir(parents=True)
    for source in (LAUNCHER, PREFLIGHT, PROTOCOL_BUILD):
        shutil.copy2(source, scripts / source.name)
    library = scripts / "lib/agent-fabric-workspace-freshness.sh"
    library.parent.mkdir()
    shutil.copy2(FRESHNESS_LIBRARY, library)


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
    _write(root / "node_modules/tsx/dist/loader.mjs")
    now = 1_700_000_000
    os.utime(fabric_source, (now, now))
    os.utime(protocol_source, (now, now))
    if launcher_mode == "packaged":
        _write(fabric_dist)
        os.utime(fabric_dist, (now + 30, now + 30))
    if protocol_dist != "missing":
        _write(protocol_output)
        output_time = now + 20 if protocol_dist == "current" else now - 20
        os.utime(protocol_output, (output_time, output_time))

    marker = tmp_path / "daemon-election-attempt"
    fake_node = tmp_path / "bin" / "node"
    _write(fake_node, "#!/bin/sh\nprintf '%s\\n' \"$@\" > \"$LAUNCHER_TEST_MARKER\"\n")
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
    now = 1_700_000_000
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
    (node_modules / "tsx").symlink_to(Path(tsx_package_json).parent, target_is_directory=True)
    (node_modules / "@local/agent-fabric-protocol").symlink_to(
        protocol_root,
        target_is_directory=True,
    )
    return root


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

    if protocol_dist == "current":
        assert result.returncode == 0, result.stderr
        assert marker.read_text(encoding="utf-8").splitlines() == [
            "--import",
            str(root / "node_modules/tsx/dist/loader.mjs"),
            str(root / "runtime/agent-fabric/src/cli/main.ts"),
            "doctor",
        ]
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


@pytest.mark.parametrize("protocol_dist", ["missing", "stale"])
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
    assert f'repair: "{root / "scripts/agent-fabric-protocol-build"}"' in result.stderr
    assert not marker.exists(), "stale protocol dist must fail before daemon election"


def test_reported_protocol_repair_clears_staleness_without_root_build(
    tmp_path: Path,
) -> None:
    root, marker, fake_node = _fixture(
        tmp_path,
        launcher_mode="packaged",
        protocol_dist="stale",
    )
    npm_marker = tmp_path / "npm-invocations"
    fake_npm = fake_node.parent / "npm"
    _write(fake_npm, "#!/bin/sh\nprintf '%s\\n' \"$*\" > \"$NPM_TEST_MARKER\"\n")
    fake_npm.chmod(0o755)
    env = {
        **os.environ,
        "AGENTS_HOME": str(root),
        "NPM_TEST_MARKER": str(npm_marker),
        "PATH": f"{fake_node.parent}:{os.environ['PATH']}",
    }

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
