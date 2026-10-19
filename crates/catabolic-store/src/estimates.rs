// SPDX-FileCopyrightText: 2026 The Catabolic Contributors
// SPDX-License-Identifier: MIT

//! Versioned, recorded-only size heuristics and retained-evidence calibration.
use crate::{Error, Result, Store, migration::rows};
use catabolic_core::decode_json;
use serde_json::{Value, json};
fn positive(value: &Value) -> Option<f64> {
    let number = match value {
        Value::Bool(b) => Some(u8::from(*b) as f64),
        Value::String(s) => {
            let s = s.trim();
            let c: Vec<_> = s.chars().collect();
            if c.iter().enumerate().any(|(i, ch)| {
                *ch == '_'
                    && (i == 0
                        || i + 1 == c.len()
                        || !c[i - 1].is_ascii_digit()
                        || !c[i + 1].is_ascii_digit())
            }) {
                None
            } else {
                s.replace('_', "").parse::<f64>().ok()
            }
        }
        Value::Number(n) => n.as_f64(),
        _ => None,
    }?;
    (number.is_finite() && number > 0.0 && number <= 1e15).then_some(number)
}
fn number(value: &Value, default: f64) -> f64 {
    value.as_f64().unwrap_or(default)
}
fn integer(value: f64) -> Result<Value> {
    Ok(decode_json(&format!("{value:.0}"))?)
}
fn decimal_sum<'a>(values: impl Iterator<Item = &'a Value>) -> Result<Value> {
    let mut sum = vec![0u8];
    for value in values {
        let raw = value.to_string();
        if !raw.bytes().all(|b| b.is_ascii_digit()) {
            return Err(Error(
                "estimate totals require nonnegative integer bytes".into(),
            ));
        }
        let mut carry = 0;
        for (i, digit) in raw.bytes().rev().enumerate() {
            if i == sum.len() {
                sum.push(0);
            }
            let n = sum[i] + digit - b'0' + carry;
            sum[i] = n % 10;
            carry = n / 10;
        }
        let mut i = raw.len();
        while carry > 0 {
            if i == sum.len() {
                sum.push(0);
            }
            let n = sum[i] + carry;
            sum[i] = n % 10;
            carry = n / 10;
            i += 1;
        }
    }
    Ok(decode_json(
        &sum.iter()
            .rev()
            .map(|b| char::from(b'0' + b))
            .collect::<String>(),
    )?)
}
fn g(value: f64) -> String {
    let mut raw = [0i8; 64];
    // SAFETY: a correctly sized live output buffer and a literal format with
    // one double argument are passed to snprintf. The native process uses C locale.
    unsafe {
        libc::snprintf(raw.as_mut_ptr(), raw.len(), c"%g".as_ptr(), value);
    }
    // SAFETY: snprintf always NUL-terminates this nonempty buffer.
    unsafe { std::ffi::CStr::from_ptr(raw.as_ptr()) }
        .to_string_lossy()
        .into_owned()
}
pub fn estimate(
    recipe: &Value,
    observation: &Value,
    probe: &Value,
    assumptions: &Value,
) -> Result<Value> {
    let preset = recipe["preset"].as_str().unwrap_or("");
    let mut result = json!({"estimator_version":1,"expected_bytes":null,"low_bytes":null,"high_bytes":null,"output_limit_bytes":recipe["max_output_bytes"],"method":"unknown","assumptions":[],"exceeds_output_limit":false});
    if probe.is_null() || probe.as_object().is_some_and(|o| o.is_empty()) {
        result["assumptions"] = json!(["Current probe facts are missing; size is unknown."]);
        return Ok(result);
    }
    let streams = probe["streams"]
        .as_array()
        .map(Vec::as_slice)
        .unwrap_or(&[]);
    let mut duration =
        positive(&probe["summary"]["duration"]).or_else(|| positive(&probe["format"]["duration"]));
    let video = streams
        .iter()
        .filter(|s| s["codec_type"] == "video")
        .collect::<Vec<_>>();
    let audio = streams
        .iter()
        .filter(|s| s["codec_type"] == "audio")
        .collect::<Vec<_>>();
    let mut bytes = None;
    let mut method = "unknown";
    let mut notes = vec![];
    match preset {
        "thumbnail" => {
            if let Some(stream) = video.get(recipe["video_stream"].as_u64().unwrap_or(0) as usize)
                && let (Some(w), Some(h)) =
                    (positive(&stream["width"]), positive(&stream["height"]))
            {
                let width = w.min(640.0);
                let raw = width * ((h * width / w / 2.0).ceil() * 2.0).max(2.0) * 3.0;
                bytes = Some((raw * 0.6 + 4096.0, raw * 0.1 + 4096.0, raw * 1.2 + 65536.0));
                method = "scaled_rgb_png";
                notes.push("PNG size varies with image complexity; RGB storage heuristic at a maximum width of 640.".into());
            }
        }
        "waveform" => {
            let raw = number(&recipe["width"], 0.0) * number(&recipe["height"], 0.0) * 4.0;
            bytes = Some((raw * 0.3 + 4096.0, 4096.0, raw * 1.2 + 65536.0));
            method = "waveform_rgba_png";
            notes.push(
                "PNG compression varies with waveform complexity; fixed output dimensions.".into(),
            );
        }
        "preview" | "h264-720p" | "h264-1080p" | "av1-720p" | "hdr-sdr-1080p"
            if duration.is_some() =>
        {
            if preset == "preview" {
                duration = Some(
                    number(&recipe["duration_seconds"], 0.0)
                        .min((duration.unwrap() - number(&recipe["start_seconds"], 0.0)).max(0.0)),
                );
            }
            let duration = duration.unwrap();
            let kbps = number(
                &assumptions["video_kbps"],
                if ["h264-1080p", "hdr-sdr-1080p"].contains(&preset) {
                    5000.0
                } else {
                    2500.0
                },
            );
            let rate =
                if !audio.is_empty() && recipe.get("audio_stream").is_none_or(|v| !v.is_null()) {
                    number(
                        &recipe["audio_bitrate_kbps"],
                        if preset == "av1-720p" { 96.0 } else { 160.0 },
                    )
                } else {
                    0.0
                };
            bytes = Some((
                duration * (kbps + rate) * 1000.0 / 8.0 * 1.02,
                duration * (kbps * 0.4 + rate) * 1000.0 / 8.0,
                duration * (kbps * 3.0 + rate) * 1000.0 / 8.0 * 1.05,
            ));
            method = "duration_times_assumed_bitrate";
            notes.push(format!(
                "Assume {kbps} kbps video and {rate} kbps audio; 2% container overhead."
            ));
            notes.push("This is a planning assumption, not an encoder bitrate setting. CRF output can fall outside this heuristic range.".into());
        }
        "remux-mkv" => {
            if let Some(size) = positive(&observation["size"]) {
                bytes = Some((size * 1.01, size * 0.95, size * 1.1));
                method = "source_size";
                notes.push("Stream copy usually stays near source size; selecting fewer streams can make it much smaller.".into());
            }
        }
        "audio-aac" | "audio-opus" if duration.is_some() => {
            let bitrate = number(
                &recipe["audio_bitrate_kbps"],
                if preset == "audio-opus" { 96.0 } else { 128.0 },
            );
            let expected = duration.unwrap() * bitrate * 1000.0 / 8.0 * 1.02;
            bytes = Some((expected, expected * 0.85, expected * 1.2));
            method = "audio_bitrate";
            notes.push(format!("Assume {bitrate} kbps audio; VBR and container overhead vary. AAC's omitted 128 kbps is an estimate of the encoder default."));
        }
        "audio-flac" | "audio-normalize" if duration.is_some() => {
            if let Some(stream) = audio.get(recipe["stream"].as_u64().unwrap_or(0) as usize) {
                let mut rate = positive(&stream["sample_rate"]);
                let channels = positive(&stream["channels"]);
                let mut bits = positive(&stream["bits_per_raw_sample"]).unwrap_or(16.0);
                if preset == "audio-normalize" {
                    rate = Some(48000.0);
                    bits = 16.0;
                }
                if let (Some(rate), Some(channels)) = (rate, channels) {
                    let raw = duration.unwrap() * rate * channels * bits / 8.0;
                    bytes = Some((raw * 0.65, raw * 0.3, raw * 1.1));
                    method = "pcm_compression_ratio";
                    notes.push(format!("Assume {}-bit samples and 65% of raw PCM size; compression varies with content.",g(bits)));
                }
            }
        }
        "subtitle-srt" if duration.is_some() => {
            let duration = duration.unwrap();
            bytes = Some((
                duration * 20.0 + 1024.0,
                duration * 2.0 + 1024.0,
                duration * 100.0 + 1024.0,
            ));
            method = "subtitle_text_density";
            notes.push(
                "Assume 20 bytes of subtitle text/timing per second; dialogue density varies."
                    .into(),
            );
        }
        _ => {}
    }
    if let Some((expected, low, high)) = bytes {
        result["expected_bytes"] = integer(expected.ceil())?;
        result["low_bytes"] = integer(low.ceil())?;
        result["high_bytes"] = integer(high.ceil())?;
        result["exceeds_output_limit"] = json!(high > number(&recipe["max_output_bytes"], 0.0));
    } else {
        notes = vec![
            "Probe facts lack the duration or stream properties needed for an estimate.".into(),
        ];
    }
    result["method"] = json!(method);
    result["assumptions"] = json!(notes);
    Ok(result)
}
pub fn total(values: &Value) -> Result<Value> {
    let values = values
        .as_array()
        .ok_or_else(|| Error("estimates must be a list".into()))?;
    let known = values
        .iter()
        .filter(|v| !v["expected_bytes"].is_null())
        .collect::<Vec<_>>();
    let unknown = values.len() - known.len();
    let sum = |key: &str| decimal_sum(known.iter().map(move |v| &v[key]));
    Ok(
        json!({"output_count":values.len(),"estimated_count":known.len(),"unknown_count":unknown,"expected_bytes":if unknown==0{sum("expected_bytes")?}else{Value::Null},"known_expected_bytes":sum("expected_bytes")?,"low_bytes":if unknown==0{sum("low_bytes")?}else{Value::Null},"high_bytes":if unknown==0{sum("high_bytes")?}else{Value::Null},"planning_bytes":decimal_sum(values.iter().map(|v|if v["high_bytes"].is_null(){&v["output_limit_bytes"]}else{&v["high_bytes"]}))?,"configured_output_limits_bytes":decimal_sum(values.iter().map(|v|&v["output_limit_bytes"]))?,"estimates_exceeding_output_limit":values.iter().filter(|v|v["exceeds_output_limit"]==true).count()}),
    )
}
pub fn calibration(
    store: &Store,
    profile: &str,
    recipe: &Value,
    assumptions: &Value,
) -> Result<Value> {
    let mut report = json!({"version":1,"recipe_id":recipe["id"],"sample_count":0,"minimum_samples":3,"applied":false,"factor":null,"reason":"at least three distinct successful source files are required"});
    if assumptions.as_object().is_some_and(|o| !o.is_empty()) {
        report["reason"] = json!("explicit planning assumptions take precedence");
        return Ok(report);
    }
    let records = rows(
        &store.db,
        "WITH samples AS (SELECT j.file_id,j.snapshot,a.validation,a.size,a.id,a.created_at,a.rowid AS sample_order,row_number() OVER (PARTITION BY j.file_id ORDER BY a.created_at DESC,a.rowid DESC) AS rank FROM processing_jobs j JOIN processing_artifacts a ON a.job_id=j.id WHERE j.profile=? AND j.recipe_id=? AND j.state='complete' AND a.state='ready') SELECT * FROM samples WHERE rank=1 ORDER BY created_at DESC,sample_order DESC LIMIT 100",
        &[
            profile.to_owned().into(),
            recipe["id"].as_str().unwrap_or("").to_owned().into(),
        ],
    )?;
    let mut ratios = vec![];
    for row in records {
        let validation = decode_json(
            row["validation"]
                .as_str()
                .filter(|s| !s.is_empty())
                .unwrap_or("{}"),
        )?;
        let baseline = estimate(
            &recipe["definition"],
            &decode_json(row["snapshot"].as_str().unwrap())?,
            &validation["input"],
            &json!({}),
        )?;
        if let (Some(baseline), Some(size)) = (
            baseline["expected_bytes"].as_f64().filter(|n| *n > 0.0),
            row["size"].as_f64().filter(|n| *n > 0.0),
        ) {
            ratios.push(size / baseline);
        }
    }
    report["sample_count"] = json!(ratios.len());
    if ratios.len() < 3 {
        return Ok(report);
    }
    ratios.sort_by(f64::total_cmp);
    let n = ratios.len();
    report["applied"] = json!(true);
    report["factor"] = json!(if n % 2 == 0 {
        (ratios[n / 2 - 1] + ratios[n / 2]) / 2.0
    } else {
        ratios[n / 2]
    });
    report["low_factor"] = json!(ratios[0]);
    report["high_factor"] = json!(ratios[n - 1]);
    report["reason"] = json!(
        "median actual / heuristic bytes for this recipe and profile; budget never decreases"
    );
    Ok(report)
}
pub fn calibrated(value: &Value, report: &Value) -> Result<Value> {
    if report["applied"] != true || value["expected_bytes"].is_null() {
        return Ok(value.clone());
    }
    let mut result = value.clone();
    let original = number(&value["expected_bytes"], 0.0);
    let expected = (original * number(&report["factor"], 0.0)).ceil().max(1.0);
    // Python max/min retain exact integer operands. Do not round a previously
    // recorded integer byte budget through f64 merely to compare it.
    let expected = integer(expected)?;
    let measured_high = integer((original * number(&report["high_factor"], 0.0) * 1.25).ceil())?;
    let measured_low = integer((original * number(&report["low_factor"], 0.0)).floor())?;
    let cmp = |a: &Value, b: &Value| {
        let a = a.to_string();
        let b = b.to_string();
        a.len().cmp(&b.len()).then_with(|| a.cmp(&b))
    };
    let high = [&value["high_bytes"], &measured_high, &expected]
        .into_iter()
        .max_by(|a, b| cmp(a, b))
        .unwrap()
        .clone();
    let low = if cmp(&value["low_bytes"], &measured_low).is_lt() {
        value["low_bytes"].clone()
    } else {
        measured_low
    };
    result["estimator_version"] = json!(2);
    result["expected_bytes"] = expected;
    result["low_bytes"] = low;
    result["exceeds_output_limit"] = json!(cmp(&high, &value["output_limit_bytes"]).is_gt());
    result["high_bytes"] = high;
    result["method"] = json!(value["method"].as_str().unwrap_or("").to_owned() + "_measured");
    result["calibration_samples"] = report["sample_count"].clone();
    result["assumptions"].as_array_mut().ok_or_else(||Error("estimate assumptions must be a list".into()))?.push(json!("Historical recipe measurements adjust expectation; original high budget is retained and may increase."));
    Ok(result)
}
