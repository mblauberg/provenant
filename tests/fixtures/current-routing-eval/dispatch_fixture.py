#!/usr/bin/env python3
"""Run the real dispatch owner against the deterministic routing adapter."""

from __future__ import annotations

import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parents[3]
sys.path.insert(0, str(ROOT / "skills" / "orchestrate" / "scripts"))

import dispatch_run


dispatch_run.CF_DISPATCH = Path(__file__).with_name("provider_fixture.py")
raise SystemExit(dispatch_run.dispatch(dispatch_run.parser().parse_args()))
