use cua_driver_core::capture_runtime::{
    CaptureActionRequest, CaptureFrameIdentity, CapturePublication, CaptureService, CaptureTarget,
    EncodedScreenshotDimensions, NativeActionDimensions, ScreenshotToActionTransform,
};
use serde_json::Value;

thread_local! {
    static WORKER_RECORDING_SINK: std::cell::RefCell<Option<cua_driver_core::recording::RecordingAdmissionSink>> = const { std::cell::RefCell::new(None) };
}

pub(crate) fn recording_sink() -> Option<cua_driver_core::recording::RecordingAdmissionSink> {
    cua_driver_core::recording::recording_admission_sink()
        .or_else(|| WORKER_RECORDING_SINK.with(|slot| slot.borrow().clone()))
}

pub(crate) fn with_recording_sink<T>(
    sink: Option<cua_driver_core::recording::RecordingAdmissionSink>,
    work: impl FnOnce() -> T,
) -> T {
    struct Restore(Option<cua_driver_core::recording::RecordingAdmissionSink>);
    impl Drop for Restore {
        fn drop(&mut self) {
            WORKER_RECORDING_SINK.with(|slot| {
                slot.replace(self.0.take());
            });
        }
    }
    let _restore = Restore(WORKER_RECORDING_SINK.with(|slot| slot.replace(sink)));
    work()
}

fn window_target(pid: u32, window_id: u64) -> CaptureTarget {
    CaptureTarget::Window { pid, window_id }
}

/// Called before any capture work, never reconstructed after pixels arrive.
pub fn pin_publication_binding(service: &CaptureService, args: &Value) -> anyhow::Result<Value> {
    pin_publication_binding_for_target(service, args, CaptureTarget::PrimaryDesktop)
}

pub fn pin_publication_binding_for_target(
    service: &CaptureService,
    args: &Value,
    target: CaptureTarget,
) -> anyhow::Result<Value> {
    let binding = service.binding_from_args(args)?;
    if target == CaptureTarget::PrimaryDesktop
        && args
            .get("_capture_started_sequence")
            .and_then(Value::as_u64)
            .is_some()
        && args
            .get("_capture_session_generation")
            .and_then(Value::as_u64)
            == Some(binding.session_generation())
    {
        return Ok(args.clone());
    }
    let sequence = service.begin_publication(&binding, target)?;
    let mut args = args.clone();
    args["_capture_session_generation"] = serde_json::json!(binding.session_generation());
    args["_capture_started_sequence"] = serde_json::json!(sequence);
    Ok(args)
}

#[allow(clippy::too_many_arguments)]
fn publish(
    service: &CaptureService,
    args: &Value,
    png_bytes: &[u8],
    target: CaptureTarget,
    encoded_dimensions: (u32, u32),
    native_action_dimensions: (u32, u32),
    screenshot_to_action: ScreenshotToActionTransform,
    frame_identity: Option<CaptureFrameIdentity>,
) -> anyhow::Result<String> {
    let binding = if let Some(generation) = args
        .get("_capture_session_generation")
        .and_then(Value::as_u64)
    {
        service.binding(
            args.get("_session_id")
                .and_then(Value::as_str)
                .ok_or_else(|| anyhow::anyhow!("capture session unavailable"))?,
            generation,
        )?
    } else {
        service.binding_from_args(args)?
    };
    let capture_id = service.publish_started_in_frame(
        CapturePublication {
            png_bytes: png_bytes.to_vec(),
            target,
            encoded_dimensions: EncodedScreenshotDimensions::new(
                encoded_dimensions.0,
                encoded_dimensions.1,
            )?,
            native_action_dimensions: NativeActionDimensions::new(
                native_action_dimensions.0,
                native_action_dimensions.1,
            )?,
            screenshot_to_action,
            session_id: binding.session_id().into(),
            session_generation: binding.session_generation(),
        },
        frame_identity,
        args.get("_capture_started_sequence")
            .and_then(Value::as_u64),
    )?;
    Ok(capture_id.to_string())
}

