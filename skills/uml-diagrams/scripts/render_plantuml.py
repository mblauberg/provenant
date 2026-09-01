#!/usr/bin/env python3
"""Render PlantUML files to SVG/PNG/PDF using a local PlantUML installation.

Resolution order:
1. --plantuml-jar path or PLANTUML_JAR environment variable, invoked via java -jar.
2. `plantuml` executable on PATH.

Examples:
    python3 "$(provenant root)/skills/uml-diagrams/scripts/render_plantuml.py" diagram.puml --format svg
    PLANTUML_JAR=/path/to/plantuml.jar python3 "$(provenant root)/skills/uml-diagrams/scripts/render_plantuml.py" diagram.puml --format png
"""
from __future__ import annotations

import argparse
import os
import shutil
import subprocess
import sys
from pathlib import Path
from typing import Iterable

VALID_FORMATS = {"svg", "png", "pdf", "eps", "txt", "utxt"}


def _find_renderer(jar: str | None) -> list[str]:
    jar_path = jar or os.environ.get("PLANTUML_JAR")
    if jar_path:
        path = Path(jar_path).expanduser()
        if not path.exists():
            raise FileNotFoundError(f"PlantUML JAR not found: {path}")
        java = shutil.which("java")
        if java is None:
            raise RuntimeError("Java is required to run plantuml.jar but was not found on PATH.")
        return [java, "-jar", str(path)]

    exe = shutil.which("plantuml")
    if exe:
        return [exe]

    raise RuntimeError(
        "No PlantUML renderer found. Install the `plantuml` CLI or set PLANTUML_JAR=/path/to/plantuml.jar."
    )


def _render_file(renderer: list[str], puml: Path, fmt: str, out_dir: Path | None) -> None:
    if not puml.exists():
        raise FileNotFoundError(f"Input file not found: {puml}")
    if puml.suffix.lower() not in {".puml", ".plantuml", ".uml"}:
        print(f"Warning: {puml} does not look like a PlantUML file", file=sys.stderr)

    cmd = [*renderer, f"-t{fmt}"]
    cwd = puml.parent
    if out_dir is not None:
        out_dir.mkdir(parents=True, exist_ok=True)
        # PlantUML's -o is interpreted relative to the input file's directory.
        try:
            rel_out = out_dir.resolve().relative_to(cwd.resolve())
            out_arg = str(rel_out)
        except ValueError:
            out_arg = str(out_dir.resolve())
        cmd += ["-o", out_arg]
    cmd.append(str(puml.name))

    result = subprocess.run(cmd, cwd=cwd, text=True, capture_output=True)
    if result.returncode != 0:
        raise RuntimeError(
            f"PlantUML failed for {puml}\nCommand: {' '.join(cmd)}\nSTDOUT:\n{result.stdout}\nSTDERR:\n{result.stderr}"
        )
    if result.stdout.strip():
        print(result.stdout.strip())
    if result.stderr.strip():
        print(result.stderr.strip(), file=sys.stderr)


def render(paths: Iterable[Path], fmt: str, out_dir: Path | None, jar: str | None) -> None:
    if fmt not in VALID_FORMATS:
        raise ValueError(f"Unsupported format {fmt!r}. Choose one of: {', '.join(sorted(VALID_FORMATS))}")
    renderer = _find_renderer(jar)
    for puml in paths:
        _render_file(renderer, puml, fmt, out_dir)


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description="Render PlantUML diagrams with a local CLI/JAR.")
    parser.add_argument("paths", nargs="+", type=Path, help="PlantUML .puml files to render")
    parser.add_argument("--format", "-t", default="svg", choices=sorted(VALID_FORMATS), help="Output format")
    parser.add_argument("--out-dir", "-o", type=Path, default=None, help="Optional output directory")
    parser.add_argument("--plantuml-jar", default=None, help="Path to plantuml.jar; overrides PLANTUML_JAR")
    args = parser.parse_args(argv)

    try:
        render(args.paths, args.format, args.out_dir, args.plantuml_jar)
    except Exception as exc:  # keep CLI errors readable for agents
        print(f"ERROR: {exc}", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
