//! Process-level ownership of recording argv validation. No GUI or daemon.
use std::process::Command;

#[cfg(unix)]
#[test]
fn recording_start_forwards_the_directory_and_video_opt_in_to_the_selected_daemon() {
    use cua_driver_core::daemon::{DaemonRequest, DaemonResponse};
    use std::io::{BufRead, BufReader, Write};
    use std::os::unix::net::UnixListener;
    use std::time::{Duration, Instant};

    let home = tempfile::tempdir().unwrap();
    let socket = home.path().join("daemon.sock");
    let directory = home.path().join("render");
    let listener = UnixListener::bind(&socket).unwrap();
    listener.set_nonblocking(true).unwrap();
    let expected_dir = directory.clone();
    let server = std::thread::spawn(move || {
        let deadline = Instant::now() + Duration::from_secs(10);
        for (method, name) in [
            ("list", None),
            ("metadata", None),
            ("session_end", None),
            ("call", Some("start_recording")),
            ("call", Some("get_recording_state")),
        ] {
            let mut stream = loop {
                match listener.accept() {
                    Ok((stream, _)) => break stream,
                    Err(error)
                        if error.kind() == std::io::ErrorKind::WouldBlock
                            && Instant::now() < deadline =>
                    {
                        std::thread::sleep(Duration::from_millis(5));
                    }
                    Err(error) => panic!("expected {method}: {error}"),
                }
            };
            stream
                .set_read_timeout(Some(Duration::from_secs(2)))
                .unwrap();
            let mut line = String::new();
            BufReader::new(stream.try_clone().unwrap())
                .read_line(&mut line)
                .unwrap();
            let request: DaemonRequest = serde_json::from_str(&line).unwrap();
            assert_eq!(request.method, method);
            assert_eq!(request.name.as_deref(), name);
            let result = match method {
                "metadata" => {
                    serde_json::to_value(cua_driver_core::daemon::current_daemon_metadata())
                        .unwrap()
                }
                "call" if request.name.as_deref() == Some("start_recording") => {
                    assert_eq!(
                        request.args.unwrap(),
                        serde_json::json!({
                            "output_dir": expected_dir, "record_video": true,
                        })
                    );
                    serde_json::json!({})
                }
                "call" => {
                    assert_eq!(request.name.as_deref(), Some("get_recording_state"));
                    serde_json::json!({"structuredContent":{"next_turn":1}})
                }
                _ => serde_json::json!({}),
            };
            writeln!(
                stream,
                "{}",
                serde_json::to_string(&DaemonResponse::ok(result)).unwrap()
            )
            .unwrap();
        }
    });
    let result = Command::new(env!("CARGO_BIN_EXE_cua-driver"))
        .args([
            "recording",
            "start",
            "--video",
            directory.to_str().unwrap(),
            "--socket",
            socket.to_str().unwrap(),
        ])
        .env("CUA_DRIVER_RS_HOME", home.path())
        .env("CUA_DRIVER_RS_TELEMETRY_ENABLED", "0")
        .output()
        .unwrap();
    server.join().unwrap();
    assert!(result.status.success(), "{result:?}");
    assert!(directory.is_dir());
    assert!(String::from_utf8(result.stdout)
        .unwrap()
        .contains("Recording started"));
}

#[test]
fn invalid_recording_arguments_fail_before_daemon_or_filesystem_access() {
    let home = tempfile::tempdir().unwrap();
    let directory = home.path().join("must-not-be-created");
    let path = directory.to_str().unwrap();
    for (args, message) in [
        (
            vec!["recording", "start", "--bogus", path],
            "Unknown recording start option '--bogus'",
        ),
        (
            vec!["recording", "render", "--scale", "1.5", "in"],
            "Usage: cua-driver recording render <input-dir> <out.mp4> [--no-zoom] [--scale N]",
        ),
        (
            vec!["recording", "render", "in", "out", "--scale"],
            "--scale requires a finite positive number",
        ),
        (
            vec!["recording", "render", "in", "out", "--socket"],
            "--socket requires a path",
        ),
        (
            vec!["recording", "stop", "unexpected"],
            "recording stop accepts no arguments",
        ),
    ] {
        let result = Command::new(env!("CARGO_BIN_EXE_cua-driver"))
            .args(&args)
            .env("CUA_DRIVER_RS_HOME", home.path())
            .env("CUA_DRIVER_RS_TELEMETRY_ENABLED", "0")
            .output()
            .unwrap();
        assert_eq!(result.status.code(), Some(64), "{args:?}: {:?}", result);
        assert_eq!(String::from_utf8(result.stderr).unwrap().trim(), message);
        assert!(!directory.exists());
    }
}

#[test]
fn recording_never_falls_back_to_a_local_runtime_without_its_daemon() {
    let home = tempfile::tempdir().unwrap();
    let socket = home.path().join("missing.sock");
    let directory = home.path().join("must-not-be-created");
    let result = Command::new(env!("CARGO_BIN_EXE_cua-driver"))
        .args([
            "--socket",
            socket.to_str().unwrap(),
            "recording",
            "start",
            "--video",
            directory.to_str().unwrap(),
        ])
        .env("CUA_DRIVER_RS_HOME", home.path())
        .env("CUA_DRIVER_RS_TELEMETRY_ENABLED", "0")
        .output()
        .unwrap();
    assert_eq!(result.status.code(), Some(1));
    assert_eq!(
        String::from_utf8(result.stderr).unwrap().trim(),
        "Cua Driver daemon is not running.\nStart it first with: cua-driver serve"
    );
    assert!(!directory.exists());
}
