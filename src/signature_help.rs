use tower_lsp::lsp_types::{
    ParameterInformation, ParameterLabel, Position, SignatureHelp, SignatureInformation,
};
use yukic_ast::{ItemKind, Module};

use crate::document::Document;
use crate::workspace::WorkspaceEnv;

pub fn compute_signature_help(
    doc: &Document,
    pos: Position,
    module: &Module,
    env: &WorkspaceEnv,
) -> Option<SignatureHelp> {
    let offset = doc.position_to_byte_offset(pos);
    let (fn_name, active_param) = find_call_context(&doc.text, offset)?;
    let current_file = doc.uri.to_file_path().unwrap_or_default();

    // Check local module functions and methods
    for item in &module.items {
        match &item.kind {
            ItemKind::Fun(ref f) if f.name.name == fn_name => {
                let sig = format_fun_signature(&f.name.name, &f.params, f.return_type.as_ref());
                let params = f
                    .params
                    .iter()
                    .map(|p| ParameterInformation {
                        label: ParameterLabel::Simple(format!("{}: {:?}", p.name.name, p.ty)),
                        documentation: None,
                    })
                    .collect();

                return Some(SignatureHelp {
                    signatures: vec![SignatureInformation {
                        label: sig,
                        documentation: None,
                        parameters: Some(params),
                        active_parameter: Some(active_param),
                    }],
                    active_signature: Some(0),
                    active_parameter: Some(active_param),
                });
            }
            ItemKind::Class(ref c) => {
                for m in &c.methods {
                    if m.name.name == fn_name {
                        let sig = format_method_signature(&m.name.name, &m.params, m.return_type.as_ref());
                        let params = m
                            .params
                            .iter()
                            .map(|p| ParameterInformation {
                                label: ParameterLabel::Simple(format!("{}: {:?}", p.name.name, p.ty)),
                                documentation: None,
                            })
                            .collect();

                        return Some(SignatureHelp {
                            signatures: vec![SignatureInformation {
                                label: sig,
                                documentation: None,
                                parameters: Some(params),
                                active_parameter: Some(active_param),
                            }],
                            active_signature: Some(0),
                            active_parameter: Some(active_param),
                        });
                    }
                }
            }
            _ => {}
        }
    }

    // Check imported symbols
    for item in &module.items {
        if let ItemKind::Import(ref imp) = item.kind {
            if let Some(target_file) = env.resolve_import(&imp.path, &current_file) {
                if let Some(cached) = env.get_or_parse_module(&target_file) {
                    for export in &cached.exports {
                        if export.name == fn_name {
                            let params = parse_signature_parameters(&export.signature);
                            return Some(SignatureHelp {
                                signatures: vec![SignatureInformation {
                                    label: export.signature.clone(),
                                    documentation: export
                                        .doc
                                        .clone()
                                        .map(tower_lsp::lsp_types::Documentation::String),
                                    parameters: Some(params),
                                    active_parameter: Some(active_param),
                                }],
                                active_signature: Some(0),
                                active_parameter: Some(active_param),
                            });
                        }
                    }
                }
            }
        }
    }

    None
}

fn parse_signature_parameters(sig: &str) -> Vec<ParameterInformation> {
    let mut params = Vec::new();
    let start_paren = match sig.find('(') {
        Some(idx) => idx + 1,
        None => return params,
    };
    let end_paren = match sig[start_paren..].rfind(')') {
        Some(idx) => start_paren + idx,
        None => sig.len(),
    };
    let inner = &sig[start_paren..end_paren];
    if inner.trim().is_empty() {
        return params;
    }

    for p in inner.split(',') {
        let trimmed = p.trim();
        if !trimmed.is_empty() {
            params.push(ParameterInformation {
                label: ParameterLabel::Simple(trimmed.to_string()),
                documentation: None,
            });
        }
    }

    params
}

fn find_call_context(text: &str, offset: usize) -> Option<(&str, u32)> {
    let safe_offset = offset.min(text.len());
    let prefix = &text[..safe_offset];

    let mut depth = 0i32;
    let mut comma_count = 0u32;
    let mut open_paren_idx = None;

    for (i, c) in prefix.char_indices().rev() {
        match c {
            ')' => depth += 1,
            '(' => {
                if depth == 0 {
                    open_paren_idx = Some(i);
                    break;
                } else {
                    depth -= 1;
                }
            }
            ',' => {
                if depth == 0 {
                    comma_count += 1;
                }
            }
            '\n' | ';' => {
                if depth <= 0 {
                    break;
                }
            }
            _ => {}
        }
    }

    let paren_pos = open_paren_idx?;
    let before_paren = &prefix[..paren_pos].trim_end();
    let fn_start = before_paren
        .rfind(|c: char| !c.is_alphanumeric() && c != '_')
        .map(|idx| idx + 1)
        .unwrap_or(0);

    let fn_name = &before_paren[fn_start..];
    if fn_name.is_empty() {
        return None;
    }

    Some((fn_name, comma_count))
}

fn format_fun_signature(name: &str, params: &[yukic_ast::Param], ret: Option<&yukic_ast::TypeNode>) -> String {
    let mut s = format!("fun {name}(");
    for (i, p) in params.iter().enumerate() {
        if i > 0 {
            s.push_str(", ");
        }
        s.push_str(&p.name.name);
    }
    s.push(')');
    if let Some(_) = ret {
        s.push_str(" -> return_type");
    }
    s
}

fn format_method_signature(name: &str, params: &[yukic_ast::Param], ret: Option<&yukic_ast::TypeNode>) -> String {
    let mut s = format!("fun {name}(");
    for (i, p) in params.iter().enumerate() {
        if i > 0 {
            s.push_str(", ");
        }
        s.push_str(&p.name.name);
    }
    s.push(')');
    if let Some(_) = ret {
        s.push_str(" -> return_type");
    }
    s
}
