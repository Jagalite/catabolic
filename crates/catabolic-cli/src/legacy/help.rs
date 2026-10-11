// SPDX-FileCopyrightText: 2026 The Catabolic Contributors
// SPDX-License-Identifier: MIT
//! Native formatter for frozen argparse presentation tokens. Development-time
//! tokens retain word/hyphen boundaries; line wrapping follows runtime COLUMNS.
use serde_json::Value;
use std::collections::VecDeque;
fn text<'a>(v: &'a Value, k: &str) -> &'a str {
    v[k].as_str().unwrap_or("")
}
fn width() -> usize {
    std::env::var("COLUMNS")
        .ok()
        .and_then(|v| v.parse::<usize>().ok())
        .filter(|v| *v > 0)
        .unwrap_or(80)
        .saturating_sub(2)
        .max(1)
}
fn len(s: &str) -> usize {
    s.chars().count()
}
fn wrapped(chunks: &Value, width: usize) -> Vec<String> {
    let mut chunks = chunks
        .as_array()
        .unwrap()
        .iter()
        .filter_map(Value::as_str)
        .map(str::to_owned)
        .collect::<VecDeque<_>>();
    let mut lines = vec![];
    while !chunks.is_empty() {
        if !lines.is_empty() && chunks.front().is_some_and(|s| s.trim().is_empty()) {
            chunks.pop_front();
        }
        let mut line = String::new();
        while chunks.front().is_some_and(|s| len(&line) + len(s) <= width) {
            line.push_str(&chunks.pop_front().unwrap());
        }
        if let Some(mut next) = chunks.pop_front_if(|s| len(s) > width) {
            let space = width
                .saturating_sub(len(&line))
                .max(usize::from(line.is_empty()));
            let head = next.chars().take(space).collect::<String>();
            next = next.chars().skip(space).collect();
            line.push_str(&head);
            if !next.is_empty() {
                chunks.push_front(next);
            }
        }
        let line = line.trim_end().to_owned();
        if !line.is_empty() {
            lines.push(line);
        }
    }
    lines
}
fn strings(value: &Value) -> Vec<String> {
    value
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_str().unwrap().to_owned())
        .collect()
}
fn get_lines(parts: &[String], indent: usize, prefix: bool, width: usize) -> Vec<String> {
    let mut lines = vec![];
    let mut line = vec![];
    let mut current = if prefix { 6 } else { indent.saturating_sub(1) };
    for part in parts {
        if current + 1 + len(part) > width && !line.is_empty() {
            lines.push(format!("{}{}", " ".repeat(indent), line.join(" ")));
            line.clear();
            current = indent.saturating_sub(1);
        }
        line.push(part.clone());
        current += len(part) + 1;
    }
    if !line.is_empty() {
        lines.push(format!("{}{}", " ".repeat(indent), line.join(" ")));
    }
    if prefix && !lines.is_empty() {
        lines[0] = lines[0].chars().skip(indent).collect();
    }
    lines
}
pub fn usage(node: &Value) -> String {
    let width = width();
    let prog = text(node, "prog");
    let parts = strings(&node["usage_parts"]);
    let start = node["usage_positionals_start"].as_u64().unwrap() as usize;
    let full = std::iter::once(prog.to_owned())
        .chain(parts.iter().cloned())
        .filter(|s| !s.is_empty())
        .collect::<Vec<_>>()
        .join(" ");
    if len(&full) + 7 <= width {
        return format!("usage: {full}\n");
    }
    let (opt, pos) = parts.split_at(start);
    let lines = if (7 + len(prog)) as f64 <= 0.75 * width as f64 {
        let indent = 7 + len(prog) + 1;
        if !opt.is_empty() {
            let mut first = vec![prog.into()];
            first.extend_from_slice(opt);
            let mut lines = get_lines(&first, indent, true, width);
            lines.extend(get_lines(pos, indent, false, width));
            lines
        } else if !pos.is_empty() {
            let mut first = vec![prog.into()];
            first.extend_from_slice(pos);
            get_lines(&first, indent, true, width)
        } else {
            vec![prog.into()]
        }
    } else {
        let mut lines = get_lines(&parts, 7, false, width);
        if lines.len() > 1 {
            lines = get_lines(opt, 7, false, width);
            lines.extend(get_lines(pos, 7, false, width));
        }
        let mut all = vec![prog.into()];
        all.extend(lines);
        all
    };
    format!("usage: {}\n", lines.join("\n"))
}
pub fn help(node: &Value) -> String {
    let width = width();
    let mut out = usage(node);
    out.push('\n');
    let description = wrapped(&node["description_chunks"], width.max(11));
    if !description.is_empty() {
        out.push_str(&description.join("\n"));
        out.push_str("\n\n");
    }
    let maximum = node["action_max_length"].as_u64().unwrap() as usize;
    let help_position = (maximum + 2).min(24.min(width.saturating_sub(20).max(4)));
    for section in node["help_sections"].as_array().unwrap() {
        out.push_str(text(section, "title"));
        out.push_str(":\n");
        let desc = wrapped(
            &section["description_chunks"],
            width.saturating_sub(2).max(11),
        );
        if !desc.is_empty() {
            out.push_str(&format!("  {}\n\n", desc.join("\n  ")));
        }
        for entry in section["entries"].as_array().unwrap() {
            let indent = entry["indent"].as_u64().unwrap() as usize;
            let invocation = text(entry, "invocation");
            let chunks = &entry["chunks"];
            if chunks.as_array().unwrap().is_empty() {
                out.push_str(&format!("{}{invocation}\n", " ".repeat(indent)));
                continue;
            }
            let action_width = help_position.saturating_sub(indent + 2);
            let lines = wrapped(chunks, width.saturating_sub(help_position).max(11));
            if len(invocation) <= action_width {
                out.push_str(&format!(
                    "{}{invocation}{}  {}\n",
                    " ".repeat(indent),
                    " ".repeat(action_width - len(invocation)),
                    lines[0]
                ));
            } else {
                out.push_str(&format!(
                    "{}{invocation}\n{}{}\n",
                    " ".repeat(indent),
                    " ".repeat(help_position),
                    lines[0]
                ));
            }
            for line in &lines[1..] {
                out.push_str(&format!("{}{line}\n", " ".repeat(help_position)));
            }
        }
        out.push('\n');
    }
    format!("{}\n", out.trim_end())
}
