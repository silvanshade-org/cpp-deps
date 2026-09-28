#!/bin/sh
# Keep the workflow's mise release aligned with the verified image installer.
set -eu
root=$(CDPATH='' cd -- "$(dirname -- "$0")/../.." && pwd)
cd "$root"

ci=.github/workflows/ci.yml
[ -f "$ci" ] || ci=.github/workflows/ci.yaml
image=.github/docker/ci.Dockerfile

# Both setup anchors, when present, must resolve to one release.
workflow_version=$(sed -n '/uses: jdx\/mise-action@/{n; n; s/^[[:space:]]*version: "\([^"]*\)".*/\1/p;}' "$ci" | sort -u)
# Images with a literal URL pin and those extracting the workflow pin share
# this assertion. A changed version without a corresponding digest fails.
image_version=$(sed -n -e 's/.*mise-v\(20[0-9.]*\)-linux-.*/\1/p' -e 's/.*\[ "[$]MISE_VERSION" = \(20[0-9.]*\) \].*/\1/p' "$image" | sort -u)
minimum=$(sed -n 's/^min_version = "\([^"]*\)"$/\1/p' mise.toml)
if [ -z "$workflow_version" ] || [ -z "$image_version" ] || [ -z "$minimum" ] || [ "$workflow_version" != "$image_version" ]; then
  echo "check-pins: mise drift or missing pin (workflow=$workflow_version, image=$image_version, minimum=$minimum)" >&2
  exit 1
fi
if [ "$(printf '%s\n%s\n' "$minimum" "$workflow_version" | sort -V | head -n 1)" != "$minimum" ]; then
  echo "check-pins: workflow mise $workflow_version is older than floor $minimum" >&2
  exit 1
fi
echo "check-pins: OK: workflow and image use mise $workflow_version (floor $minimum)"
