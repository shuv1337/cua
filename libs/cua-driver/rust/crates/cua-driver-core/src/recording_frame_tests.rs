use super::*;
use crate::capture_runtime::*;
use crate::recording_frame::*;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

struct Context {
    frame: RecordingFrame,
    png: Vec<u8>,
    live: Arc<AtomicBool>,
    captures: AtomicUsize,
    point: Option<(f64, f64)>,
}

impl RecordingTurnContext for Context {
    fn frame(&self) -> &RecordingFrame {
        &self.frame
    }
    fn capture(&self, deadline: Instant) -> ScreenshotCapture {
        if Instant::now() >= deadline {
            return ScreenshotCapture::unavailable(
                RecordingUnavailable::BudgetExceeded.classification(),
            );
        }
        if !self.live.load(Ordering::Acquire) {
            return ScreenshotCapture::unavailable(
                RecordingUnavailable::StaleFrame.classification(),
            );
        }
        self.captures.fetch_add(1, Ordering::SeqCst);
        ScreenshotCapture::captured(self.png.clone())
    }
    fn click_point(&self) -> Result<Option<(f64, f64)>, RecordingUnavailable> {
        Ok(self.point)
    }
    fn validate_point(&self, point: (f64, f64)) -> Result<(f64, f64), RecordingUnavailable> {
        if !self.live.load(Ordering::Acquire) {
            return Err(RecordingUnavailable::StaleFrame);
        }
        self.frame.image_point(point.0, point.1)
    }
    fn cursor_point(&self) -> Result<Option<(f64, f64)>, RecordingUnavailable> {
        self.validate_point((1.0, 1.0)).map(Some)
    }
}

fn image(w: u32, h: u32) -> Vec<u8> {
    crate::image_utils::encode_rgba_to_png(&vec![0; (w * h * 4) as usize], w, h).unwrap()
}

fn context(service: &CaptureService, name: &str, dimensions: (u32, u32)) -> Arc<Context> {
    Arc::new(Context {
        frame: RecordingFrame {
            binding: Some(service.current_binding(name).unwrap()),
            identity: Some(CaptureFrameIdentity::from_native_snapshot(b"frame-A")),
            origin: (-2560, 0),
            dimensions,
            window: None,
        },
        png: image(dimensions.0, dimensions.1),
        live: Arc::new(AtomicBool::new(true)),
        captures: AtomicUsize::new(0),
        point: Some((2.0, 1.0)),
    })
}

fn admission(service: &CaptureService, context: &Context) -> CaptureActionAdmission {
    let binding = context.frame.binding.clone().unwrap();
    let encoded = (
        context.frame.dimensions.0 / 2,
        context.frame.dimensions.1 / 2,
    );
    let id = service
        .publish_in_frame(
            CapturePublication {
                png_bytes: image(encoded.0, encoded.1),
                target: CaptureTarget::PrimaryDesktop,
                encoded_dimensions: EncodedScreenshotDimensions::new(encoded.0, encoded.1).unwrap(),
                native_action_dimensions: NativeActionDimensions::new(
                    context.frame.dimensions.0,
                    context.frame.dimensions.1,
                )
                .unwrap(),
                screenshot_to_action: ScreenshotToActionTransform::new(
                    2.0, 0.0, 0.0, 2.0, 0.0, 0.0,
                )
                .unwrap(),
                session_id: binding.session_id().into(),
                session_generation: binding.session_generation(),
            },
            context.frame.identity.clone(),
        )
        .unwrap();
    service
        .admit_action_in_frame(
            CaptureActionRequest {
                capture_id: id,
                binding,
                target: CaptureTarget::PrimaryDesktop,
                current_native_action_dimensions: NativeActionDimensions::new(
                    context.frame.dimensions.0,
                    context.frame.dimensions.1,
                )
                .unwrap(),
                screenshot_x: f64::from(encoded.0) * 0.75,
                screenshot_y: f64::from(encoded.1) * 0.5,
            },
            context.frame.identity.as_ref(),
        )
        .unwrap()
}

