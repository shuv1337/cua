//! Hyprland window discovery and capture helpers.
//!
//! The primary Linux backend is X11/XWayland. On Hyprland, native Wayland
//! clients never appear in _NET_CLIENT_LIST, but hyprctl clients -j exposes
//! enough read-only metadata for list_windows, and per-window screenshots
//! go through the hyprland-toplevel-export-v1 protocol
//! (`crate::wayland_capture`) — which copies the toplevel's own buffer, so
//! occluded/background windows capture their real content. grim region
//! cropping remains only as a fallback when the protocol path fails.

use anyhow::{bail, Result};
use serde::Deserialize;
use std::process::Command;
use std::time::{Duration, Instant};

use crate::x11::WindowInfo;

#[derive(Debug, Deserialize)]
struct HyprClient {
    address: String,
    mapped: bool,
    hidden: bool,
    pid: i64,
    title: String,
    class: String,
    at: [i32; 2],
    size: [i32; 2],
    #[serde(default)]
    monitor: Option<i64>,
    #[serde(default)]
    workspace: Option<HyprWorkspaceRef>,
}

#[derive(Debug, Deserialize)]
struct HyprWorkspaceRef {
    id: i64,
}

#[derive(Debug, Deserialize)]
struct HyprMonitor {
    id: i64,
    name: String,
    #[serde(default)]
    scale: f64,
    #[serde(default)]
    focused: bool,
    #[serde(default, rename = "activeWorkspace")]
    active_workspace: Option<HyprWorkspaceRef>,
    #[serde(default, rename = "specialWorkspace")]
    special_workspace: Option<HyprWorkspaceRef>,
}

#[derive(Debug, Deserialize)]
struct HyprActiveWindow {
    #[serde(default)]
    address: Option<String>,
}

pub fn list_windows(filter_pid: Option<u32>) -> Vec<WindowInfo> {
    list_windows_inner(filter_pid).unwrap_or_default()
}

/// How a per-window capture was obtained. `RegionCrop` pixels come from the
/// live composited screen at the window's geometry — unlike a true
/// `ToplevelExport` surface copy they can show overlapping windows, so
/// callers surfacing the image to a user/LLM should attach a warning.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CaptureMethod {
    ToplevelExport,
    RegionCrop,
}

/// True while a previous toplevel-export scratch thread has not finished.
/// The capture is fully deadline-bounded now, so this should never stay set;
/// it caps the damage at one outstanding thread+connection if something
/// unforeseen (e.g. a blocking AF_UNIX connect) wedges a worker anyway.
static TOPLEVEL_CAPTURE_IN_FLIGHT: std::sync::atomic::AtomicBool =
    std::sync::atomic::AtomicBool::new(false);

/// Per-window screenshot. Tries hyprland-toplevel-export first (true
/// surface capture: correct content for occluded/background windows and
/// windows on other workspaces), falling back to a grim screen-region crop
/// of the client geometry when the protocol path is unavailable.
pub fn screenshot_window_bytes(window_id: u64) -> Result<Vec<u8>> {
    screenshot_window_bytes_with_provenance(window_id).map(|(png, _)| png)
}

/// Like [`screenshot_window_bytes`] but reports which capture method
/// produced the pixels, so tool surfaces can warn about region crops.
///
/// The protocol capture runs on a bounded scratch thread: both the
/// connect/registry handshake and the frame dispatch loops are
/// deadline-bounded in `wayland_capture`, and this function is called
/// synchronously from the recording write path — a wedged compositor must
/// cost at most the timeout, not a hang or a leaked thread.
pub fn screenshot_window_bytes_with_provenance(
    window_id: u64,
) -> Result<(Vec<u8>, CaptureMethod)> {
    use std::sync::atomic::Ordering;

    if TOPLEVEL_CAPTURE_IN_FLIGHT
        .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
        .is_err()
    {
        tracing::debug!(
            "previous toplevel-export capture still in flight; \
             using grim region crop for 0x{window_id:x}"
        );
        return screenshot_window_bytes_grim(window_id)
            .map(|png| (png, CaptureMethod::RegionCrop));
    }

    let (tx, rx) = std::sync::mpsc::sync_channel(1);
    let spawn = std::thread::Builder::new().name("wl-shot".into()).spawn(move || {
        let result = crate::wayland_capture::capture_toplevel_png(window_id);
        TOPLEVEL_CAPTURE_IN_FLIGHT.store(false, Ordering::Release);
        let _ = tx.send(result);
    });
    let result = match spawn {
        Ok(_) => rx
            .recv_timeout(Duration::from_secs(6))
            .map_err(|_| anyhow::anyhow!("toplevel-export capture timed out")),
        Err(e) => {
            TOPLEVEL_CAPTURE_IN_FLIGHT.store(false, Ordering::Release);
            Err(anyhow::anyhow!("capture thread spawn failed: {e}"))
        }
    };
    match result {
        Ok(Ok(png)) => return Ok((png, CaptureMethod::ToplevelExport)),
        Ok(Err(e)) => {
            tracing::debug!(
                "toplevel-export capture failed for 0x{window_id:x} ({e:#}); \
                 falling back to grim region crop"
            );
        }
        Err(e) => {
            tracing::debug!(
                "toplevel-export capture for 0x{window_id:x} did not complete ({e:#}); \
                 falling back to grim region crop"
            );
        }
    }
    screenshot_window_bytes_grim(window_id).map(|png| (png, CaptureMethod::RegionCrop))
}

