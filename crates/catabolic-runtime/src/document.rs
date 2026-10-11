// SPDX-FileCopyrightText: 2026 The Catabolic Contributors
// SPDX-License-Identifier: MIT

//! Explicit, bounded UTF-8, embedded-PDF-text and single-image OCR adapters.
//! Call extraction inside a validated source-descriptor lifetime. These
//! adapters never establish absence of text or verify OCR recognition accuracy.

use crate::process::{CommandFailure, CommandOptions, command_output};
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::ffi::{OsStr, OsString};
use std::fs::File;
use std::io::{self, Read, Seek};
use std::os::fd::AsRawFd;
use std::os::unix::fs::{FileExt, MetadataExt, PermissionsExt};
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

type Result<T> = std::result::Result<T, CommandFailure>;
const MAX_SEGMENTS: usize = 10_000;
const PDF_COVERAGE: &str = "embedded PDF text; scanned pages need separate OCR";
const OCR_COVERAGE: &str = "OCR of one raster image; recognition accuracy is not verified";

/// Validate caller-supplied text options without mutating them and resolve the
/// extractor identity to include in the immutable queued job configuration.
pub fn operation_config(options: &Value) -> Result<Value> {
    let input = options
        .as_object()
        .filter(|_| catabolic_core::encode(options).len() <= 256 * 1024)
        .ok_or_else(|| {
            CommandFailure::new("failed", "payload must be a JSON object of at most 256 KiB")
        })?;
    let backend = input
        .get("backend")
        .map_or(Some("utf8"), Value::as_str)
        .filter(|value| ["utf8", "pdf", "ocr"].contains(value))
        .ok_or_else(|| CommandFailure::new("failed", "text backend must be utf8, pdf or ocr"))?;
    if input.keys().any(|key| {
        ![
            "timeout",
            "max_bytes",
            "analysis_bytes",
            "analysis_us",
            "backend",
        ]
        .contains(&key.as_str())
            && !(backend == "pdf" && key == "max_pages")
            && !(backend == "ocr" && key == "language")
    }) {
        return Err(CommandFailure::new("failed", "unknown processing option"));
    }
    let mut config = json!({"timeout":20,"max_bytes":2*1024*1024,"analysis_bytes":1024*1024,"analysis_us":1_000_000});
    for (key, value) in input {
        config[key] = value.clone();
    }
    if backend == "pdf" && config.get("max_pages").is_none() {
        config["max_pages"] = json!(100);
    }
    if backend == "ocr" && config.get("language").is_none() {
        config["language"] = json!("eng");
    }
    validate_config(&config)?;
    config["extractor"] = tool_identity(backend);
    Ok(config)
}

fn budget(options: &Value, key: &str, maximum: u64) -> Result<u64> {
    options[key]
        .as_u64()
        .filter(|value| (1..=maximum).contains(value))
        .ok_or_else(|| CommandFailure::new("failed", format!("invalid {key} budget")))
}

fn validate_config(options: &Value) -> Result<&str> {
    let backend = options
        .get("backend")
        .map_or(Some("utf8"), Value::as_str)
        .filter(|value| ["utf8", "pdf", "ocr"].contains(value))
        .ok_or_else(|| CommandFailure::new("failed", "text backend must be utf8, pdf or ocr"))?;
    for (key, maximum) in [
        ("timeout", 86400),
        ("max_bytes", 8 * 1024 * 1024),
        ("analysis_bytes", 64 * 1024 * 1024),
        ("analysis_us", 60_000_000),
    ] {
        budget(options, key, maximum)?;
    }
    if backend == "pdf"
        && !options["max_pages"]
            .as_u64()
            .is_some_and(|n| (1..=1000).contains(&n))
    {
        return Err(CommandFailure::new("failed", "max_pages must be 1..1000"));
    }
    if backend == "ocr" {
        let valid = options["language"].as_str().is_some_and(|language| {
            let codes = language.split('+').collect::<Vec<_>>();
            (1..=4).contains(&codes.len())
                && codes
                    .iter()
                    .all(|code| code.len() == 3 && code.bytes().all(|b| b.is_ascii_lowercase()))
        });
        if !valid {
            return Err(CommandFailure::new(
                "failed",
                "OCR language must be one to four three-letter language codes",
            ));
        }
    }
    Ok(backend)
}

/// Python-compatible adapter/stat/version provenance. Optional tools are
/// discovered only through PATH; the selected executable is canonicalized.
pub fn tool_identity(backend: &str) -> Value {
    if backend == "utf8" {
        return json!({"adapter":"text-v1"});
    }
    let name = if backend == "pdf" {
        "pdftotext"
    } else {
        "tesseract"
    };
    let missing = || json!({"adapter":format!("text-{backend}-v1"),"tool":name,"available":false});
    if !["pdf", "ocr"].contains(&backend) {
        return missing();
    }
    let Some(search) = std::env::var_os("PATH") else {
        return missing();
    };
    let path = std::env::split_paths(&search)
        .map(|directory| directory.join(name))
        .find(|path| {
            path.metadata().is_ok_and(|metadata| {
                metadata.is_file() && metadata.permissions().mode() & 0o111 != 0
            })
        })
        .and_then(|path| path.canonicalize().ok());
    path.and_then(|path| identity_at_path(backend, &path))
        .unwrap_or_else(missing)
}

