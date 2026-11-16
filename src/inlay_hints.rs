use tower_lsp::lsp_types::{InlayHint, InlayHintKind, InlayHintLabel, Position, Range};
use yukic_ast::Module;

use crate::document::Document;
use crate::workspace::WorkspaceEnv;

pub fn compute_inlay_hints(
    doc: &Document,
    _range: Range,
    _module: &Module,
    _env: &WorkspaceEnv,
) -> Vec<InlayHint> {
    let mut hints = Vec::new();
    let text = &doc.text;

    // Fast heuristic scan for unannotated `set <var> = `
    let mut line_num = 0u32;
    for line in text.lines() {
        let trimmed = line.trim_start();
        if trimmed.starts_with("set ") {
            let rest = trimmed["set ".len()..].trim_start();
            if let Some(eq_pos) = rest.find('=') {
                let var_part = rest[..eq_pos].trim();
                // If it does not contain a colon, type was omitted!
                if !var_part.contains(':') && !var_part.is_empty() {
                    let col = (line.len() - trimmed.len() + "set ".len() + var_part.len()) as u32;
                    let val_part = rest[eq_pos + 1..].trim();
                    let inferred_type = infer_simple_type(val_part);

                    if let Some(ty) = inferred_type {
                        hints.push(InlayHint {
                            position: Position::new(line_num, col),
                            label: InlayHintLabel::String(format!(": {ty}")),
                            kind: Some(InlayHintKind::TYPE),
                            text_edits: None,
                            tooltip: None,
                            padding_left: Some(false),
                            padding_right: Some(true),
                            data: None,
                        });
                    }
                }
            }
        }
        line_num += 1;
    }

    hints
}

fn infer_simple_type(expr: &str) -> Option<String> {
    let trimmed = expr.trim();
    if trimmed.starts_with('"') {
        Some("string".to_string())
    } else if trimmed.starts_with('\'') {
        Some("char".to_string())
    } else if trimmed == "true" || trimmed == "false" {
        Some("bool".to_string())
    } else if let Some(init_rest) = trimmed.strip_prefix("init ") {
        let cls_name = init_rest
            .split(|c: char| c == '(' || c.is_whitespace() || c == '{')
            .next()?
            .trim();
        if !cls_name.is_empty() {
            Some(cls_name.to_string())
        } else {
            None
        }
    } else if trimmed.starts_with("terminal_size()") {
        Some("(int32, int32)".to_string())
    } else if trimmed.starts_with("cast(") {
        if trimmed.ends_with("\"string\")") {
            Some("string".to_string())
        } else if trimmed.ends_with("\"int32\")") {
            Some("int32".to_string())
        } else if trimmed.ends_with("\"int64\")") {
            Some("int64".to_string())
        } else if trimmed.ends_with("\"float32\")") {
            Some("float32".to_string())
        } else if trimmed.ends_with("\"float64\")") {
            Some("float64".to_string())
        } else if trimmed.ends_with("\"bool\")") {
            Some("bool".to_string())
        } else {
            None
        }
    } else if trimmed.parse::<i64>().is_ok() {
        Some("int32".to_string())
    } else if trimmed.parse::<f64>().is_ok() {
        Some("float64".to_string())
    } else {
        None
    }
}
