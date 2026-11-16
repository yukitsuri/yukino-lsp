use std::fs;
use tower_lsp::lsp_types::{Location, Position, Url};
use yukic_ast::Module;

use crate::document::Document;
use crate::workspace::WorkspaceEnv;

pub fn compute_references(
    doc: &Document,
    pos: Position,
    _module: &Module,
    env: &WorkspaceEnv,
    _include_declaration: bool,
) -> Vec<Location> {
    let offset = doc.position_to_byte_offset(pos);
    let word = match get_word_at_offset(&doc.text, offset) {
        Some(w) if !w.is_empty() => w,
        _ => return Vec::new(),
    };

    let mut locations = Vec::new();

    // Search in current document
    find_word_occurrences_in_doc(doc, word, &mut locations);

    // Search in other workspace files recursively
    let mut files = Vec::new();
    collect_workspace_ykn_files(&env.project_root, &mut files);

    for p in files {
        let file_uri = match Url::from_file_path(&p) {
            Ok(u) => u,
            Err(_) => continue,
        };
        if file_uri == doc.uri {
            continue;
        }
        if let Ok(content) = fs::read_to_string(&p) {
            let other_doc = Document::new(file_uri, content, 1);
            find_word_occurrences_in_doc(&other_doc, word, &mut locations);
        }
    }

    locations
}

fn collect_workspace_ykn_files(dir: &std::path::Path, out: &mut Vec<std::path::PathBuf>) {
    if let Ok(entries) = fs::read_dir(dir) {
        for entry in entries.flatten() {
            let p = entry.path();
            if p.is_dir() {
                let name = p.file_name().and_then(|s| s.to_str()).unwrap_or("");
                if name != "yuki-work" && name != ".git" && name != "target" && name != "temp" {
                    collect_workspace_ykn_files(&p, out);
                }
            } else if p.extension().is_some_and(|ext| ext == "ykn") {
                out.push(p);
            }
        }
    }
}

fn find_word_occurrences_in_doc(doc: &Document, word: &str, out: &mut Vec<Location>) {
    let text = &doc.text;
    let mut start_idx = 0;

    while let Some(rel_pos) = text[start_idx..].find(word) {
        let match_start = start_idx + rel_pos;
        let match_end = match_start + word.len();
        start_idx = match_end;

        // Check word boundaries
        let is_ident_char = |c: char| c.is_alphanumeric() || c == '_';
        let left_bounded = match_start == 0
            || !text[..match_start]
                .chars()
                .last()
                .map(is_ident_char)
                .unwrap_or(false);
        let right_bounded = match_end >= text.len()
            || !text[match_end..]
                .chars()
                .next()
                .map(is_ident_char)
                .unwrap_or(false);

        if left_bounded && right_bounded {
            let start_pos = doc.byte_offset_to_position(match_start);
            let end_pos = doc.byte_offset_to_position(match_end);
            out.push(Location {
                uri: doc.uri.clone(),
                range: tower_lsp::lsp_types::Range {
                    start: start_pos,
                    end: end_pos,
                },
            });
        }
    }
}

fn get_word_at_offset(text: &str, offset: usize) -> Option<&str> {
    if text.is_empty() {
        return None;
    }
    let safe_offset = offset.min(text.len().saturating_sub(1));
    let is_ident_char = |c: char| c.is_alphanumeric() || c == '_';

    let start = text[..=safe_offset]
        .rfind(|c: char| !is_ident_char(c))
        .map(|idx| idx + 1)
        .unwrap_or(0);

    let end_rel = text[safe_offset..]
        .find(|c: char| !is_ident_char(c))
        .unwrap_or(text.len() - safe_offset);

    let end = safe_offset + end_rel;
    if start >= end {
        None
    } else {
        Some(&text[start..end])
    }
}
