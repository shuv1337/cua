//! Recording callbacks for Linux, mirroring
//! `platform_macos::recording_hooks`.
//!
//! - `app_state_json_for` → per-turn `app_state.json`: the AT-SPI tree in
//!   the same `{pid, window_id, element_count, tree_markdown}` shape
//!   `get_window_state` returns (minus screenshot fields).
//! - `element_window_local_xy` → element center in window-local screenshot
//!   pixels, so `click.png` markers and `action.json.click_point` work for
//!   element_index-addressed clicks, not just pixel ones.
//!
//! Coordinate spaces: AT-SPI extents and hyprctl geometry are logical
//! compositor coordinates, but toplevel-export screenshots of native
//! Wayland windows are physical pixels at the window's monitor render
//! scale (1.5x fractional scaling is common). X11/XWayland `import`
//! captures are 1:1 with X11 logical coordinates, so they need no scale.
//!
//! Known limitation: element bounds come from a post-action AT-SPI
//! re-walk, so if the action changed the tree the index can drift and the
//! marker lands on the wrong element. macOS avoids this with a process-
//! global element cache; porting that to Linux is the proper fix.

use std::sync::mpsc::SyncSender;
use std::sync::OnceLock;
use std::time::Duration;

/// Jobs for the single long-lived hook worker, boxed so one thread serves
/// both hook shapes.
type HookJob = Box<dyn FnOnce() + Send + 'static>;

/// `Some(sender)` once the "rec-hook" worker is up; `None` when the spawn
/// failed (hooks then permanently skip, matching the old per-call `.ok()?`).
static HOOK_TX: OnceLock<Option<SyncSender<HookJob>>> = OnceLock::new();

/// Run a hook body on the long-lived "rec-hook" worker with a bounded wait.
///
/// Recording hooks are invoked synchronously from `write_turn` on a tokio
/// async worker thread (tool impls escape via `spawn_blocking`, the
/// recording path does not). The AT-SPI layer drives its own runtime via
/// `block_on`, which panics when called from inside an async context — so
/// hop to a plain thread first. A single persistent worker (rather than a
/// spawn per call) caps in-flight AT-SPI work at one: agent-driven turn
/// rates against a slow tree would otherwise accumulate leaked scratch
/// threads all contending on the shared single-worker AT-SPI runtime.
///
/// The join is deadline-bounded so a wedged D-Bus walk cannot stall the
/// tool-response path. The job queue holds at most one pending job; while
/// the worker is busy and the slot is taken, further turns skip (`None`,
/// callers omit the artifact for that turn). A timed-out job's late result
/// lands in its own dropped channel and is discarded, leaving the worker
/// free for the next turn.
fn on_scratch_thread<T, F>(timeout: Duration, f: F) -> Option<T>
where
    F: FnOnce() -> Option<T> + Send + 'static,
    T: Send + 'static,
{
    let tx = HOOK_TX
        .get_or_init(|| {
            let (tx, rx) = std::sync::mpsc::sync_channel::<HookJob>(1);
            std::thread::Builder::new()
                .name("rec-hook".into())
                .spawn(move || {
                    while let Ok(job) = rx.recv() {
                        job();
                    }
                })
                .ok()
                .map(|_| tx)
        })
        .as_ref()?;

    let (result_tx, result_rx) = std::sync::mpsc::sync_channel(1);
    let job: HookJob = Box::new(move || {
        // Capacity-1 + a fresh channel per job: the send never blocks, and
        // a result arriving after the recv_timeout below is simply dropped
        // with the channel.
        let _ = result_tx.try_send(f());
    });
    if tx.try_send(job).is_err() {
        // Queue full: a previous turn's walk is still running AND one job is
        // already waiting behind it — skip this turn rather than backlog.
        return None;
    }
    result_rx.recv_timeout(timeout).ok().flatten()
}

