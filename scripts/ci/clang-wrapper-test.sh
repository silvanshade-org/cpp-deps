#!/bin/sh
# Clang's scanner must see a real compiler through a path-based wrapper;
# a shim conceals the resource directory containing builtin headers.
set -eu
wrapper="$RUNNER_TEMP/wrapped-cxx/clang++"
mkdir -p "$(dirname "$wrapper")"
printf '#!/bin/sh\nexec "%s" "$@"\n' "$(mise which clang++)" > "$wrapper"
chmod +x "$wrapper"
CXX="ccache $wrapper" cargo nextest run --profile default -p module-smoke
