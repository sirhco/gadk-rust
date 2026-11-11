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

if [ "$fail" -eq 0 ]; then
  echo "ok: $n fixtures and $schema_n telemetry goldens match ADK-Go; $wire_n wire fixtures;" \
    "$a2a_wire_n A2A wire fixtures"
fi
exit "$fail"
