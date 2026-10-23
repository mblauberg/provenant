#!/usr/bin/env python3
"""Dispatch one prompt to a configured provider CLI with conservative safety defaults.

This script is a helper, not an authority. The caller still chooses an appropriate
different-family verifier, checks data policy, and records failures in the run manifest.
Assurance remains the default. Ordinary execution can explicitly allow a same-family route.
When Fabric is used, the caller records correlation; this helper owns only the
provider call and its direct receipt.
"""

from __future__ import annotations

from dataclasses import dataclass, field
import functools
import json
import os
import re
import secrets
import shutil
import signal
import subprocess
import sys
import traceback

SCRIPT_DIR = os.path.dirname(os.path.abspath(__file__))
sys.path.insert(0, SCRIPT_DIR)

import output_custody  # noqa: E402

# The provider must inherit the same repository discovered by the validator,
# never a repository redirected through the caller's Git environment.
GIT_CONTEXT_VARIABLES = (
    "GIT_ALTERNATE_OBJECT_DIRECTORIES", "GIT_CEILING_DIRECTORIES", "GIT_COMMON_DIR",
    "GIT_CONFIG", "GIT_CONFIG_COUNT", "GIT_CONFIG_PARAMETERS", "GIT_DIR",
    "GIT_DISCOVERY_ACROSS_FILESYSTEM", "GIT_GRAFT_FILE", "GIT_IMPLICIT_WORK_TREE",
    "GIT_INDEX_FILE", "GIT_NAMESPACE", "GIT_NO_REPLACE_OBJECTS", "GIT_OBJECT_DIRECTORY",
    "GIT_PREFIX", "GIT_QUARANTINE_PATH", "GIT_REPLACE_REF_BASE", "GIT_SHALLOW_FILE",
    "GIT_WORK_TREE",
)

USAGE = """\
Usage: cf_dispatch.py --tool TOOL --orchestrator-family FAMILY --prompt TEXT [options]
       cf_dispatch.py --chain "tool:model:effort ..." --orchestrator-family FAMILY --prompt TEXT [options]
       cf_dispatch.py --doctor

Options:
  --tool TOOL                  One of claude, codex, cursor, agy, kiro, copilot, opencode.
  --task-class CLASS           Route task class through model_route.py.
  --chain SPECS                Space-separated fallback chain.
  --orchestrator-family FAMILY Labels the chair family; assurance requires separation.
  --intent INTENT              assurance (default) or ordinary execution.
  --alias ALIAS                Durable route alias: principal, senior, junior.
                               Defaults from --role: principal for lead,
                               orchestrator and critical-review, senior otherwise.
  --role ROLE                  Route role (default: reviewer).
  --risk-tier TIER             Lifecycle/receipt risk metadata; never selects a model.
  --model-override-tier TIER   Explicit special-model override tier.
  --reviewer-id ID             Stable worker/reviewer identity for receipt binding.
  --model MODEL                Optional model passed to adapter.
  --effort EFFORT              Optional effort passed to adapter.
  --account LABEL              Claude account from telemetry configuration.
  --timeout-seconds N          Supervisor wall clock deadline, all adapters.
  --add-dir PATH               Additional provider directory; repeatable.
  --read-root PATH             Outside directory a read-only cwd may use; repeatable.
  --plan-only                  Print resolved provider argv and controls as JSON.
  --sandbox MODE               read-only, workspace-write, or full.
  --network BOOL               true or false; unsupported controls are warned.
  --capabilities JSON          Per-dispatch provider capability grants.
  --resume-session ID          Resume a retained provider session.
  --no-preface                 Omit the route attribution preface.
  --access-mode MODE           read_only (default) or worktree_write.
  --worktree PATH              Git worktree root the writer owns exclusively.
                               Required by, and only valid with, worktree_write.
  --out PATH                   Clean output path; defaults to mktemp.
  --run-dir PATH               Provider attempt directory; defaults to a private temp directory.
  --prompt TEXT                Prompt text.
  --prompt-file PATH           Read prompt from file.
  --doctor                     Print local dispatch diagnostics and exit.
  -h, --help                   Show this help.

When Fabric is used, the caller records any Fabric correlation.
"""

# The adapters with an executing arm in the supervisor. This is the only
# adapter list kept here: dispatch state (implemented / dormant / unsupported)
# is owned by `dispatch_registry` in config/adapter-compatibility.yaml.
# There are no dormant
# adapters today; a future dormant adapter must be refused here (absent from
# this list) until its arm and safety boundary exist.
DISPATCH_IMPLEMENTED_ADAPTERS = ("agy", "claude", "codex", "copilot", "cursor", "kiro", "opencode")

# Mirror of the routing catalogue's per-adapter endpoint_provider, drift-bound
# by tests/test_adapter_identity_maps.py.
ENDPOINT_PROVIDERS = {
    "claude": "anthropic",
    "codex": "openai",
    "cursor": "cursor",
    "agy": "agy",
    "kiro": "aws",
    "copilot": "github",
    "opencode": "opencode",
}
FAMILY_ALIASES = {"claude": "anthropic", "codex": "openai"}
ORCHESTRATOR_FAMILIES = ("anthropic", "openai")
PROVIDER_CLIS = {"cursor": "cursor-agent", "kiro": "kiro-cli"}
MODEL_DEFAULT_ENV = {
    "cursor": "CF_DISPATCH_CURSOR_MODEL",
    "kiro": "CF_DISPATCH_KIRO_MODEL",
    "copilot": "CF_DISPATCH_COPILOT_MODEL",
    "opencode": "CF_DISPATCH_OPENCODE_MODEL",
}

