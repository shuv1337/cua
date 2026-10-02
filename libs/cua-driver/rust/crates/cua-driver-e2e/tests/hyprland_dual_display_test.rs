//! Manual supporting acceptance for DP-2/DP-1 at 3840x2160@1.5.
//! No compositor reconfiguration, background admission bypass or input replay.
//! See the test harness guide before explicitly running the ignored row.
#![cfg(target_os = "linux")]

use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use cua_driver_testkit::e2e::OracleKind;
use cua_driver_testkit::{spawn_in_job, workspace_root, Driver, McpDriver};
use image::RgbImage;
use serde_json::{json, Value};

#[path = "support/dual_display_guard.rs"]
mod dual_display_guard;
use dual_display_guard::{phase, OwnedDesktop};

const SESSION: &str = "hyprland-dual-display";

fn ipc(topic: &str) -> Value {
    dual_display_guard::query(topic).expect("bounded independent Hyprland observation")
}

fn qualify_layout(monitors: &Value) {
    let monitors = monitors.as_array().expect("monitor array");
    assert_eq!(
        monitors.len(),
        2,
        "requires exactly the prepared home layout"
    );
    for (name, x) in [("DP-2", 0), ("DP-1", 2560)] {
        let m = monitors.iter().find(|m| m["name"] == name).unwrap();
        for (key, expected) in [
            ("width", 3840),
            ("height", 2160),
            ("x", x),
            ("y", 0),
            ("transform", 0),
        ] {
            assert_eq!(m[key], expected, "{name} {key}");
        }
        assert_eq!(m["scale"], 1.5, "{name} scale");
        assert_eq!(m["dpmsStatus"], true, "{name} must be powered");
        assert_eq!(m["disabled"], false, "{name} must be enabled");
        assert!(
            matches!(m.get("mirrorOf"), Some(Value::Null))
                || matches!(m["mirrorOf"].as_str(), Some("") | Some("none")),
            "{name} must not be mirrored"
        );
    }
}

fn events(path: &Path) -> Vec<Value> {
    assert!(
        std::fs::metadata(path).unwrap().len() <= 4 * 1024 * 1024,
        "fixture journal resource cap exceeded"
    );
    let mut text = String::new();
    std::fs::File::open(path)
        .unwrap()
        .take(4 * 1024 * 1024 + 1)
        .read_to_string(&mut text)
        .unwrap();
    assert!(
        text.len() <= 4 * 1024 * 1024,
        "bounded fixture journal reader overflow"
    );
    let events: Vec<Value> = text
        .split_inclusive('\n')
        .filter(|line| line.ends_with('\n'))
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    assert!(
        events.len() <= 20001 && !events.iter().any(|e| e["kind"] == "journal-resource-cap"),
        "fixture event resource cap reached"
    );
    events
}

struct Fixture {
    pid: u32,
    wid: u64,
    x: f64,
    journal: PathBuf,
}

fn launch(driver: &mut McpDriver, dir: &Path, output: &str, x: f64) -> Fixture {
    let journal = dir.join(format!("{output}.jsonl"));
    let script =
        workspace_root().join("crates/cua-driver-e2e/tests/support/dual_display_fixture.py");
    let child = spawn_in_job(
        Command::new("python3")
            .arg(script)
            .args(["--output", output, "--journal"])
            .arg(&journal)
            .env("GDK_BACKEND", "wayland")
            .stdout(Stdio::null())
            .stderr(Stdio::inherit()),
    )
    .expect("launch disposable native GTK color/event oracle");
    let pid = child.id();
    driver.reaper().push(child); // RAII cleanup also runs on assertion failure.
    dual_display_guard::spawned(pid, output);
    let deadline = Instant::now() + Duration::from_secs(15);
    loop {
        phase();
        let clients = ipc("clients");
        if let Some(c) = clients.as_array().unwrap().iter().find(|c| c["pid"] == pid) {
            if c["at"] == json!([x as i64, 0]) && c["size"] == json!([2560, 1440]) {
                assert_eq!(c["xwayland"], false);
                let wid = u64::from_str_radix(
                    c["address"].as_str().unwrap().trim_start_matches("0x"),
                    16,
                )
                .unwrap();
                if journal.exists()
                    && events(&journal)
                        .iter()
                        .any(|e| e["width"] == 2560 && e["height"] == 1440)
                {
                    dual_display_guard::ready(pid, c["address"].as_str().unwrap());
                    return Fixture {
                        pid,
                        wid,
                        x,
                        journal,
                    };
                }
            }
        }
        assert!(
            Instant::now() < deadline,
            "exact fullscreen native fixture did not become ready on {output}"
        );
        std::thread::sleep(Duration::from_millis(50));
    }
}

fn call(driver: &mut McpDriver, tool: &str, mut args: Value) -> Value {
    phase();
    if args.get("session").is_none() {
        args["session"] = json!(SESSION);
    }
    let started = Instant::now();
    let response = driver.call(tool, args);
    phase();
    println!(
        "[dual-display-timing] tool={tool} elapsed_ms={}",
        started.elapsed().as_millis()
    );
    assert!(!response.is_error(), "{tool}: {}", response.text());
    response.structured().clone()
}

