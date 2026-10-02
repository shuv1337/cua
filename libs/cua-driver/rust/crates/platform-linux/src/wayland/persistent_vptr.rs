//! Persistent virtual-pointer for stateful `mouse_button_down` / `mouse_drag` /
//! `mouse_button_up`.
//!
//! The non-persistent path in [`crate::wayland::click`] opens its own
//! `ZwlrVirtualPointerV1`, presses, releases, drops the connection — useful
//! for one-shot clicks but useless for held-button drags: each tool call
//! emits a fresh device whose press/release pair is matched by the
//! compositor, so apps that distinguish a real drag (press, motion+, release)
//! from a series of clicks (press, release, press, release, …) miss the
//! drag entirely.
//!
//! This module keeps the virtual-pointer alive across tool calls. A single
//! owner thread per process owns one Wayland `Connection`, one `EventQueue`,
//! and a map of `cursor_id -> ActivePointer`. Commands are sent over a
//! `crossbeam-channel`; replies come back on a per-call reply channel so the
//! caller blocks until the compositor has roundtripped.
//!
//! Lifecycle:
//! - First `press` for a cursor_id binds a fresh `ZwlrVirtualPointerV1`,
//!   activates the foreign-toplevel target window once, presses the button,
//!   adds to the held-button set, roundtrips.
//! - Subsequent `move_to` calls emit `motion_absolute` on the same vptr (no
//!   activate — would steal focus mid-drag) and roundtrip.
//! - `release` emits a button release, removes from the held set; if the
//!   set is empty the vptr is destroyed and the map entry dropped.
//! - Failed release acknowledgment quarantines the owner: it never reconnects
//!   to invent cleanup on another device. All cleanup releases tracked owned
//!   buttons before destroying the original proxy.

use std::collections::{HashMap, HashSet};
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc, OnceLock,
};
use std::thread;

use crossbeam_channel::{bounded, Receiver, Sender};
use wayland_client::{protocol::wl_pointer::ButtonState, Connection};
use wayland_protocols_wlr::virtual_pointer::v1::client::zwlr_virtual_pointer_v1::ZwlrVirtualPointerV1;

use super::evdev_pointer_button;

/// One in-flight command from the public API to the owner thread.
enum Cmd {
    Press {
        cursor_id: String,
        window_id: u64,
        x: i32,
        y: i32,
        button: u8,
        reply: Sender<anyhow::Result<()>>,
    },
    MoveTo {
        cursor_id: String,
        x: i32,
        y: i32,
        reply: Sender<anyhow::Result<()>>,
    },
    Release {
        cursor_id: String,
        button: u8,
        reply: Sender<anyhow::Result<()>>,
    },
    /// Release tracked buttons on the original device, acknowledge, then drop.
    Forget {
        cursor_id: String,
        reply: Sender<anyhow::Result<()>>,
    },
}

/// State held inside the owner thread for one cursor_id.
struct ActivePointer {
    pending_replies: Vec<Arc<AtomicBool>>,
    vptr: ZwlrVirtualPointerV1,
    /// evdev codes of buttons currently held down. When this set becomes
    /// empty the vptr is destroyed and the entry dropped from the map.
    held: HashSet<u32>,
    /// Pointer-space extent and layout origin at session open time — needed
    /// for motion_absolute.
    out_w: u32,
    out_h: u32,
    origin_x: i32,
    origin_y: i32,
    frame: Option<super::hyprland::DesktopFrame>,
    target: Option<(super::hyprland::Window, super::hyprland::LocalWindowFrame)>,
}

/// Process-global command channel into the owner thread. Lazily started on
/// first use.
type Request = (Cmd, Arc<AtomicBool>);
static TX: OnceLock<Sender<Request>> = OnceLock::new();

fn tx() -> &'static Sender<Request> {
    TX.get_or_init(|| {
        let (tx, rx) = bounded::<Request>(32);
        thread::Builder::new()
            .name("cua-persistent-vptr".into())
            .spawn(move || owner_thread(rx))
            .expect("spawn cua-persistent-vptr thread");
        tx
    })
}

