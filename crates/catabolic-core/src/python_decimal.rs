// SPDX-FileCopyrightText: 2026 The Catabolic Contributors
// SPDX-License-Identifier: MIT
//! Shared frozen decimal digit classification; this does not parse numbers.

// CPython 3.14.6's frozen Unicode 16.0 decimal digits. Every Nd block
// contains ten sequential values; adjacent mathematical blocks are distinct.
// CPython 3.11/3.12 use older Unicode tables and reject subsequently added Nd
// blocks. Compatibility spellings follow the frozen 3.14 oracle, not host Rust UCD.
const PYTHON_DECIMAL_ZEROES: &[u32] = &[
    0x30, 0x660, 0x6f0, 0x7c0, 0x966, 0x9e6, 0xa66, 0xae6, 0xb66, 0xbe6, 0xc66, 0xce6, 0xd66,
    0xde6, 0xe50, 0xed0, 0xf20, 0x1040, 0x1090, 0x17e0, 0x1810, 0x1946, 0x19d0, 0x1a80, 0x1a90,
    0x1b50, 0x1bb0, 0x1c40, 0x1c50, 0xa620, 0xa8d0, 0xa900, 0xa9d0, 0xa9f0, 0xaa50, 0xabf0, 0xff10,
    0x104a0, 0x10d30, 0x10d40, 0x11066, 0x110f0, 0x11136, 0x111d0, 0x112f0, 0x11450, 0x114d0,
    0x11650, 0x116c0, 0x116d0, 0x116da, 0x11730, 0x118e0, 0x11950, 0x11bf0, 0x11c50, 0x11d50,
    0x11da0, 0x11f50, 0x16130, 0x16a60, 0x16ac0, 0x16b50, 0x16d70, 0x1ccf0, 0x1d7ce, 0x1d7d8,
    0x1d7e2, 0x1d7ec, 0x1d7f6, 0x1e140, 0x1e2f0, 0x1e4f0, 0x1e5f1, 0x1e950, 0x1fbf0,
];
/// Decimal value in the frozen CPython 3.14.6 Unicode 16.0 Nd table.
/// Other numeric categories (superscripts, circled digits) are not decimal digits.
pub fn decimal_digit(character: char) -> Option<u8> {
    let number = character as u32;
    let end = PYTHON_DECIMAL_ZEROES.partition_point(|zero| *zero <= number);
    let difference = number.checked_sub(*PYTHON_DECIMAL_ZEROES.get(end.checked_sub(1)?)?)?;
    (difference < 10).then_some(difference as u8)
}

#[cfg(test)]
mod tests {
    use super::decimal_digit;

    #[test]
    fn shared_classifier_matches_every_frozen_cli_decimal_digit() {
        let frozen: serde_json::Value = serde_json::from_str(include_str!(
            "../../catabolic-cli/resources/legacy-cli.json"
        ))
        .unwrap();
        let digits = frozen["decimal_digits"].as_object().unwrap();
        let mut found = 0;
        for character in (0..=0x10ffff).filter_map(char::from_u32) {
            let expected = digits
                .get(&character.to_string())
                .map(|value| value.as_str().unwrap().parse::<u8>().unwrap());
            assert_eq!(decimal_digit(character), expected, "{character:?}");
            found += usize::from(expected.is_some());
        }
        assert_eq!(found, digits.len());
    }
}
