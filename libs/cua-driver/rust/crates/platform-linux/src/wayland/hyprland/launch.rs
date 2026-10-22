//! Hidden launch, adapted from downstream 1c3c5c889, d4199a971, 811c82a24,
//! 70f360855 and ec42f733c. Current Hyprland openLate/Lua move callbacks lose
//! focus or run after it was stolen. The matching plugin therefore owns the
//! synchronous pre-layout/pre-focus lifetime guard; Rust owns one-send launch
//! and exact process/window correlation, never a visible-workspace fallback.

use super::*;
use std::collections::HashMap;

const WORKSPACE: &str = "special:cua";
const TOKEN_KEY: &str = "CUA_LAUNCH_TOKEN";

#[derive(Debug, thiserror::Error)]
#[error("hidden_launch_guard_unavailable: enable the matching production plugin before launching; nothing was launched")]
pub(crate) struct GuardUnavailable;

fn shell_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\\''"))
}

fn lua_quote(value: &str) -> String {
    let mut result = String::from("\"");
    for byte in value.bytes() {
        match byte {
            b'\\' => result.push_str("\\\\"),
            b'"' => result.push_str("\\\""),
            0..=31 | 127..=255 => result.push_str(&format!("\\{byte:03}")),
            _ => result.push(char::from(byte)),
        }
    }
    result.push('"');
    result
}

fn launch_command(argv: &[String], token: &str) -> Result<String> {
    if argv.is_empty() || argv.iter().any(|arg| arg.contains('\0')) {
        bail!("hidden launch requires NUL-free argv");
    }
    let command = std::iter::once("env".to_owned())
        .chain([
            format!("{TOKEN_KEY}={token}"),
            "ACCESSIBILITY_ENABLED=1".into(),
            "NO_AT_BRIDGE=0".into(),
        ])
        .chain(argv.iter().cloned())
        .map(|arg| shell_quote(&arg))
        .collect::<Vec<_>>()
        .join(" ");
    Ok(format!(
        "dispatch hl.dsp.exec_cmd({}, {{ workspace = \"{WORKSPACE} silent\" }})",
        lua_quote(&command)
    ))
}

fn guard_available(status: &serde_json::Value) -> bool {
    status["configured"] == true
        && status["abi"]["match"] == true
        && status["input"]["protocol"] == 3
        && status["input"]["test_only"] == false
        && status["input"]["transport_ready"] == true
        && status["input"]["hidden_launch_version"] == 1
}

fn dispatch(peer: &CompositorPeer, command: &str) -> Result<()> {
    let mut ipc = ipc_connection()?;
    if CompositorPeer::from_ipc(&ipc)? != *peer {
        bail!("hidden launch compositor changed");
    }
    peer.verify_connection(&wayland_connection()?)?;
    // Mutations have one send only. A lost reply must not run another app or
    // another dispatcher. This profile uses the selected Hyprland's Lua API.
    let reply = read_reply(&mut ipc, command.as_bytes(), QUERY_TIMEOUT)?;
    if reply != b"ok" {
        bail!(
            "hidden launch not confirmed: {}",
            String::from_utf8_lossy(&reply)
        );
    }
    Ok(())
}

fn token_matches(bytes: &[u8], token: &str) -> bool {
    let expected = format!("{TOKEN_KEY}={token}");
    bytes
        .split(|byte| *byte == 0)
        .any(|entry| entry == expected.as_bytes())
}

fn token_processes(token: &str) -> HashMap<u32, u64> {
    let Ok(entries) = std::fs::read_dir("/proc") else {
        return HashMap::new();
    };
    entries
        .flatten()
        .filter_map(|entry| {
            let pid = entry.file_name().to_str()?.parse::<u32>().ok()?;
            let before = crate::proc_fs::process_instance_id(pid)?;
            if !crate::proc_fs::is_process_live(pid) {
                return None;
            }
            let mut bytes = Vec::new();
            std::fs::File::open(entry.path().join("environ"))
                .ok()?
                .take(64 * 1024 + 1)
                .read_to_end(&mut bytes)
                .ok()?;
            if bytes.len() > 64 * 1024
                || !token_matches(&bytes, token)
                || crate::proc_fs::process_instance_id(pid)? != before
            {
                return None;
            }
            Some((pid, before))
        })
        .collect()
}

