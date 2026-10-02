#pragma once

#include <cstdint>
#include <cstring>
#include <wayland-server-core.h>
#include <wayland-server-protocol.h>

namespace cua::hyprland {

// The logger runs BEFORE closure_send. A later event on this same resource
// proves the previous send returned successfully: libwayland handle_array
// refuses all subsequent events after marshal/send failure. Two frames prove
// the owned button event AND the first frame were queued. This is server queue
// evidence, never client receipt, toolkit processing or application effect.
// No allocation, input, payload storage or callback recursion occurs here.
struct HeldPointerJournal {
    std::uint32_t button = 0;
    bool press_seen = false, press_queued = false, press_frame_queued = false;
    bool release_seen = false, release_queued = false, release_frame_queued = false;
    bool left = false, invalid = false;
    enum class Pending { none, press, release, press_frame, release_frame };
    Pending pending = Pending::none;

    void observe(std::uint32_t opcode, std::uint32_t code = 0, std::uint32_t state = 0) noexcept {
        // Only an exact owned button, frame or leave event is a sentinel.
        if (opcode == WL_POINTER_BUTTON && code != button) return;
        if (opcode != WL_POINTER_BUTTON && opcode != WL_POINTER_FRAME && opcode != WL_POINTER_LEAVE) return;
        switch (pending) {
            case Pending::press: press_queued = true; break;
            case Pending::release: release_queued = true; break;
            case Pending::press_frame: press_frame_queued = true; break;
            case Pending::release_frame: release_frame_queued = true; break;
            case Pending::none: break;
        }
        pending = Pending::none;
        if (opcode == WL_POINTER_BUTTON) {
            if (state == WL_POINTER_BUTTON_STATE_PRESSED) {
                if (press_seen || left) invalid = true;
                press_seen = true;
                pending = Pending::press;
            } else if (state == WL_POINTER_BUTTON_STATE_RELEASED) {
                if (!press_seen) invalid = true;
                release_seen = true;
                pending = Pending::release;
            } else invalid = true;
        } else if (opcode == WL_POINTER_FRAME) {
            if (release_seen) pending = Pending::release_frame;
            else if (press_seen) pending = Pending::press_frame;
        } else left = true;
    }
    bool began() const noexcept { return !invalid && press_seen && press_queued && press_frame_queued && !release_seen && !left; }
    bool cleaned() const noexcept { return !invalid && (!press_seen || (release_seen && release_queued && release_frame_queued)); }
};

// Resource/owner/incarnation equality is checked by the caller BEFORE this
// parser; only these numeric fields are read. No titles, keys or arbitrary
// protocol arguments are recorded.
inline void observe_held_pointer_event(HeldPointerJournal& journal, wl_protocol_logger_type direction,
                                       const wl_protocol_logger_message* event) noexcept {
    if (direction != WL_PROTOCOL_LOGGER_EVENT || !event || !event->message || !event->message->name) return;
    if (event->message_opcode == WL_POINTER_BUTTON && std::strcmp(event->message->name, "button") == 0 &&
        event->arguments_count == 4 && event->arguments) {
        journal.observe(WL_POINTER_BUTTON, event->arguments[2].u, event->arguments[3].u);
    } else if (event->message_opcode == WL_POINTER_FRAME && std::strcmp(event->message->name, "frame") == 0 && event->arguments_count == 0) {
        journal.observe(WL_POINTER_FRAME);
    } else if (event->message_opcode == WL_POINTER_LEAVE && std::strcmp(event->message->name, "leave") == 0 && event->arguments_count == 2) {
        journal.observe(WL_POINTER_LEAVE);
    }
}

} // namespace cua::hyprland
