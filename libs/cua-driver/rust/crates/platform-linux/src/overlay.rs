//! Linux agent-cursor overlay — X11 RGBA override-redirect window.
//!
//! Architecture:
//! - Creates an override-redirect (non-reparented) X11 window with 32-bit ARGB visual
//!   from XComposite.  The window covers the full display area.
//! - A background thread renders frames at ~60 Hz using tiny-skia and XShmPutImage
//!   (or XPutImage fallback) with XRender ARGB compositing.
//! - Mouse events pass through via `XShapeSelectInput(ShapeInput, empty-region)`.
//! - Z-ordering: `XRaiseWindow` every 80ms to stay above normal windows.
//! - Wayland: when WAYLAND_DISPLAY is set but DISPLAY is also available (XWayland),
//!   the X11 path is used.  Pure Wayland support is a TODO.
//!
//! ## Cross-platform note (2026-05 dedup audit)
//!
//! Animation state + render pipeline live in `cursor_overlay::render_state`
//! (`RenderStateCore`, `tick_motion`, `apply_command_base`, `render_frame`).
//! What stays here is the X11 window plumbing: connection setup,
//! override-redirect visual, ShapeInput passthrough, and the XPutImage paint.

use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

use cursor_overlay::{CursorConfig, OverlayCommand, RenderStateCore};
#[cfg(target_os = "linux")]
use cursor_overlay::ZOrderEnforcer;

// ── Global channel ────────────────────────────────────────────────────────

static CMD_TX: OnceLock<std::sync::mpsc::SyncSender<OverlayCommand>> = OnceLock::new();
static CMD_RX_CELL: Mutex<Option<std::sync::mpsc::Receiver<OverlayCommand>>> = Mutex::new(None);
static RENDER: Mutex<Option<RenderState>> = Mutex::new(None);
static ARRIVAL_TX: Mutex<Option<tokio::sync::oneshot::Sender<()>>> = Mutex::new(None);

/// Set by [`stop`] to tell the render thread to tear down its X11 window and
/// exit. The render loop checks it every iteration (and is nudged awake out of
/// its quiescent wait), so teardown is observed within one poll interval.
static STOP: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
/// Join handle for the spawned `cua-overlay-x11` thread, so [`stop`] can wait
/// for the window to actually be destroyed before returning.
static OVERLAY_THREAD: Mutex<Option<std::thread::JoinHandle<()>>> = Mutex::new(None);

pub fn init(cfg: CursorConfig) {
    // No agent-cursor overlay in headless-X mode: there is no user watching
    // the off-screen Xvfb, and a full-desktop ARGB override window on an
    // UNCOMPOSITED Xvfb draws black over everything in `import -window root`
    // captures (#18). Skipping init leaves the overlay fully inert.
    if crate::headless_x::is_active() {
        return;
    }
    let (tx, rx) = std::sync::mpsc::sync_channel(4096);
    let _ = CMD_TX.set(tx);
    *CMD_RX_CELL.lock().unwrap() = Some(rx);
    *RENDER.lock().unwrap() = Some(RenderState::new(cfg));
}

pub fn send_command(cmd: OverlayCommand) {
    if crate::headless_x::is_active() {
        return;
    }
    if let Some(tx) = CMD_TX.get() {
        let _ = tx.try_send(cmd);
    }
}

pub fn is_enabled() -> bool {
    RENDER.lock().ok()
        .and_then(|g| g.as_ref().map(|rs| rs.core.visible))
        .unwrap_or(false)
}

pub fn current_position() -> (f64, f64) {
    RENDER.lock().ok()
        .and_then(|g| g.as_ref().map(|rs| rs.core.pos))
        .unwrap_or((-200.0, -200.0))
}

pub async fn animate_cursor_to(x: f64, y: f64) {
    let should_animate = {
        let guard = RENDER.lock().unwrap();
        match guard.as_ref() {
            Some(rs) if rs.core.cfg.enabled && rs.core.visible && rs.core.pos.0 > -50.0 => true,
            _ => false,
        }
    };
    if !should_animate {
        return;
    }

    let (tx, rx) = tokio::sync::oneshot::channel::<()>();
    {
        let mut guard = ARRIVAL_TX.lock().unwrap();
        if let Some(old_tx) = guard.take() {
            let _ = old_tx.send(());
        }
        *guard = Some(tx);
    }

    send_command(OverlayCommand::MoveTo {
        x,
        y,
        end_heading_radians: std::f64::consts::FRAC_PI_4,
    });

    let _ = rx.await;
}

