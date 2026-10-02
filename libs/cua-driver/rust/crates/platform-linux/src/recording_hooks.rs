//! Application-state snapshots used by trajectory recording on Linux.

#[cfg(target_os = "linux")]
use cua_driver_core::recording_frame::{
    RecordingFrame, RecordingTurnContext, RecordingUnavailable, RecordingWindow,
    SharedRecordingTurn,
};

/// Pins geometry before any image/point query. Pixel markers come from the
/// action worker's explicit post-conversion handoff, not another scale lookup.
#[cfg(target_os = "linux")]
pub fn hyprland_turn_context(
    args: &serde_json::Value,
    window_id: Option<u64>,
    pid: Option<i64>,
    binding: cua_driver_core::capture_runtime::CaptureBinding,
    binding_current: impl Fn() -> bool + Send + Sync + 'static,
) -> Result<SharedRecordingTurn, RecordingUnavailable> {
    use cua_driver_core::tool_args::ArgsExt;
    // Discovery runs inside the budgeted preparation worker. Do not use the
    // legacy element callback synchronously before acquiring the runtime slot.
    let window_id = window_id.or_else(|| {
        let pid = i32::try_from(pid?).ok()?;
        let store = cua_driver_core::snapshot_store::current_runtime_store::<
            crate::atspi::snapshot::AtspiSnapshot,
        >()?;
        let (_, window, _) = store.resolve(pid, args).ok()?.into_parts(None);
        window
    });
    let plan = crate::wayland::hyprland::desktop_capture_plan().map_err(|error| {
        if error
            .downcast_ref::<crate::wayland::hyprland::UnsupportedDisplayTransform>()
            .is_some()
        {
            RecordingUnavailable::UnqualifiedFrame
        } else {
            RecordingUnavailable::StaleFrame
        }
    })?;
    let native = plan.frame;
    let window = match (pid, window_id) {
        (Some(pid), Some(address)) => {
            let pid = u32::try_from(pid).map_err(|_| RecordingUnavailable::InvalidFrame)?;
            let window = crate::wayland::hyprland::window_for_address(address)
                .filter(|w| w.pid == pid && w.visible)
                .ok_or(RecordingUnavailable::StaleFrame)?;
            Some(RecordingWindow {
                pid,
                window_id: address,
                origin: (window.x, window.y),
                dimensions: (window.width, window.height),
            })
        }
        (Some(pid), None) => {
            let pid = u32::try_from(pid).map_err(|_| RecordingUnavailable::InvalidFrame)?;
            let windows: Vec<_> = crate::wayland::hyprland::list_windows()
                .map_err(|_| RecordingUnavailable::StaleFrame)?
                .into_iter()
                .filter(|w| w.pid == pid && w.visible)
                .collect();
            if windows.len() == 1 {
                let window = &windows[0];
                Some(RecordingWindow {
                    pid,
                    window_id: window.address,
                    origin: (window.x, window.y),
                    dimensions: (window.width, window.height),
                })
            } else {
                None
            }
        }
        (None, None) => None,
        _ => return Err(RecordingUnavailable::InvalidFrame),
    };
    let frame = RecordingFrame {
        binding: Some(binding),
        identity: Some(native.identity.clone()),
        origin: (native.x, native.y),
        dimensions: (native.width, native.height),
        window,
    };
    frame.validate()?;
    let mut context = HyprlandRecordingTurn {
        native,
        composite: plan.composite,
        frame,
        point: Ok(None),
        binding_current: Box::new(binding_current),
    };
    if args.get("capture_id").is_none() {
        context.point = match (args.opt_f64("x"), args.opt_f64("y")) {
            (Some(_), Some(_)) => Ok(None),
            _ => {
                // Element resolution uses the same pinned window bounds and
                // frame; do not invoke the legacy point hook with a new frame.
                Ok(element_point_for_turn(&context, args))
            }
        };
    }
    context.validate()?;
    Ok(std::sync::Arc::new(context))
}

