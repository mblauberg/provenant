import base64
import http.client
import json
import os
from pathlib import Path
import socket
import subprocess
import time
from urllib.error import HTTPError, URLError
from urllib.parse import quote
from urllib.request import Request, urlopen

import pytest

from ui_ux_live_test_support import RESUME, SERVER, SCRIPTS, STATUS, TOKEN, write_live_config


_write_config = write_live_config


def _available_port() -> int:
    with socket.socket() as sock:
        sock.bind(("127.0.0.1", 0))
        return int(sock.getsockname()[1])


class LiveServer:
    def __init__(self, project: Path, *, env: dict[str, str] | None = None):
        self.project = project
        self.port = _available_port()
        self.process: subprocess.Popen[str] | None = None
        self.token = ""
        self.agent_token = ""
        self.env = env

    def __enter__(self):
        self.process = subprocess.Popen(
            ["node", str(SERVER), f"--port={self.port}"],
            cwd=self.project,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            text=True,
            env=self.env,
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
                self.agent_token = json.loads(
                    Path(info["agentStatePath"]).read_text()
                )["agentToken"]
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


@pytest.mark.parametrize("body", [b"null", b"[]", b'"text"'])
@pytest.mark.parametrize("endpoint", ["events", "poll"])
def test_live_server_rejects_non_object_json_without_crashing(
    tmp_path: Path, endpoint: str, body: bytes,
) -> None:
    with LiveServer(tmp_path) as server:
        status, _, response = _request(
            f"{server.base_url}/{endpoint}",
            method="POST",
            body=body,
            extra_headers={"Content-Type": "application/json"},
        )
        assert status == 400
        assert "error" in json.loads(response)
        assert _request(f"{server.base_url}/health")[0] == 200


@pytest.mark.parametrize("port", ["0", "65536"])
def test_live_server_rejects_unusable_explicit_ports(
    tmp_path: Path, port: str
) -> None:
    process = subprocess.Popen(
        ["node", str(SERVER), f"--port={port}"],
        cwd=tmp_path,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        text=True,
    )
    try:
        deadline = time.monotonic() + 2
        while process.poll() is None and time.monotonic() < deadline:
            time.sleep(0.025)
        assert process.poll() is not None
        _, stderr = process.communicate(timeout=1)
        assert process.returncode != 0
        assert "invalid_port" in stderr
        assert not (tmp_path / ".impeccable" / "live" / "server.json").exists()
    finally:
        if process.poll() is None:
            process.terminate()
            process.wait(timeout=3)


def test_concurrent_background_launchers_only_claim_their_own_child(
    tmp_path: Path,
) -> None:
    launchers = [
        subprocess.Popen(
            ["node", str(SERVER), "--background"],
            cwd=tmp_path,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            text=True,
        )
        for _ in range(2)
    ]
    results: list[tuple[int, str, str]] = []
    try:
        for launcher in launchers:
            stdout, stderr = launcher.communicate(timeout=15)
            results.append((launcher.returncode, stdout, stderr))
        successes = [result for result in results if result[0] == 0]
        failures = [result for result in results if result[0] != 0]
        assert len(successes) == 1, results
        assert len(failures) == 1, results
        public = json.loads(successes[0][1])
        private = json.loads(
            (tmp_path / ".impeccable" / "live" / "server.json").read_text()
        )
        assert private["pid"] == public["pid"]
        status, _, body = _request(
            f"http://127.0.0.1:{private['port']}/status?token={quote(private['token'])}"
        )
        assert status == 200
        assert json.loads(body)["pid"] == public["pid"]
    finally:
        subprocess.run(
            ["node", str(SERVER), "stop", "--keep-inject"],
            cwd=tmp_path,
            check=False,
            capture_output=True,
            text=True,
            timeout=5,
        )
        for launcher in launchers:
            if launcher.poll() is None:
                launcher.terminate()
                launcher.wait(timeout=3)


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


def _read_sse_messages(
    server: "LiveServer", count: int, *, last_event_id: str | None = None,
) -> tuple[list[dict[str, object]], str | None]:
    connection = http.client.HTTPConnection("127.0.0.1", server.port, timeout=1)
    headers = {"Last-Event-ID": last_event_id} if last_event_id is not None else {}
    connection.request(
        "GET", f"/events?token={quote(server.token)}", headers=headers,
    )
    response = connection.getresponse()
    assert response.status == 200
    messages: list[dict[str, object]] = []
    latest_event_id = last_event_id
    try:
        while len(messages) < count:
            line = response.fp.readline().decode("utf-8")
            if line.startswith("id: "):
                latest_event_id = line.removeprefix("id: ").strip()
            elif line.startswith("data: "):
                messages.append(json.loads(line.removeprefix("data: ")))
    finally:
        connection.close()
    return messages, latest_event_id


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
        status, _, body = _request(
            f"{server.base_url}/annotation?token={quote(server.token)}&eventId=first",
            method="POST",
            body=b"first",
            extra_headers={"Content-Type": "image/png"},
        )
        assert status == 200
        first_path = Path(json.loads(body)["path"])
        session_dir = first_path.parent
        assert not session_dir.is_relative_to(tmp_path)
        assert session_dir.stat().st_mode & 0o777 == 0o700
        assert not (tmp_path / ".impeccable" / "live" / "annotations").exists()
        outside = tmp_path.parent / f"{tmp_path.name}-annotation-outside.png"
        outside.write_bytes(b"outside")
        os.link(outside, session_dir / "hard-link.png")

        status, _, body = _request(
            f"{server.base_url}/annotation?token={quote(server.token)}&eventId=hard-link",
            method="POST",
            body=b"replacement",
            extra_headers={"Content-Type": "image/png"},
        )

        assert status in {409, 500}
        assert "error" in json.loads(body)
        assert outside.read_bytes() == b"outside"


def test_live_status_identifies_the_authenticated_server_process(tmp_path: Path) -> None:
    with LiveServer(tmp_path) as server:
        event = {
            "token": server.token,
            "type": "generate",
            "id": "deadbeef",
            "action": "polish",
            "count": 1,
            "element": {"outerHTML": "<main>safe</main>"},
        }
        assert _request(
            f"{server.base_url}/events",
            method="POST",
            body=json.dumps(event).encode(),
            extra_headers={"Content-Type": "application/json"},
        )[0] == 200
        status, _, body = _request(
            f"{server.base_url}/status?token={quote(server.token)}"
        )

        assert status == 200
        payload = json.loads(body)
        assert payload["pid"] == server.process.pid
        assert payload["port"] == server.port

        cli = subprocess.run(
            ["node", str(STATUS)],
            cwd=tmp_path,
            check=False,
            capture_output=True,
            text=True,
        )
        assert cli.returncode == 0, cli.stderr
        cli_payload = json.loads(cli.stdout)
        assert cli_payload["liveServer"]["pid"] == server.process.pid
        assert cli_payload["activeSessions"] == [
            {
                "id": "deadbeef",
                "phase": "generate_requested",
                "revision": 0,
                "hasPendingEvent": True,
                "pendingEventType": "generate",
            }
        ]


def test_live_server_does_not_replay_a_project_preseeded_journal(
    tmp_path: Path,
) -> None:
    sessions = tmp_path / ".impeccable" / "live" / "sessions"
    sessions.mkdir(parents=True)
    (sessions / "deadbeef.jsonl").write_text(
        json.dumps(
            {
                "seq": 1,
                "id": "deadbeef",
                "type": "discard",
                "ts": "2026-08-30T00:00:00.000Z",
                "event": {"id": "deadbeef", "type": "discard"},
            }
        )
        + "\n"
    )

    with LiveServer(tmp_path) as server:
        status = json.loads(
            _request(f"{server.base_url}/status?token={quote(server.token)}")[2]
        )

        assert status["pendingEvents"] == []


def _write_untrusted_pending_journal(project: Path) -> str:
    prompt_text = "PROJECT JOURNAL TEXT MUST NOT BECOME AN ACTION"
    sessions = project / ".impeccable" / "live" / "sessions"
    sessions.mkdir(parents=True, exist_ok=True)
    entries = [
        {
            "seq": 1,
            "id": "deadbeef",
            "type": "generate",
            "ts": "2026-08-30T00:00:00.000Z",
            "event": {
                "id": "deadbeef",
                "type": "generate",
                "action": "polish",
                "count": 1,
                "freeformPrompt": prompt_text,
                "element": {"outerHTML": "<main>untrusted</main>"},
            },
        },
        {
            "seq": 2,
            "id": "deadbeef",
            "type": "checkpoint",
            "ts": "2026-08-30T00:00:01.000Z",
            "event": {
                "id": "deadbeef",
                "type": "checkpoint",
                "revision": 7,
                "phase": prompt_text,
            },
        },
    ]
    (sessions / "deadbeef.jsonl").write_text(
        "\n".join(json.dumps(entry) for entry in entries) + "\n"
    )
    return prompt_text


def test_live_resume_reports_only_inert_advisory_session_metadata(
    tmp_path: Path,
) -> None:
    prompt_text = _write_untrusted_pending_journal(tmp_path)

    result = subprocess.run(
        ["node", str(RESUME), "--id", "deadbeef"],
        cwd=tmp_path,
        check=False,
        capture_output=True,
        text=True,
    )

    assert result.returncode == 0, result.stderr
    assert prompt_text not in result.stdout
    assert "outerHTML" not in result.stdout
    payload = json.loads(result.stdout)
    assert payload["authority"] == "advisory_untrusted"
    assert set(payload) == {"retained", "authority", "session", "instruction"}
    assert payload["session"] == {
        "id": "deadbeef",
        "phase": "unknown",
        "revision": 7,
        "hasPendingEvent": True,
        "pendingEventType": "generate",
    }
    assert "reissue" in payload["instruction"].lower()
    assert "authenticated browser" in payload["instruction"].lower()


def test_live_status_labels_retained_journals_untrusted_without_requeue_advice(
    tmp_path: Path,
) -> None:
    prompt_text = _write_untrusted_pending_journal(tmp_path)

    result = subprocess.run(
        ["node", str(STATUS)],
        cwd=tmp_path,
        check=False,
        capture_output=True,
        text=True,
    )

    assert result.returncode == 0, result.stderr
    assert prompt_text not in result.stdout
    assert "outerHTML" not in result.stdout
    payload = json.loads(result.stdout)
    assert payload["retainedSessionsAuthority"] == "advisory_untrusted"
    assert payload["activeSessions"] == [
        {
            "id": "deadbeef",
            "phase": "unknown",
            "revision": 7,
            "hasPendingEvent": True,
            "pendingEventType": "generate",
        }
    ]
    assert "requeue" not in payload["recoveryHint"].lower()
    assert "reissue" in payload["recoveryHint"].lower()
    assert "authenticated browser" in payload["recoveryHint"].lower()


def test_live_status_sanitises_a_spoofed_loopback_status_response(
    tmp_path: Path,
) -> None:
    live_dir = tmp_path / ".impeccable" / "live"
    live_dir.mkdir(parents=True)
    (live_dir / "server.json").write_text(
        json.dumps({"pid": 42, "port": 8400, "token": TOKEN})
    )
    sentinel = "SPOOFED_STATUS_TEXT_MUST_NOT_ESCAPE"
    status_url = STATUS.as_uri()
    script = (
        "globalThis.fetch=async()=>({ok:true,json:async()=>({"
        "status:{freeformPrompt:'" + sentinel + "'},"
        "pid:'" + sentinel + "',port:8400,"
        "connectedClients:{outerHTML:'" + sentinel + "'},"
        "pendingEvents:["
        "{id:'deadbeef',type:'generate',leased:true,leaseUntil:123,"
        "freeformPrompt:'" + sentinel + "'},"
        "{id:'bad id',type:'" + sentinel + "',nested:{outerHTML:'" + sentinel + "'}}],"
        "activeSessions:[{id:'deadbeef',phase:'generate_requested',"
        "pendingEvent:{freeformPrompt:'" + sentinel + "',"
        "element:{outerHTML:'" + sentinel + "'}}}]})});"
        f"const module=await import({json.dumps(status_url)});"
        "await module.statusCli();"
    )

    result = subprocess.run(
        ["node", "--input-type=module", "-e", script],
        cwd=tmp_path,
        check=False,
        capture_output=True,
        text=True,
    )

    assert result.returncode == 0, result.stderr
    assert sentinel not in result.stdout
    assert "outerHTML" not in result.stdout
    assert "freeformPrompt" not in result.stdout
    payload = json.loads(result.stdout)
    assert payload["activeSessions"] == []
    assert payload["liveServer"] == {
        "status": "unknown",
        "pid": None,
        "port": 8400,
        "connectedClients": None,
        "pendingEvents": [
            {
                "id": "deadbeef",
                "type": "generate",
                "leased": True,
                "leaseUntil": 123,
            }
        ],
    }


def _write_invalid_advisory_session_files(project: Path) -> None:
    sessions = project / ".impeccable" / "live" / "sessions"
    sessions.mkdir(parents=True, exist_ok=True)
    (sessions / "bad!.jsonl").write_text("{}\n")
    (sessions / "deadbeef.jsonl").write_text("{not-json}\n")


def test_live_status_skips_unsafe_and_malformed_advisory_session_files(
    tmp_path: Path,
) -> None:
    _write_invalid_advisory_session_files(tmp_path)

    result = subprocess.run(
        ["node", str(STATUS)],
        cwd=tmp_path,
        check=False,
        capture_output=True,
        text=True,
    )

    assert result.returncode == 0, result.stderr
    assert json.loads(result.stdout)["activeSessions"] == []


def test_server_status_skips_unsafe_and_malformed_advisory_session_files(
    tmp_path: Path,
) -> None:
    _write_invalid_advisory_session_files(tmp_path)

    with LiveServer(tmp_path) as server:
        status, _, body = _request(
            f"{server.base_url}/status?token={quote(server.token)}"
        )

    assert status == 200
    assert json.loads(body)["activeSessions"] == []


@pytest.mark.parametrize("change_unconfigured_source", [False, True])
def test_live_server_does_not_replay_pending_events_after_a_crash(
    tmp_path: Path, change_unconfigured_source: bool,
) -> None:
    page = tmp_path / "index.html"
    page.write_text("<html><body>safe</body></html>\n")
    component = tmp_path / "component.tsx"
    component.write_text("export const Card = () => <article>before</article>;\n")
    _write_config(tmp_path, ["index.html"])
    first = LiveServer(tmp_path)
    first.__enter__()
    try:
        event = {"token": first.token, "type": "discard", "id": "deadbeef"}
        assert _request(
            f"{first.base_url}/events",
            method="POST",
            body=json.dumps(event).encode(),
            extra_headers={"Content-Type": "application/json"},
        )[0] == 200
        assert first.process is not None
        first.process.kill()
        first.process.wait(timeout=3)
        if change_unconfigured_source:
            component.write_text("export const Card = () => <article>changed</article>;\n")

        with LiveServer(tmp_path) as recovered:
            status, _, body = _request(
                f"{recovered.base_url}/poll?token={quote(recovered.agent_token)}"
                "&timeout=1&leaseMs=1000"
            )
            assert status == 200
            assert json.loads(body)["type"] == "timeout"
    finally:
        first.close()


def test_live_event_token_never_reaches_poll_or_durable_journal(tmp_path: Path) -> None:
    event_id = "deadbeef"
    with LiveServer(tmp_path) as server:
        status, _, uploaded = _request(
            f"{server.base_url}/annotation?token={quote(server.token)}&eventId={event_id}",
            method="POST",
            body=b"\x89PNG\r\n\x1a\nprivate",
            extra_headers={"Content-Type": "image/png"},
        )
        assert status == 200, uploaded
        screenshot_path = json.loads(uploaded)["path"]
        event = {
            "token": server.token,
            "type": "generate",
            "id": event_id,
            "action": "polish",
            "count": 1,
            "element": {"outerHTML": "<main>safe</main>"},
            "screenshotPath": screenshot_path,
        }
        status, _, body = _request(
            f"{server.base_url}/events",
            method="POST",
            body=json.dumps(event).encode(),
            extra_headers={"Content-Type": "application/json"},
        )
        assert status == 200, body

        status, _, polled = _request(
            f"{server.base_url}/poll?token={quote(server.agent_token)}&timeout=1000&leaseMs=1000"
        )
        assert status == 200
        polled_payload = json.loads(polled)
        assert polled_payload["id"] == event_id
        assert polled_payload["screenshotPath"] == screenshot_path
        assert server.token not in polled

        journal = tmp_path / ".impeccable" / "live" / "sessions" / f"{event_id}.jsonl"
        assert journal.exists()
        assert server.token not in journal.read_text()
        assert screenshot_path not in journal.read_text()


def test_live_generate_rejects_unbound_browser_supplied_screenshot_path(
    tmp_path: Path,
) -> None:
    outside = tmp_path / "unrelated.png"
    outside.write_bytes(b"\x89PNG\r\n\x1a\nsecret")
    with LiveServer(tmp_path) as server:
        event = {
            "token": server.token,
            "type": "generate",
            "id": "deadbeef",
            "action": "polish",
            "count": 1,
            "element": {"outerHTML": "<main>safe</main>"},
            "screenshotPath": str(outside),
        }
        status, _, body = _request(
            f"{server.base_url}/events",
            method="POST",
            body=json.dumps(event).encode(),
            extra_headers={"Content-Type": "application/json"},
        )

        assert status == 400
        assert "not bound" in json.loads(body)["error"]
        status, _, polled = _request(
            f"{server.base_url}/poll?token={quote(server.agent_token)}&timeout=1&leaseMs=1000"
        )
        assert status == 200
        assert json.loads(polled)["type"] == "timeout"


def test_browser_credential_cannot_complete_work_and_error_does_not_starve_queue(
    tmp_path: Path,
) -> None:
    event_id = "deadbeef"
    with LiveServer(tmp_path) as server:
        event = {
            "token": server.token,
            "type": "accept",
            "id": event_id,
            "variantId": "1",
        }
        assert _request(
            f"{server.base_url}/events",
            method="POST",
            body=json.dumps(event).encode(),
            extra_headers={"Content-Type": "application/json"},
        )[0] == 200

        def reply(
            token: str,
            event_type: str,
            lease_token: str | None = None,
            reply_id: str = event_id,
        ) -> tuple[int, object, str]:
            return _request(
                f"{server.base_url}/poll",
                method="POST",
                body=json.dumps(
                    {
                        "token": token,
                        "leaseToken": lease_token,
                        "id": reply_id,
                        "type": event_type,
                    }
                ).encode(),
                extra_headers={"Content-Type": "application/json"},
            )

        assert reply(server.token, "complete")[0] == 401
        assert reply(server.agent_token, "complete")[0] == 409

        poll_url = (
            f"{server.base_url}/poll?token={quote(server.agent_token)}"
            "&timeout=1000&leaseMs=1000"
        )
        first_lease = json.loads(_request(poll_url)[2])
        assert first_lease["id"] == event_id
        assert reply(server.agent_token, "erorr")[0] == 400
        assert reply(server.agent_token, "done", first_lease["leaseToken"])[0] == 409
        assert reply(server.agent_token, "error", first_lease["leaseToken"])[0] == 200
        next_id = "cafebabe"
        assert _request(
            f"{server.base_url}/events",
            method="POST",
            body=json.dumps(
                {"token": server.token, "type": "discard", "id": next_id}
            ).encode(),
            extra_headers={"Content-Type": "application/json"},
        )[0] == 200
        second_lease = json.loads(_request(poll_url)[2])
        assert second_lease["id"] == next_id
        assert second_lease["leaseToken"] != first_lease["leaseToken"]
        assert reply(server.agent_token, "complete", first_lease["leaseToken"])[0] == 409
        assert reply(
            server.agent_token,
            "discarded",
            second_lease["leaseToken"],
            next_id,
        )[0] == 200

        status = json.loads(
            _request(
                f"{server.base_url}/status?token={quote(server.token)}"
            )[2]
        )
        assert status["pendingEvents"] == []


@pytest.mark.parametrize(
    ("browser_event", "agent_reply"),
    [("accept", "complete"), ("discard", "discarded"), ("accept", "error")],
)
def test_live_server_replays_terminal_outcome_after_browser_reconnect(
    tmp_path: Path, browser_event: str, agent_reply: str,
) -> None:
    event_id = "deadbeef"
    with LiveServer(tmp_path) as server:
        event = {"token": server.token, "type": browser_event, "id": event_id}
        if browser_event == "accept":
            event["variantId"] = "1"
        assert _request(
            f"{server.base_url}/events",
            method="POST",
            body=json.dumps(event).encode(),
            extra_headers={"Content-Type": "application/json"},
        )[0] == 200
        leased = json.loads(
            _request(
                f"{server.base_url}/poll?token={quote(server.agent_token)}"
                "&timeout=1000&leaseMs=1000"
            )[2]
        )
        assert leased["id"] == event_id
        initial, watermark = _read_sse_messages(server, 1)
        assert initial[0]["type"] == "connected"
        assert watermark is not None

        status, _, body = _request(
            f"{server.base_url}/poll",
            method="POST",
            body=json.dumps(
                {
                    "token": server.agent_token,
                    "leaseToken": leased["leaseToken"],
                    "id": event_id,
                    "type": agent_reply,
                }
            ).encode(),
            extra_headers={"Content-Type": "application/json"},
        )
        assert status == 200, body

        reconnected, _ = _read_sse_messages(
            server, 2, last_event_id=watermark,
        )
        connected, replayed = reconnected
        assert connected["type"] == "connected"
        assert replayed == {"type": agent_reply, "id": event_id}


def test_live_server_signals_when_terminal_replay_history_has_expired(
    tmp_path: Path,
) -> None:
    with LiveServer(tmp_path) as server:
        poll_url = (
            f"{server.base_url}/poll?token={quote(server.agent_token)}"
            "&timeout=1000&leaseMs=1000"
        )
        for value in range(129):
            event_id = f"{value:08x}"
            assert _request(
                f"{server.base_url}/events",
                method="POST",
                body=json.dumps(
                    {"token": server.token, "type": "discard", "id": event_id}
                ).encode(),
                extra_headers={"Content-Type": "application/json"},
            )[0] == 200
            lease = json.loads(_request(poll_url)[2])
            assert _request(
                f"{server.base_url}/poll",
                method="POST",
                body=json.dumps(
                    {
                        "token": server.agent_token,
                        "leaseToken": lease["leaseToken"],
                        "id": event_id,
                        "type": "discarded",
                    }
                ).encode(),
                extra_headers={"Content-Type": "application/json"},
            )[0] == 200

        messages, watermark = _read_sse_messages(server, 130, last_event_id="0")

        assert messages[0] == {"type": "connected", "hasProjectContext": False}
        assert messages[1] == {"type": "discarded", "id": "00000001"}
        assert messages[-2] == {"type": "discarded", "id": "00000080"}
        assert messages[-1] == {"type": "replay_gap"}
        assert watermark == "129"


def test_expired_lease_cannot_override_a_competing_worker_lease(
    tmp_path: Path,
) -> None:
    event_id = "deadbeef"
    with LiveServer(tmp_path) as server:
        assert _request(
            f"{server.base_url}/events",
            method="POST",
            body=json.dumps(
                {"token": server.token, "type": "discard", "id": event_id}
            ).encode(),
            extra_headers={"Content-Type": "application/json"},
        )[0] == 200
        poll_url = (
            f"{server.base_url}/poll?token={quote(server.agent_token)}"
            "&timeout=1000&leaseMs=25"
        )
        first_lease = json.loads(_request(poll_url)[2])
        assert first_lease["id"] == event_id
        assert first_lease["leaseToken"]

        time.sleep(0.06)
        second_lease_url = (
            f"{server.base_url}/poll?token={quote(server.agent_token)}"
            "&timeout=1000&leaseMs=10000"
        )
        second_lease = json.loads(_request(second_lease_url)[2])
        assert second_lease["id"] == event_id
        assert second_lease["leaseToken"] != first_lease["leaseToken"]

        def acknowledge(lease_token: str) -> tuple[int, object, str]:
            return _request(
                f"{server.base_url}/poll",
                method="POST",
                body=json.dumps(
                    {
                        "token": server.agent_token,
                        "leaseToken": lease_token,
                        "id": event_id,
                        "type": "discarded",
                    }
                ).encode(),
                extra_headers={"Content-Type": "application/json"},
            )

        stale = acknowledge(first_lease["leaseToken"])
        assert stale[0] == 409
        assert json.loads(stale[2])["error"] == "No matching leased event"
        current = acknowledge(second_lease["leaseToken"])
        assert current[0] == 200, current[2]


def test_live_reply_persists_before_removing_the_leased_event(tmp_path: Path) -> None:
    event_id = "deadbeef"
    with LiveServer(tmp_path) as server:
        event = {
            "token": server.token,
            "type": "accept",
            "id": event_id,
            "variantId": "1",
        }
        assert _request(
            f"{server.base_url}/events",
            method="POST",
            body=json.dumps(event).encode(),
            extra_headers={"Content-Type": "application/json"},
        )[0] == 200
        poll_url = (
            f"{server.base_url}/poll?token={quote(server.agent_token)}"
            "&timeout=1000&leaseMs=5000"
        )
        lease = json.loads(_request(poll_url)[2])
        assert lease["id"] == event_id

        sessions = tmp_path / ".impeccable" / "live" / "sessions"
        moved = sessions.with_name("sessions-moved")
        outside = tmp_path / "outside-state"
        outside.mkdir()
        sessions.rename(moved)
        sessions.symlink_to(outside, target_is_directory=True)
        try:
            rejected = _request(
                f"{server.base_url}/poll",
                method="POST",
                body=json.dumps(
                    {
                        "token": server.agent_token,
                        "leaseToken": lease["leaseToken"],
                        "id": event_id,
                        "type": "complete",
                    }
                ).encode(),
                extra_headers={"Content-Type": "application/json"},
            )
            assert rejected[0] == 500
            assert json.loads(rejected[2])["error"] == "session_store_append_failed"
            assert not list(outside.iterdir())
        finally:
            sessions.unlink()
            moved.rename(sessions)

        status = json.loads(
            _request(f"{server.base_url}/status?token={quote(server.token)}")[2]
        )
        assert status["pendingEvents"][0]["id"] == event_id
        completed = _request(
            f"{server.base_url}/poll",
            method="POST",
            body=json.dumps(
                {
                    "token": server.agent_token,
                    "leaseToken": lease["leaseToken"],
                    "id": event_id,
                    "type": "complete",
                }
            ).encode(),
            extra_headers={"Content-Type": "application/json"},
        )
        assert completed[0] == 200, completed[2]


@pytest.mark.parametrize(
    ("completion_args", "expected_phase"),
    [([], "completed"), (["--error", "cleanup failed"], "agent_error")],
)
def test_carbonized_accept_can_finish_through_the_live_server(
    tmp_path: Path, completion_args: list[str], expected_phase: str,
) -> None:
    event_id = "deadbeef"
    with LiveServer(tmp_path) as server:
        event = {
            "token": server.token,
            "type": "accept",
            "id": event_id,
            "variantId": "1",
        }
        assert _request(
            f"{server.base_url}/events",
            method="POST",
            body=json.dumps(event).encode(),
            extra_headers={"Content-Type": "application/json"},
        )[0] == 200
        poll_url = (
            f"{server.base_url}/poll?token={quote(server.agent_token)}"
            "&timeout=1000&leaseMs=1000"
        )
        lease = json.loads(_request(poll_url)[2])
        assert lease["id"] == event_id
        acknowledged = _request(
            f"{server.base_url}/poll",
            method="POST",
            body=json.dumps(
                {
                    "token": server.agent_token,
                    "leaseToken": lease["leaseToken"],
                    "id": event_id,
                    "type": "agent_done",
                    "data": {"carbonize": True},
                }
            ).encode(),
            extra_headers={"Content-Type": "application/json"},
        )
        assert acknowledged[0] == 200, acknowledged[2]
        status = json.loads(
            _request(f"{server.base_url}/status?token={quote(server.token)}")[2]
        )
        assert status["pendingEvents"] == []
        assert status["activeSessions"][0]["phase"] == "carbonize_required"

        completed = subprocess.run(
            [
                "node",
                str(SCRIPTS / "live-complete.mjs"),
                "--id",
                event_id,
                *completion_args,
            ],
            cwd=tmp_path,
            check=False,
            capture_output=True,
            text=True,
        )
        assert completed.returncode == 0, completed.stderr
        assert json.loads(completed.stdout)["phase"] == expected_phase
        messages, _ = _read_sse_messages(server, 2, last_event_id="0")
        terminal = next(message for message in messages if message["type"] in {"complete", "error"})
        assert terminal["data"]["cleanup"] is True
        status = json.loads(
            _request(f"{server.base_url}/status?token={quote(server.token)}")[2]
        )
        assert status["pendingEvents"] == []
        if expected_phase == "completed":
            assert status["activeSessions"] == []
        else:
            assert status["activeSessions"][0]["phase"] == "agent_error"


def test_retried_browser_event_is_acknowledged_without_requeueing(tmp_path: Path) -> None:
    event = {"type": "discard", "id": "deadbeef"}
    with LiveServer(tmp_path) as server:
        event["token"] = server.token
        assert _request(
            f"{server.base_url}/events", method="POST", body=json.dumps(event).encode(),
            extra_headers={"Content-Type": "application/json"},
        )[0] == 200
        lease = json.loads(_request(
            f"{server.base_url}/poll?token={quote(server.agent_token)}&timeout=1000&leaseMs=1000"
        )[2])
        assert _request(
            f"{server.base_url}/poll", method="POST",
            body=json.dumps({
                "token": server.agent_token, "leaseToken": lease["leaseToken"],
                "id": event["id"], "type": "discarded",
            }).encode(), extra_headers={"Content-Type": "application/json"},
        )[0] == 200

        retried = _request(
            f"{server.base_url}/events", method="POST", body=json.dumps(event).encode(),
            extra_headers={"Content-Type": "application/json"},
        )
        assert retried[0] == 200
        assert json.loads(retried[2])["duplicate"] is True
        status = json.loads(_request(
            f"{server.base_url}/status?token={quote(server.token)}"
        )[2])
        assert status["pendingEvents"] == []


def test_carbonized_accept_finishes_locally_when_stale_server_state_has_no_agent_state(
    tmp_path: Path,
) -> None:
    event_id = "deadbeef"
    module_url = (SCRIPTS / "live-session-store.mjs").as_uri()
    setup = (
        f"import {{ createLiveSessionStore }} from {json.dumps(module_url)};"
        "const store=createLiveSessionStore({cwd:process.argv[1]});"
        f"store.appendEvent({{type:'agent_done',id:{json.dumps(event_id)},carbonize:true}});"
    )
    prepared = subprocess.run(
        ["node", "--input-type=module", "-e", setup, str(tmp_path)],
        check=False,
        capture_output=True,
        text=True,
    )
    assert prepared.returncode == 0, prepared.stderr
    live_dir = tmp_path / ".impeccable" / "live"
    (live_dir / "server.json").write_text(
        json.dumps({"pid": 2_147_483_647, "port": 8400, "token": TOKEN})
    )

    completed = subprocess.run(
        ["node", str(SCRIPTS / "live-complete.mjs"), "--id", event_id],
        cwd=tmp_path,
        check=False,
        capture_output=True,
        text=True,
    )

    assert completed.returncode == 0, completed.stderr
    payload = json.loads(completed.stdout)
    assert payload["ok"] is True
    assert payload["phase"] == "completed"
    journal = live_dir / "sessions" / f"{event_id}.jsonl"
    assert json.loads(journal.read_text().splitlines()[-1])["type"] == "complete"


@pytest.mark.parametrize(
    ("setup_event", "completion_args"),
    [
        (None, []),
        ({"type": "generate", "id": "deadbeef", "count": 1}, []),
        ({"type": "agent_done", "id": "deadbeef", "carbonize": True}, ["--discarded"]),
    ],
)
def test_offline_completion_requires_an_existing_phase_compatible_journal(
    tmp_path: Path,
    setup_event: dict[str, object] | None,
    completion_args: list[str],
) -> None:
    if setup_event is not None:
        module_url = (SCRIPTS / "live-session-store.mjs").as_uri()
        setup = (
            f"import {{ createLiveSessionStore }} from {json.dumps(module_url)};"
            "const store=createLiveSessionStore({cwd:process.argv[1]});"
            f"store.appendEvent({json.dumps(setup_event)});"
        )
        prepared = subprocess.run(
            ["node", "--input-type=module", "-e", setup, str(tmp_path)],
            check=False,
            capture_output=True,
            text=True,
        )
        assert prepared.returncode == 0, prepared.stderr

    completed = subprocess.run(
        [
            "node",
            str(SCRIPTS / "live-complete.mjs"),
            "--id",
            "deadbeef",
            *completion_args,
        ],
        cwd=tmp_path,
        check=False,
        capture_output=True,
        text=True,
    )

    assert completed.returncode != 0
    assert json.loads(completed.stderr)["error"] == "live_completion_rejected"
    journal = tmp_path / ".impeccable" / "live" / "sessions" / "deadbeef.jsonl"
    if journal.exists():
        assert json.loads(journal.read_text().splitlines()[-1])["type"] == setup_event["type"]


def test_offline_completion_rejects_a_malformed_carbonize_journal(tmp_path: Path) -> None:
    event_id = "deadbeef"
    module_url = (SCRIPTS / "live-session-store.mjs").as_uri()
    setup = (
        f"import {{ createLiveSessionStore }} from {json.dumps(module_url)};"
        "const store=createLiveSessionStore({cwd:process.argv[1]});"
        f"store.appendEvent({{type:'agent_done',id:{json.dumps(event_id)},carbonize:true}});"
    )
    prepared = subprocess.run(
        ["node", "--input-type=module", "-e", setup, str(tmp_path)],
        check=False,
        capture_output=True,
        text=True,
    )
    assert prepared.returncode == 0, prepared.stderr
    journal = tmp_path / ".impeccable" / "live" / "sessions" / f"{event_id}.jsonl"
    journal.write_text(journal.read_text() + "{malformed\n")
    before = journal.read_text()

    completed = subprocess.run(
        ["node", str(SCRIPTS / "live-complete.mjs"), "--id", event_id],
        cwd=tmp_path,
        check=False,
        capture_output=True,
        text=True,
    )

    assert completed.returncode != 0
    assert json.loads(completed.stderr)["error"] == "live_completion_rejected"
    assert journal.read_text() == before


def test_offline_discard_cleanup_can_record_an_agent_error(tmp_path: Path) -> None:
    event_id = "deadbeef"
    module_url = (SCRIPTS / "live-session-store.mjs").as_uri()
    setup = (
        f"import {{ createLiveSessionStore }} from {json.dumps(module_url)};"
        "const store=createLiveSessionStore({cwd:process.argv[1]});"
        f"store.appendEvent({{type:'discard',id:{json.dumps(event_id)}}});"
    )
    prepared = subprocess.run(
        ["node", "--input-type=module", "-e", setup, str(tmp_path)],
        check=False,
        capture_output=True,
        text=True,
    )
    assert prepared.returncode == 0, prepared.stderr

    completed = subprocess.run(
        [
            "node",
            str(SCRIPTS / "live-complete.mjs"),
            "--id",
            event_id,
            "--error",
            "discard cleanup failed",
        ],
        cwd=tmp_path,
        check=False,
        capture_output=True,
        text=True,
    )

    assert completed.returncode == 0, completed.stderr
    assert json.loads(completed.stdout)["phase"] == "agent_error"


def test_live_complete_rejects_a_missing_agent_state_at_an_unsafe_path(
    tmp_path: Path,
) -> None:
    live_dir = tmp_path / ".impeccable" / "live"
    live_dir.mkdir(parents=True)
    unsafe_agent_path = tmp_path.parent / "untrusted-live-state" / "agent.json"
    (live_dir / "server.json").write_text(
        json.dumps(
            {
                "pid": 2_147_483_647,
                "port": 8400,
                "token": TOKEN,
                "agentStatePath": str(unsafe_agent_path),
            }
        )
    )

    completed = subprocess.run(
        ["node", str(SCRIPTS / "live-complete.mjs"), "--id", "deadbeef"],
        cwd=tmp_path,
        check=False,
        capture_output=True,
        text=True,
    )

    assert completed.returncode != 0
    assert json.loads(completed.stderr)["error"] == "live_completion_rejected"
    assert not (live_dir / "sessions" / "deadbeef.jsonl").exists()


def test_session_store_normalization_redacts_token_defensively(tmp_path: Path) -> None:
    module_url = (SCRIPTS / "live-session-store.mjs").as_uri()
    script = (
        f"import {{ createLiveSessionStore }} from {json.dumps(module_url)};"
        "const store=createLiveSessionStore({cwd:process.argv[1],sessionId:'deadbeef'});"
        "store.appendEvent({id:'deadbeef',type:'generate',token:'journal-secret',"
        "screenshotPath:'/tmp/transient.png'});"
    )

    result = subprocess.run(
        ["node", "--input-type=module", "-e", script, str(tmp_path)],
        check=False,
        capture_output=True,
        text=True,
    )

    assert result.returncode == 0, result.stderr
    journal = tmp_path / ".impeccable" / "live" / "sessions" / "deadbeef.jsonl"
    assert "journal-secret" not in journal.read_text()
    assert "/tmp/transient.png" not in journal.read_text()


def test_delayed_checkpoint_cannot_regress_a_carbonize_required_session(
    tmp_path: Path,
) -> None:
    module_url = (SCRIPTS / "live-session-store.mjs").as_uri()
    script = (
        f"import {{ createLiveSessionStore }} from {json.dumps(module_url)};"
        "const store=createLiveSessionStore({cwd:process.argv[1]});"
        "store.appendEvent({type:'accept',id:'deadbeef',variantId:'1'});"
        "store.appendEvent({type:'agent_done',id:'deadbeef',carbonize:true});"
        "const result=store.appendEvent({type:'checkpoint',id:'deadbeef',"
        "revision:1,phase:'saving'});"
        "process.stdout.write(JSON.stringify(result));"
    )

    result = subprocess.run(
        ["node", "--input-type=module", "-e", script, str(tmp_path)],
        check=False,
        capture_output=True,
        text=True,
    )

    assert result.returncode == 0, result.stderr
    snapshot = json.loads(result.stdout)
    assert snapshot["phase"] == "carbonize_required"
    assert snapshot["diagnostics"][-1]["error"] == "checkpoint_phase_locked"


def test_session_store_recovery_never_restores_a_transient_screenshot_path(
    tmp_path: Path,
) -> None:
    module_url = (SCRIPTS / "live-session-store.mjs").as_uri()
    script = (
        f"import {{ createLiveSessionStore }} from {json.dumps(module_url)};"
        "const cwd=process.argv[1];"
        "createLiveSessionStore({cwd}).appendEvent({id:'deadbeef',type:'generate',"
        "action:'polish',count:1,screenshotPath:'/tmp/transient.png'});"
        "const recovered=createLiveSessionStore({cwd}).getSnapshot('deadbeef');"
        "process.stdout.write(JSON.stringify(recovered));"
    )

    result = subprocess.run(
        ["node", "--input-type=module", "-e", script, str(tmp_path)],
        check=False,
        capture_output=True,
        text=True,
    )

    assert result.returncode == 0, result.stderr
    recovered = json.loads(result.stdout)
    assert "screenshotPath" not in recovered["pendingEvent"]
    assert recovered["annotationArtifacts"] == []


@pytest.mark.parametrize(
    "query",
    [
        "timeout=0",
        "timeout=-1",
        "timeout=NaN",
        "timeout=1.5",
        "timeout=600001",
        "leaseMs=0",
        "leaseMs=-1",
        "leaseMs=NaN",
        "leaseMs=1.5",
        "leaseMs=600001",
    ],
)
def test_live_poll_rejects_malformed_timeout_and_lease_values(
    tmp_path: Path, query: str
) -> None:
    with LiveServer(tmp_path) as server:
        status, _, body = _request(
            f"{server.base_url}/events",
            method="POST",
            body=json.dumps(
                {"token": server.token, "type": "discard", "id": "deadbeef"}
            ).encode(),
            extra_headers={"Content-Type": "application/json"},
        )
        assert status == 200, body
        status, _, body = _request(
            f"{server.base_url}/poll?token={quote(server.agent_token)}&{query}"
        )

        assert status == 400
        assert json.loads(body)["error"] == "Invalid poll bounds"


def test_default_lease_covers_the_normal_ten_minute_agent_poll_window(
    tmp_path: Path,
) -> None:
    with LiveServer(tmp_path) as server:
        assert _request(
            f"{server.base_url}/events",
            method="POST",
            body=json.dumps(
                {"token": server.token, "type": "discard", "id": "deadbeef"}
            ).encode(),
            extra_headers={"Content-Type": "application/json"},
        )[0] == 200
        before_poll_ms = int(time.time() * 1000)
        assert _request(
            f"{server.base_url}/poll?token={quote(server.agent_token)}&timeout=1000"
        )[0] == 200
        status = json.loads(
            _request(f"{server.base_url}/status?token={quote(server.token)}")[2]
        )

        assert status["pendingEvents"][0]["leaseUntil"] >= before_poll_ms + 590_000


def test_design_sidecar_endpoint_rejects_a_symlink_instead_of_serving_it(
    tmp_path: Path,
) -> None:
    outside = tmp_path.parent / f"{tmp_path.name}-design-secret.json"
    outside.write_text(json.dumps({"schemaVersion": 2, "secret": "do-not-serve"}))
    sidecar = tmp_path / ".impeccable" / "design.json"
    sidecar.parent.mkdir(parents=True)
    sidecar.symlink_to(outside)

    with LiveServer(tmp_path) as server:
        status, _, body = _request(
            f"{server.base_url}/design-system.json?token={quote(server.token)}"
        )

        assert status == 200
        assert "do-not-serve" not in body
        payload = json.loads(body)
        assert payload["hasSidecar"] is False
        assert "sidecarError" in payload


def test_design_markdown_endpoint_rejects_a_project_symlink(tmp_path: Path) -> None:
    project = tmp_path / "project"
    project.mkdir()
    secret = tmp_path / "outside-design.md"
    secret.write_text("# do-not-serve\n")
    (project / "DESIGN.md").symlink_to(secret)

    with LiveServer(project) as server:
        status, _, body = _request(
            f"{server.base_url}/design-system/raw?token={quote(server.token)}"
        )

        assert status == 403
        assert "do-not-serve" not in body


def test_design_endpoints_allow_an_explicit_external_context_root(tmp_path: Path) -> None:
    project = tmp_path / "project"
    context = tmp_path / "authorised-context"
    project.mkdir()
    context.mkdir()
    design_md = "# Design system\n\n## Direction\nQuiet and useful.\n"
    design_json = {"schemaVersion": 2, "narrative": {"direction": "quiet"}}
    (context / "DESIGN.md").write_text(design_md)
    (context / "DESIGN.json").write_text(json.dumps(design_json))
    env = {**os.environ, "IMPECCABLE_CONTEXT_DIR": str(context)}

    with LiveServer(project, env=env) as server:
        raw_status, _, raw = _request(
            f"{server.base_url}/design-system/raw?token={quote(server.token)}"
        )
        json_status, _, body = _request(
            f"{server.base_url}/design-system.json?token={quote(server.token)}"
        )

        assert raw_status == 200
        assert raw == design_md
        assert json_status == 200
        payload = json.loads(body)
        assert payload["hasMd"] is True
        assert payload["hasSidecar"] is True
        assert payload["sidecar"] == design_json


@pytest.mark.parametrize("symlink_part", [".impeccable", "live", "sessions"])
def test_live_server_rejects_a_preexisting_symlinked_state_root(
    tmp_path: Path, symlink_part: str
) -> None:
    project = tmp_path / "project"
    outside = tmp_path / "outside-state"
    project.mkdir()
    outside.mkdir()
    if symlink_part == ".impeccable":
        (project / ".impeccable").symlink_to(outside, target_is_directory=True)
    elif symlink_part == "live":
        impeccable = project / ".impeccable"
        impeccable.mkdir()
        (impeccable / "live").symlink_to(outside, target_is_directory=True)
    else:
        live = project / ".impeccable" / "live"
        live.mkdir(parents=True)
        (live / "sessions").symlink_to(outside, target_is_directory=True)

    result = subprocess.run(
        ["node", str(SERVER), f"--port={_available_port()}"],
        cwd=project,
        check=False,
        capture_output=True,
        text=True,
        timeout=5,
    )

    assert result.returncode != 0
    assert "live_state_root_invalid" in result.stderr
    assert not (outside / "server.json").exists()
    assert not list(outside.glob("*.jsonl"))
    assert not list(outside.glob("*.snapshot.json"))


def test_live_server_stop_does_not_follow_a_symlinked_state_root(tmp_path: Path) -> None:
    project = tmp_path / "project"
    outside = tmp_path / "outside-state"
    project.mkdir()
    (outside / "live").mkdir(parents=True)
    record = outside / "live" / "server.json"
    record.write_text(json.dumps({"pid": 2_147_483_647, "port": 65534, "token": "safe"}))
    (project / ".impeccable").symlink_to(outside, target_is_directory=True)

    result = subprocess.run(
        ["node", str(SERVER), "stop", "--keep-inject"],
        cwd=project,
        check=False,
        capture_output=True,
        text=True,
        timeout=5,
    )

    assert result.returncode != 0
    assert "live_state_root_invalid" in result.stderr
    assert record.exists()


@pytest.mark.parametrize("link_kind", ["symlink", "hardlink"])
@pytest.mark.parametrize("suffix", [".jsonl", ".snapshot.json"])
def test_live_session_store_rejects_linked_state_files(
    tmp_path: Path, suffix: str, link_kind: str
) -> None:
    project = tmp_path / "project"
    sessions = project / ".impeccable" / "live" / "sessions"
    sessions.mkdir(parents=True)
    outside = tmp_path / "outside.txt"
    outside.write_text("outside-safe")
    state_file = sessions / f"security-test{suffix}"
    if link_kind == "symlink":
        state_file.symlink_to(outside)
    else:
        os.link(outside, state_file)
    module_url = (SCRIPTS / "live-session-store.mjs").as_uri()
    script = (
        f"import {{ createLiveSessionStore }} from {json.dumps(module_url)};"
        "const store=createLiveSessionStore({cwd:process.argv[1]});"
        "store.appendEvent({id:'security-test',type:'generate'});"
    )

    result = subprocess.run(
        ["node", "--input-type=module", "-e", script, str(project)],
        check=False,
        capture_output=True,
        text=True,
    )

    assert result.returncode != 0
    assert outside.read_text() == "outside-safe"


def test_live_session_store_rejects_a_post_construction_parent_swap(
    tmp_path: Path,
) -> None:
    project = tmp_path / "project"
    outside = tmp_path / "outside-state"
    project.mkdir()
    outside.mkdir()
    module_url = (SCRIPTS / "live-session-store.mjs").as_uri()
    script = (
        "import fs from 'node:fs';"
        f"import {{ createLiveSessionStore }} from {json.dumps(module_url)};"
        "const store=createLiveSessionStore({cwd:process.argv[1]});"
        "fs.renameSync(store.rootDir,store.rootDir+'-moved');"
        "fs.symlinkSync(process.argv[2],store.rootDir,'dir');"
        "try{store.appendEvent({id:'deadbeef',type:'accept',variantId:'1'})}"
        "catch(error){process.stdout.write(error.code);process.exit(7)}"
    )

    result = subprocess.run(
        [
            "node",
            "--input-type=module",
            "-e",
            script,
            str(project),
            str(outside),
        ],
        check=False,
        capture_output=True,
        text=True,
    )

    assert result.returncode == 7
    assert result.stdout == "live_state_root_invalid"
    assert not list(outside.iterdir())


def test_live_session_store_ignores_a_symlinked_legacy_parent(
    tmp_path: Path,
) -> None:
    project = tmp_path / "project"
    outside = tmp_path / "outside-state"
    project.mkdir()
    outside.mkdir()
    (outside / "deadbeef.jsonl").write_text(
        json.dumps({"seq": 1, "id": "deadbeef", "type": "accept"}) + "\n"
    )
    legacy = project / ".impeccable-live"
    legacy.mkdir()
    (legacy / "sessions").symlink_to(outside, target_is_directory=True)
    module_url = (SCRIPTS / "live-session-store.mjs").as_uri()
    script = (
        f"import {{ createLiveSessionStore }} from {json.dumps(module_url)};"
        "const store=createLiveSessionStore({cwd:process.argv[1]});"
        "process.stdout.write(JSON.stringify(store.listActiveSessions()));"
    )

    result = subprocess.run(
        ["node", "--input-type=module", "-e", script, str(project)],
        check=False,
        capture_output=True,
        text=True,
    )

    assert result.returncode == 0, result.stderr
    assert json.loads(result.stdout) == []
    assert not list((project / ".impeccable" / "live" / "sessions").iterdir())


def test_live_session_store_rejects_a_post_construction_legacy_parent_swap(
    tmp_path: Path,
) -> None:
    project = tmp_path / "project"
    legacy = project / ".impeccable-live" / "sessions"
    outside = tmp_path / "outside-state"
    legacy.mkdir(parents=True)
    outside.mkdir()
    (legacy / "deadbeef.jsonl").write_text("")
    (outside / "deadbeef.jsonl").write_text(
        json.dumps({"seq": 1, "id": "deadbeef", "type": "accept"}) + "\n"
    )
    module_url = (SCRIPTS / "live-session-store.mjs").as_uri()
    script = (
        "import fs from 'node:fs';"
        f"import {{ createLiveSessionStore }} from {json.dumps(module_url)};"
        "const store=createLiveSessionStore({cwd:process.argv[1]});"
        "fs.renameSync(store.legacyRootDir,store.legacyRootDir+'-moved');"
        "fs.symlinkSync(process.argv[2],store.legacyRootDir,'dir');"
        "try{store.getSnapshot('deadbeef')}"
        "catch(error){process.stdout.write(error.code);process.exit(7)}"
    )

    result = subprocess.run(
        ["node", "--input-type=module", "-e", script, str(project), str(outside)],
        check=False,
        capture_output=True,
        text=True,
    )

    assert result.returncode == 7
    assert result.stdout == "live_state_root_invalid"


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
    ("event_type", "expected_error"),
    [
        ("discard", "Pending event capacity reached"),
        ("checkpoint", "Active session capacity reached"),
    ],
)
def test_live_server_bounds_queued_work_and_active_sessions(
    tmp_path: Path, event_type: str, expected_error: str,
) -> None:
    with LiveServer(tmp_path) as server:
        for index in range(64):
            event = {
                "token": server.token,
                "type": event_type,
                "id": f"{index:08x}",
            }
            if event_type == "checkpoint":
                event["revision"] = 0
            assert _request(
                f"{server.base_url}/events",
                method="POST",
                body=json.dumps(event).encode(),
                extra_headers={"Content-Type": "application/json"},
            )[0] == 200

        overflow = {"token": server.token, "type": event_type, "id": "ffffffff"}
        if event_type == "checkpoint":
            overflow["revision"] = 0
        status, _, body = _request(
            f"{server.base_url}/events",
            method="POST",
            body=json.dumps(overflow).encode(),
            extra_headers={"Content-Type": "application/json"},
        )
        assert status == 429
        assert json.loads(body)["error"] == expected_error
        assert _request(f"{server.base_url}/health")[0] == 200