fn screenshot_window_bytes_grim(window_id: u64) -> Result<Vec<u8>> {
    let client = clients()?
        .into_iter()
        .find(|c| parse_address(&c.address) == Some(window_id))
        .ok_or_else(|| anyhow::anyhow!("Hyprland client 0x{window_id:x} not found"))?;
    if client.size[0] <= 1 || client.size[1] <= 1 {
        bail!("Hyprland client 0x{window_id:x} has invalid geometry");
    }
    // grim has no window concept: -g crops the live composited output at
    // these screen coordinates. If the client's workspace is not the active
    // one on its monitor, the crop would return unrelated screen content
    // that looks like a faithful capture — refuse instead, so callers
    // degrade to "no frame" rather than storing a wrong one. (A window on
    // the active workspace but occluded can still be cropped to the
    // covering window's pixels; that residual is why callers get
    // CaptureMethod::RegionCrop provenance.)
    if let (Some(workspace), Some(monitor_id)) = (&client.workspace, client.monitor) {
        let active = monitors().ok().and_then(|ms| {
            ms.into_iter()
                .find(|m| m.id == monitor_id)
                .and_then(|m| m.active_workspace)
        });
        if let Some(active) = active {
            if active.id != workspace.id {
                bail!(
                    "Hyprland client 0x{window_id:x} is on workspace {} but its monitor \
                     shows workspace {}; a grim region crop would capture unrelated content",
                    workspace.id,
                    active.id
                );
            }
        }
    }

    let geometry = format!(
        "{},{} {}x{}",
        client.at[0], client.at[1], client.size[0], client.size[1]
    );
    let out = Command::new("grim")
        .args(["-g", &geometry, "-t", "png", "-"])
        .output()?;
    if !out.status.success() || out.stdout.is_empty() {
        bail!("grim failed for Hyprland geometry {geometry}");
    }
    Ok(out.stdout)
}

/// Full-desktop screenshot via grim (all outputs composited). Used by the
/// display capture path when running under Wayland, where the X11 root
/// only shows XWayland content.
pub fn screenshot_display_bytes_grim() -> Result<Vec<u8>> {
    let out = Command::new("grim").args(["-t", "png", "-"]).output()?;
    if !out.status.success() || out.stdout.is_empty() {
        bail!("grim full-output capture failed");
    }
    Ok(out.stdout)
}

/// True when running inside a Hyprland session (hyprctl reachable).
pub fn is_hyprland_session() -> bool {
    std::env::var_os("HYPRLAND_INSTANCE_SIGNATURE").is_some()
}

/// Name of the currently focused monitor (e.g. "DP-1"), for selecting the
/// wl_output to record.
pub fn focused_monitor_name() -> Option<String> {
    monitors()
        .ok()?
        .into_iter()
        .find(|m| m.focused)
        .map(|m| m.name)
}

/// Render scale of the monitor a window currently sits on (e.g. 1.5 for
/// fractional scaling). toplevel-export buffers are physical pixels at
/// this scale while hyprctl/AT-SPI geometry is logical, so element
/// coordinates must be multiplied by it to land in screenshot pixels.
pub fn monitor_scale_for_window(window_id: u64) -> Option<f64> {
    let client = clients()
        .ok()?
        .into_iter()
        .find(|c| parse_address(&c.address) == Some(window_id))?;
    let monitor_id = client.monitor?;
    monitors()
        .ok()?
        .into_iter()
        .find(|m| m.id == monitor_id)
        .map(|m| if m.scale > 0.0 { m.scale } else { 1.0 })
}