#[cfg(target_os = "linux")]
struct HyprlandRecordingTurn {
    native: crate::wayland::hyprland::DesktopFrame,
    composite: bool,
    frame: RecordingFrame,
    point: Result<Option<(f64, f64)>, RecordingUnavailable>,
    binding_current: Box<dyn Fn() -> bool + Send + Sync>,
}

#[cfg(target_os = "linux")]
impl HyprlandRecordingTurn {
    fn validate(&self) -> Result<(), RecordingUnavailable> {
        if !(self.binding_current)() {
            return Err(RecordingUnavailable::StaleFrame);
        }
        self.native
            .validate_current()
            .map_err(|_| RecordingUnavailable::StaleFrame)?;
        if let Some(window) = &self.frame.window {
            let live = crate::wayland::hyprland::window_for_address(window.window_id)
                .ok_or(RecordingUnavailable::StaleFrame)?;
            if live.pid != window.pid
                || !live.visible
                || (live.x, live.y) != window.origin
                || (live.width, live.height) != window.dimensions
            {
                return Err(RecordingUnavailable::StaleFrame);
            }
        }
        if !(self.binding_current)() {
            return Err(RecordingUnavailable::StaleFrame);
        }
        Ok(())
    }

    fn window_point(&self, x: f64, y: f64) -> Result<(f64, f64), RecordingUnavailable> {
        let window = self
            .frame
            .window
            .as_ref()
            .ok_or(RecordingUnavailable::InvalidFrame)?;
        if x < 0.0
            || y < 0.0
            || x >= f64::from(window.dimensions.0)
            || y >= f64::from(window.dimensions.1)
        {
            return Err(RecordingUnavailable::InvalidFrame);
        }
        self.frame.layout_point(
            f64::from(window.origin.0) + x,
            f64::from(window.origin.1) + y,
        )
    }
}

#[cfg(target_os = "linux")]
impl RecordingTurnContext for HyprlandRecordingTurn {
    fn frame(&self) -> &RecordingFrame {
        &self.frame
    }

    fn capture(
        &self,
        deadline: std::time::Instant,
    ) -> cua_driver_core::recording::ScreenshotCapture {
        use cua_driver_core::recording::ScreenshotCapture;
        let capture = || -> Result<RecordingPixels, RecordingUnavailable> {
            if std::time::Instant::now() >= deadline {
                return Err(RecordingUnavailable::BudgetExceeded);
            }
            self.validate()?;
            // Uses this turn's exact plan, never a newly observed DesktopFrame.
            let png = capture_recording_pixels(
                &self.native,
                self.composite,
                deadline,
                || {
                    recording_native_or_fallback(&self.native, deadline, || {
                        single_output_recording_capture(deadline)
                    })
                },
                |frame| {
                    recording_native_or_fallback(frame, deadline, || {
                        crate::wayland::hyprland::compose_desktop_capture_until(frame, deadline)
                            .map(RecordingPixels::unqualified)
                    })
                },
            )
            .map_err(|error| {
                if error.to_string().contains("timeout") || error.to_string().contains("budget") {
                    RecordingUnavailable::BudgetExceeded
                } else if error.to_string() == RecordingUnavailable::InvalidFrame.classification() {
                    RecordingUnavailable::InvalidFrame
                } else {
                    RecordingUnavailable::CaptureFailed
                }
            })?;
            if std::time::Instant::now() >= deadline {
                return Err(RecordingUnavailable::BudgetExceeded);
            }
            self.validate()?;
            Ok(png)
        };
        match capture() {
            Ok(pixels) if pixels.frame_identity.as_ref() == self.frame.identity.as_ref() => {
                ScreenshotCapture::captured_in_frame(pixels.png)
            }
            Ok(pixels) => ScreenshotCapture::unqualified(pixels.png),
            Err(error) => ScreenshotCapture::unavailable(error.classification()),
        }
    }

    fn click_point(&self) -> Result<Option<(f64, f64)>, RecordingUnavailable> {
        self.point?
            .map(|point| self.validate_point(point))
            .transpose()
    }

