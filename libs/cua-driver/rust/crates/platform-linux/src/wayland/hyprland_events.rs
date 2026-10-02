//! Private, incarnation-bound Hyprland event authority. A barrier drains all
//! currently readable events before returning an epoch; disconnect, malformed
//! input, overflow and reader loss revoke the subscription rather than guessing.
//!
//! A custom dispatch marker fences the ordered socket2 reader. Socket2 lacks
//! layout/mode/DPMS events in v0.56.2: complete desktop authority additionally
//! requires the loaded production v3 plugin's existing monotonic generations.
//! Neither observation mechanism makes userspace query -> input atomic.

use anyhow::{bail, Context, Result};
use std::io::Read;
use std::os::fd::AsRawFd;
use std::os::unix::net::UnixStream;
use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{mpsc, Arc, Mutex, OnceLock};
use std::time::Duration;

const MAX_PENDING: usize = 64 * 1024;
const MAX_DRAIN: usize = 256 * 1024;
const BARRIER_TIMEOUT: Duration = Duration::from_secs(3);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct EventStamp {
    generation: u64,
    epoch: u64,
    topology_epoch: u64,
}

impl EventStamp {
    #[cfg(test)]
    pub(super) fn test_stamp(generation: u64, epoch: u64, topology_epoch: u64) -> Self {
        Self {
            generation,
            epoch,
            topology_epoch,
        }
    }
    pub(super) fn client_identity(self) -> (u64, u64) {
        (self.generation, self.epoch)
    }
    pub(super) fn topology(self) -> Self {
        Self {
            epoch: self.topology_epoch,
            ..self
        }
    }
}

struct Tracker {
    incarnation: String,
    generation: u64,
    alive: Arc<AtomicBool>,
    commands: mpsc::SyncSender<(String, Option<u64>, mpsc::SyncSender<Result<EventStamp>>)>,
}

thread_local! {
    static WINDOW_EPOCHS: std::cell::RefCell<std::collections::HashMap<u64, u64>> = Default::default();
}

static NEXT_GENERATION: AtomicU64 = AtomicU64::new(1);

pub(super) fn barrier(
    incarnation: &str,
    peer: &super::hyprland::CompositorPeer,
) -> Result<EventStamp> {
    scoped_barrier(incarnation, peer, None)
}

pub(super) fn window_barrier(
    incarnation: &str,
    peer: &super::hyprland::CompositorPeer,
    address: u64,
) -> Result<EventStamp> {
    scoped_barrier(incarnation, peer, Some(address))
}