/// Spawn the overlay on a dedicated thread.  Non-blocking.
pub fn run_on_thread() {
    let rx = match CMD_RX_CELL.lock().unwrap().take() {
        Some(r) => r,
        None => return,
    };

    let cfg = {
        let guard = RENDER.lock().unwrap();
        match &*guard {
            Some(rs) => rs.core.cfg.clone(),
            None => return,
        }
    };

    if !cfg.enabled {
        return;
    }

    STOP.store(false, std::sync::atomic::Ordering::Release);
    let handle = std::thread::Builder::new()
        .name("cua-overlay-x11".into())
        .spawn(move || {
            run_overlay_thread(cfg, rx);
        })
        .expect("spawn overlay thread");
    *OVERLAY_THREAD.lock().unwrap() = Some(handle);
}

/// Stop the overlay render thread and destroy its X11 window.
///
/// Wired into daemon shutdown so the long-lived overlay thread is torn down
/// deterministically instead of relying on process exit to reap it. Idempotent
/// and terminal: a no-op if the overlay never started or already stopped, and
/// NOT a pause — the `CMD_TX`/`CMD_RX_CELL` channel is process-lifetime
/// (`OnceLock`), so there is no supported restart after `stop`.
///
/// Sets the STOP flag, nudges the thread out of its quiescent `recv_timeout`
/// with a benign command so it observes the flag immediately, then joins it —
/// the thread destroys the override-redirect window on its way out.
pub fn stop() {
    if crate::headless_x::is_active() {
        return;
    }
    STOP.store(true, std::sync::atomic::Ordering::Release);
    if let Some(tx) = CMD_TX.get() {
        // Benign wake; the thread breaks on the STOP check before applying it.
        let _ = tx.try_send(OverlayCommand::SetEnabled(false));
    }
    if let Some(handle) = OVERLAY_THREAD.lock().unwrap().take() {
        let _ = handle.join();
    }
}

/// Cheap hash of everything that affects the rendered pixels, used by the
/// render loop to skip the full-screen `render_frame` + BGRA byte-swap +
/// `XPutImage` when the visual is unchanged. Floats are quantised so sub-pixel
/// jitter doesn't force a repaint while genuine motion (≥¼px), heading, fade,
/// and click-pulse changes still do. Discrete style/shape/palette/motion
/// changes arrive as commands and force a repaint directly (see the loop), so
/// they need not all be folded in here.
#[cfg(target_os = "linux")]
fn paint_signature(core: &RenderStateCore) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    core.visible.hash(&mut h);
    ((core.pos.0 * 4.0).round() as i64).hash(&mut h);
    ((core.pos.1 * 4.0).round() as i64).hash(&mut h);
    ((core.heading * 256.0).round() as i64).hash(&mut h);
    ((core.idle_alpha * 255.0).round() as i64).hash(&mut h);
    core.click_t.map(|t| (t * 255.0).round() as i64).hash(&mut h);
    core.shape.is_some().hash(&mut h);
    core.gradient_colors.hash(&mut h);
    core.bloom_override.hash(&mut h);
    h.finish()
}

// ── Animation state ───────────────────────────────────────────────────────
//
// The platform-agnostic fields + tick + apply_command + render pipeline live
// in `cursor_overlay::render_state` (2026-05 dedup audit). What stays here
// is the X11-specific screen dimensions.

struct RenderState {
    core: RenderStateCore,
    /// X11 screen dimensions in pixels (populated after XOpenDisplay).
    scr_w: u32,
    scr_h: u32,
}

impl RenderState {
    fn new(cfg: CursorConfig) -> Self {
        RenderState {
            core: RenderStateCore::new(cfg),
            scr_w: 1920,
            scr_h: 1080,
        }
    }

    fn tick(&mut self, dt: f64) -> bool {
        self.core.tick_motion(dt)
    }

    fn apply_command(&mut self, cmd: OverlayCommand) {
        // Linux uses the non-sentinel-snap behaviour for both MoveTo and
        // ClickPulse: every command updates `self.pos` unconditionally.
        // Custom-shape / gradient / focus-rect commands are not rendered on
        // Linux at present; `apply_command_base` consumes SetShape +
        // SetGradient and returns false for ShowFocusRect — both cases drop
        // the visual update silently so callers don't see an error.
        let _ = self.core.apply_command_base(cmd, false, false);
    }
}

