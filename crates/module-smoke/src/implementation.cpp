module sample;
import :detail;

auto
answer() -> int
{
  return part_value() + detail_value();
}

extern "C" auto
implementation_value() -> int
{
  return answer();
}
