use super::*;
use cua_driver_core::{capture_runtime::*, recording::*, recording_frame::*};

struct Context {
    frame: RecordingFrame,
    png: Vec<u8>,
    live: Arc<AtomicBool>,
    alternate: AtomicBool,
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
        ScreenshotCapture::captured_in_frame(self.png.clone())
    }
    fn click_point(&self) -> std::result::Result<Option<(f64, f64)>, RecordingUnavailable> {
        self.alternate.store(true, Ordering::Release);
        Ok(Some((1.0, 1.0)))
    }
    fn validate_point(
        &self,
        point: (f64, f64),
    ) -> std::result::Result<(f64, f64), RecordingUnavailable> {
        self.validate_authority()?;
        self.frame.image_point(point.0, point.1)
    }
    fn cursor_point(&self) -> std::result::Result<Option<(f64, f64)>, RecordingUnavailable> {
        Ok(None)
    }
    fn validate_authority(&self) -> std::result::Result<(), RecordingUnavailable> {
        if self.live.load(Ordering::Acquire) {
            Ok(())
        } else {
            Err(RecordingUnavailable::StaleFrame)
        }
    }
}
fn png(w: u32, h: u32) -> Vec<u8> {
    cua_driver_core::image_utils::encode_rgba_to_png(&vec![0; (w * h * 4) as usize], w, h).unwrap()
}
fn consume(service: &CaptureService, source: &WindowSource, x: f64) -> CaptureActionAdmission {
    let id = service
        .publish_in_frame(
            CapturePublication {
                png_bytes: png(1500, 50),
                target: CaptureTarget::PrimaryDesktop,
                encoded_dimensions: EncodedScreenshotDimensions::new(1500, 50).unwrap(),
                native_action_dimensions: NativeActionDimensions::new(3000, 100).unwrap(),
                screenshot_to_action: ScreenshotToActionTransform::new(
                    2.0, 0.0, 0.0, 2.0, 0.0, 0.0,
                )
                .unwrap(),
                session_id: source.binding.session_id().into(),
                session_generation: source.binding.session_generation(),
            },
            Some(source.frame.identity.clone()),
        )
        .unwrap();
    service
        .admit_action_in_frame(
            CaptureActionRequest {
                capture_id: id,
                binding: source.binding.clone(),
                target: CaptureTarget::PrimaryDesktop,
                current_native_action_dimensions: NativeActionDimensions::new(3000, 100).unwrap(),
                screenshot_x: x,
                screenshot_y: 10.375,
            },
            Some(&source.frame.identity),
        )
        .unwrap()
}

