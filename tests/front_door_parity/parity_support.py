"""Language-neutral parity support for the provenant front door.

The suite drives whichever implementation `PROVENANT_PARITY_IMPL` names:

* `python` (default): `scripts/provenant.template` is the stub role and
  `scripts/provenant` is the dispatcher role.
* `rust`: one compiled binary plays both roles. Its path is
  `PROVENANT_FRONT_DOOR_BIN`, else `tools/front-door/target/release/provenant`.

Stub role means the binary sits in a bin directory outside any checkout, as
installed. Dispatcher role means a copy lives at `<checkout>/scripts/provenant`.

Every fixture checkout carries fake owners at each owner path. A fake owner
prints one JSON object describing how it was started and exits with
`PROVENANT_TEST_EXIT`, so a test can see argv, argv[0], cwd, environment, pid
and a hash of stdin without the front door being involved in reading any of it.
"""

from __future__ import annotations

import hashlib
import json
import os
import shutil
import subprocess
import sys
from dataclasses import dataclass
from pathlib import Path

import pytest

ROOT = Path(__file__).resolve().parents[2]
IMPL = os.environ.get("PROVENANT_PARITY_IMPL", "python")
if IMPL not in ("python", "rust"):
    raise RuntimeError(f"PROVENANT_PARITY_IMPL must be python or rust, got {IMPL!r}")

#: Checkout whose Python dispatcher is the byte-for-byte reference for help
#: text. Point it at a tree that has #1094 and #1095 merged to pin the union.
REFERENCE_ROOT = Path(os.environ.get("PROVENANT_PARITY_REFERENCE_ROOT") or ROOT)

GIT_REDIRECT_VARIABLES = (
    "GIT_DIR",
    "GIT_WORK_TREE",
    "GIT_COMMON_DIR",
    "GIT_INDEX_FILE",
    "GIT_OBJECT_DIRECTORY",
    "GIT_ALTERNATE_OBJECT_DIRECTORIES",
    "GIT_CEILING_DIRECTORIES",
    "GIT_DISCOVERY_ACROSS_FILESYSTEM",
)
#: Variables that steer the front door. Cleared from the inherited environment
#: so a case answers the question it asks and not the ambient machine's.
AMBIENT_VARIABLES = (
    "AGENT_FABRIC_INSTANCE_ROOT",
    "AGENT_FABRIC_PRODUCT_ROOT",
    "AGENTS_HOME",
    "AGENT_FABRIC_SEAT",
    "PROVENANT_STALE_CHECKED",
    "PROVENANT_DEBUG",
    "PROVENANT_FRONT_DOOR",
    "PROVENANT_FRONT_DOOR_BINARY",
    "PROVENANT_TEST_EXIT",
    "PROVENANT_TEST_MARKER",
    *GIT_REDIRECT_VARIABLES,
)
ROLES = ("stub", "dispatcher")
FABRIC_MCP = "runtime/fabric/bin/fabric-mcp"
REPAIR = (
    "repair: re-run install-harness from the product checkout, "
    "or set AGENT_FABRIC_PRODUCT_ROOT"
)

#: Every owner path the front door can reach, relative to a checkout.
OWNER_PATHS = (
    "scripts/model-route",
    "scripts/worktree",
    "scripts/clean.py",
    "scripts/check-harness",
    "scripts/gate",
    "scripts/fabric-hosts",
    "scripts/provenant-telemetry",
    "scripts/provenant-quota",
    "scripts/provenant-profile",
    "skills/orchestrate/scripts/capabilities.py",
    "skills/orchestrate/scripts/dispatch_run.py",
    "skills/orchestrate/scripts/batch_run.py",
    "skills/orchestrate/scripts/run_controls.py",
    "runtime/fabric/bin/fabric",
    FABRIC_MCP,
)

RECORDER = """#!{python}
import hashlib
import json
import os
import sys

payload = {{
    "argv": sys.argv[1:],
    "argv0": sys.argv[0],
    "cwd": os.getcwd(),
    "env": dict(os.environ),
    "pid": os.getpid(),
    "stdin_sha256": hashlib.sha256(sys.stdin.buffer.read()).hexdigest(),
}}
print(json.dumps(payload, sort_keys=True))
print("dummy stderr", file=sys.stderr)
raise SystemExit(int(os.environ.get("PROVENANT_TEST_EXIT", "0")))
"""


