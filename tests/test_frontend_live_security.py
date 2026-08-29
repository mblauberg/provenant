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
WRAP = SCRIPTS / "live-wrap.mjs"
ACCEPT = SCRIPTS / "live-accept.mjs"
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


def _run_wrap(project: Path, *args: str) -> subprocess.CompletedProcess[str]:
    return subprocess.run(
        [
            "node",
            str(WRAP),
            "--id",
            "security-test",
            "--count",
            "1",
            "--query",
            "target",
            *args,
        ],
        cwd=project,
        check=False,
        capture_output=True,
        text=True,
    )


def _run_accept(project: Path, *args: str) -> subprocess.CompletedProcess[str]:
    return subprocess.run(
        ["node", str(ACCEPT), "--id", "security-test", *args],
        cwd=project,
        check=False,
        capture_output=True,
        text=True,
    )


def _write_wrapped_session(project: Path, *, with_variant: bool) -> Path:
    page = project / "index.html"
    page.write_text('<html><body><section class="target">original</section></body></html>\n')
    wrapped = _run_wrap(project, "--file", "index.html")
    assert wrapped.returncode == 0, wrapped.stderr
    if with_variant:
        marker = "<!-- Variants: insert below this line -->"
        page.write_text(
            page.read_text().replace(
                marker,
                marker
                + '\n  <div data-impeccable-variant="1"><section class="target">accepted</section></div>',
            )
        )
    return page


def _run_contained_source_probe(project: Path, body: str) -> subprocess.CompletedProcess[str]:
    module_url = (SCRIPTS / "contained-source.mjs").as_uri()
    script = (
        f"import {{ readContainedSource, replaceContainedSources }} from {json.dumps(module_url)};"
        + body
    )
    return subprocess.run(
        ["node", "--input-type=module", "-e", script, str(project)],
        cwd=project,
        check=False,
        capture_output=True,
        text=True,
    )


def test_contained_source_batch_cleans_staged_files_if_later_staging_fails(tmp_path: Path) -> None:
    first = tmp_path / "first.html"
    second = tmp_path / "second.html"
    first.write_text("first")
    second.write_text("second")
    result = _run_contained_source_probe(
        tmp_path,
        "const root=process.argv[1];"
        "const first=readContainedSource(root,'first.html',{relativeOnly:true});"
        "const second=readContainedSource(root,'second.html',{relativeOnly:true});"
        "const bad={toString(){throw new Error('forced-stage-failure')}};"
        "try{replaceContainedSources([{snapshot:first,content:'changed'},{snapshot:second,content:bad}]);}"
        "catch(error){process.stdout.write(JSON.stringify({code:error.code}));process.exit(7)}",
    )

    assert result.returncode == 7
    assert json.loads(result.stdout)["code"] == "source_replace_failed"
    assert first.read_text() == "first"
    assert second.read_text() == "second"
    assert not list(tmp_path.glob(".*.tmp"))


def test_contained_source_batch_rolls_back_prior_replacements_on_commit_failure(tmp_path: Path) -> None:
    first = tmp_path / "first.html"
    second = tmp_path / "second.html"
    first.write_text("first")
    second.write_text("second")
    result = _run_contained_source_probe(
        tmp_path,
        "const root=process.argv[1];"
        "const first=readContainedSource(root,'first.html',{relativeOnly:true});"
        "const second=readContainedSource(root,'second.html',{relativeOnly:true});"
        "try{replaceContainedSources([{snapshot:first,content:'changed-first'},"
        "{snapshot:second,content:'changed-second'}],"
        "{beforeReplace({index}){if(index===1)throw new Error('forced-commit-failure')}});}"
        "catch(error){process.stdout.write(JSON.stringify({code:error.code}));process.exit(7)}",
    )

    assert result.returncode == 7
    assert json.loads(result.stdout)["code"] == "source_replace_failed"
    assert first.read_text() == "first"
    assert second.read_text() == "second"
    assert not list(tmp_path.glob(".*.tmp"))


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
    payload = json.loads(result.stdout)
    assert payload["file"] == page.name
    assert payload["startLine"] >= 1
    assert payload["endLine"] >= payload["startLine"]
    assert "impeccable-variants-start security-test" in page.read_text()
    assert not marker.exists()


def test_live_mutation_help_discloses_project_relative_existing_file_boundary(
    tmp_path: Path,
) -> None:
    wrap = _run_wrap(tmp_path, "--help")
    inject = _run_inject(tmp_path, "--help")
    assert wrap.returncode == 0
    assert inject.returncode == 0
    for output in (wrap.stdout, inject.stdout):
        lowered = output.lower()
        assert "project-relative" in lowered
        assert "existing" in lowered
        assert "regular file" in lowered


