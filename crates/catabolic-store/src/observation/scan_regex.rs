// SPDX-FileCopyrightText: 2026 The Catabolic Contributors
// SPDX-License-Identifier: MIT
//! Translation of the qualified subset of frozen `regex` VERSION0 scan
//! filters into the pinned fancy-regex syntax.
//!
//! The translator never relies on the engine's own Unicode data or case
//! folding. `\d`, `\w`, `\s` and word boundaries, and every case-insensitive
//! literal and class, are expanded into explicit code point classes from
//! `resources/scan-regex-tables-v1.json`, recorded from the pinned frozen
//! `regex` module by `scripts/generate_scan_regex_tables.py`. The case
//! insensitive flag is therefore never passed to fancy-regex. Literals are
//! emitted as hexadecimal escapes so that no escape changes meaning between
//! dialects (`\<` is a literal in Python and a word boundary in fancy-regex)
//! and no decoded digit can extend a preceding backreference.
//!
//! Everything outside the subset is refused with [`UNQUALIFIED`], never
//! approximated: bare inline flags after the start, contradictory or unknown
//! flags, `(*VERB)`, duplicate group names, capturing groups inside any
//! lookaround, backreferences inside lookbehind or to captures inside a
//! repeated group, a backreference inside a repeated group, assertions
//! inside lookbehind, backreferences to open groups or under
//! case-insensitive matching, multi-digit backreferences, nested or POSIX
//! classes and set operators, possessive quantifiers, and quantifiers that
//! follow an assertion or another quantifier.
use crate::{Error, Result};
use serde_json::Value;
use std::collections::{BTreeMap, HashSet};
use std::fmt::Write as _;
use std::sync::OnceLock;

pub(super) const UNQUALIFIED: &str = "native scan regex construct is not qualified";

const MAX_CODE_POINT: u32 = 0x10FFFF;
const SURROGATES: (u32, u32) = (0xD800, 0xDFFF);

type Set = Vec<(u32, u32)>;

struct Tables {
    word: Set,
    digit: Set,
    space: Set,
    fold: BTreeMap<u32, Vec<u32>>,
}

fn tables() -> &'static Tables {
    static TABLES: OnceLock<Tables> = OnceLock::new();
    TABLES.get_or_init(|| {
        let document: Value =
            serde_json::from_str(include_str!("../../resources/scan-regex-tables-v1.json"))
                .expect("embedded scan regex tables are valid JSON");
        assert_eq!(document["contract"], "native-scan-regex-tables-v1");
        let set = |name: &str| -> Set {
            document[name]
                .as_array()
                .expect("range table")
                .iter()
                .map(|pair| {
                    (
                        pair[0].as_u64().expect("range start") as u32,
                        pair[1].as_u64().expect("range end") as u32,
                    )
                })
                .collect()
        };
        let fold = document["fold"]
            .as_object()
            .expect("fold table")
            .iter()
            .map(|(key, values)| {
                (
                    key.parse().expect("fold key"),
                    values
                        .as_array()
                        .expect("fold values")
                        .iter()
                        .map(|v| v.as_u64().expect("fold value") as u32)
                        .collect(),
                )
            })
            .collect();
        Tables {
            word: set("word"),
            digit: set("digit"),
            space: set("space"),
            fold,
        }
    })
}

fn normalize(mut set: Set) -> Set {
    set.sort_unstable();
    let mut merged: Set = Vec::with_capacity(set.len());
    for (start, end) in set {
        match merged.last_mut() {
            Some(last) if start <= last.1.saturating_add(1) => last.1 = last.1.max(end),
            _ => merged.push((start, end)),
        }
    }
    // Surrogates are not characters; regex classes cannot hold them.
    let mut out = Vec::with_capacity(merged.len());
    for (start, end) in merged {
        if end < SURROGATES.0 || start > SURROGATES.1 {
            out.push((start, end));
            continue;
        }
        if start < SURROGATES.0 {
            out.push((start, SURROGATES.0 - 1));
        }
        if end > SURROGATES.1 {
            out.push((SURROGATES.1 + 1, end));
        }
    }
    out
}

