#include "hidden_launch_identity.hpp"
#include <cassert>
#include <string>
using namespace cua::hyprland;
int main() {
    const std::string prefix = "42 (a process ) with spaces) ";
    std::string fields = "S";
    for (unsigned field = 4; field < 22; ++field) fields += " 0";
    assert(launch_process_instance(prefix + fields + " 12345 0") == 12345);
    assert(!launch_process_instance(prefix + "Z" + fields.substr(1) + " 12345"));
    assert(!launch_process_instance(prefix + "X" + fields.substr(1) + " 12345"));
    assert(!launch_process_instance(prefix + fields + " 12345x"));
    assert(!launch_process_instance("42 (short) S 1"));
    const std::string valid = "CUA_LAUNCH_TOKEN=0123456789abcdef0123456789abcdef";
    assert(hidden_launch_token(valid));
    assert(hidden_launch_token(std::string("OTHER=1\0", 8) + valid + '\0'));
    for (const auto& invalid : {"NOT_" + valid, "X=" + valid, valid + "0", valid.substr(0, valid.size() - 1),
                               std::string("CUA_LAUNCH_TOKEN=0123456789abcdef0123456789abcdeF")})
        assert(!hidden_launch_token(invalid));
}