#[cfg(test)]
pub fn publish_window(
    service: &CaptureService,
    args: &Value,
    png_bytes: &[u8],
    pid: u32,
    window_id: u64,
    encoded_dimensions: (u32, u32),
    native_action_dimensions: (u32, u32),
) -> anyhow::Result<String> {
    publish_window_in_frame(
        service,
        args,
        png_bytes,
        pid,
        window_id,
        encoded_dimensions,
        native_action_dimensions,
        None,
    )
}

#[allow(clippy::too_many_arguments)]
pub fn publish_window_in_frame(
    service: &CaptureService,
    args: &Value,
    png_bytes: &[u8],
    pid: u32,
    window_id: u64,
    encoded_dimensions: (u32, u32),
    native_action_dimensions: (u32, u32),
    frame_identity: Option<CaptureFrameIdentity>,
) -> anyhow::Result<String> {
    publish(
        service,
        args,
        png_bytes,
        window_target(pid, window_id),
        encoded_dimensions,
        native_action_dimensions,
        scaled_transform(encoded_dimensions, native_action_dimensions)?,
        frame_identity,
    )
}

#[cfg(test)]
pub fn publish_desktop(
    service: &CaptureService,
    args: &Value,
    png_bytes: &[u8],
    encoded_dimensions: (u32, u32),
    native_action_dimensions: (u32, u32),
) -> anyhow::Result<String> {
    publish_desktop_in_frame(
        service,
        args,
        png_bytes,
        encoded_dimensions,
        native_action_dimensions,
        None,
    )
}

pub fn publish_desktop_in_frame(
    service: &CaptureService,
    args: &Value,
    png_bytes: &[u8],
    encoded_dimensions: (u32, u32),
    native_action_dimensions: (u32, u32),
    frame_identity: Option<CaptureFrameIdentity>,
) -> anyhow::Result<String> {
    // The desktop screenshot can be downsized below the action frame.
    publish(
        service,
        args,
        png_bytes,
        CaptureTarget::PrimaryDesktop,
        encoded_dimensions,
        native_action_dimensions,
        scaled_transform(encoded_dimensions, native_action_dimensions)?,
        frame_identity,
    )
}

/// Screenshot-to-action scaling for a capture encoded at `encoded` pixels of
/// a `native` action frame. The resizer preserves aspect ratio before rounding
/// each encoded axis, so both ratios are derived independently: a one-pixel
/// rounded height must not skew Y coordinates.
fn scaled_transform(
    encoded: (u32, u32),
    native: (u32, u32),
) -> anyhow::Result<ScreenshotToActionTransform> {
    anyhow::ensure!(
        encoded.0 > 0 && encoded.1 > 0,
        "capture has an empty encoded frame: {}x{}",
        encoded.0,
        encoded.1
    );
    let scale_x = f64::from(native.0) / f64::from(encoded.0);
    let scale_y = f64::from(native.1) / f64::from(encoded.1);
    Ok(ScreenshotToActionTransform::new(
        scale_x, 0.0, 0.0, scale_y, 0.0, 0.0,
    )?)
}

fn admit(
    service: &CaptureService,
    args: &Value,
    capture_id: &str,
    target: CaptureTarget,
    screenshot_x: f64,
    screenshot_y: f64,
) -> anyhow::Result<(f64, f64)> {
    // This function runs in the invocation future. Any future worker handoff
    // must carry this handle and binding explicitly; neither may be reacquired
    // after observation/consumption under a successor session generation.
    let recording_sink = recording_sink();
    let source_binding = service.binding_from_args(args)?;
    if crate::wayland::is_wayland() && crate::wayland::hyprland::is_session() {
        let (dimensions, identity, desktop_frame) = match &target {
            CaptureTarget::Window { pid, window_id } => {
                let frame = crate::wayland::hyprland::local_window_frame(*pid, *window_id)?;
                ((frame.width, frame.height), frame.identity, None)
            }
            CaptureTarget::PrimaryDesktop => {
                let frame = crate::wayland::hyprland::desktop_frame()?;
                (
                    (frame.width, frame.height),
                    frame.identity.clone(),
                    Some(frame),
                )
            }
        };
        let admission = service.admit_action_checked(
            CaptureActionRequest {
                capture_id: service.parse_capture_id(capture_id)?,
                binding: source_binding.clone(),
                target,
                current_native_action_dimensions: NativeActionDimensions::new(
                    dimensions.0,
                    dimensions.1,
                )?,
                screenshot_x,
                screenshot_y,
            },
            Some(&identity),
            |x, y| {
                desktop_frame
                    .as_ref()
                    .is_none_or(|frame| frame.checked_layout_point(x, y).is_ok())
            },
        )?;
        if let Some(source) = crate::wayland::hyprland_input::current_window_source() {
            source.retain_admission(&source_binding, &admission);
        }
        if admission.target == CaptureTarget::PrimaryDesktop {
            if let Some(sink) = &recording_sink {
                sink.retain(&source_binding, &admission);
            }
        }
        // Client-local identity is not global recording authority. Until an
        // exact origin/root/revision guard is retained across dispatch, leave
        // this marker explicitly unavailable rather than poisoning the sink
        // with a local digest and attempting a later repair.
        return Ok((admission.action_x, admission.action_y));
    }
    let current_native_action_dimensions = live_action_dimensions(&target)?;
    let admission = service.admit_action(CaptureActionRequest {
        capture_id: service.parse_capture_id(capture_id)?,
        binding: source_binding.clone(),
        target,
        current_native_action_dimensions,
        screenshot_x,
        screenshot_y,
    })?;
    if let Some(sink) = &recording_sink {
        sink.retain(&source_binding, &admission);
    }
    Ok((admission.action_x, admission.action_y))
}

