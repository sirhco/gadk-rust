#!/usr/bin/env bash
# Copyright 2026 Chris Olson — Apache-2.0 (see LICENSE)
# Regenerates the gadk-agentanalytics fixtures with ADK-Go's plugin/agentanalytics (7b-1, 7b-2).
#
# Usage: scripts/gen-go-agentanalytics/run.sh [OUT_DIR]
#   OUT_DIR defaults to crates/gadk-agentanalytics/tests/fixtures; it is replaced as a whole
#   once the probe succeeds. ADK-Go is $ADK_GO_DIR, else adk-go beside the main checkout (the
#   parent of git's common dir, so a git worktree finds it too), else ../adk-go.
#   The probe runs under Go $GADK_PROBE_GOTOOLCHAIN (default go1.26.10), downloaded by the go
#   command on first use (GOTOOLCHAIN); module downloads use the normal GOPROXY.
#
# The probe is an overlay test: ADK-Go's plugin/agentanalytics is copied into a temp dir, its
# go.mod/go.sum are replaced by module.go.mod/module.go.sum (ADK-Go's module file plus
# `replace google.golang.org/adk/v2 => <ADK-Go checkout>`, tidied under go1.26.10), and
# overlay/zz_probe_test.go.txt and overlay/zz_probe_7b2_test.go.txt are added as zz_probe_test.go
# and zz_probe_7b2_test.go. ADK-Go is never modified.
set -euo pipefail
here="$(cd "$(dirname "$0")" && pwd)"
repo="$(cd "$here/../.." && pwd)"
if [ -n "${ADK_GO_DIR:-}" ]; then
  go_dir="$ADK_GO_DIR"
elif common="$(git -C "$repo" rev-parse --path-format=absolute --git-common-dir 2>/dev/null)" &&
  [ -d "$(dirname "$(dirname "$common")")/adk-go" ]; then
  go_dir="$(dirname "$(dirname "$common")")/adk-go"
else
  go_dir="$repo/../adk-go"
fi
out="${1:-$repo/crates/gadk-agentanalytics/tests/fixtures}"
toolchain="${GADK_PROBE_GOTOOLCHAIN:-go1.26.10}"
if [ ! -d "$go_dir/plugin/agentanalytics" ]; then
  echo "ADK-Go not found at $go_dir (set ADK_GO_DIR)" >&2
  exit 1
fi
go_abs="$(cd "$go_dir" && pwd)"
tmp="$(mktemp -d)"
trap 'rm -rf "$tmp"' EXIT
pkg="$tmp/agentanalytics"
mkdir -p "$pkg"
cp "$go_abs"/plugin/agentanalytics/*.go "$pkg/"
sed "s#=> ../../../adk-go#=> $go_abs#" "$here/module.go.mod" >"$pkg/go.mod"
cp "$here/module.go.sum" "$pkg/go.sum"
cp "$here/overlay/zz_probe_test.go.txt" "$pkg/zz_probe_test.go"
cp "$here/overlay/zz_probe_7b2_test.go.txt" "$pkg/zz_probe_7b2_test.go"
mkdir -p "$tmp/out"
(cd "$pkg" && TZ=UTC GOTOOLCHAIN="$toolchain" GOFLAGS=-mod=readonly AGENTANALYTICS_PROBE_OUT="$tmp/out" \
  go test -v -count=1 -run '^TestGenerateFixtures$' .) >"$tmp/log" 2>&1 || {
  cat "$tmp/log" >&2
  echo "gen-go-agentanalytics: probe failed" >&2
  exit 1
}
grep '^wrote ' "$tmp/log" || true
rm -rf "$out"
mkdir -p "$(dirname "$out")"
cp -R "$tmp/out" "$out"
