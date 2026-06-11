//! Direct `/dev/uinput` virtual-device backend for the real-input tier —
//! tracked follow-up to shuv1337/cua#18.
//!
//! The shipped real-input tier drives ydotoold (see [`super::real`]), which
//! owns the virtual device. This module is the no-ydotoold fallback: open
//! `/dev/uinput`, declare an absolute pointer + key device, and emit events
//! directly. It is NOT yet implemented — the capability probe reports
//! `/dev/uinput` writability so `cua-driver doctor` can surface it, but
//! [`super::real::Capability::preferred_backend`] does not select this path,
//! so these functions are unreachable in normal operation. They exist so
//! the backend dispatch in [`super::real`] is total and the follow-up has a
//! clear home.
//!
//! Implementing it means: `UI_SET_EVBIT`/`UI_SET_KEYBIT`/`UI_SET_ABSBIT`
//! ioctls, a `UI_DEV_SETUP` + `UI_DEV_CREATE` lifecycle (created once,
//! kept alive for the daemon), an `ABS_X`/`ABS_Y` range the closed loop in
//! [`super::real`] already self-calibrates against, and `BTN_LEFT/RIGHT/
//! MIDDLE` + the [`super::evdev`] key table for keystrokes.

use anyhow::{bail, Result};

const NOT_IMPLEMENTED: &str = "direct /dev/uinput injection is not yet implemented; \
     start ydotoold to use the real-input tier (shuv1337/cua#18 follow-up)";

pub fn move_abs(_x: i32, _y: i32) -> Result<()> {
    bail!(NOT_IMPLEMENTED)
}

pub fn click(_button: u8) -> Result<()> {
    bail!(NOT_IMPLEMENTED)
}

pub fn type_text(_text: &str) -> Result<()> {
    bail!(NOT_IMPLEMENTED)
}

pub fn key(_main: u16, _modifiers: &[u16]) -> Result<()> {
    bail!(NOT_IMPLEMENTED)
}
