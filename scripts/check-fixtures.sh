#!/usr/bin/env bash
# Copyright 2026 Chris Olson — Apache-2.0 (see LICENSE)
# R20: every httprr fixture copied under crates/gadk-engine/tests/testdata/ is byte-identical to
# its ADK-Go original (same relative path under $ADK_GO_DIR), and there are exactly 30 of them.
set -euo pipefail
repo="$(cd "$(dirname "$0")/.." && pwd)"
go="${ADK_GO_DIR:-$repo/../adk-go}"
base="$repo/crates/gadk-engine/tests/testdata"
expected=30
n=0
fail=0
while IFS= read -r -d '' f; do
  rel="${f#"$base"/}"
  if ! cmp -s "$f" "$go/$rel"; then
    echo "fixture differs from ADK-Go (or is missing there): $rel" >&2
    fail=1
  fi
  n=$((n + 1))
done < <(find "$base" -type f -name '*.httprr' -print0)
if [ "$n" -ne "$expected" ]; then
  echo "expected $expected fixtures under $base, found $n" >&2
  fail=1
fi
if [ "$fail" -eq 0 ]; then
  echo "ok: $n fixtures match ADK-Go"
fi
exit "$fail"