/// Address of the currently active (focused) Hyprland window, if any.
pub fn active_window_address() -> Option<u64> {
    if !is_hyprland_session() {
        return None;
    }
    let out = Command::new("hyprctl").args(["activewindow", "-j"]).output().ok()?;
    if !out.status.success() || out.stdout.is_empty() {
        return None;
    }
    let active: HyprActiveWindow = serde_json::from_slice(&out.stdout).ok()?;
    parse_address(&active.address?)
}

/// Refocus a window by address (best effort).
///
/// Hyprland ≥0.55 replaced the hyprlang dispatch grammar with Lua
/// (`hl.dsp.focus({ window = 'address:0x...' })`); older releases use the
/// legacy `focuswindow address:0x...` form. Try modern first, then legacy.
pub fn focus_window(address: u64) {
    let modern = format!("hl.dsp.focus({{ window = 'address:0x{address:x}' }})");
    if hyprctl_dispatch(&modern) {
        return;
    }
    let legacy = format!("focuswindow address:0x{address:x}");
    let _ = hyprctl_dispatch(&legacy);
}

/// Workspace ids currently visible on any monitor: each monitor's active
/// workspace, plus its special-workspace overlay when one is open (id 0 =
/// no special workspace shown). A window whose workspace is in this set is
/// what a user would call "on screen".
pub fn visible_workspace_ids() -> Vec<i64> {
    let Ok(ms) = monitors() else { return Vec::new() };
    let mut ids = Vec::new();
    for m in ms {
        if let Some(w) = m.active_workspace {
            ids.push(w.id);
        }
        if let Some(w) = m.special_workspace {
            if w.id != 0 {
                ids.push(w.id);
            }
        }
    }
    ids
}

/// Name of the hidden special workspace background launches land on.
pub const BACKGROUND_WORKSPACE: &str = "special:cua";

/// Launch `shell_cmd` onto the hidden [`BACKGROUND_WORKSPACE`] via
/// `hyprctl dispatch exec` with a `[workspace special:cua silent]` rule
/// prefix — the window maps there without a workspace switch or focus
/// change, so the user's session is untouched (the driver's no-foreground
/// contract). Hyprland forks the child itself, so the pid is discovered by
/// diffing the client list: returns the first new window (preferring one
/// that actually landed on a special workspace, in case the user opened
/// something mid-poll). `Ok(None)` means the dispatch was accepted but no
/// new window mapped within the deadline — a slow cold start, or a
/// single-instance app that routed to an existing process.
pub fn launch_on_special_workspace(shell_cmd: &str) -> Result<Option<WindowInfo>> {
    let before: std::collections::HashSet<u64> =
        list_windows(None).iter().map(|w| w.xid).collect();
    dispatch_exec_with_rules(
        &format!("workspace {BACKGROUND_WORKSPACE} silent"),
        shell_cmd,
    )?;
    let deadline = Instant::now() + Duration::from_secs(10);
    while Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(250));
        let fresh: Vec<WindowInfo> = list_windows(None)
            .into_iter()
            .filter(|w| !before.contains(&w.xid))
            .collect();
        if fresh.is_empty() {
            continue;
        }
        let found = fresh
            .iter()
            .find(|w| w.workspace_id.is_some_and(|id| id < 0))
            .cloned()
            .unwrap_or_else(|| fresh[0].clone());
        // The exec rule only covers the FIRST window the spawned pid maps.
        // Apps with a splash screen burn the rule on the splash and map
        // their real main frame onto the user's active workspace seconds
        // later (observed: PrusaSlicer 2.9.5 — splash got special:cua, main
        // frame arrived on the active workspace ~6 s in), and modal dialogs
        // can map MINUTES later (observed: the "Send G-Code" modal, #15).
        // Sweep the pid's windows back now and keep enforcing for the
        // pid's whole lifetime.
        if let Some(pid) = found.pid {
            enforce_background_placement(pid);
            guard_background_placement_for_pid_lifetime(pid);
        }
        return Ok(Some(found));
    }
    Ok(None)
}

