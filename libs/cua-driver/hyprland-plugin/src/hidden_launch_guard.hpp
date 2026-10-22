#pragma once
#include <functional>
#include <memory>
namespace cua::hyprland {
// Exact same-user token/process ownership at Hyprland's synchronous map seam.
// Does not dispatch input, activate a workspace or change lasting focus rules.
class HiddenLaunchGuard {
  public:
    explicit HiddenLaunchGuard(std::function<bool()> enabled);
    ~HiddenLaunchGuard();
  private:
    struct Impl;
    std::unique_ptr<Impl> impl_;
};
} // namespace cua::hyprland
