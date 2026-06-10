//! Linux video-recording backend.
//!
//! On Wayland (Hyprland) display-level capture is the wrong source for
//! background automation: the target usually lives on `special:cua`, and when
//! the physical session is locked, the compositor output is just the lock
//! surface. Instead of streaming the display, the Wayland backend synthesizes
//! `recording.mp4` at stop time from the recorder's own per-turn
//! target-window screenshots (`turn-*/click.png` or `screenshot.png`,
//! filtered to turns whose arguments carry `pid`/`window_id` — the same
//! fields the recorder uses to pick its screenshot source).
//!
//! Timing model: the mp4 is testing evidence, not a render/zoom input. Each
//! frame is shown from its turn's `t_ms_from_session_start` until the next
//! turn's, with a 250 ms display floor, so video time tracks turn timestamps
//! only approximately — bursts of sub-floor turns push later frames past
//! their timestamps, and `VideoMetadata.duration_ms` (wall time) can differ
//! from the encoded duration accordingly.
//!
//! `stop()` runs while the daemon-wide recording mutex is held (see
//! `RecordingState::stop_owner`), so the encode is strictly bounded: ffmpeg
//! is killed at `ENCODE_DEADLINE` and the failure lands in
//! `VideoMetadata.error` instead of wedging every recorded tool call.
//!
//! X11 sessions still use the core ffmpeg x11grab backend.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use cua_driver_core::video::{VideoBackend, VideoBackendFactory, VideoMetadata};

/// Output fps for the synthesized trajectory mp4. Content is static between
/// turns, so a low rate loses nothing while keeping the stop-time encode —
/// which runs under the daemon-wide recording mutex — short.
const FPS: u32 = 10;
/// Hard ceiling on the stop-time ffmpeg encode. stop() holds the daemon-wide
/// recording mutex, so a wedged or pathologically slow encode must be killed
/// rather than waited on; the timeout surfaces in `VideoMetadata.error`.
const ENCODE_DEADLINE: Duration = Duration::from_secs(60);

pub struct LinuxVideoBackendFactory;

impl VideoBackendFactory for LinuxVideoBackendFactory {
    fn start(&self, output_path: &Path) -> Result<Box<dyn VideoBackend>> {
        if std::env::var_os("WAYLAND_DISPLAY").is_some() {
            // Do not record compositor output on Wayland. Hidden-workspace
            // automation and locked desktops both make the focused monitor an
            // unrelated source. Build the mp4 from target-window turn frames.
            return ScreenshotSequenceVideoBackend::start(output_path)
                .map(|b| Box::new(b) as Box<dyn VideoBackend>)
                .map_err(|e| e.context("Wayland trajectory video failed"));
        }
        cua_driver_core::video_ffmpeg::FfmpegVideoBackendFactory.start(output_path)
    }
}

#[derive(Debug, Clone)]
struct TurnFrame {
    path: PathBuf,
    t_ms: u64,
}

/// Lock-safe Linux Wayland video backend. It starts cheaply and creates the
/// mp4 on stop after all `turn-*` folders have been written.
struct ScreenshotSequenceVideoBackend {
    output_path: PathBuf,
    started_at: Instant,
    ffmpeg: PathBuf,
}

impl ScreenshotSequenceVideoBackend {
    fn start(output_path: &Path) -> Result<Self> {
        let ffmpeg = cua_driver_core::video_ffmpeg::find_ffmpeg().context(
            "ffmpeg not found on PATH. Install with: apt install ffmpeg (Debian/Ubuntu) \
             or pacman -S ffmpeg (Arch).",
        )?;
        if let Some(parent) = output_path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| {
                anyhow::anyhow!(
                    "failed to create recording output directory {}: {e}",
                    parent.display()
                )
            })?;
        }
        Ok(Self {
            output_path: output_path.to_path_buf(),
            started_at: Instant::now(),
            ffmpeg,
        })
    }
}

impl VideoBackend for ScreenshotSequenceVideoBackend {
    fn stop(self: Box<Self>) -> Result<VideoMetadata> {
        let elapsed = self.started_at.elapsed();
        let duration_ms = elapsed.as_millis() as u64;
        let session_dir = self
            .output_path
            .parent()
            .map(Path::to_path_buf)
            .unwrap_or_else(|| PathBuf::from("."));

        let frames = collect_target_window_frames(&session_dir);
        if frames.is_empty() {
            let error = "no target-window screenshots were recorded; Linux Wayland video avoids \
                         display capture because locked desktops only expose the lock screen"
                .to_string();
            return Ok(VideoMetadata {
                path: self.output_path,
                duration_ms,
                finalized: false,
                error: Some(error),
            });
        }

        let result =
            encode_screenshot_sequence(&self.ffmpeg, &self.output_path, &frames, duration_ms);
        Ok(VideoMetadata {
            path: self.output_path,
            duration_ms,
            finalized: result.is_ok(),
            error: result.err().map(|e| e.to_string()),
        })
    }
}

