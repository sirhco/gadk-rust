#!/usr/bin/env bash
# Copyright 2026 Chris Olson — Apache-2.0 (see LICENSE)
# Regenerates the 8b Go goldens with ADK-Go itself (spec Design E, R10):
#   crates/gadk-vertexai/testdata/example_store/  (tool/exampletool/vertexai, TestProvider_Examples)
#   crates/gadk-cli/testdata/environment/         (cmd/adkgo/internal/deploy/agentengine,
#                                                  TestEnvironmentDeploymentRequests)
#
# Usage: scripts/gen-go-8b/run.sh [OUT_DIR]
#   Without OUT_DIR both golden directories above are replaced as a whole once the probe succeeds.
#   With OUT_DIR the probe writes OUT_DIR/example_store and OUT_DIR/environment instead.
#   ADK-Go is $ADK_GO_DIR, else adk-go beside the main checkout (the parent of git's common dir,
#   so a git worktree finds it too), else ../adk-go. The probe runs under Go
#   $GADK_PROBE_GOTOOLCHAIN (default go1.26.10), downloaded by the go command on first use
#   (GOTOOLCHAIN); module downloads use the normal GOPROXY.
#
# The probe is two overlay tests: overlay/vertexai_zz_gen8b_test.go.txt and
# overlay/agentengine_zz_gen8b_test.go.txt are mapped into ADK-Go's packages as zz_gen8b_test.go
# with `go test -overlay` (ADK-Go's own go.mod/go.sum, -mod=readonly), so ADK-Go's tree is never
# written.
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
toolchain="${GADK_PROBE_GOTOOLCHAIN:-go1.26.10}"
vertexai_pkg="tool/exampletool/vertexai"
agentengine_pkg="cmd/adkgo/internal/deploy/agentengine"
if [ ! -d "$go_dir/$vertexai_pkg" ] || [ ! -f "$go_dir/$agentengine_pkg/environment.go" ]; then
  echo "ADK-Go (with $vertexai_pkg and $agentengine_pkg/environment.go) not found at $go_dir (set ADK_GO_DIR)" >&2
  exit 1
fi
go_abs="$(cd "$go_dir" && pwd)"
tmp="$(mktemp -d)"
trap 'rm -rf "$tmp"' EXIT
cat >"$tmp/overlay.json" <<EOF
{"Replace":{
"$go_abs/$vertexai_pkg/zz_gen8b_test.go":"$here/overlay/vertexai_zz_gen8b_test.go.txt",
"$go_abs/$agentengine_pkg/zz_gen8b_test.go":"$here/overlay/agentengine_zz_gen8b_test.go.txt"
}}
EOF
mkdir -p "$tmp/out"
(cd "$go_abs" && TZ=UTC GOTOOLCHAIN="$toolchain" GOFLAGS=-mod=readonly GEN_GO_8B_OUT="$tmp/out" \
  go test -count=1 -overlay "$tmp/overlay.json" -run '^TestZZGen8b(ExampleStore|Environment)$' \
  "./$vertexai_pkg/" "./$agentengine_pkg/") >"$tmp/log" 2>&1 || {
  cat "$tmp/log" >&2
  echo "gen-go-8b: probe failed" >&2
  exit 1
}
store_n="$(find "$tmp/out/example_store" -type f -name '*.json' | wc -l | tr -d ' ')"
env_n="$(find "$tmp/out/environment" -type f -name '*.json' | wc -l | tr -d ' ')"
echo "wrote $store_n example_store and $env_n environment goldens"
if [ -n "${1:-}" ]; then
  rm -rf "$1/example_store" "$1/environment"
  mkdir -p "$1"
  cp -R "$tmp/out/example_store" "$tmp/out/environment" "$1/"
else
  rm -rf "$repo/crates/gadk-vertexai/testdata/example_store" "$repo/crates/gadk-cli/testdata/environment"
  mkdir -p "$repo/crates/gadk-vertexai/testdata" "$repo/crates/gadk-cli/testdata"
  cp -R "$tmp/out/example_store" "$repo/crates/gadk-vertexai/testdata/"
  cp -R "$tmp/out/environment" "$repo/crates/gadk-cli/testdata/"
fi