/// A single-instance/D-Bus handoff is not guessed by title or class. Even when
/// this observation times out or the Driver exits, the plugin retains native
/// lifetime ownership of the original process and token-bearing children.
pub(crate) fn launch(argv: &[String]) -> Result<crate::x11::WindowInfo> {
    let (status, peer): (serde_json::Value, _) = query_with_peer("j/cua:status")?;
    if !guard_available(&status) {
        bail!(GuardUnavailable);
    }
    let mut nonce = [0; 16];
    getrandom::fill(&mut nonce)
        .map_err(|error| anyhow::anyhow!("launch token unavailable: {error}"))?;
    let token = format!("{:032x}", u128::from_ne_bytes(nonce));
    dispatch(&peer, &launch_command(argv, &token)?)?;
    let deadline = Instant::now() + Duration::from_secs(20);
    let mut owners = HashMap::new();
    while Instant::now() < deadline {
        owners.extend(token_processes(&token));
        owners.retain(|pid, start| {
            crate::proc_fs::is_process_live(*pid)
                && crate::proc_fs::process_instance_id(*pid) == Some(*start)
        });
        let (clients, observed): (Vec<serde_json::Value>, _) = query_with_peer("j/clients")?;
        if observed != peer {
            bail!("hidden launch compositor changed during correlation");
        }
        for raw in clients {
            let client: Client = serde_json::from_value(raw.clone())?;
            let Ok(pid) = u32::try_from(client.pid) else {
                continue;
            };
            if !client.mapped || !owners.contains_key(&pid) {
                continue;
            }
            if raw["workspace"]["name"] != WORKSPACE {
                bail!(
                    "hidden_launch_isolation_lost: token-owned window mapped outside special:cua"
                );
            }
            let window = windows_from_clients(vec![client], &HashSet::new())?
                .pop()
                .context("invalid token-owned launch window")?;
            return Ok(crate::x11::WindowInfo {
                xid: window.address,
                pid: Some(window.pid),
                app_name: window.app_id,
                title: window.title,
                is_on_screen: false,
                z_index: None,
                x: window.x,
                y: window.y,
                width: window.width,
                height: window.height,
            });
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    bail!(
        "hidden launch accepted but no exact token-owned window appeared; inspect before retrying"
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn launch_quotes_each_argv_and_keeps_the_silent_workspace_rule_outside_the_shell() {
        let command =
            launch_command(&["app".into(), "a'; touch /not-a-command\nλ".into()], "abc").unwrap();
        assert!(command.starts_with("dispatch hl.dsp.exec_cmd(\""));
        assert!(command.contains("workspace = \"special:cua silent\""));
        assert!(command.contains("'\\\\''"));
        assert!(command.contains("\\010\\206\\187"));
        assert!(!command.contains('\n'));
        assert!(launch_command(&["a\0b".into()], "abc").is_err());
        assert!(launch_command(&[], "abc").is_err());
    }
    #[test]
    fn token_ownership_is_exact_not_a_substring_or_a_process_name() {
        assert!(token_matches(b"OTHER=1\0CUA_LAUNCH_TOKEN=abc\0", "abc"));
        for bytes in [
            b"CUA_LAUNCH_TOKEN=abcd\0".as_slice(),
            b"NOT_CUA_LAUNCH_TOKEN=abc\0",
            b"X=CUA_LAUNCH_TOKEN=abc\0",
        ] {
            assert!(!token_matches(bytes, "abc"));
        }
    }
    #[test]
    fn hidden_launch_requires_the_enabled_matching_production_guard() {
        let status = serde_json::json!({"configured":true,"abi":{"match":true},"input":{
            "protocol":3,"test_only":false,"transport_ready":true,"hidden_launch_version":1}});
        assert!(guard_available(&status));
        for pointer in [
            "/configured",
            "/abi/match",
            "/input/protocol",
            "/input/transport_ready",
            "/input/hidden_launch_version",
        ] {
            let mut unavailable = status.clone();
            *unavailable.pointer_mut(pointer).unwrap() = serde_json::Value::Null;
            assert!(!guard_available(&unavailable), "{pointer}");
        }
        let mut experimental = status;
        experimental["input"]["test_only"] = true.into();
        assert!(!guard_available(&experimental));
    }
}