fn owner_thread(rx: Receiver<Request>) {
    let mut active: HashMap<String, ActivePointer> = HashMap::new();
    let mut cleanup_failures: HashMap<String, String> = HashMap::new();
    let mut revoked_releases: HashMap<String, HashSet<u32>> = HashMap::new();
    loop {
        let command = rx.recv_timeout(std::time::Duration::from_millis(100));
        // Keep cancellation observable even if send succeeded just before the
        // caller's bounded receive timed out. Completed callers drop their Arc;
        // only unresolved replies remain; motion refuses at the explicit cap.
        let cancelled: Vec<_> = active
            .iter_mut()
            .filter_map(|(key, pointer)| {
                let revoked = revoke_cancelled_replies(&mut pointer.pending_replies);
                revoked.then(|| key.clone())
            })
            .collect();
        for key in cancelled {
            let cleanup = destroy_owned(&mut active, &key);
            retain_cleanup_failure(&mut cleanup_failures, &key, &cleanup);
        }
        // Poll even while the caller is idle. Metadata failures revoke the
        // originally owned device; never open a new connection to release into
        // a successor compositor. Polling does not prove unseen ABA transitions.
        if active
            .values()
            .any(|p| p.frame.is_some() || p.target.is_some())
        {
            let observed = super::hyprland::with_observation_deadline(
                std::time::Instant::now() + std::time::Duration::from_secs(2),
                || {
                    let current = if active.values().any(|p| p.frame.is_some()) {
                        Some(super::hyprland::desktop_frame()?)
                    } else {
                        None
                    };
                    let revoked: Vec<_> = active
                        .iter()
                        .filter(|(_, p)| {
                            p.frame.as_ref().is_some_and(|frame| {
                                current
                                    .as_ref()
                                    .is_none_or(|now| now.identity != frame.identity)
                            }) || validate_owned_target(p).is_err()
                        })
                        .map(|(key, _)| key.clone())
                        .collect();
                    Ok(revoked)
                },
            );
            let revoked = observed.unwrap_or_else(|_| active.keys().cloned().collect());
            for key in revoked {
                let held = active
                    .get(&key)
                    .map(|pointer| pointer.held.clone())
                    .unwrap_or_default();
                if let Err(error) = destroy_owned(&mut active, &key) {
                    cleanup_failures.insert(key, error.to_string());
                } else {
                    revoked_releases.insert(key, held);
                }
            }
        }
        let (cmd, cancelled) = match command {
            Ok(cmd) => cmd,
            Err(crossbeam_channel::RecvTimeoutError::Timeout) => continue,
            Err(crossbeam_channel::RecvTimeoutError::Disconnected) => break,
        };
        let (cursor_id, reply) = match &cmd {
            Cmd::Press {
                cursor_id, reply, ..
            }
            | Cmd::MoveTo {
                cursor_id, reply, ..
            }
            | Cmd::Release {
                cursor_id, reply, ..
            }
            | Cmd::Forget { cursor_id, reply } => (cursor_id, reply),
        };
        if cancelled.load(Ordering::Acquire) {
            let result = destroy_owned(&mut active, cursor_id);
            retain_cleanup_failure(&mut cleanup_failures, cursor_id, &result);
            continue;
        }
        if let Some(error) = cleanup_failures.get(cursor_id) {
            let _ = reply.try_send(Err(anyhow::anyhow!("{error}")));
            continue;
        }
        match cmd {
            Cmd::Press {
                cursor_id,
                window_id,
                x,
                y,
                button,
                reply,
            } => {
                if !cleanup_failures.is_empty() {
                    let _ = reply.try_send(Err(anyhow::anyhow!("cleanup_unconfirmed: primary-seat owner quarantined; no replacement pointer input permitted")));
                    continue;
                }
                revoked_releases.remove(&cursor_id);
                let r = super::hyprland::with_observation_deadline(
                    std::time::Instant::now() + std::time::Duration::from_secs(2),
                    || handle_press(&mut active, &cursor_id, window_id, x, y, button, &cancelled),
                );
                let cleanup_unconfirmed = r
                    .as_ref()
                    .err()
                    .is_some_and(|error| error.to_string().starts_with("cleanup_unconfirmed:"));
                let r = if cancelled.load(Ordering::Acquire) && !cleanup_unconfirmed {
                    destroy_owned(&mut active, &cursor_id).and_then(|()| {
                        Err(anyhow::anyhow!(
                            "cancelled_after_input: partial/unknown effect"
                        ))
                    })
                } else {
                    r
                };
                retain_cleanup_failure(&mut cleanup_failures, &cursor_id, &r);
                if let Some(pointer) = active.get_mut(&cursor_id) {
                    pointer.pending_replies.push(cancelled.clone());
                }
                if reply.send(r).is_err() {
                    let cleanup = destroy_owned(&mut active, &cursor_id);
                    retain_cleanup_failure(&mut cleanup_failures, &cursor_id, &cleanup);
                }
            }
            Cmd::MoveTo {
                cursor_id,
                x,
                y,
                reply,
            } => {
                let r = super::hyprland::with_observation_deadline(
                    std::time::Instant::now() + std::time::Duration::from_secs(2),
                    || handle_move(&mut active, &cursor_id, x, y, &cancelled),
                );
                let r = finish_command(&mut active, &cursor_id, r);
                retain_cleanup_failure(&mut cleanup_failures, &cursor_id, &r);
                if let Some(pointer) = active.get_mut(&cursor_id) {
                    pointer.pending_replies.push(cancelled.clone());
                }
                if reply.send(r).is_err() {
                    let cleanup = destroy_owned(&mut active, &cursor_id);
                    retain_cleanup_failure(&mut cleanup_failures, &cursor_id, &cleanup);
                }
            }
            Cmd::Release {
                cursor_id,
                button,
                reply,
            } => {
                if let Some(held) = revoked_releases.get_mut(&cursor_id) {
                    if held.remove(&evdev_pointer_button(button)) {
                        // Idle revocation already acknowledged this original
                        // device's release. No new wire event or connection.
                        let _ = reply.try_send(Ok(()));
                        continue;
                    }
                }
                let r = handle_release(&mut active, &cursor_id, button);
                let r = finish_command(&mut active, &cursor_id, r);
                retain_cleanup_failure(&mut cleanup_failures, &cursor_id, &r);
                let _ = reply.send(r);
            }
            Cmd::Forget { cursor_id, reply } => {
                revoked_releases.remove(&cursor_id);
                let result = destroy_owned(&mut active, &cursor_id);
                retain_cleanup_failure(&mut cleanup_failures, &cursor_id, &result);
                let _ = reply.send(result);
            }
        }
    }
    let keys: Vec<_> = active.keys().cloned().collect();
    for key in keys {
        let _ = destroy_owned(&mut active, &key);
    }
}

