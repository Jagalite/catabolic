// SPDX-FileCopyrightText: 2026 The Catabolic Contributors
// SPDX-License-Identifier: MIT
//! XML-only wire decoding. The borrowed tree never outlives its decoded owner;
//! HTTP and non-XML response bodies retain their existing byte semantics.
use super::{Failure, MAXIMUM, NetworkResult, invalid, xml_encodings};
use std::borrow::Cow;

pub(crate) struct XmlText<'a>(Cow<'a, str>);

fn whitespace(c: char) -> bool {
    matches!(c, ' ' | '\t' | '\r' | '\n')
}

/// Read only the initial declaration's ASCII pseudo-attributes. The XML parser
/// validates their order and the complete document. We additionally validate
/// EncName because roxmltree does not interpret or validate declared encodings.
fn declaration(raw: &[u8]) -> NetworkResult<Option<&str>> {
    if !raw.starts_with(b"<?xml") || !raw.get(5).is_some_and(|b| whitespace(*b as char)) {
        return Ok(None);
    }
    let end = raw
        .windows(2)
        .position(|b| b == b"?>")
        .ok_or_else(invalid)?;
    let mut rest = std::str::from_utf8(&raw[5..end]).map_err(|_| invalid())?;
    while !rest.trim_matches(whitespace).is_empty() {
        let (name, value) = rest
            .trim_start_matches(whitespace)
            .split_once('=')
            .ok_or_else(invalid)?;
        let value = value.trim_start_matches(whitespace);
        let quote = value
            .chars()
            .next()
            .filter(|c| matches!(c, '\'' | '"'))
            .ok_or_else(invalid)?;
        let end = value[1..].find(quote).ok_or_else(invalid)? + 1;
        let encoding = &value[1..end];
        rest = &value[end + 1..];
        if name.trim_end_matches(whitespace) == "encoding" {
            if !encoding
                .as_bytes()
                .first()
                .is_some_and(u8::is_ascii_alphabetic)
                || !encoding
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-'))
            {
                return Err(invalid());
            }
            return Ok(Some(encoding));
        }
    }
    Ok(None)
}

fn append(text: &mut String, c: char) -> NetworkResult<()> {
    if text.len() + c.len_utf8() > MAXIMUM {
        return Err(Failure::new("response_limit"));
    }
    text.push(c);
    Ok(())
}

/// CPython codec lookup lowercases and collapses hyphens/underscores. EncName
/// already excludes spaces and other punctuation; dots remain significant.
fn normalized(name: &str) -> String {
    let mut result = String::with_capacity(name.len());
    let mut separator = false;
    for byte in name.bytes() {
        if matches!(byte, b'-' | b'_') {
            separator = !result.is_empty();
        } else {
            if separator {
                result.push('_');
                separator = false;
            }
            result.push(byte.to_ascii_lowercase() as char);
        }
    }
    result
}

