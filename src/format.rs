use tower_lsp::lsp_types::{Position, Range, TextEdit};

use crate::document::Document;

pub fn compute_formatting(doc: &Document) -> Option<Vec<TextEdit>> {
    let formatted = format_source(&doc.text);
    if formatted == doc.text {
        return None;
    }

    let end_pos = doc.byte_offset_to_position(doc.text.len());
    Some(vec![TextEdit {
        range: Range {
            start: Position { line: 0, character: 0 },
            end: end_pos,
        },
        new_text: formatted,
    }])
}

pub fn format_source(text: &str) -> String {
    let mut result = String::with_capacity(text.len());
    let mut indent_level: usize = 0;
    let mut prev_blank = false;

    for raw_line in text.lines() {
        let trimmed = raw_line.trim();

        if trimmed.is_empty() {
            if !prev_blank && !result.is_empty() {
                result.push('\n');
                prev_blank = true;
            }
            continue;
        }
        prev_blank = false;

        let closes = count_leading_closers(trimmed);
        let active_indent = indent_level.saturating_sub(closes);

        for _ in 0..active_indent {
            result.push_str("    ");
        }
        result.push_str(trimmed);
        result.push('\n');

        let opens = count_net_opens(trimmed);
        indent_level = (indent_level as i32 + opens).max(0) as usize;
    }

    result
}

fn count_leading_closers(line: &str) -> usize {
    let mut count = 0;
    for ch in line.chars() {
        match ch {
            '}' | ']' | ')' => count += 1,
            ' ' | '\t' => continue,
            _ => break,
        }
    }
    count
}

fn count_net_opens(line: &str) -> i32 {
    let mut net = 0;
    let mut in_string = false;
    let mut in_char = false;
    let mut prev_char = ' ';

    for ch in line.chars() {
        if ch == '"' && prev_char != '\\' && !in_char {
            in_string = !in_string;
        } else if ch == '\'' && prev_char != '\\' && !in_string {
            in_char = !in_char;
        } else if !in_string && !in_char {
            match ch {
                '{' | '[' | '(' => net += 1,
                '}' | ']' | ')' => net -= 1,
                '/' if prev_char == '/' => break,
                _ => {}
            }
        }
        prev_char = ch;
    }

    net
}
