//! Hidden-workspace launch, adapted from downstream 1c3c5c889, d4199a971,
//! 811c82a24, 70f360855 and ec42f733c. Keep current IPC peer attestation,
//! deadlines and window identities rather than the old hyprctl subprocesses.

use super::*;
use std::collections::HashMap;

const WORKSPACE: &str = "special:cua";
const TOKEN_KEY: &str = "CUA_LAUNCH_TOKEN";

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

/// Mutations have one send only. A lost reply is not permission to launch twice
/// or to run on the human workspace. The selected profile uses Hyprland's Lua API.
fn dispatch(peer: &CompositorPeer, command: &str) -> Result<()> {
    let mut ipc = ipc_connection()?;
    if CompositorPeer::from_ipc(&ipc)? != *peer {
        bail!("hidden launch compositor changed");
    }
    let connection = wayland_connection()?;
    peer.verify_connection(&connection)?;
    let reply = read_reply(&mut ipc, command.as_bytes(), QUERY_TIMEOUT)?;
    if reply != b"ok" {
        bail!(
            "hidden launch command refused: {}",
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
            // Bounded and scoped to this launch's exact inherited token. A wrapper
            // and its descendants may all carry it; that is not ambiguous ownership.
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

fn guard_key(pid: u32, generation: u64) -> String {
    format!("cua_bg_{pid}_{generation}")
}

fn guard_command(pid: u32, generation: u64) -> String {
    let key = guard_key(pid, generation);
    // Recheck starttime inside the callback as well as in the Rust lifetime
    // guard. A delayed event must not move a successor which reused the PID.
    // All interpolated values are integers or fixed strings, never app text.
    format!(
        r#"lua local key="{key}"; if _G[key] then _G[key]:unregister() end; _G[key]=hl.on("window.open",function(ev) local w=ev and ev.window; if not w or w.pid~={pid} then return end; local f=io.open("/proc/{pid}/stat","r"); if not f then return end; local s=f:read("*a"); f:close(); local rest=s:match("^.*%)%s+(.+)$"); local n=0; local start=nil; if rest then for v in rest:gmatch("%S+") do n=n+1; if n==20 then start=v; break end end end; if start~="{generation}" then return end; local ws=w.workspace; if not ws or ws.name~="{WORKSPACE}" then hl.dispatch(hl.dsp.window.move({{window=w,workspace="{WORKSPACE}",silent=true}})) end end)"#
    )
}

fn enforce(peer: &CompositorPeer, owners: &HashMap<u32, u64>) -> Result<()> {
    for client in query::<Vec<Client>>("j/clients")? {
        let Ok(pid) = u32::try_from(client.pid) else {
            continue;
        };
        let Some(generation) = owners.get(&pid) else {
            continue;
        };
        if crate::proc_fs::process_instance_id(pid) != Some(*generation) {
            continue;
        }
        // Preserve any deliberate special-workspace placement, as downstream
        // did; only ordinary workspaces need correction after splash/child maps.
        if client.mapped && client.workspace.id >= 0 {
            let address = u64::from_str_radix(
                client.address.strip_prefix("0x").unwrap_or(&client.address),
                16,
            )?;
            if address == 0 {
                bail!("invalid hidden launch window address");
            }
            dispatch(peer, &format!(
                "dispatch hl.dsp.window.move({{window=\"address:0x{address:x}\",workspace=\"{WORKSPACE}\",silent=true}})"
            ))?;
        }
    }
    Ok(())
}

fn update_guards(
    peer: &CompositorPeer,
    owners: &mut HashMap<u32, u64>,
    live: &HashMap<u32, u64>,
) -> Result<()> {
    for (&pid, &generation) in live {
        if owners.get(&pid) != Some(&generation) {
            dispatch(peer, &guard_command(pid, generation))?;
            owners.insert(pid, generation);
        }
    }
    let retired: Vec<_> = owners
        .iter()
        .filter(|(pid, generation)| live.get(pid) != Some(generation))
        .map(|(&pid, &generation)| (pid, generation))
        .collect();
    for (pid, generation) in retired {
        remove_guard(peer, pid, generation)?;
        owners.remove(&pid);
    }
    enforce(peer, owners)
}

fn remove_guard(peer: &CompositorPeer, pid: u32, generation: u64) -> Result<()> {
    let key = guard_key(pid, generation);
    dispatch(
        peer,
        &format!("lua if _G.{key} then _G.{key}:unregister(); _G.{key}=nil end"),
    )
}

/// Launch without a fallback onto the visible workspace. Success requires an
/// exact token-owned window; a single-instance/D-Bus handoff is not guessed by
/// title or class. The guard survives the splash and remains until every
/// token-bearing process exits, including reparented children and late modals.
pub(crate) fn launch(argv: &[String]) -> Result<crate::x11::WindowInfo> {
    let ipc = ipc_connection()?;
    let peer = CompositorPeer::from_ipc(&ipc)?;
    drop(ipc);
    peer.verify_connection(&wayland_connection()?)?;
    let mut nonce = [0; 16];
    getrandom::fill(&mut nonce)
        .map_err(|error| anyhow::anyhow!("launch token unavailable: {error}"))?;
    let token = format!("{:032x}", u128::from_ne_bytes(nonce));
    let command = launch_command(argv, &token)?;
    let (reply, receive) = std::sync::mpsc::sync_channel(1);
    // Own the complete operation before sending exec. Cancellation of the tool
    // or failure to start a second thread must not strand a launched process.
    std::thread::Builder::new()
        .name("cua-hidden-launch".into())
        .spawn(move || {
            if let Err(error) = dispatch(&peer, &command) {
                let _ = reply.send(Err(error));
                return;
            }
            let mut reply = Some(reply);
            let mut owners = HashMap::new();
            let deadline = Instant::now() + Duration::from_secs(20);
            let mut empty_since = None;
            loop {
                let mut live = token_processes(&token);
                // Once discovered, a process is guarded for its actual lifetime,
                // even if it erases the environment token after initialization.
                for (&pid, &generation) in &owners {
                    if crate::proc_fs::is_process_live(pid)
                        && crate::proc_fs::process_instance_id(pid) == Some(generation) {
                        live.insert(pid, generation);
                    }
                }
                if let Err(error) = update_guards(&peer, &mut owners, &live) {
                    tracing::error!(%error, "hidden launch guard lost compositor authority");
                    if let Some(reply) = reply.take() { let _ = reply.send(Err(error)); }
                    break;
                }
                if reply.is_some() {
                    let found = super::super::list_windows_dispatch(None).into_iter().find(|window| {
                        window.pid.is_some_and(|pid| live.get(&pid).is_some_and(|generation| {
                            crate::proc_fs::process_instance_id(pid) == Some(*generation)
                        }))
                    });
                    if let Some(window) = found {
                        let _ = reply.take().unwrap().send(Ok(window));
                    } else if Instant::now() >= deadline {
                        let _ = reply.take().unwrap().send(Err(anyhow::anyhow!(
                            "hidden launch accepted but no exact token-owned window appeared; inspect before retrying"
                        )));
                    }
                }
                if live.is_empty() {
                    if reply.is_none() && empty_since.get_or_insert_with(Instant::now).elapsed()
                        >= Duration::from_secs(5)
                    {
                        break;
                    }
                } else {
                    empty_since = None;
                }
                std::thread::sleep(Duration::from_millis(250));
            }
            for (pid, generation) in owners {
                let _ = remove_guard(&peer, pid, generation);
            }
        })?;
    receive
        .recv_timeout(Duration::from_secs(25))
        .context("hidden launch worker did not confirm its outcome")?
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
    fn late_window_guard_binds_pid_starttime_and_never_activates_a_workspace() {
        let command = guard_command(42, 123);
        assert!(command.contains("w.pid~=42"));
        assert!(command.contains("start~=\"123\""));
        assert!(command.contains("/proc/42/stat"));
        assert!(command.contains("silent=true"));
        assert!(!command.contains("hl.dsp.focus"));
        assert!(!command.contains("workspace.switch"));
        assert_ne!(guard_key(42, 123), guard_key(42, 124));
    }
}
