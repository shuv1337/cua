use crate::{
    capture_runtime::*,
    protocol::ToolResult,
    tool::{Tool, ToolDef, ToolRegistry},
};
use async_trait::async_trait;
use serde_json::{json, Value};
use std::sync::{
    atomic::{AtomicUsize, Ordering},
    Arc, Mutex,
};

fn def(name: &str, read_only: bool) -> ToolDef {
    ToolDef {
        name: name.into(),
        description: "instrumented provider".into(),
        input_schema: json!({"type":"object"}),
        read_only,
        destructive: false,
        idempotent: false,
        open_world: false,
    }
}

struct Capture {
    def: ToolDef,
    service: Arc<CaptureService>,
    observed: Arc<Mutex<Option<Value>>>,
    entered: Arc<tokio::sync::Notify>,
    release: Arc<tokio::sync::Notify>,
}
#[async_trait]
impl Tool for Capture {
    fn def(&self) -> &ToolDef {
        &self.def
    }
    async fn invoke(&self, args: Value) -> ToolResult {
        *self.observed.lock().unwrap() = Some(args.clone());
        if args["wait"] == true {
            self.entered.notify_one();
            self.release.notified().await;
        }
        let capped = args["capped"] == true;
        let (w, h) = if capped { (1024, 288) } else { (5120, 1440) };
        let mut result = json!({"platform":"linux","display":"primary","scale_factor":1.5,"screenshot_mime_type":"image/png","screen_width":5120,"screen_height":1440,"screenshot_width":w,"screenshot_height":h,"action_capture_available":false,"capture_id":null,"provider":args["provider"]});
        if capped {
            result["screenshot_original_width"] = json!(5120);
            result["screenshot_original_height"] = json!(1440);
        }
        if args["qualified"] == true {
            let binding = self.service.binding_from_args(&args).unwrap();
            let png =
                crate::image_utils::encode_rgba_to_png(&vec![255; (w * h * 4) as usize], w, h)
                    .unwrap();
            let publication = CapturePublication {
                png_bytes: png,
                session_id: binding.session_id().into(),
                session_generation: binding.session_generation(),
                target: CaptureTarget::PrimaryDesktop,
                encoded_dimensions: EncodedScreenshotDimensions::new(w, h).unwrap(),
                native_action_dimensions: NativeActionDimensions::new(5120, 1440).unwrap(),
                screenshot_to_action: ScreenshotToActionTransform::new(
                    5120.0 / f64::from(w),
                    0.0,
                    0.0,
                    1440.0 / f64::from(h),
                    0.0,
                    0.0,
                )
                .unwrap(),
            };
            match self.service.publish_started_in_frame(
                publication,
                Some(CaptureFrameIdentity::from_native_snapshot(
                    b"original operation peer and output plan",
                )),
                args["_capture_started_sequence"].as_u64(),
            ) {
                Ok(id) => {
                    result["action_capture_available"] = json!(true);
                    result["capture_id"] = json!(id.to_string());
                }
                Err(error) => return ToolResult::error(error.to_string()),
            }
        }
        ToolResult::text("captured").with_structured(result)
    }
}
struct Input {
    def: ToolDef,
    hits: Arc<AtomicUsize>,
    point: Arc<Mutex<Option<Value>>>,
}
#[async_trait]
impl Tool for Input {
    fn def(&self) -> &ToolDef {
        &self.def
    }
    async fn invoke(&self, args: Value) -> ToolResult {
        self.hits.fetch_add(1, Ordering::SeqCst);
        *self.point.lock().unwrap() = Some(args);
        ToolResult::text("instrumented input")
    }
}
struct Fixture {
    registry: Arc<ToolRegistry>,
    service: Arc<CaptureService>,
    hits: Arc<AtomicUsize>,
    point: Arc<Mutex<Option<Value>>>,
    observed: Arc<Mutex<Option<Value>>>,
    entered: Arc<tokio::sync::Notify>,
    release: Arc<tokio::sync::Notify>,
}
fn fixture() -> Fixture {
    let mut registry = ToolRegistry::new();
    let service = registry.capture_service();
    let hits = Arc::new(AtomicUsize::new(0));
    let point = Arc::new(Mutex::new(None));
    let observed = Arc::new(Mutex::new(None));
    let entered = Arc::new(tokio::sync::Notify::new());
    let release = Arc::new(tokio::sync::Notify::new());
    registry.register(Box::new(Capture {
        def: def("get_desktop_state", true),
        service: service.clone(),
        observed: observed.clone(),
        entered: entered.clone(),
        release: release.clone(),
    }));
    registry.register(Box::new(Input {
        def: def("move_cursor", false),
        hits: hits.clone(),
        point: point.clone(),
    }));
    Fixture {
        registry: Arc::new(registry),
        service,
        hits,
        point,
        observed,
        entered,
        release,
    }
}
impl Fixture {
    async fn capture(&self, qualified: bool, capped: bool, provider: &str) -> ToolResult {
        self.registry.invoke_from_trusted_adapter("get_desktop_state",json!({"_session_id":"visual-availability","qualified":qualified,"capped":capped,"provider":provider})).await
    }
    async fn input(&self) -> ToolResult {
        self.registry
            .invoke_from_trusted_adapter(
                "move_cursor",
                json!({"_session_id":"visual-availability","scope":"desktop","x":100.0,"y":100.0}),
            )
            .await
    }
    fn binding(&self) -> CaptureBinding {
        self.service
            .binding_from_args(self.observed.lock().unwrap().as_ref().unwrap())
            .unwrap()
    }
    fn assert_no_legacy_cap(&self) {
        let binding = self.binding();
        let mut args =
            json!({"_session_id":binding.session_id(),"scope":"desktop","x":100.0,"y":100.0});
        crate::desktop_capture_scale::map_desktop_args_for_binding(&mut args, Some(&binding));
        assert_eq!(
            (args["x"].as_f64(), args["y"].as_f64()),
            (Some(100.0), Some(100.0))
        );
    }
}

