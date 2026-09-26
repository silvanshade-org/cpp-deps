module;
#include "value.hpp"
export module sample;
export import :part;
export int answer() { return part_value() + VALUE_OFFSET; }