    fn validate_authority(&self) -> Result<(), RecordingUnavailable> {
        self.validate()
    }

    fn validate_point(&self, point: (f64, f64)) -> Result<(f64, f64), RecordingUnavailable> {
        self.validate()?;
        self.native
            .checked_layout_point(point.0, point.1)
            .map_err(|_| RecordingUnavailable::InvalidFrame)?;
        self.frame.image_point(point.0, point.1)
    }

    fn cursor_point(&self) -> Result<Option<(f64, f64)>, RecordingUnavailable> {
        self.validate()?;
        let (x, y) = crate::wayland::hyprland::cursor_position()
            .map_err(|_| RecordingUnavailable::CaptureFailed)?;
        let point = self.frame.layout_point(x, y)?;
        self.validate_point(point).map(Some)
    }
}

/// Backend selection is made from the pinned plan. Single-output recording
/// preserves the native screencopy/ext-copy/portal/X11 cascade (no grim
/// dependency); its actual qualified buffer is normalized to logical pixels.
#[cfg(target_os = "linux")]
fn capture_recording_pixels(
    frame: &crate::wayland::hyprland::DesktopFrame,
    composite_required: bool,
    deadline: std::time::Instant,
    single: impl FnOnce() -> anyhow::Result<RecordingPixels>,
    composite: impl FnOnce(&crate::wayland::hyprland::DesktopFrame) -> anyhow::Result<RecordingPixels>,
) -> anyhow::Result<RecordingPixels> {
    anyhow::ensure!(
        std::time::Instant::now() < deadline,
        "recording capture timeout"
    );
    anyhow::ensure!(
        frame.outputs.iter().all(|output| output.native_width > 0
            && output.native_height > 0
            && u64::from(output.native_width) * u64::from(output.native_height)
                <= cua_driver_core::recording_frame::MAX_RECORDING_PIXELS),
        "recording native allocation budget exceeded"
    );
    let mut pixels = if !composite_required {
        single()?
    } else {
        composite(frame)?
    };
    anyhow::ensure!(
        std::time::Instant::now() < deadline,
        "recording capture timeout"
    );
    let dimensions = cua_driver_core::recording_frame::validate_recording_png(&pixels.png)
        .map_err(|error| anyhow::anyhow!(error.classification()))?;
    // Unknown/wrong-peer source remains raw legacy pixels. Shape cannot confer
    // provenance or authorize normalization into a compositor coordinate frame.
    if pixels.frame_identity.as_ref() != Some(&frame.identity) {
        pixels.frame_identity = None;
        return Ok(pixels);
    }
    let png = pixels.png;
    let logical = (frame.width, frame.height);
    anyhow::ensure!(
        png.len() <= cua_driver_core::recording_frame::MAX_RECORDING_PNG_BYTES,
        "recording capture byte budget exceeded"
    );
    if dimensions == logical {
        return Ok(RecordingPixels::qualified(png, frame.identity.clone()));
    }
    let output = frame
        .outputs
        .first()
        .filter(|_| frame.outputs.len() == 1)
        .ok_or_else(|| anyhow::anyhow!("recording composite dimensions mismatch"))?;
    anyhow::ensure!(
        dimensions == (output.native_width, output.native_height),
        "recording native buffer does not match pinned output mode"
    );
    let mut limits = image::Limits::default();
    limits.max_image_width = Some(output.native_width);
    limits.max_image_height = Some(output.native_height);
    limits.max_alloc = Some(cua_driver_core::recording_frame::MAX_RECORDING_PIXELS * 4);
    anyhow::ensure!(
        u64::from(dimensions.0) * u64::from(dimensions.1)
            <= cua_driver_core::recording_frame::MAX_RECORDING_PIXELS,
        "recording native allocation budget exceeded"
    );
    let decoder = image::codecs::png::PngDecoder::with_limits(std::io::Cursor::new(&png), limits)?;
    let image = image::DynamicImage::from_decoder(decoder)?;
    anyhow::ensure!(
        std::time::Instant::now() < deadline,
        "recording capture timeout"
    );
    let normalized =
        image.resize_exact(logical.0, logical.1, image::imageops::FilterType::Lanczos3);
    anyhow::ensure!(
        std::time::Instant::now() < deadline,
        "recording capture timeout"
    );
    let mut encoded = std::io::Cursor::new(Vec::new());
    normalized.write_to(&mut encoded, image::ImageFormat::Png)?;
    anyhow::ensure!(
        std::time::Instant::now() < deadline,
        "recording capture timeout"
    );
    Ok(RecordingPixels::qualified(
        encoded.into_inner(),
        frame.identity.clone(),
    ))
}

