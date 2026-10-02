//! Read-only native workspace isolation, including assertion-unwind cleanup.
use std::cell::RefCell;
use std::collections::BTreeMap;
use std::ops::{Deref, DerefMut};
use std::process::Command;
use std::time::{Duration, Instant};

use cua_driver_testkit::{Driver, McpDriver};
use serde_json::{json, Value};

pub fn query(topic: &str) -> Result<Value, String> {
    let output = Command::new("timeout")
        .args(["--kill-after=1s", "10s", "hyprctl", "-j", topic])
        .output()
        .map_err(|_| format!("cannot run bounded hyprctl {topic}"))?;
    if !output.status.success() {
        return Err(format!("hyprctl {topic} failed"));
    }
    if output.stdout.len() > 4 * 1024 * 1024 {
        return Err(format!("oversized hyprctl {topic}"));
    }
    serde_json::from_slice(&output.stdout).map_err(|_| format!("invalid hyprctl {topic} JSON"))
}

fn need(condition: bool, why: &str) -> Result<(), String> {
    if condition {
        Ok(())
    } else {
        Err(why.to_owned())
    }
}

fn array(v: &Value) -> Result<&Vec<Value>, String> {
    v.as_array()
        .ok_or_else(|| "missing native observation array".to_owned())
}

fn output_ids(monitors: &Value) -> Result<[i64; 2], String> {
    let mut ids = [0; 2];
    for (i, (name, workspace)) in [("DP-2", 91), ("DP-1", 92)].iter().enumerate() {
        let m = array(monitors)?
            .iter()
            .find(|m| m["name"] == *name)
            .ok_or_else(|| "missing prepared output".to_owned())?;
        ids[i] = m["id"]
            .as_i64()
            .ok_or_else(|| "missing output identity".to_owned())?;
        need(
            m["activeWorkspace"]["id"] == *workspace,
            "prepared workspace no longer active",
        )?;
        need(
            m["specialWorkspace"]["id"] == 0,
            "special workspace is shown",
        )?;
    }
    need(ids[0] != ids[1], "duplicate output identity")?;
    Ok(ids)
}

fn empty_workspaces(workspaces: &Value) -> Result<(), String> {
    workspace_counts(workspaces, &[])
}

fn workspace_counts(workspaces: &Value, bindings: &[Binding]) -> Result<(), String> {
    for (name, id) in [("DP-2", 91), ("DP-1", 92)] {
        let ws = array(workspaces)?
            .iter()
            .find(|w| w["id"] == id)
            .ok_or_else(|| "prepared workspace missing".to_owned())?;
        let count = bindings
            .iter()
            .filter(|b| 91 + b.index as i64 == id)
            .count();
        need(
            ws["monitor"] == name && ws["windows"] == count && ws["hasfullscreen"] == (count > 0),
            "prepared workspace count/fullscreen/output differs from exact owned fixtures",
        )?;
    }
    Ok(())
}

pub fn no_overlay_layers(layers: &Value) -> Result<(), String> {
    for name in ["DP-2", "DP-1"] {
        let level = layers[name]["levels"]["3"]
            .as_array()
            .ok_or_else(|| "overlay-layer observation unavailable".to_owned())?;
        need(
            level.is_empty(),
            "overlay layer present; parent must prepare do-not-disturb",
        )?;
    }
    Ok(())
}

// hidden:false is NOT a workspace visibility predicate. Managed normal and
// hidden-special clients remain allowed when they are not on an active workspace.
fn inactive_managed(c: &Value, ids: [i64; 2]) -> Result<(), String> {
    let ws = c["workspace"]["id"]
        .as_i64()
        .ok_or_else(|| "ambiguous unmanaged workspace".to_owned())?;
    let monitor = c["monitor"]
        .as_i64()
        .ok_or_else(|| "ambiguous unmanaged monitor".to_owned())?;
    need(c["pinned"].is_boolean(), "ambiguous pinning metadata")?;
    // v0.56.2 assigns unmanaged X11 surfaces to a workspace on map/configure.
    // IPC provides no overrideRedirect field. Unknown workspace/monitor is
    // ambiguous and refused; known inactive workspaces are safe, even for X11.
    need(ws != 0, "unmanaged or ambiguous workspace")?;
    need(
        ws != 91 && ws != 92,
        "non-fixture client in a visible prepared workspace",
    )?;
    need(
        !(ids.contains(&monitor) && c["pinned"] == true),
        "pinned client on a prepared output",
    )
}

