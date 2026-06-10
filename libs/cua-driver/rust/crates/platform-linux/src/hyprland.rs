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
        let on_special = fresh
            .iter()
            .find(|w| w.workspace_id.is_some_and(|id| id < 0))
            .cloned();
        return Ok(Some(on_special.unwrap_or_else(|| fresh[0].clone())));
    }
    Ok(None)
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
