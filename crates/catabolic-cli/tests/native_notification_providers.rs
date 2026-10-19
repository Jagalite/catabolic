// SPDX-FileCopyrightText: 2026 The Catabolic Contributors
// SPDX-License-Identifier: MIT
//! Owned loopback acceptance for the native notification worker supervision.
use catabolic_runtime::{
    notifications::{HttpTransport, WORKER_ARGUMENT},
    process,
};
use catabolic_store::notifications::Outcome;
use serde_json::{Value, json};
use std::{
    io::{self, Read, Write},
    net::{TcpListener, TcpStream},
    path::Path,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant},
};
fn native() -> &'static Path {
    static BINARY: std::sync::OnceLock<std::path::PathBuf> = std::sync::OnceLock::new();
    BINARY.get_or_init(|| {
        let binary = std::env::var_os("CATABOLIC_NOTIFICATION_TEST_NATIVE_BIN")
            .map(std::path::PathBuf::from)
            .unwrap_or_else(|| env!("CARGO_BIN_EXE_catabolic-native").into());
        warm(binary)
    })
}
fn legacy() -> &'static Path {
    static BINARY: std::sync::OnceLock<std::path::PathBuf> = std::sync::OnceLock::new();
    BINARY.get_or_init(|| {
        let binary = std::env::var_os("CATABOLIC_NOTIFICATION_TEST_LEGACY_BIN")
            .map(std::path::PathBuf::from)
            .unwrap_or_else(|| env!("CARGO_BIN_EXE_catabolic-legacy-native").into());
        warm(binary)
    })
}
/// macOS can spend about a second on the first launch of a newly linked
/// executable. Pay that once, untimed, so the worker deadlines below measure
/// supervision rather than host first-launch policy checks.
fn warm(binary: std::path::PathBuf) -> std::path::PathBuf {
    // `{}` is refused as an invalid destination before any network activity.
    let warmed = process::command_output(
        &[binary.as_os_str(), std::ffi::OsStr::new(WORKER_ARGUMENT)],
        process::CommandOptions {
            input_bytes: Some(b"{}"),
            maximum: 4096,
            timeout: Duration::from_secs(30),
            ..Default::default()
        },
    );
    assert!(
        !matches!(&warmed, Err(e) if e.state == "timeout"),
        "notification worker warm-up exceeded its bound"
    );
    binary
}
fn register() {
    catabolic_runtime::helper::register(native(), None).unwrap();
}
fn payload(url: &str) -> String {
    catabolic_core::encode(
        &json!({"url":url,"event":"job_completed","severity":"info","envelope":{"id":"fixture","subject":"must-never-be-forwarded"}}),
    )
}
fn accept_with_timeout(listener: &TcpListener, timeout: Duration) -> io::Result<TcpStream> {
    listener.set_nonblocking(true)?;
    let started = Instant::now();
    loop {
        match listener.accept() {
            Ok((stream, _)) => {
                stream.set_nonblocking(false)?;
                return Ok(stream);
            }
            Err(error)
                if matches!(
                    error.kind(),
                    io::ErrorKind::WouldBlock | io::ErrorKind::Interrupted
                ) => {}
            Err(error) => return Err(error),
        }
        let remaining = timeout.saturating_sub(started.elapsed());
        if remaining.is_zero() {
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "native notification worker did not connect to the owned loopback fixture",
            ));
        }
        std::thread::sleep(remaining.min(Duration::from_millis(10)));
    }
}
#[track_caller]
fn accept_worker(listener: &TcpListener) -> TcpStream {
    accept_with_timeout(listener, Duration::from_secs(3))
        .expect("required notification worker connection was not observed")
}
fn request(stream: &mut TcpStream) -> Vec<u8> {
    stream
        .set_read_timeout(Some(Duration::from_secs(3)))
        .unwrap();
    let mut data = vec![];
    let mut one = [0];
    while !data.ends_with(b"\r\n\r\n") {
        stream.read_exact(&mut one).unwrap();
        data.push(one[0]);
        assert!(data.len() < 16384);
    }
    let text = String::from_utf8(data.clone()).unwrap();
    let count = text
        .lines()
        .find_map(|line| {
            line.split_once(':')
                .filter(|(k, _)| k.eq_ignore_ascii_case("content-length"))
                .map(|(_, v)| v.trim().parse::<usize>().unwrap())
        })
        .unwrap_or(0);
    let mut body = vec![0; count];
    stream.read_exact(&mut body).unwrap();
    data.extend(body);
    data
}
#[test]
fn loopback_fixture_times_out_without_a_worker_connection() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let (sender, receiver) = std::sync::mpsc::channel();
    let server = std::thread::spawn(move || {
        sender
            .send(accept_with_timeout(&listener, Duration::from_millis(50)).map(|_| ()))
            .unwrap();
    });
    let error = receiver
        .recv_timeout(Duration::from_secs(3))
        .expect("loopback fixture did not finish its accept deadline")
        .unwrap_err();
    assert_eq!(error.kind(), io::ErrorKind::TimedOut);
    assert!(error.to_string().contains("did not connect"));
    server.join().unwrap();
}
#[test]
fn both_native_executables_dispatch_bounded_provider_protocol() {
    for binary in [native(), legacy()] {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!(
            "ntfy://{}/synthetic?image=no",
            listener.local_addr().unwrap()
        );
        let server = std::thread::spawn(move || {
            let mut stream = accept_worker(&listener);
            let bytes = request(&mut stream);
            stream
                .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\nConnection: close\r\n\r\n")
                .unwrap();
            bytes
        });
        let input = payload(&url);
        let result = process::command_output(
            &[binary.as_os_str(), std::ffi::OsStr::new(WORKER_ARGUMENT)],
            process::CommandOptions {
                input_bytes: Some(input.as_bytes()),
                maximum: 4096,
                timeout: Duration::from_secs(3),
                ..Default::default()
            },
        )
        .unwrap();
        assert_eq!(result.returncode, 0);
        assert!(result.stderr.is_empty());
        assert_eq!(
            serde_json::from_slice::<Value>(&result.stdout).unwrap(),
            json!({"status":"complete"})
        );
        let wire = String::from_utf8(server.join().unwrap()).unwrap();
        assert!(wire.contains("A Catabolic processing job completed."));
        assert!(!wire.contains("must-never-be-forwarded"));
    }
}
#[test]
fn cancellation_kills_stalled_http_and_prevents_later_fanout() {
    register();
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!(
        "ntfy://{}/a/z?retry=10&wait=20&rto=30",
        listener.local_addr().unwrap()
    );
    let cancel = Arc::new(AtomicBool::new(false));
    let trigger = cancel.clone();
    let server = std::thread::spawn(move || {
        let mut stream = accept_worker(&listener);
        request(&mut stream);
        trigger.store(true, Ordering::Release);
        let mut byte = [0];
        assert_eq!(
            stream.read(&mut byte).unwrap(),
            0,
            "child must close the owned socket when cancelled"
        );
        listener.set_nonblocking(true).unwrap();
        std::thread::sleep(Duration::from_millis(100));
        assert!(
            listener.accept().is_err(),
            "cancelled child cannot send another topic"
        );
    });
    let start = Instant::now();
    let outcome = process::with_cancellation(cancel, || {
        HttpTransport::default().send_url(&url, &json!({"id":"fixture"}), "job_completed", "info")
    });
    assert_eq!(outcome, Outcome::TemporarilyFailed);
    assert!(start.elapsed() < Duration::from_secs(2));
    server.join().unwrap();
}
#[test]
fn cancellation_kills_stalled_smtp_banner() {
    register();
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!(
        "mailto://sender@example.invalid?smtp=127.0.0.1&port={}&to=recipient@example.invalid",
        listener.local_addr().unwrap().port()
    );
    let cancel = Arc::new(AtomicBool::new(false));
    let trigger = cancel.clone();
    let server = std::thread::spawn(move || {
        let mut stream = accept_worker(&listener);
        stream
            .set_read_timeout(Some(Duration::from_secs(3)))
            .unwrap();
        trigger.store(true, Ordering::Release);
        let mut byte = [0];
        assert_eq!(stream.read(&mut byte).unwrap(), 0);
    });
    let start = Instant::now();
    let outcome = process::with_cancellation(cancel, || {
        HttpTransport::default().send_url(&url, &json!({"id":"fixture"}), "job_completed", "info")
    });
    assert_eq!(outcome, Outcome::TemporarilyFailed);
    assert!(start.elapsed() < Duration::from_secs(2));
    server.join().unwrap();
}
#[test]
fn outer_deadline_reaps_native_http_worker_and_all_its_resources() {
    // Resolve (and warm) the worker before any accept or wall-clock deadline.
    let binary = native();
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!(
        "gotify://{}/synthetic?rto=30",
        listener.local_addr().unwrap()
    );
    let server = std::thread::spawn(move || {
        let mut stream = accept_worker(&listener);
        request(&mut stream);
        let mut byte = [0];
        assert_eq!(stream.read(&mut byte).unwrap(), 0);
    });
    let input = payload(&url);
    let start = Instant::now();
    let failure = process::command_output(
        &[binary.as_os_str(), std::ffi::OsStr::new(WORKER_ARGUMENT)],
        process::CommandOptions {
            input_bytes: Some(input.as_bytes()),
            maximum: 4096,
            // Leave room for pre-exec descriptor cleanup before the worker connects.
            // The owned request/EOF checks and two-second command wall bound remain.
            timeout: Duration::from_secs(1),
            ..Default::default()
        },
    )
    .unwrap_err();
    assert_eq!(failure.state, "timeout");
    assert!(failure.stdout.is_empty());
    assert!(failure.stderr.is_empty());
    assert!(start.elapsed() < Duration::from_secs(2));
    server.join().unwrap();
}
#[test]
fn invalid_input_and_budget_never_disclose_credentials() {
    register();
    let transport = HttpTransport::default();
    assert_eq!(
        transport.send_url(
            "ntfy://127.0.0.1/topic?token=secret%0D%0Ainjected",
            &json!({"id":"fixture"}),
            "job_completed",
            "info"
        ),
        Outcome::InvalidDestination
    );
    assert_eq!(
        transport.send_url(
            "ntfy://127.0.0.1/topic",
            &json!({"private":"x".repeat(16384)}),
            "job_completed",
            "info"
        ),
        Outcome::InvalidDestination
    );
    for input in [
        "not-json".to_string(),
        "x".repeat(16385),
        payload("file:///tmp/private"),
    ] {
        let result = process::command_output(
            &[native().as_os_str(), std::ffi::OsStr::new(WORKER_ARGUMENT)],
            process::CommandOptions {
                input_bytes: Some(input.as_bytes()),
                maximum: 4096,
                timeout: Duration::from_secs(2),
                ..Default::default()
            },
        )
        .unwrap();
        assert_eq!(result.returncode, 0);
        assert!(result.stderr.is_empty());
        assert!(result.stdout.len() < 80);
        let output = String::from_utf8(result.stdout).unwrap();
        assert!(!output.contains("private"));
        assert!(!output.contains("secret"));
    }
}