fn capture(driver: &mut McpDriver, dir: &Path, cap: u32) -> (Value, RgbImage) {
    let path = dir.join(format!("desktop-{cap}.png"));
    let state = call(
        driver,
        "get_desktop_state",
        json!({
            "screenshot_out_file": path, "max_image_dimension": cap
        }),
    );
    let pixels = image::open(path).unwrap().to_rgb8();
    assert_eq!(
        pixels.dimensions(),
        if cap == 0 { (5120, 1440) } else { (2560, 720) }
    );
    assert_eq!(state["screenshot_width"], pixels.width());
    assert_eq!(state["screenshot_height"], pixels.height());
    assert_eq!(state["display"], "primary");
    assert_eq!(state["screen_width"], 5120);
    assert_eq!(state["screen_height"], 1440);
    assert_eq!(
        state["scale_factor"], 1.0,
        "composite capture normalization, not output DPI multiplier"
    );
    if cap != 0 {
        assert_eq!(state["screenshot_original_width"], 5120);
        assert_eq!(state["screenshot_original_height"], 1440);
    }
    (state, pixels)
}

fn output_pixels(dir: &Path, output: &str) -> RgbImage {
    phase();
    let path = dir.join(format!("{output}.png"));
    assert!(Command::new("timeout")
        .args(["--kill-after=1s", "10s", "grim", "-s", "1", "-o", output])
        .arg(&path)
        .status()
        .unwrap()
        .success());
    let raw = image::open(path).unwrap().to_rgb8();
    phase();
    assert_eq!(raw.dimensions(), (2560, 1440));
    raw
}

fn marker(pixels: &RgbImage, output_index: u32) -> (f64, f64) {
    let half = pixels.width() / 2;
    let mut bounds = (u32::MAX, u32::MAX, 0, 0);
    let mut count = 0;
    for y in 0..pixels.height() {
        for x in output_index * half..(output_index + 1) * half {
            if pixels.get_pixel(x, y).0.iter().all(|v| *v >= 245) {
                bounds.0 = bounds.0.min(x);
                bounds.1 = bounds.1.min(y);
                bounds.2 = bounds.2.max(x);
                bounds.3 = bounds.3.max(y);
                count += 1;
            }
        }
    }
    assert!(count > 100, "visible white fixture marker missing");
    assert!(
        count <= 4096 && bounds.2 - bounds.0 < 96 && bounds.3 - bounds.1 < 96,
        "white pixels exceed bounded marker region; possible overlay contamination"
    );
    (
        (bounds.0 + bounds.2 + 1) as f64 / 2.0,
        (bounds.1 + bounds.3 + 1) as f64 / 2.0,
    )
}

fn pixel_close(actual: &image::Rgb<u8>, expected: &image::Rgb<u8>) {
    assert!(
        actual
            .0
            .iter()
            .zip(expected.0)
            .all(|(a, b)| (i16::from(*a) - i16::from(b)).abs() <= 8),
        "independent capture pixels differ: {actual:?} vs {expected:?}"
    );
}

fn fixture_pixels_ready(pixels: &RgbImage, output: &str) -> bool {
    // IPC geometry goals alone do not prove map/fullscreen animations finished.
    // Sample the static drawing across the actual output before Driver capture.
    (32..2560).step_by(128).all(|x| {
        (32..1440).step_by(128).all(|y| {
            let expected = if x < 1280 && y < 720 {
                [51, 204, 77]
            } else if output == "DP-2" {
                [31, 82, 179]
            } else {
                [217, 51, 26]
            };
            pixels
                .get_pixel(x, y)
                .0
                .iter()
                .zip(expected)
                .all(|(actual, expected)| (i16::from(*actual) - expected).abs() <= 2)
        })
    })
}

fn assert_point(actual: &Value, x: f64, y: f64) {
    for (key, expected) in [("x", x), ("y", y)] {
        let observed = actual[key].as_f64().expect("independent coordinate");
        assert!(
            (observed - expected).abs() <= 1.0,
            "{key}: expected {expected}, observed {observed}"
        );
    }
}

fn pointer(driver: &mut McpDriver, x: f64, y: f64) {
    assert_point(&ipc("cursorpos"), x, y);
    let readback = call(driver, "get_cursor_position", json!({}));
    assert_eq!(
        readback["source"], "compositor",
        "commanded position is not live readback"
    );
    assert_point(&readback, x, y);
}

fn wait_event(f: &Fixture, from: usize, kind: &str, x: f64, y: f64) -> Value {
    let deadline = Instant::now() + Duration::from_secs(3);
    loop {
        let journal = events(&f.journal);
        let matching: Vec<_> = journal[from..]
            .iter()
            .filter(|e| e["kind"] == kind)
            .collect();
        if let Some(e) = matching.last() {
            assert_eq!(
                matching.len(),
                1,
                "one dispatch must produce exactly one {kind}; never replay"
            );
            assert_point(e, x, y);
            return (*e).clone();
        }
        assert!(Instant::now() < deadline, "missing external {kind} event");
        std::thread::sleep(Duration::from_millis(20));
    }
}

