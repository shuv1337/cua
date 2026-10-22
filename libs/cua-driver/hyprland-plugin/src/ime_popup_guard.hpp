#pragma once

namespace cua::hyprland {
bool install_ime_popup_guard(void* plugin);
void remove_ime_popup_guard();
bool ime_popup_guard_active();
} // namespace cua::hyprland