fn identity_at_path(backend: &str, path: &Path) -> Option<Value> {
    let metadata = path.metadata().ok()?;
    let tool = path.to_str()?;
    if !metadata.is_file() || metadata.permissions().mode() & 0o111 == 0 {
        return None;
    }
    let version_arg = if backend == "pdf" { "-v" } else { "--version" };
    let version = command_output(
        &[path.as_os_str(), OsStr::new(version_arg)],
        CommandOptions {
            timeout: Duration::from_secs(5),
            maximum: 65536,
            ..Default::default()
        },
    )
    .ok()
    .and_then(|output| {
        let bytes = output.into_bytes(true);
        let text = String::from_utf8_lossy(&bytes);
        python_lines(&text).first().map(|line| (*line).to_owned())
    })
    .unwrap_or_else(|| "version unavailable".into());
    Some(
        json!({"adapter":format!("text-{backend}-v1"),"tool":tool,"available":true,"size":metadata.size(),"mtime_ns":i128::from(metadata.mtime())*1_000_000_000+i128::from(metadata.mtime_nsec()),"version":version}),
    )
}

/// Extract through an already validated, open, regular source file. The caller
/// must retain its source checks through this function and validate afterward.
pub fn extract(
    file: &mut File,
    source_path: &str,
    options: &Value,
    cancel: &AtomicBool,
) -> Result<Value> {
    let backend = validate_config(options)?;
    if cancel.load(Ordering::Acquire) {
        return Err(CommandFailure::new("cancelled", "processing cancelled"));
    }
    if backend == "utf8" {
        let extension = Path::new(source_path)
            .extension()
            .map(|v| format!(".{}", v.to_string_lossy().to_lowercase()))
            .unwrap_or_default();
        if ![".txt", ".md", ".srt", ".vtt"].contains(&extension.as_str()) {
            return Err(CommandFailure::new(
                "unsupported",
                "text supports UTF-8 txt, md, srt and vtt files; select the pdf or ocr backend explicitly",
            ));
        }
        let maximum = budget(options, "max_bytes", 8 * 1024 * 1024)? as usize;
        let deadline = Instant::now() + Duration::from_secs(budget(options, "timeout", 86400)?);
        file.rewind().map_err(io_failure)?;
        let mut bytes = Vec::new();
        let mut chunk = [0; 65536];
        loop {
            if cancel.load(Ordering::Acquire) {
                return Err(CommandFailure::new("cancelled", "processing cancelled"));
            }
            if Instant::now() >= deadline {
                return Err(CommandFailure::new(
                    "timeout",
                    "text exceeded elapsed-time limit",
                ));
            }
            let capacity = chunk.len().min(maximum + 1 - bytes.len());
            let count = match file.read(&mut chunk[..capacity]) {
                Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
                result => result.map_err(io_failure)?,
            };
            if count == 0 {
                break;
            }
            bytes.extend_from_slice(&chunk[..count]);
            if bytes.len() > maximum {
                return Err(CommandFailure::new(
                    "partial",
                    "text exceeds the byte limit",
                ));
            }
        }
        return text_payload(&bytes, &extension);
    }
    extract_document(file, options, cancel, &tool_identity(backend))
}

fn extract_document(
    file: &mut File,
    options: &Value,
    cancel: &AtomicBool,
    current_identity: &Value,
) -> Result<Value> {
    let backend = validate_config(options)?;
    let identity = &options["extractor"];
    if identity != current_identity {
        return Err(CommandFailure::new(
            "changed",
            "document extractor changed since enqueue; enqueue a new job",
        ));
    }
    let tool = identity["tool"]
        .as_str()
        .ok_or_else(|| CommandFailure::new("failed", "malformed document extractor identity"))?;
    if identity["available"] != true {
        return Err(CommandFailure::new(
            "unsupported",
            format!("optional tool {tool} is unavailable"),
        ));
    }
    let maximum = budget(options, "max_bytes", 8 * 1024 * 1024)? as usize;
    let metadata = file.metadata().map_err(io_failure)?;
    if !metadata.is_file() {
        return Err(CommandFailure::new(
            "unsupported",
            "document source must be a regular file",
        ));
    }
    if metadata.len() > maximum as u64 {
        return Err(CommandFailure::new(
            "partial",
            "document input exceeds the byte limit",
        ));
    }
    let mut signature = [0u8; 16];
    let read = file.read_at(&mut signature, 0).map_err(io_failure)?;
    let signature = &signature[..read];
    let fd = file.as_raw_fd();
    let path = format!("/dev/fd/{fd}");
    let args = if backend == "pdf" {
        if !signature.starts_with(b"%PDF-") {
            return Err(CommandFailure::new(
                "unsupported",
                "pdf backend requires a PDF signature",
            ));
        }
        vec![
            tool.to_owned(),
            "-enc".into(),
            "UTF-8".into(),
            "-f".into(),
            "1".into(),
            "-l".into(),
            (options["max_pages"].as_u64().unwrap_or(100) + 1).to_string(),
            path,
            "-".into(),
        ]
    } else {
        if !(signature.starts_with(b"\x89PNG\r\n\x1a\n") || signature.starts_with(b"\xff\xd8\xff"))
        {
            return Err(CommandFailure::new(
                "unsupported",
                "ocr backend supports single PNG or JPEG images",
            ));
        }
        vec![
            tool.to_owned(),
            path,
            "stdout".into(),
            "-l".into(),
            options["language"].as_str().unwrap_or("eng").into(),
            "--psm".into(),
            "3".into(),
        ]
    };
    file.rewind().map_err(io_failure)?;
    let mut env: BTreeMap<OsString, OsString> = std::env::vars_os().collect();
    env.insert("OMP_THREAD_LIMIT".into(), "1".into());
    let output = command_output(
        &args,
        CommandOptions {
            pass_fds: &[fd],
            timeout: Duration::from_secs(budget(options, "timeout", 86400)?),
            maximum,
            cancel: Some(cancel),
            env: Some(&env),
            ..Default::default()
        },
    )
    .map_err(|mut failure| {
        // This race can occur if a previously discovered optional tool is
        // removed before exec. Keep the terminal state accepted by the store.
        if failure.state == "unavailable" {
            failure.state = "unsupported".into();
        }
        failure
    })?;
    document_payload(&output.stdout, backend, options)
}

