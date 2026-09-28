module;
#include "value.hpp"
module sample:detail;

auto
detail_value() -> int // NOLINT(misc-use-internal-linkage): imported partition needs module linkage.
{
  return VALUE_OFFSET;
}