fn validate_scroll(group: &[Value], x: f64, y: f64, before: u64) -> Result<(), String> {
    let scrolls: Vec<_> = group.iter().filter(|e| e["kind"] == "scroll").collect();
    if scrolls.is_empty() || scrolls.len() > 8 {
        return Err("missing/oversized scroll packet group".into());
    }
    let mut down = 0;
    for e in scrolls {
        let px = e["x"].as_f64().ok_or("missing scroll x")?;
        let py = e["y"].as_f64().ok_or("missing scroll y")?;
        if !px.is_finite() || !py.is_finite() || (px - x).abs() > 1.0 || (py - y).abs() > 1.0 {
            return Err("scroll coordinates differ".into());
        }
        let dx = e["delta_x"].as_f64().ok_or("missing scroll delta_x")?;
        let dy = e["delta_y"].as_f64().ok_or("missing scroll delta_y")?;
        let stop = e["is_stop"].as_bool().ok_or("missing scroll stop flag")?;
        let ticks = e["scrolls"]
            .as_u64()
            .ok_or("missing discrete tick counter")?;
        if !dx.is_finite()
            || !dy.is_finite()
            || dx.abs() > 0.000001
            || ticks < before
            || ticks > before + 1
        {
            return Err("invalid deltas/tick counter".into());
        }
        match e["direction"].as_str() {
            Some("down") if !stop && ticks == before + 1 => down += 1,
            Some("smooth") if (stop && dy.abs() <= 0.000001) || (!stop && dy > 0.0) => (),
            _ => {
                return Err(
                    "scroll must be discrete down plus optional downward smooth/stop packets"
                        .into(),
                )
            }
        }
    }
    if down != 1 {
        return Err(
            "expected exactly one discrete-down effect, never a smooth-only pass or replay".into(),
        );
    }
    Ok(())
}

fn wait_scroll(f: &Fixture, from: usize, x: f64, y: f64, before: u64) {
    let deadline = Instant::now() + Duration::from_secs(3);
    loop {
        let journal = events(&f.journal);
        if journal[from..]
            .iter()
            .any(|e| e["kind"] == "scroll" && e["direction"] != "smooth")
        {
            std::thread::sleep(Duration::from_millis(150));
            validate_scroll(&events(&f.journal)[from..], x, y, before)
                .expect("directional discrete scroll/effect oracle");
            return;
        }
        assert!(
            Instant::now() < deadline,
            "no discrete-down scroll effect; smooth/stop alone is insufficient"
        );
        std::thread::sleep(Duration::from_millis(20));
    }
}

fn click(
    driver: &mut McpDriver,
    f: &Fixture,
    peer: &Fixture,
    x: f64,
    y: f64,
    encoded: Option<(f64, f64)>,
    capture_id: Option<&str>,
) {
    let before = events(&f.journal);
    let peer_before = events(&peer.journal).len();
    let clicks = before
        .iter()
        .rev()
        .find_map(|e| e["clicks"].as_u64())
        .unwrap();
    let (image_x, image_y) = encoded.unwrap_or((x, y));
    let mut args = json!({"scope": "desktop", "x": image_x, "y": image_y});
    if let Some(id) = capture_id {
        args["capture_id"] = json!(id);
    }
    call(driver, "click", args);
    wait_event(f, before.len(), "button-press", x - f.x, y);
    let released = wait_event(f, before.len(), "button-release", x - f.x, y);
    assert_eq!(released["clicks"], clicks + 1, "actual fixture effect");
    pointer(driver, x, y);
    std::thread::sleep(Duration::from_millis(150));
    let settled = events(&f.journal);
    for kind in ["button-press", "button-release"] {
        assert_eq!(
            settled[before.len()..]
                .iter()
                .filter(|e| e["kind"] == kind)
                .count(),
            1,
            "late duplicate {kind}"
        );
        assert!(
            !events(&peer.journal)[peer_before..]
                .iter()
                .any(|e| e["kind"] == kind),
            "input leaked to other output's fixture"
        );
    }
}