def test_live_server_bounds_sse_clients(tmp_path: Path) -> None:
    connections: list[http.client.HTTPConnection] = []
    responses: list[http.client.HTTPResponse] = []
    with LiveServer(tmp_path) as server:
        try:
            for _ in range(16):
                connection = http.client.HTTPConnection("127.0.0.1", server.port, timeout=3)
                connection.request("GET", f"/events?token={quote(server.token)}")
                response = connection.getresponse()
                assert response.status == 200
                connections.append(connection)
                responses.append(response)

            status, _, body = _request(
                f"{server.base_url}/events?token={quote(server.token)}"
            )
            assert status == 429
            assert json.loads(body)["error"] == "SSE client capacity reached"

            responses[0].close()
            connections[0].close()
            deadline = time.monotonic() + 2
            while time.monotonic() < deadline:
                replacement = http.client.HTTPConnection(
                    "127.0.0.1", server.port, timeout=3
                )
                replacement.request("GET", f"/events?token={quote(server.token)}")
                replacement_response = replacement.getresponse()
                if replacement_response.status == 200:
                    connections.append(replacement)
                    responses.append(replacement_response)
                    break
                replacement_response.read()
                replacement.close()
                time.sleep(0.025)
            else:
                pytest.fail("SSE slot was not reclaimed after disconnect")
        finally:
            for response in responses:
                response.close()
            for connection in connections:
                connection.close()


