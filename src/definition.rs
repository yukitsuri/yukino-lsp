use tower_lsp::lsp_types::{GotoDefinitionResponse, Location, Position, Url};
use yukic_ast::{EnumVariant, ExterncItem, ItemKind, Module};

use crate::document::Document;
use crate::workspace::WorkspaceEnv;

pub fn compute_definition(
    doc: &Document,
    pos: Position,
    module: &Module,
    env: &WorkspaceEnv,
) -> Option<GotoDefinitionResponse> {
    let offset = doc.position_to_byte_offset(pos);
    let word = get_word_at_offset(&doc.text, offset)?;
    let current_file = doc.uri.to_file_path().unwrap_or_default();

    for item in &module.items {
        match &item.kind {
            ItemKind::Fun(ref f) if f.name.name == word => {
                let range = doc.span_to_range(item.span.start, item.span.end);
                return Some(GotoDefinitionResponse::Scalar(Location {
                    uri: doc.uri.clone(),
                    range,
                }));
            }
            ItemKind::Cfun(ref c) if c.name.name == word => {
                let range = doc.span_to_range(item.span.start, item.span.end);
                return Some(GotoDefinitionResponse::Scalar(Location {
                    uri: doc.uri.clone(),
                    range,
                }));
            }
            ItemKind::Struct(ref s) => {
                if s.name.name == word {
                    let range = doc.span_to_range(item.span.start, item.span.end);
                    return Some(GotoDefinitionResponse::Scalar(Location {
                        uri: doc.uri.clone(),
                        range,
                    }));
                }
                for f in &s.fields {
                    if f.name.name == word {
                        let range = doc.span_to_range(f.span.start, f.span.end);
                        return Some(GotoDefinitionResponse::Scalar(Location {
                            uri: doc.uri.clone(),
                            range,
                        }));
                    }
                }
            }
            ItemKind::Cstruct(ref cs) => {
                if cs.name.name == word {
                    let range = doc.span_to_range(item.span.start, item.span.end);
                    return Some(GotoDefinitionResponse::Scalar(Location {
                        uri: doc.uri.clone(),
                        range,
                    }));
                }
                for f in &cs.fields {
                    if f.name.name == word {
                        let range = doc.span_to_range(f.span.start, f.span.end);
                        return Some(GotoDefinitionResponse::Scalar(Location {
                            uri: doc.uri.clone(),
                            range,
                        }));
                    }
                }
            }
            ItemKind::Class(ref c) => {
                if c.name.name == word {
                    let range = doc.span_to_range(item.span.start, item.span.end);
                    return Some(GotoDefinitionResponse::Scalar(Location {
                        uri: doc.uri.clone(),
                        range,
                    }));
                }
                for m in &c.methods {
                    if m.name.name == word {
                        let range = doc.span_to_range(m.span.start, m.span.end);
                        return Some(GotoDefinitionResponse::Scalar(Location {
                            uri: doc.uri.clone(),
                            range,
                        }));
                    }
                }
                for a in &c.accessors {
                    if a.name.name == word {
                        let range = doc.span_to_range(a.span.start, a.span.end);
                        return Some(GotoDefinitionResponse::Scalar(Location {
                            uri: doc.uri.clone(),
                            range,
                        }));
                    }
                }
                for f in &c.fields {
                    if f.name.name == word {
                        let range = doc.span_to_range(f.span.start, f.span.end);
                        return Some(GotoDefinitionResponse::Scalar(Location {
                            uri: doc.uri.clone(),
                            range,
                        }));
                    }
                }
            }
            ItemKind::Enum(ref e) => {
                if e.name.name == word {
                    let range = doc.span_to_range(item.span.start, item.span.end);
                    return Some(GotoDefinitionResponse::Scalar(Location {
                        uri: doc.uri.clone(),
                        range,
                    }));
                }
                for v in &e.variants {
                    let (v_name, v_span) = match v {
                        EnumVariant::Simple(ref id) => (&id.name, id.span),
                        EnumVariant::WithValue(ref id, _) => (&id.name, id.span),
                        EnumVariant::Adt(ref id, _) => (&id.name, id.span),
                    };
                    if v_name == word {
                        let range = doc.span_to_range(v_span.start, v_span.end);
                        return Some(GotoDefinitionResponse::Scalar(Location {
                            uri: doc.uri.clone(),
                            range,
                        }));
                    }
                }
            }
            ItemKind::Interface(ref i) if i.name.name == word => {
                let range = doc.span_to_range(item.span.start, item.span.end);
                return Some(GotoDefinitionResponse::Scalar(Location {
                    uri: doc.uri.clone(),
                    range,
                }));
            }
            ItemKind::Static(ref s) if s.name.name == word => {
                let range = doc.span_to_range(item.span.start, item.span.end);
                return Some(GotoDefinitionResponse::Scalar(Location {
                    uri: doc.uri.clone(),
                    range,
                }));
            }
            ItemKind::Macro(ref m) if m.name.name == word => {
                let range = doc.span_to_range(item.span.start, item.span.end);
                return Some(GotoDefinitionResponse::Scalar(Location {
                    uri: doc.uri.clone(),
                    range,
                }));
            }
            ItemKind::TypeAlias(ref ta) if ta.name.name == word => {
                let range = doc.span_to_range(item.span.start, item.span.end);
                return Some(GotoDefinitionResponse::Scalar(Location {
                    uri: doc.uri.clone(),
                    range,
                }));
            }
            ItemKind::Alias(ref a) if a.alias.name == word => {
                let range = doc.span_to_range(item.span.start, item.span.end);
                return Some(GotoDefinitionResponse::Scalar(Location {
                    uri: doc.uri.clone(),
                    range,
                }));
            }
            ItemKind::Externc(ref ext) => {
                for ext_item in &ext.items {
                    match ext_item {
                        ExterncItem::Cfun(ref c) if c.name.name == word => {
                            let range = doc.span_to_range(c.span.start, c.span.end);
                            return Some(GotoDefinitionResponse::Scalar(Location {
                                uri: doc.uri.clone(),
                                range,
                            }));
                        }
                        ExterncItem::Cstruct(ref cs) if cs.name.name == word => {
                            let range = doc.span_to_range(cs.span.start, cs.span.end);
                            return Some(GotoDefinitionResponse::Scalar(Location {
                                uri: doc.uri.clone(),
                                range,
                            }));
                        }
                        _ => {}
                    }
                }
            }
            _ => {}
        }
    }

    for item in &module.items {
        if let ItemKind::Import(ref imp) = item.kind {
            if let Some(target_file) = env.resolve_import(&imp.path, &current_file) {
                if let Some(cached) = env.get_or_parse_module(&target_file) {
                    for export in &cached.exports {
                        if export.name == word {
                            let target_content =
                                std::fs::read_to_string(&export.file_path).unwrap_or_default();
                            let target_doc = Document::new(
                                Url::from_file_path(&export.file_path).unwrap(),
                                target_content,
                                1,
                            );
                            let range =
                                target_doc.span_to_range(export.span.start, export.span.end);
                            return Some(GotoDefinitionResponse::Scalar(Location {
                                uri: target_doc.uri,
                                range,
                            }));
                        }
                    }
                }
            }
        }
    }

    None
}

