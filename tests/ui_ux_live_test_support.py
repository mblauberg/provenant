import json
import os
import socket
import subprocess
import time
from pathlib import Path
from urllib.error import HTTPError, URLError
from urllib.parse import quote
from urllib.request import Request, urlopen

from tests.support.proc import run_cli


ROOT = Path(__file__).resolve().parents[1]
SKILL_SCRIPTS = ROOT / "skills" / "ui-ux-design" / "scripts"
SCRIPTS = ROOT / "runtime" / "ui-live"
LIVE = SKILL_SCRIPTS / "live.mjs"
SERVER_WRAPPER = SKILL_SCRIPTS / "live-server.mjs"
INJECT = SCRIPTS / "live-inject.mjs"
SERVER = SCRIPTS / "live-server.mjs"
WRAP = SCRIPTS / "live-wrap.mjs"
ACCEPT = SCRIPTS / "live-accept.mjs"
RESUME = SCRIPTS / "live-resume.mjs"
STATUS = SCRIPTS / "live-status.mjs"
TOKEN = "11111111-1111-4111-8111-111111111111"


def write_live_config(project: Path, files: list[str]) -> None:
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


def run_node(
    script: Path | str,
    *args: str | Path,
    cwd: Path | None = None,
    env: dict[str, str] | None = None,
    timeout: float = 120,
) -> subprocess.CompletedProcess[str]:
    """Run ``node script args...`` and capture text output without raising."""
    return run_cli(
        ["node", script, *args], cwd=cwd or Path.cwd(), env=env, timeout=timeout
    )


def run_node_eval(
    source: str,
    *args: str | Path,
    cwd: Path | None = None,
    env: dict[str, str] | None = None,
    timeout: float = 120,
) -> subprocess.CompletedProcess[str]:
    """Run an ES module snippet with ``node --input-type=module -e``."""
    return run_cli(
        ["node", "--input-type=module", "-e", source, *args],
        cwd=cwd or Path.cwd(),
        env=env,
        timeout=timeout,
    )


def run_inject(project: Path, *args: str) -> subprocess.CompletedProcess[str]:
    return run_node(INJECT, *args, cwd=project)


def available_port() -> int:
    with socket.socket() as sock:
        sock.bind(("127.0.0.1", 0))
        return int(sock.getsockname()[1])


def http_request(
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


class LiveServer:
    def __init__(self, project: Path, *, env: dict[str, str] | None = None):
        self.project = project
        self.port = available_port()
        self.process: subprocess.Popen[str] | None = None
        self.token = ""
        self.agent_token = ""
        self.env = env
        if self.env is None:
            self.env = dict(os.environ)
            self.env["AGENT_FABRIC_PRODUCT_ROOT"] = str(ROOT)
            self.env.pop("AGENTS_HOME", None)

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
            http_request(f"{self.base_url}/stop?token={quote(self.token)}")
        except (HTTPError, URLError, ConnectionError):
            pass
        try:
            self.process.wait(timeout=3)
        except subprocess.TimeoutExpired:
            self.process.terminate()
            self.process.wait(timeout=3)

    def __exit__(self, _exc_type, _exc, _tb):
        self.close()
