#!/usr/bin/env bash
# Isolated Git history exercises the skip boundary and the fail-open paths.
set -euo pipefail
script="$(cd "$(dirname "$0")" && pwd)/changed-categories.sh"
fixture=$(mktemp -d)
trap 'rm -rf "$fixture"' EXIT
git -C "$fixture" init -q
git -C "$fixture" config user.name CI
git -C "$fixture" config user.email ci@example.invalid
git -C "$fixture" commit -q --allow-empty -m base
base=$(git -C "$fixture" rev-parse HEAD)
run_filter() (
  cd "$fixture"
  GITHUB_OUTPUT="" CI_EVENT_NAME=push CI_BEFORE_SHA="$1" bash "$script"
)
mkdir -p "$fixture/docs"
printf 'readme\n' > "$fixture/docs/readme.md"
git -C "$fixture" add docs/readme.md
git -C "$fixture" commit -q -m docs
[[ $(run_filter "$base") == rust=false ]]
docs=$(git -C "$fixture" rev-parse HEAD)
printf 'code\n' > "$fixture/source.rs"
git -C "$fixture" add source.rs
git -C "$fixture" commit -q -m code
[[ $(run_filter "$docs") == rust=true ]]
code=$(git -C "$fixture" rev-parse HEAD)
git -C "$fixture" mv source.rs docs/source.md
git -C "$fixture" commit -q -m rename
[[ $(run_filter "$code") == rust=true ]]
[[ $(run_filter 0000000000000000000000000000000000000000) == rust=true ]]
[[ $(run_filter deadbeefdeadbeefdeadbeefdeadbeefdeadbeef) == rust=true ]]
[[ $(cd "$fixture" && GITHUB_OUTPUT="" CI_EVENT_NAME=merge_group bash "$script") == rust=true ]]
printf 'path filter cases passed\n'
