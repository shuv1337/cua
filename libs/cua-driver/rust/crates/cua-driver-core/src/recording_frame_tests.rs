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
    qualified: bool,
    blocked_point: Option<Arc<(Mutex<bool>, std::sync::Condvar)>>,
    after_capture_gate: Option<Arc<(Mutex<bool>, std::sync::Condvar)>>,
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
        if self.captures.fetch_add(1, Ordering::SeqCst) > 0 {
            if let Some(gate) = &self.after_capture_gate {
                let mut released = gate.0.lock().unwrap();
                while !*released {
                    released = gate.1.wait(released).unwrap();
                }
            }
        }
        if self.qualified {
            ScreenshotCapture::captured_in_frame(self.png.clone())
        } else {
            ScreenshotCapture::unqualified(self.png.clone())
        }
    }
    fn click_point(&self) -> Result<Option<(f64, f64)>, RecordingUnavailable> {
        self.point
            .map(|point| self.validate_point(point))
            .transpose()
    }
    fn validate_point(&self, point: (f64, f64)) -> Result<(f64, f64), RecordingUnavailable> {
        if let Some(gate) = self.blocked_point.as_ref().filter(|_| point != (1.0, 1.0)) {
            let (lock, wake) = &**gate;
            let mut released = lock.lock().unwrap();
            while !*released {
                released = wake.wait(released).unwrap();
            }
        }
        if !self.live.load(Ordering::Acquire) {
            return Err(RecordingUnavailable::StaleFrame);
        }
        self.frame.image_point(point.0, point.1)
    }
    fn cursor_point(&self) -> Result<Option<(f64, f64)>, RecordingUnavailable> {
        assert!(
            self.qualified,
            "unqualified pixels cannot receive a compositor cursor"
        );
        self.validate_point((1.0, 1.0)).map(Some)
    }

    fn validate_authority(&self) -> Result<(), RecordingUnavailable> {
        if self.live.load(Ordering::Acquire) {
            Ok(())
        } else {
            Err(RecordingUnavailable::StaleFrame)
        }
    }
}

fn image(w: u32, h: u32) -> Vec<u8> {
    crate::image_utils::encode_rgba_to_png(&vec![0; (w * h * 4) as usize], w, h).unwrap()
}

fn context(service: &CaptureService, name: &str, dimensions: (u32, u32)) -> Arc<Context> {
    set_click_marker_fn(|png, x, y| {
        Some(
            crate::image_utils::crosshair_png_bytes(png, x, y)
                .unwrap_or_else(|_| b"click".to_vec()),
        )
    });
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
        qualified: true,
        blocked_point: None,
        after_capture_gate: None,
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
        assert_eq!(action.get("click_point").is_none(), changed_binding);
        assert_eq!(
            action["result_summary"],
            "partial action truth stays independent"
        );
        let evidence = read(&turn, "evidence.json");
        assert_eq!(
            evidence["after"]["screenshot"]["classification"],
            "recording_stale_frame"
        );
        if changed_binding {
            assert_eq!(evidence["click"]["classification"], "recording_stale_frame");
        } else {
            assert_eq!(evidence["click"]["status"], "captured");
        }
        assert_eq!(evidence["click"]["source_image"], "before.png");
        assert!(!turn.join("after.png").exists());
        assert_eq!(turn.join("click.png").exists(), !changed_binding);
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
        budget: Duration::from_secs(12),
        expected_capture_id: None,
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

#[tokio::test]
async fn wrong_peer_same_geometry_keeps_raw_pixels_without_marker_cursor_or_native_identity() {
    let service = CaptureService::new(Default::default()).unwrap();
    let mut context = context(&service, "owner", (8, 4));
    Arc::get_mut(&mut context).unwrap().qualified = false;
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
        retain_recording_admission(context.frame.binding.as_ref().unwrap(), &admitted);
    })
    .await;
    session.finish_turn(pending, "action delivered independently");
    let turn = output.path().join("turn-00001");
    assert_eq!(std::fs::read(turn.join("before.png")).unwrap(), context.png);
    let evidence = read(&turn, "evidence.json");
    assert_eq!(evidence["before"]["screenshot"]["status"], "captured");
    assert_eq!(
        evidence["before"]["screenshot"]["classification"],
        "recording_coordinate_attestation_unavailable"
    );
    assert_eq!(
        evidence["click"]["classification"],
        "recording_coordinate_attestation_unavailable"
    );
    assert!(evidence["cursor"]["before"].is_null());
    assert!(evidence["cursor"]["after"].is_null());
    let action = read(&turn, "action.json");
    assert_eq!(
        action["recording_frame"]["native_identity_available"],
        false
    );
    assert!(action["recording_frame"]["native_frame_identity"].is_null());
    assert!(action.get("click_point").is_none());
    assert!(!turn.join("click.png").exists());
}