// ── X11 thread ────────────────────────────────────────────────────────────

#[cfg(target_os = "linux")]
fn run_overlay_thread(cfg: CursorConfig, rx: std::sync::mpsc::Receiver<OverlayCommand>) {
    use x11rb::connection::Connection;
    use x11rb::protocol::xproto::*;
    use x11rb::protocol::xproto::ConnectionExt as _;
    use x11rb::protocol::shape::*;
    use x11rb::protocol::shape::ConnectionExt as _;
    use x11rb::wrapper::ConnectionExt as _;
    use x11rb::COPY_FROM_PARENT;

    // Connect to X11.
    let (conn, screen_num) = match x11rb::connect(None) {
        Ok(c) => c,
        Err(e) => {
            tracing::warn!("X11 overlay: cannot connect to display: {e}");
            return;
        }
    };

    let screen = &conn.setup().roots[screen_num];
    let root   = screen.root;
    let scr_w  = screen.width_in_pixels as u32;
    let scr_h  = screen.height_in_pixels as u32;

    // Update render state with screen size.
    {
        let mut guard = RENDER.lock().unwrap();
        if let Some(rs) = guard.as_mut() {
            rs.scr_w = scr_w;
            rs.scr_h = scr_h;
        }
    }

    // Find 32-bit ARGB visual for compositing.
    // Falls back to the default visual if XComposite 32-bit isn't available.
    let (visual_id, depth, colormap) = find_argb_visual(&conn, screen)
        .unwrap_or((screen.root_visual, screen.root_depth, screen.default_colormap));

    // Create a matching colormap if we got a non-default visual.
    let colormap = if visual_id != screen.root_visual {
        let cm = conn.generate_id().unwrap();
        conn.create_colormap(ColormapAlloc::NONE, cm, root, visual_id).ok();
        cm
    } else {
        colormap
    };

    // Create the overlay window.
    let win = conn.generate_id().unwrap();
    let win_aux = CreateWindowAux::new()
        .background_pixel(0)
        .border_pixel(0)
        .colormap(colormap)
        // Override-redirect = no window manager decoration, no focus.
        .override_redirect(1u32)
        // Input passthrough: do not receive button/key events.
        .event_mask(EventMask::NO_EVENT);

    conn.create_window(
        depth, win, root,
        0, 0, scr_w as u16, scr_h as u16,
        0,
        WindowClass::INPUT_OUTPUT,
        visual_id,
        &win_aux,
    ).ok();

    // Set window title (identifies our overlay, matches Windows convention).
    // `Cua.` namespace mirrors the Windows class-name + install-path
    // convention; was `TropeCUA.` (leaked codename from an early C# ref).
    let title = format!("Cua.AgentCursorOverlay.{}", cfg.cursor_id);
    conn.change_property8(
        PropMode::REPLACE, win,
        AtomEnum::WM_NAME,
        AtomEnum::STRING,
        title.as_bytes(),
    ).ok();

    // Make the window fully click-through by setting an EMPTY input region
    // (the X11 equivalent of WS_EX_TRANSPARENT on Windows). `shape_mask` with
    // a `None` source sets the input shape to the WHOLE window — the opposite
    // of click-through — so the overlay was silently input-opaque and ate real
    // pointer clicks on any XWayland window beneath it (it never surfaced
    // before the uinput tier because XSendEvent bypasses pointer routing, and
    // native-Wayland windows stack above this XWayland overlay in Hyprland).
    // `shape_rectangles` with zero rectangles is the correct empty-region idiom.
    conn.shape_rectangles(
        x11rb::protocol::shape::SO::SET,
        x11rb::protocol::shape::SK::INPUT,
        x11rb::protocol::xproto::ClipOrdering::UNSORTED,
        win,
        0, 0,
        &[],
    ).ok();

    conn.map_window(win).ok();
    conn.flush().ok();

    // Main render loop. Two cost controls keep an idle/hidden agent-cursor at
    // ~0% CPU instead of re-uploading the WHOLE screen at 60 Hz forever (the
    // cause of a multi-day single-core pin: a session that placed the cursor
    // then went idle left this thread allocating a full-screen pixmap, doing a
    // per-pixel BGRA byte-swap, and `XPutImage`-ing the entire display every
    // 16 ms with nothing on screen):
    //
    //  1. PAINT GATE — a cheap per-frame signature of everything that affects
    //     the rendered pixels (`paint_signature`). The full-screen render +
    //     byte-swap + upload only run when that signature changes or a command
    //     was just applied, so a static or faded-out cursor uploads nothing.
    //  2. QUIESCENT BACKOFF — while nothing is animating the thread BLOCKS on
    //     the command channel (bounded wait) rather than spinning at 60 Hz; it
    //     wakes only for a command, idle-fade bookkeeping, z-order upkeep, or a
    //     `stop()` request.
    let frame_dur = Duration::from_millis(16);
    // Bounded so (a) idle-fade onset is detected promptly, (b) z-order
    // reasserts stay timely, and (c) a STOP request is observed within this
    // window even when the thread is otherwise idle.
    let quiescent_wait = Duration::from_millis(50);
    let mut last_tick = Instant::now();
    let mut last_ztick = Instant::now();
    let mut last_sig: Option<u64> = None;
    let z_enforcer = X11ZOrderEnforcer { conn: &conn, win };

    loop {
        if STOP.load(std::sync::atomic::Ordering::Acquire) {
            break;
        }

        // Is the cursor visually evolving — a path/spring/click in flight, or an
        // idle-fade still in progress? This governs cadence: a tight 60 Hz
        // frame loop while animating, vs. a blocking wait at rest.
        let animating = {
            let guard = RENDER.lock().unwrap();
            guard.as_ref().is_some_and(|rs| {
                let c = &rs.core;
                let moving =
                    c.path.is_some() || c.spring.is_some() || c.click_t.is_some();
                // Fade evolves until `idle_hide_ms + 180ms` after last activity.
                let fade_active = c.visible
                    && c.motion.idle_hide_ms > 0.0
                    && c.idle_secs < c.motion.idle_hide_ms / 1000.0 + 0.18;
                moving || fade_active
            })
        };

        // First command for this iteration. While animating, never block — keep
        // the frame cadence. At rest, BLOCK (bounded) so the thread does not
        // spin: this is what drops idle CPU to ~0.
        let first_cmd = if animating {
            rx.try_recv().ok()
        } else {
            match rx.recv_timeout(quiescent_wait) {
                Ok(cmd) => Some(cmd),
                Err(std::sync::mpsc::RecvTimeoutError::Timeout) => None,
                // The sender is a process-lifetime static, so a disconnect is
                // unreachable in practice; treat it as a stop just in case.
                Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => break,
            }
        };

        let now = Instant::now();
        let dt = now.duration_since(last_tick).as_secs_f64().min(0.05);
        last_tick = now;

        // Apply the first command (if any), drain the rest, then tick. Track
        // whether ANY command was applied so discrete style/shape/palette/motion
        // changes force a repaint even if `paint_signature` doesn't capture them.
        let (fire_arrival, had_cmd) = {
            let mut guard = RENDER.lock().unwrap();
            if let Some(rs) = guard.as_mut() {
                let mut had_cmd = false;
                if let Some(cmd) = first_cmd {
                    rs.apply_command(cmd);
                    had_cmd = true;
                }
                while let Ok(cmd) = rx.try_recv() {
                    rs.apply_command(cmd);
                    had_cmd = true;
                }
                (rs.tick(dt), had_cmd)
            } else {
                (false, false)
            }
        };

        // PAINT GATE: render + upload only when the rendered pixels changed (or
        // a command was just applied). An unchanged frame skips the full-screen
        // alloc, byte-swap, and `XPutImage` entirely.
        let (sig, pixmap) = {
            let guard = RENDER.lock().unwrap();
            match guard.as_ref() {
                Some(rs) => {
                    let sig = paint_signature(&rs.core);
                    let pm = if had_cmd || last_sig != Some(sig) {
                        Some(cursor_overlay::render_frame(
                            &rs.core,
                            rs.scr_w.max(1),
                            rs.scr_h.max(1),
                            0.0, 0.0, // Linux uses screen-local coords (no origin offset)
                            None,     // focus-rect is macOS-only
                        ))
                    } else {
                        None
                    };
                    (Some(sig), pm)
                }
                None => (None, None),
            }
        };

        if let Some(pm) = pixmap {
            paint_x11(&conn, win, scr_w, scr_h, depth, visual_id, &pm);
            last_sig = sig;
        }

        if fire_arrival {
            if let Some(tx) = ARRIVAL_TX.lock().unwrap().take() {
                let _ = tx.send(());
            }
        }

        // Z-order maintenance every 80ms — delegate to the cross-platform
        // ZOrderEnforcer so the contract for "z+1 of the application under
        // test" is documented once in `cursor_overlay::z_order`.
        if last_ztick.elapsed() >= Duration::from_millis(80) {
            last_ztick = Instant::now();
            let pinned_wid = {
                let guard = RENDER.lock().unwrap();
                guard.as_ref().and_then(|rs| rs.core.pinned_wid)
            };
            z_enforcer.reassert(pinned_wid);
        }

        // Drain any X events (needed to avoid blocking).
        while let Ok(Some(_)) = conn.poll_for_event() {}

        // Frame pacing only matters while animating; the quiescent path already
        // slept inside `recv_timeout` above.
        if animating {
            let elapsed = Instant::now().duration_since(last_tick);
            if let Some(remaining) = frame_dur.checked_sub(elapsed) {
                std::thread::sleep(remaining);
            }
        }
    }

    // Teardown (reached via `stop()`): drop the override-redirect overlay
    // window so a stopped overlay leaves nothing behind on the X server.
    // Qualified to disambiguate the xproto vs. shape `ConnectionExt` globs.
    let _ = x11rb::protocol::xproto::ConnectionExt::destroy_window(&conn, win);
    let _ = conn.flush();
}