fn session(context: Arc<Context>, output: &Path) -> RecordingSession {
    let session = RecordingSession::new();
    session.set_turn_context_fn(move |args, _, _| {
        assert_eq!(
            args["_session_id"],
            context.frame.binding.as_ref().unwrap().session_id()
        );
        Ok(context.clone())
    });
    session
        .start_with_state_budget(output.to_str().unwrap(), false, None, None)
        .unwrap();
    session
}

fn read(root: &Path, file: &str) -> Value {
    serde_json::from_slice(&std::fs::read(root.join(file)).unwrap()).unwrap()
}

#[tokio::test]
async fn real_consumed_admission_binds_image_marker_cursor_and_after_to_one_turn() {
    let service = CaptureService::new(Default::default()).unwrap();
    let context = context(&service, "recording-session", (5120, 1440));
    let admitted = admission(&service, &context);
    assert_eq!((admitted.action_x, admitted.action_y), (3840.0, 720.0));
    // The id is consumed already; evidence cannot obtain authority by looking it up.
    assert!(service
        .read_for_perception(admitted.capture_id, context.frame.binding.as_ref().unwrap())
        .is_err());
    let output = tempfile::tempdir().unwrap();
    let session = session(context.clone(), output.path());
    set_click_marker_fn(|png, x, y| {
        Some(
            crate::image_utils::crosshair_png_bytes(png, x, y)
                .unwrap_or_else(|_| b"click".to_vec()),
        )
    });
    let pending = session.begin_turn("click", &serde_json::json!({"capture_id": admitted.capture_id.to_string(), "x":1920,"y":360,"scope":"desktop"}), 0,
        RecordingCaller { session: Some("recording-session"), transport: None }).unwrap();
    scope_dispatch_click_capture(Some(&pending), async {
        let sink = recording_admission_sink().unwrap();
        let binding = context.frame.binding.clone().unwrap();
        tokio::task::spawn_blocking(move || sink.retain(&binding, &admitted))
            .await
            .unwrap();
    })
    .await;
    assert!(recording_admission_sink().is_none());
    session.finish_turn(pending, "delivered once");
    let turn = output.path().join("turn-00001");
    let action = read(&turn, "action.json");
    assert_eq!(
        action["click_point"],
        serde_json::json!({"x":3840.0,"y":720.0})
    );
    assert_eq!(action["recording_frame"]["origin"]["x"], -2560);
    assert!(action["arguments"].get("_session_id").is_none());
    let marked = image::load_from_memory(&std::fs::read(turn.join("click.png")).unwrap())
        .unwrap()
        .to_rgba8();
    assert_ne!(marked.get_pixel(3840, 720).0, [0, 0, 0, 0]);
    assert_eq!(marked.get_pixel(1920, 360).0, [0, 0, 0, 0]);
    let evidence = read(&turn, "evidence.json");
    assert_eq!(evidence["click"]["source_image"], "before.png");
    assert_eq!(evidence["click"]["status"], "captured");
    assert_eq!(
        evidence["cursor"]["before"],
        serde_json::json!({"x":1.0,"y":1.0})
    );
    assert_eq!(context.captures.load(Ordering::SeqCst), 2);
}

