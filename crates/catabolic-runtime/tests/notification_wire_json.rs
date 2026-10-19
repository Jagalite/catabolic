// SPDX-FileCopyrightText: 2026 The Catabolic Contributors
// SPDX-License-Identifier: MIT
//! Exact ordinary notification HTTP bodies against an isolated frozen Python
//! worker. Only owned numeric-loopback receivers and synthetic envelopes are used.
use catabolic_runtime::notifications::HttpTransport;
use catabolic_store::notifications::Outcome;
use serde_json::{Value, json};
use std::{
    collections::BTreeMap,
    io::{Read, Write},
    net::TcpListener,
    process::{Command, Stdio},
    thread,
    time::{Duration, Instant},
};

#[derive(Debug, PartialEq, Eq)]
struct Request {
    line: String,
    headers: BTreeMap<String, String>,
    body: Vec<u8>,
}

fn paired(envelope: Value, mapped: bool) -> Vec<u8> {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    listener.set_nonblocking(true).unwrap();
    let url = format!("http://{}/notification", listener.local_addr().unwrap());
    let receiver = thread::spawn(move || {
        let deadline = Instant::now() + Duration::from_secs(30);
        let mut requests = Vec::new();
        while requests.len() < 2 {
            let (mut socket, _) = match listener.accept() {
                Ok(value) => value,
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                    assert!(
                        Instant::now() < deadline,
                        "notification never reached receiver"
                    );
                    thread::sleep(Duration::from_millis(5));
                    continue;
                }
                Err(error) => panic!("{error}"),
            };
            // macOS accepted sockets inherit the listener's nonblocking flag;
            // Linux sockets do not. The bounded read below needs blocking IO.
            socket.set_nonblocking(false).unwrap();
            socket
                .set_read_timeout(Some(Duration::from_secs(5)))
                .unwrap();
            let mut raw = Vec::new();
            let mut byte = [0];
            while !raw.ends_with(b"\r\n\r\n") {
                socket.read_exact(&mut byte).unwrap();
                raw.push(byte[0]);
                assert!(raw.len() < 16384);
            }
            let head = std::str::from_utf8(&raw).unwrap();
            let mut lines = head.split("\r\n");
            let line = lines.next().unwrap().to_owned();
            let headers: BTreeMap<_, _> = lines
                .filter_map(|line| line.split_once(':'))
                .map(|(key, value)| (key.to_ascii_lowercase(), value.trim().to_owned()))
                .collect();
            let mut body = vec![0; headers["content-length"].parse().unwrap()];
            socket.read_exact(&mut body).unwrap();
            socket
                .write_all(
                    b"HTTP/1.1 204 No Content\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
                )
                .unwrap();
            requests.push(Request {
                line,
                headers: [
                    "content-type",
                    "idempotency-key",
                    "user-agent",
                    "content-length",
                ]
                .into_iter()
                .map(|key| (key.to_owned(), headers[key].clone()))
                .collect(),
                body,
            });
        }
        requests
    });
    let request = json!({
        "url": url, "method": "POST", "has_body": true, "body": envelope,
        "headers": {}, "auth": null, "responses": {"204": {"has_body": false}}
    });
    let python = std::env::var_os("CATABOLIC_PYTHON_ORACLE")
        .expect("set CATABOLIC_PYTHON_ORACLE to the qualification interpreter");
    let reference = std::env::var_os("CATABOLIC_PYTHON_REFERENCE")
        .expect("set CATABOLIC_PYTHON_REFERENCE to pinned 453fca9 src/");
    let mut child = Command::new(python)
        .args(["-c", include_str!("reference/notification_wire_oracle.py")])
        .arg(&reference)
        .env("PYTHONPATH", &reference)
        .env_remove("PYTHONHOME")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(
            catabolic_core::encode(&json!({"mapped": mapped, "request": request})).as_bytes(),
        )
        .unwrap();
    let output = child.wait_with_output().unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(output.stdout, b"complete\n");
    let transport = HttpTransport::default();
    let outcome = if mapped {
        transport.send_request(&request, "wire-fixture")
    } else {
        transport.send_url(&url, &envelope, "job_completed", "info")
    };
    assert_eq!(outcome, Outcome::Complete);
    let requests = receiver.join().unwrap();
    assert_eq!(
        requests[0], requests[1],
        "frozen Python / native wire bytes"
    );
    requests.into_iter().next().unwrap().body
}

#[test]
fn ordinary_unicode_keys_values_controls_nesting_and_order_match_frozen_worker() {
    let controls: String = (0..=0x1f).map(|c| char::from_u32(c).unwrap()).collect();
    // Deliberately non-canonical input ordering. The frozen send() boundary
    // sorts every object before its worker applies default json.dumps escaping.
    let mut envelope: Value = serde_json::from_str(
        r#"{"z":0,"id":"wire-fixture","a":{"z":2,"a":1},"array":[true,null,false,{},[]]}"#,
    )
    .unwrap();
    envelope["BMP-é-中-\u{e000}"] = json!("é e\u{301} \u{7f}\u{80}\u{2028}\u{2029}\u{ffff}");
    envelope["supplementary-😀-\u{10000}-\u{10ffff}"] = json!(["😀", "\u{10000}", "\u{10ffff}"]);
    envelope["nested"] = json!({"😀": {"é": "\u{1f}", "中": []}, "\u{e000}": 2, "\u{10000}": 1});
    envelope["\u{e000}"] = json!("BMP key sorts before supplementary key");
    envelope["\u{10000}"] = json!("supplementary key");
    envelope["controls\n\t\"\\"] = json!({"all": controls, "quoted": "\"\\/\u{8}\u{c}\n\r\t"});
    envelope["numbers"] =
        serde_json::from_str("[-0.0,1e-7,1e20,1.25,1208925819614629174706176]").unwrap();
    let body = paired(envelope.clone(), false);
    assert!(body.is_ascii());
    let text = std::str::from_utf8(&body).unwrap();
    for spelling in [
        "\\u00e9",
        "\\u007f",
        "\\ud83d\\ude00",
        "\\udbff\\udfff",
        "\\u0000",
        "\\n",
        "\\\"",
    ] {
        assert!(text.contains(spelling), "missing {spelling}");
    }
    // Arbitrary-precision Value equality also compares float token spelling;
    // decoding the Python spelling 1e-07 must not be compared lexically to 1e-7.
    assert_eq!(
        catabolic_core::encode(&catabolic_core::decode_json_bytes(&body).unwrap()),
        catabolic_core::encode(&envelope)
    );
}

#[test]
fn ordinary_ascii_envelope_retains_separators_order_and_no_newline() {
    let body = paired(
        json!({"z": [2, 1], "id": "wire-fixture", "a": {"z": null, "a": "quoted\"\\/"}}),
        false,
    );
    assert_eq!(
        body,
        br#"{"a": {"a": "quoted\"\\/", "z": null}, "id": "wire-fixture", "z": [2, 1]}"#
    );
}

#[test]
fn ordinary_escape_expansion_does_not_change_existing_input_budget() {
    let body = paired(
        json!({"id": "wire-fixture", "value": "é".repeat(3000)}),
        false,
    );
    assert!(body.is_ascii());
    assert!(body.len() > 16384);
}

#[test]
fn mapped_request_body_keeps_its_distinct_utf8_encoding() {
    let body = paired(
        json!({"id": "wire-fixture", "é": ["😀", "中", {"z": 2, "a": 1}]}),
        true,
    );
    assert!(!body.is_ascii());
    assert!(std::str::from_utf8(&body).unwrap().contains("😀"));
}