// ── Z-order enforcer (Linux impl of cursor_overlay::ZOrderEnforcer) ──────

/// X11 implementation of [`cursor_overlay::ZOrderEnforcer`].
///
/// Borrows the X11 connection and overlay window id; lives only inside
/// `run_overlay_thread` (the X11 connection is not `'static`). Called
/// every 80 ms from the render loop.
#[cfg(target_os = "linux")]
struct X11ZOrderEnforcer<'a, C: x11rb::connection::Connection> {
    conn: &'a C,
    win: u32,
}

#[cfg(target_os = "linux")]
impl<'a, C: x11rb::connection::Connection> ZOrderEnforcer for X11ZOrderEnforcer<'a, C> {
    fn reassert(&self, target: Option<u64>) {
        use x11rb::protocol::xproto::*;
        use x11rb::protocol::xproto::ConnectionExt as _;

        // Per the ZOrderEnforcer trait contract, a stale `target` (window
        // gone) should fall back to the `None` behavior — top of the
        // normal stack, no sibling. Using a stale XID as a `sibling` here
        // triggers BadWindow on every tick of the overlay-enforcer loop
        // (~125 Hz), spamming the X server and silently skipping the
        // intended z-reassertion. Probe liveness via get_window_attributes
        // before committing to the sibling path.
        let target_live = target.and_then(|xid| {
            self.conn
                .get_window_attributes(xid as u32)
                .ok()
                .and_then(|c| c.reply().ok())
                .map(|_| xid)
        });
        let aux = if let Some(target_xid) = target_live {
            // Place overlay just above the pinned X11 window.
            ConfigureWindowAux::new()
                .sibling(target_xid as u32)
                .stack_mode(StackMode::ABOVE)
        } else {
            // No pin (or stale target XID) → raise to the top of the
            // normal stack. (X11 has no OS-level "always-on-top" band like
            // Windows / NSStatusWindowLevel, so a plain ABOVE here cannot
            // accidentally float over a focused foreground app the way
            // HWND_TOPMOST would on Windows.)
            ConfigureWindowAux::new().stack_mode(StackMode::ABOVE)
        };
        self.conn.configure_window(self.win, &aux).ok();
        self.conn.flush().ok();
    }
}

