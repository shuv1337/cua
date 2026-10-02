//! Immutable geometry and native authority for one recording turn.
//!
//! Recording failure is evidence failure, never an action refusal or a reason
//! to repeat input. Native identity is optional: legacy adapters cannot claim
//! the same attestation as a qualified compositor snapshot.

use std::sync::Arc;

use crate::capture_registry::{
    CaptureActionAdmission, CaptureBinding, CaptureFrameIdentity, CaptureTarget,
};
use crate::recording::ScreenshotCapture;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RecordingUnavailable {
    StaleFrame,
    BudgetExceeded,
    InvalidFrame,
    CaptureFailed,
    AdmissionUnavailable,
    UnqualifiedFrame,
}

impl RecordingUnavailable {
    pub fn classification(self) -> &'static str {
        match self {
            Self::StaleFrame => "recording_stale_frame",
            Self::BudgetExceeded => "recording_budget_exceeded",
            Self::InvalidFrame => "recording_invalid_frame",
            Self::CaptureFailed => "capture_failed",
            Self::AdmissionUnavailable => "recording_admission_unavailable",
            Self::UnqualifiedFrame => "recording_coordinate_attestation_unavailable",
        }
    }
}

/// Coordinates and image dimensions are immutable for the entire turn.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RecordingFrame {
    pub binding: Option<CaptureBinding>,
    pub identity: Option<CaptureFrameIdentity>,
    pub origin: (i32, i32),
    pub dimensions: (u32, u32),
    pub window: Option<RecordingWindow>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RecordingWindow {
    pub pid: u32,
    pub window_id: u64,
    pub origin: (i32, i32),
    pub dimensions: (u32, u32),
}

pub const MAX_RECORDING_PIXELS: u64 = 64 * 1024 * 1024;
pub const MAX_RECORDING_PNG_BYTES: usize = (MAX_RECORDING_PIXELS * 5) as usize;

impl RecordingFrame {
    pub fn validate(&self) -> Result<(), RecordingUnavailable> {
        let (w, h) = self.dimensions;
        if w == 0 || h == 0 || u64::from(w) * u64::from(h) > MAX_RECORDING_PIXELS {
            return Err(RecordingUnavailable::InvalidFrame);
        }
        Ok(())
    }

    pub fn layout_point(&self, x: f64, y: f64) -> Result<(f64, f64), RecordingUnavailable> {
        self.image_point(x - f64::from(self.origin.0), y - f64::from(self.origin.1))
    }

    pub fn image_point(&self, x: f64, y: f64) -> Result<(f64, f64), RecordingUnavailable> {
        self.validate()?;
        if !x.is_finite()
            || !y.is_finite()
            || x < 0.0
            || y < 0.0
            || x >= f64::from(self.dimensions.0)
            || y >= f64::from(self.dimensions.1)
        {
            return Err(RecordingUnavailable::InvalidFrame);
        }
        Ok((x, y))
    }

    /// Uses only the consumed admission's result, not a registry lookup, raw
    /// screenshot coordinates or another application of its affine transform.
    pub fn admitted_point(
        &self,
        admission: &CaptureActionAdmission,
    ) -> Result<(f64, f64), RecordingUnavailable> {
        self.action_point(
            admission.frame_identity.as_ref(),
            &admission.target,
            (
                admission.native_action_dimensions.width(),
                admission.native_action_dimensions.height(),
            ),
            admission.action_x,
            admission.action_y,
        )
    }

    pub fn action_point(
        &self,
        identity: Option<&CaptureFrameIdentity>,
        target: &CaptureTarget,
        native: (u32, u32),
        x: f64,
        y: f64,
    ) -> Result<(f64, f64), RecordingUnavailable> {
        if identity != self.identity.as_ref() {
            return Err(RecordingUnavailable::StaleFrame);
        }
        match target {
            CaptureTarget::PrimaryDesktop if native == self.dimensions => self.image_point(x, y),
            CaptureTarget::Window { pid, window_id } => {
                let window = self
                    .window
                    .as_ref()
                    .filter(|w| {
                        w.pid == *pid && w.window_id == *window_id && w.dimensions == native
                    })
                    .ok_or(RecordingUnavailable::StaleFrame)?;
                if x < 0.0 || y < 0.0 || x >= f64::from(native.0) || y >= f64::from(native.1) {
                    return Err(RecordingUnavailable::InvalidFrame);
                }
                self.layout_point(
                    f64::from(window.origin.0) + x,
                    f64::from(window.origin.1) + y,
                )
            }
            _ => Err(RecordingUnavailable::StaleFrame),
        }
    }

    pub fn check_png(&self, png: &[u8]) -> Result<(), RecordingUnavailable> {
        self.validate()?;
        if png.len() > MAX_RECORDING_PNG_BYTES
            || crate::image_utils::png_dimensions(png).ok() != Some(self.dimensions)
        {
            return Err(RecordingUnavailable::InvalidFrame);
        }
        Ok(())
    }
}

/// Adapter-owned immutable snapshot; every operation fences against this same
/// authority. A stale after phase must not capture the successor frame.
pub trait RecordingTurnContext: Send + Sync {
    fn frame(&self) -> &RecordingFrame;
    fn capture(&self, deadline: std::time::Instant) -> ScreenshotCapture;
    fn click_point(&self) -> Result<Option<(f64, f64)>, RecordingUnavailable>;
    fn validate_point(&self, point: (f64, f64)) -> Result<(f64, f64), RecordingUnavailable>;
    fn cursor_point(&self) -> Result<Option<(f64, f64)>, RecordingUnavailable> {
        Ok(None)
    }
}

pub type SharedRecordingTurn = Arc<dyn RecordingTurnContext>;
