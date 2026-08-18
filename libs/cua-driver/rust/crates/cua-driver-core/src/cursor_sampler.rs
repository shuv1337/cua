//! Cross-platform cursor-position sampler. Runs on a dedicated thread
//! during recording, polls the OS for the current mouse position every
//! ~33 ms (≈30 Hz to match the video framerate), and writes one
//! `{t_ms, x, y}` JSON object per line to `<output_dir>/cursor.jsonl`.
//!
//! Reference: `libs/cua-driver/swift/Sources/CuaDriverCore/Recording/CursorSampler.swift`
//!
//! Per-platform polling:
//! - **Windows:** `GetCursorPos` (returns physical screen coords)
//! - **macOS:** `CGEventCreate` + `CGEventGetLocation`
//! - **Linux X11:** `XQueryPointer` against the root window
//! - **Linux Wayland:** no portable API exists; sampler runs but logs
//!   no samples — the resulting cursor.jsonl is empty and the zoom
//!   renderer falls back to the click-point-only path.

use std::fs::File;
use std::io::{BufWriter, Write};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

/// Sampling rate in Hz. 30 matches the video framerate, so the
/// per-frame zoom curve can resolve cursor position at frame
/// granularity without interpolation noise.
pub const SAMPLE_RATE_HZ: u32 = 30;

/// Final tallies from one sampler run, surfaced in `session.json` so an
/// apparent sub-30 Hz sample rate is self-explaining instead of looking like
/// sampler loss.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct CursorStats {
    /// Polls that resolved a position and were written to `cursor.jsonl`.
    pub samples: usize,
    /// Polls that resolved a position lying outside the recorded capture
    /// surface. These are deliberately not written — the renderer has no
    /// frame to place them on — and are counted separately so they are not
    /// confused with an unavailable cursor API.
    pub outside_capture_surface: usize,
    /// Polls where the platform could not report a cursor position at all
    /// (no portable API for this session type, or a failed query).
    pub unavailable: usize,
    /// JSONL writes (including the final buffered flush) that failed. This is
    /// additive telemetry; `samples` only counts records accepted by the
    /// writer.
    pub write_failures: usize,
}

fn write_sample(writer: &mut impl Write, t_ms: f64, x: f64, y: f64) -> bool {
    writeln!(
        writer,
        "{{\"t_ms\":{:.3},\"x\":{:.2},\"y\":{:.2}}}",
        t_ms, x, y
    )
    .is_ok()
}

/// One poll outcome. `OutsideCaptureSurface` is distinguished from
/// `Unavailable` so a platform that CAN read the cursor but has it off the
/// recorded surface is not reported as having no cursor API.
enum CursorPoll {
    /// Not constructed on targets whose poll cannot resolve a position at
    /// all (Linux today, where neither X11 nor Wayland is wired up here).
    #[allow(dead_code)]
    At(f64, f64),
    /// No current platform poll resolves a position it can also place
    /// outside the recorded surface, so this arm is constructed only by a
    /// platform poll that gains that knowledge (for example a compositor
    /// query that reports monitor-relative coordinates).
    #[allow(dead_code)]
    OutsideCaptureSurface,
    Unavailable,
}

/// One running cursor sampler. Drop or `stop()` to terminate.
pub struct CursorSampler {
    handle: Option<JoinHandle<CursorStats>>,
    stop_flag: Arc<AtomicBool>,
    output_path: PathBuf,
}

impl CursorSampler {
    /// Start sampling. Writes JSON-line records to `output_path` from a
    /// background thread. `session_start` is used as the time anchor —
    /// `t_ms` in each sample is `(now - session_start).as_millis()`.
    pub fn start(output_path: PathBuf, session_start: Instant) -> std::io::Result<Self> {
        let file = File::create(&output_path)?;
        let stop_flag = Arc::new(AtomicBool::new(false));
        let flag_for_thread = stop_flag.clone();
        let path_for_thread = output_path.clone();
        let handle = std::thread::spawn(move || {
            let mut writer = BufWriter::new(file);
            let interval = Duration::from_millis(1000 / SAMPLE_RATE_HZ as u64);
            let mut stats = CursorStats::default();
            while !flag_for_thread.load(Ordering::Relaxed) {
                match sample_cursor() {
                    CursorPoll::At(x, y) => {
                        let t_ms = session_start.elapsed().as_millis() as f64;
                        // Write one JSON object per line. We hand-format
                        // the trivial shape rather than pulling serde_json
                        // into the hot loop — keeps wakeup-cost bounded.
                        if write_sample(&mut writer, t_ms, x, y) {
                            stats.samples += 1;
                        } else {
                            stats.write_failures += 1;
                        }
                    }
                    CursorPoll::OutsideCaptureSurface => stats.outside_capture_surface += 1,
                    CursorPoll::Unavailable => stats.unavailable += 1,
                }
                std::thread::sleep(interval);
            }
            if writer.flush().is_err() {
                stats.write_failures += 1;
            }
            let _ = path_for_thread; // keep path moved (warning silencer)
            stats
        });
        Ok(CursorSampler {
            handle: Some(handle),
            stop_flag,
            output_path,
        })
    }