fn complement(set: &Set) -> Set {
    let mut out = Vec::new();
    let mut next = 0u32;
    for &(start, end) in set {
        if start > next {
            out.push((next, start - 1));
        }
        next = end.saturating_add(1);
    }
    if next <= MAX_CODE_POINT {
        out.push((next, MAX_CODE_POINT));
    }
    normalize(out)
}

fn contains(set: &Set, cp: u32) -> bool {
    set.binary_search_by(|&(start, end)| {
        if end < cp {
            std::cmp::Ordering::Less
        } else if start > cp {
            std::cmp::Ordering::Greater
        } else {
            std::cmp::Ordering::Equal
        }
    })
    .is_ok()
}

/// Frozen case-insensitive class membership: `x` matches when some member
/// `c` of the class matches `x` case-insensitively, per the frozen table.
fn fold_closure(set: Set) -> Set {
    let mut extra = Vec::new();
    for (&cp, equivalents) in &tables().fold {
        if contains(&set, cp) {
            extra.extend(equivalents.iter().map(|&e| (e, e)));
        }
    }
    let mut set = set;
    set.extend(extra);
    normalize(set)
}

fn emit_set(out: &mut String, set: &Set) -> Result<()> {
    if set.is_empty() {
        return Err(Error(UNQUALIFIED.into()));
    }
    out.push('[');
    for &(start, end) in set {
        let _ = write!(out, "\\x{{{start:X}}}");
        if end != start {
            let _ = write!(out, "-\\x{{{end:X}}}");
        }
    }
    out.push(']');
    Ok(())
}

fn emit_char(out: &mut String, character: char, insensitive: bool) -> Result<()> {
    let cp = character as u32;
    if insensitive && tables().fold.contains_key(&cp) {
        return emit_set(out, &fold_closure(vec![(cp, cp)]));
    }
    if character.is_ascii_alphabetic() {
        out.push(character);
    } else {
        let _ = write!(out, "\\x{{{cp:X}}}");
    }
    Ok(())
}

/// The class escapes, as code point sets.
fn class_escape(letter: char) -> Option<Set> {
    let t = tables();
    Some(match letter {
        'd' => t.digit.clone(),
        'D' => complement(&t.digit),
        'w' => t.word.clone(),
        'W' => complement(&t.word),
        's' => t.space.clone(),
        'S' => complement(&t.space),
        _ => return None,
    })
}

/// A single character written as an escape, valid inside and outside a class.
fn char_escape(chars: &[char], i: &mut usize) -> Result<Option<char>> {
    let refuse = || Error(UNQUALIFIED.into());
    let next = *chars.get(*i + 1).ok_or_else(refuse)?;
    let simple = match next {
        't' => Some('\t'),
        'n' => Some('\n'),
        'r' => Some('\r'),
        'f' => Some('\x0c'),
        'v' => Some('\x0b'),
        _ => None,
    };
    if let Some(character) = simple {
        *i += 2;
        return Ok(Some(character));
    }
    let count = match next {
        'x' => 2,
        'u' => 4,
        'U' => 8,
        c if c.is_ascii() && !c.is_ascii_alphanumeric() && !c.is_ascii_whitespace() => {
            *i += 2;
            return Ok(Some(c));
        }
        _ => return Ok(None),
    };
    let digits: String = chars.iter().skip(*i + 2).take(count).collect();
    if digits.chars().count() != count || !digits.chars().all(|c| c.is_ascii_hexdigit()) {
        return Err(refuse());
    }
    let character = u32::from_str_radix(&digits, 16)
        .ok()
        .and_then(char::from_u32)
        .ok_or_else(refuse)?;
    *i += 2 + count;
    Ok(Some(character))
}

#[derive(Clone, Copy, PartialEq)]
enum Last {
    /// Start of the pattern, a group or an alternative: nothing to repeat.
    Nothing,
    Atom,
    Quantified,
    Assertion,
}

struct Frame {
    insensitive: bool,
    lookbehind: bool,
    /// Inside any lookaround: frozen commits captures made there, while
    /// fancy-regex can backtrack into them, so captures are refused.
    lookaround: bool,
    capture: Option<usize>,
    assertion: bool,
    /// Captures opened inside this group, including its own.
    captures: Vec<usize>,
    /// Whether a backreference appears inside this group.
    backreference: bool,
}