# The route fields a successful route must carry as non-empty strings, and the
# fields every route must carry as NUL-free strings. tests/test_dispatch_run.py
# binds the fast planner's copies of these to them.
ROUTE_REQUIRED_FIELDS = ("resolved_model", "model_family", "endpoint_provider", "identity_source")
ROUTE_FIELDS = (
    "status", "resolved_model", "model_family", "endpoint_provider",
    "identity_source", "requested_effort", "effort", "effort_source",
    "effort_capability_source", "effort_substitution", "substitution",
    "fallback_model", "catalog_model", "model_selection",
    "model_override_tier", "policy_override", "alias", "reason",
    "endpoint_profile", "endpoint_base_url", "endpoint_token_env",
    "endpoint_wire_api",
)

VALUE_FLAGS = {
    "--fallback": "fallback", "--cwd": "provider_cwd", "--sandbox": "sandbox",
    "--account": "account", "--network": "network", "--capabilities": "capabilities",
    "--resume-session": "resume_session", "--tool": "tool",
    "--task-class": "task_class", "--model": "model", "--effort": "effort",
    "--access-mode": "access_mode", "--timeout-seconds": "timeout_seconds",
    "--worktree": "worktree", "--out": "out", "--run-dir": "run_dir",
    "--prompt": "prompt", "--prompt-file": "prompt_file",
    "--original-prompt-file": "original_prompt_file", "--chain": "chain",
    "--orchestrator-family": "orchestrator_family", "--intent": "intent",
    "--execution-intent": "intent", "--alias": "model_alias", "--role": "route_role",
    "--risk-tier": "risk_tier", "--model-override-tier": "model_override_tier",
    "--reviewer-id": "reviewer_id",
}


@dataclass
class Options:
    tool: str = ""
    model: str = ""
    effort: str = ""
    out: str = ""
    run_dir: str = ""
    prompt: str = ""
    prompt_file: str = ""
    original_prompt_file: str = ""
    chain: str = ""
    orchestrator_family: str = ""
    model_alias: str = ""
    task_class: str = ""
    route_role: str = "reviewer"
    risk_tier: str = ""
    model_override_tier: str = ""
    reviewer_id: str = ""
    intent: str = "assurance"
    sandbox: str = ""
    network: str = ""
    capabilities: str = ""
    account: str = ""
    resume_session: str = ""
    provider_cwd: str = ""
    fallback: str = ""
    access_mode: str = "read_only"
    timeout_seconds: str = ""
    worktree: str = ""
    doctor: bool = False
    plan_only: bool = False
    preface: bool = True
    alias_explicit: bool = False
    add_dirs: list[str] = field(default_factory=list)
    read_roots: list[str] = field(default_factory=list)


@dataclass
class Attempt:
    """One adapter attempt: what the receipt records and the route supplied."""

    tool: str
    model: str = ""
    effort: str = ""
    status: str = ""
    guarantee: str = ""
    rc: int = 1
    alias: str = ""
    route_alias: str = ""
    family: str = ""
    endpoint: str = ""
    identity: str = ""
    requested_model: str = ""
    requested_effort: str = ""
    effort_source: str = ""
    effort_capability_source: str = ""
    effort_substitution: str = ""
    substitution: str = ""
    fallback_model: str = ""
    catalog_model: str = ""
    model_selection: str = ""
    policy_override: str = ""
    route_model_override_tier: str = ""
    route_reason: str = ""


def die(message: str, code: int = 2) -> None:
    print(message, file=sys.stderr)
    raise SystemExit(code)


def write_stream(stream, text: str) -> None:
    stream.flush()
    stream.buffer.write(text.encode("utf-8", "surrogateescape"))
    stream.buffer.flush()


def parse_args(argv: list[str]) -> Options:
    opts = Options()
    index = 0
    while index < len(argv):
        flag = argv[index]
        if flag in ("-h", "--help"):
            write_stream(sys.stdout, USAGE)
            raise SystemExit(0)
        if flag == "--doctor":
            opts.doctor = True
        elif flag == "--plan-only":
            opts.plan_only = True
        elif flag == "--no-preface":
            opts.preface = False
        elif flag in VALUE_FLAGS or flag in ("--add-dir", "--read-root"):
            if index + 1 >= len(argv):
                die(f"missing value for {flag}")
            value = argv[index + 1]
            index += 1
            if flag == "--add-dir":
                opts.add_dirs.append(value)
            elif flag == "--read-root":
                opts.read_roots.append(value)
            else:
                setattr(opts, VALUE_FLAGS[flag], value)
                if flag == "--alias":
                    opts.alias_explicit = True
        else:
            die(f"unknown arg: {flag}")
        index += 1
    return opts


