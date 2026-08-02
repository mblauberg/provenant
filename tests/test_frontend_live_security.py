import json
import http.client
import os
from pathlib import Path
import socket
import subprocess
import time
from urllib.error import HTTPError, URLError
from urllib.parse import quote
from urllib.request import Request, urlopen

import pytest


ROOT = Path(__file__).resolve().parents[1]
SCRIPTS = ROOT / "skills" / "ui-ux-design" / "scripts"
INJECT = SCRIPTS / "live-inject.mjs"
SERVER = SCRIPTS / "live-server.mjs"
TOKEN = "11111111-1111-4111-8111-111111111111"


def _write_config(project: Path, files: list[str]) -> None:
    config_dir = project / ".impeccable" / "live"
    config_dir.mkdir(parents=True, exist_ok=True)
    (config_dir / "config.json").write_text(
        json.dumps(
            {
                "files": files,
                "insertBefore": "</body>",
                "commentSyntax": "html",
            }
        )
    )


def _run_inject(project: Path, *args: str) -> subprocess.CompletedProcess[str]:
    return subprocess.run(
        ["node", str(INJECT), *args],
        cwd=project,
        check=False,
        capture_output=True,
        text=True,
    )


def test_live_wrap_does_not_execute_shell_syntax_from_project_filenames(
    tmp_path: Path,
) -> None:
    marker = tmp_path / "SHELL_INJECTION_MARKER"
    page = tmp_path / "$(touch SHELL_INJECTION_MARKER).html"
    page.write_text('<html><body><section class="target">safe</section></body></html>\n')

    result = subprocess.run(
        [
            "node",
            str(SCRIPTS / "live-wrap.mjs"),
            "--id",
            "security-test",
            "--count",
            "1",
            "--query",
            "target",
        ],
        cwd=tmp_path,
        check=False,
        capture_output=True,
        text=True,
    )

    assert result.returncode == 0, result.stderr
    assert not marker.exists()


def test_live_entrypoint_does_not_execute_shell_syntax_from_server_state(
    tmp_path: Path,
) -> None:
    marker = tmp_path / "SHELL_INJECTION_MARKER"
    page = tmp_path / "index.html"
    page.write_text("<html><body>safe</body></html>\n")
    _write_config(tmp_path, ["index.html"])
    server_path = tmp_path / ".impeccable" / "live" / "server.json"
    server_path.write_text(
        json.dumps(
            {
                "pid": os.getpid(),
                "port": 8400,
                "token": "$(touch SHELL_INJECTION_MARKER)",
            }
        )
    )

    result = subprocess.run(
        ["node", str(SCRIPTS / "live.mjs")],
        cwd=tmp_path,
        check=False,
        capture_output=True,
        text=True,
    )

    assert result.returncode != 0
    assert json.loads(result.stdout)["error"] == "inject_failed"
    assert not marker.exists()


def test_live_server_record_is_atomically_replaced_with_private_permissions(
    tmp_path: Path,
) -> None:
    live_dir = tmp_path / ".impeccable" / "live"
    live_dir.mkdir(parents=True)
    server_path = live_dir / "server.json"
    server_path.write_text(json.dumps({"pid": 1, "port": 1, "token": "old"}))
    server_path.chmod(0o644)
    original_inode = server_path.stat().st_ino
    module_url = (SCRIPTS / "impeccable-paths.mjs").as_uri()
    script = (
        f"import {{ writeLiveServerInfo }} from {json.dumps(module_url)};"
        "writeLiveServerInfo(process.argv[1], {pid: 2, port: 8400, token: 'new'});"
    )

    result = subprocess.run(
        ["node", "--input-type=module", "-e", script, str(tmp_path)],
        check=False,
        capture_output=True,
        text=True,
    )

    assert result.returncode == 0, result.stderr
    assert json.loads(server_path.read_text()) == {"pid": 2, "port": 8400, "token": "new"}
    assert not list(live_dir.glob(".server.json.*.tmp"))
    if os.name != "nt":
        assert server_path.stat().st_mode & 0o777 == 0o600
        assert server_path.stat().st_ino != original_inode


@pytest.mark.parametrize("escaping_path", ["../outside.html", "/tmp/outside.html"])
def test_live_inject_rejects_ancestor_and_absolute_targets_before_any_write(
    tmp_path: Path, escaping_path: str
) -> None:
    safe = tmp_path / "safe.html"
    original = "<html><body>safe</body></html>\n"
    safe.write_text(original)
    _write_config(tmp_path, ["safe.html", escaping_path])

    result = _run_inject(tmp_path, "--port", "8400", "--token", TOKEN)

    assert result.returncode != 0
    assert safe.read_text() == original
    assert "impeccable-live-start" not in safe.read_text()


