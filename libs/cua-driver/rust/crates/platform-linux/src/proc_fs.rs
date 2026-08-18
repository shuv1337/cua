//! Linux process enumeration via the /proc filesystem.
//!
//! Each /proc/<pid>/status file contains Name:, Pid:, PPid: etc.
//! /proc/<pid>/cmdline is the full command line (NUL-separated).

use std::fs;
use std::path::Path;

#[derive(Debug, Clone)]
pub struct ProcessInfo {
    pub pid: u32,
    pub name: String,
    pub cmdline: String,
}

fn is_process_live_state(status: &str) -> bool {
    status
        .lines()
        .find_map(|line| {
            line.strip_prefix("State:")
                .and_then(|state| state.trim().chars().next())
        })
        .is_some_and(|state| !matches!(state, 'Z' | 'X'))
}

/// Return whether a PID still represents a live process.
///
/// A zombie remains visible under `/proc` until its parent reaps it, so an
/// existence check alone is not enough for callers that may query AT-SPI or
/// X11 state for the process. Treat zombies and dead processes as gone.
pub fn is_process_live(pid: u32) -> bool {
    let status = match fs::read_to_string(format!("/proc/{pid}/status")) {
        Ok(status) => status,
        Err(_) => return false,
    };

    is_process_live_state(&status)
}

fn process_start_time_from_stat(stat: &str) -> Option<u64> {
    // `comm` is parenthesized and may contain spaces. Fields after its closing
    // parenthesis begin with state (field 3); starttime is field 22.
    stat.rsplit_once(") ")?
        .1
        .split_whitespace()
        .nth(19)?
        .parse()
        .ok()
}

/// Kernel start-time token for one PID. Unlike the numeric PID alone, this
/// distinguishes a live process from a later process that reused its PID.
pub fn process_instance_id(pid: u32) -> Option<u64> {
    let stat = fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    process_start_time_from_stat(&stat)
}

/// Block until the exact process instance `(pid, instance_id)` exits.
///
/// Prefers `pidfd_open(2)`: the kernel file descriptor is bound to that
/// *process*, not to the numeric pid, so a `poll` wakeup is proof the exact
/// instance died — it cannot be satisfied by a later process that recycled
/// the pid. The `(pid, /proc starttime)` pair is re-checked immediately after
/// the descriptor is opened, which closes the open-time TOCTOU: if the pid
/// had already been recycled before `pidfd_open`, the identity check fails
/// and the caller is told the instance is gone.
///
/// Falls back to polling `/proc/<pid>/stat` starttime (kernels < 5.3, or a
/// seccomp policy that blocks the syscall) with `poll_interval`.
///
/// Returns once the instance is gone. This is a blocking call; run it on a
/// dedicated thread.
pub fn wait_for_process_exit(pid: u32, instance_id: u64, poll_interval: std::time::Duration) {
    if wait_for_process_exit_pidfd(pid, instance_id) {
        return;
    }
    while process_instance_id(pid) == Some(instance_id) {
        std::thread::sleep(poll_interval);
    }
}

/// `true` when the pidfd path conclusively observed the instance exit (or
/// proved it was already gone). `false` means the syscall is unavailable and
/// the caller must poll.
fn wait_for_process_exit_pidfd(pid: u32, instance_id: u64) -> bool {
    // Not this instance any more (or never was): nothing to wait for, and
    // opening a pidfd now would bind to an unrelated process.
    if process_instance_id(pid) != Some(instance_id) {
        return true;
    }
    // SAFETY: `pidfd_open` takes a pid and a flags word; it either returns a
    // new file descriptor or -1. No memory is shared with the kernel.
    let fd = unsafe { libc::syscall(libc::SYS_pidfd_open, pid as libc::pid_t, 0) };
    if fd < 0 {
        return false;
    }
    let fd = fd as libc::c_int;
    // Re-check identity now that the descriptor is pinned. If the pid was
    // recycled between the first check and the open, this fd belongs to the
    // wrong process — close it and report the instance as already gone.
    if process_instance_id(pid) != Some(instance_id) {
        // SAFETY: `fd` is a live descriptor this function just opened.
        unsafe { libc::close(fd) };
        return true;
    }
    loop {
        let mut poll_fd = libc::pollfd {
            fd,
            events: libc::POLLIN,
            revents: 0,
        };
        // SAFETY: one initialized `pollfd` is passed with a matching length.
        let ready = unsafe { libc::poll(&mut poll_fd, 1, -1) };
        if ready >= 0 {
            break;
        }
        // SAFETY: `__errno_location` returns a valid thread-local pointer.
        let errno = std::io::Error::last_os_error().raw_os_error();
        if errno != Some(libc::EINTR) {
            break;
        }
    }
    // SAFETY: `fd` is a live descriptor this function just opened.
    unsafe { libc::close(fd) };
    true
}

/// Return live processes whose environment contains exactly `KEY=VALUE`.
pub fn processes_with_env(key: &str, value: &str) -> Vec<u32> {
    let needle = format!("{key}={value}").into_bytes();
    let mut matches = Vec::new();
    let Ok(entries) = fs::read_dir("/proc") else {
        return matches;
    };
    for entry in entries.flatten() {
        let name = entry.file_name();
        let Ok(pid) = name.to_string_lossy().parse::<u32>() else {
            continue;
        };
        if !is_process_live(pid) {
            continue;
        }
        let Ok(environ) = fs::read(entry.path().join("environ")) else {
            continue;
        };
        if environ.split(|byte| *byte == 0).any(|item| item == needle) {
            matches.push(pid);
        }
    }
    matches.sort_unstable();
    matches
}