def front_door_binary() -> Path:
    configured = os.environ.get("PROVENANT_FRONT_DOOR_BIN")
    path = Path(configured) if configured else ROOT / "tools/front-door/target/release/provenant"
    if not path.is_file():
        pytest.fail(
            f"PROVENANT_PARITY_IMPL=rust but no binary at {path}; build it with "
            "`cargo build --release --locked --manifest-path tools/front-door/Cargo.toml` "
            "or set PROVENANT_FRONT_DOOR_BIN",
            pytrace=False,
        )
    return path


@dataclass
class Result:
    returncode: int
    stdout: bytes
    stderr: bytes

    @property
    def out(self) -> str:
        return self.stdout.decode("utf-8", "surrogateescape")

    @property
    def err(self) -> str:
        return self.stderr.decode("utf-8", "surrogateescape")

    def payload(self) -> dict:
        """The JSON a fake owner printed."""
        return json.loads(self.stdout)


def sha256(data: bytes) -> str:
    return hashlib.sha256(data).hexdigest()


def clean_environment(**updates: str | None) -> dict[str, str]:
    """A copy of the inherited environment minus the ambient steering variables."""
    env = os.environ.copy()
    for name in AMBIENT_VARIABLES:
        env.pop(name, None)
    for name, value in updates.items():
        if value is None:
            env.pop(name, None)
        else:
            env[name] = value
    return env


def git(cwd: Path, *arguments: str, check: bool = True) -> subprocess.CompletedProcess[str]:
    env = clean_environment()
    return subprocess.run(
        ["git", "-c", "user.name=t", "-c", "user.email=t@example.com", *arguments],
        cwd=cwd,
        env=env,
        text=True,
        capture_output=True,
        check=check,
    )


def write_owners(checkout: Path) -> None:
    recorder = RECORDER.format(python=sys.executable)
    # lint (#1094) and lease (#1095) exist in a fixture only when the reference
    # tree has them, so Rust, which gates on these owners, sees the same tree.
    optional = (
        *(("scripts/provenant_lint.py",) if REFERENCE_HAS_LINT else ()),
        *(("runtime/fabric/src/landing-cli.ts",) if REFERENCE_HAS_LEASE else ()),
    )
    for relative in (*OWNER_PATHS, *optional):
        path = checkout / relative
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_text(recorder)
        path.chmod(0o755)


def install_front_door(destination: Path, role: str) -> None:
    """Place the implementation under test at `destination` for `role`."""
    destination.parent.mkdir(parents=True, exist_ok=True)
    if IMPL == "rust":
        shutil.copy2(front_door_binary(), destination)
    elif role == "stub":
        shutil.copy2(ROOT / "scripts/provenant.template", destination)
    else:
        shutil.copy2(ROOT / "scripts/provenant", destination)
    destination.chmod(0o755)


def build_checkout(base: Path, role: str, name: str = "checkout") -> Path:
    """A fake product checkout with recorder owners and the front door in place.

    `scripts/provenant` always exists. For Python it is the dispatcher; for
    Rust it is the binary in the dispatcher role and the Python dispatcher in
    the stub role (the stub never runs it, the `check` helper loads it). A
    Rust tree also carries the Python source as `scripts/provenant.py`.
    """
    checkout = base / name
    scripts = checkout / "scripts"
    scripts.mkdir(parents=True)
    shutil.copy2(ROOT / "scripts/worktree.py", scripts / "worktree.py")
    shutil.copytree(ROOT / "scripts/lib", scripts / "lib")
    shutil.copy2(ROOT / "scripts/check_target.py", scripts / "check_target.py")
    shutil.copy2(ROOT / "scripts/provenant", scripts / "provenant")
    if IMPL == "rust":
        shutil.copy2(ROOT / "scripts/provenant", scripts / "provenant.py")
        if role == "dispatcher":
            install_front_door(scripts / "provenant", role)
    write_owners(checkout)
    return checkout