def test_live_inject_rejects_symlink_escape_before_any_write(tmp_path: Path) -> None:
    safe = tmp_path / "safe.html"
    original = "<html><body>safe</body></html>\n"
    safe.write_text(original)
    outside = tmp_path.parent / f"{tmp_path.name}-outside.html"
    outside.write_text("<html><body>outside</body></html>\n")
    (tmp_path / "escape.html").symlink_to(outside)
    _write_config(tmp_path, ["safe.html", "escape.html"])

    result = _run_inject(tmp_path, "--port", "8400", "--token", TOKEN)

    assert result.returncode != 0
    assert safe.read_text() == original
    assert outside.read_text() == "<html><body>outside</body></html>\n"


def test_live_inject_preserves_authorised_insert_and_remove_workflow(tmp_path: Path) -> None:
    page = tmp_path / "index.html"
    original = "<html><body>safe</body></html>\n"
    page.write_text(original)
    _write_config(tmp_path, ["index.html"])

    inserted = _run_inject(tmp_path, "--port", "8400", "--token", TOKEN)
    assert inserted.returncode == 0, inserted.stderr
    assert f"/live.js?token={TOKEN}" in page.read_text()

    removed = _run_inject(tmp_path, "--remove")
    assert removed.returncode == 0, removed.stderr
    assert page.read_text() == original


def test_live_entrypoint_passes_server_token_into_injected_script(tmp_path: Path) -> None:
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
        assert f"/live.js?token={payload['serverToken']}" in page.read_text()
    finally:
        subprocess.run(
            ["node", str(SERVER), "stop"],
            cwd=tmp_path,
            check=False,
            capture_output=True,
            text=True,
            timeout=10,
        )


def _available_port() -> int:
    with socket.socket() as sock:
        sock.bind(("127.0.0.1", 0))
        return int(sock.getsockname()[1])


class LiveServer:
    def __init__(self, project: Path):
        self.project = project
        self.port = _available_port()
        self.process: subprocess.Popen[str] | None = None
        self.token = ""

    def __enter__(self):
        self.process = subprocess.Popen(
            ["node", str(SERVER), f"--port={self.port}"],
            cwd=self.project,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            text=True,
        )
        info_path = self.project / ".impeccable" / "live" / "server.json"
        deadline = time.monotonic() + 10
        while time.monotonic() < deadline:
            if self.process.poll() is not None:
                stdout, stderr = self.process.communicate()
                raise AssertionError(f"live server exited early\nstdout={stdout}\nstderr={stderr}")
            try:
                info = json.loads(info_path.read_text())
                self.token = info["token"]
                if info["port"] == self.port:
                    return self
            except (FileNotFoundError, json.JSONDecodeError, KeyError):
                pass
            time.sleep(0.05)
        self.close()
        raise AssertionError("timed out waiting for live server")

    @property
    def base_url(self) -> str:
        return f"http://127.0.0.1:{self.port}"

    def close(self) -> None:
        if not self.process or self.process.poll() is not None:
            return
        try:
            _request(f"{self.base_url}/stop?token={quote(self.token)}")
        except (HTTPError, URLError, ConnectionError):
            pass
        try:
            self.process.wait(timeout=3)
        except subprocess.TimeoutExpired:
            self.process.terminate()
            self.process.wait(timeout=3)

    def __exit__(self, _exc_type, _exc, _tb):
        self.close()


def _request(
    url: str,
    *,
    origin: str | None = None,
    method: str = "GET",
    body: bytes | None = None,
    extra_headers: dict[str, str] | None = None,
):
    headers = {"Origin": origin} if origin else {}
    headers.update(extra_headers or {})
    request = Request(url, headers=headers, method=method, data=body)
    try:
        with urlopen(request, timeout=3) as response:
            return response.status, response.headers, response.read().decode("utf-8")
    except HTTPError as error:
        return error.code, error.headers, error.read().decode("utf-8")


def test_live_script_requires_token_and_never_echoes_it_to_cross_origin_callers(
    tmp_path: Path,
) -> None:
    with LiveServer(tmp_path) as server:
        status, headers, body = _request(
            f"{server.base_url}/live.js", origin="https://attacker.example"
        )
        assert status == 401
        assert server.token not in body
        assert headers.get("Access-Control-Allow-Origin") is None

        status, headers, body = _request(
            f"{server.base_url}/live.js?token={quote(server.token)}",
            origin="https://attacker.example",
        )
        assert status == 200
        assert server.token not in body
        assert headers.get("Access-Control-Allow-Origin") is None
        assert "document.currentScript.src" in body
        assert "searchParams.get('token')" in body

        status, headers, _ = _request(
            f"{server.base_url}/events",
            origin="http://localhost:5173",
            method="OPTIONS",
        )
        assert status == 204
        assert headers.get("Access-Control-Allow-Origin") == "http://localhost:5173"
        assert headers.get("Access-Control-Allow-Origin") != "*"