def git_output(directory: str, *args: str) -> str:
    """Stdout of a git command in `directory`, or empty when it fails."""
    try:
        result = subprocess.run(
            ["git", "-C", directory, *args], stdout=subprocess.PIPE,
            stderr=subprocess.DEVNULL, text=True, errors="replace",
        )
    except OSError:
        return ""
    return result.stdout.rstrip("\n") if result.returncode == 0 else ""


def validate_options(opts: Options) -> None:
    if opts.intent not in ("assurance", "ordinary"):
        die(f"invalid intent: {opts.intent}")
    # The caller's deadline, in whole seconds. Only the arms whose CLI accepts a
    # headless timeout consume it; the rest stay bounded by the calling owner.
    if opts.timeout_seconds and not re.fullmatch(r"[1-9][0-9]*", opts.timeout_seconds):
        die(f"invalid timeout-seconds: {opts.timeout_seconds}")
    # An adapter with no arm is an input error, refused here rather than after a
    # temporary directory, prompt staging and route resolution have been paid for.
    for candidate in (*opts.tool.split(), *opts.chain.split()):
        adapter = candidate.split(":", 1)[0]
        if adapter not in DISPATCH_IMPLEMENTED_ADAPTERS:
            die(f"unimplemented adapter: {adapter} (known: {' '.join(DISPATCH_IMPLEMENTED_ADAPTERS)})")
    validate_access_mode(opts)
    if not opts.alias_explicit and not opts.task_class:
        if opts.model or opts.model_override_tier:
            # An explicitly named model has already made the cost decision, so leave the
            # alias at principal rather than narrowing the candidate list under it.
            opts.model_alias = "principal"
        elif opts.route_role in ("lead", "orchestrator", "critical-review"):
            opts.model_alias = "principal"
        else:
            opts.model_alias = "senior"


def validate_access_mode(opts: Options) -> None:
    """Access mode is the only writable route.

    It stays off by default, is refused for assurance work, and demands a Git
    worktree root the caller has already given the worker exclusively; the
    dispatch owner holds the one-writer lease over that path.
    """
    if opts.access_mode == "read_only":
        if opts.worktree:
            die("--worktree requires --access-mode worktree_write")
        return
    if opts.access_mode != "worktree_write":
        die(f"invalid access mode: {opts.access_mode}")
    if not opts.worktree:
        die("--access-mode worktree_write requires --worktree")
    if opts.intent != "ordinary":
        die("--access-mode worktree_write requires --intent ordinary")
    if opts.tool not in DISPATCH_IMPLEMENTED_ADAPTERS:
        die(f"--access-mode worktree_write is unsupported for adapter: {opts.tool or '<chain>'}")
    if not shutil.which("git"):
        die("--access-mode worktree_write requires git")
    if not (os.path.isdir(opts.worktree) and os.access(opts.worktree, os.X_OK)):
        die("--worktree is not a readable directory")
    worktree = os.path.realpath(opts.worktree)
    top = git_output(worktree, "rev-parse", "--path-format=absolute", "--show-toplevel")
    if not top or os.path.realpath(top) != worktree:
        die(f"--worktree must be the root of a Git worktree: {worktree}")
    common = git_output(worktree, "rev-parse", "--path-format=absolute", "--git-common-dir")
    git_dir = git_output(worktree, "rev-parse", "--path-format=absolute", "--absolute-git-dir")
    if not common or git_dir == common:
        die("worktree_write requires a linked worktree; fix: create a linked worktree")
    opts.worktree = worktree


def append_cli_paths() -> None:
    home = os.environ.get("HOME", "")
    candidates = ["/opt/homebrew/bin", "/usr/local/bin"]
    if home:
        candidates += [f"{home}/.local/bin", f"{home}/bin"]
    path = os.environ.get("PATH", "")
    for directory in candidates:
        if os.path.isdir(directory) and f":{directory}:" not in f":{path}:":
            path = f"{path}:{directory}"
    os.environ["PATH"] = path


def logical_pwd() -> str:
    """The shell's `pwd`: $PWD when it still names the working directory."""
    pwd = os.environ.get("PWD", "")
    try:
        if pwd and os.path.samefile(pwd, "."):
            return pwd
    except OSError:
        pass
    return os.getcwd()


def show_doctor() -> None:
    lines = ["cf_dispatch doctor", f"pwd={logical_pwd()}", f"PATH={os.environ.get('PATH', '')}"]
    root = git_output(".", "rev-parse", "--show-toplevel")
    if root:
        status = git_output(".", "status", "--short")
        lines += [f"git_root={root}", f"git_status_short_count={len(status.splitlines())}"]
    else:
        lines.append("git_root=NONE")
    for name in ("CF_DISPATCH_ENABLE_KIRO", "CF_DISPATCH_ENABLE_COPILOT"):
        lines.append(f"{name}={os.environ.get(name) or '0'}")
    lines.append(f"CF_DISPATCH_AGY_ADD_DIR={os.environ.get('CF_DISPATCH_AGY_ADD_DIR', '')}")
    for tool in ("claude", "codex", "cursor-agent", "agy", "kiro-cli", "copilot", "opencode"):
        command = shutil.which(tool)
        if not command:
            lines.append(f"{tool}=NOT_FOUND")
            continue
        lines.append(f"{tool}={command}")
        if tool in ("claude", "codex", "agy", "opencode"):
            try:
                version = subprocess.run(
                    [command, "--version"], stdout=subprocess.PIPE, stderr=subprocess.DEVNULL,
                    text=True, errors="replace",
                ).stdout.splitlines()
            except OSError:
                version = []
            if version:
                lines.append(f"{tool}_version={version[0]}")
    write_stream(sys.stdout, "\n".join(lines) + "\n")