#[cfg(test)]
fn admit_with_live_dimensions(
    service: &CaptureService,
    args: &Value,
    capture_id: &str,
    target: CaptureTarget,
    current_native_action_dimensions: NativeActionDimensions,
    screenshot_x: f64,
    screenshot_y: f64,
) -> anyhow::Result<(f64, f64)> {
    let binding = service.binding_from_args(args)?;
    let capture_id = service.parse_capture_id(capture_id)?;
    let admission = service.admit_action(CaptureActionRequest {
        capture_id,
        binding,
        target,
        current_native_action_dimensions,
        screenshot_x,
        screenshot_y,
    })?;
    Ok((admission.action_x, admission.action_y))
}

#[cfg(target_os = "linux")]
fn live_action_dimensions(target: &CaptureTarget) -> anyhow::Result<NativeActionDimensions> {
    let dimensions = match target {
        CaptureTarget::Window { pid, window_id } => {
            let identity_matches = if crate::wayland::is_wayland() {
                crate::wayland::window_was_listed_for_pid(*pid, *window_id)
            } else {
                crate::x11::window_belongs_to_pid(*window_id, *pid)
            };
            anyhow::ensure!(
                identity_matches,
                "native window identity changed after capture"
            );
            let png = crate::wayland::screenshot_dispatch_with_pid(*window_id, *pid)?;
            crate::capture::png_dimensions_pub(&png)?
        }
        CaptureTarget::PrimaryDesktop => {
            if crate::wayland::is_wayland() && crate::wayland::hyprland::is_session() {
                let frame = crate::wayland::hyprland::desktop_frame()?;
                return Ok(NativeActionDimensions::new(frame.width, frame.height)?);
            }
            let png = crate::capture::screenshot_display_bytes()?;
            let native = crate::capture::png_dimensions_pub(&png)?;
            desktop_action_dimensions(native)?
        }
    };
    Ok(NativeActionDimensions::new(dimensions.0, dimensions.1)?)
}

#[cfg(not(target_os = "linux"))]
fn live_action_dimensions(_target: &CaptureTarget) -> anyhow::Result<NativeActionDimensions> {
    anyhow::bail!("live Linux capture validation is unavailable on this platform")
}

#[cfg(target_os = "linux")]
pub(crate) fn desktop_action_dimensions(native: (u32, u32)) -> anyhow::Result<(u32, u32)> {
    let logical = if crate::wayland::is_wayland() {
        crate::wayland::compositor_logical_frame().transpose()?
    } else {
        None
    };
    select_desktop_action_dimensions(native, logical)
}

fn select_desktop_action_dimensions(
    native: (u32, u32),
    compositor_logical: Option<(u32, u32)>,
) -> anyhow::Result<(u32, u32)> {
    let dimensions = compositor_logical.unwrap_or(native);
    anyhow::ensure!(
        dimensions.0 > 0 && dimensions.1 > 0,
        "desktop action frame is empty: {}x{}",
        dimensions.0,
        dimensions.1
    );
    Ok(dimensions)
}

