#!/usr/bin/env python3
# Copyright 2026 Chris Olson — Apache-2.0 (see LICENSE)
"""R1: workspace dependency hygiene and the allowed gadk-* dependency edges.

- Every member dependency entry is `{ workspace = true [, optional/features] }` (a self
  path dev-dependency `{ path = ".", features = [...] }` is allowed).
- Package metadata is inherited.
- Normal gadk-* dependencies of each crate equal ALLOWED[crate] exactly; OPTIONAL edges are
  `optional = true`.
- A gadk-* dev-dependency is the crate itself or a crate that does not (transitively, via
  ALLOWED) depend on it, and never `gadk`.
- Only `gadk`, `gadk-tools`, (5a) `gadk-server`, (5b) `gadk-a2a` and (6) `gadk-launcher` may depend on `gadk-engine`, and only `gadk` may depend on
  `gadk-tools`, normal or dev (2a: gadk-tools builds on the engine's public API; its tests use
  the engine's `testutil` feature).
- 2b: `gadk-auth` depends on exactly `gadk-agent` and `gadk-platform` (never `gadk-engine` or
  `gadk-tools`), and only the façade (and, from 2c, `gadk-mcp`) depends on it; `gadk-tools` stays
  free of `gadk-auth` because `toolauth` is a pure data model.
- 2c: `gadk-mcp` depends on exactly `gadk-agent`, `gadk-auth`, `gadk-genai` and `gadk-platform`
  (never `gadk-engine` or `gadk-tools`), and only the façade depends on it; `gadk-gemini`
  depends on `gadk-platform` for the shared tracking header.
- 3a: `gadk-anthropic` depends on exactly `gadk-genai`, `gadk-model` and `gadk-platform` (the
  shared SSE parser lives in `gadk-model`, the injectable environment in `gadk-platform`), and
  only the façade depends on it (feature `anthropic`).
- 3b: `gadk-openai` depends on exactly `gadk-genai`, `gadk-model` and `gadk-platform`, and only
  the façade depends on it (feature `openai`), normal or dev.
- 4a: `gadk-session-database` depends on exactly `gadk-session`, `gadk-model` and `gadk-platform`;
  only the façade depends on it (feature `database`), and `gadk-engine` may dev-depend on it (its
  compaction parity test).
- 4b: `gadk-artifact-gcs` depends on exactly `gadk-artifact`, `gadk-genai`, `gadk-model` and
  `gadk-platform`, and only the façade depends on it (feature `gcs`); `gadk-model`'s `gcp-auth`
  feature adds no gadk-* edge.
- 4c: `gadk-vertexai` depends on exactly `gadk-session`, `gadk-memory`, `gadk-genai`, `gadk-model`
  and `gadk-platform`, and only the façade depends on it (feature `vertexai`).
- 5a: `gadk-server` depends on exactly `gadk-agent`, `gadk-engine`, `gadk-session`, `gadk-memory`,
  `gadk-artifact`, `gadk-genai`, `gadk-model` and `gadk-platform`; it is the third crate that may
  depend on `gadk-engine`, and only the façade depends on it (feature `server`).
- 5b: `gadk-a2a` depends on exactly `gadk-agent`, `gadk-engine`, `gadk-session`, `gadk-genai`,
  `gadk-model`, `gadk-auth`, `gadk-platform` and, optionally (its `gadk-server` feature, for the
  authn `Caller`), `gadk-server`; it is the fourth crate that may depend on `gadk-engine`, the
  second (after `gadk-mcp`) besides the façade that depends on `gadk-auth`, and only the façade
  depends on it (feature `a2a`).
- 6: `gadk-launcher` depends on exactly `gadk-agent`, `gadk-engine`, `gadk-session`,
  `gadk-artifact`, `gadk-memory`, `gadk-genai`, `gadk-model` (Go JSON for the console's HITL
  prompts), `gadk-platform` and, optionally, `gadk-server`
  (feature `web`) and `gadk-a2a` (feature `a2a`); it is the fifth crate that may depend on
  `gadk-engine`, and only the façade depends on it (feature `launcher`). `gadk-genai` depends on
  `gadk-platform` for the Go duration port.
"""
import pathlib, sys, tomllib

