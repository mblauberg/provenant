#!/usr/bin/env python3
"""Run the real dispatch owner against the deterministic routing adapter."""

from __future__ import annotations

import importlib.util
from pathlib import Path

ROOT = Path(__file__).resolve().parents[3]

# This fixture is executed directly by path, so the product root is not on
# `sys.path`. Load the dispatch owner from its own file: that binds one
# module without changing how anything else resolves imports (#755).
_spec = importlib.util.spec_from_file_location(
    "dispatch_run", ROOT / "skills" / "orchestrate" / "scripts" / "dispatch_run.py"
)
dispatch_run = importlib.util.module_from_spec(_spec)
_spec.loader.exec_module(dispatch_run)


dispatch_run.CF_DISPATCH = Path(__file__).with_name("provider_fixture.py")
raise SystemExit(dispatch_run.dispatch(dispatch_run.parser().parse_args()))