fn scoped_barrier(
    incarnation: &str,
    peer: &super::hyprland::CompositorPeer,
    address: Option<u64>,
) -> Result<EventStamp> {
    static TRACKER: OnceLock<Mutex<Option<Tracker>>> = OnceLock::new();
    let deadline = std::time::Instant::now() + super::hyprland::remaining_budget(BARRIER_TIMEOUT)?;
    let tracker_slot = TRACKER.get_or_init(|| Mutex::new(None));
    let mut slot = loop {
        match tracker_slot.try_lock() {
            Ok(slot) => break slot,
            Err(std::sync::TryLockError::Poisoned(error)) => break error.into_inner(),
            Err(std::sync::TryLockError::WouldBlock) => {
                if std::time::Instant::now() >= deadline {
                    bail!("Hyprland event authority lock deadline exceeded");
                }
                std::thread::sleep(Duration::from_millis(2));
            }
        }
    };
    if slot.as_ref().is_none_or(|tracker| {
        tracker.incarnation != incarnation || !tracker.alive.load(Ordering::Acquire)
    }) {
        // A new generation never revives an old frame, even if every monitor
        // has returned to exactly the old dimensions and origins.
        *slot = Some(connect(incarnation, &peer.path, peer.pid)?);
    }
    let tracker = slot
        .as_ref()
        .context("Hyprland event authority unavailable")?;
    let mut nonce = [0; 16];
    getrandom::fill(&mut nonce).map_err(|error| {
        tracker.alive.store(false, Ordering::Release);
        anyhow::anyhow!("Hyprland event marker randomness unavailable: {error}")
    })?;
    let nonce = u128::from_ne_bytes(nonce);
    let marker = format!("cua-authority-{}-{nonce:032x}", tracker.generation,);
    let (reply, receive) = mpsc::sync_channel(1);
    // Queue the reader before emitting a marker. It must observe that exact
    // marker, not merely reach EAGAIN before the compositor flushes socket2.
    tracker
        .commands
        .try_send((marker.clone(), address, reply))
        .map_err(|error| {
            tracker.alive.store(false, Ordering::Release);
            anyhow::anyhow!("Hyprland event barrier unavailable or saturated: {error}")
        })?;
    let result = (|| -> Result<EventStamp> {
        super::hyprland::event_marker(&marker, peer)?;
        receive
            .recv_timeout(super::hyprland::remaining_budget(BARRIER_TIMEOUT)?)
            .context("Hyprland event barrier lost/deadline exceeded")?
    })();
    let stamp = match result {
        Ok(stamp) => stamp,
        Err(error) => {
            // A failed marker/deadline permanently revokes this generation;
            // late reader progress must not revive any previously bound frame.
            tracker.alive.store(false, Ordering::Release);
            return Err(error);
        }
    };
    if stamp.generation != tracker.generation || !tracker.alive.load(Ordering::Acquire) {
        bail!("Hyprland event authority revoked");
    }
    Ok(stamp)
}

fn connect(incarnation: &str, ipc_path: &Path, peer: libc::pid_t) -> Result<Tracker> {
    let path = ipc_path.with_file_name(".socket2.sock");
    let socket = socket2::Socket::new(socket2::Domain::UNIX, socket2::Type::STREAM, None)?;
    socket.connect_timeout(
        &socket2::SockAddr::unix(path)?,
        super::hyprland::remaining_budget(Duration::from_secs(1))?,
    )?;
    if super::hyprland::peer_pid(socket.as_raw_fd())? != peer {
        bail!("Hyprland event stream belongs to a different compositor");
    }
    let stream: UnixStream = socket.into();
    stream.set_nonblocking(true)?;
    let generation = NEXT_GENERATION
        .fetch_update(Ordering::AcqRel, Ordering::Acquire, |n| n.checked_add(1))
        .map_err(|_| anyhow::anyhow!("Hyprland event generation exhausted"))?;
    let alive = Arc::new(AtomicBool::new(true));
    let (commands, receive) = mpsc::sync_channel(16);
    let reader_alive = alive.clone();
    std::thread::Builder::new()
        .name("cua-hyprland-events".into())
        .spawn(move || {
            read_events(stream, receive, generation);
            reader_alive.store(false, Ordering::Release);
        })?;
    Ok(Tracker {
        incarnation: incarnation.into(),
        generation,
        alive,
        commands,
    })
}