fn window_foreground_rows(driver: &mut McpDriver, target: &Fixture, peer: &Fixture) {
    let session = "dual-display-window-frame";
    call(
        driver,
        "start_session",
        json!({"session":session,"capture_scope":"window"}),
    );
    let base = json!({"session":session,"pid":target.pid,"window_id":target.wid,
                      "coordinate_frame":"desktop","delivery_mode":"foreground"});
    for (tool, count, button) in [
        ("click", 1, 1),
        ("double_click", 2, 1),
        ("right_click", 1, 3),
    ] {
        let before = events(&target.journal);
        let peer_before = events(&peer.journal).len();
        let clicks = before
            .iter()
            .rev()
            .find_map(|e| e["clicks"].as_u64())
            .unwrap();
        let mut args = base.clone();
        args["x"] = json!(3840);
        args["y"] = json!(800);
        call(driver, tool, args);
        let deadline = Instant::now() + Duration::from_secs(3);
        loop {
            let journal = events(&target.journal);
            let releases: Vec<_> = journal[before.len()..]
                .iter()
                .filter(|e| e["kind"] == "button-release")
                .collect();
            if releases.len() >= count {
                std::thread::sleep(Duration::from_millis(150));
                let settled = events(&target.journal);
                for kind in ["button-press", "button-release"] {
                    let observed: Vec<_> = settled[before.len()..]
                        .iter()
                        .filter(|e| e["kind"] == kind)
                        .collect();
                    assert_eq!(observed.len(), count, "{tool}: exact fixture event count");
                    for event in observed {
                        assert_point(event, 1280.0, 800.0);
                        assert_eq!(event["button"], button);
                    }
                }
                assert_eq!(releases.last().unwrap()["clicks"], clicks + count as u64);
                break;
            }
            assert!(
                Instant::now() < deadline,
                "{tool} never changed target fixture"
            );
            std::thread::sleep(Duration::from_millis(20));
        }
        assert!(!events(&peer.journal)[peer_before..]
            .iter()
            .any(|e| e["kind"] == "button-press" || e["kind"] == "button-release"));
        pointer(driver, 3840.0, 800.0);
    }
    // Native window-local foreground coordinates are already logical. This
    // contrasts with the desktop-frame rows and catches a second /1.5 factor.
    let before = events(&target.journal);
    let clicks = before
        .iter()
        .rev()
        .find_map(|e| e["clicks"].as_u64())
        .unwrap();
    let mut local = base.clone();
    local.as_object_mut().unwrap().remove("coordinate_frame");
    local["x"] = json!(100);
    local["y"] = json!(50);
    call(driver, "click", local);
    wait_event(target, before.len(), "button-press", 100.0, 50.0);
    assert_eq!(
        wait_event(target, before.len(), "button-release", 100.0, 50.0)["clicks"],
        clicks + 1
    );
    pointer(driver, 2660.0, 50.0);
    let before = events(&target.journal);
    let scrolls = before
        .iter()
        .rev()
        .find_map(|e| e["scrolls"].as_u64())
        .unwrap();
    let peer_before = events(&peer.journal).len();
    let mut args = base.clone();
    args["x"] = json!(3840);
    args["y"] = json!(1000);
    args["direction"] = json!("down");
    args["by"] = json!("line");
    args["amount"] = json!(1);
    call(driver, "scroll", args);
    wait_scroll(target, before.len(), 1280.0, 1000.0, scrolls);
    assert!(!events(&peer.journal)[peer_before..]
        .iter()
        .any(|e| e["kind"] == "scroll"));
    let before = events(&target.journal).len();
    let mut args = base.clone();
    args["from_x"] = json!(3760);
    args["from_y"] = json!(900);
    args["to_x"] = json!(3920);
    args["to_y"] = json!(900);
    call(driver, "drag", args);
    wait_event(target, before, "button-press", 1200.0, 900.0);
    wait_event(target, before, "button-release", 1360.0, 900.0);
    pointer(driver, 3920.0, 900.0);
}

fn held_button_row(driver: &mut OwnedDesktop, target: &Fixture) {
    let session = "dual-display-window-hold";
    call(
        driver,
        "start_session",
        json!({"session":session,"capture_scope":"window"}),
    );
    phase();
    let before = events(&target.journal).len();
    let down = json!({"session":session,"pid":target.pid,"window_id":target.wid,
                   "coordinate_frame":"desktop","x":2660,"y":1100});
    let up = down.clone();
    // Arm before dispatch because an error/transport loss may have an unknown
    // effect. Cleanup is ONLY an owned-target release, never input replay.
    driver.release = Some(owned_release_args(session, target.pid, target.wid));
    let down_response = driver.call("mouse_button_down", down);
    let up_response = driver.call("mouse_button_up", up);
    eprintln!(
        "[owned-hold] down={} up={}",
        down_response.raw, up_response.raw
    );
    // No panicking response wrapper, workspace check or oracle between down/up.
    assert!(
        !down_response.is_error(),
        "owned down refused/unknown; release attempted"
    );
    assert!(
        !up_response.is_error(),
        "owned release refused/unknown; cleanup guard remains armed"
    );
    phase();
    wait_event(target, before, "button-press", 100.0, 1100.0);
    wait_event(target, before, "button-release", 100.0, 1100.0);
    pointer(driver, 2660.0, 1100.0);
    driver.release = None;
}