fn client_snapshot(clients: &Value) -> Result<BTreeMap<String, Value>, String> {
    let mut snapshot = BTreeMap::new();
    for c in array(clients)? {
        let address = c["address"]
            .as_str()
            .ok_or_else(|| "missing client address".to_owned())?;
        // An inactive managed X11 client can legitimately advertise PID 0.
        // Workspace/pinning proves non-visibility; preserve that value rather
        // than rejecting an ordinary inactive client for missing X11 PID hints.
        need(c["pid"].as_u64().is_some(), "missing client PID field")?;
        // Preserve all reported client fields except the two documented focus
        // bookkeeping fields. Never print these private snapshots in diagnostics.
        let mut stable = c.clone();
        let object = stable
            .as_object_mut()
            .ok_or_else(|| "invalid client record".to_owned())?;
        object.remove("focusHistoryID");
        object.remove("fullscreenClient");
        need(
            snapshot.insert(address.to_owned(), stable).is_none(),
            "duplicate client address",
        )?;
    }
    Ok(snapshot)
}

#[derive(Clone)]
pub struct Binding {
    pub pid: u32,
    pub address: Option<String>,
    pub index: usize,
}

#[derive(Clone)]
struct Context {
    before: BTreeMap<String, Value>,
    ids: [i64; 2],
    bindings: Vec<Binding>,
}

impl Context {
    fn validate(&self, monitors: &Value, clients: &Value, strict: bool) -> Result<(), String> {
        need(
            output_ids(monitors)? == self.ids,
            "output identities changed",
        )?;
        let current = client_snapshot(clients)?;
        for (address, old) in &self.before {
            need(
                current.get(address) == Some(old),
                "pre-existing client changed/disappeared",
            )?;
        }
        for c in array(clients)? {
            let address = c["address"].as_str().unwrap();
            if self.before.contains_key(address) {
                inactive_managed(c, self.ids)?;
                continue;
            }
            let b = self
                .bindings
                .iter()
                .find(|b| c["pid"] == b.pid)
                .ok_or_else(|| "unexpected client appeared during test".to_owned())?;
            let expected_ws = 91 + b.index as i64;
            if strict || b.address.is_some() {
                need(
                    c["address"].as_str() == b.address.as_deref(),
                    "wrong fixture address/PID binding",
                )?;
                need(
                    c["workspace"]["id"] == expected_ws && c["monitor"] == self.ids[b.index],
                    "wrong fixture workspace/output binding",
                )?;
                need(
                    c["xwayland"] == false
                        && c["mapped"] == true
                        && c["hidden"] == false
                        && c["pinned"] == false
                        && c["fullscreen"].as_i64().unwrap_or(0) != 0,
                    "fixture is not an exact visible native fullscreen client",
                )?;
                need(
                    c["at"] == json!([2560 * b.index, 0]) && c["size"] == json!([2560, 1440]),
                    "fixture bounds changed",
                )?;
            } else {
                need(
                    matches!(c["workspace"]["id"].as_i64(), Some(91 | 92)),
                    "pending fixture mapped outside disposable workspaces",
                )?;
            }
        }
        if strict {
            need(
                current.len() == self.before.len() + self.bindings.len(),
                "fixture missing or duplicated",
            )?;
        }
        Ok(())
    }

    fn observe(&self, strict: bool) -> Result<(), String> {
        let monitors = query("monitors")?;
        super::qualify_layout(&monitors);
        let clients = query("clients")?;
        let workspaces = query("workspaces")?;
        let after = query("monitors")?;
        need(
            output_ids(&monitors)? == output_ids(&after)?,
            "active/special workspace changed during observation",
        )?;
        super::qualify_layout(&after);
        self.validate(&after, &clients, strict)?;
        no_overlay_layers(&query("layers")?)?;
        if strict {
            workspace_counts(&workspaces, &self.bindings)?;
        }
        Ok(())
    }
}

thread_local! { static PHASE: RefCell<Option<Context>> = const { RefCell::new(None) }; }

pub fn phase() {
    PHASE.with(|slot| {
        if let Some(context) = slot.borrow().as_ref() {
            context
                .observe(context.bindings.iter().all(|b| b.address.is_some()))
                .expect("workspace isolation changed; no further action/capture admitted");
        }
    });
}

pub fn spawned(pid: u32, output: &str) {
    PHASE.with(|slot| {
        let mut slot = slot.borrow_mut();
        let context = slot.as_mut().expect("owned isolation guard");
        context.bindings.push(Binding {
            pid,
            address: None,
            index: usize::from(output == "DP-1"),
        });
    });
}

pub fn ready(pid: u32, address: &str) {
    PHASE.with(|slot| {
        slot.borrow_mut()
            .as_mut()
            .unwrap()
            .bindings
            .iter_mut()
            .find(|b| b.pid == pid)
            .unwrap()
            .address = Some(address.to_owned());
    });
    phase();
}