def test_annotation_capacity_is_reclaimed_after_agent_acknowledgement(
    tmp_path: Path,
) -> None:
    event_id = "00000000"
    with LiveServer(tmp_path) as server:
        interrupted = http.client.HTTPConnection("127.0.0.1", server.port, timeout=3)
        interrupted.putrequest(
            "POST",
            f"/annotation?token={quote(server.token)}&eventId=interrupted",
        )
        interrupted.putheader("Content-Type", "image/png")
        interrupted.putheader("Content-Length", "1024")
        interrupted.endheaders()
        interrupted.send(b"partial")
        interrupted.close()
        time.sleep(0.05)

        screenshot_path = None
        for index in range(64):
            current_id = f"{index:08x}"
            status, _, body = _request(
                f"{server.base_url}/annotation?token={quote(server.token)}&eventId={current_id}",
                method="POST",
                body=b"\x89PNG\r\n\x1a\n",
                extra_headers={"Content-Type": "image/png"},
            )
            assert status == 200
            if current_id == event_id:
                screenshot_path = json.loads(body)["path"]
            event = {
                "token": server.token,
                "type": "generate",
                "id": current_id,
                "action": "polish",
                "count": 1,
                "element": {"outerHTML": "<main>safe</main>"},
                "screenshotPath": json.loads(body)["path"],
            }
            assert _request(
                f"{server.base_url}/events",
                method="POST",
                body=json.dumps(event).encode(),
                extra_headers={"Content-Type": "application/json"},
            )[0] == 200

        assert screenshot_path
        assert _request(
            f"{server.base_url}/annotation?token={quote(server.token)}&eventId=overflow",
            method="POST",
            body=b"\x89PNG\r\n\x1a\n",
            extra_headers={"Content-Type": "image/png"},
        )[0] == 507

        _, _, body = _request(
            f"{server.base_url}/poll?token={quote(server.agent_token)}&timeout=1000&leaseMs=1000"
        )
        leased = json.loads(body)
        reply = {
            "token": server.agent_token,
            "type": "agent_done",
            "id": event_id,
            "leaseToken": leased["leaseToken"],
        }
        assert _request(
            f"{server.base_url}/poll",
            method="POST",
            body=json.dumps(reply).encode(),
            extra_headers={"Content-Type": "application/json"},
        )[0] == 200
        assert not Path(screenshot_path).exists()
        assert _request(
            f"{server.base_url}/annotation?token={quote(server.token)}&eventId=reused-slot",
            method="POST",
            body=b"\x89PNG\r\n\x1a\n",
            extra_headers={"Content-Type": "image/png"},
        )[0] == 200


