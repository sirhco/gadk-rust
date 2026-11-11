#!/usr/bin/env bash
# Copyright 2026 Chris Olson — Apache-2.0 (see LICENSE)
# R20 (1b) / R17 (2c) / R18 (3a): every httprr fixture copied under
# crates/gadk-engine/tests/testdata/, crates/gadk/tests/testdata/,
# crates/gadk-anthropic/tests/testdata/ or crates/gadk-a2a/tests/testdata/ (5b) is byte-identical
# to its ADK-Go original (same relative path under $ADK_GO_DIR), and there are exactly 48 of them
# (30 + 11 + 3 + 4).
# 5a: the telemetry-schema goldens copied from ADK-Go's
# internal/telemetry/functionaltest/testdata/ are byte-identical to it (80), and the wire probe
# (scripts/gen-go-adkrest) wrote exactly 82 fixtures under crates/gadk-server/testdata/wire/
# (main 67, strict 11, custom 4). crates/gadk-server/testdata/oidc/ holds test-only keys, not
# fixtures, and is not counted.
# 5b: the A2A wire probe (scripts/gen-go-a2a) wrote exactly 7 fixtures under
# crates/gadk-a2a/testdata/wire/; the replay's declared deltas (*.deltas.json) are not counted.
# 6: the vendored web UI bundle crates/gadk-launcher/webui/distr/ holds exactly 106 files, they
# match crates/gadk-launcher/webui/SHA256SUMS (106 lines), and, when ADK-Go is present, they are
# byte-identical to its cmd/launcher/web/webui/distr/ (scripts/sync-webui regenerates all three);
# the launcher probe (scripts/gen-go-launcher) wrote exactly 70 web, 15 console and 34 CLI fixtures
# under crates/gadk-launcher/testdata/{wire,console,cli}/; the replay's declared deltas
# (*.deltas.json) are not counted.
# 5c: the Agent Engine probe (scripts/gen-go-agentengine) wrote exactly 29 server fixtures under
# crates/gadk-server/testdata/agentengine/ and 13 launcher fixtures under
# crates/gadk-launcher/testdata/agentengine/ (declared deltas, *.deltas.json, not counted); when
# go and ADK-Go are present, its --shapes generator reproduces
# crates/gadk-server/src/agentengine/encode_shapes.rs byte for byte.
set -euo pipefail
repo="$(cd "$(dirname "$0")/.." && pwd)"
go="${ADK_GO_DIR:-$repo/../adk-go}"
bases=("$repo/crates/gadk-engine/tests/testdata" "$repo/crates/gadk/tests/testdata"
  "$repo/crates/gadk-anthropic/tests/testdata" "$repo/crates/gadk-a2a/tests/testdata")
expected=48
n=0
fail=0
for base in "${bases[@]}"; do
  while IFS= read -r -d '' f; do
    rel="${f#"$base"/}"
    if ! cmp -s "$f" "$go/$rel"; then
      echo "fixture differs from ADK-Go (or is missing there): $rel" >&2
      fail=1
    fi
    n=$((n + 1))
  done < <(find "$base" -type f -name '*.httprr' -print0)
done
if [ "$n" -ne "$expected" ]; then
  echo "expected $expected fixtures under ${bases[*]}, found $n" >&2
  fail=1
fi

schema="$repo/crates/gadk-engine/tests/internal_telemetry_functionaltest_telemetry_schema/testdata"
schema_go="$go/internal/telemetry/functionaltest/testdata"
schema_expected=80
schema_n=0
while IFS= read -r -d '' f; do
  rel="${f#"$schema"/}"
  if ! cmp -s "$f" "$schema_go/$rel"; then
    echo "telemetry golden differs from ADK-Go (or is missing there): $rel" >&2
    fail=1
  fi
  schema_n=$((schema_n + 1))
done < <(find "$schema" -type f -print0)
if [ "$schema_n" -ne "$schema_expected" ]; then
  echo "expected $schema_expected telemetry goldens under $schema, found $schema_n" >&2
  fail=1
fi

wire="$repo/crates/gadk-server/testdata/wire"
wire_expected=82
wire_n="$(find "$wire" -type f -name '*.json' | wc -l | tr -d ' ')"
if [ "$wire_n" -ne "$wire_expected" ]; then
  echo "expected $wire_expected wire fixtures under $wire, found $wire_n" >&2
  fail=1
fi

a2a_wire="$repo/crates/gadk-a2a/testdata/wire"
a2a_wire_expected=7
a2a_wire_n="$(find "$a2a_wire" -type f -name '*.json' ! -name '*.deltas.json' | wc -l | tr -d ' ')"
if [ "$a2a_wire_n" -ne "$a2a_wire_expected" ]; then
  echo "expected $a2a_wire_expected A2A wire fixtures under $a2a_wire, found $a2a_wire_n" >&2
  fail=1