/// Silently move every non-special-workspace window of `pid` onto
/// [`BACKGROUND_WORKSPACE`]. One enforcement pass; returns how many windows
/// were moved.
fn enforce_background_placement(pid: u32) -> usize {
    let mut moved = 0;
    for c in clients().unwrap_or_default() {
        if c.pid == pid as i64 && c.workspace.as_ref().is_some_and(|w| w.id >= 0) {
            if let Some(addr) = parse_address(&c.address) {
                if move_window_to_background_workspace(addr) {
                    moved += 1;
                }
            }
        }
    }
    moved
}

/// Keep every window `pid` maps — for the pid's whole lifetime — off the
/// user's workspaces (#15: a fixed 20 s watch missed modal dialogs that
/// open minutes into a workflow, exactly at the most user-visible step).
/// Two tiers:
///
/// - Hyprland ≥0.55: a compositor-resident `window.open` hook moves a
///   guarded pid's new window onto [`BACKGROUND_WORKSPACE`] inside the
///   open event itself — before a frame renders, so nothing flashes on
///   the user's workspace and no polling runs. A detached reaper drops
///   the pid from the compositor-side registry when the process exits.
/// - Older releases (no compositor Lua state): the polling sweep, kept
///   alive until the pid exits instead of a fixed watch window.
///
/// Either way the parent window is already on [`BACKGROUND_WORKSPACE`],
/// so a swept modal lands on the SAME workspace as its parent — never
/// orphaned cross-workspace, which would break the compositor's focus
/// routing for the whole app (observed: `activewindow: None`, all input
/// dead until the pair was reunited).
fn guard_background_placement_for_pid_lifetime(pid: u32) {
    if register_background_pid_hook(pid) {
        spawn_background_pid_reaper(pid);
    } else {
        spawn_background_placement_guard(pid);
    }
}

/// Add `pid` to the compositor-side guard registry (`_G.cua_bg.pids`),
/// installing the shared `window.open` subscription on first use. The
/// callback runs inside the compositor at map time and reuses the atomic
/// move + re-hide pattern from [`move_window_to_background_workspace`].
/// Returns false on pre-Lua Hyprland (legacy grammar), where the caller
/// must fall back to polling.
///
/// An event subscription is used rather than `hl.window_rule` because
/// rules have no pid matcher and can only ever be disabled, never removed
/// (`:set_enabled(false)` is the whole runtime API, verified live on
/// 0.55.3) — and a per-launch rule that can't be removed is a leak that
/// pid recycling would eventually turn into misplaced windows.
fn register_background_pid_hook(pid: u32) -> bool {
    reset_background_pid_registry_once();
    let workspace = BACKGROUND_WORKSPACE;
    let name = workspace.trim_start_matches("special:");
    let lua = format!(
        "(function() \
         if _G.cua_bg == nil then _G.cua_bg = {{ pids = {{}} }} end \
         _G.cua_bg.pids[{pid}] = true \
         if _G.cua_bg.sub == nil then \
         _G.cua_bg.sub = hl.on('window.open', function(w) \
         local t = _G.cua_bg \
         if t == nil or w == nil or not t.pids[w.pid] then return end \
         if w.workspace == nil or w.workspace.id >= 0 then \
         hl.dispatch(hl.dsp.window.move({{ workspace = '{workspace}', window = 'address:'..tostring(w.address) }})) \
         local sp = hl.get_active_special_workspace() \
         if sp ~= nil and tostring(sp):find('{workspace}', 1, true) then \
         hl.dispatch(hl.dsp.workspace.toggle_special('{name}')) end \
         end \
         end) \
         end \
         return hl.dsp.no_op() end)()"
    );
    hyprctl_dispatch(&lua)
}

/// One-time (per daemon process) cleanup of compositor-side guard state a
/// previous daemon instance may have left behind: a stale `window.open`
/// subscription whose reaper died with the old daemon would keep sweeping
/// its registered pids — and pids recycle.
fn reset_background_pid_registry_once() {
    static RESET: std::sync::Once = std::sync::Once::new();
    RESET.call_once(|| {
        let _ = hyprctl_dispatch(
            "(function() \
             local t = _G.cua_bg \
             if t ~= nil and t.sub ~= nil then t.sub:remove() end \
             _G.cua_bg = { pids = {} } \
             return hl.dsp.no_op() end)()",
        );
    });
}