impl<'a> XmlText<'a> {
    pub(super) fn decode(raw: &'a [u8]) -> NetworkResult<Self> {
        if raw.len() > MAXIMUM {
            return Err(Failure::new("response_limit"));
        }
        // Expat does not accept UTF-32, even with a matching declaration.
        if raw.starts_with(&[0, 0, 0xfe, 0xff]) || raw.starts_with(&[0xff, 0xfe, 0, 0]) {
            return Err(invalid());
        }
        let (raw, little) = if let Some(rest) = raw.strip_prefix(&[0xff, 0xfe]) {
            (rest, Some(true))
        } else if let Some(rest) = raw.strip_prefix(&[0xfe, 0xff]) {
            (rest, Some(false))
        } else if raw.len() >= 2 && raw[0] != 0 && raw[1] == 0 {
            (raw, Some(true))
        } else if raw.len() >= 2 && raw[0] == 0 && raw[1] != 0 {
            (raw, Some(false))
        } else {
            (raw.strip_prefix(&[0xef, 0xbb, 0xbf]).unwrap_or(raw), None)
        };
        let mut decoded = if let Some(little) = little {
            if raw.len() % 2 != 0 {
                return Err(invalid());
            }
            let mut text = String::with_capacity(raw.len());
            let units = raw.as_chunks::<2>().0.iter().map(|b| {
                if little {
                    u16::from_le_bytes([b[0], b[1]])
                } else {
                    u16::from_be_bytes([b[0], b[1]])
                }
            });
            for c in char::decode_utf16(units) {
                append(&mut text, c.map_err(|_| invalid())?)?;
            }
            if let Some(name) = declaration(text.as_bytes())?
                && !name.eq_ignore_ascii_case("UTF-16")
                && !name.eq_ignore_ascii_case(if little { "UTF-16LE" } else { "UTF-16BE" })
            {
                return Err(invalid());
            }
            Cow::Owned(text)
        } else {
            match declaration(raw)? {
                // Expat treats this exact spelling (case-insensitively) as its
                // built-in UTF-8 decoder. Python aliases such as utf8 instead
                // take its static unknown-encoding map, which is ASCII-only.
                None => Cow::Borrowed(std::str::from_utf8(raw).map_err(|_| invalid())?),
                Some(name) if name.eq_ignore_ascii_case("UTF-8") => {
                    Cow::Borrowed(std::str::from_utf8(raw).map_err(|_| invalid())?)
                }
                Some(name) => {
                    let key = normalized(name);
                    let index = xml_encodings::ALIASES
                        .binary_search_by_key(&key.as_str(), |(name, _)| *name)
                        .map_err(|_| invalid())?;
                    let table = &xml_encodings::TABLES[xml_encodings::ALIASES[index].1];
                    let mut text = String::with_capacity(raw.len());
                    for byte in raw {
                        append(
                            &mut text,
                            char::from_u32(table[*byte as usize]).ok_or_else(invalid)?,
                        )?;
                    }
                    Cow::Owned(text)
                }
            }
        };
        // One wire BOM was already consumed. roxmltree would skip another;
        // Expat rejects a second BOM instead of treating it as another marker.
        if decoded.starts_with('\u{feff}') {
            return Err(invalid());
        }
        // Decode first: UTF-16 and declared encodings cannot hide a DTD/entity
        // behind NULs or alternate wire bytes. No resolver is ever installed.
        let upper = decoded.to_ascii_uppercase();
        if upper.contains("<!DOCTYPE") || upper.contains("<!ENTITY") {
            return Err(invalid());
        }
        // Expat accepts XML whitespace after <?xml; roxmltree requires a space
        // in this position. Normalize this one byte after encoding validation.
        if decoded.starts_with("<?xml")
            && decoded
                .as_bytes()
                .get(5)
                .is_some_and(|b| matches!(b, b'\t' | b'\r' | b'\n'))
        {
            decoded.to_mut().replace_range(5..6, " ");
        }
        Ok(Self(decoded))
    }

    pub(crate) fn parse(&self) -> NetworkResult<roxmltree::Document<'_>> {
        let document = roxmltree::Document::parse_with_options(
            &self.0,
            roxmltree::ParsingOptions {
                allow_dtd: false,
                nodes_limit: 200_000,
                entity_resolver: None,
            },
        )
        .map_err(|_| invalid())?;
        // XML reserves every case spelling of the exact PI target "xml".
        // roxmltree recognizes the valid initial lowercase declaration, but
        // otherwise permits mixed/uppercase spellings as ordinary PIs. Expat
        // rejects those; inspect parsed nodes so comments, CDATA and escaped
        // attribute values cannot be mistaken for processing instructions.
        if document.descendants().any(|node| {
            node.pi()
                .is_some_and(|pi| pi.target.eq_ignore_ascii_case("xml"))
        }) {
            return Err(invalid());
        }
        Ok(document)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::Value;

    fn attribute(raw: &[u8]) -> Result<String, String> {
        let xml = XmlText::decode(raw).map_err(|e| e.code)?;
        let document = xml.parse().map_err(|e| e.code)?;
        Ok(document.root_element().attribute("a").unwrap_or("").into())
    }

    fn utf16(text: &str, little: bool, bom: bool) -> Vec<u8> {
        let mut bytes = Vec::new();
        if bom {
            bytes.extend(if little { [0xff, 0xfe] } else { [0xfe, 0xff] });
        }
        for unit in text.encode_utf16() {
            bytes.extend(if little {
                unit.to_le_bytes()
            } else {
                unit.to_be_bytes()
            });
        }
        bytes
    }

