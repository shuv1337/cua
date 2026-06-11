//! Real (kernel-level uinput) input tier for Linux — the opt-in
//! `dispatch:"real"` path (shuv1337/cua#18).
//!
//! Every no-foreground synthetic-input path the backend has dead-ends on
//! some Wayland targets: XSendEvent button/key events are dropped by
//! wx/GTK3 (`send_event=true` filtering), and XTest routes to the X focus
//! which a native-Wayland compositor ignores. Kernel-level uinput is the
//! one mechanism Hyprland routes as genuine hardware input.
//!
//! This tier DELIBERATELY breaks the no-foreground contract: real input
//! only routes when the compositor agrees on focus, so the target must be
//! on a visible workspace and the real pointer physically moves. The tool
//! layer reveals the window, saves the pointer, drives this tier, then
//! restores the pointer and re-hides — see the `dispatch:"real"` arm in
//! `tools::impl_`.
//!
//! ## Why a closed loop instead of a fixed coordinate map
//!
//! The device→logical coordinate mapping is host-specific and not 1:1:
//! libinput applies a per-axis scale (fractional-scaled multi-monitor) and
//! the abs device range clamps past the layout edges. Rather than hardcode
//! a scale, every move is closed-loop: command an absolute device
//! position, read the TRUE logical cursor back from the compositor
//! (`hyprctl cursorpos`), estimate the per-axis gain from the response, and
//! Newton-correct until within tolerance — or refuse to click. This
//! self-calibrates to any host and is the safety contract: we never click
//! unless the pointer is verified on target.

use anyhow::{bail, Context, Result};
use std::path::PathBuf;
use std::process::Command;
use std::time::Duration;

/// Logical-pixel tolerance for the closed-loop placement. The compositor
/// reports integer logical coords; ±2 px absorbs the readback quantization
/// without ever landing on a neighbouring control.
const PLACEMENT_TOLERANCE_PX: f64 = 2.0;

/// Max closed-loop correction iterations before giving up (no click). Gain
/// estimation converges in 2–3; this is the runaway guard.
const MAX_PLACEMENT_ITERS: usize = 8;

/// Settle delay after a uinput move before reading the cursor back — the
/// compositor processes the event asynchronously.
const MOVE_SETTLE: Duration = Duration::from_millis(60);

/// Initial guess for logical-px per device-unit, before the loop measures
/// the real per-axis gain. Only needs the right order of magnitude; the
/// two-sample estimate takes over immediately.
const INITIAL_GAIN: f64 = 2.5;

/// Which kernel-input mechanism the real tier drives. Selection prefers
/// ydotoold (no special perms beyond the socket; it owns the virtual
/// device) and falls back to a direct `/dev/uinput` device (#18 decision:
/// probe both, prefer ydotoold).
#[derive(Debug, Clone)]
pub enum Backend {
    /// ydotool CLI talking to a running `ydotoold` over its socket.
    Ydotool { socket: PathBuf },
    /// A `cua-driver`-owned virtual device written straight to
    /// `/dev/uinput` (see [`super::uinput_device`]).
    DevUinput,
}

impl Backend {
    pub fn name(&self) -> &'static str {
        match self {
            Backend::Ydotool { .. } => "ydotool",
            Backend::DevUinput => "uinput",
        }
    }
}

/// What the real tier can reach on this host — surfaced by `cua-driver
/// doctor` so an operator can see why `dispatch:"real"` is or isn't
/// available before they hit the gated error.
#[derive(Debug, Clone)]
pub struct Capability {
    /// Discovered ydotoold socket, if one is reachable.
    pub ydotool_socket: Option<PathBuf>,
    /// `/dev/uinput` exists and is writable by this process.
    pub dev_uinput_writable: bool,
    /// This process is in the `input` group (the usual grant for
    /// `/dev/uinput` outside of a udev rule).
    pub in_input_group: bool,
}

