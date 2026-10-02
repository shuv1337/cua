//! Application-state snapshots used by trajectory recording on Linux.

/// Per-turn application state for trajectory recording, walked under the
/// shared `get_window_state` budget. The walk reports the shared walk fields so
/// the turn evidence says when the tree is partial.
#[cfg(target_os = "linux")]
pub fn app_state_json_for(
    window_id: Option<u64>,
    pid: Option<i64>,
    budget: cua_driver_core::recording::StateCaptureBudget,
) -> Option<Vec<u8>> {
    if tokio::runtime::Handle::try_current().is_ok() {
        return std::thread::spawn(move || app_state_json_for_blocking(window_id, pid, budget))
            .join()
            .ok()
            .flatten();
    }
    app_state_json_for_blocking(window_id, pid, budget)
}

#[cfg(target_os = "linux")]
fn app_state_json_for_blocking(
    window_id: Option<u64>,
    pid: Option<i64>,
    budget: cua_driver_core::recording::StateCaptureBudget,
) -> Option<Vec<u8>> {
    let pid = u32::try_from(pid?).ok()?;
    let window_id = if crate::wayland::is_inject_mode() {
        // Most injected actions already carry the protocol-verified window id.
        // Process-scoped setup calls such as browser_prepare do not, so resolve
        // their single target here instead of classifying required AX evidence
        // as a capture failure.
        match window_id {
            Some(window_id) => window_id,
            None => resolve_window_for_recording(pid, None)?.xid,
        }
    } else {
        resolve_window_for_recording(pid, window_id)?.xid
    };
    let timeout = std::time::Duration::from_millis(budget.timeout_ms);
    let result = if crate::wayland::is_inject_mode() {
        // Evidence capture runs inside the daemon call. One attempt only, so
        // an unresponsive renderer cannot block input past the budget.
        crate::atspi::walk_tree_for_recording(pid, window_id, timeout)
    } else {
        crate::atspi::walk_tree_bounded_within(pid, window_id, None, None, None, timeout)
    };
    if result.nodes.is_empty() || result.tree_markdown.trim().is_empty() {
        return None;
    }
    let element_count = result
        .nodes
        .iter()
        .filter(|node| node.element_index.is_some())
        .count();
    let mut payload = serde_json::json!({
        "pid": pid,
        "window_id": window_id,
        "element_count": element_count,
        "tree_markdown": result.tree_markdown,
        // The shared walk fields (see cua_driver_core::walk_budget).
        "truncated": result.truncated,
        "nodes_visited": result.nodes_visited,
        "nodes_pending": result.nodes_pending,
        "walk_elapsed_ms": result.elapsed_ms as u64,
        "timeout_ms": budget.timeout_ms,
    });
    if let Some(reason) = result.truncation_reason {
        payload["truncation_reason"] = serde_json::json!(reason);
    }
    serde_json::to_vec_pretty(&payload).ok()
}

#[cfg(target_os = "linux")]
pub fn screenshot_for_recording(window_id: Option<u64>, pid: Option<i64>) -> Option<Vec<u8>> {
    if crate::wayland::is_wayland() {
        if crate::wayland::hyprland::is_session() {
            // Recording uses the same qualified logical canvas even with one
            // scaled output. Raw physical pixels cannot share logical markers.
            let frame = crate::wayland::hyprland::desktop_frame().ok()?;
            return crate::wayland::hyprland::compose_desktop_capture(&frame).ok();
        }
        // Wayland surface ids are connection-scoped, so a later recording hook
        // cannot safely re-attest the action call's per-window crop. Preserve
        // ownership by recording the compositor's complete rendered output.
        return crate::wayland::screenshot_display_dispatch().ok();
    }
    if let Some(window_id) = window_id {
        crate::wayland::screenshot_dispatch(window_id).ok()
    } else if let Some(pid) = pid.and_then(|pid| u32::try_from(pid).ok()) {
        let window = resolve_window_for_recording(pid, None)?;
        crate::wayland::screenshot_dispatch(window.xid).ok()
    } else {
        crate::capture::screenshot_display_bytes().ok()
    }
}

#[cfg(target_os = "linux")]
pub fn element_window_local_xy(
    pid: i64,
    args: &serde_json::Value,
    capture_point: bool,
) -> Option<(u64, Option<(f64, f64)>)> {
    let cache = cua_driver_core::snapshot_store::current_runtime_store::<
        crate::atspi::snapshot::AtspiSnapshot,
    >()?;
    let resolved = cache.resolve(i32::try_from(pid).ok()?, args).ok()?;
    let (index, window, _) = resolved.into_parts(None);
    let window_id = window?;
    let element_index = u32::try_from(index?).ok()?;
    let point = if !capture_point {
        None
    } else if tokio::runtime::Handle::try_current().is_ok() {
        std::thread::spawn(move || element_window_local_xy_blocking(window_id, pid, element_index))
            .join()
            .ok()
            .flatten()
    } else {
        element_window_local_xy_blocking(window_id, pid, element_index)
    };
    Some((window_id, point))
}