pub struct OwnedDesktop {
    driver: Option<McpDriver>,
    before: Context,
    pub release: Option<Value>,
}

impl OwnedDesktop {
    pub fn new() -> Self {
        let monitors = query("monitors").expect("prepared monitors");
        super::qualify_layout(&monitors);
        let ids = output_ids(&monitors)
            .expect("workspaces 91/92 must already be active, no special workspace shown");
        empty_workspaces(&query("workspaces").expect("prepared workspaces"))
            .expect("prepared workspaces must be empty");
        let clients = query("clients").expect("pre-existing clients");
        for client in array(&clients).unwrap() {
            inactive_managed(client, ids).expect("visible/pinned/unmanaged client refusal");
        }
        let before = Context {
            before: client_snapshot(&clients).unwrap(),
            ids,
            bindings: Vec::new(),
        };
        let layers = query("layers").expect("layer preflight");
        no_overlay_layers(&layers).expect("no notification/other overlay surfaces allowed");
        for name in ["DP-2", "DP-1"] {
            let counts: Vec<_> = (0..4)
                .map(|level| {
                    layers[name]["levels"][level.to_string()]
                        .as_array()
                        .map(Vec::len)
                })
                .collect();
            println!("[layers-preflight] output={name} level_counts={counts:?}");
        }
        before
            .observe(true)
            .expect("prepared workspace/layer safety preflight");
        let driver = McpDriver::spawn_named(super::SESSION).expect("candidate Driver");
        PHASE.with(|slot| {
            assert!(slot.borrow().is_none());
            *slot.borrow_mut() = Some(before.clone());
        });
        Self {
            driver: Some(driver),
            before,
            release: None,
        }
    }
}

impl Deref for OwnedDesktop {
    type Target = McpDriver;
    fn deref(&self) -> &McpDriver {
        self.driver.as_ref().unwrap()
    }
}
impl DerefMut for OwnedDesktop {
    fn deref_mut(&mut self) -> &mut McpDriver {
        self.driver.as_mut().unwrap()
    }
}