/// Drop `pid` from the compositor-side guard registry, removing the shared
/// `window.open` subscription once no guarded pids remain.
fn unregister_background_pid_hook(pid: u32) {
    let lua = format!(
        "(function() \
         local t = _G.cua_bg \
         if t ~= nil then \
         t.pids[{pid}] = nil \
         if t.sub ~= nil and next(t.pids) == nil then t.sub:remove() t.sub = nil end \
         end \
         return hl.dsp.no_op() end)()"
    );
    let _ = hyprctl_dispatch(&lua);
}

/// Watch for `pid`'s exit, then unregister its compositor-side guard
/// entry. Process identity is `(pid, /proc starttime)` so a recycled pid
/// can't keep an unrelated process's windows getting swept off-screen.
fn spawn_background_pid_reaper(pid: u32) {
    std::thread::spawn(move || {
        let Some(start) = proc_start_time(pid) else {
            // Gone before the reaper started; nothing left to guard.
            unregister_background_pid_hook(pid);
            return;
        };
        while proc_start_time(pid) == Some(start) {
            std::thread::sleep(Duration::from_secs(1));
        }
        unregister_background_pid_hook(pid);
    });
}

/// Pre-0.55 fallback for [`guard_background_placement_for_pid_lifetime`]:
/// keep sweeping the launched pid's windows onto the background workspace
/// until the process exits. A late window can flash on the active
/// workspace for up to one poll interval before the sweep catches it —
/// the event-hook tier exists precisely to avoid that. Polls fast through
/// app startup (splash → main frame churn), then relaxes. Best-effort,
/// same spirit as [`spawn_focus_restore_guard`].
fn spawn_background_placement_guard(pid: u32) {
    std::thread::spawn(move || {
        let Some(start) = proc_start_time(pid) else { return };
        let begun = Instant::now();
        while proc_start_time(pid) == Some(start) {
            std::thread::sleep(if begun.elapsed() < Duration::from_secs(30) {
                Duration::from_millis(300)
            } else {
                Duration::from_secs(1)
            });
            enforce_background_placement(pid);
        }
    });
}

/// Kernel start time of `pid` (`/proc/<pid>/stat` field 22, clock ticks
/// since boot) — the stable half of a `(pid, starttime)` process identity.
/// `None` once the process is gone. The comm field may itself contain
/// spaces and parens, so fields are taken from after the LAST `)`.
fn proc_start_time(pid: u32) -> Option<u64> {
    let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    let (_, rest) = stat.rsplit_once(')')?;
    rest.split_ascii_whitespace().nth(19)?.parse().ok()
}

/// Move a window (by Hyprland client address) onto [`BACKGROUND_WORKSPACE`]
/// without revealing it.
///
/// On Hyprland ≥0.55 `hl.dsp.window.move` ALWAYS reveals the target special
/// workspace — a `silent = true` key is accepted but ignored (verified
/// live), so a plain move would pop the hidden workspace over the user's
/// session. The modern path is therefore an atomic Lua script: move, then
/// re-hide the overlay if (and only if) it now shows OUR workspace, all
/// inside one compositor dispatch so no frame is ever rendered with the
/// overlay visible. It returns `hl.dsp.no_op()` so hyprctl answers "ok".
/// Older releases fall back to legacy `movetoworkspacesilent`, which is
/// silent natively.
pub fn move_window_to_background_workspace(address: u64) -> bool {
    let workspace = BACKGROUND_WORKSPACE;
    let name = workspace.trim_start_matches("special:");
    let modern = format!(
        "(function() \
         hl.dispatch(hl.dsp.window.move({{ workspace = '{workspace}', window = 'address:0x{address:x}' }})) \
         local sp = hl.get_active_special_workspace() \
         if sp ~= nil and tostring(sp):find('{workspace}', 1, true) then \
         hl.dispatch(hl.dsp.workspace.toggle_special('{name}')) end \
         return hl.dsp.no_op() end)()"
    );
    if hyprctl_dispatch(&modern) {
        return true;
    }
    hyprctl_dispatch(&format!("movetoworkspacesilent {workspace},address:0x{address:x}"))
}

