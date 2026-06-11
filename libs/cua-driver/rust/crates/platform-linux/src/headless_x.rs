//! Headless-X session backend (shuv1337/cua#18).
//!
//! True off-screen / background "real input" for stubborn X11/wxWidgets apps
//! (PrusaSlicer and its modal dialogs) that defeat both AT-SPI (a wx modal
//! collapses the app's accessibility tree) and XSendEvent (wx/GTK3 drop
//! `send_event=true` synthetic events). The trick — proven live 2026-06-11 —
//! is to run the app in a PRIVATE headless `Xvfb` X server and drive it with
//! XTest (`XTestFakeInput`), which carries `send_event=false` and is the exact
//! input a wx modal accepts. Inside a private Xvfb the modal IS that server's
//! focused window, so the usual objection to XTest ("it hits the focused
//! window on the shared :0", see `input/mod.rs`) evaporates.
//!
//! Crucially this never touches the user's compositor: Xvfb is a standalone X
//! server, not a Wayland/Hyprland output, so it structurally cannot trigger
//! the Aquamarine headless-output SIGSEGV that crashes Hyprland 0.55.3 — and
//! it renders to an off-screen framebuffer the user never sees.
//!
//! When [`activate`] runs (daemon started with `--headless-x`), the process
//! `DISPLAY` is pointed at the managed Xvfb and `HYPRLAND_INSTANCE_SIGNATURE`
//! is cleared, so the rest of the Linux backend (window enumeration in
//! `x11`, capture in `capture`, etc. — all of which `connect(None)` against
//! `$DISPLAY`) transparently targets the headless server via its existing
//! X11 (non-Hyprland) code paths. Only input is special-cased: `input`
//! routes to XTest while [`is_active`] is set.

use anyhow::{bail, Context, Result};
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;
use std::time::{Duration, Instant};

static ACTIVE: AtomicBool = AtomicBool::new(false);
static SESSION: Mutex<Option<HeadlessXSession>> = Mutex::new(None);

/// Whether the daemon is running in headless-X mode (input should use XTest
/// against the managed Xvfb instead of XSendEvent against the user's :0).
pub fn is_active() -> bool {
    ACTIVE.load(Ordering::Relaxed)
}

/// A managed headless X session: an `Xvfb` server plus a minimal window
/// manager. Dropping it tears both down.
pub struct HeadlessXSession {
    pub display: String,
    xvfb: Child,
    wm: Option<Child>,
}

impl HeadlessXSession {
    /// Start `Xvfb` on a free display with `width`x`height`, then a minimal
    /// WM (openbox if available) so wx modals get focus + stacking. The WM is
    /// best-effort — Xvfb alone still serves input/capture.
    pub fn start(width: u32, height: u32) -> Result<Self> {
        let display = pick_free_display()?;
        let mut xvfb_cmd = Command::new("Xvfb");
        xvfb_cmd
            .args([
                &display,
                "-screen",
                "0",
                &format!("{width}x{height}x24"),
                "-ac",
                "+extension",
                "GLX",
                "+render",
                "-noreset",
            ])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        die_with_parent(&mut xvfb_cmd);
        let xvfb = xvfb_cmd
            .spawn()
            .context("spawning Xvfb — is it installed? (pacman -S xorg-server-xvfb)")?;

        let mut session = Self { display: display.clone(), xvfb, wm: None };

        if let Err(e) = wait_for_display(&display, Duration::from_secs(10)) {
            let _ = session.xvfb.kill();
            return Err(e);
        }

        // Minimal WM so toplevels (esp. wx modals) get focus and stacking.
        let mut wm_cmd = Command::new("openbox");
        wm_cmd
            .env("DISPLAY", &display)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        die_with_parent(&mut wm_cmd);
        session.wm = wm_cmd.spawn().ok();

        Ok(session)
    }
}

