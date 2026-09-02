#!/usr/bin/env python3
"""Capture a normalised runtime capability snapshot for one configured provider.

One module, one per-provider table. A row names the binary the probe shells out
to, whether the probe needs a route (alias and effort), and how the output is
written. The discovery function on the row turns a bounded CLI run into the
snapshot the route resolver consumes. Adding a provider is a row plus its
normaliser, not another copy of the process, JSON and output plumbing.

Providers:

codex
    `codex debug models` prints a JSON catalogue whose reasoning levels are per
    model, so the snapshot keys on the model slug and keeps that model's own
    efforts.

agy
    `agy models` prints one dispatchable model id per line, optionally followed
    by a tab-separated display name, with reasoning effort baked into the id
    suffix, for example `gemini-3.1-pro-high`. The bare family id is not
    dispatchable on its own: `agy --model gemini-3.1-pro` exits 1 with
    "requires --effort (available: low, high)". Efforts are per model rather
    than global, and the CLI's static help can describe a broader effort set
    than an individual model exposes, so the runtime list is authoritative. The
    snapshot keys on the family id and records the efforts that family offers.

claude
    No catalogue command exists, so the probe runs a subscription canary and
    reads the resolved model out of the reported usage. Provider output is
    scrubbed before it reaches a diagnostic, because a failure can carry an
    email address or an organisation id.
"""

from __future__ import annotations

import argparse
from dataclasses import dataclass
from datetime import datetime, timezone
import json
from pathlib import Path
import re
import sys
from typing import Any, Callable

# The shared library sits one level above this skill and is not reachable
# from the script's own directory, so this entry point establishes it (#755).
sys.path.insert(0, str(Path(__file__).resolve().parents[2]))
from _shared.bounded_process import run_bounded


OUTPUT_LIMIT_BYTES = 1_048_576
EFFORTS = {"low", "medium", "high", "xhigh", "max"}
EFFORT_SUFFIX = re.compile(r"^(?P<family>.+?)-(?P<effort>low|medium|high)$")
# Agy also fronts models from other vendors. They are recorded so a caller can
# see them, but they must never satisfy a google-family route, and they must
# never be used to claim a cross-family opinion against the family they
# actually belong to.
FOREIGN_FAMILY = re.compile(r"claude|gpt|llama|mistral|qwen|oss", re.IGNORECASE)


@dataclass(frozen=True)
class Probe:
    """One completed provider CLI run that exited zero within its budget."""

    stdout: str
    stderr: str


def observed_at() -> str:
    return datetime.now(timezone.utc).isoformat().replace("+00:00", "Z")


def scrubbed_failure_detail(stdout: str, stderr: str) -> str:
    combined = "\n".join(part for part in (stderr.strip(), stdout.strip()) if part)
    if re.search(
        r"organi[sz]ation.*disabled.*subscription access|subscription access.*disabled",
        combined,
        re.IGNORECASE | re.DOTALL,
    ):
        return "provider access denied; subscription access disabled by organisation policy"
    if stderr.strip():
        return stderr.strip()
    status = re.search(r"\b(?:HTTP\s*)?(401|403|429)\b", stdout, re.IGNORECASE)
    if status:
        return f"provider returned HTTP {status.group(1)}"
    return ""


def load_json(raw: str) -> Any:
    def reject_duplicates(pairs: list[tuple[str, Any]]) -> dict[str, Any]:
        value: dict[str, Any] = {}
        for key, item in pairs:
            if key in value:
                raise ValueError(f"duplicate JSON member: {key}")
            value[key] = item
        return value

    return json.loads(raw, object_pairs_hook=reject_duplicates)


def run_cli(command: list[str], timeout: int, label: str = "command") -> Probe:
    result = run_bounded(
        command,
        cwd=Path.cwd(),
        timeout_seconds=timeout,
        output_limit_bytes=OUTPUT_LIMIT_BYTES,
        merge_stderr=False,
    )
    stdout = result.stdout or ""
    stderr = (result.stderr or "").strip()
    if result.timed_out:
        detail = f": {stderr}" if stderr else ""
        raise ValueError(f"{label} timed out after {timeout} seconds{detail}")
    if result.returncode != 0:
        reason = scrubbed_failure_detail(stdout, stderr)
        detail = f": {reason}" if reason else ""
        raise ValueError(f"{label} exited {result.returncode}{detail}")
    return Probe(stdout=stdout, stderr=stderr)