/// `hyprctl dispatch exec` with a window-rule prefix, speaking both
/// grammars: Hyprland ≥0.55 replaced hyprlang dispatch with Lua —
/// `dispatch exec "[rules] cmd"` is a parse error there and the modern
/// form is `hl.dsp.exec_cmd('[rules] cmd')` (verified on 0.55: the rule
/// prefix rides inside the exec payload in both grammars). Try modern
/// first, legacy second, mirroring `focus_window` above.
fn dispatch_exec_with_rules(rules: &str, shell_cmd: &str) -> Result<()> {
    let payload = format!("[{rules}] {shell_cmd}");
    let lua = format!(
        "hl.dsp.exec_cmd('{}')",
        payload.replace('\\', r"\\").replace('\'', r"\'")
    );
    if hyprctl_dispatch(&lua) {
        return Ok(());
    }
    if hyprctl_dispatch(&format!("exec {payload}")) {
        return Ok(());
    }
    bail!("hyprctl dispatch exec was rejected in both the modern (Lua) and legacy grammar");
}

/// Run `hyprctl dispatch <arg>`; true only when the compositor answered
/// "ok" (hyprctl can exit 0 while printing an error).
fn hyprctl_dispatch(arg: &str) -> bool {
    Command::new("hyprctl")
        .args(["dispatch", arg])
        .output()
        .map(|o| {
            o.status.success()
                && String::from_utf8_lossy(&o.stdout).trim_start().starts_with("ok")
        })
        .unwrap_or(false)
}

/// Preserve the active window across an app launch: snapshot the focused
/// window now and watch — for the next ~3 s — for a launch-induced focus
/// grab, putting the previous window back.
///
/// Hyprland focuses newly mapped windows by default; the driver's contract
/// is that the user's frontmost window must not change, so launch_app
/// restores it. Two phases distinguish launch-induced grabs from a user
/// alt-tab during the watch:
///
/// - For the first ~700 ms ANY focus change away from the snapshot is
///   reverted, including to a pre-existing window — xdg-open handing a URL
///   to an already-running browser raises that browser's EXISTING toplevel
///   within a few hundred ms, while a deliberate human alt-tab in that
///   sliver is improbable.
/// - After that, only focus grabs by NEW windows (slow app cold-starts) are
///   reverted, so a user alt-tabbing to a pre-existing window later in the
///   watch is left alone.
///
/// Detached and best-effort: if the previous window closed or hyprctl
/// fails, focus is simply left alone.
pub fn spawn_focus_restore_guard() {
    if !is_hyprland_session() {
        return;
    }
    let Some(previous) = active_window_address() else {
        return;
    };
    let preexisting: std::collections::HashSet<u64> = clients()
        .unwrap_or_default()
        .iter()
        .filter_map(|c| parse_address(&c.address))
        .collect();
    std::thread::spawn(move || {
        const EARLY_RESTORE: Duration = Duration::from_millis(700);
        let start = Instant::now();
        let deadline = start + Duration::from_secs(3);
        while Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(100));
            match active_window_address() {
                Some(current) if current != previous => {
                    if start.elapsed() < EARLY_RESTORE || !preexisting.contains(&current) {
                        focus_window(previous);
                        return;
                    }
                }
                _ => {}
            }
        }
    });
}

fn list_windows_inner(filter_pid: Option<u32>) -> Result<Vec<WindowInfo>> {
    let mut out = Vec::new();
    for client in clients()? {
        if !client.mapped || client.hidden || client.size[0] <= 1 || client.size[1] <= 1 {
            continue;
        }
        let pid = u32::try_from(client.pid).ok();
        if let Some(filter_pid) = filter_pid {
            if pid != Some(filter_pid) {
                continue;
            }
        }
        let Some(window_id) = parse_address(&client.address) else {
            continue;
        };
        let title = if client.title.trim().is_empty() {
            client.class
        } else {
            client.title
        };
        if title.trim().is_empty() {
            continue;
        }
        out.push(WindowInfo {
            xid: window_id,
            pid,
            title,
            x: client.at[0],
            y: client.at[1],
            width: client.size[0] as u32,
            height: client.size[1] as u32,
            workspace_id: client.workspace.as_ref().map(|w| w.id),
        });
    }
    Ok(out)
}

fn clients() -> Result<Vec<HyprClient>> {
    if !is_hyprland_session() {
        return Ok(Vec::new());
    }
    let out = Command::new("hyprctl").args(["clients", "-j"]).output()?;
    if !out.status.success() || out.stdout.is_empty() {
        bail!("hyprctl clients -j failed");
    }
    Ok(serde_json::from_slice(&out.stdout)?)
}