/// Ask the kernel to SIGTERM this child when the daemon (its parent) dies.
/// `Drop` never runs on process exit / SIGKILL / crash, so this is what
/// actually guarantees the Xvfb + WM don't leak when `cua-driver serve`
/// stops by any means. `PR_SET_PDEATHSIG` is relative to the spawning
/// THREAD — `activate()` runs on the main thread, which lives for the
/// daemon's whole lifetime, so the signal fires exactly at daemon exit.
fn die_with_parent(cmd: &mut Command) {
    use std::os::unix::process::CommandExt;
    unsafe {
        cmd.pre_exec(|| {
            libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGTERM as libc::c_ulong);
            Ok(())
        });
    }
}

impl Drop for HeadlessXSession {
    fn drop(&mut self) {
        if let Some(wm) = self.wm.as_mut() {
            let _ = wm.kill();
            let _ = wm.wait();
        }
        let _ = self.xvfb.kill();
        let _ = self.xvfb.wait();
        // Xvfb usually removes its own socket on a clean exit; best-effort.
        let n = self.display.trim_start_matches(':');
        let _ = std::fs::remove_file(format!("/tmp/.X11-unix/X{n}"));
        let _ = std::fs::remove_file(format!("/tmp/.X{n}-lock"));
    }
}

/// Start a headless X session and route the whole Linux backend at it:
/// point `DISPLAY` at the managed Xvfb, clear the Wayland/Hyprland env so the
/// X11 (non-Hyprland) code paths are taken, and flip [`is_active`] so input
/// uses XTest. Returns the chosen display (`:N`). Idempotent: a second call
/// returns the existing display.
pub fn activate(width: u32, height: u32) -> Result<String> {
    let mut guard = SESSION.lock().unwrap();
    if let Some(s) = guard.as_ref() {
        return Ok(s.display.clone());
    }
    let session = HeadlessXSession::start(width, height)?;
    let dpy = session.display.clone();

    // Route every `connect(None)` (enumeration, capture, input) at the Xvfb,
    // and drop the Wayland/Hyprland markers so `is_hyprland_session()` is
    // false and launch_app direct-spawns instead of dispatching to Hyprland.
    // Safe here: called once at serve startup before the MCP threads spawn.
    std::env::set_var("DISPLAY", &dpy);
    std::env::remove_var("HYPRLAND_INSTANCE_SIGNATURE");
    std::env::remove_var("WAYLAND_DISPLAY");
    // Xvfb has no GPU, so apps must use the Mesa software rasterizer or they
    // abort on the GL probe (PrusaSlicer dies instantly otherwise). Force
    // GTK onto X11 too. Spawned children inherit these; the daemon itself
    // does no GL, so they're harmless to it.
    std::env::set_var("LIBGL_ALWAYS_SOFTWARE", "1");
    std::env::set_var("GALLIUM_DRIVER", "llvmpipe");
    std::env::set_var("GDK_BACKEND", "x11");

    *guard = Some(session);
    ACTIVE.store(true, Ordering::Relaxed);
    tracing::info!("headless-X session active on {}", dpy);
    Ok(dpy)
}

/// Tear down the managed session (kills Xvfb + WM). Best-effort.
pub fn shutdown() {
    ACTIVE.store(false, Ordering::Relaxed);
    *SESSION.lock().unwrap() = None;
}

/// First display `:N` (70..=199) whose X socket and lock are both absent.
fn pick_free_display() -> Result<String> {
    for n in 70..200u32 {
        if !Path::new(&format!("/tmp/.X11-unix/X{n}")).exists()
            && !Path::new(&format!("/tmp/.X{n}-lock")).exists()
        {
            return Ok(format!(":{n}"));
        }
    }
    bail!("no free X display in :70..:199")
}

/// Poll until an X server answers on `display`, or `timeout` elapses.
fn wait_for_display(display: &str, timeout: Duration) -> Result<()> {
    let deadline = Instant::now() + timeout;
    loop {
        if x11rb::rust_connection::RustConnection::connect(Some(display)).is_ok() {
            return Ok(());
        }
        if Instant::now() >= deadline {
            bail!("Xvfb did not become ready on {display} within {timeout:?}");
        }
        std::thread::sleep(Duration::from_millis(100));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pick_free_display_returns_colon_form() {
        let d = pick_free_display().unwrap();
        assert!(d.starts_with(':'));
        assert!(d[1..].parse::<u32>().is_ok());
    }
}