pub fn app_state_json_for(window_id: Option<u64>, pid: Option<i64>) -> Option<Vec<u8>> {
    // pid is required, matching the macOS hook: AT-SPI lookup is by pid.
    let pid = pid?;
    let pid_u32 = u32::try_from(pid).ok()?;
    // Bail fast when the action closed the app's last window (OK-button
    // clicks, dialog dismissals): an AT-SPI walk against a dying process
    // burns multi-second D-Bus timeouts and stalls the tool response.
    let windows = crate::x11::list_windows(Some(pid_u32));
    if windows.is_empty() {
        return None;
    }
    // Match macOS/Windows: always emit a numeric window id when one can be
    // resolved (first window of the pid when the recorded args had none).
    let window_id = window_id.or_else(|| windows.first().map(|w| w.xid));
    on_scratch_thread(Duration::from_secs(12), move || {
        // Native AT-SPI only — without the X11-properties fallback the
        // shared walk_tree wrapper uses for get_window_state. That
        // fallback fabricates a one-node title tree (and truncates
        // Hyprland addresses to u32), which is worse than omitting
        // app_state.json for the turn.
        let (tree_markdown, nodes) = crate::atspi::native::walk_tree(pid_u32).ok().flatten()?;
        if tree_markdown.is_empty() {
            return None;
        }
        let payload = serde_json::json!({
            "pid": pid,
            "window_id": window_id,
            "element_count": nodes.len(),
            "tree_markdown": tree_markdown,
        });
        serde_json::to_vec_pretty(&payload).ok()
    })
}

pub fn element_window_local_xy(
    window_id: u64,
    pid: i64,
    element_index: u32,
) -> Option<(f64, f64)> {
    let pid_u32 = u32::try_from(pid).ok()?;
    on_scratch_thread(Duration::from_secs(8), move || {
        element_window_local_xy_blocking(window_id, pid_u32, element_index)
    })
}

fn element_window_local_xy_blocking(
    window_id: u64,
    pid_u32: u32,
    element_index: u32,
) -> Option<(f64, f64)> {
    // Window lookup first: it's cheap, and when the click closed the
    // window there's no point burning AT-SPI timeouts on a dead app.
    let win = crate::x11::list_windows(Some(pid_u32))
        .into_iter()
        .find(|w| w.xid == window_id)?;

    // Budget strictly under the 8s scratch-thread join above: unbounded,
    // this call parks the shared hook worker on a wedged AT-SPI peer long
    // after the join gave up (see `get_element_bounds_bounded`).
    let (x, y, w, h) = crate::atspi::native::get_element_bounds_bounded(
        pid_u32,
        element_index as usize,
        Duration::from_secs(7),
    )
    .ok()?;
    let cx = x as f64 + w as f64 / 2.0;
    let cy = y as f64 + h as f64 / 2.0;

    let native_wayland = window_id > u32::MAX as u64;

    // AT-SPI extents are screen coordinates for X11/XWayland apps, but
    // toolkits on native Wayland cannot know their global position and
    // report window-local coordinates (no Wayland protocol exposes the
    // window's place in the layout). Both interpretations are tested for
    // in-window containment; when both fit (window near the layout
    // origin), native Wayland prefers the window-local reading because
    // that is what GTK4/Qt actually emit — the screen-coordinate reading
    // only arises from the rare all-zero-extents fallback in
    // `component_extents_for_pid`, and mis-picking there costs at most
    // the (small) window-origin offset.
    let in_window = |lx: f64, ly: f64| {
        lx >= 0.0 && ly >= 0.0 && lx <= win.width as f64 && ly <= win.height as f64
    };
    let screen_rel = (cx - win.x as f64, cy - win.y as f64);
    let (lx, ly) = if native_wayland && in_window(cx, cy) {
        (cx, cy)
    } else if in_window(screen_rel.0, screen_rel.1) {
        screen_rel
    } else {
        return None;
    };

    // Native Wayland windows (Hyprland addresses) are captured at physical
    // pixel scale; X11/XWayland captures match logical coordinates.
    let scale = if native_wayland {
        crate::hyprland::monitor_scale_for_window(window_id).unwrap_or(1.0)
    } else {
        1.0
    };

    Some((lx * scale, ly * scale))
}