fn collect_target_window_frames(session_dir: &Path) -> Vec<TurnFrame> {
    let mut frames = Vec::new();
    let Ok(entries) = std::fs::read_dir(session_dir) else {
        return frames;
    };
    for entry in entries.flatten() {
        let Ok(file_type) = entry.file_type() else {
            continue;
        };
        if !file_type.is_dir() {
            continue;
        }
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if !name.starts_with("turn-") {
            continue;
        }
        let turn_dir = entry.path();
        let screenshot = preferred_turn_frame_path(&turn_dir);
        if !screenshot.exists() {
            continue;
        }
        let Some((t_ms, target_window)) = turn_timestamp_and_target(&turn_dir.join("action.json"))
        else {
            continue;
        };
        if target_window {
            frames.push(TurnFrame {
                path: screenshot,
                t_ms,
            });
        }
    }
    frames.sort_by(|a, b| a.t_ms.cmp(&b.t_ms).then_with(|| a.path.cmp(&b.path)));
    frames
}

fn preferred_turn_frame_path(turn_dir: &Path) -> PathBuf {
    let click = turn_dir.join("click.png");
    if click.exists() {
        click
    } else {
        turn_dir.join("screenshot.png")
    }
}

fn turn_timestamp_and_target(action_path: &Path) -> Option<(u64, bool)> {
    let text = std::fs::read_to_string(action_path).ok()?;
    let json: serde_json::Value = serde_json::from_str(&text).ok()?;
    let t_ms = json
        .get("t_ms_from_session_start")
        .and_then(|v| v.as_u64())
        .unwrap_or(0);
    let args = json.get("arguments")?;
    let target_window = args.get("window_id").is_some() || args.get("pid").is_some();
    Some((t_ms, target_window))
}

fn encode_screenshot_sequence(
    ffmpeg: &Path,
    output_path: &Path,
    frames: &[TurnFrame],
    duration_ms: u64,
) -> Result<()> {
    let durations = frame_durations_ms(frames, duration_ms);
    let (width, height) = output_dimensions(frames)?;

    if let Some(parent) = output_path.parent() {
        std::fs::create_dir_all(parent)?;
    }

    let mut cmd = Command::new(ffmpeg);
    cmd.arg("-y").arg("-loglevel").arg("error");
    for (frame, duration) in frames.iter().zip(durations.iter()) {
        cmd.arg("-loop")
            .arg("1")
            .arg("-t")
            .arg(format_duration(*duration))
            .arg("-i")
            .arg(&frame.path);
    }

    let mut filter_parts = Vec::with_capacity(frames.len() + 1);
    for i in 0..frames.len() {
        filter_parts.push(format!(
            "[{i}:v]scale={width}:{height}:force_original_aspect_ratio=decrease,\
             pad={width}:{height}:(ow-iw)/2:(oh-ih)/2,setsar=1,format=rgba[v{i}]"
        ));
    }
    let inputs = (0..frames.len())
        .map(|i| format!("[v{i}]"))
        .collect::<String>();
    filter_parts.push(format!(
        "{inputs}concat=n={}:v=1:a=0,format=yuv420p[v]",
        frames.len()
    ));
    let filter_complex = filter_parts.join(";");

    cmd.arg("-filter_complex")
        .arg(filter_complex)
        .arg("-map")
        .arg("[v]")
        .arg("-r")
        .arg(FPS.to_string())
        .arg("-c:v")
        .arg("libx264")
        .arg("-preset")
        .arg("ultrafast")
        .arg("-pix_fmt")
        .arg("yuv420p")
        .arg("-movflags")
        .arg("+faststart")
        .arg("-g")
        .arg(FPS.to_string())
        .arg(output_path)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped());

    let mut child = cmd
        .spawn()
        .context("spawn ffmpeg for screenshot-sequence recording")?;
    let stderr_thread = child
        .stderr
        .take()
        .map(cua_driver_core::video_ffmpeg::spawn_stderr_drain);

    // Bounded wait: stop() runs under the daemon-wide recording mutex, so a
    // wedged encode must be killed, never waited on indefinitely.
    let deadline = Instant::now() + ENCODE_DEADLINE;
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) => {
                if Instant::now() > deadline {
                    let _ = child.kill();
                    let _ = child.wait();
                    anyhow::bail!(
                        "ffmpeg screenshot-sequence encode exceeded {}s ({} frames); \
                         killed to avoid blocking the recording mutex",
                        ENCODE_DEADLINE.as_secs(),
                        frames.len()
                    );
                }
                std::thread::sleep(Duration::from_millis(50));
            }
            Err(e) => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(e).context("wait on ffmpeg screenshot-sequence encode");
            }
        }
    };
    if status.success() {
        return Ok(());
    }
    let stderr_tail = stderr_thread
        .and_then(|h| h.join().ok())
        .map(|buf| String::from_utf8_lossy(&buf).into_owned())
        .unwrap_or_default();
    anyhow::bail!("ffmpeg screenshot-sequence encode failed ({status}): {stderr_tail}");
}