pub fn compute_declaration(
    doc: &Document,
    pos: Position,
    module: &Module,
    env: &WorkspaceEnv,
) -> Option<GotoDefinitionResponse> {
    compute_definition(doc, pos, module, env)
}

pub fn compute_type_definition(
    doc: &Document,
    pos: Position,
    module: &Module,
    env: &WorkspaceEnv,
) -> Option<GotoDefinitionResponse> {
    let offset = doc.position_to_byte_offset(pos);
    let word = get_word_at_offset(&doc.text, offset)?;

    for item in &module.items {
        match &item.kind {
            ItemKind::Class(ref c) if c.name.name == word => {
                let range = doc.span_to_range(item.span.start, item.span.end);
                return Some(GotoDefinitionResponse::Scalar(Location {
                    uri: doc.uri.clone(),
                    range,
                }));
            }
            ItemKind::Struct(ref s) if s.name.name == word => {
                let range = doc.span_to_range(item.span.start, item.span.end);
                return Some(GotoDefinitionResponse::Scalar(Location {
                    uri: doc.uri.clone(),
                    range,
                }));
            }
            ItemKind::Enum(ref e) if e.name.name == word => {
                let range = doc.span_to_range(item.span.start, item.span.end);
                return Some(GotoDefinitionResponse::Scalar(Location {
                    uri: doc.uri.clone(),
                    range,
                }));
            }
            ItemKind::Interface(ref i) if i.name.name == word => {
                let range = doc.span_to_range(item.span.start, item.span.end);
                return Some(GotoDefinitionResponse::Scalar(Location {
                    uri: doc.uri.clone(),
                    range,
                }));
            }
            _ => {}
        }
    }

    compute_definition(doc, pos, module, env)
}

pub fn compute_implementation(
    doc: &Document,
    pos: Position,
    module: &Module,
    _env: &WorkspaceEnv,
) -> Option<GotoDefinitionResponse> {
    let offset = doc.position_to_byte_offset(pos);
    let word = get_word_at_offset(&doc.text, offset)?;

    let mut locations = Vec::new();

    for item in &module.items {
        if let ItemKind::Class(ref c) = item.kind {
            if c.interfaces.iter().any(|i| i.name == word) {
                let range = doc.span_to_range(item.span.start, item.span.end);
                locations.push(Location {
                    uri: doc.uri.clone(),
                    range,
                });
            }
        }
    }

    if locations.is_empty() {
        None
    } else if locations.len() == 1 {
        Some(GotoDefinitionResponse::Scalar(locations.remove(0)))
    } else {
        Some(GotoDefinitionResponse::Array(locations))
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
