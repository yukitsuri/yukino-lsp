use serde::Serialize;
use std::fs;
use std::path::Path;
use tower_lsp::lsp_types::Url;
use yukic_ast::{ItemKind, Module};

use crate::diagnostics::analyze_document;
use crate::document::Document;
use crate::workspace::WorkspaceEnv;

#[derive(Serialize)]
pub struct AgentContext {
    pub file: String,
    pub line: u32,
    pub col: u32,
    pub available_libs: Vec<String>,
    pub stdlib_modules: Vec<String>,
    pub local_symbols: Vec<ContextSymbol>,
    pub imported_symbols: Vec<ContextSymbol>,
    pub diagnostics_count: usize,
}

#[derive(Serialize)]
pub struct ContextSymbol {
    pub name: String,
    pub kind: String,
    pub signature: String,
    pub source_file: String,
}

pub fn run_context(path: &Path, line: u32, col: u32) {
    let content = fs::read_to_string(path).unwrap_or_default();
    let uri = Url::from_file_path(path).unwrap_or_else(|_| Url::parse("file:///tmp").unwrap());
    let doc = Document::new(uri, content, 1);
    let env = WorkspaceEnv::for_path(path);
    let (module, diags) = analyze_document(&doc, Some(&env));
    let local_symbols = collect_local_symbols(&module, path);
    let imported_symbols = collect_imported_symbols(&module, path, &env);
    let stdlib_modules = env.list_package_submodules("std");

    let ctx = AgentContext {
        file: path.display().to_string(),
        line,
        col,
        available_libs: env.available_libs.clone(),
        stdlib_modules,
        local_symbols,
        imported_symbols,
        diagnostics_count: diags.len(),
    };

    let serialized = serde_json::to_string_pretty(&ctx).unwrap_or_default();
    println!("{serialized}");
}

