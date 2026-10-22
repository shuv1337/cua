#pragma once
#include <charconv>
#include <optional>
#include <string_view>

namespace cua::hyprland {
inline std::optional<unsigned long long> launch_process_instance(std::string_view stat) {
    const auto end = stat.rfind(')');
    if (end == std::string_view::npos) return {};
    auto rest = stat.substr(end + 1);
    for (unsigned field = 3; field <= 22; ++field) {
        const auto first = rest.find_first_not_of(" \t\n");
        if (first == std::string_view::npos) return {};
        rest.remove_prefix(first);
        const auto count = rest.find_first_of(" \t\n");
        const auto value = rest.substr(0, count);
        if (field == 3 && (value == "Z" || value == "X")) return {};
        if (field == 22) {
            unsigned long long start = 0;
            const auto parsed = std::from_chars(value.data(), value.data() + value.size(), start);
            if (parsed.ec != std::errc{} || parsed.ptr != value.data() + value.size()) return {};
            return start;
        }
        if (count == std::string_view::npos) return {};
        rest.remove_prefix(count);
    }
    return {};
}

inline bool hidden_launch_token(std::string_view environment) {
    constexpr std::string_view prefix = "CUA_LAUNCH_TOKEN=";
    while (!environment.empty()) {
        const auto end = environment.find('\0');
        const auto entry = environment.substr(0, end);
        if (entry.starts_with(prefix)) {
            const auto token = entry.substr(prefix.size());
            if (token.size() != 32) return false;
            for (char c : token) if (!((c >= '0' && c <= '9') || (c >= 'a' && c <= 'f'))) return false;
            return true;
        }
        if (end == std::string_view::npos) break;
        environment.remove_prefix(end + 1);
    }
    return false;
}
} // namespace cua::hyprland