/// Native capture provenance supplied by a provider that actually verifies its
/// capture connection/output. Metadata/header equality alone is NOT that proof.
#[cfg(target_os = "linux")]
#[derive(Debug)]
pub struct RecordingPixels {
    pub png: Vec<u8>,
    pub frame_identity: Option<cua_driver_core::capture_runtime::CaptureFrameIdentity>,
}

// The only qualification bridge uses the proof returned by the actual owned
// in-process screencopy operation. All legacy cascades remain unqualified.
#[cfg(target_os = "linux")]
fn recording_native_or_fallback(
    frame: &crate::wayland::hyprland::DesktopFrame,
    deadline: std::time::Instant,
    fallback: impl FnOnce() -> anyhow::Result<RecordingPixels>,
) -> anyhow::Result<RecordingPixels> {
    match crate::wayland::hyprland::capture_native_desktop_until(frame, deadline) {
        Ok(proof) => {
            let (png, identity) = proof.into_parts();
            Ok(RecordingPixels::qualified(png, identity))
        }
        Err(_) => {
            anyhow::ensure!(
                std::time::Instant::now() < deadline,
                "recording capture timeout"
            );
            frame.validate_current()?;
            fallback()
        }
    }
}

#[cfg(target_os = "linux")]
impl RecordingPixels {
    pub fn unqualified(png: Vec<u8>) -> Self {
        Self {
            png,
            frame_identity: None,
        }
    }
    /// Caller must carry native capture connection/output attestation from the
    /// same operation. Do not call for generic cascade, portal, X11 or grim
    /// bytes just because another metadata connection has matching geometry.
    pub fn qualified(
        png: Vec<u8>,
        identity: cua_driver_core::capture_runtime::CaptureFrameIdentity,
    ) -> Self {
        Self {
            png,
            frame_identity: Some(identity),
        }
    }
}

#[cfg(target_os = "linux")]
fn single_output_recording_capture(
    deadline: std::time::Instant,
) -> anyhow::Result<RecordingPixels> {
    // Preserve the baseline backend order without screenshot_display_dispatch's
    // independent topology query/composite selection. The pinned capture plan
    // establishes that exactly one output was present, including standby.
    anyhow::ensure!(
        std::time::Instant::now() < deadline,
        "recording capture timeout"
    );
    if crate::wayland::shell_helper::available() {
        return crate::wayland::shell_helper::screenshot_display()
            .map(RecordingPixels::unqualified)
            .ok_or_else(|| anyhow::anyhow!("shell helper recording capture failed"));
    }
    if let Ok(png) = crate::wayland::screenshot_bytes() {
        return Ok(RecordingPixels::unqualified(png));
    }
    anyhow::ensure!(
        std::time::Instant::now() < deadline,
        "recording capture timeout"
    );
    if let Ok(png) = crate::wayland::ext_screencopy::screenshot_via_ext_copy() {
        return Ok(RecordingPixels::unqualified(png));
    }
    anyhow::ensure!(
        std::time::Instant::now() < deadline,
        "recording capture timeout"
    );
    if let Ok(png) = crate::wayland::portal_screenshot::screenshot_via_portal() {
        return Ok(RecordingPixels::unqualified(png));
    }
    anyhow::ensure!(
        std::time::Instant::now() < deadline,
        "recording capture timeout"
    );
    crate::capture::screenshot_display_bytes_x11().map(RecordingPixels::unqualified)
}