    #[test]
    fn frozen_single_byte_declaration_matrix() {
        let oracle: Value = serde_json::from_str(include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../tests/fixtures/plex_xml_encodings.json"
        )))
        .unwrap();
        for (name, outcome) in oracle["cases"].as_object().unwrap() {
            let expected = &oracle["outcomes"][outcome.as_u64().unwrap() as usize];
            for byte in 0u8..=255 {
                let mut raw =
                    format!("<?xml version=\"1.0\" encoding=\"{name}\"?><x a=\"").into_bytes();
                raw.extend([byte, b'"', b'/', b'>']);
                match expected[byte as usize].as_str() {
                    Some(value) => {
                        assert_eq!(attribute(&raw), Ok(value.into()), "{name}: {byte:#04x}")
                    }
                    None => assert!(attribute(&raw).is_err(), "{name}: {byte:#04x}"),
                }
            }
        }
        for name in oracle["rejected_declarations"].as_object().unwrap().keys() {
            let raw = format!("<?xml version=\"1.0\" encoding=\"{name}\"?><x/>");
            assert!(attribute(raw.as_bytes()).is_err(), "{name}");
        }
    }

    #[test]
    fn frozen_complete_encoding_wire_corpus() {
        let oracle: Value = serde_json::from_str(include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../tests/fixtures/plex_xml_encodings.json"
        )))
        .unwrap();
        for case in oracle["wire_cases"].as_array().unwrap() {
            let hex = case["raw_hex"].as_str().unwrap();
            let raw: Vec<u8> = (0..hex.len())
                .step_by(2)
                .map(|i| u8::from_str_radix(&hex[i..i + 2], 16).unwrap())
                .collect();
            if let Some(value) = case["value"].as_str() {
                assert_eq!(attribute(&raw), Ok(value.into()), "{}", case["label"]);
            } else {
                assert!(attribute(&raw).is_err(), "{}", case["label"]);
            }
        }
    }

    #[test]
    fn utf16_byte_orders_boms_declarations_and_unicode() {
        for little in [false, true] {
            for bom in [false, true] {
                for declaration in ["", "UTF-16", if little { "UTF-16LE" } else { "UTF-16BE" }] {
                    let prefix = if declaration.is_empty() {
                        String::new()
                    } else {
                        format!("<?xml version='1.0' encoding='{declaration}'?>")
                    };
                    let raw = utf16(&format!("{prefix}<x a='Café😀 &amp; 漢字'/>"), little, bom);
                    assert_eq!(attribute(&raw), Ok("Café😀 & 漢字".into()));
                }
                for whitespace in [" ", "\t", "\r", "\n"] {
                    assert_eq!(
                        attribute(&utf16(&format!("{whitespace}<x a='ok'/>"), little, bom)),
                        Ok("ok".into())
                    );
                    assert_eq!(
                        attribute(&utf16(
                            &format!(
                                "<?xml{whitespace}version='1.0' encoding='utf-16'?><x a='ok'/>"
                            ),
                            little,
                            bom
                        )),
                        Ok("ok".into())
                    );
                }
                for declaration in [
                    "UTF-8",
                    "UTF-32",
                    "ISO-8859-1",
                    if little { "UTF-16BE" } else { "UTF-16LE" },
                ] {
                    let text = format!("<?xml version='1.0' encoding='{declaration}'?><x/>");
                    assert!(attribute(&utf16(&text, little, bom)).is_err());
                }
            }
        }
    }

    #[test]
    fn utf8_declared_aliases_and_legacy_tables_are_not_interchangeable() {
        for bom in [false, true] {
            for declaration in ["", "UTF-8", "uTf-8"] {
                let mut raw = if bom {
                    vec![0xef, 0xbb, 0xbf]
                } else {
                    Vec::new()
                };
                if !declaration.is_empty() {
                    raw.extend(format!("<?xml version='1.0' encoding='{declaration}'?>").bytes());
                }
                raw.extend("<x a='Café😀'/>".bytes());
                assert_eq!(attribute(&raw), Ok("Café😀".into()));
            }
        }
        assert_eq!(
            attribute(b"<?xml version='1.0' encoding='ISO-8859-1'?><x a='\x80\xe9'/>"),
            Ok("\u{80}é".into())
        );
        assert_eq!(
            attribute(b"<?xml version='1.0' encoding='windows--1252'?><x a='\x80\xe9'/>"),
            Ok("€é".into())
        );
        assert_eq!(
            attribute(b"<?xml version='1.0' encoding='utf8'?><x a='ASCII'/>"),
            Ok("ASCII".into())
        );
        assert!(attribute("<?xml version='1.0' encoding='utf8'?><x a='é'/>".as_bytes()).is_err());
        assert_eq!(
            attribute(b"<?xml version='1.0' encoding='unicode_escape'?><x a='\\u00e9'/>"),
            Ok("\\u00e9".into())
        );
        assert!(attribute(b"<?xml version='1.0' encoding='hz'?><x a='~{abcd~}'/>").is_err());
    }

    #[test]
    fn malformed_truncated_utf16_utf32_and_hidden_declarations_fail_closed() {
        for little in [false, true] {
            for unit in [0xd800u16, 0xdc00, 0xdfff] {
                let mut raw = utf16("<x a='", little, true);
                raw.extend(if little {
                    unit.to_le_bytes()
                } else {
                    unit.to_be_bytes()
                });
                raw.extend(utf16("'/>", little, false));
                assert!(attribute(&raw).is_err());
            }
            let mut raw = utf16("<x/>", little, true);
            raw.pop();
            assert!(attribute(&raw).is_err());
            for xml in [
                "<!DOCTYPE x []><x/>",
                "<!DOCTYPE x [<!ENTITY a 'allowed by frozen UTF16'>]><x a='&a;'/>",
                "<!-- <!ENTITY --> <x/>",
            ] {
                assert!(attribute(&utf16(xml, little, true)).is_err());
            }
        }
        for little in [false, true] {
            for bom in [false, true] {
                let mut raw = Vec::new();
                if bom {
                    raw.extend(if little {
                        [0xff, 0xfe, 0, 0]
                    } else {
                        [0, 0, 0xfe, 0xff]
                    });
                }
                for c in "<?xml version='1.0' encoding='UTF-32'?><x/>".chars() {
                    raw.extend(if little {
                        (c as u32).to_le_bytes()
                    } else {
                        (c as u32).to_be_bytes()
                    });
                }
                assert!(attribute(&raw).is_err());
            }
        }
        for raw in [
            b"\xff".as_slice(),
            b"\xef\xbb",
            b"<x a='\xc0\xaf'/>",
            b"<x a='\xed\xa0\x80'/>",
            b"<?xml version='1.0' encoding='UTF-16'?><x/>",
        ] {
            assert!(attribute(raw).is_err());
        }
    }

    #[test]
    fn reserved_xml_pi_targets_fail_without_rejecting_literal_text() {
        for target in ["xml", "xmL", "xMl", "xML", "Xml", "XmL", "XMl", "XML"] {
            let instruction = format!("<?{target} version='1.0' encoding='UTF-8'?>");
            for text in [
                format!("<x a='ok'>{instruction}</x>"),
                format!("<x a='ok'/>{instruction}"),
                format!("<!-- before -->{instruction}<x a='ok'/>"),
            ] {
                assert!(attribute(text.as_bytes()).is_err(), "{text}");
                for little in [false, true] {
                    assert!(attribute(&utf16(&text, little, true)).is_err(), "{text}");
                }
            }
            if target != "xml" {
                let text = format!("{instruction}<x a='ok'/>");
                assert!(attribute(text.as_bytes()).is_err());
                for little in [false, true] {
                    assert!(attribute(&utf16(&text, little, true)).is_err());
                }
            }
        }
        for text in [
            "<?xml version='1.0'?><x a='ok'/>",
            "<!-- <?XML version='1.0'?> --><x a='ok'/>",
            "<x a='ok'><![CDATA[<?XML version='1.0'?>]]></x>",
            "<?xml-stylesheet href='fixture'?><x a='ok'/>",
            "<x a='ok'/><?XML-stylesheet href='fixture'?>",
        ] {
            assert_eq!(attribute(text.as_bytes()), Ok("ok".into()));
            for little in [false, true] {
                assert_eq!(attribute(&utf16(text, little, true)), Ok("ok".into()));
            }
        }
        assert_eq!(attribute(b"<x a='&lt;?XML'/>"), Ok("<?XML".into()));
    }

    #[test]
    fn raw_decoded_and_node_budgets_apply_before_unbounded_work() {
        assert_eq!(
            attribute(&vec![b' '; MAXIMUM + 1]),
            Err("response_limit".into())
        );
        let text = format!("<x a='{}'/>", "漢".repeat(MAXIMUM / 3));
        let raw = utf16(&text, true, true);
        assert!(raw.len() < MAXIMUM);
        assert_eq!(attribute(&raw), Err("response_limit".into()));
        let mut raw = b"<?xml version='1.0' encoding='windows-1252'?><x a='".to_vec();
        raw.extend(std::iter::repeat_n(0x80, MAXIMUM / 3));
        raw.extend(b"'/>");
        assert!(raw.len() < MAXIMUM);
        assert_eq!(attribute(&raw), Err("response_limit".into()));
        let raw = utf16(&format!("<x>{}</x>", "<y/>".repeat(200_000)), false, true);
        assert_eq!(attribute(&raw), Err("invalid_response".into()));
        // The maximum valid UTF-8 text is accepted exactly at the byte ceiling.
        let raw = format!("<x a='{}'/>", "a".repeat(MAXIMUM - 9));
        assert_eq!(raw.len(), MAXIMUM);
        assert!(attribute(raw.as_bytes()).is_ok());
    }
}