pub fn admit_window_click(
    service: &CaptureService,
    args: &Value,
    capture_id: &str,
    pid: u32,
    window_id: u64,
    screenshot_x: f64,
    screenshot_y: f64,
) -> anyhow::Result<(f64, f64)> {
    admit(
        service,
        args,
        capture_id,
        window_target(pid, window_id),
        screenshot_x,
        screenshot_y,
    )
}

pub fn admit_desktop_click(
    service: &CaptureService,
    args: &Value,
    capture_id: &str,
    screenshot_x: f64,
    screenshot_y: f64,
) -> anyhow::Result<(f64, f64)> {
    admit(
        service,
        args,
        capture_id,
        CaptureTarget::PrimaryDesktop,
        screenshot_x,
        screenshot_y,
    )
}

pub fn admit_desktop_click_in(
    service: &CaptureService,
    args: &Value,
    capture_id: &str,
    screenshot_x: f64,
    screenshot_y: f64,
    space: &crate::wayland::DesktopInputSpace,
) -> anyhow::Result<(f64, f64)> {
    let Some(frame) = space.frame() else {
        return admit_desktop_click(service, args, capture_id, screenshot_x, screenshot_y);
    };
    admit_desktop_click_in_frame(service, args, capture_id, screenshot_x, screenshot_y, frame)
}

pub(crate) fn admit_desktop_click_in_frame(
    service: &CaptureService,
    args: &Value,
    capture_id: &str,
    screenshot_x: f64,
    screenshot_y: f64,
    frame: &crate::wayland::hyprland::DesktopFrame,
) -> anyhow::Result<(f64, f64)> {
    let recording_sink = recording_sink();
    let source_binding = service.binding_from_args(args)?;
    if let Some(source) = crate::wayland::hyprland_input::current_window_source() {
        anyhow::ensure!(
            source.frame.identity == frame.identity,
            "window_source_changed: admission frame differs from retained source"
        );
        source.validate_native()?;
    } else {
        frame.validate_current()?;
    }
    let request = CaptureActionRequest {
        capture_id: service.parse_capture_id(capture_id)?,
        binding: source_binding.clone(),
        target: CaptureTarget::PrimaryDesktop,
        current_native_action_dimensions: NativeActionDimensions::new(frame.width, frame.height)?,
        screenshot_x,
        screenshot_y,
    };
    let admission = service.admit_action_checked(request, Some(&frame.identity), |x, y| {
        frame.checked_layout_point(x, y).is_ok()
    })?;
    if let Some(source) = crate::wayland::hyprland_input::current_window_source() {
        source.retain_admission(&source_binding, &admission);
    }
    if let Some(sink) = &recording_sink {
        sink.retain(&source_binding, &admission);
    }
    Ok((admission.action_x, admission.action_y))
}

pub fn retire_runtime(service: &CaptureService) {
    service.retire_runtime();
}

#[cfg(test)]
mod tests {
    #[test]
    fn actual_publication_adapter_cannot_rebind_a_paused_capture_after_retirement() {
        let service = std::sync::Arc::new(CaptureService::default());
        let call = args("paused-native-adapter");
        let pinned = pin_publication_binding(&service, &call).unwrap();
        let (resume, wait) = std::sync::mpsc::channel();
        let worker_service = service.clone();
        let worker = std::thread::spawn(move || {
            wait.recv().unwrap();
            publish_desktop_in_frame(
                &worker_service,
                &pinned,
                &png(4, 3, 0x42),
                (4, 3),
                (8, 6),
                Some(CaptureFrameIdentity::from_native_snapshot(b"A")),
            )
        });
        service.retire_session(&service.binding_from_args(&call).unwrap());
        resume.send(()).unwrap();
        assert!(worker.join().unwrap().is_err());
    }