def test_session_journal_has_a_bounded_capacity(tmp_path: Path) -> None:
    module_url = (SCRIPTS / "live-session-store.mjs").as_uri()
    script = (
        f"import {{ createLiveSessionStore }} from {json.dumps(module_url)};"
        "const store=createLiveSessionStore({cwd:process.argv[1]});"
        "const payload='x'.repeat(240000);"
        "let code=null;"
        "for(let revision=0;revision<32;revision+=1){"
        "try{store.appendEvent({type:'checkpoint',id:'deadbeef',revision,"
        "phase:'generating',paramValues:{payload}});}"
        "catch(error){code=error.code;break;}"
        "}"
        "process.stdout.write(JSON.stringify({code}));"
    )
    result = subprocess.run(
        ["node", "--input-type=module", "-e", script, str(tmp_path)],
        check=False,
        capture_output=True,
        text=True,
    )
    assert result.returncode == 0, result.stderr
    assert json.loads(result.stdout)["code"] == "live_session_limit"
    journal = tmp_path / ".impeccable" / "live" / "sessions" / "deadbeef.jsonl"
    assert 2 * 1024 * 1024 < journal.stat().st_size <= int(2.5 * 1024 * 1024)

    terminal = subprocess.run(
        [
            "node",
            "--input-type=module",
            "-e",
            f"import {{ createLiveSessionStore }} from {json.dumps(module_url)};"
            "const store=createLiveSessionStore({cwd:process.argv[1]});"
            "store.appendEvent({type:'accept',id:'deadbeef',variantId:'1'});"
            "store.appendEvent({type:'agent_done',id:'deadbeef',carbonize:true});"
            "const result=store.appendEvent({type:'complete',id:'deadbeef'});"
            "process.stdout.write(JSON.stringify(result));",
            str(tmp_path),
        ],
        check=False,
        capture_output=True,
        text=True,
    )
    assert terminal.returncode == 0, terminal.stderr
    assert json.loads(terminal.stdout)["phase"] == "completed"


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