@functools.cache
def resolve_product_root() -> str:
    """The single product-root derivation in this script (#754).

    It mirrors the precedence in scripts/lib/roots.py: an explicit
    AGENT_FABRIC_PRODUCT_ROOT wins, because a caller who set it knows better
    than any derivation; then the repository this script physically lives in,
    so a linked worktree tests its own config; then the checkout layout above
    skills/<skill>/scripts.
    """
    explicit = os.environ.get("AGENT_FABRIC_PRODUCT_ROOT")
    if explicit:
        return explicit
    return git_output(SCRIPT_DIR, "rev-parse", "--show-toplevel") or os.path.normpath(
        os.path.join(SCRIPT_DIR, "..", "..", "..")
    )


def parse_route(text: str) -> dict[str, str]:
    """Validate a route record and return every field of it as a string."""

    def reject_duplicate_members(pairs):
        value = {}
        for key, item in pairs:
            if key in value:
                raise ValueError(f"duplicate route member: {key}")
            value[key] = item
        return value

    text.encode("utf-8")  # a record that is not valid UTF-8 is not a record
    route = json.loads(text, object_pairs_hook=reject_duplicate_members)
    if not isinstance(route, dict):
        raise ValueError("route must be a JSON object")
    if route.get("status") == "ok":
        for key in ROUTE_REQUIRED_FIELDS:
            value = route.get(key)
            if not isinstance(value, str) or not value.strip():
                raise ValueError(f"successful route requires non-empty {key}")
    fields = {}
    for key in ROUTE_FIELDS:
        value = route.get(key, "")
        if value is None:
            value = ""
        if not isinstance(value, str) or "\0" in value:
            raise ValueError(f"route field {key} must be a NUL-free string")
        fields[key] = value
    return fields


def append_text(path: str, text: str) -> None:
    with open(path, "ab") as handle:
        handle.write(text.encode("utf-8", "surrogateescape"))