fn monitors() -> Result<Vec<HyprMonitor>> {
    if !is_hyprland_session() {
        bail!("not a Hyprland session");
    }
    let out = Command::new("hyprctl").args(["monitors", "-j"]).output()?;
    if !out.status.success() || out.stdout.is_empty() {
        bail!("hyprctl monitors -j failed");
    }
    Ok(serde_json::from_slice(&out.stdout)?)
}

fn parse_address(address: &str) -> Option<u64> {
    u64::from_str_radix(address.trim_start_matches("0x"), 16).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn proc_start_time_reads_a_live_process() {
        assert!(proc_start_time(std::process::id()).is_some());
    }

    #[test]
    fn proc_start_time_is_stable_for_a_live_process() {
        let pid = std::process::id();
        assert_eq!(proc_start_time(pid), proc_start_time(pid));
    }

    #[test]
    fn proc_start_time_none_once_gone() {
        // pid 0 (the idle task) never has a /proc/<pid>/stat entry.
        assert_eq!(proc_start_time(0), None);
    }

    /// Kills the probe process even when an assertion panics mid-test.
    struct KillOnDrop(u32);
    impl Drop for KillOnDrop {
        fn drop(&mut self) {
            let _ = Command::new("kill").arg(self.0.to_string()).output();
        }
    }

    fn probe_windows(class: &str) -> Vec<HyprClient> {
        clients()
            .unwrap_or_default()
            .into_iter()
            .filter(|c| c.class == class)
            .collect()
    }

    /// Live acceptance test for #15 — needs a running Hyprland ≥0.55 and
    /// kitty; run with `cargo test -p platform-linux -- --ignored`.
    ///
    /// Maps a SECOND window from an already-guarded pid (kitty remote
    /// control: one process, two OS windows) and asserts the window.open
    /// hook placed it on the background workspace without revealing the
    /// special workspace on any monitor.
    #[test]
    #[ignore = "drives the live compositor; Hyprland + kitty only"]
    fn live_window_open_hook_sweeps_late_windows() {
        const CLASS: &str = "cua-live-issue15";
        if !is_hyprland_session()
            || !Command::new("kitty").arg("--version").output().is_ok_and(|o| o.status.success())
        {
            eprintln!("skipping: needs a live Hyprland session and kitty");
            return;
        }
        assert!(probe_windows(CLASS).is_empty(), "stale {CLASS} probe window present");

        dispatch_exec_with_rules(
            &format!("workspace {BACKGROUND_WORKSPACE} silent"),
            &format!(
                "kitty --class {CLASS} -o allow_remote_control=yes \
                 -o confirm_os_window_close=0 --listen-on=unix:@{CLASS}"
            ),
        )
        .expect("dispatch exec");

        let deadline = Instant::now() + Duration::from_secs(10);
        let first = loop {
            if let Some(c) = probe_windows(CLASS).pop() {
                break c;
            }
            assert!(Instant::now() < deadline, "first probe window never mapped");
            std::thread::sleep(Duration::from_millis(200));
        };
        let pid = first.pid as u32;
        let _guard = KillOnDrop(pid);

        assert!(register_background_pid_hook(pid), "hook registration rejected");

        // Same pid maps a second OS window — Hyprland's default placement
        // for it would be the user's active workspace.
        let out = Command::new("kitten")
            .args(["@", "--to", &format!("unix:@{CLASS}"), "launch", "--type=os-window"])
            .output()
            .expect("kitten launch");
        assert!(out.status.success(), "kitten remote launch failed");

        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            let wins = probe_windows(CLASS);
            if wins.len() >= 2
                && wins.iter().all(|w| w.workspace.as_ref().is_some_and(|ws| ws.id < 0))
            {
                break;
            }
            assert!(
                Instant::now() < deadline,
                "late window never reached {BACKGROUND_WORKSPACE}: {:?}",
                wins.iter().map(|w| w.workspace.as_ref().map(|ws| ws.id)).collect::<Vec<_>>()
            );
            std::thread::sleep(Duration::from_millis(100));
        }

        // The sweep must not have revealed the special workspace anywhere.
        for m in monitors().expect("monitors") {
            assert!(
                m.special_workspace.as_ref().is_none_or(|ws| ws.id == 0),
                "special workspace left revealed on {}",
                m.name
            );
        }

        unregister_background_pid_hook(pid);
    }
}
