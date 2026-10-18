// SPDX-FileCopyrightText: 2026 The Catabolic Contributors
// SPDX-License-Identifier: MIT

//! Actual fixed-presets qualification against exclusively created temporary files.
use catabolic_runtime::rendering;
use serde_json::json;
use std::{fs::File, process::Command, sync::atomic::AtomicBool};
fn fixture(path: &std::path::Path, kind: &str) {
    let mut command = Command::new("ffmpeg");
    command.args([
        "-v",
        "error",
        "-f",
        "lavfi",
        "-i",
        "testsrc2=size=160x120:rate=10:duration=2",
    ]);
    if kind == "av" {
        command.args([
            "-f",
            "lavfi",
            "-i",
            "sine=frequency=440:sample_rate=48000:duration=2",
            "-c:v",
            "libx264",
            "-pix_fmt",
            "yuv420p",
            "-c:a",
            "aac",
        ]);
    } else if kind == "hdr" {
        command.args([
            "-c:v",
            "libx264",
            "-pix_fmt",
            "yuv420p10le",
            "-x264-params",
            "colorprim=bt2020:transfer=smpte2084:colormatrix=bt2020nc",
            "-color_range",
            "tv",
        ]);
    } else {
        command.args(["-frames:v", "1", "-c:v", "png", "-update", "1"]);
    }
    let output = command
        .args(["-threads", "1", "-filter_threads", "1"])
        .arg(path)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
}
#[test]
fn every_available_fixed_preset_validates_actual_output_and_preserves_source() {
    let dir = tempfile::tempdir().unwrap();
    let av = dir.path().join("input.mkv");
    let photo = dir.path().join("input.png");
    let hdr = dir.path().join("hdr.mkv");
    let subtitle = dir.path().join("input.srt");
    fixture(&av, "av");
    fixture(&photo, "photo");
    fixture(&hdr, "hdr");
    std::fs::write(
        &subtitle,
        b"1\n00:00:00,000 --> 00:00:01,000\nNative text\n",
    )
    .unwrap();
    let capabilities = rendering::capabilities().unwrap();
    let identity = &capabilities["tools"];
    for spec in rendering::PRESETS {
        if spec.name == "component-mux" {
            continue;
        }
        let available = capabilities["presets"]
            .as_array()
            .unwrap()
            .iter()
            .find(|v| v["name"] == spec.name)
            .unwrap();
        assert_eq!(
            available["available"], true,
            "this acceptance lane requires preset {}: {available}",
            spec.name
        );
        let source = match spec.name {
            "image-jpeg" | "image-webp" => &photo,
            "hdr-sdr-1080p" => &hdr,
            "subtitle-srt" => &subtitle,
            _ => &av,
        };
        let before = std::fs::read(source).unwrap();
        let mut input = File::open(source).unwrap();
        let mut output = tempfile::tempfile().unwrap();
        let options = if spec.name == "av1-720p" {
            json!({"encoder_speed":"veryfast"})
        } else {
            json!({})
        };
        let recipe = rendering::definition(spec.name, &options).unwrap();
        let rendered = rendering::render(
            &mut input,
            &mut output,
            &recipe,
            identity,
            &AtomicBool::new(false),
            &mut || Ok(()),
        )
        .unwrap_or_else(|error| panic!("{}: {error}", spec.name));
        assert!(rendered.size > 0);
        if spec.name == "audio-normalize" {
            let streams = rendered.validation["output"]["streams"].as_array().unwrap();
            let audio = streams.iter().find(|s| s["codec_type"] == "audio").unwrap();
            assert_eq!(audio["codec_name"], "flac");
            assert_eq!(audio["sample_rate"], "48000");
            assert_eq!(audio["sample_fmt"], "s16");
        }
        assert_eq!(rendered.sha256, rendering::digest(&mut output).unwrap());
        assert_eq!(
            rendered.output_revision,
            rendering::file_revision(&output).unwrap()
        );
        assert_eq!(
            rendered.validation["recipe_digest"],
            catabolic_core::sha256(catabolic_core::encode(&recipe))
        );
        assert_eq!(
            std::fs::read(source).unwrap(),
            before,
            "{} changed source bytes",
            spec.name
        );
    }
}
#[test]
fn recipe_bounds_hdr_admission_and_output_identity_fail_closed() {
    assert!(rendering::definition("image-jpeg", &json!({"timeout":301})).is_err());
    assert!(rendering::definition("preview", &json!({"video_stream":true})).is_err());
    assert!(rendering::definition("remux-mkv", &json!({"stream_indices":[1,1]})).is_err());
    assert!(rendering::definition("audio-flac", &json!({"command":"rm"})).is_err());
    let recipe = rendering::definition("hdr-sdr-1080p", &json!({})).unwrap();
    let missing = json!({"streams":[{"index":0,"codec_type":"video","width":160,"height":120,"color_space":"bt2020nc","color_range":"tv"}]});
    assert_eq!(
        rendering::input_error(&recipe, &missing).unwrap().unwrap(),
        "tone mapping requires tagged BT.2020 PQ/HLG input with a known range"
    );
    let dir = tempfile::tempdir().unwrap();
    let source = dir.path().join("input.mkv");
    fixture(&source, "av");
    let bytes = std::fs::read(&source).unwrap();
    let mut input = File::open(&source).unwrap();
    let mut same = input.try_clone().unwrap();
    let identity = rendering::tools(true).unwrap();
    assert!(
        rendering::render(
            &mut input,
            &mut same,
            &rendering::definition("thumbnail", &json!({})).unwrap(),
            &identity,
            &AtomicBool::new(false),
            &mut || Ok(())
        )
        .is_err()
    );
    assert_eq!(std::fs::read(&source).unwrap(), bytes);
    let mut output = tempfile::tempfile().unwrap();
    let recipe = rendering::definition("thumbnail", &json!({"max_size_ratio":0.0000001})).unwrap();
    assert!(
        rendering::render(
            &mut input,
            &mut output,
            &recipe,
            &identity,
            &AtomicBool::new(false),
            &mut || Ok(())
        )
        .is_err()
    );
    assert_eq!(std::fs::read(&source).unwrap(), bytes);
}