#[cfg(target_os = "linux")]
fn element_window_local_xy_blocking(
    window_id: u64,
    pid: i64,
    element_index: u32,
) -> Option<(f64, f64)> {
    let pid = u32::try_from(pid).ok()?;
    let (screen_x, screen_y, width, height) =
        crate::atspi::get_element_bounds_for_window(pid, window_id, element_index as usize).ok()?;
    let window = resolve_window_for_recording(pid, Some(window_id))?;
    if crate::wayland::is_wayland() && crate::wayland::hyprland::is_session() {
        let frame = crate::wayland::hyprland::desktop_frame().ok()?;
        // This hook retains the whole output, not the window-local tool image.
        let point = hyprland_recording_point(
            (screen_x, screen_y, width, height),
            (window.x, window.y, window.width, window.height),
            (frame.x, frame.y, frame.width, frame.height),
        )?;
        frame.checked_layout_point(point.0, point.1).ok()?;
        frame.validate_current().ok()?;
        return Some(point);
    }
    Some((
        f64::from(screen_x) - f64::from(window.x) + f64::from(width) / 2.0,
        f64::from(screen_y) - f64::from(window.y) + f64::from(height) / 2.0,
    ))
}

#[cfg(any(target_os = "linux", test))]
fn hyprland_recording_point(
    element: (i32, i32, u32, u32),
    window: (i32, i32, u32, u32),
    display: (i32, i32, u32, u32),
) -> Option<(f64, f64)> {
    let (x, y, width, height) = element;
    if width == 0 || height == 0 || window.2 == 0 || window.3 == 0 {
        return None;
    }
    let cx = f64::from(x) + f64::from(width) / 2.0;
    let cy = f64::from(y) + f64::from(height) / 2.0;
    hyprland_output_point((cx, cy), window, display)
}

#[cfg(any(target_os = "linux", test))]
fn hyprland_output_point(
    (cx, cy): (f64, f64),
    window: (i32, i32, u32, u32),
    display: (i32, i32, u32, u32),
) -> Option<(f64, f64)> {
    let in_window = cx >= f64::from(window.0)
        && cy >= f64::from(window.1)
        && cx < f64::from(window.0) + f64::from(window.2)
        && cy < f64::from(window.1) + f64::from(window.3);
    let fx = cx - f64::from(display.0);
    let fy = cy - f64::from(display.1);
    let in_display =
        fx >= 0.0 && fy >= 0.0 && fx < f64::from(display.2) && fy < f64::from(display.3);
    (in_window && in_display).then_some((fx, fy))
}

#[cfg(target_os = "linux")]
pub fn hyprland_pixel_recording_point(
    window_id: Option<u64>,
    pid: Option<i64>,
    x: f64,
    y: f64,
) -> Option<(f64, f64)> {
    // The recorded image is the desktop frame. Window geometry is in layout
    // coordinates, so move a window point into the frame before placing it.
    let frame = crate::wayland::hyprland::desktop_frame().ok()?;
    let display = (frame.x, frame.y, frame.width, frame.height);
    let point = match (window_id, pid) {
        (Some(window_id), Some(pid)) => {
            let pid = u32::try_from(pid).ok()?;
            let window = resolve_window_for_recording(pid, Some(window_id))?;
            if window.pid != Some(pid) {
                return None;
            }
            hyprland_output_point(
                (f64::from(window.x) + x, f64::from(window.y) + y),
                (window.x, window.y, window.width, window.height),
                display,
            )
        }
        (None, None) => frame.checked_layout_point(x, y).ok().map(|_| (x, y)),
        _ => None,
    }?;
    frame.checked_layout_point(point.0, point.1).ok()?;
    frame.validate_current().ok()?;
    Some(point)
}

#[cfg(target_os = "linux")]
fn resolve_window_for_recording(
    pid: u32,
    window_id: Option<u64>,
) -> Option<crate::x11::WindowInfo> {
    let windows = crate::wayland::list_windows_dispatch(Some(pid));
    let stable_ids = !crate::wayland::is_wayland() || crate::wayland::hyprland::is_session();
    select_recording_window(windows, pid, window_id, stable_ids)
}