struct Translator<'a> {
    chars: &'a [char],
    i: usize,
    out: String,
    /// Case-insensitivity set by the leading global flags.
    global: bool,
    insensitive: bool,
    lookbehind: bool,
    lookaround: bool,
    stack: Vec<Frame>,
    names: std::collections::HashMap<String, usize>,
    groups: usize,
    closed: HashSet<usize>,
    /// Captures inside a repeated group: fancy-regex simplifies nested
    /// repetition (`(a+)+`), so references to them are refused.
    repeated: HashSet<usize>,
    /// The group that just closed: (index after its `)`, its captures,
    /// whether it holds a backreference).
    closed_at: Option<(usize, Vec<usize>, bool)>,
    /// Minimum widths, checked: per open sequence (current alternative,
    /// narrowest completed alternative), the last atom, and each capture.
    widths: Vec<(u64, Option<u64>)>,
    last_width: u64,
    /// Composed repeat counts: per open sequence the largest product of
    /// nested repeat counts inside it, and that of the last atom.
    costs: Vec<u64>,
    last_cost: u64,
    capture_widths: std::collections::HashMap<usize, u64>,
    last: Last,
}

/// Any pattern whose minimum match is wider than this can never match a scan
/// path; refusing it keeps the engine's own width arithmetic from overflowing.
const MAX_MIN_WIDTH: u64 = 1 << 16;
/// Largest number of engine iterations per consumed character. Iterations of
/// a body that consumes input are bounded by the path length at any nesting
/// depth; only repeats of a body that can match empty multiply work, so their
/// explicit counts are composed and bounded by this.
const MAX_ZERO_WIDTH_ITERATIONS: u64 = 1_000;

/// Parse inline flags at `self.i` (just after `(?`): returns (on, off, close).
fn flags(chars: &[char], start: usize) -> Result<(String, String, char, usize)> {
    let refuse = || Error(UNQUALIFIED.into());
    let (mut on, mut off) = (String::new(), String::new());
    let mut j = start;
    let mut negative = false;
    while let Some(&f) = chars.get(j) {
        match f {
            'i' | 's' => {
                let target = if negative { &mut off } else { &mut on };
                if target.contains(f) {
                    return Err(refuse());
                }
                target.push(f);
            }
            '-' if !negative => negative = true,
            _ => break,
        }
        j += 1;
    }
    let close = *chars.get(j).ok_or_else(refuse)?;
    if (on.is_empty() && off.is_empty())
        || (negative && off.is_empty())
        || on.chars().any(|f| off.contains(f))
        || !matches!(close, ')' | ':')
    {
        return Err(refuse());
    }
    Ok((on, off, close, j + 1))
}