impl Capability {
    /// The backend the selector would pick, or `None` when no working path
    /// is available.
    ///
    /// ydotoold is preferred and is the only IMPLEMENTED backend today; the
    /// direct `/dev/uinput` write path is probed and reported (so doctor can
    /// show it) but not yet selectable — it is the #18 follow-up. A host
    /// with only `/dev/uinput` therefore resolves to `None` and
    /// [`RealInput::detect`] tells the operator to start ydotoold.
    pub fn preferred_backend(&self) -> Option<Backend> {
        self.ydotool_socket
            .as_ref()
            .map(|socket| Backend::Ydotool { socket: socket.clone() })
    }

    /// One-line human summary for doctor / gated-error text.
    pub fn summary(&self) -> String {
        match (&self.ydotool_socket, self.dev_uinput_writable) {
            (Some(s), _) => format!("ydotoold socket {} (preferred)", s.display()),
            (None, true) => {
                "/dev/uinput writable but ydotoold not running — start ydotoold; \
                 direct /dev/uinput injection is a tracked follow-up"
                    .to_string()
            }
            (None, false) => {
                let grp = if self.in_input_group { "in 'input' group" } else { "NOT in 'input' group" };
                format!("unavailable — no ydotoold socket and /dev/uinput not writable ({grp})")
            }
        }
    }
}

/// Probe both backends without mutating anything. Cheap (a stat + a group
/// lookup); safe to call from doctor and from the gated error path.
pub fn capability() -> Capability {
    Capability {
        ydotool_socket: find_ydotool_socket(),
        dev_uinput_writable: dev_uinput_writable(),
        in_input_group: in_input_group(),
    }
}

/// Locate a ydotoold socket: explicit `$YDOTOOL_SOCKET`, then the
/// per-user default, then the legacy `/tmp` default. Returns the first
/// that exists as a socket.
fn find_ydotool_socket() -> Option<PathBuf> {
    let mut candidates: Vec<PathBuf> = Vec::new();
    if let Some(env) = std::env::var_os("YDOTOOL_SOCKET") {
        candidates.push(PathBuf::from(env));
    }
    if let Some(run) = std::env::var_os("XDG_RUNTIME_DIR") {
        candidates.push(PathBuf::from(run).join(".ydotool_socket"));
    }
    if let Ok(uid) = std::env::var("UID") {
        candidates.push(PathBuf::from(format!("/run/user/{uid}/.ydotool_socket")));
    }
    candidates.push(PathBuf::from("/tmp/.ydotool_socket"));
    candidates.into_iter().find(|p| is_socket(p))
}

fn is_socket(path: &std::path::Path) -> bool {
    use std::os::unix::fs::FileTypeExt;
    std::fs::metadata(path)
        .map(|m| m.file_type().is_socket())
        .unwrap_or(false)
}

fn dev_uinput_writable() -> bool {
    // W_OK without opening — opening can have side effects on a device node.
    let path = std::ffi::CString::new("/dev/uinput").unwrap();
    unsafe { libc::access(path.as_ptr(), libc::W_OK) == 0 }
}

fn in_input_group() -> bool {
    group_names().iter().any(|g| g == "input")
}

/// Supplementary group names of this process. Best-effort; empty on error.
fn group_names() -> Vec<String> {
    unsafe {
        let n = libc::getgroups(0, std::ptr::null_mut());
        if n <= 0 {
            return Vec::new();
        }
        let mut gids = vec![0 as libc::gid_t; n as usize];
        if libc::getgroups(n, gids.as_mut_ptr()) < 0 {
            return Vec::new();
        }
        gids.into_iter().filter_map(gid_to_name).collect()
    }
}

fn gid_to_name(gid: libc::gid_t) -> Option<String> {
    unsafe {
        let grp = libc::getgrgid(gid);
        if grp.is_null() {
            return None;
        }
        let name = (*grp).gr_name;
        if name.is_null() {
            return None;
        }
        std::ffi::CStr::from_ptr(name).to_str().ok().map(str::to_owned)
    }
}