fi

webui="$repo/crates/gadk-launcher/webui"
webui_go="$go/cmd/launcher/web/webui/distr"
webui_expected=106
webui_n="$(find "$webui/distr" -type f | wc -l | tr -d ' ')"
if [ "$webui_n" -ne "$webui_expected" ]; then
  echo "expected $webui_expected web UI files under $webui/distr, found $webui_n" >&2
  fail=1
fi
webui_sums_n="$(wc -l <"$webui/SHA256SUMS" | tr -d ' ')"
if [ "$webui_sums_n" -ne "$webui_expected" ]; then
  echo "expected $webui_expected lines in $webui/SHA256SUMS, found $webui_sums_n" >&2
  fail=1
fi
if command -v sha256sum >/dev/null 2>&1; then
  sha256_check=(sha256sum -c)
else
  sha256_check=(shasum -a 256 -c)
fi
if ! webui_sums_out="$(cd "$webui/distr" && "${sha256_check[@]}" ../SHA256SUMS 2>&1)"; then
  printf '%s\n' "$webui_sums_out" | grep -v ': OK$' >&2 || true
  echo "web UI bundle does not match $webui/SHA256SUMS" >&2
  fail=1
fi
if [ -d "$webui_go" ] && ! diff -rq "$webui/distr" "$webui_go" >&2; then
  echo "web UI bundle differs from ADK-Go's $webui_go" >&2
  fail=1
fi

launcher_counts=""
for spec in wire:70 console:15 cli:34; do
  dir="$repo/crates/gadk-launcher/testdata/${spec%%:*}"
  want="${spec#*:}"
  got="$(find "$dir" -type f -name '*.json' ! -name '*.deltas.json' | wc -l | tr -d ' ')"
  if [ "$got" -ne "$want" ]; then
    echo "expected $want launcher fixtures under $dir, found $got" >&2
    fail=1
  fi
  launcher_counts="$launcher_counts ${spec%%:*}=$got"
done

agentengine_counts=""
for spec in gadk-server:29 gadk-launcher:13; do
  dir="$repo/crates/${spec%%:*}/testdata/agentengine"
  want="${spec#*:}"
  got="$(find "$dir" -type f -name '*.json' ! -name '*.deltas.json' | wc -l | tr -d ' ')"
  if [ "$got" -ne "$want" ]; then
    echo "expected $want agentengine fixtures under $dir, found $got" >&2
    fail=1
  fi
  agentengine_counts="$agentengine_counts ${spec%%:*}=$got"
done

shapes="$repo/crates/gadk-server/src/agentengine/encode_shapes.rs"
shapes_status="not checked"
if ! command -v go >/dev/null 2>&1 || [ ! -d "$go/server/agentengine" ]; then
  echo "skipping the encode_shapes.rs check: needs go and ADK-Go at $go (set ADK_GO_DIR)" >&2
else
  go_abs="$(cd "$go" && pwd)"
  shapes_tmp="$(mktemp -d)"
  sed "s#=> ../../../adk-go#=> $go_abs#" "$repo/scripts/gen-go-agentengine/go.mod" >"$shapes_tmp/go.mod"
  cp "$repo/scripts/gen-go-agentengine/go.sum" "$shapes_tmp/go.sum"
  if ! (cd "$repo/scripts/gen-go-agentengine" && ADK_GO_DIR="$go_abs" \
    go run -modfile="$shapes_tmp/go.mod" . --shapes --out "$shapes_tmp/encode_shapes.rs" >/dev/null); then
    echo "scripts/gen-go-agentengine --shapes failed" >&2
    fail=1
  elif ! cmp -s "$shapes_tmp/encode_shapes.rs" "$shapes"; then
    diff -u "$shapes" "$shapes_tmp/encode_shapes.rs" | head -40 >&2 || true
    echo "$shapes differs from scripts/gen-go-agentengine --shapes output" >&2
    fail=1
  else
    shapes_status="matches the generator"
  fi
  rm -rf "$shapes_tmp"
fi

if [ "$fail" -eq 0 ]; then
  echo "ok: $n fixtures and $schema_n telemetry goldens match ADK-Go; $wire_n wire fixtures;" \
    "$a2a_wire_n A2A wire fixtures; $webui_n web UI files; launcher fixtures$launcher_counts;" \
    "agentengine fixtures$agentengine_counts; encode_shapes.rs $shapes_status"
fi
exit "$fail"
