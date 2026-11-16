use tower_lsp::lsp_types::{DocumentSymbol, DocumentSymbolResponse, SymbolKind};
use yukic_ast::{EnumVariant, ExterncItem, ItemKind, Module};

use crate::document::Document;

pub fn compute_document_symbols(doc: &Document, module: &Module) -> Option<DocumentSymbolResponse> {
    let mut symbols = Vec::new();

    for item in &module.items {
        let (name, kind, children) = match &item.kind {
            ItemKind::Fun(ref f) => (f.name.name.clone(), SymbolKind::FUNCTION, None),
            ItemKind::Cfun(ref c) => (c.name.name.clone(), SymbolKind::FUNCTION, None),
            ItemKind::Struct(ref s) => {
                let mut members = Vec::new();
                for field in &s.fields {
                    let field_range = doc.span_to_range(field.span.start, field.span.end);
                    #[allow(deprecated)]
                    members.push(DocumentSymbol {
                        name: field.name.name.clone(),
                        detail: None,
                        kind: SymbolKind::FIELD,
                        tags: None,
                        deprecated: None,
                        range: field_range,
                        selection_range: field_range,
                        children: None,
                    });
                }
                for m in &s.methods {
                    let m_range = doc.span_to_range(m.span.start, m.span.end);
                    #[allow(deprecated)]
                    members.push(DocumentSymbol {
                        name: m.name.name.clone(),
                        detail: None,
                        kind: SymbolKind::METHOD,
                        tags: None,
                        deprecated: None,
                        range: m_range,
                        selection_range: m_range,
                        children: None,
                    });
                }
                (s.name.name.clone(), SymbolKind::STRUCT, Some(members))
            }
            ItemKind::Cstruct(ref cs) => {
                let fields = cs
                    .fields
                    .iter()
                    .map(|field| {
                        let field_range = doc.span_to_range(field.span.start, field.span.end);
                        #[allow(deprecated)]
                        DocumentSymbol {
                            name: field.name.name.clone(),
                            detail: None,
                            kind: SymbolKind::FIELD,
                            tags: None,
                            deprecated: None,
                            range: field_range,
                            selection_range: field_range,
                            children: None,
                        }
                    })
                    .collect();
                (cs.name.name.clone(), SymbolKind::STRUCT, Some(fields))
            }
            ItemKind::Class(ref c) => {
                let mut members = Vec::new();
                for m in &c.methods {
                    let m_range = doc.span_to_range(m.span.start, m.span.end);
                    #[allow(deprecated)]
                    members.push(DocumentSymbol {
                        name: m.name.name.clone(),
                        detail: None,
                        kind: SymbolKind::METHOD,
                        tags: None,
                        deprecated: None,
                        range: m_range,
                        selection_range: m_range,
                        children: None,
                    });
                }
                for a in &c.accessors {
                    let a_range = doc.span_to_range(a.span.start, a.span.end);
                    #[allow(deprecated)]
                    members.push(DocumentSymbol {
                        name: a.name.name.clone(),
                        detail: None,
                        kind: SymbolKind::PROPERTY,
                        tags: None,
                        deprecated: None,
                        range: a_range,
                        selection_range: a_range,
                        children: None,
                    });
                }
                for f in &c.fields {
                    let f_range = doc.span_to_range(f.span.start, f.span.end);
                    #[allow(deprecated)]
                    members.push(DocumentSymbol {
                        name: f.name.name.clone(),
                        detail: None,
                        kind: SymbolKind::FIELD,
                        tags: None,
                        deprecated: None,
                        range: f_range,
                        selection_range: f_range,
                        children: None,
                    });
                }
                (c.name.name.clone(), SymbolKind::CLASS, Some(members))
            }
            ItemKind::Enum(ref e) => {
                let variants = e
                    .variants
                    .iter()
                    .map(|v| {
                        let (v_name, v_span) = match v {
                            EnumVariant::Simple(ref id) => (id.name.clone(), id.span),
                            EnumVariant::WithValue(ref id, _) => (id.name.clone(), id.span),
                            EnumVariant::Adt(ref id, _) => (id.name.clone(), id.span),
                        };
                        let v_range = doc.span_to_range(v_span.start, v_span.end);
                        #[allow(deprecated)]
                        DocumentSymbol {
                            name: v_name,
                            detail: None,
                            kind: SymbolKind::ENUM_MEMBER,
                            tags: None,
                            deprecated: None,
                            range: v_range,
                            selection_range: v_range,
                            children: None,
                        }
                    })
                    .collect();
                (e.name.name.clone(), SymbolKind::ENUM, Some(variants))
            }
            ItemKind::Interface(ref i) => (i.name.name.clone(), SymbolKind::INTERFACE, None),
            ItemKind::Static(ref s) => (s.name.name.clone(), SymbolKind::CONSTANT, None),
            ItemKind::Macro(ref m) => (m.name.name.clone(), SymbolKind::FUNCTION, None),
            ItemKind::TypeAlias(ref ta) => (ta.name.name.clone(), SymbolKind::TYPE_PARAMETER, None),
            ItemKind::Alias(ref a) => (a.alias.name.clone(), SymbolKind::NAMESPACE, None),
            ItemKind::Externc(ref ext) => {
                let mut members = Vec::new();
                for ext_item in &ext.items {
                    match ext_item {
                        ExterncItem::Cfun(ref c) => {
                            let c_range = doc.span_to_range(c.span.start, c.span.end);
                            #[allow(deprecated)]
                            members.push(DocumentSymbol {
                                name: c.name.name.clone(),
                                detail: None,
                                kind: SymbolKind::FUNCTION,
                                tags: None,
                                deprecated: None,
                                range: c_range,
                                selection_range: c_range,
                                children: None,
                            });
                        }
                        ExterncItem::Cstruct(ref cs) => {
                            let cs_range = doc.span_to_range(cs.span.start, cs.span.end);
                            #[allow(deprecated)]
                            members.push(DocumentSymbol {
                                name: cs.name.name.clone(),
                                detail: None,
                                kind: SymbolKind::STRUCT,
                                tags: None,
                                deprecated: None,
                                range: cs_range,
                                selection_range: cs_range,
                                children: None,
                            });
                        }
                    }
                }
                (
                    ext.header
                        .as_deref()
                        .map(|h| format!("externc \"{h}\""))
                        .unwrap_or_else(|| "externc".to_string()),
                    SymbolKind::NAMESPACE,
                    Some(members),
                )
            }
            _ => continue,
        };

        let range = doc.span_to_range(item.span.start, item.span.end);
        #[allow(deprecated)]
        symbols.push(DocumentSymbol {
            name,
            detail: None,
            kind,
            tags: None,
            deprecated: None,
            range,
            selection_range: range,
            children,
        });
    }

    Some(DocumentSymbolResponse::Nested(symbols))
}