#[test]
fn logical_ihdr_prefix_is_not_a_captured_image_and_stale_authority_skips_enabled_state() {
    let service = CaptureService::new(Default::default()).unwrap();
    let mut invalid = context(&service, "owner", (8, 4));
    Arc::get_mut(&mut invalid).unwrap().png.truncate(24);
    assert_eq!(
        invalid.frame.check_png(&invalid.png),
        Err(RecordingUnavailable::InvalidFrame)
    );
    assert!(!point_in_image(&invalid.png, 1.0, 1.0));
    let png = image(8, 4);
    assert_eq!(
        validate_recording_png(&png[..png.len() - 12]),
        Err(RecordingUnavailable::InvalidFrame)
    );
    let scope = |context| TurnFrameScope {
        context: Ok(context),
        admitted_point: Mutex::new(None),
        capture_bound: false,
        capture_busy: Arc::new(AtomicBool::new(false)),
        point_failure: Mutex::new(None),
        budget: Duration::from_millis(100),
        expected_capture_id: None,
    };
    let capture = capture_turn_in_frame(
        None,
        None,
        StatePlan::Skip(STATE_SKIPPED_DISABLED),
        Some(&scope(invalid)),
    );
    assert_eq!(
        capture.screenshot_classification,
        Some("recording_invalid_frame")
    );
    assert!(capture.screenshot.is_none());
    assert!(capture.cursor_point.is_none());
    assert!(!capture.frame_qualified);

    let stale = context(&service, "owner", (8, 4));
    stale.live.store(false, Ordering::Release);
    let capture = capture_turn_in_frame(
        Some(42),
        Some(43),
        StatePlan::Capture(StateCaptureBudget::default()),
        Some(&scope(stale)),
    );
    assert_eq!(
        capture.screenshot_classification,
        Some("recording_stale_frame")
    );
    assert_eq!(capture.state.classification, Some("recording_stale_frame"));
    assert!(capture.state.bytes.is_none());
}

fn release_gate(gate: &Arc<(Mutex<bool>, std::sync::Condvar)>) {
    *gate.0.lock().unwrap() = true;
    gate.1.notify_all();
}

#[tokio::test]
async fn slow_factory_and_semantic_point_share_the_phase_slot_and_cannot_publish_late() {
    for block_semantic in [false, true] {
        let service = CaptureService::new(Default::default()).unwrap();
        let mut context = context(&service, "owner", (8, 4));
        let gate = Arc::new((Mutex::new(false), std::sync::Condvar::new()));
        if block_semantic {
            Arc::get_mut(&mut context).unwrap().blocked_point = Some(gate.clone());
        }
        let calls = Arc::new(AtomicUsize::new(0));
        let output = tempfile::tempdir().unwrap();
        let session = RecordingSession::new_with_frame_budget(Duration::from_millis(40));
        let source = context.clone();
        let count = calls.clone();
        session.set_turn_context_fn(move |_, _, _| {
            count.fetch_add(1, Ordering::SeqCst);
            if !block_semantic {
                std::thread::sleep(Duration::from_millis(180));
            }
            Ok(source.clone())
        });
        session
            .start_with_state_budget(output.path().to_str().unwrap(), false, None, None)
            .unwrap();
        let start = Instant::now();
        let pending = session
            .begin_turn(
                "click",
                &serde_json::json!({"element_token":"semantic"}),
                0,
                RecordingCaller::default(),
            )
            .unwrap();
        assert!(start.elapsed() < Duration::from_millis(150));
        session.finish_turn(pending, "valid action not refused by recording timeout");
        let second = session
            .begin_turn(
                "click",
                &serde_json::json!({"x":3,"y":1}),
                0,
                RecordingCaller::default(),
            )
            .unwrap();
        session.finish_turn(second, "next action unchanged");
        assert_eq!(
            calls.load(Ordering::SeqCst),
            1,
            "an abandoned provider holds the runtime slot"
        );
        release_gate(&gate);
        std::thread::sleep(Duration::from_millis(220));
        for (index, result) in [
            (1, "valid action not refused by recording timeout"),
            (2, "next action unchanged"),
        ] {
            let turn = output.path().join(format!("turn-{index:05}"));
            assert_eq!(read(&turn, "action.json")["result_summary"], result);
            assert_eq!(
                read(&turn, "evidence.json")["before"]["screenshot"]["classification"],
                "recording_budget_exceeded"
            );
            assert!(!turn.join("before.png").exists());
            assert!(!turn.join("after.png").exists());
        }
        assert_eq!(context.captures.load(Ordering::SeqCst), 0);
    }
}

