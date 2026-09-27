#!/usr/bin/env bash
# Filter code/toolchain changes; unknown bases and failed diffs run every gate.
set -euo pipefail
emit() {
  if [[ -n ${GITHUB_OUTPUT:-} ]]; then
    printf 'rust=%s\n' "$1" >> "$GITHUB_OUTPUT"
  else
    printf 'rust=%s\n' "$1"
  fi
}
if [[ ${CI_EVENT_NAME:-} == merge_group ]]; then
  emit true
  exit 0
fi
if [[ ${CI_EVENT_NAME:-} == pull_request ]]; then
  base=${CI_BASE_SHA:-}
else
  base=${CI_BEFORE_SHA:-}
fi
if [[ -z $base || $base == 0000000000000000000000000000000000000000 ]]; then
  emit true
  exit 0
fi
if ! git cat-file -e "$base^{commit}" 2>/dev/null && ! git fetch --depth=1 origin "$base" 2>/dev/null; then
  emit true
  exit 0
fi
changes=$(mktemp)
trap 'rm -f "$changes"' EXIT
# Renames list both deletion and addition: moving code into docs cannot skip tests.
if ! git diff --no-renames --name-only -z "$base" HEAD > "$changes"; then
  emit true
  exit 0
fi
rust=false
while IFS= read -r -d '' path; do
  case "$path" in
    docs/*|*.md|LICENSE*|.gitignore) ;;
    *) rust=true; break ;;
  esac
done < "$changes"
emit "$rust"