fn io_failure(error: io::Error) -> CommandFailure {
    let mut result = CommandFailure::new("failed", error.to_string());
    result.io_error = Some(error);
    result
}

fn decode_utf8<'a>(raw: &'a [u8], state: &str) -> Result<&'a str> {
    std::str::from_utf8(raw.strip_prefix(b"\xef\xbb\xbf").unwrap_or(raw))
        .map_err(|_| CommandFailure::new(state, "text requires UTF-8 encoding"))
}

fn document_payload(raw: &[u8], backend: &str, options: &Value) -> Result<Value> {
    let text = decode_utf8(raw, "failed")?;
    let mut segments = Vec::new();
    if backend == "pdf" {
        let mut pages = text.split('\u{c}').collect::<Vec<_>>();
        if pages.last() == Some(&"") {
            pages.pop();
        }
        if pages.len() > options["max_pages"].as_u64().unwrap_or(100) as usize {
            return Err(CommandFailure::new(
                "partial",
                "PDF exceeds the page limit; no partial text was indexed",
            ));
        }
        for (index, page) in pages.into_iter().enumerate() {
            let page = page.trim_matches(python_whitespace);
            if !page.is_empty() {
                segments.push(json!({"locator":{"page":index+1},"text":page}));
            }
        }
    } else {
        for (index, line) in python_lines(text).into_iter().enumerate() {
            if !line.trim_matches(python_whitespace).is_empty() {
                segments.push(json!({"locator":{"page":1,"line":index+1},"text":line}));
            }
            if segments.len() > MAX_SEGMENTS {
                return Err(CommandFailure::new(
                    "partial",
                    "document exceeds 10000 text segments",
                ));
            }
        }
    }
    if segments.is_empty() {
        return Err(CommandFailure::new(
            "unsupported",
            "no text found; this does not establish that the document contains no text",
        ));
    }
    Ok(
        json!({"segments":segments,"backend":backend,"extractor":options["extractor"],"coverage":if backend=="pdf" {PDF_COVERAGE} else {OCR_COVERAGE}}),
    )
}

// CPython 3.14.6's frozen Unicode 16.0 decimal digits. Every Nd block
// contains ten sequential values; adjacent mathematical blocks are distinct.
// CPython 3.11/3.12 use older Unicode tables and reject subsequently added Nd
// blocks. Subtitle spellings follow the frozen 3.14 oracle, not host Rust UCD.
const PYTHON_DECIMAL_ZEROES: &[u32] = &[
    0x30, 0x660, 0x6f0, 0x7c0, 0x966, 0x9e6, 0xa66, 0xae6, 0xb66, 0xbe6, 0xc66, 0xce6, 0xd66,
    0xde6, 0xe50, 0xed0, 0xf20, 0x1040, 0x1090, 0x17e0, 0x1810, 0x1946, 0x19d0, 0x1a80, 0x1a90,
    0x1b50, 0x1bb0, 0x1c40, 0x1c50, 0xa620, 0xa8d0, 0xa900, 0xa9d0, 0xa9f0, 0xaa50, 0xabf0, 0xff10,
    0x104a0, 0x10d30, 0x10d40, 0x11066, 0x110f0, 0x11136, 0x111d0, 0x112f0, 0x11450, 0x114d0,
    0x11650, 0x116c0, 0x116d0, 0x116da, 0x11730, 0x118e0, 0x11950, 0x11bf0, 0x11c50, 0x11d50,
    0x11da0, 0x11f50, 0x16130, 0x16a60, 0x16ac0, 0x16b50, 0x16d70, 0x1ccf0, 0x1d7ce, 0x1d7d8,
    0x1d7e2, 0x1d7ec, 0x1d7f6, 0x1e140, 0x1e2f0, 0x1e4f0, 0x1e5f1, 0x1e950, 0x1fbf0,
];
fn decimal_digit(character: char) -> Option<u8> {
    let number = character as u32;
    let end = PYTHON_DECIMAL_ZEROES.partition_point(|zero| *zero <= number);
    let difference = number.checked_sub(*PYTHON_DECIMAL_ZEROES.get(end.checked_sub(1)?)?)?;
    (difference < 10).then_some(difference as u8)
}
fn float_ascii_space(character: char) -> bool {
    matches!(character, ' ' | '\t' | '\n' | '\r' | '\x0b' | '\x0c')
}
/// Python float(str) spelling adapter, restricted to timestamp number parsing.
/// Decimal underscores are legal only between digits, including Unicode Nd.
/// Other numeric categories (superscripts, circled numbers) remain rejected.
fn timestamp_number(raw: &str) -> Option<f64> {
    let normalized: String = raw
        .chars()
        .map(|c| {
            if let Some(digit) = decimal_digit(c) {
                char::from(b'0' + digit)
            } else if !c.is_ascii() && python_whitespace(c) {
                ' '
            } else {
                c
            }
        })
        .collect();
    let normalized = normalized.trim_matches(float_ascii_space);
    let bytes = normalized.as_bytes();
    let mut number = String::with_capacity(bytes.len());
    for (i, byte) in bytes.iter().enumerate() {
        if *byte == b'_' {
            if i == 0
                || i + 1 == bytes.len()
                || !bytes[i - 1].is_ascii_digit()
                || !bytes[i + 1].is_ascii_digit()
            {
                return None;
            }
        } else if byte.is_ascii() {
            number.push(char::from(*byte));
        } else {
            return None;
        }
    }
    let unsigned = number.strip_prefix(['+', '-']).unwrap_or(&number);
    if unsigned.eq_ignore_ascii_case("nan") {
        return Some(f64::NAN);
    }
    if unsigned.eq_ignore_ascii_case("inf") || unsigned.eq_ignore_ascii_case("infinity") {
        return Some(if number.starts_with('-') {
            f64::NEG_INFINITY
        } else {
            f64::INFINITY
        });
    }
    let bytes = unsigned.as_bytes();
    let mut i = 0;
    while bytes.get(i).is_some_and(u8::is_ascii_digit) {
        i += 1;
    }
    let mut digits = i;
    if bytes.get(i) == Some(&b'.') {
        i += 1;
        let start = i;
        while bytes.get(i).is_some_and(u8::is_ascii_digit) {
            i += 1;
        }
        digits += i - start;
    }
    if digits == 0 {
        return None;
    }
    if matches!(bytes.get(i), Some(b'e' | b'E')) {
        i += 1;
        if matches!(bytes.get(i), Some(b'+' | b'-')) {
            i += 1;
        }
        let start = i;
        while bytes.get(i).is_some_and(u8::is_ascii_digit) {
            i += 1;
        }
        if i == start {
            return None;
        }
    }
    if i != bytes.len() {
        return None;
    }
    number.parse().ok()
}