#[tokio::test]
async fn action_handoff_does_not_query_blocked_point_validator_and_after_timeout_is_bounded() {
    let service = CaptureService::new(Default::default()).unwrap();
    let mut context = context(&service, "owner", (8, 4));
    let gate = Arc::new((Mutex::new(false), std::sync::Condvar::new()));
    let mutable = Arc::get_mut(&mut context).unwrap();
    mutable.point = None;
    mutable.blocked_point = Some(gate.clone());
    mutable.after_capture_gate = Some(gate.clone());
    let admitted = admission(&service, &context);
    let output = tempfile::tempdir().unwrap();
    let session = RecordingSession::new_with_frame_budget(Duration::from_millis(40));
    let source = context.clone();
    session.set_turn_context_fn(move |_, _, _| Ok(source.clone()));
    session
        .start_with_state_budget(output.path().to_str().unwrap(), false, None, None)
        .unwrap();
    let pending = session
        .begin_turn(
            "click",
            &serde_json::json!({"capture_id":admitted.capture_id.to_string(),"x":3,"y":1}),
            0,
            RecordingCaller::default(),
        )
        .unwrap();
    let (tx, rx) = std::sync::mpsc::channel();
    scope_dispatch_click_capture(Some(&pending), async {
        let sink = recording_admission_sink().unwrap();
        let binding = context.frame.binding.clone().unwrap();
        std::thread::spawn(move || {
            sink.retain(&binding, &admitted);
            tx.send(()).unwrap();
        });
    })
    .await;
    if rx.recv_timeout(Duration::from_millis(100)).is_err() {
        release_gate(&gate);
        panic!("native validator blocked action-facing retain");
    }
    let start = Instant::now();
    use crate::action_record::{
        ActionEffect, ActionExecutionRecord, ActionTransport, ActualDelivery, RequestedDelivery,
    };
    let truth = ActionExecutionRecord::builder(
        ActionEffect::Unverifiable,
        ActionTransport::LinuxAtSpiAction,
        RequestedDelivery::Background,
    )
    .actual_delivery(ActualDelivery::Background)
    .build()
    .unwrap();
    session.finish_turn_with_outcome(
        pending,
        "original action effect retained",
        Some(&truth),
        false,
    );
    assert!(start.elapsed() < Duration::from_millis(150));
    let turn = output.path().join("turn-00001");
    assert_eq!(
        read(&turn, "evidence.json")["after"]["screenshot"]["classification"],
        "recording_budget_exceeded"
    );
    let action = std::fs::read(turn.join("action.json")).unwrap();
    assert_eq!(
        read(&turn, "action.json")["action_truth"],
        truth.debug_json()
    );
    assert_eq!(read(&turn, "action.json")["result_error"], false);
    release_gate(&gate);
    std::thread::sleep(Duration::from_millis(80));
    assert_eq!(std::fs::read(turn.join("action.json")).unwrap(), action);
    assert!(!turn.join("after.png").exists());
    assert_eq!(read(&turn, "evidence.json")["click"]["status"], "captured");
    assert_eq!(
        std::fs::read(turn.join("click.png")).unwrap(),
        crate::image_utils::crosshair_png_bytes(&context.png, 6.0, 2.0).unwrap()
    );
}