class Dispatcher:
    def __init__(self, opts: Options) -> None:
        self.opts = opts
        self.out = opts.out
        self.out_created = False
        self.prompt_tmp = ""
        self.prompt_staged = False
        self.active_run_tmpdir = ""
        self.identity: tuple[str, int, int] | None = None
        self.signalled = False
        self.all_add_dirs = [*opts.add_dirs, *(
            path for path in os.environ.get("CF_DISPATCH_AGY_ADD_DIR", "").split(":") if path
        )]

    # Temporary files and cleanup.

    def make_tmp(self, directory: bool) -> str:
        """Create a private temp file or directory, or return '' after a diagnostic.

        The PID keeps names unique among live dispatches; creation is exclusive.
        """
        root = os.environ.get("TMPDIR") or "/tmp"
        kind = "directory" if directory else "file"
        if not os.path.isdir(root):
            print(f"temporary directory does not exist: {root}", file=sys.stderr)
            return ""
        prefix = "cf-dispatch-run" if directory else "cf-dispatch"
        for _ in range(8):
            path = f"{root}/{prefix}.{os.getpid()}.{secrets.randbelow(10**10)}"
            try:
                if directory:
                    os.mkdir(path, 0o700)
                else:
                    os.close(os.open(path, os.O_WRONLY | os.O_CREAT | os.O_EXCL, 0o600))
            except OSError:
                continue
            return path
        print(f"could not create a temporary {kind} in {root}", file=sys.stderr)
        return ""

    def cleanup(self) -> None:
        if self.prompt_tmp:
            remove_file(self.prompt_tmp)
        if self.active_run_tmpdir:
            shutil.rmtree(self.active_run_tmpdir, ignore_errors=True)

    def on_signal(self, _signum, _frame) -> None:
        # Like a shell trap, take effect once the foreground child has exited.
        self.signalled = True

    def abort_if_signalled(self) -> None:
        if not self.signalled:
            return
        self.cleanup()
        if self.out:
            remove_file(self.out + ".raw.jsonl")
            if self.out_created:
                remove_file(self.out)
        raise SystemExit(143)

    def stage_prompt(self) -> bool:
        if self.prompt_staged:
            return True
        try:
            if self.opts.prompt_file:
                shutil.copyfile(self.opts.prompt_file, self.prompt_tmp)
            else:
                with open(self.prompt_tmp, "wb") as handle:
                    handle.write(os.fsencode(self.opts.prompt))
            with open(self.prompt_tmp, "rb") as handle:
                content = handle.read()
        except OSError:
            return False
        if not content or b"\0" in content:
            return False
        self.prompt_staged = True
        return True

    def install(self, source: str) -> bool:
        """Install `source` as the identity-bound output, keeping its identity for the receipt."""
        self.identity = None
        try:
            self.identity = output_custody.install(source, self.out)
        except (output_custody.CustodyError, OSError) as exc:
            print(f"output custody failed: {exc}", file=sys.stderr)
        return self.identity is not None

    def verify(self, destination: str) -> bool:
        if self.identity is None:
            return False
        try:
            output_custody.verify(destination, *self.identity)
        except (output_custody.CustodyError, OSError) as exc:
            print(f"output custody failed: {exc}", file=sys.stderr)
            return False
        return True

    # The receipt.

    def emit(self, a: Attempt, status: str, rc: int, path: str, guarantee: str,
             full: bool = False) -> tuple[str, bool]:
        """Format the dispatch receipt line and whether the attempt succeeded.

        `full` carries the route's catalogue, selection, override and reason
        fields; the early rejections before supervision record only the rest.
        """
        opts = self.opts
        alias = "" if a.model and not opts.alias_explicit else a.alias
        requested_model = a.requested_model or a.model
        model = a.model or os.environ.get(MODEL_DEFAULT_ENV.get(a.tool, ""), "")
        output_digest = ""
        if path:
            if self.verify(path):
                output_digest = self.identity[0]
            else:
                path, guarantee = "", "none"
        if status == "ok" and not output_digest:
            status, rc, path, guarantee = "output_identity_invalid", 1, "", "none"
        cross = (
            opts.orchestrator_family in ORCHESTRATOR_FAMILIES
            and a.family not in ("", "generic-open", "open-weight")
            and opts.orchestrator_family != a.family
        )
        certified = (
            opts.intent == "assurance" and status == "ok" and bool(output_digest) and cross
            and guarantee in ("enforced", "oauth_safe_mode")
        )
        record = {
            "tool": a.tool,
            "adapter": a.tool,
            "adapter_gate": "direct-cli",
            "execution_intent": opts.intent,
            "model": model,
            "requested_model": requested_model,
            "resolved_model": model,
            "fallback_model": a.fallback_model,
            "requested_effort": a.requested_effort,
            "effort": a.effort,
            "effort_source": a.effort_source,
            "effort_capability_source": a.effort_capability_source,
            "effort_substitution": a.effort_substitution,
            "substitution": a.substitution,
            "status": status,
            "reason": a.route_reason if full else "",
            "exit": rc,
            "output_path": path,
            "output_digest": output_digest,
            "read_only_guarantee": guarantee,
            "provider_sandbox": None,
            "provider_network": None,
            "access_mode": opts.access_mode,
            "worktree": opts.worktree,
            "orchestrator_family": opts.orchestrator_family,
            "provider_family": a.family,
            "model_family": a.family,
            "endpoint_provider": a.endpoint or ENDPOINT_PROVIDERS.get(a.tool, ""),
            "identity_source": a.identity or "unresolved",
            "catalog_model": a.catalog_model if full else "",
            "model_selection": a.model_selection if full else "",
            "route_alias": alias,
            "reviewer_id": opts.reviewer_id,
            "risk_tier": opts.risk_tier,
            "model_override_tier": (a.route_model_override_tier if full else "") or opts.model_override_tier,
            "policy_override": a.policy_override if full else "",
            "cross_family": cross,
            "certification_eligible": certified,
        }
        return json.dumps(record, separators=(",", ":")) + "\n", status == "ok"

    # Routing.

    def resolve_routing(self, a: Attempt, requested_model: str, effort: str,
                        capabilities_file: str, diag: str) -> tuple[int, str]:
        """Resolve the route through the model-route owner.

        Returns the owner's exit status and JSON. Without its wrapper or source,
        the JSON carries status model_routing_unavailable.
        """
        opts = self.opts
        # Pin route resolution to the product tree containing this dispatcher.
        # This matters when dispatch runs from a linked worktree, whose config
        # edits must be the ones under test rather than the primary checkout's.
        # The instance root remains independently selected below.
        product_root = resolve_product_root()
        instance_root = os.environ.get("AGENT_FABRIC_INSTANCE_ROOT") or f"{os.environ.get('HOME', '')}/.agents"
        # Catalogue fallback belongs to the route-owner invocation, never the provider seat.
        if not os.path.lexists(f"{instance_root}/config/model-routing.json"):
            instance_root = product_root

        args = ["--adapter", a.tool, "--role", opts.route_role, "--lead-family", opts.orchestrator_family]
        args += ["--task-class", opts.task_class] if opts.task_class else ["--alias", a.alias]
        if opts.intent == "assurance":
            args.append("--require-distinct")
        for flag, value in (
            ("--fallback", opts.fallback), ("--model", requested_model), ("--effort", effort),
            ("--model-override-tier", opts.model_override_tier),
        ):
            if value:
                args += [flag, value]
        if capabilities_file and os.path.isfile(capabilities_file):
            args += ["--capabilities-file", capabilities_file]
        # Endpoint profiles are named in the routing catalogue, never configured here:
        # the caller names one, and the router decides whether it is usable.
        endpoint = os.environ.get("CF_DISPATCH_ENDPOINT")
        if endpoint:
            args += ["--endpoint", endpoint]

        if os.access(f"{product_root}/scripts/model-route", os.X_OK):
            # The wrapper picks an interpreter with PyYAML; a stock python3 may lack it.
            command = [f"{product_root}/scripts/model-route", "resolve", *args]
        elif os.path.isfile(f"{product_root}/scripts/model_route.py"):
            command = [sys.executable, f"{product_root}/scripts/model_route.py", "resolve", *args]
        else:
            return 127, '{"status":"model_routing_unavailable","reason":"scripts/model-route and scripts/model_route.py not found"}'
        env = dict(os.environ)
        # A defaulted alias beside a named model is ours, not the caller's; an env
        # hint (not a flag) preserves that distinction in the model-route owner.
        env["FABRIC_ALIAS_IMPLIED"] = "0" if opts.alias_explicit else "1"
        env["AGENT_FABRIC_PRODUCT_ROOT"] = product_root
        env["AGENT_FABRIC_INSTANCE_ROOT"] = instance_root
        with open(diag, "ab") as stderr:
            try:
                result = subprocess.run(command, env=env, stdout=subprocess.PIPE, stderr=stderr)
            except OSError as exc:
                stderr.write(f"{command[0]}: {exc.strerror}\n".encode())
                return (127 if isinstance(exc, FileNotFoundError) else 126), ""
        return result.returncode, result.stdout.decode("utf-8", "surrogateescape").rstrip("\n")

    def parse_route_record(self, a: Attempt, route_json: str, tmpdir: str, diag: str) -> bool:
        """Persist the route for the supervisor and load its fields into `a`."""
        with open(f"{tmpdir}/route.json", "wb") as handle:
            handle.write(route_json.encode("utf-8", "surrogateescape"))
        try:
            fields = parse_route(route_json)
        except ValueError as exc:  # JSONDecodeError included
            append_text(diag, f"{type(exc).__name__}: {exc}\n")
            return False
        a.status = fields["status"]
        a.model = fields["resolved_model"]
        a.family = fields["model_family"]
        a.endpoint = fields["endpoint_provider"]
        a.identity = fields["identity_source"]
        a.requested_effort = fields["requested_effort"]
        a.effort = fields["effort"]
        a.effort_source = fields["effort_source"]
        a.effort_capability_source = fields["effort_capability_source"]
        a.effort_substitution = fields["effort_substitution"]
        a.substitution = fields["substitution"]
        a.fallback_model = fields["fallback_model"]
        a.catalog_model = fields["catalog_model"]
        a.model_selection = fields["model_selection"]
        a.route_model_override_tier = fields["model_override_tier"]
        a.policy_override = fields["policy_override"]
        a.route_reason = fields["reason"]
        a.route_alias = fields["alias"]
        return bool(a.status)

    def preflight_original(self, route: dict, provider_cwd: str, diag: str) -> bool:
        """Refuse protected inputs the selected route cannot confine."""
        import provider_exec

        opts = self.opts
        add_dirs = [path for path in self.all_add_dirs if path]
        try:
            for candidate in [route, *route.get("fallback_candidates", [])]:
                protected = []
                for prompt_file in dict.fromkeys((opts.original_prompt_file, opts.prompt_file)):
                    protected = provider_exec.provider_confinement.check_protected_inputs(
                        candidate, os.getcwd(), provider_cwd, worktree=opts.worktree or None,
                        prompt_file=prompt_file or None, add_dirs=add_dirs,
                    )
                if protected and (candidate.get("adapter") == "codex" or not provider_exec.provider_confinement._sandbox_exec_path()):
                    raise ValueError("protected paths require sandbox-exec read confinement; fix: use a non-training route")
        except (OSError, ValueError) as exc:
            append_text(diag, f"{exc}\n")
            return False
        except Exception:
            append_text(diag, traceback.format_exc())
            return False
        return True

    def probe_capabilities(self, a: Attempt, tmpdir: str, diag: str, *extra: str) -> str:
        """Run the capability probe for the tool; return its file, or '' when it failed."""
        capabilities_file = f"{tmpdir}/{a.tool}-capabilities.json"
        with open(diag, "ab") as log:
            result = subprocess.run(
                [sys.executable, f"{SCRIPT_DIR}/capabilities.py", a.tool, "--out", capabilities_file, *extra],
                stdout=log, stderr=subprocess.STDOUT,
            )
        if result.returncode == 0:
            return capabilities_file
        remove_file(capabilities_file)
        return ""

    def reroute(self, a: Attempt, requested_model: str, route_effort_input: str,
                capabilities_file: str, tmpdir: str, diag: str) -> tuple[int, str]:
        rc, route_json = self.resolve_routing(a, requested_model, route_effort_input, capabilities_file, diag)
        if not self.parse_route_record(a, route_json, tmpdir, diag):
            a.status = "routing_record_invalid"
            rc = 1
        return rc, route_json

    # One attempt.

    def attempt(self, tool: str, model_pin: str, route_effort_input: str, tmpdir: str,
                chain: bool) -> tuple[int, str]:
        """Run one adapter; return the exit status and what it writes to stdout."""
        opts = self.opts
        a = Attempt(tool=tool, effort=route_effort_input, requested_effort=route_effort_input,
                    alias=opts.model_alias, guarantee="none")
        a.model = model_pin or os.environ.get(MODEL_DEFAULT_ENV.get(tool, ""), "")
        a.requested_model = a.model
        diag = f"{tmpdir}/diag"
        clean = f"{tmpdir}/clean"
        for path in (diag, clean):
            open(path, "wb").close()
        agy_text = f"{a.model} {a.effort} {os.environ.get('CF_DISPATCH_AGY_ADD_DIR', '')}"
        if tool == "agy" and "--dangerously-skip-permissions" in agy_text:
            a.status = "unsafe_by_default"
            write_text(diag, "agy refused: --dangerously-skip-permissions is not allowed on the read-only route\n")
        elif tool == "codex" and os.environ.get("CF_DISPATCH_CODEX_NETWORK", "1") not in ("0", "1"):
            a.status = "invalid_configuration"
            write_text(diag, "CF_DISPATCH_CODEX_NETWORK must be 0 or 1\n")
        elif tool == "agy" and os.environ.get("CF_DISPATCH_AGY_SANDBOX", "0") not in ("0", "1"):
            a.status = "invalid_configuration"
            write_text(diag, "CF_DISPATCH_AGY_SANDBOX must be 0 or 1\n")
        elif opts.orchestrator_family and opts.orchestrator_family not in ORCHESTRATOR_FAMILIES:
            a.status = "invalid_orchestrator_family"
            write_text(diag, f"invalid orchestrator family: {opts.orchestrator_family}\n")
        elif opts.intent == "assurance" and not opts.orchestrator_family:
            a.status = "orchestrator_family_required"
            write_text(diag, f"{tool} disabled: pass --orchestrator-family so cross-family status can be proven\n")
        else:
            done = self.supervise(a, model_pin, route_effort_input, tmpdir, diag, chain)
            if done is not None:
                return done
        shutil.copyfile(diag, clean)
        path = self.out if self.install(clean) else ""
        line, ok = self.emit(a, a.status, a.rc, path, a.guarantee, full=True)
        return (0 if ok else 1), line

    def supervise(self, a: Attempt, model_pin: str, route_effort_input: str, tmpdir: str,
                  diag: str, chain: bool) -> tuple[int, str] | None:
        """Resolve the route and hand it to the supervisor.

        Returns the attempt's result when it ends here, or None to record the
        rejection left in `a` and the diagnostic.
        """
        opts = self.opts
        tool = a.tool
        # Resolve configuration before any provider-backed capability probe. This
        # makes disabled or malformed adapter policy a non-executing rejection.
        route_rc, route_json = self.resolve_routing(a, a.requested_model, route_effort_input, "", diag)
        if not self.parse_route_record(a, route_json, tmpdir, diag):
            a.status = "routing_record_invalid"
            append_text(diag, "model routing returned no valid JSON record\n")
            return None
        if tool == "codex" and a.status == "capability_discovery_failed":
            capabilities = self.probe_capabilities(a, tmpdir, diag)
            if capabilities:
                route_rc, route_json = self.reroute(a, a.requested_model, route_effort_input, capabilities, tmpdir, diag)
        elif tool == "agy" and a.status in ("ok", "model_required_for_broker"):
            capabilities = self.probe_capabilities(a, tmpdir, diag)
            if capabilities:
                route_rc, route_json = self.reroute(a, a.requested_model, route_effort_input, capabilities, tmpdir, diag)
        if (tool == "claude" and opts.task_class and a.status == "task_class_capability_unverified"
                and a.model and a.requested_effort):
            capabilities = self.probe_capabilities(a, tmpdir, diag, "--alias", a.model, "--effort", a.requested_effort)
            if capabilities:
                route_rc, route_json = self.reroute(a, a.requested_model, route_effort_input, capabilities, tmpdir, diag)
        if a.route_alias:
            a.alias = a.route_alias
        a.requested_model = a.requested_model or a.model
        if route_rc != 0 or a.status != "ok":
            append_text(diag, route_json + "\n")
            return None

        selected_cwd = opts.worktree or opts.provider_cwd or os.getcwd()
        if not self.preflight_original(json.loads(route_json), selected_cwd, diag):
            return self.reject(a, "protected_path_denied", 2, diag, 2)
        if not self.stage_prompt():
            write_text(diag, "cannot retain prompt or prompt contains unsupported NUL bytes\n")
            return self.reject(a, "prompt_unavailable", 2, diag, 2)
        provider_cli = PROVIDER_CLIS.get(tool, tool)
        if not shutil.which(provider_cli):
            write_text(diag, f"{provider_cli} not found. PATH={os.environ.get('PATH', '')}\n")
            return self.reject(a, "tool_missing", 127, diag, 1)

        supervisor = [
            sys.executable, f"{SCRIPT_DIR}/provider_exec.py", "--route-file", f"{tmpdir}/route.json",
            "--adapter", tool, "--prompt-file", self.prompt_tmp, "--out", self.out,
            "--run-dir", opts.run_dir or tmpdir, "--mode", opts.access_mode,
            "--workspace-root", os.getcwd(),
            "--intent", opts.intent, "--orchestrator-family", opts.orchestrator_family,
            "--reviewer-id", opts.reviewer_id,
            "--risk-tier", opts.risk_tier, "--model-override-tier", opts.model_override_tier,
            "--requested-model", model_pin, "--requested-effort", route_effort_input,
        ]
        if opts.original_prompt_file:
            supervisor += ["--original-prompt-file", opts.original_prompt_file]
        if opts.plan_only:
            supervisor.append("--plan-only")
        if not opts.preface:
            supervisor.append("--no-preface")
        for flag, value in (
            ("--worktree", opts.worktree), ("--cwd", opts.provider_cwd), ("--sandbox", opts.sandbox),
            ("--account", opts.account), ("--network", opts.network), ("--capabilities", opts.capabilities),
            ("--resume-session", opts.resume_session), ("--timeout-seconds", opts.timeout_seconds),
        ):
            if value:
                supervisor += [flag, value]
        for flag, values in (("--add-dir", self.all_add_dirs), ("--read-root", opts.read_roots)):
            for value in values:
                if value:
                    supervisor += [flag, value]
        self.abort_if_signalled()
        if not chain:
            supervisor += ["--cleanup-dir", tmpdir, "--cleanup-prompt"]
            sys.stdout.flush()
            signal.signal(signal.SIGPIPE, signal.SIG_DFL)  # Python ignores it; the supervisor must not inherit that
            try:
                os.execv(sys.executable, supervisor)
            except OSError as exc:
                print(f"{supervisor[0]}: {exc.strerror}", file=sys.stderr)
                return (127 if isinstance(exc, FileNotFoundError) else 126), ""
        result = subprocess.run(supervisor, stdout=subprocess.PIPE)
        return result.returncode, result.stdout.decode("utf-8", "surrogateescape")

    def reject(self, a: Attempt, status: str, rc: int, diag: str, returned: int) -> tuple[int, str]:
        """Record a rejection before supervision, installing the diagnostic as output."""
        path = self.out if self.install(diag) else ""
        line, _ = self.emit(a, status, rc, path, "none")
        return returned, line

    # Entry.

    def run(self) -> int:
        opts = self.opts
        if opts.doctor:
            show_doctor()
            return 0
        policy = f"{SCRIPT_DIR}/../../../scripts/worktree.py"
        if not os.path.isfile(policy):
            die(f"worktree context validator is unavailable: {policy}")
        validated = subprocess.run(
            [sys.executable, policy, "validate-context", "--repo", os.getcwd(), "--allow-non-git"],
            stdout=subprocess.DEVNULL,
        )
        if validated.returncode != 0:
            return 2
        if opts.prompt_file:
            if not os.access(opts.prompt_file, os.R_OK):
                die(f"cannot read prompt file: {opts.prompt_file}")
            opts.original_prompt_file = opts.original_prompt_file or opts.prompt_file
        elif not opts.prompt:
            die("need --prompt or --prompt-file")
        for signum in (signal.SIGINT, signal.SIGTERM, signal.SIGHUP):
            signal.signal(signum, self.on_signal)
        try:
            return self.dispatch()
        finally:
            self.cleanup()

    def dispatch(self) -> int:
        opts = self.opts
        if not self.out:
            self.out_created = True
            self.out = self.make_tmp(False)
            if not self.out:
                return 1
        self.prompt_tmp = self.make_tmp(False)
        if not self.prompt_tmp:
            return 1
        opts.orchestrator_family = FAMILY_ALIASES.get(opts.orchestrator_family, opts.orchestrator_family)

        if not opts.chain:
            if not opts.tool:
                die("need --tool or --chain")
            self.active_run_tmpdir = self.make_tmp(True)
            if not self.active_run_tmpdir:
                return 1
            rc, line = self.attempt(opts.tool, opts.model, opts.effort, self.active_run_tmpdir, False)
            self.abort_if_signalled()
            write_stream(sys.stdout, line)
            return rc

        for spec in opts.chain.split():
            tool, _, rest = spec.partition(":")
            model, _, effort = rest.partition(":") if ":" in spec else ("", "", "")
            if effort == model:
                effort = ""
            self.active_run_tmpdir = self.make_tmp(True)
            if not self.active_run_tmpdir:
                return 1
            self.prompt_staged = False
            rc, line = self.attempt(tool, model, effort, self.active_run_tmpdir, True)
            shutil.rmtree(self.active_run_tmpdir, ignore_errors=True)
            self.active_run_tmpdir = ""
            self.abort_if_signalled()
            record = line.rstrip("\n") + "\n"
            write_stream(sys.stderr, record)
            if rc == 0:
                write_stream(sys.stdout, record)
                return 0
            remove_file(self.out + ".raw.jsonl")
        remove_file(self.out + ".raw.jsonl")
        if self.out_created:
            remove_file(self.out)
        line, _ = self.emit(Attempt(tool="chain", alias=opts.model_alias), "all_failed", 1, "", "none")
        write_stream(sys.stdout, line)
        return 1


def remove_file(path: str) -> None:
    try:
        os.unlink(path)
    except OSError:
        pass


def write_text(path: str, text: str) -> None:
    with open(path, "wb") as handle:
        handle.write(text.encode("utf-8", "surrogateescape"))


def main(argv: list[str] | None = None) -> int:
    for name in GIT_CONTEXT_VARIABLES:
        os.environ.pop(name, None)
    opts = parse_args(sys.argv[1:] if argv is None else argv)
    validate_options(opts)
    append_cli_paths()
    return Dispatcher(opts).run()


if __name__ == "__main__":
    raise SystemExit(main())