#[tokio::test]
async fn retained_held_begin_and_move_use_real_recording_sink_original_consumed_id_without_alternate_handoff(
) {
    set_click_marker_fn(|png, x, y| {
        cua_driver_core::image_utils::crosshair_png_bytes(png, x, y).ok()
    });
    for cause in ["valid", "crossed_cap", "lost_before", "lost_after"] {
        let service = CaptureService::new(Default::default()).unwrap();
        let (mut source, peer) = super::tests::held_fixture(true);
        let value = Arc::get_mut(&mut source).unwrap();
        value.binding = service.current_binding("held-recording").unwrap();
        value.frame.width = 3000;
        value.frame.height = 100;
        let original = consume(&service, &source, 1285.25);
        let crossed = consume(&service, &source, 1286.25);
        source.retain_admission(&source.binding, &original);
        Arc::get_mut(&mut source).unwrap().requested_capture_id =
            Some(original.capture_id.to_string());
        assert!(
            service
                .admit_action_in_frame(
                    CaptureActionRequest {
                        capture_id: original.capture_id,
                        binding: source.binding.clone(),
                        target: CaptureTarget::PrimaryDesktop,
                        current_native_action_dimensions: NativeActionDimensions::new(3000, 100)
                            .unwrap(),
                        screenshot_x: 1285.25,
                        screenshot_y: 10.375
                    },
                    Some(&source.frame.identity)
                )
                .is_err(),
            "consumed admission cannot be reused"
        );
        let context = Arc::new(Context {
            frame: RecordingFrame {
                binding: Some(source.binding.clone()),
                identity: Some(source.frame.identity.clone()),
                origin: (0, 0),
                dimensions: (3000, 100),
                window: None,
            },
            png: png(3000, 100),
            live: Arc::new(AtomicBool::new(cause != "lost_before")),
            alternate: AtomicBool::new(false),
        });
        let recording = RecordingSession::new_with_frame_budget(Duration::from_millis(100));
        let retained = context.clone();
        recording.set_turn_context_fn(move |_, _, _| Ok(retained.clone()));
        let output = tempfile::tempdir().unwrap();
        recording
            .start_with_state_budget(output.path().to_str().unwrap(), false, None, None)
            .unwrap();
        let server = std::thread::spawn(move || {
            super::tests::serve_held_begin(&peer);
            let mut bytes = [0u8; 2048];
            let receive = || {
                let n = unsafe {
                    libc::recv(peer.as_raw_fd(), bytes.as_mut_ptr().cast(), bytes.len(), 0)
                };
                assert!(n >= 0);
                String::from_utf8(bytes[..n as usize].to_vec()).unwrap()
            };
            let mut receive = receive;
            assert!(receive().starts_with("HOLD_MOVE 2 "));
            peer.send(json!({"ok":true,"phase":"held_moved","handle":"55555555555555555555555555555555","sequence":2}).to_string().as_bytes()).unwrap();
            assert_eq!(receive(), "HOLD_RELEASE 3 55555555555555555555555555555555");
            peer.send(json!({"ok":true,"cleanup_confirmed":true,"sequence":3,"handle":"55555555555555555555555555555555"}).to_string().as_bytes()).unwrap();
        });
        let expected = if cause == "crossed_cap" {
            crossed.capture_id
        } else {
            original.capture_id
        };
        let args = json!({"capture_id":expected.to_string(),"_session_id":source.binding.session_id(),"scope":"desktop","x":1285.25,"y":10.375});
        let pending = recording
            .begin_turn(
                "mouse_button_down",
                &args,
                0,
                RecordingCaller {
                    session: Some(source.binding.session_id()),
                    transport: None,
                },
            )
            .unwrap();
        let retained = source.clone();
        let handle = scope_dispatch_click_capture(Some(&pending), async {
            let sink = recording_admission_sink();
            tokio::task::spawn_blocking(move || {
                crate::capture_action_frame::with_recording_sink(sink, || {
                    begin_held_source(
                        retained,
                        (10.25, 10.25),
                        1,
                        true,
                        ActionCancellation::default(),
                    )
                })
            })
            .await
            .unwrap()
            .unwrap()
        })
        .await;
        if cause == "lost_after" {
            context.live.store(false, Ordering::Release);
        }
        recording.finish_turn(pending, "original held BEGIN outcome unchanged");
        let pending = recording
            .begin_turn(
                "mouse_drag",
                &args,
                0,
                RecordingCaller {
                    session: Some(source.binding.session_id()),
                    transport: None,
                },
            )
            .unwrap();
        let worker = handle.clone();
        let owner = source.owner.clone();
        let binding = source.binding.clone();
        scope_dispatch_click_capture(Some(&pending), async {
            let sink = recording_admission_sink();
            tokio::task::spawn_blocking(move || {
                crate::capture_action_frame::with_recording_sink(sink, || {
                    worker.drag_to(
                        &owner,
                        &binding,
                        (10.25, 10.25),
                        (20.25, 20.25),
                        Duration::ZERO,
                        1,
                        ActionCancellation::default(),
                    )
                })
            })
            .await
            .unwrap()
            .unwrap();
        })
        .await;
        recording.finish_turn(pending, "original held MOVE outcome unchanged");
        handle.release(&source.owner, &source.binding).unwrap();
        drop(handle);
        server.join().unwrap();
        for turn in ["turn-00001", "turn-00002"] {
            let path = output.path().join(turn);
            assert_eq!(
                path.join("click.png").exists(),
                cause == "valid" || (cause == "lost_after" && turn == "turn-00001"),
                "{cause}/{turn}"
            );
        }
        assert!(
            !context.alternate.load(Ordering::Acquire),
            "no independent semantic/pixel fallback for original-ID handoff"
        );
    }
}