def warning_from(probe: Probe, label: str) -> str | None:
    # A successful run that still said something on stderr is not a failure:
    # rejecting it is what took an adapter offline for one warning line, and
    # parsing it is what invented a phantom model id. Keep it visible so the
    # dispatcher's diagnostics record it rather than losing it.
    return f"{label} warned: {probe.stderr}" if probe.stderr else None


def discover_codex(binary: str, options: argparse.Namespace) -> tuple[dict[str, Any], str | None]:
    label = "codex debug models"
    probe = run_cli([binary, "debug", "models"], 10, label)
    return normalise_codex(load_json(probe.stdout)), warning_from(probe, label)


def normalise_codex(raw: Any) -> dict[str, Any]:
    if not isinstance(raw, dict) or not isinstance(raw.get("models"), list):
        raise ValueError("catalogue root must contain a models list")
    models: dict[str, Any] = {}
    for item in raw["models"]:
        if not isinstance(item, dict):
            raise ValueError("catalogue model entry must be an object")
        slug = item.get("slug")
        if not isinstance(slug, str) or not slug.strip():
            raise ValueError("catalogue model slug must be a non-empty string")
        levels = item.get("supported_reasoning_levels")
        if not isinstance(levels, list):
            raise ValueError("catalogue reasoning levels must be a list")
        if not levels:
            raise ValueError("catalogue reasoning levels must not be empty")
        efforts = []
        for level in levels:
            if not isinstance(level, dict):
                raise ValueError("catalogue reasoning-level entry must be an object")
            effort = level.get("effort")
            if not isinstance(effort, str) or not effort.strip():
                raise ValueError("catalogue reasoning effort must be a non-empty string")
            efforts.append(effort.lower())
        normalized_slug = slug.casefold()
        if normalized_slug in models:
            raise ValueError("catalogue model slugs must be unique when case-folded")
        models[normalized_slug] = {
            "resolved_model": slug,
            "supported_efforts": efforts,
        }
    if not models:
        raise ValueError("catalogue contains no usable model entries")
    return {
        "schema_version": 1,
        "source": "codex debug models",
        "observed_at": observed_at(),
        "models": models,
    }


def discover_agy(binary: str, options: argparse.Namespace) -> tuple[dict[str, Any], str | None]:
    label = "agy models"
    probe = run_cli([binary, "models"], 60, label)
    return normalise_agy(probe.stdout), warning_from(probe, label)


def normalise_agy(raw: str) -> dict[str, Any]:
    ids = [
        line.split("\t", 1)[0].strip()
        for line in raw.splitlines()
        if line.strip()
    ]
    if not ids:
        raise ValueError("agy models returned no model ids")

    models: dict[str, Any] = {}
    for model_id in ids:
        match = EFFORT_SUFFIX.match(model_id)
        family = match.group("family") if match else model_id
        entry = models.setdefault(
            family,
            {"resolved_model": family, "supported_efforts": [], "dispatchable_ids": []},
        )
        entry["dispatchable_ids"].append(model_id)
        if match:
            effort = match.group("effort")
            if effort not in entry["supported_efforts"]:
                entry["supported_efforts"].append(effort)

    for entry in models.values():
        entry["supported_efforts"].sort(key=["low", "medium", "high"].index)
        entry["dispatchable_ids"].sort()

    # The route resolver requires every entry in `models` to carry at least one
    # selectable effort. Agy lists some ids with no effort suffix at all, such
    # as claude-sonnet-4-6, which are dispatchable but not effort-controllable.
    # Routing one of those through this adapter would also break family
    # distinctness, since they are not Gemini. They stay visible under
    # `effortless_models` and out of the routable set.
    routable = {k: v for k, v in models.items() if v["supported_efforts"]}
    effortless = sorted(k for k, v in models.items() if not v["supported_efforts"])
    if not routable:
        raise ValueError("agy models listed no effort-controllable model")

    return {
        "schema_version": 1,
        "source": "agy models",
        "observed_at": observed_at(),
        "models": routable,
        "effortless_models": effortless,
        "google_models": sorted(m for m in routable if m.startswith("gemini")),
        "foreign_models": sorted(m for m in models if FOREIGN_FAMILY.search(m)),
    }