fn window_admission(
    service: &CaptureService,
    context: &Context,
    screenshot_x: f64,
) -> CaptureActionAdmission {
    let window = context.frame.window.as_ref().unwrap();
    let binding = context.frame.binding.as_ref().unwrap();
    let local_identity = CaptureFrameIdentity::from_native_snapshot(b"client-local-root/image");
    let target = CaptureTarget::Window {
        pid: window.pid,
        window_id: window.window_id,
    };
    let encoded = (window.dimensions.0 / 2, window.dimensions.1 / 2);
    let id = service
        .publish_in_frame(
            CapturePublication {
                png_bytes: image(encoded.0, encoded.1),
                target: target.clone(),
                encoded_dimensions: EncodedScreenshotDimensions::new(encoded.0, encoded.1).unwrap(),
                native_action_dimensions: NativeActionDimensions::new(
                    window.dimensions.0,
                    window.dimensions.1,
                )
                .unwrap(),
                screenshot_to_action: ScreenshotToActionTransform::new(
                    2.0, 0.0, 0.0, 2.0, 0.0, 0.0,
                )
                .unwrap(),
                session_id: binding.session_id().into(),
                session_generation: binding.session_generation(),
            },
            Some(local_identity.clone()),
        )
        .unwrap();
    service
        .admit_action_in_frame(
            CaptureActionRequest {
                capture_id: id,
                binding: binding.clone(),
                target,
                current_native_action_dimensions: NativeActionDimensions::new(
                    window.dimensions.0,
                    window.dimensions.1,
                )
                .unwrap(),
                screenshot_x,
                screenshot_y: 1.0,
            },
            Some(&local_identity),
        )
        .unwrap()
}

#[tokio::test]
async fn every_reconciled_window_or_generic_handoff_enforces_the_expected_consumed_id() {
    for route in [
        "valid-window",
        "crossed-window",
        "id-free-window",
        "id-free-generic",
        "crossed-generic",
        "valid-generic",
    ] {
        let service = CaptureService::new(Default::default()).unwrap();
        let mut context = context(&service, "owner", (8, 4));
        Arc::get_mut(&mut context).unwrap().frame.window = Some(RecordingWindow {
            pid: 7,
            window_id: 9,
            origin: (-2560, 0),
            dimensions: (8, 4),
        });
        let expected = window_admission(&service, &context, 3.0);
        let crossed = window_admission(&service, &context, 2.0);
        assert_ne!(expected.capture_id, crossed.capture_id);
        assert_ne!(
            expected.frame_identity, context.frame.identity,
            "client-local identity is not global recording proof"
        );
        let output = tempfile::tempdir().unwrap();
        let session = session(context.clone(), output.path());
        let pending = session.begin_turn("click",&serde_json::json!({"pid":7,"window_id":9,"capture_id":expected.capture_id.to_string(),"x":3,"y":1}),0,
            RecordingCaller { session:Some("owner"),transport:None }).unwrap();
        scope_dispatch_click_capture(Some(&pending), async {
            let sink = recording_admission_sink().unwrap();
            let binding = context.frame.binding.as_ref().unwrap();
            let global = context.frame.identity.as_ref();
            let source = context.frame.window.as_ref().unwrap();
            match route {
                "valid-window" => sink.retain_window_action_point_with_capture_id(
                    binding,
                    global,
                    source,
                    (expected.action_x, expected.action_y),
                    Some(&expected.capture_id),
                ),
                "crossed-window" => sink.retain_window_action_point_with_capture_id(
                    binding,
                    global,
                    source,
                    (crossed.action_x, crossed.action_y),
                    Some(&crossed.capture_id),
                ),
                "id-free-window" => sink.retain_window_action_point(
                    binding,
                    global,
                    source,
                    (expected.action_x, expected.action_y),
                ),
                "id-free-generic" => sink.retain_action_point(
                    binding,
                    global,
                    &expected.target,
                    source.dimensions,
                    (expected.action_x, expected.action_y),
                ),
                "crossed-generic" => sink.retain_action_point_with_capture_id(
                    binding,
                    global,
                    &crossed.target,
                    source.dimensions,
                    (crossed.action_x, crossed.action_y),
                    Some(&crossed.capture_id),
                ),
                "valid-generic" => sink.retain_action_point_with_capture_id(
                    binding,
                    global,
                    &expected.target,
                    source.dimensions,
                    (expected.action_x, expected.action_y),
                    Some(&expected.capture_id),
                ),
                _ => unreachable!(),
            }
        })
        .await;
        session.finish_turn(pending, "original action unchanged");
        let turn = output.path().join("turn-00001");
        let valid = route.starts_with("valid");
        if valid {
            assert_eq!(read(&turn, "evidence.json")["click"]["status"], "captured");
            assert_eq!(
                std::fs::read(turn.join("click.png")).unwrap(),
                crate::image_utils::crosshair_png_bytes(&context.png, 6.0, 2.0).unwrap()
            );
        } else {
            assert_eq!(
                read(&turn, "evidence.json")["click"]["classification"],
                "recording_invalid_frame"
            );
            assert!(!turn.join("click.png").exists());
        }
    }
}