class FrontDoor:
    """One implementation in one role, over a fresh fixture checkout."""

    def __init__(self, tmp_path: Path, role: str) -> None:
        tmp_path = tmp_path.resolve()  # owners see realpaths (macOS /var -> /private/var)
        self.tmp = tmp_path
        self.role = role
        self.home = tmp_path / "home"
        self.home.mkdir()
        self.checkout = build_checkout(tmp_path, role)
        if role == "stub":
            self.command = tmp_path / "stable-bin" / "provenant"
            install_front_door(self.command, "stub")
        else:
            self.command = self.checkout / "scripts/provenant"
        self.cwd = tmp_path / "caller"
        self.cwd.mkdir()

    @property
    def is_stub(self) -> bool:
        return self.role == "stub"

    def owner(self, relative: str) -> Path:
        return self.checkout / relative

    def env(self, **updates: str | None) -> dict[str, str]:
        updates.setdefault("HOME", str(self.home))
        return clean_environment(**updates)

    def run(
        self,
        *arguments: str | bytes,
        cwd: Path | None = None,
        stdin: bytes = b"",
        env: dict[str, str] | None = None,
        product: bool | Path = True,
        timeout: float = 60,
        argv0: str | None = None,
        **env_updates: str | None,
    ) -> Result:
        """Run the front door.

        `product=True` points `AGENT_FABRIC_PRODUCT_ROOT` at the fixture
        checkout (the stub needs some root; the dispatcher prefers its own
        tree regardless). `product=False` leaves root selection to the case.
        """
        if env is None:
            if product is not False:
                root = self.checkout if product is True else product
                env_updates.setdefault("AGENT_FABRIC_PRODUCT_ROOT", str(root))
            env = self.env(**env_updates)
        argv = [os.fsencode(a) for a in arguments]
        first = argv0.encode() if argv0 is not None else os.fsencode(self.command)
        try:
            done = subprocess.run(
                [first, *argv],
                executable=os.fsencode(self.command),
                cwd=cwd or self.cwd,
                env=env,
                input=stdin,
                capture_output=True,
                timeout=timeout,
                check=False,
            )
        except subprocess.TimeoutExpired as exc:  # pragma: no cover - defensive
            pytest.fail(f"front door timed out after {timeout}s: {exc}")
        return Result(done.returncode, done.stdout, done.stderr)


def reference_run(*arguments: str, env: dict[str, str] | None = None) -> Result:
    """The live Python dispatcher, run directly: the byte reference for help."""
    dispatcher = REFERENCE_ROOT / "scripts/provenant"
    done = subprocess.run(
        [sys.executable, str(dispatcher), *arguments],
        env=env or clean_environment(),
        capture_output=True,
        stdin=subprocess.DEVNULL,
        check=False,
        timeout=60,
    )
    return Result(done.returncode, done.stdout, done.stderr)


def reference_supports(verb: str) -> bool:
    return f"  {verb} " in reference_run("help").out


#: lint (#1094) and lease (#1095) exist only once those PRs are on the
#: reference branch. Rust follows its tree's owners exactly as Python does.
REFERENCE_HAS_LINT = reference_supports("lint")
REFERENCE_HAS_LEASE = reference_supports("lease")
SUPPORTS_LINT = REFERENCE_HAS_LINT
SUPPORTS_LEASE = REFERENCE_HAS_LEASE
HELP_BUDGET = 1100 if SUPPORTS_LINT else 1024

needs_lint = pytest.mark.skipif(not SUPPORTS_LINT, reason="lint (#1094) is not on this branch")
needs_lease = pytest.mark.skipif(not SUPPORTS_LEASE, reason="lease (#1095) is not on this branch")


def assert_matches_reference(actual: Result, expected: Result) -> None:
    """Byte equality with the live Python reference, with no exemptions."""
    assert (actual.returncode, actual.stdout, actual.stderr) == (
        expected.returncode,
        expected.stdout,
        expected.stderr,
    )


# ---------------------------------------------------------------- git fixtures


def init_primary(checkout: Path) -> None:
    git(checkout, "init", "-q", "-b", "main")
    git(checkout, "add", ".")
    git(checkout, "commit", "-qm", "fixture")


def add_linked(primary: Path, destination: Path, branch: str) -> Path:
    git(primary, "worktree", "add", "-q", "-b", branch, str(destination))
    return destination


def add_origin(checkout: Path, base: Path) -> Path:
    """Give `checkout` an `origin` with `main` pushed and `origin/HEAD` set."""
    origin = base / "origin.git"
    subprocess.run(
        ["git", "init", "-q", "--bare", "--initial-branch=main", str(origin)],
        check=True,
        env=clean_environment(),
    )
    git(checkout, "remote", "add", "origin", str(origin))
    git(checkout, "push", "-q", "-u", "origin", "main")
    git(checkout, "remote", "set-head", "origin", "main")
    return origin


def push_from_clone(origin: Path, base: Path, name: str, branch: str = "main") -> None:
    clone = base / f"clone-{name}"
    git(base, "clone", "-q", str(origin), str(clone))
    (clone / name).write_text(name)
    git(clone, "add", name)
    git(clone, "commit", "-qm", name)
    git(clone, "push", "-q", "origin", f"HEAD:{branch}")
