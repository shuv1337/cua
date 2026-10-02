#include "held_phase.hpp"
#include <cstdlib>

namespace { void check(bool value) { if (!value) std::abort(); } }
int main() {
    using namespace cua::hyprland;
    const auto now = HeldPhase::Clock::now();
    const SourceWitness original{"root-nonce", 4, 7, {2560.25, 10.5, 640, 480, 2560.25, 10.5}};
    HeldPhase held{123, "device-handle", original, now + std::chrono::seconds(5)};
    check(held.permits(123, "device-handle", original, now));
    check(!held.permits(456, "device-handle", original, now));
    check(!held.permits(123, "foreign-handle", original, now));
    check(!held.permits(123, "device-handle", original, held.deadline));
    auto changed = original;
    changed.root = "successor";
    check(!held.permits(123, "device-handle", changed, now));
    changed = original; ++changed.revision;
    check(!held.permits(123, "device-handle", changed, now));
    changed = original; ++changed.desktop_generation;
    check(!held.permits(123, "device-handle", changed, now));
    for (unsigned i = 0; i < 6; ++i) {
        changed = original; changed.geometry[i] += .25;
        check(!held.permits(123, "device-handle", changed, now));
    }
    held.retire(false);
    held.retire(true);
    check(!held.cleanup_confirmed);
    check(!held.permits(123, "device-handle", original, now));
    // Coordinate-free cleanup still recognizes its original owner after revoke.
    check(held.owns(123, "device-handle"));
    check(!held.owns(456, "device-handle"));
}