#[tokio::test]
async fn actual_registry_first_limited_capped_and_uncapped_capture_never_dispatches_input() {
    for provider in [
        "wrong-peer equal-size fallback",
        "missing-plugin limited authority",
    ] {
        for capped in [false, true] {
            let f = fixture();
            assert_ne!(
                f.input().await.is_error,
                Some(true),
                "raw input before any image remains supported"
            );
            f.hits.store(0, Ordering::SeqCst);
            assert_ne!(
                f.capture(false, capped, provider).await.is_error,
                Some(true)
            );
            f.assert_no_legacy_cap();
            let result = f.input().await;
            assert_eq!(result.is_error, Some(true));
            assert_eq!(
                result.structured_content.unwrap()["code"],
                "capture_frame_mismatch"
            );
            assert_eq!(f.hits.load(Ordering::SeqCst), 0);
        }
    }
}

#[tokio::test]
async fn actual_registry_qualified_limited_qualified_refresh_keeps_cap_and_authority_together() {
    let f = fixture();
    assert_ne!(
        f.capture(true, true, "qualified native").await.is_error,
        Some(true)
    );
    assert_ne!(f.input().await.is_error, Some(true));
    assert_eq!(f.point.lock().unwrap().as_ref().unwrap()["x"], 500.0);
    assert_ne!(
        f.capture(false, true, "wrong peer").await.is_error,
        Some(true)
    );
    f.assert_no_legacy_cap();
    f.hits.store(0, Ordering::SeqCst);
    assert_eq!(f.input().await.is_error, Some(true));
    assert_eq!(f.hits.load(Ordering::SeqCst), 0);
    assert_ne!(
        f.capture(true, true, "qualified original peer")
            .await
            .is_error,
        Some(true)
    );
    assert_ne!(f.input().await.is_error, Some(true));
    assert_eq!(f.point.lock().unwrap().as_ref().unwrap()["x"], 500.0);
}

#[tokio::test]
async fn actual_registry_reverse_visual_completion_cannot_revoke_newer_qualified_capture() {
    let f = fixture();
    let registry = f.registry.clone();
    let old = tokio::spawn(async move {
        registry.invoke_from_trusted_adapter("get_desktop_state",json!({"_session_id":"visual-availability","qualified":false,"capped":true,"wait":true})).await
    });
    f.entered.notified().await;
    assert_ne!(
        f.capture(true, true, "newer qualified").await.is_error,
        Some(true)
    );
    f.release.notify_one();
    assert_ne!(old.await.unwrap().is_error, Some(true));
    assert_ne!(f.input().await.is_error, Some(true));
    assert_eq!(f.point.lock().unwrap().as_ref().unwrap()["x"], 500.0);
}

#[tokio::test]
async fn actual_registry_retired_visual_completion_cannot_install_cap_or_tombstone_for_successor() {
    let f = fixture();
    let registry = f.registry.clone();
    let old = tokio::spawn(async move {
        registry.invoke_from_trusted_adapter("get_desktop_state",json!({"_session_id":"visual-availability","qualified":false,"capped":true,"wait":true})).await
    });
    f.entered.notified().await;
    let binding = f.binding();
    f.service.retire_session(&binding);
    f.release.notify_one();
    old.await.unwrap();
    assert!(!f.service.desktop_observation_unavailable(&binding));
    let successor = f.service.current_binding(binding.session_id()).unwrap();
    assert!(!f.service.desktop_observation_unavailable(&successor));
    let mut point =
        json!({"_session_id":binding.session_id(),"scope":"desktop","x":100.0,"y":100.0});
    crate::desktop_capture_scale::map_desktop_args_for_binding(&mut point, Some(&successor));
    assert_eq!(point["x"], 100.0);
}
