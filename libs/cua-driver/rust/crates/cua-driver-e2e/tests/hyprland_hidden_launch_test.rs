//! Independent compositor/application evidence for hidden launch lifetime.
#![cfg(target_os = "linux")]

use std::io::Read;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use cua_driver_testkit::{spawn_in_job, workspace_root, ChildReaper, Driver, McpDriver};
use serde_json::{json, Value};

fn ipc(command: &str) -> Value {
    let output = Command::new("timeout")
        .args(["--kill-after=1s", "5s", "hyprctl", "-j", command])
        .output()
        .expect("bounded independent compositor query");
    assert!(output.status.success(), "hyprctl {command} failed");
    serde_json::from_slice(&output.stdout).expect("compositor JSON")
}

// Driver-created processes are not Child handles. Only terminate processes
// with this private fixture AND this test's unique journal argument. pidfds
// prevent PID reuse between ownership inspection and cleanup.
struct OwnedLaunch {
    script: PathBuf,
    journal: PathBuf,
}
impl Drop for OwnedLaunch {
    fn drop(&mut self) {
        let Ok(entries) = std::fs::read_dir("/proc") else {
            return;
        };
        for entry in entries.flatten() {
            let Some(pid) = entry
                .file_name()
                .to_str()
                .and_then(|pid| pid.parse::<u32>().ok())
            else {
                continue;
            };
            let fd = unsafe { libc::syscall(libc::SYS_pidfd_open, pid, 0) };
            if fd < 0 {
                continue;
            }
            let fd = unsafe { OwnedFd::from_raw_fd(fd as i32) };
            let Ok(cmdline) = std::fs::read(entry.path().join("cmdline")) else {
                continue;
            };
            let args: Vec<_> = cmdline.split(|b| *b == 0).collect();
            if args.contains(&self.script.as_os_str().as_encoded_bytes())
                && args.contains(&self.journal.as_os_str().as_encoded_bytes())
            {
                unsafe {
                    libc::syscall(
                        libc::SYS_pidfd_send_signal,
                        fd.as_raw_fd(),
                        libc::SIGKILL,
                        std::ptr::null::<libc::siginfo_t>(),
                        0,
                    );
                }
            }
        }
    }
}

