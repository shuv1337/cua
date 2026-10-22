#include "hidden_launch_guard.hpp"
#include "hidden_launch_identity.hpp"

#include <src/desktop/view/Window.hpp>
#include <src/desktop/Workspace.hpp>
#include <src/event/EventBus.hpp>
#include <src/output/Monitor.hpp>
#include <src/state/WorkspaceState.hpp>
#include <src/helpers/MiscFunctions.hpp>

#include <cerrno>
#include <fcntl.h>
#include <format>
#include <optional>
#include <string>
#include <sys/stat.h>
#include <unistd.h>
#include <unordered_map>

namespace cua::hyprland {
namespace {
struct Fd {
    int value;
    ~Fd() { if (value >= 0) close(value); }
};
std::optional<std::string> read_proc(int directory, const char* name, std::size_t maximum) {
    Fd file{openat(directory, name, O_RDONLY | O_CLOEXEC | O_NOFOLLOW)};
    if (file.value < 0) return {};
    std::string result;
    char chunk[4096];
    for (;;) {
        const auto count = read(file.value, chunk, sizeof(chunk));
        if (count < 0 && errno == EINTR) continue;
        if (count < 0) return {};
        if (count == 0) return result;
        if (result.size() + count > maximum) return {};
        result.append(chunk, count);
    }
}
}

struct HiddenLaunchGuard::Impl {
    std::function<bool()> enabled;
    std::unordered_map<pid_t, unsigned long long> owners;
    CHyprSignalListener early, before_focus;

    bool owns(PHLWINDOW window) {
        if (!enabled()) return false;
        const auto pid = window->getPID();
        if (pid <= 0) return false;
        Fd directory{open(std::format("/proc/{}", pid).c_str(), O_RDONLY | O_DIRECTORY | O_CLOEXEC | O_NOFOLLOW)};
        struct stat metadata{};
        if (directory.value < 0 || fstat(directory.value, &metadata) || metadata.st_uid != geteuid()) return false;
        const auto stat = read_proc(directory.value, "stat", 8192);
        const auto start = stat ? launch_process_instance(*stat) : std::nullopt;
        if (!start) { owners.erase(pid); return false; }
        if (const auto owner = owners.find(pid); owner != owners.end() && owner->second == *start) return true;
        owners.erase(pid);
        const auto environment = read_proc(directory.value, "environ", 64 * 1024);
        if (!environment || !hidden_launch_token(*environment)) return false;
        const auto after = read_proc(directory.value, "stat", 8192);
        if (!after || launch_process_instance(*after) != start) return false;
        owners[pid] = *start;
        return true;
    }

    explicit Impl(std::function<bool()> active) : enabled(std::move(active)) {
        early = Event::bus()->m_events.window.openEarly.listen([this](PHLWINDOW window) {
            if (!owns(window)) return;
            auto workspace = State::workspaceState()->query().string("special:cua").run();
            if (!workspace) {
                const auto [id, name, automatic] = getWorkspaceIDNameFromString("special:cua");
                (void)automatic;
                if (id == WORKSPACE_INVALID || !window->m_monitor) return;
                workspace = State::workspaceState()->create(id, window->m_monitor->m_id, name);
            }
            if (!workspace) return;
            // Before layout registration: assign the actual target workspace,
            // not a move dispatcher which can refocus the primary seat.
            window->m_workspace = workspace;
            window->m_monitor = workspace->m_monitor;
            window->m_noInitialFocus = true;
        });
        // This event is after allows_input rules but before the map's automatic
        // focus decision. openLate (the Lua window.open event) is too late.
        before_focus = Event::bus()->m_events.window.open.listen([this](PHLWINDOW window) {
            if (owns(window)) window->m_noInitialFocus = true;
        });
    }
};

HiddenLaunchGuard::HiddenLaunchGuard(std::function<bool()> enabled) : impl_(std::make_unique<Impl>(std::move(enabled))) {}
HiddenLaunchGuard::~HiddenLaunchGuard() = default;
} // namespace cua::hyprland