#[tokio::test]
async fn successor_frame_and_other_lifetime_cannot_supply_recording_evidence() {
    for changed_binding in [false, true] {
        let service = CaptureService::new(Default::default()).unwrap();
        let context = context(&service, "owner", (8, 4));
        let admitted = admission(&service, &context);
        let output = tempfile::tempdir().unwrap();
        let session = session(context.clone(), output.path());
        let pending = session
            .begin_turn(
                "click",
                &serde_json::json!({"capture_id":admitted.capture_id.to_string(),"x":3,"y":1}),
                0,
                RecordingCaller {
                    session: Some("owner"),
                    transport: None,
                },
            )
            .unwrap();
        scope_dispatch_click_capture(Some(&pending), async {
            let binding = if changed_binding {
                service.current_binding("other-session").unwrap()
            } else {
                context.frame.binding.clone().unwrap()
            };
            if !changed_binding {
                context.live.store(false, Ordering::Release);
            }
            retain_recording_admission(&binding, &admitted);
            // Nested unrecorded turns mask the outer sink.
            scope_dispatch_click_capture(None, async {
                assert!(recording_admission_sink().is_none());
            })
            .await;
        })
        .await;
        context.live.store(false, Ordering::Release);
        session.finish_turn(pending, "partial action truth stays independent");
        let turn = output.path().join("turn-00001");
        let action = read(&turn, "action.json");
        assert!(action.get("click_point").is_none());
        assert_eq!(
            action["result_summary"],
            "partial action truth stays independent"
        );
        let evidence = read(&turn, "evidence.json");
        assert_eq!(
            evidence["after"]["screenshot"]["classification"],
            "recording_stale_frame"
        );
        assert_eq!(evidence["click"]["classification"], "recording_stale_frame");
        assert_eq!(evidence["click"]["source_image"], "before.png");
        assert!(!turn.join("after.png").exists());
        assert!(!turn.join("click.png").exists());
        assert_eq!(context.captures.load(Ordering::SeqCst), 1);
    }
}

#[test]
fn immutable_frame_rebases_pinned_window_and_rejects_identity_dimensions_and_pixel_budget() {
    let service = CaptureService::new(Default::default()).unwrap();
    let context = context(&service, "owner", (5120, 1440));
    let mut admitted = admission(&service, &context);
    let mut frame = context.frame.clone();
    frame.window = Some(RecordingWindow {
        pid: 7,
        window_id: 0x123456789,
        origin: (256, 10),
        dimensions: (800, 600),
    });
    admitted.target = CaptureTarget::Window {
        pid: 7,
        window_id: 0x123456789,
    };
    admitted.native_action_dimensions = NativeActionDimensions::new(800, 600).unwrap();
    admitted.action_x = 50.5;
    admitted.action_y = 20.25;
    assert_eq!(frame.admitted_point(&admitted), Ok((2866.5, 30.25)));
    admitted.frame_identity = Some(CaptureFrameIdentity::from_native_snapshot(
        b"same-dimensions-output-swap",
    ));
    assert_eq!(
        frame.admitted_point(&admitted),
        Err(RecordingUnavailable::StaleFrame)
    );
    admitted.frame_identity = None;
    assert_eq!(
        frame.admitted_point(&admitted),
        Err(RecordingUnavailable::StaleFrame)
    );
    admitted.frame_identity = frame.identity.clone();
    admitted.native_action_dimensions = NativeActionDimensions::new(799, 600).unwrap();
    assert_eq!(
        frame.admitted_point(&admitted),
        Err(RecordingUnavailable::StaleFrame)
    );
    admitted.native_action_dimensions = NativeActionDimensions::new(800, 600).unwrap();
    for point in [
        (-0.25, 10.0),
        (800.0, 10.0),
        (10.0, 600.0),
        (f64::NAN, 10.0),
    ] {
        admitted.action_x = point.0;
        admitted.action_y = point.1;
        assert_eq!(
            frame.admitted_point(&admitted),
            Err(RecordingUnavailable::InvalidFrame)
        );
    }
    assert_eq!(
        frame.check_png(&image(8, 4)),
        Err(RecordingUnavailable::InvalidFrame)
    );
    frame.dimensions = (u32::MAX, u32::MAX);
    assert_eq!(frame.validate(), Err(RecordingUnavailable::InvalidFrame));
}

