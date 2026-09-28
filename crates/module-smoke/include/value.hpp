// A standard header pulls in the compiler's builtin headers (`stddef.h`),
// which the Clang scanner finds only through the compiler's resource directory.
#include <cstddef>
#define VALUE_OFFSET static_cast<int>(std::size_t{ 25 })