#[cfg(target_os = "linux")]
fn find_argb_visual(
    conn: &impl x11rb::connection::Connection,
    screen: &x11rb::protocol::xproto::Screen,
) -> Option<(u32, u8, u32)> {
    use x11rb::protocol::xproto::VisualClass;
    // Walk all depth entries looking for a 32-bit ARGB visual.
    for depth_entry in &screen.allowed_depths {
        if depth_entry.depth != 32 { continue; }
        for visual in &depth_entry.visuals {
            if visual.class == VisualClass::TRUE_COLOR {
                return Some((visual.visual_id, 32, screen.default_colormap));
            }
        }
    }
    None
}

/// Blit a tiny-skia pixmap to the X11 window using XPutImage (ZPixmap).
/// The pixmap is premultiplied RGBA; X11 ARGB is premultiplied BGRA.
#[cfg(target_os = "linux")]
fn paint_x11(
    conn: &impl x11rb::connection::Connection,
    win: u32,
    w: u32, h: u32,
    depth: u8,
    _visual_id: u32,
    pm: &tiny_skia::Pixmap,
) {
    use x11rb::protocol::xproto::*;
    use x11rb::protocol::xproto::ConnectionExt as _;
    if pm.width() == 0 || pm.height() == 0 { return; }

    // Create a GC for the window if we don't have one.
    // (Simplified: we recreate it every frame which is safe but not optimal.)
    let gc_id = match conn.generate_id() {
        Ok(id) => id,
        Err(_) => return,
    };
    let gc_aux = CreateGCAux::new();
    if conn.create_gc(gc_id, win, &gc_aux).is_err() { return; }

    // Convert RGBA premult → BGRA premult for X11.
    let src = pm.data();
    let mut bgra: Vec<u8> = Vec::with_capacity(src.len());
    for chunk in src.chunks_exact(4) {
        bgra.push(chunk[2]); // B
        bgra.push(chunk[1]); // G
        bgra.push(chunk[0]); // R
        bgra.push(chunk[3]); // A
    }

    // XPutImage (ZPixmap).
    let _ = conn.put_image(
        ImageFormat::Z_PIXMAP,
        win,
        gc_id,
        w as u16, h as u16,
        0, 0,
        0,
        depth,
        &bgra,
    );

    conn.free_gc(gc_id).ok();
    conn.flush().ok();
}

