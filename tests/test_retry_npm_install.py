import importlib.util
from io import StringIO
from pathlib import Path


ROOT = Path(__file__).resolve().parents[1]
SCRIPT = ROOT / "scripts" / "retry-npm-install.py"


def load_module():
    spec = importlib.util.spec_from_file_location("retry_npm_install", SCRIPT)
    module = importlib.util.module_from_spec(spec)
    assert spec.loader
    spec.loader.exec_module(module)
    return module


class FakeProcess:
    def __init__(self, returncode, stdout="", stderr=""):
        self.returncode = returncode
        self.stdout = StringIO(stdout)
        self.stderr = StringIO(stderr)

    def wait(self):
        return self.returncode


def test_retries_only_transient_npm_registry_failures(monkeypatch):
    module = load_module()
    results = iter([
        FakeProcess(1, stderr="npm ERR! code ECONNRESET\n"),
        FakeProcess(0, stdout="installed\n"),
    ])
    calls = []
    monkeypatch.setattr(
        module.subprocess, "Popen",
        lambda command, **kwargs: (calls.append(command), next(results))[1],
    )
    delays = []
    monkeypatch.setattr(module.time, "sleep", delays.append)

    assert module.main(["--", "npm", "ci"]) == 0
    assert calls == [["npm", "ci"], ["npm", "ci"]]
    assert delays == [2]


def test_does_not_retry_other_install_failures(monkeypatch):
    module = load_module()
    calls = []
    monkeypatch.setattr(
        module.subprocess, "Popen",
        lambda command, **kwargs: (calls.append(command), FakeProcess(1, stderr="npm ERR! EACCES\n"))[1],
    )
    monkeypatch.setattr(module.time, "sleep", lambda delay: (_ for _ in ()).throw(AssertionError("unexpected retry")))

    assert module.main(["npm", "ci"]) == 1
    assert calls == [["npm", "ci"]]


def test_does_not_retry_unrelated_503_output(monkeypatch):
    module = load_module()
    calls = []
    monkeypatch.setattr(
        module.subprocess, "Popen",
        lambda command, **kwargs: (calls.append(command), FakeProcess(1, stdout="package version 1.503.0\n"))[1],
    )
    monkeypatch.setattr(module.time, "sleep", lambda delay: (_ for _ in ()).throw(AssertionError("unexpected retry")))

    assert module.main(["npm", "ci"]) == 1
    assert calls == [["npm", "ci"]]


def test_recognizes_specific_service_unavailable_signatures():
    module = load_module()

    assert module.RETRYABLE.search("npm error 503 Service Unavailable")
    assert module.RETRYABLE.search("npm error code E503")
    assert module.RETRYABLE.search("npm error status 503")
    assert module.RETRYABLE.search("npm http fetch GET 503 https://registry.npmjs.org/x")
    assert not module.RETRYABLE.search("npm error package version 1.503.0")