fn retain_cleanup_failure(
    failures: &mut HashMap<String, String>,
    cursor_id: &str,
    result: &anyhow::Result<()>,
) {
    if let Err(error) = result {
        if error.to_string().starts_with("cleanup_unconfirmed:") {
            failures.insert(cursor_id.into(), error.to_string());
        }
    }
}

fn revoke_cancelled_replies(pending: &mut Vec<Arc<AtomicBool>>) -> bool {
    let revoked = pending.iter().any(|flag| flag.load(Ordering::Acquire));
    // Check before dropping owner-only references: a timed-out caller may have
    // already dropped its Arc, but its release obligation must survive that.
    pending.retain(|flag| Arc::strong_count(flag) > 1);
    revoked
}

#[cfg(test)]
mod reply_cancellation_tests {
    use super::*;

    #[test]
    fn delivered_reply_then_receive_timeout_still_revokes_owned_pointer() {
        let caller = Arc::new(AtomicBool::new(false));
        let mut pending = vec![caller.clone()];
        assert!(!revoke_cancelled_replies(&mut pending));
        caller.store(true, Ordering::Release);
        drop(caller);
        assert!(revoke_cancelled_replies(&mut pending));
        assert!(pending.is_empty());
    }

    #[test]
    fn acknowledged_call_does_not_accumulate_or_revoke_reply_authority() {
        let caller = Arc::new(AtomicBool::new(false));
        let mut pending = vec![caller.clone()];
        drop(caller);
        assert!(!revoke_cancelled_replies(&mut pending));
        assert!(pending.is_empty());
    }
}