ALLOWED = {
    "gadk-genai": {"gadk-platform"},  # 6: Go durations (gadk_platform::go_parse_duration)
    "gadk-platform": set(),
    "gadk-model": {"gadk-genai"},
    "gadk-gemini": {"gadk-genai", "gadk-model", "gadk-platform"},
    "gadk-session": {"gadk-genai", "gadk-model", "gadk-platform"},
    "gadk-httprr": {"gadk-model"},
    "gadk-artifact": {"gadk-genai", "gadk-platform"},
    "gadk-memory": {"gadk-genai", "gadk-session"},
    "gadk-agent": {"gadk-genai", "gadk-platform", "gadk-model", "gadk-session", "gadk-artifact", "gadk-memory"},
    "gadk-engine": {"gadk-agent", "gadk-genai", "gadk-platform", "gadk-model", "gadk-session",
                    "gadk-artifact", "gadk-memory", "gadk-gemini", "gadk-httprr"},
    "gadk-tools": {"gadk-agent", "gadk-engine", "gadk-genai", "gadk-model", "gadk-platform",
                   "gadk-session", "gadk-memory", "gadk-artifact"},
    "gadk-auth": {"gadk-agent", "gadk-platform"},
    "gadk-mcp": {"gadk-agent", "gadk-auth", "gadk-genai", "gadk-platform"},
    "gadk-anthropic": {"gadk-genai", "gadk-model", "gadk-platform"},
    "gadk-openai": {"gadk-genai", "gadk-model", "gadk-platform"},
    "gadk-session-database": {"gadk-session", "gadk-model", "gadk-platform"},
    "gadk-artifact-gcs": {"gadk-artifact", "gadk-genai", "gadk-model", "gadk-platform"},
    "gadk-vertexai": {"gadk-session", "gadk-memory", "gadk-genai", "gadk-model", "gadk-platform"},
    "gadk-server": {"gadk-agent", "gadk-engine", "gadk-session", "gadk-memory", "gadk-artifact",
                    "gadk-genai", "gadk-model", "gadk-platform"},
    "gadk-a2a": {"gadk-agent", "gadk-engine", "gadk-session", "gadk-genai", "gadk-model", "gadk-auth",
                 "gadk-platform", "gadk-server"},
    "gadk-launcher": {"gadk-agent", "gadk-engine", "gadk-session", "gadk-artifact", "gadk-memory",
                      "gadk-genai", "gadk-model", "gadk-platform", "gadk-server", "gadk-a2a"},
    "gadk": {"gadk-genai", "gadk-platform", "gadk-model", "gadk-session", "gadk-gemini",
             "gadk-agent", "gadk-engine", "gadk-tools", "gadk-auth", "gadk-mcp", "gadk-artifact",
             "gadk-memory", "gadk-anthropic", "gadk-openai", "gadk-session-database",
             "gadk-artifact-gcs", "gadk-vertexai", "gadk-server", "gadk-a2a", "gadk-launcher"},
}
# (crate, dependency) normal edges that must be `optional = true`.
OPTIONAL = {("gadk-engine", "gadk-gemini"), ("gadk-engine", "gadk-httprr"), ("gadk-a2a", "gadk-server"),
            ("gadk-launcher", "gadk-server"), ("gadk-launcher", "gadk-a2a")}
FACADE = "gadk"  # no crate may dev-depend on the façade
ENGINE_DEPENDENTS = ("gadk", "gadk-tools", "gadk-server", "gadk-a2a", "gadk-launcher")  # the only crates that may depend on gadk-engine
TOOLS_DEPENDENT = "gadk"  # the only crate that may depend on gadk-tools
OPENAI_DEPENDENT = "gadk"  # the only crate that may depend on gadk-openai (3b)


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
            if d == "gadk-engine" and name not in ENGINE_DEPENDENTS:
                errs.append(f"{rel}: only gadk, gadk-tools, gadk-server, gadk-a2a and gadk-launcher may depend on gadk-engine (dev-dependency)")
            elif d == "gadk-tools" and name != TOOLS_DEPENDENT:
                errs.append(f"{rel}: only {TOOLS_DEPENDENT} may depend on gadk-tools (dev-dependency)")
            elif d == "gadk-openai" and name != OPENAI_DEPENDENT:
                errs.append(f"{rel}: only {OPENAI_DEPENDENT} may depend on gadk-openai (dev-dependency)")
            elif reaches(d, name) or d == FACADE:
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