    /// Stop the sampler. Returns the written / skipped tallies.
    pub fn stop(mut self) -> CursorStats {
        self.stop_flag.store(true, Ordering::Relaxed);
        self.handle
            .take()
            .and_then(|h| h.join().ok())
            .unwrap_or_default()
    }

    pub fn output_path(&self) -> &std::path::Path {
        &self.output_path
    }
}

impl Drop for CursorSampler {
    fn drop(&mut self) {
        self.stop_flag.store(true, Ordering::Relaxed);
        if let Some(h) = self.handle.take() {
            let _ = h.join();
        }
    }
}

// ── per-platform cursor poll ────────────────────────────────────────────────

#[cfg(target_os = "windows")]
fn sample_cursor() -> CursorPoll {
    use windows::Win32::Foundation::POINT;
    use windows::Win32::UI::WindowsAndMessaging::GetCursorPos;
    unsafe {
        let mut p = POINT::default();
        if GetCursorPos(&mut p).is_ok() {
            CursorPoll::At(p.x as f64, p.y as f64)
        } else {
            CursorPoll::Unavailable
        }
    }
}

#[cfg(target_os = "macos")]
fn sample_cursor() -> CursorPoll {
    // ApplicationServices/CGEvent.h: CGEventCreate(nil) → CGEventRef;
    // CGEventGetLocation(event) → CGPoint. The point is in points
    // (top-left origin) so it matches the cursor-space convention the
    // renderer uses.
    #[link(name = "ApplicationServices", kind = "framework")]
    extern "C" {
        fn CGEventCreate(source: *mut std::ffi::c_void) -> *mut std::ffi::c_void;
        fn CGEventGetLocation(event: *mut std::ffi::c_void) -> CGPoint;
    }
    #[link(name = "CoreFoundation", kind = "framework")]
    extern "C" {
        fn CFRelease(cf: *mut std::ffi::c_void);
    }
    #[repr(C)]
    #[derive(Copy, Clone)]
    struct CGPoint {
        x: f64,
        y: f64,
    }

    unsafe {
        let event = CGEventCreate(std::ptr::null_mut());
        if event.is_null() {
            return CursorPoll::Unavailable;
        }
        let p = CGEventGetLocation(event);
        CFRelease(event);
        CursorPoll::At(p.x, p.y)
    }
}

#[cfg(target_os = "linux")]
fn sample_cursor() -> CursorPoll {
    // Wayland has no equivalent portable poll; on X11 use XQueryPointer.
    // We try the X11 path via the `x11` crate if available; otherwise
    // return None and the sampler writes an empty cursor.jsonl.
    //
    // The X11 dep isn't always present in cua-driver's Linux build
    // (Wayland-only hosts), so this fallback is "no-op when X11 isn't
    // wired up" — the renderer copes by falling back to click-point-
    // only zoom (no cursor-follow between actions). Reported as
    // unavailable rather than off-surface: no position was resolved.
    CursorPoll::Unavailable
}

#[cfg(not(any(target_os = "windows", target_os = "macos", target_os = "linux")))]
fn sample_cursor() -> CursorPoll {
    CursorPoll::Unavailable
}

#[cfg(test)]
mod tests {
    use super::*;

    struct FailingWriter;

    impl Write for FailingWriter {
        fn write(&mut self, _buf: &[u8]) -> std::io::Result<usize> {
            Err(std::io::Error::other("fixture write failure"))
        }

        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    #[test]
    fn failed_jsonl_write_is_not_counted_as_a_sample() {
        let mut writer = FailingWriter;
        let mut stats = CursorStats::default();

        if write_sample(&mut writer, 1.0, 2.0, 3.0) {
            stats.samples += 1;
        } else {
            stats.write_failures += 1;
        }

        assert_eq!(stats.samples, 0);
        assert_eq!(stats.write_failures, 1);
    }
}