#[cfg(target_os = "linux")]
fn element_point_for_turn(
    context: &HyprlandRecordingTurn,
    args: &serde_json::Value,
) -> Option<(f64, f64)> {
    let window = context.frame.window.as_ref()?;
    let store = cua_driver_core::snapshot_store::current_runtime_store::<
        crate::atspi::snapshot::AtspiSnapshot,
    >()?;
    let (index, resolved_window, _) = store
        .resolve(i32::try_from(window.pid).ok()?, args)
        .ok()?
        .into_parts(None);
    if resolved_window != Some(window.window_id) {
        return None;
    }
    let index = index?;
    let pid = window.pid;
    let address = window.window_id;
    let bounds = move || crate::atspi::get_element_bounds_for_window(pid, address, index).ok();
    let (x, y, w, h) = if tokio::runtime::Handle::try_current().is_ok() {
        std::thread::spawn(bounds).join().ok().flatten()?
    } else {
        bounds()?
    };
    if w == 0 || h == 0 {
        return None;
    }
    context
        .window_point(
            f64::from(x) - f64::from(window.origin.0) + f64::from(w) / 2.0,
            f64::from(y) - f64::from(window.origin.1) + f64::from(h) / 2.0,
        )
        .ok()
}

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
pub fn classified_screenshot_for_recording(
    window_id: Option<u64>,
    pid: Option<i64>,
) -> cua_driver_core::recording::ScreenshotCapture {
    use cua_driver_core::recording::ScreenshotCapture;
    screenshot_for_recording(window_id, pid)
        .map(ScreenshotCapture::captured)
        .unwrap_or_else(|| ScreenshotCapture::unavailable("capture_failed"))
}

