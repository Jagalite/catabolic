// SPDX-FileCopyrightText: 2026 The Catabolic Contributors
// SPDX-License-Identifier: MIT
//! Exact decimal packet timing checks. Floating-point rounding must never make
//! a timestamp outside the two-millisecond contract look acceptable.
use catabolic_core::{encode, sha256};
use catabolic_store::{Error, Result};
use num_bigint::BigInt;
use serde_json::{Value, json};
fn decimal(raw: &str) -> Result<(BigInt, i32)> {
    if raw.len() > 768 {
        return Err(Error("excessive packet timestamp precision".into()));
    }
    let (base, exponent) = raw
        .split_once(['e', 'E'])
        .map_or((raw, "0"), |(a, b)| (a, b));
    let exponent = exponent
        .parse::<i32>()
        .ok()
        .filter(|n| (-400..=400).contains(n))
        .ok_or_else(|| Error("invalid packet timestamp exponent".into()))?;
    let negative = base.starts_with('-');
    let unsigned = base.strip_prefix(['-', '+']).unwrap_or(base);
    let (whole, fraction) = unsigned.split_once('.').unwrap_or((unsigned, ""));
    if whole.is_empty()
        || !whole.bytes().all(|b| b.is_ascii_digit())
        || !fraction.bytes().all(|b| b.is_ascii_digit())
    {
        return Err(Error("invalid packet timestamp".into()));
    }
    let mut integer = BigInt::parse_bytes(format!("{whole}{fraction}").as_bytes(), 10)
        .ok_or_else(|| Error("invalid packet timestamp".into()))?;
    if negative {
        integer = -integer;
    }
    Ok((integer, fraction.len() as i32 - exponent))
}
fn difference_within(a: &str, b: &str, offset: &str) -> Result<bool> {
    let a = decimal(a)?;
    let b = decimal(b)?;
    let offset = decimal(offset)?;
    let scale = a.1.max(b.1).max(offset.1).max(3);
    let expand = |(n, p): (BigInt, i32)| -> Result<BigInt> {
        let power =
            u32::try_from(scale - p).map_err(|_| Error("invalid packet timestamp scale".into()))?;
        if power > 1200 {
            return Err(Error("excessive packet timestamp scale".into()));
        }
        Ok(n * BigInt::from(10u8).pow(power))
    };
    let difference = expand(b)? - expand(a)? - expand(offset)?;
    let tolerance = expand((BigInt::from(2u8), 3))?;
    Ok(difference >= -tolerance.clone() && difference <= tolerance)
}
pub fn verify_packets(source: &Value, target: &Value, offset: &Value) -> Result<Value> {
    let source_rows = source
        .as_array()
        .filter(|a| !a.is_empty())
        .ok_or_else(|| Error("component packet evidence is incomplete".into()))?;
    let target_rows = target
        .as_array()
        .ok_or_else(|| Error("component packet evidence is incomplete".into()))?;
    if source_rows.len() != target_rows.len() {
        return Err(Error("mux changed the component packet count".into()));
    }
    if offset.as_f64().is_none_or(|n| !n.is_finite()) {
        return Err(Error("invalid accepted component offset".into()));
    }
    let offset_text = encode(offset);
    for (before, after) in source_rows.iter().zip(target_rows) {
        if before["data_hash"].as_str().is_none_or(str::is_empty)
            || after["data_hash"].as_str().is_none_or(str::is_empty)
        {
            return Err(Error("component packet evidence is incomplete".into()));
        }
        if before["data_hash"] != after["data_hash"] {
            return Err(Error("mux changed a selected component payload".into()));
        }
        for key in ["pts_time", "dts_time", "duration_time"] {
            let a = &before[key];
            let b = &after[key];
            if a.is_null() || b.is_null() {
                if a != b {
                    return Err(Error("mux timing verification is incomplete".into()));
                }
            } else if !difference_within(
                a.as_str()
                    .ok_or_else(|| Error("invalid packet timestamp".into()))?,
                b.as_str()
                    .ok_or_else(|| Error("invalid packet timestamp".into()))?,
                if key == "duration_time" {
                    "0"
                } else {
                    &offset_text
                },
            )? {
                return Err(Error(
                    "mux changed component timing beyond the accepted offset".into(),
                ));
            }
        }
    }
    Ok(
        json!({"packet_count":source_rows.len(),"payload_sha256":sha256(encode(&json!(source_rows.iter().map(|p|&p["data_hash"]).collect::<Vec<_>>()))),"input_timing_sha256":sha256(encode(source)),"output_timing_sha256":sha256(encode(target)),"accepted_offset_seconds":offset,"maximum_timestamp_rounding_seconds":0.002}),
    )
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn exact_thresholds_offsets_and_payloads() {
        assert!(
            difference_within(
                "1000000000000000000000.000000",
                "1000000000000000000000.002000",
                "0"
            )
            .unwrap()
        );
        assert!(
            !difference_within(
                "1000000000000000000000.000000",
                "1000000000000000000000.002001",
                "0"
            )
            .unwrap()
        );
        assert!(difference_within("-0.125000", "0.126999", "0.25").unwrap());
        assert!(!difference_within("0", "0.002", "-5e-324").unwrap());
        let source =
            json!([{"data_hash":"SHA256:abc","pts_time":"1.000000","duration_time":"0.100000"}]);
        let mut target = source.clone();
        target[0]["pts_time"] = json!("1.250000");
        assert_eq!(
            verify_packets(&source, &target, &json!(0.25)).unwrap()["packet_count"],
            1
        );
        target[0]["duration_time"] = json!("0.103000");
        assert!(verify_packets(&source, &target, &json!(0.25)).is_err());
        target = source.clone();
        target[0]["data_hash"] = json!("SHA256:def");
        assert!(verify_packets(&source, &target, &json!(0.0)).is_err());
        assert!(verify_packets(&source, &json!([]), &json!(0.0)).is_err());
    }
}