fn qualified_hyprland_version(raw: &str) -> bool {
    let raw = raw.trim();
    let raw = raw.strip_prefix('v').unwrap_or(raw);
    let Some(suffix) = raw.strip_prefix("0.56.2") else {
        return false;
    };
    if suffix.is_empty() {
        return true;
    }
    // Exact major/minor/patch only, with well-formed semver package/commit
    // suffixes. Never mistake 0.56.20, a fourth component or a new ABI for 0.56.2.
    fn identifiers(value: &str, prerelease: bool) -> bool {
        !value.is_empty()
            && value.split('.').all(|part| {
                !part.is_empty()
                    && part.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-')
                    && !(prerelease
                        && part.len() > 1
                        && part.starts_with('0')
                        && part.bytes().all(|b| b.is_ascii_digit()))
            })
    }
    if let Some(build) = suffix.strip_prefix('+') {
        return identifiers(build, false);
    }
    if let Some(rest) = suffix.strip_prefix('-') {
        let (pre, build) = rest
            .split_once('+')
            .map_or((rest, None), |(pre, build)| (pre, Some(build)));
        return identifiers(pre, true) && build.is_none_or(|build| identifiers(build, false));
    }
    false
}

fn owned_release_args(session: &str, pid: u32, window_id: u64) -> Value {
    assert!(
        pid > 0 && window_id > 0,
        "cleanup must identify an owned fixture"
    );
    // Frozen-base descriptor permits coordinate-less up; its coordinate helper
    // returns the stored hold point and the Wayland release emits no motion.
    json!({"session":session,"pid":pid,"window_id":window_id})
}

fn native_preflight() {
    for name in [
        "CUA_TEST_DUAL_DISPLAY_PREPARED",
        "CUA_DRIVER_RS_ENABLE_WAYLAND",
        "CUA_E2E_UNRESTRICTED_GUI",
    ] {
        assert_eq!(
            std::env::var(name).as_deref(),
            Ok("1"),
            "explicit prepared-desktop opt-in: {name}"
        );
    }
    assert!(std::env::var_os("HYPRLAND_INSTANCE_SIGNATURE").is_some());
    assert!(std::env::var_os("WAYLAND_DISPLAY").is_some());
    assert!(std::env::var_os("DISPLAY").is_none(), "no X11 fallback");
    assert!(Path::new(
        &std::env::var("CUA_TEST_DRIVER_BIN").expect("explicit exact-candidate binary")
    )
    .is_file());
    assert_eq!(
        std::env::var("CUA_E2E_SOURCE_SHA")
            .expect("exact candidate SHA")
            .len(),
        40
    );
    qualify_layout(&ipc("monitors"));
    let version = ipc("version");
    assert!(qualified_hyprland_version(version["version"].as_str().unwrap_or("")),"workspace-based unmanaged authority requires qualified Hyprland 0.56.2 semantics; version suffixes do not authorize a new ABI");
    println!(
        "[dual-display-version] version={} tag={} commit={}",
        version["version"], version["tag"], version["commit"]
    );
    println!(
        "[dual-display] hyprland_version={} source_sha={} oracles={:?}",
        version,
        std::env::var("CUA_E2E_SOURCE_SHA").unwrap(),
        [
            OracleKind::FixtureState,
            OracleKind::Pixels,
            OracleKind::Cursor,
            OracleKind::Focus,
            OracleKind::NoLeakedInput
        ]
    );
}

fn setup() -> (OwnedDesktop, tempfile::TempDir, Fixture, Fixture) {
    native_preflight();
    let dir = tempfile::Builder::new()
        .prefix("cua-dual-display-")
        .tempdir()
        .unwrap();
    let mut driver = OwnedDesktop::new();
    call(
        &mut driver,
        "start_session",
        json!({"capture_scope": "desktop"}),
    );
    let left = launch(&mut driver, dir.path(), "DP-2", 0.0);
    let right = launch(&mut driver, dir.path(), "DP-1", 2560.0);
    phase();
    for output in ["DP-2", "DP-1"] {
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            if fixture_pixels_ready(&output_pixels(dir.path(), output), output) {
                break;
            }
            assert!(
                Instant::now() < deadline,
                "actual fullscreen fixture pixels never settled on {output}"
            );
            std::thread::sleep(Duration::from_millis(50));
        }
    }
    (driver, dir, left, right)
}