/// Decode UTF-8 with an optional BOM, retaining Python-compatible line numbers
/// and subtitle cue timestamps. Nonfinite timestamps fail rather than becoming
/// JSON null or being silently accepted as usable evidence.
pub fn text_payload(raw: &[u8], extension: &str) -> Result<Value> {
    let text = decode_utf8(raw, "unsupported")?;
    let mut segments = Vec::new();
    if [".srt", ".vtt"].contains(&extension) {
        for block in subtitle_blocks(text) {
            let lines = python_lines(block);
            let Some(position) = lines.iter().position(|line| line.contains("-->")) else {
                continue;
            };
            let timestamp = lines[position]
                .split("-->")
                .next()
                .unwrap_or_default()
                .trim_matches(python_whitespace)
                .replace(',', ".");
            let parts = timestamp
                .split(':')
                .map(timestamp_number)
                .collect::<Option<Vec<_>>>();
            let Some(parts) = parts else {
                continue;
            };
            let seconds = parts
                .iter()
                .rev()
                .enumerate()
                .map(|(i, n)| n * 60f64.powi(i as i32))
                .sum::<f64>();
            if !seconds.is_finite() {
                return Err(CommandFailure::new(
                    "failed",
                    "subtitle timestamp must be finite",
                ));
            }
            segments
                .push(json!({"locator":{"seconds":seconds},"text":lines[position+1..].join("\n")}));
            if segments.len() > MAX_SEGMENTS {
                return Err(CommandFailure::new(
                    "partial",
                    "text exceeds 10000 segments",
                ));
            }
        }
    } else {
        for (index, line) in python_lines(text).into_iter().enumerate() {
            if !line.trim_matches(python_whitespace).is_empty() {
                segments.push(json!({"locator":{"line":index+1},"text":line}));
            }
            if segments.len() > MAX_SEGMENTS {
                return Err(CommandFailure::new(
                    "partial",
                    "text exceeds 10000 segments",
                ));
            }
        }
    }
    Ok(json!({"segments":segments}))
}

fn python_whitespace(character: char) -> bool {
    character.is_whitespace() || matches!(character, '\u{1c}'..='\u{1f}')
}

fn python_lines(text: &str) -> Vec<&str> {
    let mut lines = Vec::new();
    let mut start = 0;
    let mut characters = text.char_indices().peekable();
    while let Some((index, character)) = characters.next() {
        if matches!(
            character,
            '\n' | '\r'
                | '\u{b}'
                | '\u{c}'
                | '\u{1c}'
                | '\u{1d}'
                | '\u{1e}'
                | '\u{85}'
                | '\u{2028}'
                | '\u{2029}'
        ) {
            lines.push(&text[start..index]);
            start = index + character.len_utf8();
            if character == '\r' && characters.peek().is_some_and(|(_, next)| *next == '\n') {
                characters.next();
                start += 1;
            }
        }
    }
    if start < text.len() {
        lines.push(&text[start..]);
    }
    lines
}

