#!/usr/bin/env python3
# Copyright 2026 Chris Olson — Apache-2.0 (see LICENSE)
"""R1: workspace dependency hygiene and the allowed gadk-* dependency edges.

- Every member dependency entry is `{ workspace = true [, optional/features] }` (a self
  path dev-dependency `{ path = ".", features = [...] }` is allowed).
- Package metadata is inherited.
- Normal gadk-* dependencies of each crate equal ALLOWED[crate] exactly; OPTIONAL edges are
  `optional = true`.
- A gadk-* dev-dependency is the crate itself or a crate that does not (transitively, via
  ALLOWED) depend on it; only `gadk` may depend on `gadk-engine`, normal or dev.
"""
import pathlib, sys, tomllib

ALLOWED = {
    "gadk-genai": set(),
    "gadk-platform": set(),
    "gadk-model": {"gadk-genai"},
    "gadk-gemini": {"gadk-genai", "gadk-model"},
    "gadk-session": {"gadk-genai", "gadk-model", "gadk-platform"},
    "gadk-httprr": {"gadk-model"},
    "gadk-artifact": {"gadk-genai", "gadk-platform"},
    "gadk-memory": {"gadk-genai", "gadk-session"},
    "gadk-agent": {"gadk-genai", "gadk-platform", "gadk-model", "gadk-session", "gadk-artifact", "gadk-memory"},
    "gadk-engine": {"gadk-agent", "gadk-genai", "gadk-platform", "gadk-model", "gadk-session",
                    "gadk-artifact", "gadk-memory", "gadk-gemini", "gadk-httprr"},
    "gadk": {"gadk-genai", "gadk-platform", "gadk-model", "gadk-session", "gadk-gemini",
             "gadk-agent", "gadk-engine", "gadk-artifact", "gadk-memory"},
}
# (crate, dependency) normal edges that must be `optional = true`.
OPTIONAL = {("gadk-engine", "gadk-gemini"), ("gadk-engine", "gadk-httprr")}
ENGINE_ONLY_DEPENDENT = "gadk"


def reaches(src, dst, seen=None):
    """True when `src` depends on `dst` through ALLOWED edges (transitively)."""
    seen = seen if seen is not None else set()
    for d in ALLOWED.get(src, ()):
        if d == dst:
            return True
        if d not in seen:
            seen.add(d)
            if reaches(d, dst, seen):
                return True
    return False


def check(repo):
    repo = pathlib.Path(repo)
    errs = []
    names = set()
    for manifest in sorted(repo.glob("crates/*/Cargo.toml")):
        rel = manifest.relative_to(repo)
        data = tomllib.loads(manifest.read_text())
        name = data["package"]["name"]
        names.add(name)
        for table in ("dependencies", "dev-dependencies", "build-dependencies"):
            for dep, spec in data.get(table, {}).items():
                if dep == name and isinstance(spec, dict) and spec.get("path") == ".":
                    continue  # self dev-dependency enabling test features
                if not (isinstance(spec, dict) and spec.get("workspace") is True and "version" not in spec):
                    errs.append(f"{rel}: [{table}] {dep} must be {{ workspace = true }}")
        for key in ("version", "edition", "rust-version", "license", "repository"):
            if data["package"].get(key) != {"workspace": True}:
                errs.append(f"{rel}: package.{key} must be inherited")
        if name not in ALLOWED:
            errs.append(f"{rel}: crate {name} is missing from the ALLOWED edge table")
            continue
        normal = data.get("dependencies", {})
        have = {d for d in normal if d.startswith("gadk")}
        for d in sorted(have - ALLOWED[name]):
            errs.append(f"{rel}: dependency edge {name} -> {d} is not allowed")
        for d in sorted(ALLOWED[name] - have):
            errs.append(f"{rel}: required dependency edge {name} -> {d} is missing")
        for crate, d in sorted(OPTIONAL):
            if crate == name and d in normal and not (isinstance(normal[d], dict) and normal[d].get("optional") is True):
                errs.append(f"{rel}: {name} -> {d} must be optional = true")
        for d in sorted(x for x in data.get("dev-dependencies", {}) if x.startswith("gadk")):
            if d == name:
                continue
            if d == "gadk-engine" and name != ENGINE_ONLY_DEPENDENT:
                errs.append(f"{rel}: only {ENGINE_ONLY_DEPENDENT} may depend on gadk-engine (dev-dependency)")
            elif reaches(d, name) or d == ENGINE_ONLY_DEPENDENT:
                errs.append(f"{rel}: dev-dependency {name} -> {d} creates a cycle")
    for missing in sorted(set(ALLOWED) - names):
        errs.append(f"ALLOWED names crate {missing}, which is not a workspace member under crates/")
    return errs


def main():
    repo = pathlib.Path(__file__).resolve().parent.parent
    errs = check(repo)
    for e in errs:
        print(e, file=sys.stderr)
    return 1 if errs else 0


if __name__ == "__main__":
    sys.exit(main())