impl Drop for OwnedDesktop {
    fn drop(&mut self) {
        PHASE.with(|slot| *slot.borrow_mut() = None);
        if let (Some(driver), Some(args)) = (self.driver.as_mut(), self.release.take()) {
            let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                driver.call("mouse_button_up", args)
            }));
            match result {
                Ok(response) => eprintln!("[owned-release-cleanup] {}", response.raw),
                Err(_) => {
                    eprintln!("[owned-release-cleanup] transport panic; release outcome unknown")
                }
            }
        }
        let teardown =
            std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| drop(self.driver.take())));
        // Reap ONLY owned fixture processes, before preservation checks, even if
        // another Driver teardown operation itself panics.
        let verified = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let deadline = Instant::now() + Duration::from_secs(3);
            loop {
                let result = self.before.observe(true);
                if result.is_ok() || Instant::now() >= deadline {
                    return result;
                }
                std::thread::sleep(Duration::from_millis(50));
            }
        }));
        let restored = teardown.is_ok() && matches!(verified, Ok(Ok(())));
        if restored {
            eprintln!(
                "[workspace-cleanup] all original clients and active 91/92 workspaces preserved"
            );
        } else if std::thread::panicking() {
            eprintln!("[workspace-cleanup] FAILED preservation verification during unwind; parent must inspect/restore prepared workspaces");
        } else {
            panic!("workspace cleanup did not preserve original clients/active workspaces");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn monitors() -> Value {
        json!([
        {"name":"DP-2","id":0,"activeWorkspace":{"id":91},"specialWorkspace":{"id":0}},
        {"name":"DP-1","id":1,"activeWorkspace":{"id":92},"specialWorkspace":{"id":0}}])
    }
    fn client() -> Value {
        json!({"address":"0x123","pid":123,"workspace":{"id":1},"monitor":0,"mapped":true,"hidden":false,"visible":true,"pinned":false,"xwayland":false,"fullscreen":2,"at":[10,20],"size":[600,500]})
    }
    #[test]
    fn ordinary_mapped_inactive_hidden_false_is_allowed() {
        let c = client();
        inactive_managed(&c, [0, 1]).unwrap();
        let clients = json!([c]);
        let context = Context {
            before: client_snapshot(&clients).unwrap(),
            ids: [0, 1],
            bindings: vec![],
        };
        context.validate(&monitors(), &clients, true).unwrap();
    }
    #[test]
    fn inactive_managed_xwayland_and_hidden_special_are_allowed() {
        let mut c = client();
        c["xwayland"] = json!(true);
        c["pid"] = json!(0);
        inactive_managed(&c, [0, 1]).unwrap();
        client_snapshot(&json!([c.clone()])).unwrap();
        c["workspace"]["id"] = json!(-98);
        inactive_managed(&c, [0, 1]).unwrap();
    }
    #[test]
    fn visible_pinned_and_ambiguous_unmanaged_are_refused() {
        let mut c = client();
        c["pinned"] = json!(true);
        assert!(inactive_managed(&c, [0, 1]).is_err());
        let mut c = client();
        c["workspace"]["id"] = json!(91);
        assert!(inactive_managed(&c, [0, 1]).is_err());
        c["xwayland"] = json!(true);
        c["mapped"] = json!(false);
        c["hidden"] = json!(true);
        c["visible"] = json!(false);
        assert!(inactive_managed(&c,[0,1]).is_err(),"active-workspace membership refuses stubs/unmanaged regardless of mapped/hidden/visible");
        c["workspace"]["id"] = Value::Null;
        assert!(inactive_managed(&c, [0, 1]).is_err());
    }
    #[test]
    fn active_special_and_workspace_switch_are_refused() {
        let mut m = monitors();
        m[0]["specialWorkspace"]["id"] = json!(-1);
        assert!(output_ids(&m).is_err());
        m = monitors();
        m[1]["activeWorkspace"]["id"] = json!(1);
        assert!(output_ids(&m).is_err());
    }
    #[test]
    fn wrong_fixture_workspace_monitor_address_and_pid_are_refused() {
        let context = Context {
            before: BTreeMap::new(),
            ids: [0, 1],
            bindings: vec![Binding {
                pid: 123,
                address: Some("0x123".into()),
                index: 1,
            }],
        };
        let mut c = client();
        c["workspace"]["id"] = json!(92);
        c["monitor"] = json!(1);
        c["at"] = json!([2560, 0]);
        c["size"] = json!([2560, 1440]);
        context
            .validate(&monitors(), &json!([c.clone()]), true)
            .unwrap();
        for (key, value) in [
            ("monitor", json!(0)),
            ("address", json!("0x999")),
            ("pid", json!(999)),
            ("pinned", json!(true)),
        ] {
            let mut bad = c.clone();
            bad[key] = value;
            assert!(context.validate(&monitors(), &json!([bad]), true).is_err());
        }
        c["workspace"]["id"] = json!(91);
        assert!(context.validate(&monitors(), &json!([c]), true).is_err());
    }
    #[test]
    fn original_client_change_and_new_inactive_client_are_refused() {
        let clients = json!([client()]);
        let context = Context {
            before: client_snapshot(&clients).unwrap(),
            ids: [0, 1],
            bindings: vec![],
        };
        let mut changed = clients.clone();
        changed[0]["at"] = json!([20, 20]);
        assert!(context.validate(&monitors(), &changed, true).is_err());
        let mut new = client();
        new["address"] = json!("0x999");
        new["pid"] = json!(999);
        assert!(context
            .validate(&monitors(), &json!([client(), new]), true)
            .is_err());
    }
    #[test]
    fn overlay_layers_fail_closed() {
        let mut layers = json!({"DP-2":{"levels":{"3":[]}},"DP-1":{"levels":{"3":[]}}});
        no_overlay_layers(&layers).unwrap();
        layers["DP-1"]["levels"]["3"] = json!([{"namespace":"notification"}]);
        assert!(no_overlay_layers(&layers).is_err());
        assert!(no_overlay_layers(&json!({})).is_err());
    }
    #[test]
    fn workspace_count_and_monitor_cross_checks_are_strict() {
        let empty = json!([{"id":91,"monitor":"DP-2","windows":0,"hasfullscreen":false},{"id":92,"monitor":"DP-1","windows":0,"hasfullscreen":false}]);
        empty_workspaces(&empty).unwrap();
        let bindings = vec![
            Binding {
                pid: 1,
                address: Some("0x1".into()),
                index: 0,
            },
            Binding {
                pid: 2,
                address: Some("0x2".into()),
                index: 1,
            },
        ];
        assert!(workspace_counts(&empty, &bindings).is_err());
        let mut occupied = empty.clone();
        for i in 0..2 {
            occupied[i]["windows"] = json!(1);
            occupied[i]["hasfullscreen"] = json!(true);
        }
        workspace_counts(&occupied, &bindings).unwrap();
        occupied[1]["windows"] = json!(2);
        assert!(workspace_counts(&occupied, &bindings).is_err());
        occupied[1]["windows"] = json!(1);
        occupied[1]["monitor"] = json!("DP-2");
        assert!(workspace_counts(&occupied, &bindings).is_err());
    }
}