#[test]
fn standalone_and_companion_drains_keep_durable_partial_receipts_without_python() {
    use catabolic_store::{Store, migration, notifications as policy};
    for (binary, structured) in [(native(), true), (legacy(), false)] {
        let root = tempfile::tempdir().unwrap();
        let db = root.path().join("catalog.db");
        migration::initialize(&db).unwrap();
        {
            let store = Store::open(&db, true, false).unwrap();
            for id in ["good", "bad"] {
                policy::configure(&store,"default",&json!({"id":id,"credential_env":format!("SYNTHETIC_{}",id.to_uppercase()),"subscriptions":["job_completed"],"apply":true})).unwrap();
            }
            store
                .transaction(|db| {
                    policy::emit(
                        db,
                        "default",
                        "job_completed",
                        "info",
                        "private-must-not-leak",
                        &json!("fixture-identity"),
                    )
                })
                .unwrap();
        }
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let server = std::thread::spawn(move || {
            let mut result = vec![];
            let mut bad = false;
            for _ in 0..3 {
                let mut stream = accept_worker(&listener);
                let bytes = request(&mut stream);
                let text = String::from_utf8(bytes).unwrap();
                let code = if text.starts_with("POST /bad/") && !bad {
                    bad = true;
                    503
                } else {
                    200
                };
                write!(
                    stream,
                    "HTTP/1.1 {code} Fixture\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
                )
                .unwrap();
                result.push(text);
            }
            result
        });
        let run = || {
            let mut command = std::process::Command::new(binary);
            command
                .current_dir(root.path())
                .env_clear()
                .env("PATH", root.path())
                .env(
                    "SYNTHETIC_GOOD",
                    format!("gotify://{address}/good/synthetic"),
                )
                .env("SYNTHETIC_BAD", format!("gotify://{address}/bad/synthetic"));
            command.arg("--db").arg(&db).args(["--json", "notify"]);
            if structured {
                command.args(["drain", "--options", r#"{"limit":10}"#]);
            } else {
                command.args(["run", "--limit", "10"]);
            }
            let result = command.output().unwrap();
            assert!(
                result.status.success() || result.status.code() == Some(3),
                "stdout={} stderr={}",
                String::from_utf8_lossy(&result.stdout),
                String::from_utf8_lossy(&result.stderr)
            );
            assert!(result.stderr.is_empty());
            let text = String::from_utf8(result.stdout).unwrap();
            assert!(!text.contains("synthetic"));
            assert!(!text.contains("private-must-not-leak"));
            let value = serde_json::from_str::<Value>(&text).unwrap();
            assert_eq!(
                result.status.code(),
                Some(if structured || value["complete"] == true {
                    0
                } else {
                    3
                })
            );
            value
        };
        let first = run();
        assert_eq!(first["complete"], false);
        let mut states = first["deliveries"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v["state"].as_str().unwrap())
            .collect::<Vec<_>>();
        states.sort();
        assert_eq!(states, ["complete", "retry"]);
        {
            let store = Store::open(&db, true, false).unwrap();
            policy::control(&store, "default", "bad", "retry", policy::now()).unwrap();
        }
        let second = run();
        assert_eq!(second["complete"], true);
        assert_eq!(second["deliveries"].as_array().unwrap().len(), 1);
        let third = run();
        assert_eq!(third["deliveries"], json!([]));
        let wire = server.join().unwrap();
        assert_eq!(wire.len(), 3);
        assert_eq!(
            wire.iter().filter(|s| s.starts_with("POST /good/")).count(),
            1
        );
        assert!(wire.iter().all(|s| !s.contains("private-must-not-leak")));
        let store = Store::open(&db, false, false).unwrap();
        let status = policy::status(&store, "default", 100).unwrap();
        assert!(!status.to_string().contains("synthetic"));
        assert!(
            status["deliveries"]
                .as_array()
                .unwrap()
                .iter()
                .all(|d| d["state"] == "complete")
        );
    }
}

#[test]
fn configured_companion_failures_never_fall_back_to_the_standalone_worker() {
    use catabolic_store::{Store, migration, notifications as policy};
    for selected in ["missing", "wrong-protocol"] {
        let root = tempfile::tempdir().unwrap();
        let db = root.path().join("catalog.db");
        migration::initialize(&db).unwrap();
        {
            let store = Store::open(&db, true, false).unwrap();
            policy::configure(&store,"default",&json!({"id":"fixture","credential_env":"SYNTHETIC_DESTINATION","subscriptions":["job_completed"],"apply":true})).unwrap();
            store
                .transaction(|db| {
                    policy::emit(
                        db,
                        "default",
                        "job_completed",
                        "info",
                        "private",
                        &json!("fixture"),
                    )
                })
                .unwrap();
        }
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let path = if selected == "missing" {
            root.path().join("missing-helper")
        } else {
            Path::new("/usr/bin/false").to_path_buf()
        };
        let output = std::process::Command::new(legacy())
            .current_dir(root.path())
            .env_clear()
            .env("PATH", root.path())
            .env("CATABOLIC_NATIVE_BINARY", path)
            .env(
                "SYNTHETIC_DESTINATION",
                format!("gotify://{}/synthetic", listener.local_addr().unwrap()),
            )
            .arg("--db")
            .arg(&db)
            .args(["--json", "notify", "run", "--limit", "1"])
            .output()
            .unwrap();
        assert_eq!(output.status.code(), Some(3));
        assert!(output.stderr.is_empty());
        let report: Value = serde_json::from_slice(&output.stdout).unwrap();
        assert_eq!(
            report["deliveries"][0]["state"],
            if selected == "missing" {
                "repair"
            } else {
                "retry"
            }
        );
        assert!(
            listener.accept().is_err(),
            "a configured-helper failure cannot trigger a second destination send"
        );
    }
}

#[test]
fn replaced_standalone_fallback_is_rejected_before_a_second_delivery() {
    use catabolic_store::{Store, migration, notifications as policy};
    let root = tempfile::tempdir().unwrap();
    let db = root.path().join("catalog.db");
    migration::initialize(&db).unwrap();
    let executable = root.path().join("catabolic");
    std::fs::copy(legacy(), &executable).unwrap();
    {
        let store = Store::open(&db, true, false).unwrap();
        for id in ["first", "second"] {
            policy::configure(&store,"default",&json!({"id":id,"credential_env":"SYNTHETIC_DESTINATION","subscriptions":["job_completed"],"apply":true})).unwrap();
        }
        store
            .transaction(|db| {
                policy::emit(
                    db,
                    "default",
                    "job_completed",
                    "info",
                    "private",
                    &json!("fixture"),
                )
            })
            .unwrap();
    }
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    let target = executable.clone();
    let server = std::thread::spawn(move || {
        let mut stream = accept_worker(&listener);
        request(&mut stream);
        let replacement = target.with_extension("replaced");
        std::fs::copy("/usr/bin/false", &replacement).unwrap();
        std::fs::rename(replacement, &target).unwrap();
        stream
            .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\nConnection: close\r\n\r\n")
            .unwrap();
        listener.set_nonblocking(true).unwrap();
        std::thread::sleep(Duration::from_millis(200));
        assert!(listener.accept().is_err());
    });
    let output = std::process::Command::new(&executable)
        .current_dir(root.path())
        .env_clear()
        .env("PATH", root.path())
        .env(
            "SYNTHETIC_DESTINATION",
            format!("gotify://{address}/synthetic"),
        )
        .arg("--db")
        .arg(&db)
        .args(["--json", "notify", "run", "--limit", "2"])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(3));
    assert!(output.stderr.is_empty());
    let report: Value = serde_json::from_slice(&output.stdout).unwrap();
    let mut states = report["deliveries"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v["state"].as_str().unwrap())
        .collect::<Vec<_>>();
    states.sort();
    assert_eq!(states, ["complete", "repair"]);
    server.join().unwrap();
}