fn subtitle_blocks(text: &str) -> Vec<&str> {
    let mut blocks = Vec::new();
    let mut start = 0;
    let mut search = 0;
    while let Some(offset) = text[search..].find('\n') {
        let newline = search + offset;
        let mut end = None;
        for (index, character) in text[newline + 1..].char_indices() {
            if !python_whitespace(character) {
                break;
            }
            if character == '\n' {
                end = Some(newline + 1 + index + 1);
            }
        }
        if let Some(end) = end {
            let separator = if newline > start && text.as_bytes()[newline - 1] == b'\r' {
                newline - 1
            } else {
                newline
            };
            blocks.push(&text[start..separator]);
            start = end;
            search = end;
        } else {
            search = newline + 1;
        }
    }
    blocks.push(&text[start..]);
    blocks
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::io::Write;
    use std::path::PathBuf;

    fn source(bytes: &[u8]) -> File {
        let mut file = tempfile::tempfile().unwrap();
        file.write_all(bytes).unwrap();
        file.rewind().unwrap();
        file
    }

    fn config(backend: &str, identity: Value) -> Value {
        json!({"backend":backend,"timeout":2,"max_bytes":2*1024*1024,
            "analysis_bytes":1024*1024,"analysis_us":1_000_000,"max_pages":100,
            "language":"eng","extractor":identity})
    }

    struct FakeTool {
        _directory: tempfile::TempDir,
        path: PathBuf,
        identity: Value,
    }
    impl FakeTool {
        fn new(backend: &str, script: &str) -> Self {
            let directory = tempfile::tempdir().unwrap();
            let path = directory.path().join("fixture-extractor");
            fs::write(&path,format!("#!/bin/sh\nif [ \"$1\" = -v ] || [ \"$1\" = --version ]; then printf 'fixture version 1\\n' >&2; exit 0; fi\n{script}\n")).unwrap();
            fs::set_permissions(&path, fs::Permissions::from_mode(0o700)).unwrap();
            let identity = identity_at_path(backend, &path).unwrap();
            Self {
                _directory: directory,
                path,
                identity,
            }
        }
        fn extract(&self, backend: &str, bytes: &[u8], changes: Value) -> Result<Value> {
            let mut options = config(backend, self.identity.clone());
            for (key, value) in changes.as_object().unwrap() {
                options[key] = value.clone();
            }
            extract_document(
                &mut source(bytes),
                &options,
                &AtomicBool::new(false),
                &self.identity,
            )
        }
    }

    #[test]
    fn options_are_explicit_bounded_and_preserve_the_input() {
        let options = json!({"backend":"pdf"});
        let before = options.clone();
        let result = operation_config(&options).unwrap();
        assert_eq!(options, before);
        assert_eq!(result["max_pages"], 100);
        assert_eq!(
            operation_config(&json!({})).unwrap()["extractor"],
            json!({"adapter":"text-v1"})
        );
        for invalid in [
            json!({"backend":[]}),
            json!({"backend":"auto"}),
            json!({"max_bytes":true}),
            json!({"backend":"pdf","max_pages":true}),
            json!({"backend":"pdf","max_pages":1001}),
            json!({"backend":"utf8","language":"eng"}),
            json!({"backend":"ocr","language":"../../config"}),
            json!({"backend":"ocr","language":"eng -y"}),
            json!({"backend":"ocr","language":"eng+fra+ita+deu+spa"}),
            json!({"timeout":0}),
            json!({"timeout":86401}),
            json!({"max_bytes":8*1024*1024+1}),
            json!({"max_bytes":1.0}),
            json!({"unrecognized":1}),
            json!([]),
        ] {
            assert!(operation_config(&invalid).is_err(), "{invalid}");
        }
        assert!(operation_config(&json!({"backend":"ocr","language":"eng+fra"})).is_ok());
    }

    #[test]
    fn utf8_lines_bom_and_python_line_boundaries_match() {
        let data = text_payload(b"\xef\xbb\xbfalpha\r\n\r\n beta \n", ".txt").unwrap();
        assert_eq!(
            data,
            json!({"segments":[{"locator":{"line":1},"text":"alpha"},{"locator":{"line":3},"text":" beta "}]})
        );
        let data = text_payload(
            "one\rtwo\u{b}three\u{c}four\u{1c}five\u{85}six\u{2028}seven\u{2029}".as_bytes(),
            ".md",
        )
        .unwrap();
        assert_eq!(data["segments"].as_array().unwrap().len(), 7);
        assert_eq!(data["segments"][6]["locator"]["line"], 7);
        assert_eq!(
            text_payload(b"\xff", ".txt").unwrap_err().state,
            "unsupported"
        );
        assert_eq!(
            text_payload(b"\n \r\n", ".txt").unwrap(),
            json!({"segments":[]})
        );
    }

    #[test]
    fn subtitle_cues_preserve_timestamps_multiline_and_blank_text() {
        let srt=b"1\r\n00:01:02,500 --> 00:01:05,000\r\nHello\r\nWorld\r\n \t\r\n2\r\ninvalid --> ignored\r\nSkip\r\n\r\n3\r\n00:02:00.0 --> 00:03:00\r\n";
        assert_eq!(
            text_payload(srt, ".srt").unwrap(),
            json!({"segments":[{"locator":{"seconds":62.5},"text":"Hello\nWorld"},{"locator":{"seconds":120.0},"text":""}]})
        );
        let vtt=b"WEBVTT\n\n00:01.200 --> 00:03.500 align:start\nFirst\n\nNOTE ignore me\n\n01:00.000 --> 01:01.000\nSecond";
        let data = text_payload(vtt, ".vtt").unwrap();
        assert_eq!(data["segments"][0]["locator"]["seconds"], 1.2);
        assert_eq!(data["segments"][1]["locator"]["seconds"], 60.0);
        for timestamp in ["NaN", "inf", "-inf", "1e999"] {
            assert_eq!(
                text_payload(format!("{timestamp} --> 1\ntext").as_bytes(), ".srt")
                    .unwrap_err()
                    .state,
                "failed"
            );
        }
    }

    #[test]
    fn subtitle_python_float_spellings_preserve_positive_and_negative_cues() {
        for (timestamp, expected) in [
            ("٠٠:٠٠:٠١,٥٠٠", 1.5),
            ("00:00:1_0.25", 10.25),
            ("00:00:-١_٠.٢٥", -10.25),
            ("00:00:+.٥e١", 5.0),
            ("00:00:𝟜_٢", 42.0),
            ("00:\u{a0} ١ \u{a0}:2", 62.0),
        ] {
            for extension in [".srt", ".vtt"] {
                let value = text_payload(
                    format!("1\n{timestamp} --> 99\nretained cue").as_bytes(),
                    extension,
                )
                .unwrap();
                assert_eq!(
                    value["segments"],
                    json!([{"locator":{"seconds":expected},"text":"retained cue"}]),
                    "{timestamp:?}"
                );
            }
        }
        for timestamp in [
            "00:00:1__0",
            "00:00:_10",
            "00:00:10_",
            "00:00:1_.0",
            "00:00:1._0",
            "00:00:1e_2",
            "00:00:1e+_2",
            "00:00:²",
            "00:00:①",
            "00:00:1−2",
            "00:\u{1f}1:2",
        ] {
            assert_eq!(
                text_payload(format!("{timestamp} --> 99\nignored").as_bytes(), ".srt").unwrap(),
                json!({"segments":[]}),
                "{timestamp:?}"
            );
        }
    }

    #[test]
    fn frozen_python_float_and_exact_subtitle_payload_oracle() {
        let fixture =
            catabolic_core::decode_json(include_str!("../resources/subtitle-float-oracle.json"))
                .unwrap();
        assert_eq!(fixture["python"], "3.14.6");
        assert_eq!(fixture["unicode"], "16.0.0");
        for row in fixture["numbers"].as_array().unwrap() {
            let text = row["text"].as_str().unwrap();
            let actual = timestamp_number(text);
            match row["disposition"].as_str().unwrap() {
                "rejected" => assert!(actual.is_none(), "{text:?}: {actual:?}"),
                "nonfinite" => assert!(
                    actual.is_some_and(|n| !n.is_finite()),
                    "{text:?}: {actual:?}"
                ),
                "finite" => assert_eq!(
                    actual.map(f64::to_bits),
                    row["value"].as_f64().map(f64::to_bits),
                    "{text:?}"
                ),
                other => panic!("invalid oracle disposition: {other}"),
            }
        }
        for row in fixture["subtitles"].as_array().unwrap() {
            assert_eq!(
                text_payload(
                    row["text"].as_str().unwrap().as_bytes(),
                    row["extension"].as_str().unwrap()
                )
                .unwrap(),
                row["expected"]
            );
        }
    }

    #[test]
    fn utf8_file_type_byte_limit_cancellation_and_segment_limit() {
        let options = operation_config(&json!({"max_bytes":4})).unwrap();
        assert_eq!(
            extract(
                &mut source(b"hello"),
                "file.txt",
                &options,
                &AtomicBool::new(false)
            )
            .unwrap_err()
            .state,
            "partial"
        );
        assert_eq!(
            extract(
                &mut source(b"text"),
                "file.TXT",
                &options,
                &AtomicBool::new(false)
            )
            .unwrap()["segments"][0]["text"],
            "text"
        );
        assert_eq!(
            extract(
                &mut source(b"text"),
                "file.pdf",
                &options,
                &AtomicBool::new(false)
            )
            .unwrap_err()
            .state,
            "unsupported"
        );
        assert_eq!(
            extract(
                &mut source(b"text"),
                "file.txt",
                &options,
                &AtomicBool::new(true)
            )
            .unwrap_err()
            .state,
            "cancelled"
        );
        assert_eq!(
            text_payload("x\n".repeat(10001).as_bytes(), ".txt")
                .unwrap_err()
                .state,
            "partial"
        );
        assert_eq!(
            text_payload("0 --> 1\nx\n\n".repeat(10001).as_bytes(), ".srt")
                .unwrap_err()
                .state,
            "partial"
        );
    }

    #[test]
    fn fake_pdf_uses_only_a_source_fd_and_correct_options() {
        let tool = FakeTool::new(
            "pdf",
            r#"
            [ "$1" = -enc ] && [ "$2" = UTF-8 ] && [ "$3" = -f ] && [ "$4" = 1 ] && [ "$5" = -l ] && [ "$6" = 3 ] && [ "$8" = - ] || exit 31
            case "$7" in /dev/fd/[0-9]*) ;; *) exit 32 ;; esac
            [ "$OMP_THREAD_LIMIT" = 1 ] || exit 33
            [ "$(cat "$7")" = '%PDF-fixture' ] || exit 34
            printf 'Catalog alpha\n\fCatalog beta\n\f'
        "#,
        );
        let result = tool
            .extract("pdf", b"%PDF-fixture", json!({"max_pages":2}))
            .unwrap();
        assert_eq!(
            result["segments"][1],
            json!({"locator":{"page":2},"text":"Catalog beta"})
        );
        assert_eq!(result["extractor"], tool.identity);
        assert_eq!(result["coverage"], PDF_COVERAGE);
        assert_eq!(tool.identity["version"], "fixture version 1");
    }

    #[test]
    fn fake_ocr_provenance_language_fd_and_line_locations() {
        let tool = FakeTool::new(
            "ocr",
            r#"
            case "$1" in /dev/fd/[0-9]*) ;; *) exit 32 ;; esac
            [ "$2" = stdout ] && [ "$3" = -l ] && [ "$4" = eng+fra ] && [ "$5" = --psm ] && [ "$6" = 3 ] || exit 31
            [ "$OMP_THREAD_LIMIT" = 1 ] || exit 33
            printf 'Catalog alpha\n\n Beta \n'
        "#,
        );
        let result = tool
            .extract(
                "ocr",
                b"\x89PNG\r\n\x1a\nfixture",
                json!({"language":"eng+fra"}),
            )
            .unwrap();
        assert_eq!(
            result["segments"][1],
            json!({"locator":{"page":1,"line":3},"text":" Beta "})
        );
        assert_eq!(result["coverage"], OCR_COVERAGE);
    }

    #[test]
    fn document_limits_and_malformed_results_never_return_partial_data() {
        let tool = FakeTool::new("pdf", "printf 'first\\fextra\\f'");
        assert_eq!(
            tool.extract("pdf", b"%PDF-fixture", json!({"max_pages":1}))
                .unwrap_err()
                .state,
            "partial"
        );
        assert_eq!(
            tool.extract("pdf", b"%PDF-fixture", json!({"max_bytes":1}))
                .unwrap_err()
                .state,
            "partial"
        );
        assert_eq!(
            tool.extract("pdf", b"not a PDF", json!({}))
                .unwrap_err()
                .state,
            "unsupported"
        );
        for (script, state) in [
            ("printf ''", "unsupported"),
            ("printf '\\377'", "failed"),
            ("printf no; exit 9", "failed"),
            ("while :; do printf verylongoutput >&2; done", "partial"),
        ] {
            let tool = FakeTool::new("pdf", script);
            assert_eq!(
                tool.extract("pdf", b"%PDF-fixture", json!({"max_bytes":1024}))
                    .unwrap_err()
                    .state,
                state
            );
        }
        let tool = FakeTool::new("ocr", "printf ignored");
        assert_eq!(
            tool.extract("ocr", b"GIF89afixture", json!({}))
                .unwrap_err()
                .state,
            "unsupported"
        );
        assert_eq!(
            document_payload(
                "x\n".repeat(10001).as_bytes(),
                "ocr",
                &config("ocr", tool.identity.clone())
            )
            .unwrap_err()
            .state,
            "partial"
        );
    }

    #[test]
    fn changed_missing_removed_and_cancelled_tools_fail_closed() {
        let tool = FakeTool::new("pdf", "printf valid");
        let options = config("pdf", tool.identity.clone());
        let mut different = tool.identity.clone();
        different["version"] = json!("changed");
        assert_eq!(
            extract_document(
                &mut source(b"%PDF-"),
                &options,
                &AtomicBool::new(false),
                &different
            )
            .unwrap_err()
            .state,
            "changed"
        );
        assert_eq!(
            extract_document(
                &mut source(b"%PDF-"),
                &options,
                &AtomicBool::new(true),
                &tool.identity
            )
            .unwrap_err()
            .state,
            "cancelled"
        );
        fs::remove_file(&tool.path).unwrap();
        assert_eq!(
            extract_document(
                &mut source(b"%PDF-"),
                &options,
                &AtomicBool::new(false),
                &tool.identity
            )
            .unwrap_err()
            .state,
            "unsupported"
        );
        let missing = json!({"adapter":"text-pdf-v1","tool":"pdftotext","available":false});
        assert_eq!(
            extract_document(
                &mut source(b"%PDF-"),
                &config("pdf", missing.clone()),
                &AtomicBool::new(false),
                &missing
            )
            .unwrap_err()
            .state,
            "unsupported"
        );
    }

    #[test]
    fn document_timeout_is_bounded() {
        let tool = FakeTool::new("pdf", "sleep 30");
        let start = Instant::now();
        assert_eq!(
            tool.extract("pdf", b"%PDF-", json!({"timeout":1}))
                .unwrap_err()
                .state,
            "timeout"
        );
        assert!(start.elapsed() < Duration::from_secs(3));
    }

    #[test]
    #[ignore = "requires the optional local pdftotext executable"]
    fn real_pdf_text_and_page_overflow() {
        let options = operation_config(&json!({"backend":"pdf"})).unwrap();
        assert_eq!(
            options["extractor"]["available"], true,
            "pdftotext must be installed for this qualification"
        );
        let bytes = pdf_fixture(&["Catalog alpha", "Catalog beta"]);
        let mut file = source(&bytes);
        let result = extract(&mut file, "book.pdf", &options, &AtomicBool::new(false)).unwrap();
        assert_eq!(result["segments"][0]["text"], "Catalog alpha");
        assert_eq!(result["segments"][1]["locator"], json!({"page":2}));
        let mut limited = options;
        limited["max_pages"] = json!(1);
        assert_eq!(
            extract(&mut file, "book.pdf", &limited, &AtomicBool::new(false))
                .unwrap_err()
                .state,
            "partial"
        );
        file.rewind().unwrap();
        let mut unchanged = Vec::new();
        file.read_to_end(&mut unchanged).unwrap();
        assert_eq!(unchanged, bytes);
    }

    #[test]
    #[ignore = "requires local tesseract and its English language data"]
    fn real_single_image_ocr() {
        let options = operation_config(&json!({"backend":"ocr"})).unwrap();
        assert_eq!(
            options["extractor"]["available"], true,
            "tesseract must be installed for this qualification"
        );
        let bytes = png_fixture();
        let mut file = source(&bytes);
        let result = extract(&mut file, "catalog.png", &options, &AtomicBool::new(false)).unwrap();
        assert!(
            result["segments"]
                .as_array()
                .unwrap()
                .iter()
                .any(|segment| segment["text"]
                    .as_str()
                    .unwrap()
                    .to_lowercase()
                    .contains("catalog")),
            "{result}"
        );
        assert_eq!(result["coverage"], OCR_COVERAGE);
        file.rewind().unwrap();
        let mut unchanged = Vec::new();
        file.read_to_end(&mut unchanged).unwrap();
        assert_eq!(unchanged, bytes);
    }

    fn pdf_fixture(pages: &[&str]) -> Vec<u8> {
        let mut objects = vec![
            b"<< /Type /Catalog /Pages 2 0 R >>".to_vec(),
            format!(
                "<< /Type /Pages /Kids [{}] /Count {} >>",
                (0..pages.len())
                    .map(|i| format!("{} 0 R", 4 + 2 * i))
                    .collect::<Vec<_>>()
                    .join(" "),
                pages.len()
            )
            .into_bytes(),
            b"<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>".to_vec(),
        ];
        for (index, text) in pages.iter().enumerate() {
            let content = format!("BT /F1 24 Tf 40 200 Td ({text}) Tj ET");
            objects.push(format!("<< /Type /Page /Parent 2 0 R /MediaBox [0 0 400 300] /Resources << /Font << /F1 3 0 R >> >> /Contents {} 0 R >>",5+2*index).into_bytes());
            objects.push(
                format!(
                    "<< /Length {} >>\nstream\n{content}\nendstream",
                    content.len()
                )
                .into_bytes(),
            );
        }
        let mut result = b"%PDF-1.4\n".to_vec();
        let mut offsets = vec![0];
        for (index, object) in objects.iter().enumerate() {
            offsets.push(result.len());
            writeln!(result, "{} 0 obj", index + 1).unwrap();
            result.extend_from_slice(object);
            result.extend_from_slice(b"\nendobj\n");
        }
        let xref = result.len();
        write!(result, "xref\n0 {}\n0000000000 65535 f \n", offsets.len()).unwrap();
        for offset in &offsets[1..] {
            writeln!(result, "{offset:010} 00000 n ").unwrap();
        }
        write!(
            result,
            "trailer\n<< /Size {} /Root 1 0 R >>\nstartxref\n{xref}\n%%EOF\n",
            offsets.len()
        )
        .unwrap();
        result
    }

    fn png_fixture() -> Vec<u8> {
        let (width, height, scale) = (520usize, 130usize, 10usize);
        let mut pixels = vec![255; width * height];
        for (ordinal, letter) in b"CATALOG".iter().enumerate() {
            let glyph = match letter {
                b'C' => [
                    "01110", "10001", "10000", "10000", "10000", "10001", "01110",
                ],
                b'A' => [
                    "01110", "10001", "10001", "11111", "10001", "10001", "10001",
                ],
                b'T' => [
                    "11111", "00100", "00100", "00100", "00100", "00100", "00100",
                ],
                b'L' => [
                    "10000", "10000", "10000", "10000", "10000", "10000", "11111",
                ],
                b'O' => [
                    "01110", "10001", "10001", "10001", "10001", "10001", "01110",
                ],
                b'G' => [
                    "01110", "10001", "10000", "10111", "10001", "10001", "01110",
                ],
                _ => unreachable!(),
            };
            for (row, values) in glyph.iter().enumerate() {
                for (column, bit) in values.bytes().enumerate() {
                    if bit == b'1' {
                        for y in 30 + row * scale..30 + (row + 1) * scale {
                            let start = y * width + 30 + (ordinal * 7 + column) * scale;
                            pixels[start..start + scale].fill(0);
                        }
                    }
                }
            }
        }
        let mut raw = Vec::new();
        for row in pixels.chunks(width) {
            raw.push(0);
            raw.extend_from_slice(row);
        }
        // A standards-conforming zlib stream with stored DEFLATE blocks keeps
        // this deterministic image fixture independent of compression crates.
        let mut compressed = vec![0x78, 0x01];
        let chunks = raw.chunks(65535);
        let count = chunks.len();
        for (index, chunk) in chunks.enumerate() {
            compressed.push(u8::from(index + 1 == count));
            let length = chunk.len() as u16;
            compressed.extend_from_slice(&length.to_le_bytes());
            compressed.extend_from_slice(&(!length).to_le_bytes());
            compressed.extend_from_slice(chunk);
        }
        let (mut a, mut b) = (1u32, 0u32);
        for byte in raw {
            a = (a + u32::from(byte)) % 65521;
            b = (b + a) % 65521;
        }
        compressed.extend_from_slice(&((b << 16) | a).to_be_bytes());
        let mut png = b"\x89PNG\r\n\x1a\n".to_vec();
        let mut header = Vec::new();
        header.extend_from_slice(&(width as u32).to_be_bytes());
        header.extend_from_slice(&(height as u32).to_be_bytes());
        header.extend_from_slice(&[8, 0, 0, 0, 0]);
        png_chunk(&mut png, b"IHDR", &header);
        png_chunk(&mut png, b"IDAT", &compressed);
        png_chunk(&mut png, b"IEND", &[]);
        png
    }

    fn png_chunk(png: &mut Vec<u8>, kind: &[u8; 4], data: &[u8]) {
        png.extend_from_slice(&(data.len() as u32).to_be_bytes());
        png.extend_from_slice(kind);
        png.extend_from_slice(data);
        let mut crc = u32::MAX;
        for byte in kind.iter().chain(data) {
            crc ^= u32::from(*byte);
            for _ in 0..8 {
                crc = if crc & 1 != 0 {
                    0xedb88320 ^ (crc >> 1)
                } else {
                    crc >> 1
                };
            }
        }
        png.extend_from_slice(&(!crc).to_be_bytes());
    }
}