#[cfg(target_os = "linux")]
pub fn screenshot_for_recording(window_id: Option<u64>, pid: Option<i64>) -> Option<Vec<u8>> {
    if crate::wayland::is_wayland() {
        if crate::wayland::hyprland::is_session() {
            // Recording uses the same qualified logical canvas even with one
            // scaled output. Raw physical pixels cannot share logical markers.
            match crate::wayland::hyprland::desktop_capture_plan() {
                Ok(plan) => {
                    let frame = plan.frame;
                    frame.validate_current().ok()?;
                    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(12);
                    let png = capture_recording_pixels(
                        &frame,
                        plan.composite,
                        deadline,
                        || {
                            recording_native_or_fallback(&frame, deadline, || {
                                single_output_recording_capture(deadline)
                            })
                        },
                        |frame| {
                            recording_native_or_fallback(frame, deadline, || {
                                crate::wayland::hyprland::compose_desktop_capture_until(
                                    frame, deadline,
                                )
                                .map(RecordingPixels::unqualified)
                            })
                        },
                    )
                    .ok()?;
                    frame.validate_current().ok()?;
                    return Some(png.png);
                }
                Err(error)
                    if error
                        .downcast_ref::<crate::wayland::hyprland::UnsupportedDisplayTransform>()
                        .is_some() =>
                {
                    // Single rotated outputs retain baseline rendered pixels.
                    // This path has no qualified coordinate/marker authority.
                    return crate::wayland::screenshot_display_dispatch().ok();
                }
                Err(_) => return None,
            }
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
    fn single_output_recording_needs_no_grim_and_normalizes_the_actual_pinned_buffer() {
        use crate::wayland::hyprland::{DesktopFrame, FrameOutput};
        use cua_driver_core::recording_frame::RecordingFrame;
        let frame = DesktopFrame {
            peer: None,
            x: -8,
            y: 0,
            width: 8,
            height: 6,
            scale: 1.5,
            outputs: vec![FrameOutput {
                name: "one-scaled-output".into(),
                x: -8,
                y: 0,
                width: 8,
                height: 6,
                native_width: 12,
                native_height: 9,
            }],
            identity: cua_driver_core::capture_runtime::CaptureFrameIdentity::from_native_snapshot(
                b"one-output",
            ),
        };
        let png = |w, h| {
            cua_driver_core::image_utils::encode_rgba_to_png(&vec![40; (w * h * 4) as usize], w, h)
                .unwrap()
        };
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
        let qualified = |w, h| super::RecordingPixels::qualified(png(w, h), frame.identity.clone());
        let captured = super::capture_recording_pixels(
            &frame,
            false,
            deadline,
            || Ok(qualified(12, 9)),
            |_| panic!("single output must not invoke grim composition"),
        )
        .unwrap();
        assert_eq!(
            cua_driver_core::image_utils::png_dimensions(&captured.png).unwrap(),
            (8, 6)
        );
        let recording = RecordingFrame {
            binding: None,
            identity: Some(frame.identity.clone()),
            origin: (-8, 0),
            dimensions: (8, 6),
            window: None,
        };
        let marker = recording.layout_point(-2.0, 2.0).unwrap();
        assert_eq!(marker, (6.0, 2.0));
        recording.check_png(&captured.png).unwrap();
        let marked =
            crate::capture::crosshair_png_bytes(&captured.png, marker.0, marker.1).unwrap();
        let pixels = image::load_from_memory(&marked).unwrap().to_rgba8();
        assert_ne!(pixels.get_pixel(6, 2).0, [40; 4]);
        let error = super::capture_recording_pixels(
            &frame,
            false,
            deadline,
            || Ok(qualified(16, 12)),
            |_| panic!("no composite"),
        )
        .unwrap_err();
        assert_eq!(
            error.to_string(),
            "recording native buffer does not match pinned output mode"
        );
        let error = super::capture_recording_pixels(
            &frame,
            false,
            std::time::Instant::now(),
            || panic!("expired budget cannot capture"),
            |_| panic!("expired budget cannot compose"),
        )
        .unwrap_err();
        assert_eq!(error.to_string(), "recording capture timeout");
        let logical = super::capture_recording_pixels(
            &frame,
            false,
            deadline,
            || Ok(qualified(8, 6)),
            |_| panic!("no composite"),
        )
        .unwrap();
        assert_eq!(logical.png, png(8, 6));
        // A standby output still makes the plan composite even with only one
        // powered tile. Never choose a first-output cascade by tile count.
        let composed = super::capture_recording_pixels(
            &frame,
            true,
            deadline,
            || panic!("standby-inclusive plan requires composition"),
            |pinned| {
                assert!(std::ptr::eq(pinned, &frame));
                Ok(qualified(8, 6))
            },
        )
        .unwrap();
        assert_eq!(composed.png, png(8, 6));
        let mut oversized = frame.clone();
        oversized.outputs[0].native_width = u32::MAX;
        let error = super::capture_recording_pixels(
            &oversized,
            false,
            deadline,
            || panic!("allocation budget must be checked before source capture"),
            |_| panic!("allocation budget must be checked before composition"),
        )
        .unwrap_err();
        assert_eq!(
            error.to_string(),
            "recording native allocation budget exceeded"
        );
        // An unrelated X server/portal with equal dimensions is not an output
        // proof. Even a differently sized raw fallback remains legacy pixels.
        for dimensions in [(8, 6), (12, 9)] {
            let raw = super::capture_recording_pixels(
                &frame,
                false,
                deadline,
                || {
                    Ok(super::RecordingPixels::unqualified(png(
                        dimensions.0,
                        dimensions.1,
                    )))
                },
                |_| panic!("no forced grim"),
            )
            .unwrap();
            assert!(raw.frame_identity.is_none());
            assert_eq!(raw.png, png(dimensions.0, dimensions.1));
        }
        let invalid = png(8, 6)[..24].to_vec();
        let error = super::capture_recording_pixels(
            &frame,
            false,
            deadline,
            || {
                Ok(super::RecordingPixels::qualified(
                    invalid,
                    frame.identity.clone(),
                ))
            },
            |_| panic!("no composite"),
        )
        .unwrap_err();
        assert_eq!(error.to_string(), "recording_invalid_frame");
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
