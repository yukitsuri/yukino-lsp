use tower_lsp::lsp_types::{
    CallHierarchyIncomingCall, CallHierarchyItem, CallHierarchyOutgoingCall, Position, SymbolKind,
};
use yukic_ast::{ItemKind, Module};

use crate::document::Document;
use crate::workspace::WorkspaceEnv;

pub fn prepare_call_hierarchy(
    doc: &Document,
    pos: Position,
    module: &Module,
    _env: &WorkspaceEnv,
) -> Option<Vec<CallHierarchyItem>> {
    let offset = doc.position_to_byte_offset(pos);
    let word = get_word_at_offset(&doc.text, offset)?;

    for item in &module.items {
        match &item.kind {
            ItemKind::Fun(ref f) if f.name.name == word => {
                let range = doc.span_to_range(item.span.start, item.span.end);
                let selection_range = doc.span_to_range(f.name.span.start, f.name.span.end);
                return Some(vec![CallHierarchyItem {
                    name: f.name.name.clone(),
                    kind: SymbolKind::FUNCTION,
                    tags: None,
                    detail: Some(format!("fun {}", f.name.name)),
                    uri: doc.uri.clone(),
                    range,
                    selection_range,
                    data: None,
                }]);
            }
            ItemKind::Cfun(ref c) if c.name.name == word => {
                let range = doc.span_to_range(item.span.start, item.span.end);
                let selection_range = doc.span_to_range(c.name.span.start, c.name.span.end);
                return Some(vec![CallHierarchyItem {
                    name: c.name.name.clone(),
                    kind: SymbolKind::FUNCTION,
                    tags: None,
                    detail: Some(format!("cfun {}", c.name.name)),
                    uri: doc.uri.clone(),
                    range,
                    selection_range,
                    data: None,
                }]);
            }
            ItemKind::Class(ref cls) => {
                for m in &cls.methods {
                    if m.name.name == word {
                        let range = doc.span_to_range(m.span.start, m.span.end);
                        let selection_range = doc.span_to_range(m.name.span.start, m.name.span.end);
                        return Some(vec![CallHierarchyItem {
                            name: m.name.name.clone(),
                            kind: SymbolKind::METHOD,
                            tags: None,
                            detail: Some(format!("{}.{}", cls.name.name, m.name.name)),
                            uri: doc.uri.clone(),
                            range,
                            selection_range,
                            data: None,
                        }]);
                    }
                }
            }
            ItemKind::Struct(ref s) => {
                for m in &s.methods {
                    if m.name.name == word {
                        let range = doc.span_to_range(m.span.start, m.span.end);
                        let selection_range = doc.span_to_range(m.name.span.start, m.name.span.end);
                        return Some(vec![CallHierarchyItem {
                            name: m.name.name.clone(),
                            kind: SymbolKind::METHOD,
                            tags: None,
                            detail: Some(format!("{}.{}", s.name.name, m.name.name)),
                            uri: doc.uri.clone(),
                            range,
                            selection_range,
                            data: None,
                        }]);
                    }
                }
            }
            _ => {}
        }
    }

    None
}

pub fn incoming_calls(
    item: CallHierarchyItem,
    module: &Module,
    doc: &Document,
) -> Vec<CallHierarchyIncomingCall> {
    let mut calls = Vec::new();
    let target_name = &item.name;

    for ast_item in &module.items {
        if let ItemKind::Fun(ref f) = ast_item.kind {
            if f.name.name != *target_name {
                let fun_span = ast_item.span;
                let fun_text = doc.slice_span(fun_span.start, fun_span.end);
                if fun_text.contains(target_name) {
                    let range = doc.span_to_range(ast_item.span.start, ast_item.span.end);
                    let sel_range = doc.span_to_range(f.name.span.start, f.name.span.end);

                    calls.push(CallHierarchyIncomingCall {
                        from: CallHierarchyItem {
                            name: f.name.name.clone(),
                            kind: SymbolKind::FUNCTION,
                            tags: None,
                            detail: Some(format!("fun {}", f.name.name)),
                            uri: doc.uri.clone(),
                            range,
                            selection_range: sel_range,
                            data: None,
                        },
                        from_ranges: vec![sel_range],
                    });
                }
            }
        }
    }

    calls
}

pub fn outgoing_calls(
    item: CallHierarchyItem,
    module: &Module,
    doc: &Document,
) -> Vec<CallHierarchyOutgoingCall> {
    let mut calls = Vec::new();
    let source_name = &item.name;

    for ast_item in &module.items {
        if let ItemKind::Fun(ref f) = ast_item.kind {
            if f.name.name == *source_name {
                let fun_span = ast_item.span;
                let fun_text = doc.slice_span(fun_span.start, fun_span.end);

                for other in &module.items {
                    if let ItemKind::Fun(ref other_f) = other.kind {
                        if other_f.name.name != *source_name && fun_text.contains(&other_f.name.name) {
                            let range = doc.span_to_range(other.span.start, other.span.end);
                            let sel_range = doc.span_to_range(other_f.name.span.start, other_f.name.span.end);

                            calls.push(CallHierarchyOutgoingCall {
                                to: CallHierarchyItem {
                                    name: other_f.name.name.clone(),
                                    kind: SymbolKind::FUNCTION,
                                    tags: None,
                                    detail: Some(format!("fun {}", other_f.name.name)),
                                    uri: doc.uri.clone(),
                                    range,
                                    selection_range: sel_range,
                                    data: None,
                                },
                                from_ranges: vec![sel_range],
                            });
                        }
                    }
                }
            }
        }
    }

    calls
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