#[test]
#[ignore = "prepared empty disposable Hyprland desktop, GTK3 and candidate Driver; native launch/focus isolation"]
fn splash_child_and_late_modal_stay_hidden_without_focus_theft() {
    assert_eq!(
        std::env::var("CUA_TEST_DUAL_DISPLAY_PREPARED").as_deref(),
        Ok("1")
    );
    assert!(std::env::var_os("DISPLAY").is_none(), "native Wayland only");
    assert!(
        ipc("clients").as_array().unwrap().is_empty(),
        "never use the human desktop"
    );
    let dir = tempfile::tempdir().unwrap();
    let script =
        workspace_root().join("crates/cua-driver-e2e/tests/support/hidden_launch_fixture.py");
    let journal = dir.path().join("hidden.jsonl");
    let _cleanup = OwnedLaunch {
        script: script.clone(),
        journal: journal.clone(),
    };
    let mut driver = McpDriver::spawn_named("hyprland-hidden-launch").unwrap();
    let mut sentinel_reaper = ChildReaper::new();
    let sentinel_journal = dir.path().join("sentinel.jsonl");
    let sentinel = spawn_in_job(
        Command::new("python3")
            .arg(&script)
            .args(["--role", "sentinel", "--journal"])
            .arg(&sentinel_journal)
            .env("GDK_BACKEND", "wayland")
            .stdout(Stdio::null())
            .stderr(Stdio::inherit()),
    )
    .unwrap();
    let sentinel_pid = sentinel.id();
    sentinel_reaper.push(sentinel);
    let ready = Instant::now() + Duration::from_secs(10);
    while ipc("activewindow")["pid"] != sentinel_pid {
        assert!(Instant::now() < ready, "sentinel did not become active");
        std::thread::sleep(Duration::from_millis(50));
    }
    let before = ipc("monitors");
    let sentinel_address = ipc("activewindow")["address"]
        .as_str()
        .unwrap()
        .trim_start_matches("0x")
        .to_owned();
    std::fs::write(&sentinel_journal, "").unwrap();
    let runtime = PathBuf::from(std::env::var_os("XDG_RUNTIME_DIR").unwrap());
    let signature = std::env::var("HYPRLAND_INSTANCE_SIGNATURE").unwrap();
    assert!(signature
        .bytes()
        .all(|b| b.is_ascii_alphanumeric() || b == b'_'));
    let mut events =
        UnixStream::connect(runtime.join("hypr").join(signature).join(".socket2.sock")).unwrap();
    events.set_nonblocking(true).unwrap();
    let launched = driver.call(
        "launch_app",
        json!({"launch_path":"python3", "additional_arguments":[script, "--journal", journal]}),
    );
    assert!(!launched.is_error(), "{}", launched.text());
    assert_eq!(launched.structured()["workspace"], "special:cua");
    // Splash is present, but main/child/late-modal have not mapped yet. The
    // compositor, not the Driver's lifetime or a startup-only sweep, owns them.
    drop(driver);
    let deadline = Instant::now() + Duration::from_secs(25);
    let mut event_log = Vec::new();
    let mut owned_addresses = std::collections::HashSet::new();
    let mut roles = std::collections::HashSet::new();
    loop {
        if let Ok(text) = std::fs::read_to_string(&journal) {
            for line in text.lines() {
                let event: Value = serde_json::from_str(line).unwrap();
                roles.insert(event["role"].as_str().unwrap().to_owned());
            }
        }
        let mut chunk = [0; 8192];
        loop {
            match events.read(&mut chunk) {
                Ok(0) => panic!("compositor event stream disconnected"),
                Ok(count) => {
                    event_log.extend_from_slice(&chunk[..count]);
                    assert!(event_log.len() < 1024 * 1024);
                }
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => break,
                Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
                Err(error) => panic!("compositor event read: {error}"),
            }
        }
        let clients = ipc("clients");
        let mut current_roles = std::collections::HashSet::new();
        for client in clients.as_array().unwrap() {
            if client["pid"] == sentinel_pid {
                continue;
            }
            assert!(
                client["title"]
                    .as_str()
                    .unwrap()
                    .starts_with("CUA hidden launch "),
                "unexpected foreign client"
            );
            current_roles.insert(client["title"].as_str().unwrap().to_owned());
            owned_addresses.insert(
                client["address"]
                    .as_str()
                    .unwrap()
                    .trim_start_matches("0x")
                    .to_owned(),
            );
            assert_eq!(
                client["workspace"]["name"], "special:cua",
                "launch window escaped hidden workspace"
            );
        }
        assert_eq!(
            ipc("activewindow")["pid"],
            sentinel_pid,
            "launch stole focus"
        );
        assert!(
            !std::fs::read_to_string(&sentinel_journal)
                .unwrap()
                .contains("focus-out"),
            "independent GTK sentinel lost keyboard focus during hidden launch"
        );
        let now = ipc("monitors");
        for (old, new) in before
            .as_array()
            .unwrap()
            .iter()
            .zip(now.as_array().unwrap())
        {
            assert_eq!(
                old["activeWorkspace"], new["activeWorkspace"],
                "launch switched workspace"
            );
            assert_eq!(
                old["specialWorkspace"], new["specialWorkspace"],
                "launch showed a special workspace"
            );
        }
        for line in std::str::from_utf8(&event_log).unwrap().lines() {
            if let Some(address) = line.strip_prefix("activewindowv2>>") {
                assert_eq!(
                    address.trim_start_matches("0x"),
                    sentinel_address,
                    "native focus changed even transiently: {line}"
                );
                assert!(
                    !owned_addresses.contains(address.trim_start_matches("0x")),
                    "transient native focus theft: {line}"
                );
            }
        }
        if ["splash", "main", "child", "late-modal"]
            .iter()
            .all(|role| roles.contains(*role))
            && ["main", "child", "late-modal"]
                .iter()
                .all(|role| current_roles.contains(&format!("CUA hidden launch {role}")))
        {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "splash/main/child/late-modal evidence incomplete: {roles:?}"
        );
        std::thread::sleep(Duration::from_millis(50));
    }
}