#[tokio::test]
async fn qualified_before_marker_survives_close_move_and_after_timeout_without_rewriting_effects() {
    use crate::action_record::{
        ActionEffect, ActionExecutionRecord, ActionTransport, ActualDelivery, RequestedDelivery,
    };
    for cause in ["dismiss", "move", "timeout"] {
        let service = CaptureService::new(Default::default()).unwrap();
        let mut context = context(&service, "owner", (8, 4));
        let gate = Arc::new((Mutex::new(false), std::sync::Condvar::new()));
        let mutable = Arc::get_mut(&mut context).unwrap();
        mutable.frame.window = Some(RecordingWindow {
            pid: 7,
            window_id: 9,
            origin: (-2560, 0),
            dimensions: (8, 4),
        });
        if cause == "timeout" {
            mutable.after_capture_gate = Some(gate.clone());
        }
        let admitted = window_admission(&service, &context, 3.0);
        let output = tempfile::tempdir().unwrap();
        let session = RecordingSession::new_with_frame_budget(Duration::from_millis(50));
        let source = context.clone();
        session.set_turn_context_fn(move |_, _, _| Ok(source.clone()));
        session
            .start_with_state_budget(
                output.path().to_str().unwrap(),
                false,
                None,
                Some(StateCaptureBudget::default()),
            )
            .unwrap();
        let pending = session.begin_turn("click",&serde_json::json!({"pid":7,"window_id":9,"capture_id":admitted.capture_id.to_string(),"x":3,"y":1}),0,RecordingCaller::default()).unwrap();
        scope_dispatch_click_capture(Some(&pending), async {
            recording_admission_sink()
                .unwrap()
                .retain_window_action_point_with_capture_id(
                    context.frame.binding.as_ref().unwrap(),
                    context.frame.identity.as_ref(),
                    context.frame.window.as_ref().unwrap(),
                    (admitted.action_x, admitted.action_y),
                    Some(&admitted.capture_id),
                );
        })
        .await;
        // These are post-action changes, not a rejected pre-dispatch source.
        if cause != "timeout" {
            context.live.store(false, Ordering::Release);
        }
        let truth = ActionExecutionRecord::builder(
            ActionEffect::Unverifiable,
            ActionTransport::LinuxAtSpiAction,
            RequestedDelivery::Background,
        )
        .actual_delivery(ActualDelivery::Background)
        .build()
        .unwrap();
        session.finish_turn_with_outcome(pending, "original action outcome", Some(&truth), false);
        let turn = output.path().join("turn-00001");
        let evidence = read(&turn, "evidence.json");
        let error = if cause == "timeout" {
            "recording_budget_exceeded"
        } else {
            "recording_stale_frame"
        };
        assert_eq!(evidence["after"]["screenshot"]["classification"], error);
        assert_eq!(evidence["after"]["state"]["classification"], error);
        assert_eq!(evidence["click"]["status"], "captured");
        assert_eq!(evidence["click"]["source_image"], "before.png");
        let marked = std::fs::read(turn.join("click.png")).unwrap();
        assert_eq!(
            marked,
            crate::image_utils::crosshair_png_bytes(&context.png, 6.0, 2.0).unwrap()
        );
        let pixels = image::load_from_memory(&marked).unwrap().to_rgba8();
        assert_ne!(pixels.get_pixel(6, 2).0, [0; 4]);
        assert_eq!(std::fs::read(turn.join("before.png")).unwrap(), context.png);
        assert_eq!(
            read(&turn, "action.json")["action_truth"],
            truth.debug_json()
        );
        assert_eq!(read(&turn, "action.json")["result_error"], false);
        assert!(!turn.join("after.png").exists());
        release_gate(&gate);
        std::thread::sleep(Duration::from_millis(80));
        assert!(
            !turn.join("after.png").exists(),
            "late evidence cannot publish or replay into the completed turn"
        );
        assert_eq!(std::fs::read(turn.join("click.png")).unwrap(), marked);
    }
}

