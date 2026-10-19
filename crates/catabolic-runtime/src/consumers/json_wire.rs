// SPDX-FileCopyrightText: 2026 The Catabolic Contributors
// SPDX-License-Identifier: MIT
//! JSON-only remote response decoding shared by the consumer and PIN adapters.
//! Callers retain their own HTTP byte bounds and public error classifications.
//! Private files, HTTP request bodies and canonical JSON codecs are unchanged.
use catabolic_core::{Error, Result};
use serde_json::Value;

pub(crate) fn decode(bytes: &[u8]) -> Result<Value> {
    enum Encoding {
        Utf8,
        Utf16(bool),
        Utf32(bool),
    }
    let (encoding, raw) = if bytes.starts_with(&[0xff, 0xfe, 0, 0]) {
        (Encoding::Utf32(true), &bytes[4..])
    } else if bytes.starts_with(&[0, 0, 0xfe, 0xff]) {
        (Encoding::Utf32(false), &bytes[4..])
    } else if bytes.starts_with(&[0xff, 0xfe]) {
        (Encoding::Utf16(true), &bytes[2..])
    } else if bytes.starts_with(&[0xfe, 0xff]) {
        (Encoding::Utf16(false), &bytes[2..])
    } else if bytes.starts_with(&[0xef, 0xbb, 0xbf]) {
        (Encoding::Utf8, &bytes[3..])
    } else if bytes.len() >= 4 && bytes[0] == 0 {
        (
            if bytes[1] == 0 {
                Encoding::Utf32(false)
            } else {
                Encoding::Utf16(false)
            },
            bytes,
        )
    } else if bytes.len() >= 4 && bytes[1] == 0 {
        (
            if bytes[2] == 0 && bytes[3] == 0 {
                Encoding::Utf32(true)
            } else {
                Encoding::Utf16(true)
            },
            bytes,
        )
    } else if bytes.len() == 2 && bytes[0] == 0 {
        (Encoding::Utf16(false), bytes)
    } else if bytes.len() == 2 && bytes[1] == 0 {
        (Encoding::Utf16(true), bytes)
    } else {
        (Encoding::Utf8, bytes)
    };
    let invalid = || Error("invalid network JSON encoding".into());
    let decoded = match encoding {
        Encoding::Utf8 => std::str::from_utf8(raw).map_err(|_| invalid())?.to_owned(),
        Encoding::Utf16(little) => {
            let (chunks, remainder) = raw.as_chunks::<2>();
            if !remainder.is_empty() {
                return Err(invalid());
            }
            let units = chunks.iter().map(|c| {
                if little {
                    u16::from_le_bytes([c[0], c[1]])
                } else {
                    u16::from_be_bytes([c[0], c[1]])
                }
            });
            char::decode_utf16(units)
                .collect::<std::result::Result<String, _>>()
                .map_err(|_| invalid())?
        }
        Encoding::Utf32(little) => {
            let (chunks, remainder) = raw.as_chunks::<4>();
            if !remainder.is_empty() {
                return Err(invalid());
            }
            chunks
                .iter()
                .map(|c| {
                    let c = [c[0], c[1], c[2], c[3]];
                    char::from_u32(if little {
                        u32::from_le_bytes(c)
                    } else {
                        u32::from_be_bytes(c)
                    })
                    .ok_or_else(invalid)
                })
                .collect::<Result<String>>()?
        }
    };
    catabolic_core::decode_json(&decoded).map_err(|_| invalid())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn wires(text: &str) -> Vec<Vec<u8>> {
        let mut values = vec![text.as_bytes().to_vec()];
        values.push([&[0xef, 0xbb, 0xbf][..], text.as_bytes()].concat());
        for little in [false, true] {
            let utf16 = text
                .encode_utf16()
                .flat_map(|unit| {
                    if little {
                        unit.to_le_bytes()
                    } else {
                        unit.to_be_bytes()
                    }
                })
                .collect::<Vec<_>>();
            let bom16 = if little { [0xff, 0xfe] } else { [0xfe, 0xff] };
            values.push(utf16.clone());
            values.push([&bom16[..], &utf16].concat());
            let utf32 = text
                .chars()
                .flat_map(|c| {
                    if little {
                        (c as u32).to_le_bytes()
                    } else {
                        (c as u32).to_be_bytes()
                    }
                })
                .collect::<Vec<_>>();
            let bom32 = if little {
                [0xff, 0xfe, 0, 0]
            } else {
                [0, 0, 0xfe, 0xff]
            };
            values.push(utf32.clone());
            values.push([&bom32[..], &utf32].concat());
        }
        values
    }

    #[test]
    fn unicode_wire_encodings_keep_json_values_and_integer_lexemes() {
        for text in [
            r#"{"title":"Café€漢字😀","n":1208925819614629174706177}"#,
            "[null,true,false,0,-1,1.5,1e2,\"\\uD83D\\uDE00\"]",
            "null",
            "true",
            "0",
            "[]",
            "{}",
            " \r\n{\"a\":1}\t",
        ] {
            let expected = catabolic_core::decode_json(text).unwrap();
            for bytes in wires(text) {
                assert_eq!(decode(&bytes).unwrap(), expected);
            }
        }
    }

    #[test]
    fn wire_decoding_does_not_change_private_object_or_duplicate_handling() {
        let raw = r#"{"value":1,"value":2,"$serde_json::private::Number":"1e400","n":1208925819614629174706177}"#;
        for bytes in wires(raw) {
            let value = decode(&bytes).unwrap();
            assert_eq!(value["value"], 2);
            assert_eq!(value["$serde_json::private::Number"], "1e400");
            assert_eq!(value["n"].to_string(), "1208925819614629174706177");
        }
        // The transport-only helper does not loosen the canonical UTF-8 codec.
        let bom = [0xef, 0xbb, 0xbf, b'{', b'}'];
        assert!(catabolic_core::decode_json_bytes(&bom).is_err());
        assert_eq!(decode(&bom).unwrap(), json!({}));
    }

    #[test]
    fn malformed_encodings_and_nonfinite_json_remain_errors() {
        for bytes in [
            vec![],
            vec![0xff],
            vec![0xef, 0xbb],
            vec![0xff, 0xfe, b'{'],
            vec![0xff, 0xfe, 0, 0, b'{', 0, 0],
            vec![0, 0, 0xfe, 0xff, 0, 0x11, 0, 0],
            vec![0xff, 0xfe, b'"', 0, 0, 0xd8, b'"', 0],
            b"{\"a\":NaN}".to_vec(),
            b"{\"a\":1e400}".to_vec(),
        ] {
            assert!(decode(&bytes).is_err(), "accepted malformed {bytes:?}");
        }
    }
}