impl Translator<'_> {
    fn refuse<T>(&self) -> Result<T> {
        Err(Error(UNQUALIFIED.into()))
    }

    fn quantifiable(&mut self) -> Result<()> {
        if self.last != Last::Atom {
            return self.refuse();
        }
        // A repeated group: its captures cannot be referenced afterwards, and
        // a backreference inside it is refused (repetition semantics differ).
        if let Some((end, captures, backreference)) = self.closed_at.take()
            && end == self.i
        {
            if backreference {
                return self.refuse();
            }
            self.repeated.extend(captures);
        }
        Ok(())
    }

    fn add_width(&mut self, width: u64) -> Result<()> {
        let top = self.widths.last_mut().expect("root sequence");
        top.0 = top
            .0
            .checked_add(width)
            .filter(|w| *w <= MAX_MIN_WIDTH)
            .ok_or_else(|| Error(UNQUALIFIED.into()))?;
        self.last_width = width;
        Ok(())
    }

    fn atom(&mut self, width: u64) -> Result<()> {
        self.atom_with_cost(width, 1)
    }

    fn atom_with_cost(&mut self, width: u64, cost: u64) -> Result<()> {
        self.add_width(width)?;
        self.last_cost = cost;
        let top = self.costs.last_mut().expect("root sequence");
        *top = (*top).max(cost);
        self.last = Last::Atom;
        Ok(())
    }

    /// Repeat the last atom at least `low` times in the minimum width.
    /// Repeat the last atom up to `count` times (`None`: unbounded). Only a
    /// body that can match empty multiplies the per-character iteration cost;
    /// an unbounded repeat of it stops at its first empty iteration.
    /// Call before `repeat_width`, while `last_width` is the body's width.
    fn repeat_cost(&mut self, count: Option<u64>) -> Result<()> {
        let factor = if self.last_width == 0 {
            count.unwrap_or(1).max(1)
        } else {
            1
        };
        let cost = self
            .last_cost
            .checked_mul(factor)
            .filter(|c| *c <= MAX_ZERO_WIDTH_ITERATIONS)
            .ok_or_else(|| Error(UNQUALIFIED.into()))?;
        self.last_cost = cost;
        let top = self.costs.last_mut().expect("root sequence");
        *top = (*top).max(cost);
        Ok(())
    }

    fn repeat_width(&mut self, low: u64) -> Result<()> {
        let width = self.last_width;
        let top = self.widths.last_mut().expect("root sequence");
        top.0 = width
            .checked_mul(low)
            .and_then(|repeated| (top.0 - width).checked_add(repeated))
            .filter(|w| *w <= MAX_MIN_WIDTH)
            .ok_or_else(|| Error(UNQUALIFIED.into()))?;
        self.last_width = width.saturating_mul(low);
        Ok(())
    }

    /// Assertions inside a lookbehind: fancy-regex's reverse search cannot
    /// backtrack into the variable-width body the way the frozen engine does.
    fn assertion_allowed(&self) -> Result<()> {
        if self.lookbehind {
            return self.refuse();
        }
        Ok(())
    }

    /// Optional lazy marker after a quantifier; possessive forms are refused.
    fn after_quantifier(&mut self) -> Result<()> {
        match self.chars.get(self.i) {
            Some('?') => {
                self.out.push('?');
                self.i += 1;
            }
            Some('+') => return self.refuse(),
            _ => {}
        }
        self.last = Last::Quantified;
        Ok(())
    }

    fn word_boundary(&mut self, boundary: bool) -> Result<()> {
        self.assertion_allowed()?;
        let mut word = String::new();
        emit_set(&mut word, &tables().word)?;
        let text = if boundary {
            format!("(?:(?<={word})(?!{word})|(?<!{word})(?={word}))")
        } else {
            format!("(?:(?<={word})(?={word})|(?<!{word})(?!{word}))")
        };
        self.out.push_str(&text);
        self.last = Last::Assertion;
        Ok(())
    }

    fn escape(&mut self) -> Result<()> {
        let next = *self
            .chars
            .get(self.i + 1)
            .ok_or_else(|| Error(UNQUALIFIED.into()))?;
        if let Some(set) = class_escape(next) {
            let set = if self.insensitive {
                fold_closure(set)
            } else {
                set
            };
            emit_set(&mut self.out, &set)?;
            self.i += 2;
            self.atom(1)?;
            return Ok(());
        }
        match next {
            'b' | 'B' => {
                self.i += 2;
                return self.word_boundary(next == 'b');
            }
            'A' => {
                self.assertion_allowed()?;
                self.out.push_str("\\A");
                self.i += 2;
                self.last = Last::Assertion;
                return Ok(());
            }
            'Z' => {
                self.assertion_allowed()?;
                // Python's \Z is strict; fancy's \Z also accepts a final newline.
                self.out.push_str("\\z");
                self.i += 2;
                self.last = Last::Assertion;
                return Ok(());
            }
            '1'..='9' => {
                // Multi-digit forms are ambiguous octal or group references.
                if self.chars.get(self.i + 2).is_some_and(char::is_ascii_digit) {
                    return self.refuse();
                }
                let group = next.to_digit(10).unwrap() as usize;
                return self.backreference(group, self.i + 2);
            }
            _ => {}
        }
        let mut i = self.i;
        match char_escape(self.chars, &mut i)? {
            Some(character) => {
                emit_char(&mut self.out, character, self.insensitive)?;
                self.i = i;
                self.atom(1)?;
                Ok(())
            }
            None => self.refuse(),
        }
    }

    /// A backreference to a closed group, outside lookbehind and outside
    /// case-insensitive matching; `next` is the index after its syntax.
    fn backreference(&mut self, group: usize, next: usize) -> Result<()> {
        if !self.closed.contains(&group)
            || self.repeated.contains(&group)
            || self.lookbehind
            || self.insensitive
        {
            return self.refuse();
        }
        for frame in &mut self.stack {
            frame.backreference = true;
        }
        // Wrapped so a following digit cannot extend the number.
        let _ = write!(self.out, "(?:\\{group})");
        self.i = next;
        let width = self.capture_widths.get(&group).copied().unwrap_or(0);
        self.atom(width)
    }

    fn class(&mut self) -> Result<()> {
        self.i += 1;
        let negated = self.chars.get(self.i) == Some(&'^');
        if negated {
            self.i += 1;
        }
        let mut set: Set = Vec::new();
        let mut first = true;
        loop {
            let c = *self
                .chars
                .get(self.i)
                .ok_or_else(|| Error(UNQUALIFIED.into()))?;
            if c == ']' && !first {
                self.i += 1;
                break;
            }
            first = false;
            if c == '[' {
                return self.refuse();
            }
            if matches!(c, '&' | '-' | '~' | '|') && self.chars.get(self.i + 1) == Some(&c) {
                return self.refuse();
            }
            // One item: a character (possibly a range start) or a class escape.
            let low = if c == '\\' {
                let next = *self
                    .chars
                    .get(self.i + 1)
                    .ok_or_else(|| Error(UNQUALIFIED.into()))?;
                if let Some(escaped) = class_escape(next) {
                    set.extend(escaped);
                    self.i += 2;
                    continue;
                }
                let mut i = self.i;
                let character = char_escape(self.chars, &mut i)?;
                self.i = i;
                match character {
                    Some(character) => character,
                    None => return self.refuse(),
                }
            } else {
                self.i += 1;
                c
            };
            let range = self.chars.get(self.i) == Some(&'-')
                && self.chars.get(self.i + 1).is_some_and(|c| *c != ']');
            if !range {
                set.push((low as u32, low as u32));
                continue;
            }
            self.i += 1;
            let high = match self.chars.get(self.i) {
                Some('\\') => {
                    let mut i = self.i;
                    let character = char_escape(self.chars, &mut i)?;
                    self.i = i;
                    match character {
                        Some(character) => character,
                        None => return self.refuse(),
                    }
                }
                Some('[') | None => return self.refuse(),
                Some(&c) => {
                    self.i += 1;
                    c
                }
            };
            if (low as u32) > (high as u32) {
                return self.refuse();
            }
            set.push((low as u32, high as u32));
        }
        let mut set = normalize(set);
        if self.insensitive {
            set = fold_closure(set);
        }
        if negated {
            set = complement(&set);
        }
        emit_set(&mut self.out, &set)?;
        self.atom(1)?;
        Ok(())
    }

    fn open_group(&mut self) -> Result<()> {
        let chars = self.chars;
        let mut frame = Frame {
            insensitive: self.insensitive,
            lookbehind: self.lookbehind,
            lookaround: self.lookaround,
            capture: None,
            assertion: false,
            captures: Vec::new(),
            backreference: false,
        };
        if chars.get(self.i + 1) == Some(&'*') {
            return self.refuse();
        }
        if chars.get(self.i + 1) != Some(&'?') {
            if self.lookaround {
                return self.refuse();
            }
            self.groups += 1;
            frame.capture = Some(self.groups);
            frame.captures.push(self.groups);
            for open in &mut self.stack {
                open.captures.push(self.groups);
            }
            self.out.push('(');
            self.i += 1;
        } else {
            let rest: String = chars[self.i + 2..].iter().take(2).collect();
            if rest == "P=" {
                // Named backreference: the same rules as a numbered one.
                let start = self.i + 4;
                let end = chars[start..]
                    .iter()
                    .position(|c| *c == ')')
                    .map(|p| start + p)
                    .ok_or_else(|| Error(UNQUALIFIED.into()))?;
                let name: String = chars[start..end].iter().collect();
                let group = *self
                    .names
                    .get(&name)
                    .ok_or_else(|| Error(UNQUALIFIED.into()))?;
                return self.backreference(group, end + 1);
            }
            if rest.starts_with(':') || rest.starts_with('>') {
                self.out
                    .push_str(if rest.starts_with(':') { "(?:" } else { "(?>" });
                self.i += 3;
            } else if rest.starts_with('=') || rest.starts_with('!') {
                self.assertion_allowed()?;
                self.out.push_str(&format!("(?{}", &rest[..1]));
                frame.assertion = true;
                frame.lookaround = true;
                self.i += 3;
            } else if rest == "<=" || rest == "<!" {
                self.assertion_allowed()?;
                self.out.push_str(&format!("(?{rest}"));
                frame.assertion = true;
                frame.lookbehind = true;
                frame.lookaround = true;
                self.i += 4;
            } else if rest.starts_with("P<") || rest.starts_with('<') {
                if self.lookaround {
                    return self.refuse();
                }
                let start = self.i + if rest.starts_with('P') { 4 } else { 3 };
                let end = chars[start..]
                    .iter()
                    .position(|c| *c == '>')
                    .map(|p| start + p)
                    .ok_or_else(|| Error(UNQUALIFIED.into()))?;
                let name: String = chars[start..end].iter().collect();
                if name.is_empty()
                    || !name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
                    || name.starts_with(|c: char| c.is_ascii_digit())
                    || self.names.contains_key(&name)
                {
                    return self.refuse();
                }
                self.groups += 1;
                self.names.insert(name.clone(), self.groups);
                frame.capture = Some(self.groups);
                frame.captures.push(self.groups);
                for open in &mut self.stack {
                    open.captures.push(self.groups);
                }
                self.out.push_str(&format!("(?P<{name}>"));
                self.i = end + 1;
            } else {
                // Scoped flags only; bare flags after the start are refused.
                let (on, off, close, next) = flags(chars, self.i + 2)?;
                if close != ':' {
                    return self.refuse();
                }
                if on.contains('i') {
                    frame.insensitive = true;
                }
                if off.contains('i') {
                    frame.insensitive = false;
                }
                let dotall = match (on.contains('s'), off.contains('s')) {
                    (true, _) => "s",
                    (_, true) => "-s",
                    _ => "",
                };
                self.out.push_str(&format!("(?{dotall}:"));
                self.i = next;
            }
        }
        self.insensitive = frame.insensitive;
        self.lookbehind = frame.lookbehind;
        self.lookaround = frame.lookaround;
        self.stack.push(frame);
        self.widths.push((0, None));
        self.costs.push(1);
        self.last = Last::Nothing;
        Ok(())
    }

    fn close_group(&mut self) -> Result<()> {
        let Some(frame) = self.stack.pop() else {
            return self.refuse();
        };
        self.out.push(')');
        self.i += 1;
        if let Some(group) = frame.capture {
            self.closed.insert(group);
        }
        self.closed_at = Some((self.i, frame.captures.clone(), frame.backreference));
        let (insensitive, lookbehind, lookaround) =
            self.stack.last().map_or((self.global, false, false), |f| {
                (f.insensitive, f.lookbehind, f.lookaround)
            });
        self.insensitive = insensitive;
        self.lookbehind = lookbehind;
        self.lookaround = lookaround;
        let (sequence, narrowest) = self.widths.pop().expect("group sequence");
        let cost = self.costs.pop().expect("group sequence");
        let width = narrowest.map_or(sequence, |n| n.min(sequence));
        if let Some(group) = frame.capture {
            self.capture_widths.insert(group, width);
        }
        if frame.assertion {
            self.last_width = 0;
            self.last = Last::Assertion;
            let top = self.costs.last_mut().expect("root sequence");
            *top = (*top).max(cost);
        } else {
            self.atom_with_cost(width, cost)?;
        }
        Ok(())
    }
}