    #[test]
    fn reverse_adapter_completion_preserves_one_publication_identity_and_ratio() {
        let service = CaptureService::default();
        let call = args("reverse-native-adapter");
        let old = pin_publication_binding(&service, &call).unwrap();
        let new = pin_publication_binding(&service, &call).unwrap();
        let identity = CaptureFrameIdentity::from_native_snapshot(b"new");
        publish_desktop_in_frame(
            &service,
            &new,
            &png(4, 3, 0x42),
            (4, 3),
            (8, 6),
            Some(identity.clone()),
        )
        .unwrap();
        assert!(publish_desktop_in_frame(
            &service,
            &old,
            &png(2, 2, 0x42),
            (2, 2),
            (8, 6),
            Some(CaptureFrameIdentity::from_native_snapshot(b"old"))
        )
        .is_err());
        let binding = service.binding_from_args(&call).unwrap();
        let mut point = serde_json::json!({"scope":"desktop", "x":1.0, "y":1.0});
        assert!(service.map_desktop_observation_args(&binding, &mut point));
        assert_eq!(point["x"], 2.0);
        assert_eq!(point["y"], 2.0);
        service
            .validate_desktop_observation_source(
                &binding,
                &identity,
                point["_native_desktop_observation"].as_str(),
            )
            .unwrap();
    }
    use super::*;
    use cua_driver_core::capture_runtime::admission_error_code;
    use sha2::{Digest, Sha256};

    fn png(width: u32, height: u32, value: u8) -> Vec<u8> {
        let rgba = vec![value; (width * height * 4) as usize];
        cua_driver_core::image_utils::encode_rgba_to_png(&rgba, width, height)
            .expect("encode fixture")
    }

    fn args(session: &str) -> Value {
        serde_json::json!({"_session_id": session})
    }

    fn admit_window(
        service: &CaptureService,
        args: &Value,
        capture_id: &str,
        pid: u32,
        window_id: u64,
        point: (f64, f64),
        native: (u32, u32),
    ) -> anyhow::Result<(f64, f64)> {
        admit_with_live_dimensions(
            service,
            args,
            capture_id,
            window_target(pid, window_id),
            NativeActionDimensions::new(native.0, native.1).unwrap(),
            point.0,
            point.1,
        )
    }

    fn admit_desktop(
        service: &CaptureService,
        args: &Value,
        capture_id: &str,
        point: (f64, f64),
        native: (u32, u32),
    ) -> anyhow::Result<(f64, f64)> {
        admit_with_live_dimensions(
            service,
            args,
            capture_id,
            CaptureTarget::PrimaryDesktop,
            NativeActionDimensions::new(native.0, native.1).unwrap(),
            point.0,
            point.1,
        )
    }

    #[test]
    fn window_publication_digests_the_exact_returned_png() {
        let service = CaptureService::default();
        let delivered = png(2, 2, 0x41);
        let id = publish_window(
            &service,
            &args("digest"),
            &delivered,
            41,
            99,
            (2, 2),
            (4, 4),
        )
        .expect("publish window capture");
        let binding = service.binding_from_args(&args("digest")).unwrap();
        let capture = service
            .read_for_perception(service.parse_capture_id(&id).unwrap(), &binding)
            .expect("read published capture");

        assert_eq!(capture.png_bytes().as_ref(), delivered.as_slice());
        let expected: [u8; 32] = Sha256::digest(&delivered).into();
        assert_eq!(capture.digest().as_bytes(), &expected);
    }

    #[test]
    fn target_mismatch_does_not_consume_but_successful_admission_does() {
        let service = CaptureService::default();
        let call_args = args("one-action");
        let id = publish_window(
            &service,
            &call_args,
            &png(10, 10, 0x22),
            7,
            11,
            (10, 10),
            (20, 20),
        )
        .unwrap();

        assert!(admit_window(&service, &call_args, &id, 7, 12, (2.0, 3.0), (20, 20)).is_err());
        assert_eq!(
            admit_window(&service, &call_args, &id, 7, 11, (2.0, 3.0), (20, 20)).unwrap(),
            (4.0, 6.0)
        );
        assert!(admit_window(&service, &call_args, &id, 7, 11, (2.0, 3.0), (20, 20)).is_err());
    }

    #[test]
    fn retired_session_capture_is_stale() {
        let service = CaptureService::default();
        let call_args = args("retired");
        let id = publish_desktop(&service, &call_args, &png(3, 2, 0x77), (3, 2), (3, 2)).unwrap();

        let binding = service.binding_from_args(&call_args).unwrap();
        service.retire_session(&binding);

        assert!(admit_desktop(&service, &call_args, &id, (1.0, 1.0), (3, 2)).is_err());
    }