#[test]
#[ignore = "manual supporting evidence: native dual-output 1.5x, prepared disposable workspaces 91/92, GTK3, grim, candidate Driver"]
fn native_dual_output_capture_and_desktop_input() {
    let (mut driver, dir, left, right) = setup();
    let size = call(&mut driver, "get_screen_size", json!({}));
    assert_eq!(size["width"], 5120);
    assert_eq!(size["height"], 1440);
    assert_eq!(size["scale_factor"], 1.5);
    for (name, x) in [("DP-2", 0), ("DP-1", 2560)] {
        let m = size["monitors"]
            .as_array()
            .expect("per-output metadata")
            .iter()
            .find(|m| m["name"] == name)
            .unwrap();
        assert_eq!(m["frame_x"], x);
        assert_eq!(m["frame_y"], 0);
        assert_eq!(m["width"], 2560);
        assert_eq!(m["height"], 1440);
        assert_eq!(m["scale"], 1.5);
        assert_eq!(m["powered"], true);
    }
    let (desktop_state, full) = capture(&mut driver, dir.path(), 0);
    // Independent logical per-output captures: filtering is not required to be
    // byte-identical to Driver resampling; compare interior colors and markers.
    for (output, offset) in [("DP-2", 0), ("DP-1", 2560)] {
        let logical = output_pixels(dir.path(), output);
        for (x, y) in [(32, 32), (1800, 1000), (2500, 1400), (100, 1000)] {
            pixel_close(full.get_pixel(offset + x, y), logical.get_pixel(x, y));
        }
        // The independent output image has one marker. Embed it beside a black
        // half solely to reuse the two-output marker scanner.
        let mut independent = RgbImage::new(5120, 1440);
        image::imageops::replace(&mut independent, &logical, i64::from(offset), 0);
        let observed = marker(&full, offset / 2560);
        let expected = marker(&independent, offset / 2560);
        assert!((observed.0 - expected.0).abs() <= 1.0 && (observed.1 - expected.1).abs() <= 1.0);
    }
    // Driver window offsets must agree with exact full-address compositor identity.
    let windows = call(&mut driver, "list_windows", json!({}));
    for f in [&left, &right] {
        let w = windows["windows"]
            .as_array()
            .unwrap()
            .iter()
            .find(|w| w["pid"] == f.pid && w["window_id"] == f.wid)
            .unwrap();
        assert_eq!(w["bounds"]["x"], f.x);
        assert_eq!(w["bounds"]["y"], 0);
        assert_eq!(w["bounds"]["width"], 2560);
        assert_eq!(w["bounds"]["height"], 1440);
        let desktop_window = desktop_state["windows"]
            .as_array()
            .unwrap()
            .iter()
            .find(|w| w["pid"] == f.pid && w["window_id"] == f.wid)
            .unwrap();
        assert_eq!(desktop_window["bounds"]["x"], f.x);
    }
    for f in [&left, &right] {
        for (lx, y) in [
            (0.0, 0.0),
            (2559.0, 0.0),
            (0.0, 1439.0),
            (2559.0, 1439.0),
            (1280.0, 720.0),
        ] {
            let peer = if f.pid == left.pid { &right } else { &left };
            click(&mut driver, f, peer, f.x + lx, y, None, None);
        }
    }
    click(&mut driver, &left, &right, 2559.0, 720.0, None, None);
    click(&mut driver, &right, &left, 2560.0, 720.0, None, None);
    let (_, capped) = capture(&mut driver, dir.path(), 2560);
    pixel_close(capped.get_pixel(1920, 500), full.get_pixel(3840, 1000));
    let left_marker = marker(&capped, 0);
    let right_marker = marker(&capped, 1);
    assert_point(&json!({"x":left_marker.0,"y":left_marker.1}), 640.0, 360.0);
    assert_point(
        &json!({"x":right_marker.0,"y":right_marker.1}),
        1920.0,
        360.0,
    );
    click(
        &mut driver,
        &left,
        &right,
        1280.0,
        720.0,
        Some(left_marker),
        None,
    );
    click(
        &mut driver,
        &right,
        &left,
        3840.0,
        720.0,
        Some(right_marker),
        None,
    );
    // Capture-bound mapping owns the transform: no second preview compensation.
    let (state, pixels) = capture(&mut driver, dir.path(), 2560);
    let id = state["capture_id"]
        .as_str()
        .expect("immutable desktop capture ID");
    click(
        &mut driver,
        &right,
        &left,
        3840.0,
        720.0,
        Some(marker(&pixels, 1)),
        Some(id),
    );
    let before = events(&right.journal).len();
    let left_before = events(&left.journal).len();
    let focus_before = ipc("activewindow")["address"].clone();
    phase();
    let refused = driver.call(
        "click",
        json!({"session": SESSION, "scope": "desktop", "capture_id": id, "x": 1920, "y": 360}),
    );
    phase();
    assert!(refused.is_error());
    assert_eq!(refused.structured()["code"], "capture_not_found");
    assert_eq!(refused.structured()["effect"], "refused");
    std::thread::sleep(Duration::from_millis(150));
    assert!(
        !events(&right.journal)[before..]
            .iter()
            .any(|e| e["kind"] == "button-press" || e["kind"] == "button-release"),
        "refused capture must not replay input"
    );
    assert!(
        !events(&left.journal)[left_before..]
            .iter()
            .any(|e| e["kind"] == "button-press" || e["kind"] == "button-release"),
        "refused capture leaked into other output"
    );
    assert_eq!(
        ipc("activewindow")["address"],
        focus_before,
        "refusal changed focus"
    );
    pointer(&mut driver, 3840.0, 720.0);
    // Restore uncapped session mapping before independent scroll/drag checks.
    capture(&mut driver, dir.path(), 0);
    for f in [&left, &right] {
        let journal = events(&f.journal);
        let before = journal.len();
        let ticks = journal
            .iter()
            .rev()
            .find_map(|e| e["scrolls"].as_u64())
            .unwrap();
        let peer = if f.pid == left.pid { &right } else { &left };
        let peer_before = events(&peer.journal).len();
        call(
            &mut driver,
            "scroll",
            json!({"scope":"desktop", "x": f.x + 1280.0, "y":720, "direction":"down", "by":"line", "amount":1}),
        );
        wait_scroll(f, before, 1280.0, 720.0, ticks);
        assert!(
            !events(&peer.journal)[peer_before..]
                .iter()
                .any(|e| e["kind"] == "scroll"),
            "desktop scroll leaked to peer"
        );
        pointer(&mut driver, f.x + 1280.0, 720.0);
    }
    let before = events(&left.journal).len();
    let peer_before = events(&right.journal).len();
    call(
        &mut driver,
        "drag",
        json!({"scope":"desktop", "from_x":2500, "from_y":720, "to_x":2620, "to_y":720}),
    );
    wait_event(&left, before, "button-press", 2500.0, 720.0);
    // GTK's implicit primary-seat grab keeps the release on the original surface.
    wait_event(&left, before, "button-release", 2620.0, 720.0);
    std::thread::sleep(Duration::from_millis(150));
    assert!(
        !events(&right.journal)[peer_before..]
            .iter()
            .any(|e| e["kind"] == "button-press" || e["kind"] == "button-release"),
        "desktop drag leaked button events to peer"
    );
    pointer(&mut driver, 2620.0, 720.0);
    let before = events(&right.journal).len();
    call(
        &mut driver,
        "move_cursor",
        json!({"scope":"desktop","x":4000,"y":100}),
    );
    let deadline = Instant::now() + Duration::from_secs(3);
    loop {
        let journal = events(&right.journal);
        if let Some(motion) = journal[before..]
            .iter()
            .rev()
            .find(|e| e["kind"] == "motion-notify")
        {
            if (motion["x"].as_f64().unwrap() - 1440.0).abs() <= 1.0
                && (motion["y"].as_f64().unwrap() - 100.0).abs() <= 1.0
            {
                break;
            }
        }
        assert!(Instant::now() < deadline, "hover did not reach fixture");
        std::thread::sleep(Duration::from_millis(20));
    }
    pointer(&mut driver, 4000.0, 100.0);
    qualify_layout(&ipc("monitors"));
    phase();
    held_button_row(&mut driver, &right); // Last input row; owns unwind release.
                                          // No retained desktop images: private temp files are removed on success/panic;
                                          // McpDriver's reaper owns only the two processes spawned above.
}

