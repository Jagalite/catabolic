// SPDX-FileCopyrightText: 2026 The Catabolic Contributors
// SPDX-License-Identifier: MIT
//! Private, bounded callable transport. All parsing and rendering stay native.
use serde_json::{Value, json};
use std::{
    io::{self, Read, Write},
    sync::{
        Mutex,
        atomic::{AtomicBool, Ordering},
    },
};

pub const ARGUMENT: &str = "__catabolic-python-cli-v1";
const MAX_FRAME: usize = 1024 * 1024;
static CALLABLE: AtomicBool = AtomicBool::new(false);
// A single ordered channel also serializes writes from native worker callbacks.
static WIRE: Mutex<()> = Mutex::new(());
static CALLBACK_ERROR: Mutex<Option<Value>> = Mutex::new(None);

fn invalid() -> io::Error {
    io::Error::new(
        io::ErrorKind::InvalidData,
        "invalid Python CLI transport frame",
    )
}
fn receive() -> io::Result<Value> {
    let mut stdin = io::stdin().lock();
    let mut size = [0; 4];
    stdin.read_exact(&mut size)?;
    let size = u32::from_be_bytes(size) as usize;
    if size == 0 || size > MAX_FRAME {
        return Err(invalid());
    }
    let mut bytes = vec![0; size];
    stdin.read_exact(&mut bytes)?;
    serde_json::from_slice(&bytes).map_err(|_| invalid())
}
fn send(value: &Value) -> io::Result<()> {
    let bytes = serde_json::to_vec(value).map_err(|_| invalid())?;
    if bytes.len() > MAX_FRAME {
        return Err(invalid());
    }
    let mut stdout = io::stdout().lock();
    stdout.write_all(&(bytes.len() as u32).to_be_bytes())?;
    stdout.write_all(&bytes)?;
    stdout.flush()
}
fn acknowledgement() -> io::Result<()> {
    let response = receive()?;
    if response == json!({"kind":"ack"}) {
        return Ok(());
    }
    if response["kind"] == "callback-error"
        && response["error"]["type"].is_string()
        && response["error"]["message"].is_string()
    {
        *CALLBACK_ERROR.lock().map_err(|_| invalid())? = Some(response["error"].clone());
    }
    Err(invalid())
}
pub fn start() -> io::Result<Value> {
    CALLABLE.store(true, Ordering::Release);
    let request = receive()?;
    if request["version"] != 1 || !matches!(request["kind"].as_str(), Some("argv" | "render-error"))
    {
        return Err(invalid());
    }
    Ok(request)
}
pub fn dispatching(args: &Value) -> io::Result<()> {
    let output = super::presentation::error(
        args,
        catabolic_store::Error("native legacy CLI interrupted".into()),
    );
    send(
        &json!({"kind":"dispatch", "context":args, "interrupted":{"code":output.code,"stdout":output.stdout,"stderr":output.stderr}}),
    )?;
    acknowledgement()
}
pub fn finished(code: i32, parse_exit: bool) -> io::Result<()> {
    send(&json!({"kind":if parse_exit {"system-exit"} else {"return"}, "code":code}))
}
pub fn reporting_error() -> io::Result<()> {
    if active() {
        send(&json!({"kind":"handling-error"}))?;
        acknowledgement()?;
    }
    Ok(())
}
pub fn write(stream: &str, text: &str, flush: bool) -> io::Result<()> {
    write_inner(stream, text, flush, false)
}
pub fn progress(
    stream: &str,
    text: &str,
    flush: bool,
) -> std::result::Result<(), catabolic_runtime::maintenance::ProgressError> {
    if !active() {
        // Historical standalone maintenance deliberately ignores progress IO
        // failures. Only Python callable callbacks have a typed owner contract.
        let _ = write_inner(stream, text, flush, false);
        return Ok(());
    }
    write_inner(stream, text, flush, true).map_err(|error| {
        let supplied = CALLBACK_ERROR
            .lock()
            .ok()
            .and_then(|mut value| value.take());
        catabolic_runtime::maintenance::ProgressError {
            message: supplied
                .as_ref()
                .and_then(|value| value["message"].as_str())
                .map(str::to_owned)
                .unwrap_or_else(|| error.to_string()),
            kind: supplied
                .as_ref()
                .and_then(|value| value["type"].as_str())
                .unwrap_or("OSError")
                .to_owned(),
        }
    })
}
fn write_inner(stream: &str, text: &str, flush: bool, owner_catches: bool) -> io::Result<()> {
    if !CALLABLE.load(Ordering::Acquire) {
        if stream == "stdout" {
            let mut output = io::stdout().lock();
            output.write_all(text.as_bytes())?;
            if flush {
                output.flush()?;
            }
        } else {
            let mut output = io::stderr().lock();
            output.write_all(text.as_bytes())?;
            if flush {
                output.flush()?;
            }
        }
        return Ok(());
    }
    let _guard = WIRE.lock().map_err(|_| invalid())?;
    let mut remaining = text;
    while !remaining.is_empty() {
        let mut end = remaining.len().min(16384);
        while !remaining.is_char_boundary(end) {
            end -= 1;
        }
        send(
            &json!({"kind":"write","stream":stream,"text":&remaining[..end],"owner_catches":owner_catches}),
        )?;
        acknowledgement()?;
        remaining = &remaining[end..];
    }
    if flush {
        send(&json!({"kind":"flush","stream":stream,"owner_catches":owner_catches}))?;
        acknowledgement()?;
    }
    Ok(())
}
pub fn active() -> bool {
    CALLABLE.load(Ordering::Acquire)
}
pub fn read_stdin(maximum: usize) -> io::Result<String> {
    let _guard = WIRE.lock().map_err(|_| invalid())?;
    send(&json!({"kind":"read","size":maximum + 1}))?;
    let mut text = String::new();
    loop {
        let frame = receive()?;
        match frame["kind"].as_str() {
            Some("stdin") => {
                let chunk = frame["text"].as_str().ok_or_else(invalid)?;
                // Drain all frames, but do not accumulate an oversized payload.
                if text.len() <= maximum {
                    text.push_str(chunk);
                }
            }
            Some("stdin-end") => return Ok(text),
            _ => return Err(invalid()),
        }
    }
}