@pytest.mark.parametrize("path_kind", ["absolute", "traversal", "symlink"])
def test_live_wrap_rejects_targets_outside_project_before_any_write(
    tmp_path: Path, path_kind: str
) -> None:
    outside = tmp_path.parent / f"{tmp_path.name}-{path_kind}-outside.html"
    original = '<html><body><section class="target">outside</section></body></html>\n'
    outside.write_text(original)

    if path_kind == "absolute":
        target = str(outside)
    elif path_kind == "traversal":
        target = f"../{outside.name}"
    else:
        link = tmp_path / "escape.html"
        link.symlink_to(outside)
        target = link.name

    result = _run_wrap(tmp_path, "--file", target)

    assert result.returncode != 0
    assert json.loads(result.stderr)["error"] == "source_path_outside_project"
    assert outside.read_text() == original


@pytest.mark.parametrize("explicit", [True, False])
def test_live_wrap_rejects_hard_linked_source_before_any_write(
    tmp_path: Path, explicit: bool
) -> None:
    outside = tmp_path.parent / f"{tmp_path.name}-wrap-hardlink.html"
    original = '<html><body><section class="target">outside</section></body></html>\n'
    outside.write_text(original)
    inside = tmp_path / "index.html"
    os.link(outside, inside)

    result = _run_wrap(tmp_path, *(["--file", "index.html"] if explicit else []))

    assert result.returncode != 0
    assert outside.read_text() == original
    assert inside.read_text() == original


@pytest.mark.parametrize("mode", ["insert", "remove"])
def test_live_inject_rejects_hard_linked_source_for_every_mutation_mode(
    tmp_path: Path, mode: str
) -> None:
    page = tmp_path / "index.html"
    original = "<html><body>safe</body></html>\n"
    page.write_text(original)
    _write_config(tmp_path, ["index.html"])
    if mode == "remove":
        inserted = _run_inject(tmp_path, "--port", "8400", "--token", TOKEN)
        assert inserted.returncode == 0, inserted.stderr
    outside = tmp_path.parent / f"{tmp_path.name}-inject-{mode}-hardlink.html"
    os.link(page, outside)
    outside_before = outside.read_bytes()

    args = ["--remove"] if mode == "remove" else ["--port", "8400", "--token", TOKEN]
    result = _run_inject(tmp_path, *args)

    assert result.returncode != 0
    assert outside.read_bytes() == outside_before
    assert page.read_bytes() == outside_before


@pytest.mark.parametrize("mode", ["accept", "discard"])
def test_live_accept_rejects_hard_linked_source_for_every_mutation_mode(
    tmp_path: Path, mode: str
) -> None:
    page = _write_wrapped_session(tmp_path, with_variant=mode == "accept")
    outside = tmp_path.parent / f"{tmp_path.name}-accept-{mode}-hardlink.html"
    os.link(page, outside)
    outside_before = outside.read_bytes()

    args = ["--variant", "1"] if mode == "accept" else ["--discard"]
    result = _run_accept(tmp_path, *args)

    assert result.returncode != 0
    assert outside.read_bytes() == outside_before
    assert page.read_bytes() == outside_before


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
        assert f"/live.js?token={token}" in page.read_text()
        assert token not in result.stdout
        assert token not in result.stderr
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
    server.close()
    assert server.process is not None
    stdout, stderr = server.process.communicate(timeout=3)
    assert token not in stdout
    assert token not in stderr


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


def test_annotation_upload_refuses_an_existing_hard_link_without_changing_outside_bytes(
    tmp_path: Path,
) -> None:
    with LiveServer(tmp_path) as server:
        session_dirs = list((tmp_path / ".impeccable" / "live" / "annotations").glob("session-*"))
        assert len(session_dirs) == 1
        outside = tmp_path.parent / f"{tmp_path.name}-annotation-outside.png"
        outside.write_bytes(b"outside")
        os.link(outside, session_dirs[0] / "hard-link.png")

        status, _, body = _request(
            f"{server.base_url}/annotation?token={quote(server.token)}&eventId=hard-link",
            method="POST",
            body=b"replacement",
            extra_headers={"Content-Type": "image/png"},
        )

        assert status in {409, 500}
        assert "error" in json.loads(body)
        assert outside.read_bytes() == b"outside"


def test_live_serves_the_checked_in_detector_and_screenshot_asset_contracts(
    tmp_path: Path,
) -> None:
    detector_path = SCRIPTS / "detector" / "detect-antipatterns-browser.js"
    screenshot_path = SCRIPTS / "modern-screenshot.umd.js"
    with LiveServer(tmp_path) as server:
        status, headers, detector = _request(f"{server.base_url}/detect.js")
        assert status == 200
        assert headers.get_content_type() == "application/javascript"
        assert detector == detector_path.read_text()

        status, headers, screenshot = _request(
            f"{server.base_url}/modern-screenshot.js"
        )
        assert status == 200
        assert headers.get_content_type() == "application/javascript"
        assert screenshot == screenshot_path.read_text()

    for asset in (detector_path, screenshot_path):
        checked = subprocess.run(
            ["node", "--check", str(asset)],
            check=False,
            capture_output=True,
            text=True,
        )
        assert checked.returncode == 0, checked.stderr


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
