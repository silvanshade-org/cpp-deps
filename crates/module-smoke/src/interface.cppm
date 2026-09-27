export module sample;
export import :part;
import :detail;
export int answer() { return part_value() + detail_value(); }
