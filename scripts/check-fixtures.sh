#!/usr/bin/env bash
# Copyright 2026 Chris Olson — Apache-2.0 (see LICENSE)
# R20 (1b) / R17 (2c) / R18 (3a): every httprr fixture copied under
# crates/gadk-engine/tests/testdata/, crates/gadk/tests/testdata/ or
# crates/gadk-anthropic/tests/testdata/ is byte-identical to its ADK-Go original (same relative
# path under $ADK_GO_DIR), and there are exactly 44 of them (30 + 11 + 3).
# 5a: the telemetry-schema goldens copied from ADK-Go's
# internal/telemetry/functionaltest/testdata/ are byte-identical to it (80), and the wire probe
# (scripts/gen-go-adkrest) wrote exactly 82 fixtures under crates/gadk-server/testdata/wire/
# (main 67, strict 11, custom 4). crates/gadk-server/testdata/oidc/ holds test-only keys, not
# fixtures, and is not counted.
set -euo pipefail
repo="$(cd "$(dirname "$0")/.." && pwd)"
go="${ADK_GO_DIR:-$repo/../adk-go}"
bases=("$repo/crates/gadk-engine/tests/testdata" "$repo/crates/gadk/tests/testdata"
  "$repo/crates/gadk-anthropic/tests/testdata")
expected=44
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

if [ "$fail" -eq 0 ]; then
  echo "ok: $n fixtures and $schema_n telemetry goldens match ADK-Go; $wire_n wire fixtures"
fi
exit "$fail"