    #[test]
    fn window_transform_preserves_each_rounded_axis() {
        let service = CaptureService::default();
        let call_args = args("rounding");
        let id = publish_window(
            &service,
            &call_args,
            &png(3, 2, 0x33),
            8,
            13,
            (3, 2),
            (5, 3),
        )
        .unwrap();

        let (x, y) = admit_window(&service, &call_args, &id, 8, 13, (1.0, 1.0), (5, 3)).unwrap();
        assert_eq!(x, 5.0 / 3.0);
        assert_eq!(y, 3.0 / 2.0);
    }

    #[test]
    fn another_session_cannot_admit_or_consume_a_capture() {
        let service = CaptureService::default();
        let owner = args("owner");
        let id = publish_desktop(&service, &owner, &png(4, 3, 0x21), (4, 3), (4, 3)).unwrap();

        let refusal = admit_desktop(&service, &args("other"), &id, (2.0, 1.0), (4, 3))
            .expect_err("cross-session admission must fail");
        assert_eq!(
            admission_error_code(&refusal),
            "capture_generation_mismatch"
        );
        assert_eq!(
            admit_desktop(&service, &owner, &id, (2.0, 1.0), (4, 3)).unwrap(),
            (2.0, 1.0)
        );
    }

    #[test]
    fn hidpi_desktop_uses_the_compositors_logical_action_frame() {
        let native = (3200, 2000);
        let logical = select_desktop_action_dimensions(native, Some((1600, 1000))).unwrap();
        assert_eq!(logical, (1600, 1000));

        let service = CaptureService::default();
        let call_args = args("hidpi");
        let id = publish_desktop(
            &service,
            &call_args,
            &png(logical.0, logical.1, 0x31),
            logical,
            logical,
        )
        .unwrap();
        assert_eq!(
            admit_desktop(&service, &call_args, &id, (800.0, 500.0), logical).unwrap(),
            (800.0, 500.0)
        );
    }

    #[test]
    fn downsized_desktop_capture_maps_back_to_the_action_frame() {
        let service = CaptureService::default();
        let call_args = args("downsized");
        let id = publish_desktop(&service, &call_args, &png(4, 3, 0x42), (4, 3), (8, 6)).unwrap();
        assert_eq!(
            admit_desktop(&service, &call_args, &id, (2.0, 1.5), (8, 6)).unwrap(),
            (4.0, 3.0)
        );
    }

    #[test]
    fn desktop_publication_uses_the_post_normalization_bytes() {
        let service = CaptureService::default();
        let normalized = png(4, 3, 0x18);
        let id = publish_desktop(&service, &args("desktop"), &normalized, (4, 3), (4, 3)).unwrap();
        let binding = service.binding_from_args(&args("desktop")).unwrap();
        let capture = service
            .read_for_perception(service.parse_capture_id(&id).unwrap(), &binding)
            .unwrap();

        assert_eq!(capture.png_bytes().as_ref(), normalized.as_slice());
        assert_eq!(
            admit_desktop(&service, &args("desktop"), &id, (2.0, 1.0), (4, 3)).unwrap(),
            (2.0, 1.0)
        );
    }

    #[test]
    fn live_resize_refuses_before_dispatch_without_consuming_capture() {
        let service = CaptureService::default();
        let call_args = args("resize");
        let id = publish_window(
            &service,
            &call_args,
            &png(4, 3, 0x44),
            8,
            13,
            (4, 3),
            (8, 6),
        )
        .unwrap();
        let mut dispatches = 0;
        let refusal = admit_window(&service, &call_args, &id, 8, 13, (1.25, 1.5), (9, 6));
        if refusal.is_ok() {
            dispatches += 1;
        }
        assert_eq!(dispatches, 0);
        assert!(refusal
            .unwrap_err()
            .downcast_ref::<cua_driver_core::capture_runtime::CaptureActionError>()
            .is_some_and(|error| {
                *error
                == cua_driver_core::capture_runtime::CaptureActionError::NativeActionFrameMismatch
            }));
        assert_eq!(
            admit_window(&service, &call_args, &id, 8, 13, (1.25, 1.5), (8, 6),).unwrap(),
            (2.5, 3.0)
        );
    }
}
