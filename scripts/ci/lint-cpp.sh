#!/usr/bin/env bash
# Compile the module graph first, then lint each TU against that same graph.
set -euo pipefail

tidy_version=$(mise exec -- clang-tidy --version | sed -n 's/^  LLVM version //p')
compiler_version=$(mise exec -- clang++ --version | sed -n '1s/^clang version \([0-9.]*\).*/\1/p')
if [[ -z $tidy_version || $tidy_version != "$compiler_version" ]]; then
  printf 'clang-tidy (%s) and clang++ (%s) must match\n' "$tidy_version" "$compiler_version" >&2
  exit 1
fi

mise exec -- cargo build --locked -p module-smoke
out=''
for candidate in "${CARGO_TARGET_DIR:-target}"/debug/build/module-smoke-*/out; do
  [[ -f $candidate/libmodule_smoke.a ]] || continue
  if [[ -z $out || $candidate/libmodule_smoke.a -nt $out/libmodule_smoke.a ]]; then
    out=$candidate
  fi
done
if [[ -z $out ]]; then
  printf 'module-smoke produced no Clang module archive\n' >&2
  exit 1
fi
module_file() {
  local object name path
  object=$(jq -r --arg logical "$1" '
    .rules[] | select(any(.provides[]?; ."logical-name" == $logical)) | ."primary-output"
  ' "$out"/unit-*.p1689.json)
  name=${object##*/}
  path=$out/module-${name#unit-}
  path=${path%.o}.pcm
  [[ -f $path ]] || {
    printf 'module-smoke produced no BMI for %s\n' "$1" >&2
    return 1
  }
  printf '%s\n' "$path"
}
sample=$(module_file sample)
part=$(module_file sample:part)
detail=$(module_file sample:detail)
resource_dir=$(mise exec -- clang -print-resource-dir)
for source in part.cppm detail.cppm interface.cppm implementation.cpp consumer.cpp; do
  # conda-forge's clang-tidy and clang++ share LLVM 22.1.8 but record different
  # branch labels in PCMs. Cargo built these from this checkout immediately above.
  mise exec -- clang-tidy --quiet --warnings-as-errors='*' "crates/module-smoke/src/$source" -- \
    -std=c++20 -fmodules -Icrates/module-smoke/include "-resource-dir=$resource_dir" \
    -Xclang -fno-validate-pch \
    "-fmodule-file=sample=$sample" \
    "-fmodule-file=sample:part=$part" \
    "-fmodule-file=sample:detail=$detail"
done
