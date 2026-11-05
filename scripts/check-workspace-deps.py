#!/usr/bin/env python3
# Copyright 2026 Chris Olson — Apache-2.0 (see LICENSE)
"""R1: every member dependency entry is `{ workspace = true [, optional/features] }`."""
import pathlib, sys, tomllib

repo = pathlib.Path(__file__).resolve().parent.parent
errs = []
for manifest in sorted(repo.glob("crates/*/Cargo.toml")):
    data = tomllib.loads(manifest.read_text())
    name = data["package"]["name"]
    for table in ("dependencies", "dev-dependencies", "build-dependencies"):
        for dep, spec in data.get(table, {}).items():
            if dep == name and isinstance(spec, dict) and spec.get("path") == ".":
                continue  # self dev-dependency enabling test features
            if not (isinstance(spec, dict) and spec.get("workspace") is True and "version" not in spec):
                errs.append(f"{manifest}: [{table}] {dep} must be {{ workspace = true }}")
    for key in ("version", "edition", "rust-version", "license", "repository"):
        if data["package"].get(key) != {"workspace": True}:
            errs.append(f"{manifest}: package.{key} must be inherited")
for e in errs:
    print(e, file=sys.stderr)
sys.exit(1 if errs else 0)