/// Return all running processes by reading /proc/<pid>/status.
pub fn list_processes() -> Vec<ProcessInfo> {
    let mut result = Vec::new();
    let proc_dir = Path::new("/proc");
    let entries = match fs::read_dir(proc_dir) {
        Ok(e) => e,
        Err(_) => return result,
    };

    for entry in entries.flatten() {
        let name = entry.file_name();
        let pid_str = name.to_string_lossy();
        let pid: u32 = match pid_str.parse() {
            Ok(p) => p,
            Err(_) => continue, // Skip non-numeric entries.
        };

        let status_path = proc_dir.join(&*pid_str).join("status");
        let status = match fs::read_to_string(&status_path) {
            Ok(s) => s,
            Err(_) => continue,
        };

        if !is_process_live_state(&status) {
            continue;
        }

        let proc_name = status
            .lines()
            .find(|l| l.starts_with("Name:"))
            .map(|l| l[5..].trim().to_owned())
            .unwrap_or_default();

        let cmdline_path = proc_dir.join(&*pid_str).join("cmdline");
        let cmdline = fs::read(cmdline_path)
            .ok()
            .map(|b| {
                // cmdline is NUL-separated; first entry is argv[0].
                let s = String::from_utf8_lossy(&b);
                s.split('\0').next().unwrap_or("").trim().to_owned()
            })
            .unwrap_or_default();

        result.push(ProcessInfo {
            pid,
            name: proc_name,
            cmdline,
        });
    }

    result.sort_by_key(|p| p.pid);
    result
}

#[cfg(test)]
mod tests {
    #[test]
    fn process_start_time_handles_spaces_in_comm() {
        let mut fields = vec!["S".to_owned()];
        fields.extend((4..=21).map(|field| field.to_string()));
        fields.push("987654".to_owned());
        let stat = format!("42 (fixture with spaces) {}", fields.join(" "));
        assert_eq!(super::process_start_time_from_stat(&stat), Some(987654));
    }

    #[test]
    fn process_states_fail_closed() {
        assert!(super::is_process_live_state(
            "Name:\ttest\nState:\tR (running)\n"
        ));
        assert!(super::is_process_live_state("State:\tS (sleeping)"));
        assert!(!super::is_process_live_state("State:\tZ (zombie)"));
        assert!(!super::is_process_live_state("State:\tX (dead)"));
        assert!(!super::is_process_live_state("Name:\ttest\n"));
        assert!(!super::is_process_live_state("State:\t"));
    }

    #[test]
    fn wait_for_process_exit_returns_immediately_for_a_stale_instance() {
        // An instance id that cannot match the running process proves the
        // identity guard short-circuits instead of waiting on the pid.
        let pid = std::process::id();
        let real = super::process_instance_id(pid).expect("own starttime is readable");
        let began = std::time::Instant::now();
        super::wait_for_process_exit(
            pid,
            real.wrapping_add(1),
            std::time::Duration::from_secs(30),
        );
        assert!(
            began.elapsed() < std::time::Duration::from_secs(5),
            "a mismatched instance id must not block"
        );
    }

    #[test]
    fn wait_for_process_exit_wakes_when_the_exact_process_dies() {
        let child = unsafe { libc::fork() };
        assert!(
            child >= 0,
            "fork failed: {}",
            std::io::Error::last_os_error()
        );
        if child == 0 {
            unsafe { libc::sleep(1) };
            unsafe { libc::_exit(0) };
        }

        let pid = child as u32;
        let instance = super::process_instance_id(pid).expect("child starttime is readable");
        let began = std::time::Instant::now();
        // A zombie still shares the pid's starttime, so reap concurrently —
        // the pidfd path wakes on exit regardless, the poll fallback needs
        // the /proc entry to vanish.
        let reaper = std::thread::spawn(move || {
            let mut status = 0;
            unsafe { libc::waitpid(child, &mut status, 0) }
        });
        super::wait_for_process_exit(pid, instance, std::time::Duration::from_millis(50));
        let waited = began.elapsed();
        assert_eq!(reaper.join().expect("reaper thread"), child);

        assert!(
            waited >= std::time::Duration::from_millis(500),
            "returned before the child could have exited: {waited:?}"
        );
        assert!(
            waited < std::time::Duration::from_secs(20),
            "did not wake on the child's exit: {waited:?}"
        );
    }

    #[test]
    fn real_zombie_is_not_live_and_disappears_after_reaping() {
        let child = unsafe { libc::fork() };
        assert!(
            child >= 0,
            "fork failed: {}",
            std::io::Error::last_os_error()
        );
        if child == 0 {
            unsafe { libc::_exit(0) };
        }

        let pid = child as u32;
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
        let mut observed_zombie = false;
        while std::time::Instant::now() < deadline {
            if std::fs::read_to_string(format!("/proc/{pid}/status"))
                .ok()
                .is_some_and(|status| status.contains("State:\tZ"))
            {
                observed_zombie = true;
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        let live_while_zombie = super::is_process_live(pid);
        let mut status = 0;
        let reaped = unsafe { libc::waitpid(child, &mut status, 0) };

        assert!(observed_zombie, "child never entered zombie state");
        assert!(!live_while_zombie, "zombie pid was reported live");
        assert_eq!(reaped, child, "failed to reap child");
        assert!(!super::is_process_live(pid), "reaped pid was reported live");
    }
}
