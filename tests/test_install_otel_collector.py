"""The collector installer remains live until telemetry cut-over."""
from pathlib import Path
import os
import shutil
import stat

import pytest
from tests.support.proc import run_cli

ROOT = Path(__file__).resolve().parents[1]


def otel_tools(tmp_path, system="Darwin", swiftc=True):
    """A PATH prefix with a stand-in `uname` and, optionally, a stand-in `swiftc`."""
    tools = tmp_path / "tools"
    tools.mkdir()
    uname = tools / "uname"
    uname.write_text(f"#!/bin/sh\necho {system}\n")
    uname.chmod(0o755)
    if swiftc:
        compiler = tools / "swiftc"
        compiler.write_text('#!/bin/bash\nwhile [[ "$1" != -o ]]; do shift; done\ntouch "$2"\n')
        compiler.chmod(0o755)
    return tools


def otel_install(tmp_path, tools, path=None, **extra):
    return run_cli(
        [shutil.which("bash"), "-c", 'umask 022; exec "$1"', "bash", str(ROOT / "scripts/install-otel-collector")],
        cwd=ROOT,
        env={**os.environ, "HOME": str(tmp_path), "AGENT_FABRIC_STATE_ROOT": str(tmp_path / "state"),
             "PROVENANT_BIN_DIR": str(tmp_path / "bin"), "PROVENANT_OTEL_NO_LAUNCHCTL": "1",
             "PATH": path or f"{tools}{os.pathsep}{os.environ['PATH']}", **extra},
        timeout=5,
    )


def otel_plist(tmp_path):
    return (tmp_path / "Library/LaunchAgents/com.provenant.otlp-collector.plist").read_text()


@pytest.mark.parametrize("existing", ["new", "directory", "symlink"])
def test_otel_installer_restricts_storage_permissions(tmp_path, existing):
    tools = otel_tools(tmp_path)
    state = tmp_path / "state"
    directory = state / "otel"
    if existing == "directory":
        directory.mkdir(parents=True, mode=0o755)
    elif existing == "symlink":
        state.mkdir()
        outside = tmp_path / "outside"
        outside.mkdir(mode=0o755)
        directory.symlink_to(outside, target_is_directory=True)
    result = otel_install(tmp_path, tools)
    if existing == "symlink":
        assert result.returncode != 0
        assert stat.S_IMODE(outside.stat().st_mode) == 0o755
        assert list(outside.iterdir()) == []
        return
    assert result.returncode == 0, result.stderr
    assert stat.S_IMODE(directory.stat().st_mode) == 0o700
    assert "<string>4318</string>" in otel_plist(tmp_path) and "@PORT@" not in otel_plist(tmp_path)


def test_otel_installer_refuses_a_host_without_macos_launchd(tmp_path):
    result = otel_install(tmp_path, otel_tools(tmp_path, system="Linux"))
    assert result.returncode != 0
    assert "needs macOS and launchd; this host is Linux" in result.stderr
    assert not (tmp_path / "bin").exists() and not (tmp_path / "Library").exists()


def test_otel_installer_fails_loudly_without_swiftc(tmp_path):
    tools = otel_tools(tmp_path, swiftc=False)
    for name in ("bash", "dirname"):  # the shebang and ROOT need these; swiftc stays off PATH
        (tools / name).symlink_to(shutil.which(name))
    result = otel_install(tmp_path, tools, path=str(tools))
    assert result.returncode != 0
    assert "swiftc not found" in result.stderr
    assert not (tmp_path / "bin").exists()


def test_otel_installer_substitutes_a_configured_port(tmp_path):
    result = otel_install(tmp_path, otel_tools(tmp_path), PROVENANT_OTEL_PORT="4400")
    assert result.returncode == 0, result.stderr
    assert "<key>SockServiceName</key><string>4400</string>" in otel_plist(tmp_path)
    assert "port=4400" in result.stdout


@pytest.mark.parametrize("port", ["0", "65536", "99999", "080", "43a", "-1", " 4318", ""])
def test_otel_installer_rejects_an_invalid_port(tmp_path, port):
    result = otel_install(tmp_path, otel_tools(tmp_path), PROVENANT_OTEL_PORT=port)
    if port == "":
        # An empty value means unset: the default port.
        assert result.returncode == 0, result.stderr
        assert "<string>4318</string>" in otel_plist(tmp_path)
        return
    assert result.returncode != 0
    assert "PROVENANT_OTEL_PORT must be an integer from 1 to 65535" in result.stderr
    assert not (tmp_path / "Library").exists()

