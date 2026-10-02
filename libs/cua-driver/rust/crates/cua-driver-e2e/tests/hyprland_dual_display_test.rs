//! Manual supporting acceptance for DP-2/DP-1 at 3840x2160@1.5.
//! No compositor reconfiguration, background admission bypass or input replay.
//! See the test harness guide before explicitly running the ignored row.
#![cfg(target_os = "linux")]

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use cua_driver_testkit::{spawn_in_job, workspace_root, Driver, McpDriver};
use image::{imageops::FilterType, RgbImage};
use serde_json::{json, Value};

const SESSION: &str = "hyprland-dual-display";

fn ipc(topic: &str) -> Value {
    let output = Command::new("timeout")
        .args(["10s", "hyprctl", "-j", topic])
        .output()
        .unwrap();
    assert!(output.status.success(), "hyprctl {topic} failed");
    serde_json::from_slice(&output.stdout).unwrap()
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
    }
}

fn events(path: &Path) -> Vec<Value> {
    std::fs::read_to_string(path)
        .unwrap()
        .split_inclusive('\n')
        .filter(|line| line.ends_with('\n'))
        .map(|line| serde_json::from_str(line).unwrap())
        .collect()
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
    let deadline = Instant::now() + Duration::from_secs(15);
    loop {
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
    args["session"] = json!(SESSION);
    let response = driver.call(tool, args);
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
    (state, pixels)
}

fn output_pixels(dir: &Path, output: &str) -> RgbImage {
    let path = dir.join(format!("{output}.png"));
    assert!(Command::new("timeout")
        .args(["10s", "grim", "-o", output])
        .arg(&path)
        .status()
        .unwrap()
        .success());
    let raw = image::open(path).unwrap().to_rgb8();
    assert_eq!(raw.dimensions(), (3840, 2160));
    image::imageops::resize(&raw, 2560, 1440, FilterType::Triangle)
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

fn click(
    driver: &mut McpDriver,
    f: &Fixture,
    peer: &Fixture,
    x: f64,
    y: f64,
    encoded: bool,
    capture_id: Option<&str>,
) {
    let before = events(&f.journal);
    let peer_before = events(&peer.journal).len();
    let clicks = before
        .iter()
        .rev()
        .find_map(|e| e["clicks"].as_u64())
        .unwrap();
    let mut args = json!({"scope": "desktop", "x": if encoded { x / 2.0 } else { x },
                          "y": if encoded { y / 2.0 } else { y }});
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

#[test]
#[ignore = "manual supporting evidence: explicitly prepared empty native Hyprland dual-output 1.5x desktop, GTK3, grim, candidate Driver"]
fn native_dual_output_capture_and_desktop_input() {
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
    assert!(
        ipc("clients").as_array().unwrap().is_empty(),
        "refusing a desktop containing user windows; prepare an isolated empty desktop first"
    );
    let dir = tempfile::Builder::new()
        .prefix("cua-dual-display-")
        .tempdir()
        .unwrap();
    let mut driver = McpDriver::spawn_named(SESSION).expect("candidate Driver");
    call(
        &mut driver,
        "start_session",
        json!({"capture_scope": "desktop"}),
    );
    let left = launch(&mut driver, dir.path(), "DP-2", 0.0);
    let right = launch(&mut driver, dir.path(), "DP-1", 2560.0);
    // Only fixtures may be visible before any full-desktop image is captured.
    let clients = ipc("clients");
    assert_eq!(clients.as_array().unwrap().len(), 2);
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
    let (_, full) = capture(&mut driver, dir.path(), 0);
    // Independent per-output backing captures; compare interior pixels after
    // measured-size normalization, not a global 1.5 multiplier.
    for (output, offset) in [("DP-2", 0), ("DP-1", 2560)] {
        let logical = output_pixels(dir.path(), output);
        for (x, y) in [(32, 32), (1280, 720), (2500, 1400), (100, 1000)] {
            assert_eq!(
                full.get_pixel(offset + x, y),
                logical.get_pixel(x, y),
                "{output} composed pixels at {x},{y}"
            );
        }
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
            click(&mut driver, f, peer, f.x + lx, y, false, None);
        }
    }
    click(&mut driver, &left, &right, 2559.0, 720.0, false, None);
    click(&mut driver, &right, &left, 2560.0, 720.0, false, None);
    let (_, capped) = capture(&mut driver, dir.path(), 2560);
    assert_eq!(capped.get_pixel(1920, 500), full.get_pixel(3840, 1000));
    click(&mut driver, &left, &right, 1280.0, 720.0, true, None);
    click(&mut driver, &right, &left, 3840.0, 720.0, true, None);
    // Capture-bound mapping owns the transform: no second preview compensation.
    let (state, _) = capture(&mut driver, dir.path(), 2560);
    let id = state["capture_id"]
        .as_str()
        .expect("immutable desktop capture ID");
    click(&mut driver, &right, &left, 3840.0, 720.0, true, Some(id));
    let before = events(&right.journal).len();
    let left_before = events(&left.journal).len();
    let focus_before = ipc("activewindow")["address"].clone();
    let refused = driver.call(
        "click",
        json!({"session": SESSION, "scope": "desktop", "capture_id": id, "x": 1920, "y": 360}),
    );
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
        let before = events(&f.journal).len();
        call(
            &mut driver,
            "scroll",
            json!({"scope":"desktop", "x": f.x + 1280.0, "y":720, "direction":"down", "by":"line", "amount":1}),
        );
        wait_event(f, before, "scroll", 1280.0, 720.0);
        pointer(&mut driver, f.x + 1280.0, 720.0);
    }
    let before = events(&left.journal).len();
    call(
        &mut driver,
        "drag",
        json!({"scope":"desktop", "from_x":2500, "from_y":720, "to_x":2620, "to_y":720}),
    );
    wait_event(&left, before, "button-press", 2500.0, 720.0);
    // GTK's implicit primary-seat grab keeps the release on the original surface.
    wait_event(&left, before, "button-release", 2620.0, 720.0);
    pointer(&mut driver, 2620.0, 720.0);
    qualify_layout(&ipc("monitors"));
    // No retained desktop images: private temp files are removed on success/panic;
    // McpDriver's reaper owns only the two processes spawned above.
}

#[test]
fn home_layout_preflight_rejects_same_size_swapped_outputs() {
    let layout = json!([
        {"name":"DP-2","width":3840,"height":2160,"scale":1.5,"x":0,"y":0,"transform":0,"dpmsStatus":true,"disabled":false},
        {"name":"DP-1","width":3840,"height":2160,"scale":1.5,"x":2560,"y":0,"transform":0,"dpmsStatus":true,"disabled":false}
    ]);
    qualify_layout(&layout);
    let mut swapped = layout;
    swapped[0]["x"] = json!(2560);
    swapped[1]["x"] = json!(0);
    assert!(std::panic::catch_unwind(|| qualify_layout(&swapped)).is_err());
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