/// Translate a frozen VERSION0 scan pattern, or refuse it (see module docs).
pub(super) fn qualify_pattern(pattern: &str) -> Result<String> {
    let chars: Vec<char> = pattern.chars().collect();
    let refuse = || Error(UNQUALIFIED.into());
    let mut translator = Translator {
        chars: &chars,
        i: 0,
        out: String::with_capacity(pattern.len() + 16),
        global: false,
        insensitive: false,
        lookbehind: false,
        lookaround: false,
        stack: Vec::new(),
        names: std::collections::HashMap::new(),
        groups: 0,
        closed: HashSet::new(),
        repeated: HashSet::new(),
        closed_at: None,
        widths: vec![(0, None)],
        last_width: 0,
        costs: vec![1],
        last_cost: 1,
        capture_widths: std::collections::HashMap::new(),
        last: Last::Nothing,
    };
    // Leading global flags, in one or more groups at the very start.
    let mut global_insensitive = false;
    let mut global_dotall = false;
    while chars.get(translator.i) == Some(&'(') && chars.get(translator.i + 1) == Some(&'?') {
        let Ok((on, off, close, next)) = flags(&chars, translator.i + 2) else {
            break;
        };
        if close != ')' {
            break;
        }
        global_insensitive = (global_insensitive || on.contains('i')) && !off.contains('i');
        global_dotall = (global_dotall || on.contains('s')) && !off.contains('s');
        translator.i = next;
    }
    if global_dotall {
        translator.out.push_str("(?s)");
    }
    translator.global = global_insensitive;
    translator.insensitive = global_insensitive;
    while translator.i < chars.len() {
        let c = chars[translator.i];
        match c {
            '\\' => translator.escape()?,
            '[' => translator.class()?,
            '(' => translator.open_group()?,
            ')' => translator.close_group()?,
            '|' => {
                translator.out.push('|');
                translator.i += 1;
                let top = translator.widths.last_mut().expect("root sequence");
                top.1 = Some(top.1.map_or(top.0, |n| n.min(top.0)));
                top.0 = 0;
                translator.last = Last::Nothing;
            }
            '^' => {
                translator.assertion_allowed()?;
                translator.out.push('^');
                translator.i += 1;
                translator.last = Last::Assertion;
            }
            '$' => {
                translator.assertion_allowed()?;
                // Python: end of string, or before a final newline.
                translator.out.push_str("(?=\\n?\\z)");
                translator.i += 1;
                translator.last = Last::Assertion;
            }
            '.' => {
                translator.out.push('.');
                translator.i += 1;
                translator.atom(1)?;
            }
            '*' | '+' | '?' => {
                translator.quantifiable()?;
                translator.out.push(c);
                translator.i += 1;
                translator.repeat_cost(if c == '?' { Some(1) } else { None })?;
                translator.repeat_width(u64::from(c == '+'))?;
                translator.after_quantifier()?;
            }
            '{' => {
                translator.quantifiable()?;
                let end = chars[translator.i..]
                    .iter()
                    .position(|c| *c == '}')
                    .map(|p| translator.i + p)
                    .ok_or_else(refuse)?;
                let body: String = chars[translator.i + 1..end].iter().collect();
                let (low, high) = match body.split_once(',') {
                    Some((low, high)) => (low, Some(high)),
                    None => (body.as_str(), None),
                };
                let numeric = |s: &str| !s.is_empty() && s.chars().all(|c| c.is_ascii_digit());
                if !(numeric(low) || (low.is_empty() && high.is_some_and(numeric)))
                    || high.is_some_and(|h| !h.is_empty() && !numeric(h))
                {
                    return Err(refuse());
                }
                let bound = |s: &str| s.parse::<u32>().map_err(|_| refuse());
                if !low.is_empty() {
                    bound(low)?;
                }
                if let Some(high) = high.filter(|h| !h.is_empty()) {
                    if !low.is_empty() && bound(low)? > bound(high)? {
                        return Err(refuse());
                    }
                    bound(high)?;
                }
                translator.out.push('{');
                translator
                    .out
                    .push_str(if low.is_empty() { "0" } else { low });
                if let Some(high) = high {
                    translator.out.push(',');
                    translator.out.push_str(high);
                }
                translator.out.push('}');
                translator.i = end + 1;
                let low_count = if low.is_empty() { 0 } else { bound(low)? };
                let high_count = match high {
                    Some(h) if !h.is_empty() => Some(u64::from(bound(h)?)),
                    Some(_) => None,
                    None => Some(u64::from(low_count)),
                };
                translator.repeat_cost(high_count)?;
                translator.repeat_width(u64::from(low_count))?;
                translator.after_quantifier()?;
            }
            '}' | ']' => return Err(refuse()),
            _ => {
                emit_char(&mut translator.out, c, translator.insensitive)?;
                translator.i += 1;
                translator.atom(1)?;
            }
        }
    }
    if !translator.stack.is_empty() {
        return Err(refuse());
    }
    Ok(translator.out)
}