fn destroy_owned(
    active: &mut HashMap<String, ActivePointer>,
    cursor_id: &str,
) -> anyhow::Result<()> {
    let mut result = Ok(());
    if let Some(pointer) = active.remove(cursor_id) {
        // Destroying a virtual pointer does not release Hyprland's primary
        // seat buttons. Release only this device's tracked buttons, with no
        // motion, focus change, fresh connection or successor target.
        result = super::release_before_destroy(
            pointer.held,
            |button| pointer.vptr.button(0, button, ButtonState::Released),
            || { pointer.vptr.frame(); roundtrip_on_persistent(cursor_id) },
            || pointer.vptr.destroy(),
        ).map_err(|error| anyhow::anyhow!("cleanup_unconfirmed: partial/unknown effect; original connection terminated: {error}"));
        CONNS.with(|connections| {
            if let Some((connection, _)) = connections.borrow().get(cursor_id) {
                let _ = connection.flush();
            }
        });
    }
    forget_conn(cursor_id);
    result
}

fn finish_command(
    active: &mut HashMap<String, ActivePointer>,
    cursor_id: &str,
    result: anyhow::Result<()>,
) -> anyhow::Result<()> {
    match result {
        Ok(()) => Ok(()),
        Err(error) => {
            destroy_owned(active, cursor_id)?;
            Err(anyhow::anyhow!(
                "held_pointer_partial_unknown: owned cleanup acknowledged; do not replay: {error}"
            ))
        }
    }
}

fn handle_press(
    active: &mut HashMap<String, ActivePointer>,
    cursor_id: &str,
    window_id: u64,
    x: i32,
    y: i32,
    button: u8,
    cancelled: &AtomicBool,
) -> anyhow::Result<()> {
    if !active.is_empty() {
        anyhow::bail!("mouse_hold_conflict: generic primary-seat pointer already owns a held device; parallel independent holds require an isolated compositor route");
    }
    let target = if super::hyprland::is_session() {
        let window = super::hyprland::window_for_address(window_id)
            .ok_or_else(|| anyhow::anyhow!("native held-pointer target unavailable"))?;
        let frame = super::hyprland::local_window_frame(window.pid, window_id)?;
        Some((window, frame))
    } else {
        None
    };
    // Open a fresh session for this press — this binds the seat, the foreign-
    // toplevel manager, activates the target window, and creates a new vptr.
    // Keep the (out_w, out_h) but drop the queue + state at end of scope; the
    // vptr itself remains alive (Wayland objects survive their original queue
    // as long as the Connection is alive).
    let mut sess = super::open_vptr_session_cancellable_in(Some(window_id), None, Some(cancelled))?;
    let (w, h) = (sess.output_w, sess.output_h);
    sess.validate_layout_point(x, y)?;
    let (px, py) = sess.abs(x, y);
    let btn = evdev_pointer_button(button);

    if let Some((window, frame)) = &target {
        let validation = frame.validate_current().and_then(|()| {
            if !native_geometry_matches(window) {
                anyhow::bail!("native held-pointer target geometry changed during activation");
            }
            Ok(())
        });
        if let Err(error) = validation {
            sess.cleanup_owned()?;
            anyhow::bail!("held_pointer_partial_unknown: target changed after activation; owned cleanup acknowledged; {error}");
        }
    }

    if cancelled.load(Ordering::Acquire) {
        anyhow::bail!("cancelled_before_input");
    }

    sess.vptr.motion_absolute(0, px, py, w, h);
    sess.vptr.frame();
    sess.vptr.button(0, btn, ButtonState::Pressed);
    sess.held_buttons.insert(btn);
    sess.vptr.frame();
    if let Err(error) = super::bounded_roundtrip(&sess.conn, &mut sess.queue, &mut sess.state) {
        sess.cleanup_owned()?;
        return Err(anyhow::anyhow!(
            "held_pointer_partial_unknown: press acknowledgment failed; {error}"
        ));
    }

    // Take ownership of the vptr handle by extracting it from the session.
    // ZwlrVirtualPointerV1 is a Wayland proxy — cloning it gives another
    // handle to the same wire object; destroying it sends the destructor.
    let vptr = sess.vptr.clone();
    // Persist the live connection so the proxy stays valid after this fn returns
    // (the session goes out of scope; we need the conn alive).
    // We do this by leaking the connection into a process-static slot keyed by
    // cursor_id. Subsequent commands on the same cursor reuse this conn.
    persist_conn(cursor_id, sess.conn.clone());
    sess.owns_pointer = false;
    sess.held_buttons.clear();

    let mut held = HashSet::new();
    held.insert(btn);
    active.insert(
        cursor_id.to_string(),
        ActivePointer {
            pending_replies: Vec::new(),
            vptr,
            held,
            out_w: w,
            out_h: h,
            origin_x: sess.origin_x,
            origin_y: sess.origin_y,
            frame: sess.frame.clone(),
            target,
        },
    );
    Ok(())
}