#[test]
#[ignore = "manual supporting evidence: prepared dual-output workspaces 91/92 and explicit candidate foreground-plugin opt-in"]
fn native_dual_output_window_foreground_plugin_input() {
    assert_eq!(
        std::env::var("CUA_TEST_DUAL_DISPLAY_FOREGROUND_PLUGIN").as_deref(),
        Ok("1"),
        "foreground plugin qualification requires its explicit gate; never skip as pass"
    );
    let (mut driver, _dir, left, right) = setup();
    window_foreground_rows(&mut driver, &right, &left);
    phase();
}

#[test]
fn home_layout_preflight_rejects_same_size_swapped_outputs() {
    let layout = json!([
        {"name":"DP-2","width":3840,"height":2160,"scale":1.5,"x":0,"y":0,"transform":0,"dpmsStatus":true,"disabled":false,"mirrorOf":"none"},
        {"name":"DP-1","width":3840,"height":2160,"scale":1.5,"x":2560,"y":0,"transform":0,"dpmsStatus":true,"disabled":false,"mirrorOf":"none"}
    ]);
    qualify_layout(&layout);
    let mut swapped = layout;
    swapped[0]["x"] = json!(2560);
    swapped[1]["x"] = json!(0);
    assert!(std::panic::catch_unwind(|| qualify_layout(&swapped)).is_err());
}

#[test]
fn qualified_version_accepts_suffix_formats_but_never_another_abi() {
    for version in [
        "0.56.2",
        "v0.56.2",
        "0.56.2-3",
        "v0.56.2-dirty",
        "0.56.2-123-gabcdef",
        "0.56.2+arch.3",
        "v0.56.2-3+gabcdef",
    ] {
        assert!(
            qualified_hyprland_version(version),
            "qualified format: {version}"
        );
    }
    for version in [
        "0.56.20",
        "v0.56.3",
        "0.57.2",
        "1.56.2",
        "0.056.2",
        "vv0.56.2",
        "0.56.2.1",
        "0.56.2-",
        "0.56.2+",
        "0.56.2-01",
        "0.56.2garbage",
        "0.56.2-dirty/modified",
        "0.56.2+build+more",
        "",
    ] {
        assert!(
            !qualified_hyprland_version(version),
            "unqualified/ambiguous format: {version}"
        );
    }
}