#[test]
fn capture_slot_and_expired_deadline_fail_closed_without_starting_more_workers() {
    let service = CaptureService::new(Default::default()).unwrap();
    let context = context(&service, "owner", (8, 4));
    assert_eq!(
        context.capture(Instant::now()).classification,
        Some("recording_budget_exceeded")
    );
    let scope = TurnFrameScope {
        context: Ok(context.clone()),
        admitted_point: Mutex::new(None),
        capture_bound: false,
        capture_busy: Arc::new(AtomicBool::new(true)),
        point_failure: Mutex::new(None),
    };
    let capture = capture_turn_in_frame(
        None,
        None,
        StatePlan::Skip(STATE_SKIPPED_DISABLED),
        Some(&scope),
    );
    assert_eq!(
        capture.screenshot_classification,
        Some("recording_budget_exceeded")
    );
    assert!(capture.screenshot.is_none());
    assert_eq!(context.captures.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn concurrent_runtimes_with_the_same_session_label_keep_independent_turn_sinks() {
    let service_a = CaptureService::new(Default::default()).unwrap();
    let service_b = CaptureService::new(Default::default()).unwrap();
    let context_a = context(&service_a, "same-label", (8, 4));
    let context_b = context(&service_b, "same-label", (12, 6));
    assert_ne!(context_a.frame.binding, context_b.frame.binding);
    let admission_a = admission(&service_a, &context_a);
    let admission_b = admission(&service_b, &context_b);
    let output_a = tempfile::tempdir().unwrap();
    let output_b = tempfile::tempdir().unwrap();
    let session_a = session(context_a.clone(), output_a.path());
    let session_b = session(context_b.clone(), output_b.path());
    let args = serde_json::json!({"x":3,"y":1,"scope":"desktop"});
    let caller = RecordingCaller {
        session: Some("same-label"),
        transport: None,
    };
    let pending_a = session_a.begin_turn("click", &args, 0, caller).unwrap();
    let pending_b = session_b.begin_turn("click", &args, 0, caller).unwrap();
    tokio::join!(
        scope_dispatch_click_capture(Some(&pending_a), async {
            tokio::task::yield_now().await;
            let sink = recording_admission_sink().unwrap();
            assert_eq!(sink.frame().unwrap().dimensions, (8, 4));
            sink.retain(context_a.frame.binding.as_ref().unwrap(), &admission_a);
        }),
        scope_dispatch_click_capture(Some(&pending_b), async {
            let sink = recording_admission_sink().unwrap();
            assert_eq!(sink.frame().unwrap().dimensions, (12, 6));
            tokio::task::yield_now().await;
            sink.retain(context_b.frame.binding.as_ref().unwrap(), &admission_b);
        })
    );
    session_a.finish_turn(pending_a, "A");
    session_b.finish_turn(pending_b, "B");
    assert_eq!(
        read(&output_a.path().join("turn-00001"), "action.json")["click_point"],
        serde_json::json!({"x":6.0,"y":2.0})
    );
    assert_eq!(
        read(&output_b.path().join("turn-00001"), "action.json")["click_point"],
        serde_json::json!({"x":9.0,"y":3.0})
    );
    assert!(recording_admission_sink().is_none());
}

#[tokio::test]
async fn missing_handoff_never_remaps_raw_pixels_or_uses_an_independent_dispatch_image() {
    let service = CaptureService::new(Default::default()).unwrap();
    let context = context(&service, "owner", (8, 4));
    let output = tempfile::tempdir().unwrap();
    let session = session(context, output.path());
    let pending = session
        .begin_turn(
            "click",
            &serde_json::json!({"pid":7,"window_id":9,"x":3,"y":1}),
            0,
            RecordingCaller {
                session: Some("owner"),
                transport: None,
            },
        )
        .unwrap();
    scope_dispatch_click_capture(Some(&pending), async {
        capture_dispatch_click_target(9, 7, || {
            panic!("a second image cannot replace this turn's frame")
        });
    })
    .await;
    session.finish_turn(pending, "action outcome unchanged");
    let turn = output.path().join("turn-00001");
    assert!(read(&turn, "action.json").get("click_point").is_none());
    let evidence = read(&turn, "evidence.json");
    assert_eq!(evidence["before"]["screenshot"]["status"], "captured");
    assert_eq!(
        evidence["click"]["classification"],
        "recording_admission_unavailable"
    );
    assert_eq!(evidence["click"]["source_image"], "before.png");
    assert!(!turn.join("click_source.png").exists());
    assert!(!turn.join("click.png").exists());
}
