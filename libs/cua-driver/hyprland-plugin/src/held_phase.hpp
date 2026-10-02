#pragma once

#include <array>
#include <chrono>
#include <cstdint>
#include <string>

namespace cua::hyprland {
struct SourceWitness {
    std::string root;
    std::uint64_t revision = 0, desktop_generation = 0;
    std::array<double, 6> geometry{};
    bool operator==(const SourceWitness&) const = default;
};

// A handle is scoped to the original accepted connection, not a public label.
// The adapter owns the original resource set and cleanup; this state owns the
// immutable witness, lease deadline, and irreversible admission retirement.
struct HeldPhase {
    using Clock = std::chrono::steady_clock;
    std::uintptr_t owner;
    std::string handle;
    SourceWitness source;
    Clock::time_point deadline;
    bool retired = false, cleanup_confirmed = false;
    std::string reason{};

    bool owns(std::uintptr_t actor, const std::string& id) const {
        return owner == actor && handle == id;
    }
    bool permits(std::uintptr_t actor, const std::string& id,
                 const SourceWitness& live, Clock::time_point now) const {
        return owns(actor, id) && !retired && now < deadline && live == source;
    }
    void retire(bool confirmed) {
        if (retired) return; // duplicate abort cannot manufacture an ACK
        retired = true;
        cleanup_confirmed = confirmed;
    }
};
} // namespace cua::hyprland