fn frame_durations_ms(frames: &[TurnFrame], total_duration_ms: u64) -> Vec<u64> {
    frames
        .iter()
        .enumerate()
        .map(|(i, frame)| {
            let start = if i == 0 { 0 } else { frame.t_ms };
            let end = frames
                .get(i + 1)
                .map(|next| next.t_ms)
                .unwrap_or(total_duration_ms);
            end.saturating_sub(start).max(250)
        })
        .collect()
}

fn output_dimensions(frames: &[TurnFrame]) -> Result<(u32, u32)> {
    let mut width = 0;
    let mut height = 0;
    for frame in frames {
        let bytes = std::fs::read(&frame.path)
            .with_context(|| format!("read frame {}", frame.path.display()))?;
        let (w, h) = cua_driver_core::image_utils::png_dimensions(&bytes)
            .with_context(|| format!("read PNG dimensions for {}", frame.path.display()))?;
        width = width.max(w);
        height = height.max(h);
    }
    if width == 0 || height == 0 {
        anyhow::bail!("no valid screenshot frames found");
    }
    if width % 2 != 0 {
        width += 1;
    }
    if height % 2 != 0 {
        height += 1;
    }
    Ok((width, height))
}

fn format_duration(ms: u64) -> String {
    format!("{:.3}", ms as f64 / 1000.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir(name: &str) -> PathBuf {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let dir = std::env::temp_dir().join(format!(
            "cua-linux-video-{name}-{}-{nanos}",
            std::process::id()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn write_png(path: &Path, width: u32, height: u32) {
        let rgba = vec![0x80; (width * height * 4) as usize];
        let png = cua_driver_core::image_utils::encode_rgba_to_png(&rgba, width, height).unwrap();
        std::fs::write(path, png).unwrap();
    }

    fn write_turn(root: &Path, idx: u32, t_ms: u64, args: serde_json::Value, click_png: bool) {
        let turn = root.join(format!("turn-{idx:05}"));
        std::fs::create_dir_all(&turn).unwrap();
        let payload = serde_json::json!({
            "tool": "click",
            "arguments": args,
            "result_summary": "ok",
            "timestamp": "0.000",
            "t_ms_from_session_start": t_ms,
            "t_start_ms_from_session_start": t_ms,
        });
        std::fs::write(
            turn.join("action.json"),
            serde_json::to_vec_pretty(&payload).unwrap(),
        )
        .unwrap();
        write_png(&turn.join("screenshot.png"), 3, 5);
        if click_png {
            write_png(&turn.join("click.png"), 5, 7);
        }
    }

    #[test]
    fn collect_target_window_frames_skips_display_only_turns() {
        let dir = temp_dir("collect");
        write_turn(&dir, 1, 100, serde_json::json!({"name":"zenity"}), false);
        write_turn(
            &dir,
            2,
            250,
            serde_json::json!({"pid":42,"window_id":99}),
            true,
        );
        write_turn(&dir, 3, 500, serde_json::json!({"pid":42}), false);

        let frames = collect_target_window_frames(&dir);
        assert_eq!(frames.len(), 2);
        assert_eq!(frames[0].t_ms, 250);
        assert_eq!(frames[1].t_ms, 500);
        assert_eq!(frames[0].path.file_name().unwrap(), "click.png");

        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn frame_durations_start_first_target_at_zero_and_have_floor() {
        let frames = vec![
            TurnFrame {
                path: PathBuf::from("a.png"),
                t_ms: 5_000,
            },
            TurnFrame {
                path: PathBuf::from("b.png"),
                t_ms: 5_100,
            },
            TurnFrame {
                path: PathBuf::from("c.png"),
                t_ms: 8_000,
            },
        ];
        assert_eq!(
            frame_durations_ms(&frames, 9_000),
            vec![5_100, 2_900, 1_000]
        );
    }

    #[test]
    fn output_dimensions_uses_max_even_extent() {
        let dir = temp_dir("dimensions");
        let a = dir.join("a.png");
        let b = dir.join("b.png");
        write_png(&a, 3, 5);
        write_png(&b, 8, 6);
        let frames = vec![
            TurnFrame { path: a, t_ms: 0 },
            TurnFrame { path: b, t_ms: 1 },
        ];
        assert_eq!(output_dimensions(&frames).unwrap(), (8, 6));
        let _ = std::fs::remove_dir_all(dir);
    }
}
