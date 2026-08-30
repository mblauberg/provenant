import json
from pathlib import Path
import subprocess
import time

from test_frontend_live_server_security import LiveServer, _request
from test_frontend_source_security import _run_inject
from ui_ux_live_test_support import SERVER, SCRIPTS, TOKEN, write_live_config


_write_config = write_live_config


def test_live_inject_preflights_every_anchor_before_changing_any_file(tmp_path: Path) -> None:
    good = tmp_path / "good.html"
    bad = tmp_path / "bad.html"
    good_original = "<html><body>good</body></html>\n"
    bad_original = "<html><main>no body anchor</main></html>\n"
    good.write_text(good_original)
    bad.write_text(bad_original)
    _write_config(tmp_path, ["good.html", "bad.html"])

    result = _run_inject(tmp_path, "--port", "8400", "--token", TOKEN)

    assert result.returncode != 0
    assert json.loads(result.stderr)["error"] == "mutation_preflight_failed"
    assert good.read_text() == good_original
    assert bad.read_text() == bad_original


def test_live_entrypoint_injects_private_server_token_without_logging_it(tmp_path: Path) -> None:
    page = tmp_path / "index.html"
    page.write_text("<html><body>safe</body></html>\n")
    _write_config(tmp_path, ["index.html"])
    try:
        result = subprocess.run(
            ["node", str(SCRIPTS / "live.mjs")],
            cwd=tmp_path,
            check=False,
            capture_output=True,
            text=True,
            timeout=15,
        )
        assert result.returncode == 0, result.stderr
        payload = json.loads(result.stdout)
        assert payload["ok"] is True
        assert "serverToken" not in payload
        private = json.loads((tmp_path / ".impeccable" / "live" / "server.json").read_text())
        token = private["token"]
        project_state = (tmp_path / ".impeccable" / "live" / "server.json").read_text()
        agent_state = Path(private["agentStatePath"])
        agent_token = json.loads(agent_state.read_text())["agentToken"]
        assert "agentToken" not in project_state
        assert not agent_state.is_relative_to(tmp_path)
        assert f"/live.js?token={token}" in page.read_text()
        assert agent_token not in page.read_text()
        assert token not in result.stdout
        assert token not in result.stderr
        assert agent_token not in result.stdout
        assert agent_token not in result.stderr
    finally:
        subprocess.run(
            ["node", str(SERVER), "stop"],
            cwd=tmp_path,
            check=False,
            capture_output=True,
            text=True,
            timeout=10,
        )


def test_live_entrypoint_emits_bounded_context_metadata_without_document_bodies(
    tmp_path: Path,
) -> None:
    page = tmp_path / "index.html"
    page.write_text("<html><body>safe</body></html>\n")
    _write_config(tmp_path, ["index.html"])
    product_sentinel = "PRIVATE_PRODUCT_BODY_" + "p" * 100_000
    design_sentinel = "PRIVATE_DESIGN_BODY_" + "d" * 100_000
    (tmp_path / "PRODUCT.md").write_text(f"# Product\n\n{product_sentinel}\n## Users\n")
    (tmp_path / "DESIGN.md").write_text(f"# Design\n\n{design_sentinel}\n## Components\n")
    try:
        result = subprocess.run(
            ["node", str(SCRIPTS / "live.mjs")],
            cwd=tmp_path,
            check=False,
            capture_output=True,
            text=True,
            timeout=15,
        )

        assert result.returncode == 0, result.stderr
        assert product_sentinel not in result.stdout
        assert design_sentinel not in result.stdout
        assert len(result.stdout) < 12_000
        payload = json.loads(result.stdout)
        assert "product" not in payload
        assert "design" not in payload
        assert payload["productPath"] == "PRODUCT.md"
        assert payload["designPath"] == "DESIGN.md"
        assert payload["productChars"] > 100_000
        assert payload["designChars"] > 100_000
        assert payload["productHeadings"] == [
            {"level": 1, "title": "Product"},
            {"level": 2, "title": "Users"},
        ]
        assert payload["designHeadings"] == [
            {"level": 1, "title": "Design"},
            {"level": 2, "title": "Components"},
        ]
        assert payload["metadataTruncation"]["productHeadings"]["total"] == 2
        assert payload["metadataTruncation"]["designHeadings"]["total"] == 2
    finally:
        subprocess.run(
            ["node", str(SERVER), "stop"],
            cwd=tmp_path,
            check=False,
            capture_output=True,
            text=True,
            timeout=10,
        )


def test_live_entrypoint_stops_only_the_server_it_started_when_injection_fails(
    tmp_path: Path,
) -> None:
    good = tmp_path / "good.html"
    bad = tmp_path / "bad.html"
    good.write_text("<html><body>good</body></html>\n")
    bad.write_text("<html><main>bad anchor</main></html>\n")
    _write_config(tmp_path, ["good.html", "bad.html"])

    result = subprocess.run(
        ["node", str(SCRIPTS / "live.mjs")],
        cwd=tmp_path,
        check=False,
        capture_output=True,
        text=True,
        timeout=15,
    )

    assert result.returncode != 0
    payload = json.loads(result.stdout)
    assert payload["error"] == "inject_failed"
    assert payload["serverCleanup"] == "stopped"
    state_path = tmp_path / ".impeccable" / "live" / "server.json"
    deadline = time.monotonic() + 5
    while state_path.exists() and time.monotonic() < deadline:
        time.sleep(0.05)
    assert not state_path.exists()
    assert "impeccable-live-start" not in good.read_text()


def test_live_entrypoint_does_not_stop_a_preexisting_server_when_injection_fails(
    tmp_path: Path,
) -> None:
    page = tmp_path / "index.html"
    page.write_text("<html><main>no body anchor</main></html>\n")
    _write_config(tmp_path, ["index.html"])

    with LiveServer(tmp_path) as server:
        result = subprocess.run(
            ["node", str(SCRIPTS / "live.mjs")],
            cwd=tmp_path,
            check=False,
            capture_output=True,
            text=True,
            timeout=15,
        )
        assert result.returncode != 0
        payload = json.loads(result.stdout)
        assert payload["serverCleanup"] == "not-owned"
        assert _request(f"{server.base_url}/health")[0] == 200


def test_live_server_foreground_logs_never_expose_its_bearer_token(tmp_path: Path) -> None:
    server = LiveServer(tmp_path)
    server.__enter__()
    token = server.token
    agent_token = server.agent_token
    server.close()
    assert server.process is not None
    stdout, stderr = server.process.communicate(timeout=3)
    assert token not in stdout
    assert token not in stderr
    assert agent_token not in stdout
    assert agent_token not in stderr