#[test]
fn owned_cleanup_release_has_no_coordinates_or_new_motion_fields() {
    let args = owned_release_args("owned", 42, 0x123);
    assert_eq!(args, json!({"session":"owned","pid":42,"window_id":0x123}));
    for key in [
        "x",
        "y",
        "coordinate_frame",
        "scope",
        "from_zoom",
        "capture_id",
    ] {
        assert!(args.get(key).is_none());
    }
}

#[test]
fn scroll_oracle_requires_one_discrete_down_and_meaningful_counter() {
    let down = json!({"kind":"scroll","x":1280,"y":720,"direction":"down","delta_x":0,"delta_y":0,"is_stop":false,"scrolls":4});
    let smooth = json!({"kind":"scroll","x":1280,"y":720,"direction":"smooth","delta_x":0,"delta_y":1,"is_stop":false,"scrolls":4});
    let stop = json!({"kind":"scroll","x":1280,"y":720,"direction":"smooth","delta_x":0,"delta_y":0,"is_stop":true,"scrolls":4});
    validate_scroll(&[down.clone()], 1280.0, 720.0, 3).unwrap();
    validate_scroll(
        &[down.clone(), smooth.clone(), stop.clone()],
        1280.0,
        720.0,
        3,
    )
    .unwrap();
    assert!(validate_scroll(&[smooth.clone(), stop], 1280.0, 720.0, 3).is_err());
    assert!(validate_scroll(&[down.clone(), down.clone()], 1280.0, 720.0, 3).is_err());
    for (key, value) in [
        ("direction", json!("up")),
        ("direction", json!("left")),
        ("scrolls", json!(3)),
        ("x", json!(1200)),
        ("delta_x", json!(1)),
        ("is_stop", json!(true)),
    ] {
        let mut bad = down.clone();
        bad[key] = value;
        assert!(validate_scroll(&[bad], 1280.0, 720.0, 3).is_err());
    }
    let mut reversed = smooth;
    reversed["delta_y"] = json!(-1);
    assert!(validate_scroll(&[down, reversed], 1280.0, 720.0, 3).is_err());
}

#[test]
fn marker_oracle_refuses_distant_white_overlay_contamination() {
    let mut pixels = RgbImage::new(512, 144);
    for y in 60..84 {
        for x in 116..140 {
            pixels.put_pixel(x, y, image::Rgb([255, 255, 255]));
        }
    }
    assert_eq!(marker(&pixels, 0), (128.0, 72.0));
    pixels.put_pixel(240, 20, image::Rgb([255, 255, 255]));
    assert!(std::panic::catch_unwind(|| marker(&pixels, 0)).is_err());
}

#[test]
fn journal_resource_cap_is_a_failure_not_missing_or_truncated_evidence() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("journal.jsonl");
    std::fs::write(
        &path,
        "{\"kind\":\"state\"}\n{\"kind\":\"journal-resource-cap\"}\n",
    )
    .unwrap();
    assert!(std::panic::catch_unwind(|| events(&path)).is_err());
    std::fs::write(&path, vec![b' '; 4 * 1024 * 1024 + 1]).unwrap();
    assert!(std::panic::catch_unwind(|| events(&path)).is_err());
}

#[test]
fn static_pixel_oracle_distinguishes_outputs_and_unsettled_surfaces() {
    for (output, color) in [("DP-2", [31, 82, 179]), ("DP-1", [217, 51, 26])] {
        let mut pixels = RgbImage::from_pixel(2560, 1440, image::Rgb(color));
        for y in 0..720 {
            for x in 0..1280 {
                pixels.put_pixel(x, y, image::Rgb([51, 204, 77]));
            }
        }
        assert!(fixture_pixels_ready(&pixels, output));
        assert!(!fixture_pixels_ready(
            &pixels,
            if output == "DP-2" { "DP-1" } else { "DP-2" }
        ));
        pixels.put_pixel(32, 32, image::Rgb([0, 0, 0]));
        assert!(!fixture_pixels_ready(&pixels, output));
    }
}

#[test]
fn capped_marker_coordinates_come_from_visible_pixels() {
    let mut pixels = RgbImage::new(512, 144);
    for (cx, cy) in [(128, 72), (384, 72)] {
        for y in cy - 16..cy + 16 {
            for x in cx - 16..cx + 16 {
                pixels.put_pixel(x, y, image::Rgb([255, 255, 255]));
            }
        }
    }
    assert_eq!(marker(&pixels, 0), (128.0, 72.0));
    assert_eq!(marker(&pixels, 1), (384.0, 72.0));
    let capped = image::imageops::resize(&pixels, 256, 72, image::imageops::FilterType::Lanczos3);
    assert_eq!(marker(&capped, 0), (64.0, 36.0));
    assert_eq!(marker(&capped, 1), (192.0, 36.0));
    let mut moved = RgbImage::new(256, 72);
    for y in 24..48 {
        for x in 200..224 {
            moved.put_pixel(x, y, image::Rgb([255, 255, 255]));
        }
    }
    assert_eq!(
        marker(&moved, 1),
        (212.0, 36.0),
        "scanner must not substitute expected geometry"
    );
}
