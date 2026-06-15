//! Advertise a screen reader on the session a11y status bus.
//!
//! Chromium/Electron (AuraLinux) and GTK/Qt only build their *full* AT-SPI
//! tree once something on the bus claims to be a screen reader. While no AT is
//! listening they expose a bare frame node and lazily skip the rest — and a
//! backgrounded Electron window on a hidden workspace never gets prodded into
//! building it. Setting `ScreenReaderEnabled` + `IsEnabled` on
//! `org.a11y.Status` is exactly what Orca does on startup; it flips that switch
//! session-wide and *retroactively* for already-running apps, with no per-app
//! env or relaunch. This is the linchpin for driving hidden Electron windows.
//!
//! The properties live on the **session** bus object `org.a11y.Bus`
//! `/org/a11y/bus` (`org.a11y.Status`) — NOT the separate a11y bus the tree
//! walk talks to (that one is reached via `org.a11y.Bus.GetAddress`). zbus is
//! re-exported by the `atspi` crate, so we need no extra dependency.
//!
//! Best-effort by contract: on a session with no a11y stack `org.a11y.Bus` may
//! be unregistered and the `Set` can hang, so every call is wrapped in a short
//! timeout and any failure is logged and swallowed — daemon startup must never
//! block or panic on this.

use std::time::Duration;

use anyhow::Result;

/// How long any single Set is allowed to take before we give up. A missing
/// `org.a11y.Bus` name must not stall daemon startup.
const STATUS_TIMEOUT: Duration = Duration::from_secs(4);

/// Set `ScreenReaderEnabled=true` and `IsEnabled=true` on the session a11y
/// status bus so Chromium/Electron/GTK/Qt build their full AT-SPI tree.
///
/// MUST be called from a plain OS thread (not a tokio worker): it drives a
/// private current-thread runtime via `block_on`, and the `native.rs` safety
/// contract forbids `block_on` on the shared async runtime's workers. The
/// `serve` arm calls this on the main thread before the serve worker spawns.
pub fn enable_screen_reader() -> Result<()> {
    set_status(true)
}

/// Best-effort clear of the status flags. Intentionally NOT wired into the
/// serve shutdown path: the flags are coarse, session-wide booleans with no
/// per-client refcount, so clearing them on exit would disable accessibility
/// for any *other* AT (a running Orca) that relies on them. Leaving them true
/// is the safer default — Chromium just keeps its tree built, which is
/// harmless. Provided for callers that knowingly own the session.
pub fn disable_screen_reader() {
    if let Err(e) = set_status(false) {
        tracing::warn!("could not clear screen-reader status on a11y bus: {e:#}");
    }
}

/// Drive the session-bus property writes on a one-shot current-thread runtime.
/// A fresh runtime (rather than the shared multi-thread `native::runtime()`)
/// keeps this off any async worker and avoids cross-runtime entanglement.
fn set_status(on: bool) -> Result<()> {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;
    rt.block_on(async move {
        // Guard the whole exchange so a missing/unresponsive org.a11y.Bus
        // name never blocks daemon startup.
        tokio::time::timeout(STATUS_TIMEOUT, set_status_async(on))
            .await
            .map_err(|_| anyhow::anyhow!("timed out talking to org.a11y.Status"))?
    })
}

/// The actual D-Bus work: connect to the session bus and Set both booleans via
/// the generic `Proxy` (which calls `org.freedesktop.DBus.Properties.Set`).
async fn set_status_async(on: bool) -> Result<()> {
    let conn = atspi::zbus::Connection::session().await?;
    let proxy =
        atspi::zbus::Proxy::new(&conn, "org.a11y.Bus", "/org/a11y/bus", "org.a11y.Status").await?;
    // Both properties are documented writable booleans; pass `bool` directly
    // (it implements Into<zvariant::Value>). set_property returns a
    // zbus::fdo::Error, so map it into anyhow.
    proxy
        .set_property("ScreenReaderEnabled", on)
        .await
        .map_err(|e| anyhow::anyhow!("set ScreenReaderEnabled={on}: {e}"))?;
    proxy
        .set_property("IsEnabled", on)
        .await
        .map_err(|e| anyhow::anyhow!("set IsEnabled={on}: {e}"))?;
    Ok(())
}