fn collect_local_symbols(module: &Module, path: &Path) -> Vec<ContextSymbol> {
    let mut symbols = Vec::new();
    let path_str = path.display().to_string();

    for item in &module.items {
        match &item.kind {
            ItemKind::Fun(ref f) => {
                let params: Vec<String> = f
                    .params
                    .iter()
                    .map(|p| {
                        let mut s = String::new();
                        if p.is_ref && p.is_mut {
                            s.push_str("ref ~");
                        } else if p.is_ref {
                            s.push_str("ref ");
                        }
                        s.push_str(&p.name.name);
                        if let Some(ref ty) = p.ty {
                            s.push_str(": ");
                            s.push_str(&ty.to_string());
                        }
                        if p.is_variadic {
                            s.push_str("...");
                        }
                        s
                    })
                    .collect();
                let ret = if let Some(ref rt) = f.return_type {
                    format!(" -> {rt}")
                } else {
                    String::new()
                };
                symbols.push(ContextSymbol {
                    name: f.name.name.clone(),
                    kind: "function".to_string(),
                    signature: format!("fun {}({}){}", f.name.name, params.join(", "), ret),
                    source_file: path_str.clone(),
                });
            }
            ItemKind::Cfun(ref c) => {
                let params: Vec<String> = c
                    .params
                    .iter()
                    .map(|p| format!("{}: {}", p.name.name, p.ty))
                    .collect();
                let ret = if let Some(ref rt) = c.return_type {
                    format!(" -> {rt}")
                } else {
                    " -> void".to_string()
                };
                symbols.push(ContextSymbol {
                    name: c.name.name.clone(),
                    kind: "cfun".to_string(),
                    signature: format!("cfun {}({}){};", c.name.name, params.join(", "), ret),
                    source_file: path_str.clone(),
                });
            }
            ItemKind::Struct(ref s) => {
                let fields: Vec<String> = s
                    .fields
                    .iter()
                    .map(|f| format!("{}: {}", f.name.name, f.ty))
                    .collect();
                let signature = if fields.is_empty() {
                    format!("struct {}", s.name.name)
                } else {
                    format!("struct {} {{ {} }}", s.name.name, fields.join(", "))
                };
                symbols.push(ContextSymbol {
                    name: s.name.name.clone(),
                    kind: "struct".to_string(),
                    signature,
                    source_file: path_str.clone(),
                });
            }
            ItemKind::Cstruct(ref cs) => {
                let fields: Vec<String> = cs
                    .fields
                    .iter()
                    .map(|f| format!("{}: {}", f.name.name, f.ty))
                    .collect();
                let signature = if fields.is_empty() {
                    format!("cstruct {}", cs.name.name)
                } else {
                    format!("cstruct {} {{ {} }}", cs.name.name, fields.join(", "))
                };
                symbols.push(ContextSymbol {
                    name: cs.name.name.clone(),
                    kind: "cstruct".to_string(),
                    signature,
                    source_file: path_str.clone(),
                });
            }
            ItemKind::Class(ref c) => {
                let prefix = if c.is_locked { "locked class" } else { "class" };
                symbols.push(ContextSymbol {
                    name: c.name.name.clone(),
                    kind: "class".to_string(),
                    signature: format!("{prefix} {}", c.name.name),
                    source_file: path_str.clone(),
                });
            }
            ItemKind::Enum(ref e) => {
                let variants: Vec<String> = e
                    .variants
                    .iter()
                    .map(|v| match v {
                        yukic_ast::EnumVariant::Simple(id) => id.name.clone(),
                        yukic_ast::EnumVariant::WithValue(id, _) => format!("{} = ...", id.name),
                        yukic_ast::EnumVariant::Adt(id, fields) => {
                            let fld_strs: Vec<String> = fields
                                .iter()
                                .map(|f| format!("{}: {}", f.name.name, f.ty))
                                .collect();
                            format!("{}({})", id.name, fld_strs.join(", "))
                        }
                    })
                    .collect();
                let signature = if variants.is_empty() {
                    format!("enum {}", e.name.name)
                } else {
                    format!("enum {} {{ {} }}", e.name.name, variants.join(", "))
                };
                symbols.push(ContextSymbol {
                    name: e.name.name.clone(),
                    kind: "enum".to_string(),
                    signature,
                    source_file: path_str.clone(),
                });
            }
            ItemKind::Interface(ref i) => {
                symbols.push(ContextSymbol {
                    name: i.name.name.clone(),
                    kind: "interface".to_string(),
                    signature: format!("interface {}", i.name.name),
                    source_file: path_str.clone(),
                });
            }
            ItemKind::Static(ref s) => {
                let ty_str = if let Some(ref ty) = s.ty {
                    format!(": {ty}")
                } else {
                    String::new()
                };
                symbols.push(ContextSymbol {
                    name: s.name.name.clone(),
                    kind: "constant".to_string(),
                    signature: format!("static {}{}", s.name.name, ty_str),
                    source_file: path_str.clone(),
                });
            }
            ItemKind::Macro(ref m) => {
                symbols.push(ContextSymbol {
                    name: m.name.name.clone(),
                    kind: "macro".to_string(),
                    signature: format!("define {}!(...)", m.name.name),
                    source_file: path_str.clone(),
                });
            }
            ItemKind::TypeAlias(ref ta) => {
                symbols.push(ContextSymbol {
                    name: ta.name.name.clone(),
                    kind: "type".to_string(),
                    signature: format!("type {} = {}", ta.name.name, ta.target),
                    source_file: path_str.clone(),
                });
            }
            ItemKind::Alias(ref a) => {
                symbols.push(ContextSymbol {
                    name: a.alias.name.clone(),
                    kind: "alias".to_string(),
                    signature: format!("alias as {}", a.alias.name),
                    source_file: path_str.clone(),
                });
            }
            ItemKind::Externc(ref ext) => {
                for ext_item in &ext.items {
                    match ext_item {
                        yukic_ast::ExterncItem::Cfun(ref c) => {
                            let params: Vec<String> = c
                                .params
                                .iter()
                                .map(|p| format!("{}: {}", p.name.name, p.ty))
                                .collect();
                            let ret = if let Some(ref rt) = c.return_type {
                                format!(" -> {rt}")
                            } else {
                                " -> void".to_string()
                            };
                            symbols.push(ContextSymbol {
                                name: c.name.name.clone(),
                                kind: "cfun".to_string(),
                                signature: format!(
                                    "cfun {}({}){};",
                                    c.name.name,
                                    params.join(", "),
                                    ret
                                ),
                                source_file: path_str.clone(),
                            });
                        }
                        yukic_ast::ExterncItem::Cstruct(ref cs) => {
                            let fields: Vec<String> = cs
                                .fields
                                .iter()
                                .map(|f| format!("{}: {}", f.name.name, f.ty))
                                .collect();
                            let signature = if fields.is_empty() {
                                format!("cstruct {}", cs.name.name)
                            } else {
                                format!("cstruct {} {{ {} }}", cs.name.name, fields.join(", "))
                            };
                            symbols.push(ContextSymbol {
                                name: cs.name.name.clone(),
                                kind: "cstruct".to_string(),
                                signature,
                                source_file: path_str.clone(),
                            });
                        }
                    }
                }
            }
            _ => {}
        }
    }
    symbols
}

fn collect_imported_symbols(
    module: &Module,
    current_file: &Path,
    env: &WorkspaceEnv,
) -> Vec<ContextSymbol> {
    let mut symbols = Vec::new();

    for item in &module.items {
        if let ItemKind::Import(ref imp) = item.kind {
            if let Some(target_file) = env.resolve_import(&imp.path, current_file) {
                if let Some(cached) = env.get_or_parse_module(&target_file) {
                    for export in &cached.exports {
                        symbols.push(ContextSymbol {
                            name: export.name.clone(),
                            kind: export.kind.clone(),
                            signature: export.signature.clone(),
                            source_file: target_file.display().to_string(),
                        });
                    }
                }
            }
        }
    }

    symbols
}