#[tokio::test]
async fn known_pre_dispatch_refusal_still_omits_a_marker_even_with_a_qualified_before_image() {
    use crate::action_record::{
        ActionEffect, ActionExecutionRecord, ActionTransport, RequestedDelivery,
    };
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
        retain_recording_admission(context.frame.binding.as_ref().unwrap(), &admitted);
    })
    .await;
    context.live.store(false, Ordering::Release);
    let refusal = ActionExecutionRecord::builder(
        ActionEffect::Refused,
        ActionTransport::LinuxAtSpiAction,
        RequestedDelivery::Background,
    )
    .build()
    .unwrap();
    session.finish_turn_with_outcome(
        pending,
        "source guard refused before input",
        Some(&refusal),
        true,
    );
    let turn = output.path().join("turn-00001");
    assert_eq!(
        read(&turn, "evidence.json")["click"]["classification"],
        "action_refused_before_dispatch"
    );
    assert_eq!(
        read(&turn, "action.json")["action_truth"],
        refusal.debug_json()
    );
    assert!(turn.join("before.png").exists());
    assert!(!turn.join("click.png").exists());
}

#[tokio::test]
async fn crossed_same_session_capture_sink_and_different_source_window_origin_are_refused() {
    let service = CaptureService::new(Default::default()).unwrap();
    let mut context = context(&service, "owner", (8, 4));
    Arc::get_mut(&mut context).unwrap().frame.window = Some(RecordingWindow {
        pid: 7,
        window_id: 9,
        origin: (-2560, 0),
        dimensions: (8, 4),
    });
    let expected = admission(&service, &context);
    let wrong = admission(&service, &context);
    let output = tempfile::tempdir().unwrap();
    let session = session(context.clone(), output.path());
    let pending = session
        .begin_turn(
            "click",
            &serde_json::json!({"capture_id":expected.capture_id.to_string(),"x":3,"y":1}),
            0,
            RecordingCaller {
                session: Some("owner"),
                transport: None,
            },
        )
        .unwrap();
    scope_dispatch_click_capture(Some(&pending), async {
        retain_recording_admission(context.frame.binding.as_ref().unwrap(), &wrong);
    })
    .await;
    session.finish_turn(pending, "action result unchanged");
    assert_eq!(
        read(&output.path().join("turn-00001"), "evidence.json")["click"]["classification"],
        "recording_invalid_frame"
    );

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
        let sink = recording_admission_sink().unwrap();
        let mut source = context.frame.window.clone().unwrap();
        source.origin.0 += 1;
        sink.retain_window_action_point(
            context.frame.binding.as_ref().unwrap(),
            context.frame.identity.as_ref(),
            &source,
            (3.0, 1.0),
        );
    })
    .await;
    session.finish_turn(pending, "guarded action independently valid");
    assert_eq!(
        read(&output.path().join("turn-00002"), "evidence.json")["click"]["classification"],
        "recording_stale_frame"
    );
}