/// A ready-to-drive real-input session over a selected backend. Construct
/// with [`RealInput::detect`]; the tool layer handles reveal/restore around
/// it.
pub struct RealInput {
    backend: Backend,
}

/// Where a closed-loop placement actually landed, for honest result text.
#[derive(Debug, Clone, Copy)]
pub struct Placement {
    pub logical_x: f64,
    pub logical_y: f64,
    pub iterations: usize,
}

impl RealInput {
    /// Select a backend (ydotoold preferred), or fail with actionable text
    /// naming what doctor would show.
    pub fn detect() -> Result<Self> {
        let cap = capability();
        match cap.preferred_backend() {
            Some(backend) => Ok(Self { backend }),
            None => bail!(
                "real-input tier unavailable: {}. Start ydotoold (e.g. `systemctl --user \
                 start ydotool` or run `ydotoold`), or make /dev/uinput writable (add your \
                 user to the 'input' group and re-login). See `cua-driver doctor`.",
                cap.summary()
            ),
        }
    }

    pub fn backend_name(&self) -> &'static str {
        self.backend.name()
    }

    /// Move the real pointer to a logical screen point with closed-loop
    /// verification, then click `button` (1=left,2=middle,3=right) `count`
    /// times. Returns where the pointer was verified before clicking.
    /// Errors WITHOUT clicking if placement never converges (the safety
    /// contract: never click off-target).
    pub fn click_at_logical(
        &self,
        target: (f64, f64),
        button: u8,
        count: usize,
    ) -> Result<Placement> {
        let placement = self.move_to_logical(target)?;
        for i in 0..count.max(1) {
            self.backend_click(button)?;
            if i + 1 < count {
                std::thread::sleep(Duration::from_millis(80));
            }
        }
        Ok(placement)
    }

    /// Closed-loop move the real pointer to `target` logical coords. Two
    /// samples estimate the per-axis device→logical gain; subsequent steps
    /// Newton-correct the residual. Errors if it can't get within
    /// [`PLACEMENT_TOLERANCE_PX`].
    pub fn move_to_logical(&self, target: (f64, f64)) -> Result<Placement> {
        let (tx, ty) = target;
        // Device-space command estimate; refined as gain is measured.
        let mut gain_x = INITIAL_GAIN;
        let mut gain_y = INITIAL_GAIN;
        let mut dev_x = tx / gain_x;
        let mut dev_y = ty / gain_y;
        let mut prev: Option<((f64, f64), (f64, f64))> = None; // (device, logical)

        for iter in 0..MAX_PLACEMENT_ITERS {
            self.backend_move_abs(dev_x, dev_y)?;
            std::thread::sleep(MOVE_SETTLE);
            let (cx, cy) = cursorpos()?;
            let (ex, ey) = (tx - cx, ty - cy);
            if ex.abs() <= PLACEMENT_TOLERANCE_PX && ey.abs() <= PLACEMENT_TOLERANCE_PX {
                return Ok(Placement { logical_x: cx, logical_y: cy, iterations: iter + 1 });
            }
            // Re-estimate gain from the last two (device, logical) samples
            // when they moved enough to be meaningful; else keep the prior.
            if let Some(((pdx, pdy), (plx, ply))) = prev {
                let (ddx, ddy) = (dev_x - pdx, dev_y - pdy);
                let (dlx, dly) = (cx - plx, cy - ply);
                if ddx.abs() > 0.5 && dlx.abs() > 0.5 {
                    gain_x = dlx / ddx;
                }
                if ddy.abs() > 0.5 && dly.abs() > 0.5 {
                    gain_y = dly / ddy;
                }
            }
            prev = Some(((dev_x, dev_y), (cx, cy)));
            // Guard against a degenerate (clamped) axis producing a zero or
            // sign-flipped gain that would diverge.
            let gx = if gain_x.abs() < 0.1 { INITIAL_GAIN } else { gain_x };
            let gy = if gain_y.abs() < 0.1 { INITIAL_GAIN } else { gain_y };
            dev_x += ex / gx;
            dev_y += ey / gy;
        }
        let (cx, cy) = cursorpos().unwrap_or((f64::NAN, f64::NAN));
        bail!(
            "real-input placement did not converge on ({tx:.0},{ty:.0}) within \
             {MAX_PLACEMENT_ITERS} iterations (last cursor {cx:.0},{cy:.0}); refusing to \
             click off-target. The target may be off-screen or on a clamped axis edge."
        )
    }

    fn backend_move_abs(&self, x: f64, y: f64) -> Result<()> {
        match &self.backend {
            Backend::Ydotool { socket } => ydotool(socket, &[
                "mousemove",
                "-a",
                "-x",
                &(x.round() as i64).to_string(),
                "-y",
                &(y.round() as i64).to_string(),
            ]),
            Backend::DevUinput => super::uinput_device::move_abs(x.round() as i32, y.round() as i32),
        }
    }

    fn backend_click(&self, button: u8) -> Result<()> {
        match &self.backend {
            Backend::Ydotool { socket } => {
                // ydotool encodes a press+release as one byte: high nibble
                // 0x4/0x8/0xC selects L/R/M, low nibble 0x0/0x1/0x2 the same.
                // 0xC0 = left down+up, 0xC1 = right, 0xC2 = middle.
                let code = match button {
                    3 => "0xC1",
                    2 => "0xC2",
                    _ => "0xC0",
                };
                ydotool(socket, &["click", code])
            }
            Backend::DevUinput => super::uinput_device::click(button),
        }
    }

    /// Type literal text via the real tier (focus must already be on the
    /// target — the tool layer reveals it first).
    pub fn type_text(&self, text: &str) -> Result<()> {
        match &self.backend {
            Backend::Ydotool { socket } => ydotool(socket, &["type", "--", text]),
            Backend::DevUinput => super::uinput_device::type_text(text),
        }
    }

    /// Press a named key (with optional modifiers) via the real tier.
    pub fn key(&self, key: &str, modifiers: &[&str]) -> Result<()> {
        let main = super::evdev::keycode(key)
            .with_context(|| format!("no evdev keycode for key '{key}'"))?;
        let mods: Vec<u16> = modifiers
            .iter()
            .filter_map(|m| super::evdev::modifier_keycode(m))
            .collect();
        match &self.backend {
            Backend::Ydotool { socket } => {
                // ydotool key takes `<code>:1` press / `<code>:0` release.
                let mut seq: Vec<String> = Vec::new();
                for m in &mods {
                    seq.push(format!("{m}:1"));
                }
                seq.push(format!("{main}:1"));
                seq.push(format!("{main}:0"));
                for m in mods.iter().rev() {
                    seq.push(format!("{m}:0"));
                }
                let args: Vec<&str> = std::iter::once("key").chain(seq.iter().map(String::as_str)).collect();
                ydotool(socket, &args)
            }
            Backend::DevUinput => super::uinput_device::key(main, &mods),
        }
    }
}

/// Read the true logical cursor position from the compositor — the
/// closed-loop's ground truth. Hyprland only for now (`hyprctl cursorpos`
/// → "x, y"); structured so other compositors can be added.
pub fn cursorpos() -> Result<(f64, f64)> {
    let out = Command::new("hyprctl")
        .arg("cursorpos")
        .output()
        .context("spawning hyprctl cursorpos")?;
    if !out.status.success() {
        bail!("hyprctl cursorpos failed");
    }
    let s = String::from_utf8_lossy(&out.stdout);
    let (x, y) = s.trim().split_once(',').context("unexpected cursorpos output")?;
    Ok((x.trim().parse()?, y.trim().parse()?))
}

fn ydotool(socket: &std::path::Path, args: &[&str]) -> Result<()> {
    let out = Command::new("ydotool")
        .args(args)
        .env("YDOTOOL_SOCKET", socket)
        .output()
        .context("spawning ydotool")?;
    if !out.status.success() {
        bail!(
            "ydotool {:?} failed: {}",
            args.first().copied().unwrap_or(""),
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    Ok(())
}