def test_source_endpoint_rejects_ancestor_sibling_prefix_and_symlink_escape(
    tmp_path: Path,
) -> None:
    inside = tmp_path / "inside..page.html"
    inside.write_text("inside")
    secret = tmp_path / ".env"
    secret.write_text("API_KEY=do-not-serve")
    disguised_secret = tmp_path / "disguised.html"
    disguised_secret.symlink_to(secret)
    unconfigured = tmp_path / "other.html"
    unconfigured.write_text("not configured")
    outside_dir = tmp_path.parent / f"{tmp_path.name}-sibling"
    outside_dir.mkdir(exist_ok=True)
    outside = outside_dir / "secret.html"
    outside.write_text("secret")
    (tmp_path / "escape.html").symlink_to(outside)
    _write_config(tmp_path, [inside.name, "disguised.html"])

    with LiveServer(tmp_path) as server:
        def source(path_value: str):
            return _request(
                f"{server.base_url}/source?token={quote(server.token)}&path={quote(path_value, safe='')}"
            )

        assert source(inside.name)[0:3:2] == (200, "inside")
        assert source(".env")[0] == 403
        assert source("other.html")[0] == 403
        assert source("disguised.html")[0] == 403
        assert source(f"../{outside_dir.name}/secret.html")[0] == 403
        assert source(str(outside))[0] == 400
        assert source("escape.html")[0] == 403


def test_authenticated_null_origin_is_supported_without_unauthenticated_cors(
    tmp_path: Path,
) -> None:
    page = tmp_path / "index.html"
    page.write_text("<html><body>page</body></html>")
    _write_config(tmp_path, ["index.html"])

    with LiveServer(tmp_path) as server:
        status, headers, body = _request(
            f"{server.base_url}/source?token={quote(server.token)}&path=index.html",
            origin="null",
        )
        assert status == 200
        assert body == page.read_text()
        assert headers.get("Access-Control-Allow-Origin") == "null"
        assert headers.get("Access-Control-Allow-Origin") != "*"

        status, headers, body = _request(
            f"{server.base_url}/source?token=wrong&path=index.html",
            origin="null",
        )
        assert status == 401
        assert body == "Unauthorized"
        assert headers.get("Access-Control-Allow-Origin") is None


def test_json_post_endpoints_reject_declared_and_streamed_oversize_bodies(
    tmp_path: Path,
) -> None:
    with LiveServer(tmp_path) as server:
        oversized = b'{"padding":"' + (b'x' * (300 * 1024)) + b'"}'
        status, _, body = _request(
            f"{server.base_url}/events",
            method="POST",
            body=oversized,
            extra_headers={"Content-Type": "application/json"},
        )
        assert status == 413
        assert json.loads(body)["error"] == "Payload too large"

        connection = http.client.HTTPConnection("127.0.0.1", server.port, timeout=3)
        try:
            chunks = iter([b'{"padding":"', b'y' * (300 * 1024), b'"}'])
            connection.request(
                "POST",
                "/poll",
                body=chunks,
                headers={"Content-Type": "application/json"},
                encode_chunked=True,
            )
            response = connection.getresponse()
            response_body = response.read().decode("utf-8")
            assert response.status == 413
            assert json.loads(response_body)["error"] == "Payload too large"
        finally:
            connection.close()

        # Draining a rejected body must leave the server usable.
        status, _, body = _request(f"{server.base_url}/health")
        assert status == 200
        assert json.loads(body)["status"] == "ok"


@pytest.mark.parametrize(
    ("context_name", "expected"),
    [
        ("PRODUCT.md", True),
        ("pRoDuCt.Md", True),
        (".impeccable.md", True),
        (None, False),
    ],
)
def test_live_context_signal_matches_loader_variants(
    tmp_path: Path, context_name: str | None, expected: bool
) -> None:
    if context_name:
        (tmp_path / context_name).write_text("# Product context\n")

    with LiveServer(tmp_path) as server:
        status, _, body = _request(f"{server.base_url}/health")
        assert status == 200
        assert json.loads(body)["hasProjectContext"] is expected