fn handle_move(
    active: &mut HashMap<String, ActivePointer>,
    cursor_id: &str,
    x: i32,
    y: i32,
    cancelled: &AtomicBool,
) -> anyhow::Result<()> {
    let entry = active.get_mut(cursor_id).ok_or_else(|| {
        anyhow::anyhow!(
            "no held mouse button for cursor '{cursor_id}'; call mouse_button_down first"
        )
    })?;
    if entry.pending_replies.len() >= 32 {
        anyhow::bail!("persistent pointer unresolved reply budget exceeded");
    }
    if let Some(frame) = &entry.frame {
        frame.validate_current()?;
    }
    validate_owned_target(entry)?;
    if cancelled.load(Ordering::Acquire) {
        anyhow::bail!("cancelled_before_input");
    }
    if entry
        .frame
        .as_ref()
        .is_some_and(|frame| !frame.contains_layout_point(f64::from(x), f64::from(y)))
    {
        anyhow::bail!("desktop_coordinate_invalid: held motion outside powered coverage");
    }
    let (px, py) = super::pointer_abs(
        entry.origin_x,
        entry.origin_y,
        entry.out_w,
        entry.out_h,
        x,
        y,
    );
    entry
        .vptr
        .motion_absolute(0, px, py, entry.out_w, entry.out_h);
    entry.vptr.frame();
    roundtrip_on_persistent(cursor_id)?;
    Ok(())
}

fn native_geometry_matches(original: &super::hyprland::Window) -> bool {
    super::hyprland::window_for_address(original.address).is_some_and(|now| {
        now.pid == original.pid
            && now.x == original.x
            && now.y == original.y
            && now.width == original.width
            && now.height == original.height
            && now.visible
    })
}

fn validate_owned_target(pointer: &ActivePointer) -> anyhow::Result<()> {
    if let Some((window, frame)) = &pointer.target {
        frame.validate_current()?;
        if !native_geometry_matches(window) {
            anyhow::bail!(
                "native held-pointer target geometry changed; do not replay old local coordinates"
            );
        }
    }
    Ok(())
}

fn handle_release(
    active: &mut HashMap<String, ActivePointer>,
    cursor_id: &str,
    button: u8,
) -> anyhow::Result<()> {
    let btn = evdev_pointer_button(button);
    let drop_entry = {
        let entry = active
            .get_mut(cursor_id)
            .ok_or_else(|| anyhow::anyhow!("no held mouse button for cursor '{cursor_id}'"))?;
        if !entry.held.contains(&btn) {
            anyhow::bail!("mouse_release_not_owned: no matching held button");
        }
        entry.vptr.button(0, btn, ButtonState::Released);
        entry.vptr.frame();
        roundtrip_on_persistent(cursor_id)?;
        entry.held.remove(&btn);
        entry.held.is_empty()
    };
    if drop_entry {
        if let Some(p) = active.remove(cursor_id) {
            p.vptr.destroy();
            roundtrip_on_persistent(cursor_id).ok();
        }
        forget_conn(cursor_id);
    }
    Ok(())
}