#[cfg(target_os = "linux")]
fn select_recording_window(
    windows: Vec<crate::x11::WindowInfo>,
    pid: u32,
    window_id: Option<u64>,
    stable_ids: bool,
) -> Option<crate::x11::WindowInfo> {
    let mut windows = windows.into_iter().filter(|window| window.pid == Some(pid));
    if let Some(id) = window_id {
        // Foreign-toplevel object ids are scoped to the observation connection.
        // A new hook connection cannot prove the action's exact window even if
        // the process now has only one window. Omit evidence instead of guessing.
        stable_ids
            .then(|| windows.find(|window| window.xid == id))
            .flatten()
    } else {
        let only = windows.next()?;
        windows.next().is_none().then_some(only)
    }
}

#[cfg(not(target_os = "linux"))]
pub fn app_state_json_for(
    _window_id: Option<u64>,
    _pid: Option<i64>,
    _budget: cua_driver_core::recording::StateCaptureBudget,
) -> Option<Vec<u8>> {
    None
}

#[cfg(not(target_os = "linux"))]
pub fn screenshot_for_recording(_window_id: Option<u64>, _pid: Option<i64>) -> Option<Vec<u8>> {
    None
}

#[cfg(not(target_os = "linux"))]
pub fn element_window_local_xy(
    _pid: i64,
    _args: &serde_json::Value,
    _capture_point: bool,
) -> Option<(u64, Option<(f64, f64)>)> {
    None
}

#[cfg(test)]
mod tests {
    use super::{hyprland_output_point, hyprland_recording_point};

    #[cfg(target_os = "linux")]
    #[test]
    fn recording_window_requires_exact_or_unique_process_identity() {
        fn window(xid: u64, pid: u32) -> crate::x11::WindowInfo {
            crate::x11::WindowInfo {
                xid,
                pid: Some(pid),
                app_name: String::new(),
                title: String::new(),
                is_on_screen: true,
                z_index: None,
                x: 0,
                y: 0,
                width: 100,
                height: 100,
            }
        }
        let select = |ids: &[u64], requested, stable| {
            super::select_recording_window(
                ids.iter().map(|id| window(*id, 42)).collect(),
                42,
                requested,
                stable,
            )
            .map(|w| w.xid)
        };
        assert_eq!(select(&[10, 20], Some(20), true), Some(20));
        assert_eq!(select(&[10, 20], None, true), None);
        assert_eq!(select(&[10, 20], None, false), None);
        assert_eq!(select(&[10], None, true), Some(10));
        assert_eq!(select(&[10], None, false), Some(10));
        assert_eq!(select(&[10], Some(20), true), None);
        assert_eq!(select(&[20], Some(20), false), None);
        assert_eq!(select(&[], None, true), None);
        assert!(super::select_recording_window(vec![window(20, 99)], 42, Some(20), true).is_none());
    }

    #[test]
    fn recording_marker_rebases_negative_and_positive_frame_origins() {
        assert_eq!(
            hyprland_output_point(
                (-2250.0, 270.0),
                (-2500, 20, 800, 600),
                (-2560, 0, 5120, 1440)
            ),
            Some((310.0, 270.0))
        );
        assert_eq!(
            hyprland_output_point((2810.0, 270.0), (2560, 20, 800, 600), (2560, 0, 2560, 1440)),
            Some((250.0, 270.0))
        );
    }

    #[test]
    fn hyprland_marker_uses_retained_output_coordinates() {
        assert_eq!(
            hyprland_recording_point((130, 250, 20, 30), (100, 200, 940, 780), (0, 0, 1920, 1080)),
            Some((140.0, 265.0))
        );
    }

    #[test]
    fn hyprland_marker_does_not_invent_an_offscreen_point() {
        for element in [(10, 800, 20, 20), (-30, 10, 20, 20), (10, 10, 0, 20)] {
            assert_eq!(
                hyprland_recording_point(element, (0, 0, 940, 780), (0, 0, 1920, 1080)),
                None
            );
        }
        assert_eq!(
            hyprland_recording_point((1900, 10, 80, 20), (1800, 0, 940, 780), (0, 0, 1920, 1080)),
            None
        );
    }

    #[test]
    fn hyprland_pixel_marker_keeps_fractional_output_coordinates() {
        assert_eq!(
            hyprland_output_point((130.5, 250.25), (100, 200, 940, 780), (0, 0, 1920, 1080)),
            Some((130.5, 250.25))
        );
        for point in [(f64::NAN, 200.0), (100.0, f64::INFINITY), (99.0, 200.0)] {
            assert_eq!(
                hyprland_output_point(point, (100, 200, 940, 780), (0, 0, 1920, 1080)),
                None
            );
        }
    }
}