#[cfg(not(target_os = "linux"))]
fn run_overlay_thread(_cfg: CursorConfig, _rx: std::sync::mpsc::Receiver<OverlayCommand>) {}

#[cfg(all(test, target_os = "linux"))]
mod tests {
    use super::paint_signature;
    use cursor_overlay::{CursorConfig, RenderStateCore};

    fn core() -> RenderStateCore {
        RenderStateCore::new(CursorConfig::default())
    }

    // The paint gate's whole point: an UNCHANGED frame must hash identically,
    // so the render loop can skip the full-screen alloc + BGRA byte-swap +
    // XPutImage. This is what keeps an idle/static cursor at ~0% CPU instead
    // of re-uploading the whole screen at 60 Hz forever (the multi-day
    // single-core pin this fixes).
    #[test]
    fn signature_is_stable_when_nothing_changes() {
        let c = core();
        assert_eq!(paint_signature(&c), paint_signature(&c));
    }

    // A genuine cursor move (≥¼px after quantisation) must change the signature
    // so the frame is actually repainted.
    #[test]
    fn signature_changes_on_visible_motion() {
        let mut c = core();
        let before = paint_signature(&c);
        c.pos = (c.pos.0 + 10.0, c.pos.1 + 10.0);
        assert_ne!(before, paint_signature(&c));
    }

    // Sub-quarter-pixel jitter must NOT force a repaint — the quantisation in
    // `paint_signature` is what prevents float noise from defeating the gate.
    #[test]
    fn signature_ignores_subpixel_jitter() {
        let mut c = core();
        let before = paint_signature(&c);
        c.pos = (c.pos.0 + 0.05, c.pos.1 + 0.05);
        assert_eq!(before, paint_signature(&c));
    }

    // The idle-fade path animates `idle_alpha` toward 0; each fade step must
    // change the signature so the fade actually renders, then stay put once
    // fully hidden (alpha pinned at 0) so a hidden cursor uploads nothing.
    #[test]
    fn signature_tracks_idle_fade_then_settles() {
        let mut c = core();
        let visible = paint_signature(&c);
        c.idle_alpha = 0.5;
        let mid = paint_signature(&c);
        assert_ne!(visible, mid);
        c.idle_alpha = 0.0;
        let hidden = paint_signature(&c);
        assert_ne!(mid, hidden);
        // Fully hidden is a fixed point: re-hashing the same hidden state is
        // identical, so the loop stops repainting.
        assert_eq!(hidden, paint_signature(&c));
    }

    // Toggling visibility (SetEnabled) must change the signature so show/hide
    // is honoured by the gate.
    #[test]
    fn signature_changes_on_visibility_toggle() {
        let mut c = core();
        let shown = paint_signature(&c);
        c.visible = false;
        assert_ne!(shown, paint_signature(&c));
    }
}