def claude_json(command: list[str], timeout: int) -> Any:
    probe = run_cli(command, timeout)
    if "Warning: Unknown --effort value" in probe.stderr:
        raise ValueError("Claude CLI rejected the requested effort")
    return load_json(probe.stdout)


def discover_claude(binary: str, options: argparse.Namespace) -> tuple[dict[str, Any], str | None]:
    alias = options.alias
    effort = options.effort
    auth = claude_json([binary, "auth", "status"], 5)
    if (
        not isinstance(auth, dict)
        or auth.get("loggedIn") is not True
        or auth.get("authMethod") != "claude.ai"
        or not isinstance(auth.get("subscriptionType"), str)
        or not auth["subscriptionType"]
    ):
        raise ValueError("Claude subscription authentication is unavailable")

    result = claude_json([
        binary,
        "-p",
        "--safe-mode",
        "--no-session-persistence",
        "--permission-mode", "plan",
        "--tools", "",
        "--model", alias,
        "--effort", effort,
        "--output-format", "json",
        "Reply exactly OK.",
    ], 30)
    usage = result.get("modelUsage") if isinstance(result, dict) else None
    if (
        not isinstance(result, dict)
        or result.get("type") != "result"
        or result.get("subtype") != "success"
        or result.get("is_error") is not False
        or result.get("result") != "OK"
        or not isinstance(usage, dict)
    ):
        raise ValueError("Claude canary returned an ambiguous or unsuccessful result")
    alias_token = alias.casefold()
    matching_models = [
        model for model in usage
        if isinstance(model, str)
        and model.casefold().startswith("claude-")
        and alias_token in model.casefold().split("-")
    ]
    if len(matching_models) != 1:
        raise ValueError("Claude canary did not identify one primary runtime model")

    return {
        "schema_version": 1,
        "source": "claude subscription canary",
        "observed_at": observed_at(),
        "provenance": {
            "kind": "subscription_runtime_canary",
            "auth_method": "claude.ai",
            "subscription_type": auth["subscriptionType"],
        },
        "models": {
            alias.casefold(): {
                "resolved_model": matching_models[0],
                "requested_effort": effort,
                "effort_verified": False,
            },
        },
    }, None


@dataclass(frozen=True)
class Provider:
    """How one provider is probed and how its snapshot is delivered."""

    default_bin: str
    discover: Callable[[str, argparse.Namespace], tuple[dict[str, Any], str | None]]
    # Claude has no catalogue command, so its canary needs the route it proves.
    needs_route: bool = False
    # A canary snapshot is route-specific evidence, never a stdout convenience.
    out_required: bool = False
    # Subscription provenance is owner-readable only.
    restrict_output: bool = False


PROVIDERS: dict[str, Provider] = {
    "agy": Provider(default_bin="agy", discover=discover_agy),
    "claude": Provider(
        default_bin="claude",
        discover=discover_claude,
        needs_route=True,
        out_required=True,
        restrict_output=True,
    ),
    "codex": Provider(default_bin="codex", discover=discover_codex),
}


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(
        description="Capture a runtime capability snapshot for one provider."
    )
    parser.add_argument("provider", choices=sorted(PROVIDERS))
    parser.add_argument("--out", type=Path)
    parser.add_argument("--bin", help="provider executable; defaults to the provider name")
    parser.add_argument("--alias")
    parser.add_argument("--effort", choices=sorted(EFFORTS))
    args = parser.parse_args(argv)

    provider = PROVIDERS[args.provider]
    if provider.out_required and args.out is None:
        parser.error(f"--out is required for {args.provider}")
    if provider.needs_route:
        if not (args.alias or "").strip():
            parser.error("--alias must be non-empty")
        if not args.effort:
            parser.error("--effort is required for " + args.provider)
    elif args.alias is not None or args.effort is not None:
        parser.error(f"--alias and --effort do not apply to {args.provider}")

    try:
        snapshot, warning = provider.discover(args.bin or provider.default_bin, args)
    except (OSError, json.JSONDecodeError, ValueError) as exc:
        print(f"capability discovery failed: {exc}", file=sys.stderr)
        return 1
    if warning:
        print(warning, file=sys.stderr)
    encoded = json.dumps(snapshot, indent=2, sort_keys=True) + "\n"
    if args.out:
        args.out.write_text(encoded)
        if provider.restrict_output:
            args.out.chmod(0o600)
    else:
        print(encoded, end="")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