// Process-static slots for Connection + EventQueue keyed by cursor_id. The
// EventQueue is !Send but we only touch these on the owner thread, so wrap
// in a thread-local-by-construction pattern: store inside the same map so
// the owner thread is the sole accessor.
//
// We use a per-thread static rather than a Mutex<HashMap> because the owner
// thread is the only accessor (no contention possible).
thread_local! {
    static CONNS: std::cell::RefCell<HashMap<String, (Connection, wayland_client::EventQueue<super::State>)>>
        = std::cell::RefCell::new(HashMap::new());
}

fn persist_conn(cursor_id: &str, conn: Connection) {
    let queue = conn.new_event_queue::<super::State>();
    CONNS.with(|c| {
        c.borrow_mut().insert(cursor_id.to_string(), (conn, queue));
    });
}

fn forget_conn(cursor_id: &str) {
    CONNS.with(|c| {
        c.borrow_mut().remove(cursor_id);
    });
}

fn roundtrip_on_persistent(cursor_id: &str) -> anyhow::Result<()> {
    CONNS.with(|c| {
        let mut b = c.borrow_mut();
        let (conn, queue) = b
            .get_mut(cursor_id)
            .ok_or_else(|| anyhow::anyhow!("no persistent connection for cursor '{cursor_id}'"))?;
        let mut tmp = super::State::default();
        super::bounded_roundtrip(conn, queue, &mut tmp)?;
        Ok(())
    })
}

// ── public API ────────────────────────────────────────────────────────────

/// Press and HOLD `button` (evdev code) at output coordinates `(x, y)` on the
/// toplevel identified by `window_id`. Subsequent `move_to` / `release` calls
/// targeting the same `cursor_id` reuse the same virtual-pointer device, so
/// the compositor treats the sequence as one logical drag rather than as
/// independent clicks. Errors if `cursor_id` already has a held button.
pub fn press(cursor_id: &str, window_id: u64, x: i32, y: i32, button: u8) -> anyhow::Result<()> {
    request(|tx_r| Cmd::Press {
        cursor_id: cursor_id.to_string(),
        window_id,
        x,
        y,
        button,
        reply: tx_r,
    })
}

/// Emit motion_absolute on the held cursor's virtual-pointer. Errors if there
/// is no held button for `cursor_id`.
pub fn move_to(cursor_id: &str, x: i32, y: i32) -> anyhow::Result<()> {
    request(|tx_r| Cmd::MoveTo {
        cursor_id: cursor_id.to_string(),
        x,
        y,
        reply: tx_r,
    })
}

/// Release `button` on the held cursor. If no other buttons remain held the
/// virtual-pointer is destroyed and its Wayland connection torn down.
pub fn release(cursor_id: &str, button: u8) -> anyhow::Result<()> {
    request(|tx_r| Cmd::Release {
        cursor_id: cursor_id.to_string(),
        button,
        reply: tx_r,
    })
}

/// Drop the entry for `cursor_id` without emitting any Wayland events.
/// Useful for recovery — if the agent thinks a button is held but the
/// compositor disagrees, this clears the local state without trying to
/// send a release that would error.
pub fn forget(cursor_id: &str) -> anyhow::Result<()> {
    request(|tx_r| Cmd::Forget {
        cursor_id: cursor_id.to_string(),
        reply: tx_r,
    })
}

fn request(make: impl FnOnce(Sender<anyhow::Result<()>>) -> Cmd) -> anyhow::Result<()> {
    let (reply, receive) = bounded(1);
    let cancelled = Arc::new(AtomicBool::new(false));
    tx().send_timeout(
        (make(reply), cancelled.clone()),
        std::time::Duration::from_secs(1),
    )
    .map_err(|error| anyhow::anyhow!("persistent pointer admission unavailable: {error}"))?;
    match receive.recv_timeout(std::time::Duration::from_secs(20)) {
        Ok(result) => result,
        Err(error) => {
            cancelled.store(true, Ordering::Release);
            Err(anyhow::anyhow!("cleanup_unconfirmed: owner reply unavailable; partial/unknown effect, do not replay: {error}"))
        }
    }
}