fn read_events(
    mut stream: UnixStream,
    commands: mpsc::Receiver<(String, Option<u64>, mpsc::SyncSender<Result<EventStamp>>)>,
    generation: u64,
) {
    let mut pending = Vec::new();
    let mut epoch = 0;
    loop {
        match commands.recv_timeout(Duration::from_millis(50)) {
            Ok((marker, address, reply)) => {
                let result =
                    drain_marker(&mut stream, &mut pending, &mut epoch, &marker).map(|()| {
                        EventStamp {
                            generation,
                            topology_epoch: epoch,
                            epoch: address.map_or(epoch, |address| {
                                WINDOW_EPOCHS.with(|epochs| {
                                    epochs.borrow().get(&address).copied().unwrap_or(0)
                                })
                            }),
                        }
                    });
                let failed = result.is_err();
                let _ = reply.try_send(result);
                if failed {
                    return;
                }
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {
                if drain(&mut stream, &mut pending, &mut epoch).is_err() {
                    return;
                }
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => return,
        }
    }
}

fn drain_marker(
    stream: &mut UnixStream,
    pending: &mut Vec<u8>,
    epoch: &mut u64,
    marker: &str,
) -> Result<()> {
    let deadline = std::time::Instant::now() + Duration::from_secs(2);
    loop {
        let mut chunk = [0; 8192];
        match stream.read(&mut chunk) {
            Ok(0) => bail!("Hyprland event stream disconnected"),
            Ok(count) => {
                if pending.len() + count > MAX_PENDING {
                    bail!("Hyprland event stream exceeds authority limits");
                }
                pending.extend_from_slice(&chunk[..count]);
                while let Some(end) = pending.iter().position(|b| *b == b'\n') {
                    let line = std::str::from_utf8(&pending[..end])?;
                    let found = line == format!("custom>>{marker}");
                    if !found && invalidates_topology(line)? {
                        *epoch = epoch
                            .checked_add(1)
                            .context("Hyprland topology epoch exhausted")?;
                    }
                    pending.drain(..=end);
                    if found {
                        return Ok(());
                    }
                }
            }
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {}
            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(error) => return Err(error.into()),
        }
        let timeout = deadline
            .saturating_duration_since(std::time::Instant::now())
            .as_millis()
            .min(i32::MAX as u128) as i32;
        if timeout == 0 {
            bail!("Hyprland ordered event marker deadline exceeded");
        }
        let mut poll = libc::pollfd {
            fd: stream.as_raw_fd(),
            events: libc::POLLIN,
            revents: 0,
        };
        let result = unsafe { libc::poll(&mut poll, 1, timeout) };
        if result == 0 {
            bail!("Hyprland ordered event marker deadline exceeded");
        }
        if result < 0 {
            return Err(std::io::Error::last_os_error().into());
        }
    }
}

fn drain(stream: &mut UnixStream, pending: &mut Vec<u8>, epoch: &mut u64) -> Result<()> {
    let mut chunk = [0; 8192];
    let mut drained = 0;
    loop {
        match stream.read(&mut chunk) {
            Ok(0) => bail!("Hyprland event stream disconnected"),
            Ok(count) => {
                drained += count;
                if drained > MAX_DRAIN || pending.len() + count > MAX_PENDING {
                    bail!("Hyprland event stream exceeds authority limits");
                }
                pending.extend_from_slice(&chunk[..count]);
                while let Some(end) = pending.iter().position(|b| *b == b'\n') {
                    let line = std::str::from_utf8(&pending[..end])?;
                    if invalidates_topology(line)? {
                        *epoch = epoch
                            .checked_add(1)
                            .context("Hyprland topology epoch exhausted")?;
                    }
                    pending.drain(..=end);
                }
            }
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                // A split line is unfinished authority, not a safe barrier.
                return Ok(());
            }
            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => {}
            Err(error) => return Err(error.into()),
        }
    }
}

fn invalidates_topology(line: &str) -> Result<bool> {
    let (name, data) = line.split_once(">>").context("malformed Hyprland event")?;
    if matches!(name, "openwindow" | "closewindow") {
        let address = data
            .split(',')
            .next()
            .context("missing Hyprland client event address")?;
        let address = u64::from_str_radix(address.trim_start_matches("0x"), 16)
            .context("invalid Hyprland client event address")?;
        WINDOW_EPOCHS.with(|epochs| -> Result<()> {
            let mut epochs = epochs.borrow_mut();
            if epochs.len() >= 65536 && !epochs.contains_key(&address) {
                bail!("Hyprland client epoch budget exhausted");
            }
            let epoch = epochs.entry(address).or_default();
            *epoch = epoch
                .checked_add(1)
                .context("Hyprland client epoch exhausted")?;
            Ok(())
        })?;
    }
    if name.is_empty() || !name.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_') {
        bail!("invalid Hyprland event name");
    }
    // Hyprland v0.56.2 socket2 informational vocabulary. Monitor/config and
    // unknown events still revoke. Complete layout/mode/DPMS authority ALSO
    // requires the production plugin generation; this list cannot replace it.
    // Screencopy's own start/stop and foreign custom markers are not topology.
    Ok(!matches!(
        name,
        "activewindow"
            | "activewindowv2"
            | "workspace"
            | "workspacev2"
            | "focusedmon"
            | "focusedmonv2"
            | "createworkspace"
            | "createworkspacev2"
            | "destroyworkspace"
            | "destroyworkspacev2"
            | "renameworkspace"
            | "openwindow"
            | "closewindow"
            | "movewindow"
            | "movewindowv2"
            | "windowtitle"
            | "windowtitlev2"
            | "changefloatingmode"
            | "fullscreen"
            | "urgent"
            | "minimized"
            | "pin"
            | "screencast"
            | "screencastv2"
            | "custom"
            | "openlayer"
            | "closelayer"
            | "bell"
            | "changeworkspaceid"
            | "kill"
            | "lockgroups"
            | "moveintogroup"
            | "moveoutofgroup"
            | "submap"
            | "togglegroup"
            | "activelayout"
            | "moveworkspace"
            | "moveworkspacev2"
            | "activespecial"
            | "activespecialv2"
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn event_barrier_observes_aba_and_refuses_loss_or_malformed_authority() {
        let (mut reader, mut writer) = UnixStream::pair().unwrap();
        reader.set_nonblocking(true).unwrap();
        let mut pending = Vec::new();
        let mut epoch = 0;
        writer.write_all(b"monitorremoved>>DP-1\nmonitoradded>>DP-1\nconfigreloaded>>\nactivewindowv2>>0x123\n").unwrap();
        drain(&mut reader, &mut pending, &mut epoch).unwrap();
        assert_eq!(epoch, 3);
        drop(writer);
        assert_eq!(
            drain(&mut reader, &mut pending, &mut epoch)
                .unwrap_err()
                .to_string(),
            "Hyprland event stream disconnected"
        );
        assert_eq!(
            invalidates_topology("broken").unwrap_err().to_string(),
            "malformed Hyprland event"
        );
        assert!(invalidates_topology("futuremonitorchange>>DP-1").unwrap());
    }

    #[test]
    fn incomplete_and_oversized_stream_cannot_supply_a_safe_stamp() {
        let (mut reader, mut writer) = UnixStream::pair().unwrap();
        reader.set_nonblocking(true).unwrap();
        writer.write_all(b"monitorremoved>>").unwrap();
        let mut pending = Vec::new();
        drain(&mut reader, &mut pending, &mut 0).unwrap();
        assert_eq!(pending, b"monitorremoved>>");
        let (mut reader, mut writer) = UnixStream::pair().unwrap();
        reader.set_nonblocking(true).unwrap();
        writer.write_all(&vec![b'x'; MAX_PENDING + 1]).unwrap();
        assert_eq!(
            drain(&mut reader, &mut Vec::new(), &mut 0)
                .unwrap_err()
                .to_string(),
            "Hyprland event stream exceeds authority limits"
        );
    }

    #[test]
    fn ordered_marker_accounts_for_prior_aba_not_unrelated_custom_markers() {
        let (mut reader, mut writer) = UnixStream::pair().unwrap();
        reader.set_nonblocking(true).unwrap();
        writer
            .write_all(b"monitorremoved>>DP-1\nmonitoradded>>DP-1\ncustom>>other\ncustom>>ours\n")
            .unwrap();
        let mut epoch = 0;
        drain_marker(&mut reader, &mut Vec::new(), &mut epoch, "ours").unwrap();
        assert_eq!(epoch, 2);
    }

    #[test]
    fn capture_layer_bell_and_foreign_custom_interleaving_preserve_topology() {
        let (mut reader, mut writer) = UnixStream::pair().unwrap();
        reader.set_nonblocking(true).unwrap();
        writer.write_all(b"screencast>>1,0\nscreencastv2>>1,0,DP-1\nopenlayer>>overlay\nbell>>123\ncustom>>foreign-runtime\ncloselayer>>overlay\nscreencast>>0,0\nscreencastv2>>0,0,DP-1\ncustom>>ours\n").unwrap();
        let mut epoch = 0;
        drain_marker(&mut reader, &mut Vec::new(), &mut epoch, "ours").unwrap();
        assert_eq!(epoch, 0);
        for name in [
            "activelayout",
            "activespecial",
            "activespecialv2",
            "activewindow",
            "activewindowv2",
            "bell",
            "changefloatingmode",
            "changeworkspaceid",
            "closelayer",
            "createworkspace",
            "createworkspacev2",
            "custom",
            "destroyworkspace",
            "destroyworkspacev2",
            "focusedmon",
            "focusedmonv2",
            "fullscreen",
            "kill",
            "lockgroups",
            "minimized",
            "moveintogroup",
            "moveoutofgroup",
            "movewindow",
            "movewindowv2",
            "moveworkspace",
            "moveworkspacev2",
            "openlayer",
            "pin",
            "renameworkspace",
            "screencast",
            "screencastv2",
            "submap",
            "togglegroup",
            "urgent",
            "windowtitle",
            "windowtitlev2",
            "workspace",
            "workspacev2",
        ] {
            assert!(
                !invalidates_topology(&format!("{name}>>payload")).unwrap(),
                "{name}"
            );
        }
        for name in [
            "monitoradded",
            "monitoraddedv2",
            "monitorremoved",
            "monitorremovedv2",
            "configreloaded",
            "futuremode",
        ] {
            assert!(
                invalidates_topology(&format!("{name}>>payload")).unwrap(),
                "{name}"
            );
        }
    }

    #[test]
    fn parent_readonly_capture_sequence_seven_custom_four_start_stop_pairs_is_informational() {
        let (mut reader, mut writer) = UnixStream::pair().unwrap();
        reader.set_nonblocking(true).unwrap();
        for i in 0..7 {
            writer
                .write_all(format!("custom>>foreign-or-own-{i}\n").as_bytes())
                .unwrap();
        }
        for (started, output) in [(1, "DP-2"), (0, "DP-2"), (1, "DP-1"), (0, "DP-1")] {
            writer
                .write_all(
                    format!("screencast>>{started},0\nscreencastv2>>{started},0,{output}\n")
                        .as_bytes(),
                )
                .unwrap();
        }
        let mut epoch = 0;
        drain(&mut reader, &mut Vec::new(), &mut epoch).unwrap();
        assert_eq!(epoch, 0);
        writer
            .write_all(b"monitorremoved>>DP-1\nmonitoradded>>DP-1\ncustom>>complete\n")
            .unwrap();
        drain_marker(&mut reader, &mut Vec::new(), &mut epoch, "complete").unwrap();
        assert_eq!(epoch, 2);
    }

    #[test]
    fn client_lifetime_epoch_is_address_local_not_other_monitor_or_client_state() {
        WINDOW_EPOCHS.with(|epochs| epochs.borrow_mut().clear());
        assert!(!invalidates_topology("closewindow>>1234").unwrap());
        assert!(!invalidates_topology("openwindow>>1234,1,app,title").unwrap());
        assert!(!invalidates_topology("openwindow>>5678,2,other,title").unwrap());
        assert!(invalidates_topology("configreloaded>>").unwrap());
        WINDOW_EPOCHS.with(|epochs| {
            assert_eq!(epochs.borrow().get(&0x1234), Some(&2));
            assert_eq!(epochs.borrow().get(&0x5678), Some(&1));
        });
    }
}
